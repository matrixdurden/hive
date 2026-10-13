//! HTTPS (WinHTTP; yönlendirmeleri ve sistem proxy'sini kendisi izler). İki biçim: bütün yanıtı
//! bekleyen `request` ve gövdeyi geldikçe veren `stream` (Claude'un SSE akışı, ElevenLabs'in ses
//! akışı). Her ikisi de çağıran iş parçacığını bloklar: arayüzden değil, iş parçacığından çağrılır.
#![allow(dead_code)]

use windows::Win32::Networking::WinHttp::*;
use windows::core::PCWSTR;

use crate::util::{Res, wide};

const MAX_SIZE: usize = 64 << 20;

/// WinHTTP tutamacı; düşünce kapanır.
struct Handle(*mut core::ffi::c_void);

impl Handle {
    fn new(h: *mut core::ffi::c_void) -> windows::core::Result<Self> {
        if h.is_null() { Err(windows::core::Error::from_thread()) } else { Ok(Self(h)) }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

pub struct Response {
    pub status: u32,
    pub body: Vec<u8>,
}

impl Response {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// 2xx değilse gövdeden kısa bir hata metni.
    pub fn ok(self) -> Res<Self> {
        if (200..300).contains(&self.status) {
            return Ok(self);
        }
        let text = self.text();
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.pointer("/error/message").or(v.pointer("/detail/message")).and_then(|m| m.as_str().map(String::from)))
            .unwrap_or_else(|| text.chars().take(300).collect());
        Err(format!("HTTP {}: {msg}", self.status).into())
    }
}

/// Bağlantıyı kurar, başlıkları ve gövdeyi gönderir, yanıt başlıklarını alır: (istek tutamacı
/// ve onu yaşatan üst tutamaçlar, durum kodu).
fn open(method: &str, url: &str, headers: &[(&str, &str)], body: &[u8]) -> Res<([Handle; 3], u32)> {
    // https her yere; düz http yalnızca bu bilgisayara (yerel ses sunucusu).
    let (secure, rest) = match (url.strip_prefix("https://"), url.strip_prefix("http://")) {
        (Some(r), _) => (true, r),
        (None, Some(r)) if r.starts_with("127.0.0.1") || r.starts_with("localhost") => (false, r),
        _ => return Err(t!("https only", "yalnızca https").into()),
    };
    let (hostport, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| "port")?),
        None => (hostport, if secure { INTERNET_DEFAULT_HTTPS_PORT as u16 } else { 80 }),
    };
    let agent = wide(concat!("engram/", env!("CARGO_PKG_VERSION")));
    let (host, path, method) = (wide(host), wide(&path), wide(method));
    unsafe {
        let session = Handle::new(WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ))?;
        let connect = Handle::new(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), port, 0))?;
        let request = Handle::new(WinHttpOpenRequest(
            connect.0,
            PCWSTR(method.as_ptr()),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) },
        ))?;
        // Akışlı yanıtlar dakikalarca sürebilir: okuma zaman aşımı 10 dk.
        let _ = WinHttpSetTimeouts(request.0, 15_000, 20_000, 60_000, 600_000);
        let mut all = String::new();
        for (k, v) in headers {
            all.push_str(&format!("{k}: {v}\r\n"));
        }
        if !all.is_empty() {
            let h: Vec<u16> = all.encode_utf16().collect();
            WinHttpAddRequestHeaders(request.0, &h, WINHTTP_ADDREQ_FLAG_ADD)?;
        }
        let (ptr, len) = if body.is_empty() { (None, 0) } else { (Some(body.as_ptr().cast()), body.len() as u32) };
        WinHttpSendRequest(request.0, None, ptr, len, len, 0)?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())?;
        let (mut status, mut slen) = (0u32, 4u32);
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut slen,
            std::ptr::null_mut(),
        )?;
        Ok(([request, connect, session], status))
    }
}

/// Gövdeyi parça parça `on_chunk`'a verir; `false` dönerse okumayı keser. Durum kodunu döndürür.
/// 2xx değilse gövde `on_chunk`'a verilmez, hata metni döner.
pub fn stream(method: &str, url: &str, headers: &[(&str, &str)], body: &[u8], on_chunk: &mut dyn FnMut(&[u8]) -> bool) -> Res<u32> {
    let (handles, status) = open(method, url, headers, body)?;
    let request = &handles[0];
    let mut buf = vec![0u8; 32 * 1024];
    if !(200..300).contains(&status) {
        let mut err = Vec::new();
        loop {
            let mut n = 0u32;
            unsafe { WinHttpReadData(request.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut n)? };
            if n == 0 || err.len() > MAX_SIZE {
                break;
            }
            err.extend_from_slice(&buf[..n as usize]);
        }
        return Response { status, body: err }.ok().map(|_| status);
    }
    loop {
        let mut n = 0u32;
        unsafe { WinHttpReadData(request.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut n)? };
        if n == 0 {
            break;
        }
        if !on_chunk(&buf[..n as usize]) {
            break;
        }
    }
    Ok(status)
}

/// Bütün yanıt: (durum kodu, gövde). Durum koduna bakmaz; `Response::ok` ile denetlenir.
pub fn request(method: &str, url: &str, headers: &[(&str, &str)], body: &[u8]) -> Res<Response> {
    let mut out = Vec::new();
    let status = {
        let (handles, status) = open(method, url, headers, body)?;
        let request = &handles[0];
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut n = 0u32;
            unsafe { WinHttpReadData(request.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut n)? };
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n as usize]);
            if out.len() > MAX_SIZE {
                return Err(t!("response too large", "yanıt çok büyük").into());
            }
        }
        status
    };
    Ok(Response { status, body: out })
}

/// JSON gövdeli POST.
pub fn post_json(url: &str, headers: &[(&str, &str)], body: &serde_json::Value) -> Res<Response> {
    let mut h: Vec<(&str, &str)> = headers.to_vec();
    h.push(("content-type", "application/json"));
    request("POST", url, &h, body.to_string().as_bytes())
}

pub fn get(url: &str, headers: &[(&str, &str)]) -> Res<Response> {
    request("GET", url, headers, &[])
}
