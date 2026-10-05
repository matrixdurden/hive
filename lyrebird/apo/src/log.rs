//! `%ProgramData%\lyrebird\apo.log`: audiodg içinden tek teşhis kaynağı. Yalnızca gerçek
//! zamanlı olmayan çağrılardan (başlatma, kilitleme) yazılır.

use std::io::Write;

use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::GetCurrentProcessId;

const LIMIT: u64 = 256 * 1024;

static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn write(args: std::fmt::Arguments) {
    let _guard = LOCK.lock();
    let path = crate::bus::dir().join("apo.log");
    let big = std::fs::metadata(&path).is_ok_and(|m| m.len() > LIMIT);
    let file = std::fs::OpenOptions::new().create(true).append(!big).write(true).truncate(big).open(&path);
    if let Ok(mut f) = file {
        let t = unsafe { GetLocalTime() };
        let pid = unsafe { GetCurrentProcessId() };
        let _ = writeln!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02} [{pid}] {args}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
        );
    }
}

#[macro_export]
macro_rules! log {
    ($($t:tt)*) => { $crate::log::write(format_args!($($t)*)) };
}
