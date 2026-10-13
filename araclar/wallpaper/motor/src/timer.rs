//! Tek bekleme noktası: pencere mesajları, isteğe bağlı kare zamanlayıcısı ve klasör olayları.
//! Yüksek çözünürlüklü waitable timer, Windows'un 15.6 ms saat adımına takılmadan hassas FPS
//! sağlar. Zamanlayıcı gerekmiyorsa süresiz beklenir: CPU tamamen uyur.

use std::path::Path;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FindCloseChangeNotification, FindFirstChangeNotificationW,
    FindNextChangeNotification,
};
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE, SetWaitableTimer, TIMER_ALL_ACCESS,
};
use windows::Win32::UI::WindowsAndMessaging::{MsgWaitForMultipleObjects, QS_ALLINPUT};
use windows::core::PCWSTR;

use crate::util::{Res, wide};

pub struct Waiter {
    timer: HANDLE,
}

impl Waiter {
    pub fn new() -> Res<Self> {
        let timer = unsafe {
            CreateWaitableTimerExW(None, PCWSTR::null(), CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0)?
        };
        Ok(Self { timer })
    }

    /// `delay` dolana, bir mesaj gelene ya da `watch` tetiklenene kadar uyur.
    /// `watch` tetiklendiyse `true`.
    pub fn wait(&self, delay: Option<Duration>, watch: Option<&DirWatch>) -> bool {
        let mut handles = Vec::with_capacity(2);
        if let Some(d) = delay {
            // Negatif değer = göreli süre, 100 ns birimiyle.
            let due = -((d.as_nanos() / 100).max(1) as i64);
            if unsafe { SetWaitableTimer(self.timer, &due, 0, None, None, false) }.is_ok() {
                handles.push(self.timer);
            }
        }
        if let Some(w) = watch {
            handles.push(w.handle);
        }
        let r = unsafe { MsgWaitForMultipleObjects(Some(&handles), false, INFINITE, QS_ALLINPUT) };
        let fired = r.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
        watch.is_some() && fired == handles.len() - 1
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.timer);
        }
    }
}

/// Klasöre dosya eklenince, silinince ya da dosya yazılınca işaretlenen çekirdek nesnesi.
pub struct DirWatch {
    handle: HANDLE,
}

impl DirWatch {
    pub fn new(dir: &Path) -> Res<Self> {
        let path = wide(&dir.to_string_lossy());
        let handle = unsafe {
            FindFirstChangeNotificationW(
                PCWSTR(path.as_ptr()),
                false,
                FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE,
            )?
        };
        Ok(Self { handle })
    }

    pub fn rearm(&self) {
        unsafe {
            let _ = FindNextChangeNotification(self.handle);
        }
    }
}

impl Drop for DirWatch {
    fn drop(&mut self) {
        unsafe {
            let _ = FindCloseChangeNotification(self.handle);
        }
    }
}
