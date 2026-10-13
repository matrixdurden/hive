//! İzin sormadan bağlanmak: yükseltilmiş kurulum, kullanıcı için en yüksek yetkiyle çalışan,
//! tetikleyicisiz bir Görev Zamanlayıcı görevi kaydeder (`winloom.exe --soundboard-kur`). Sayfa
//! efekti takılı olmayan bir mikrofon görünce görevi çalıştırır: Windows yönetici izni sormaz.
//! COM başlatılmış iş parçacığında çağrılır.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::TaskScheduler::{
    ITaskFolder, ITaskService, TASK_CREATE_OR_UPDATE, TASK_LOGON_INTERACTIVE_TOKEN, TASK_STATE_QUEUED,
    TASK_STATE_RUNNING, TaskScheduler,
};
use windows::Win32::System::Variant::VARIANT;
use windows::core::BSTR;

use crate::ARG_INSTALL;

/// Görevin bitmesi için en çok bu kadar beklenir (ses hizmeti yeniden başlar, birkaç saniye).
const WAIT: Duration = Duration::from_secs(120);

pub fn name() -> String {
    format!("winloom mikrofon ({})", std::env::var("USERNAME").unwrap_or_default())
}

fn root() -> windows::core::Result<ITaskFolder> {
    unsafe {
        let svc: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
        let none = VARIANT::default();
        svc.Connect(&none, &none, &none, &none)?;
        svc.GetFolder(&BSTR::from("\\"))
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Görevin çalıştırdığı kopya: Program Files'ta, yalnızca yöneticinin yazabildiği yerde.
/// Kullanıcının klasöründeki winloom.exe'yi çalıştırsaydı, onu değiştiren herhangi bir program
/// izin sormadan yönetici olurdu.
fn runner() -> std::path::PathBuf {
    soundboard_engine::install::install_dir().join("winloom-mikrofon.exe")
}

/// Görevi kaydeder (yönetici olarak çalışan kurulumda): exe'nin kopyasını Program Files'a koyar,
/// görev onu çalıştırır. Aynı kopyayla kayıtlıysa dokunulmaz (görevin kendisi de
/// `--soundboard-kur` ile çalışır; o zaman kopya zaten kendisidir).
pub fn register() -> Result<(), String> {
    let src = std::env::current_exe().map_err(|e| e.to_string())?;
    let dst = runner();
    if !src.eq(&dst) {
        std::fs::copy(&src, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    }
    let exe = xml_escape(&dst.display().to_string());
    let root = root().map_err(|e| e.to_string())?;
    let name = BSTR::from(name());
    if let Ok(old) = unsafe { root.GetTask(&name) }
        && unsafe { old.Xml() }.is_ok_and(|x| x.to_string().contains(&format!("<Command>{exe}</Command>")))
    {
        return Ok(());
    }
    let user = xml_escape(&format!(
        "{}\\{}",
        std::env::var("USERDOMAIN").unwrap_or_default(),
        std::env::var("USERNAME").unwrap_or_default()
    ));
    // Tetikleyici yok: yalnızca winloom çalıştırır. Pilde de başlar, süre sınırı yok.
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Author>winloom</Author><Description>{desc}</Description></RegistrationInfo>
  <Triggers />
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>{exe}</Command><Arguments>{ARG_INSTALL}</Arguments></Exec></Actions>
</Task>"#,
        desc = xml_escape(t!(
            "Connects winloom's sound effects to a new microphone without asking for permission",
            "winloom'ın ses efektlerini yeni bir mikrofona izin sormadan bağlar"
        )),
    );
    let none = VARIANT::default();
    unsafe {
        root.RegisterTask(&name, &BSTR::from(xml), TASK_CREATE_OR_UPDATE.0, &none, &none, TASK_LOGON_INTERACTIVE_TOKEN, &none)
    }
    .map_err(|e| e.to_string())?;
    log!("mikrofon görevi kaydedildi: {name}");
    Ok(())
}

pub fn delete() {
    if let Ok(root) = root()
        && unsafe { root.DeleteTask(&BSTR::from(name()), 0) }.is_ok()
    {
        log!("mikrofon görevi silindi");
    }
}

/// Görev duruyor mu (kalıntı denetimi). Okunamıyorsa (izin) var sayılır.
pub fn exists() -> bool {
    let Ok(root) = root() else { return false };
    match unsafe { root.GetTask(&BSTR::from(name())) } {
        Ok(_) => true,
        Err(e) => e.code() != ERROR_FILE_NOT_FOUND.to_hresult(),
    }
}

/// Görevi çalıştırır ve bitmesini bekler. `Ok(true)`: bağlandı, `Ok(false)`: çalıştı ama
/// bağlayamadı, `Err`: görev yok ya da çalıştırılamadı.
pub fn run() -> Result<bool, String> {
    // winloom güncellendiyse kopya eskidir (eski efekti kurar): bir kez izinle kurulum yenilesin.
    let current = std::env::current_exe().ok().and_then(|e| std::fs::read(e).ok());
    if current.is_none() || std::fs::read(runner()).ok() != current {
        return Err("görevin kopyası bu sürümle aynı değil".into());
    }
    let root = root().map_err(|e| e.to_string())?;
    let name = BSTR::from(name());
    let task = unsafe { root.GetTask(&name) }.map_err(|e| e.to_string())?;
    unsafe { task.Run(&VARIANT::default()) }.map_err(|e| e.to_string())?;
    let start = Instant::now();
    let mut seen = false;
    while start.elapsed() < WAIT {
        std::thread::sleep(Duration::from_millis(300));
        let state = unsafe { root.GetTask(&name).and_then(|t| t.State()) }.map_err(|e| e.to_string())?;
        if state == TASK_STATE_RUNNING || state == TASK_STATE_QUEUED {
            seen = true;
        } else if seen || start.elapsed() > Duration::from_secs(3) {
            // Çıkış kodu: 0 bağlandı. (0x41301 hâlâ çalışıyor, 0x41303 hiç çalışmadı.)
            let code = unsafe { root.GetTask(&name).and_then(|t| t.LastTaskResult()) }.map_err(|e| e.to_string())?;
            if code != 0 {
                log!("mikrofon görevi bağlayamadı: 0x{code:x}");
            }
            return Ok(code == 0);
        }
    }
    Err("mikrofon görevi bitmedi".into())
}
