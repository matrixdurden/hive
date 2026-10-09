//! WMI (root\wmi): pilin anlık durumu (kalan mWh, deşarj mW, dolu kapasite) ve dizüstü ekranının
//! parlaklığı. COM başlatılmış iş parçacığında çağrılır (hive'ın ana iş parçacığı öyle).

use windows::Win32::System::Com::*;
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::System::Variant::*;
use windows::Win32::System::Wmi::*;
use windows::core::{BSTR, PCWSTR};

use crate::log;
use crate::util::wide;

struct Wmi {
    svc: IWbemServices,
}

impl Wmi {
    fn connect(namespace: &str) -> Option<Self> {
        unsafe {
            let loc: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).ok()?;
            let svc = loc
                .ConnectServer(&BSTR::from(namespace), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None)
                .ok()?;
            CoSetProxyBlanket(
                &svc,
                RPC_C_AUTHN_WINNT,
                RPC_C_AUTHZ_NONE,
                PCWSTR::null(),
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )
            .ok()?;
            Some(Self { svc })
        }
    }

    fn query(&self, wql: &str) -> Vec<IWbemClassObject> {
        let mut out = Vec::new();
        unsafe {
            let flags = WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY;
            let Ok(e) = self.svc.ExecQuery(&BSTR::from("WQL"), &BSTR::from(wql), flags, None) else { return out };
            loop {
                let mut objs = [None];
                let mut n = 0u32;
                let _ = e.Next(WBEM_INFINITE, &mut objs, &mut n);
                if n == 0 {
                    break;
                }
                if let Some(o) = objs[0].take() {
                    out.push(o);
                }
            }
        }
        out
    }
}

fn get(obj: &IWbemClassObject, name: &str) -> Option<VARIANT> {
    let n = wide(name);
    let mut v = VARIANT::default();
    unsafe { obj.Get(PCWSTR(n.as_ptr()), 0, &mut v, None, None).ok()? };
    Some(v)
}

/// Sayısal özellik; WMI uint32'yi VT_I4, uint64'ü metin olarak verir.
fn num(obj: &IWbemClassObject, name: &str) -> Option<i64> {
    let mut v = get(obj, name)?;
    let r = unsafe {
        let vt = v.Anonymous.Anonymous.vt;
        let a = &v.Anonymous.Anonymous.Anonymous;
        match vt {
            VT_I4 => Some(a.lVal as i64),
            VT_UI4 => Some(a.ulVal as i64),
            VT_I2 => Some(a.iVal as i64),
            VT_UI2 => Some(a.uiVal as i64),
            VT_UI1 => Some(a.bVal as i64),
            VT_I8 => Some(a.llVal),
            VT_BOOL => Some((a.boolVal.0 != 0) as i64),
            VT_BSTR => a.bstrVal.to_string().trim().parse().ok(),
            _ => None,
        }
    };
    unsafe {
        let _ = VariantClear(&mut v);
    }
    r
}

fn text(obj: &IWbemClassObject, name: &str) -> Option<String> {
    let mut v = get(obj, name)?;
    let r =
        unsafe { (v.Anonymous.Anonymous.vt == VT_BSTR).then(|| v.Anonymous.Anonymous.Anonymous.bstrVal.to_string()) };
    unsafe {
        let _ = VariantClear(&mut v);
    }
    r
}

fn variant_i32(x: i32) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        let inner = &mut *v.Anonymous.Anonymous;
        inner.vt = VT_I4;
        inner.Anonymous.lVal = x;
    }
    v
}

fn variant_u8(x: u8) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        let inner = &mut *v.Anonymous.Anonymous;
        inner.vt = VT_UI1;
        inner.Anonymous.bVal = x;
    }
    v
}

#[derive(Clone, Copy, Debug)]
pub struct Battery {
    pub remaining_mwh: i64,
    /// Pildeyken deşarj, şarjdayken şarj hızı (mW).
    pub rate_mw: i64,
    pub full_mwh: i64,
    pub online: bool,
}

/// İlk gerçek pil (gerilimi sıfır olmayan).
pub fn battery() -> Option<Battery> {
    let w = Wmi::connect(r"root\wmi")?;
    let st = w.query("SELECT RemainingCapacity, DischargeRate, ChargeRate, PowerOnline, Voltage FROM BatteryStatus");
    let obj = st.iter().find(|o| num(o, "Voltage").unwrap_or(0) > 0).or(st.first())?;
    let online = num(obj, "PowerOnline").unwrap_or(0) != 0;
    let rate = if online { num(obj, "ChargeRate") } else { num(obj, "DischargeRate") }.unwrap_or(0);
    let full = w
        .query("SELECT FullChargedCapacity FROM BatteryFullChargedCapacity")
        .first()
        .and_then(|o| num(o, "FullChargedCapacity"))
        .unwrap_or(0);
    Some(Battery {
        remaining_mwh: num(obj, "RemainingCapacity").unwrap_or(0),
        rate_mw: rate.abs(),
        full_mwh: full,
        online,
    })
}

/// Dizüstü ekranının parlaklığı (yüzde).
pub fn brightness() -> Option<u8> {
    let w = Wmi::connect(r"root\wmi")?;
    let objs = w.query("SELECT CurrentBrightness FROM WmiMonitorBrightness WHERE Active=TRUE");
    objs.first().and_then(|o| num(o, "CurrentBrightness")).map(|v| v.clamp(0, 100) as u8)
}

pub fn set_brightness(level: u8) -> bool {
    let run = || -> Option<()> {
        let w = Wmi::connect(r"root\wmi")?;
        let objs = w.query("SELECT __PATH FROM WmiMonitorBrightnessMethods WHERE Active=TRUE");
        let path = text(objs.first()?, "__PATH")?;
        unsafe {
            // Metodun giriş parametreleri sınıf tanımından türetilir.
            let mut class = None;
            w.svc
                .GetObject(
                    &BSTR::from("WmiMonitorBrightnessMethods"),
                    WBEM_GENERIC_FLAG_TYPE(0),
                    None,
                    Some(&mut class),
                    None,
                )
                .ok()?;
            let (mut sig_in, mut sig_out) = (None, None);
            let method = wide("WmiSetBrightness");
            class?.GetMethod(PCWSTR(method.as_ptr()), 0, &mut sig_in, &mut sig_out).ok()?;
            let inst = sig_in?.SpawnInstance(0).ok()?;
            let (timeout, brightness) = (wide("Timeout"), wide("Brightness"));
            inst.Put(PCWSTR(timeout.as_ptr()), 0, &variant_i32(0), 0).ok()?;
            inst.Put(PCWSTR(brightness.as_ptr()), 0, &variant_u8(level), 0).ok()?;
            w.svc
                .ExecMethod(
                    &BSTR::from(path),
                    &BSTR::from("WmiSetBrightness"),
                    WBEM_GENERIC_FLAG_TYPE(0),
                    None,
                    &inst,
                    None,
                    None,
                )
                .ok()?;
        }
        Some(())
    };
    let ok = run().is_some();
    if !ok {
        log!("battery: parlaklık %{level} ayarlanamadı");
    }
    ok
}
