//! Windows bildirimleri: bir uygulama bildirim (toast) gönderince dock'taki simgesi zıplasın.
//! Claude gibi uygulamalar pencereyi yanıp söndürmez, yalnızca bildirim gönderir. Windows'un
//! bildirim dinleyicisi (UserNotificationListener) yeni bildirimi olay olarak haber verir;
//! yalnızca gönderen uygulamanın kimliği (AppUserModelID) okunur, bildirimin içeriğine
//! bakılmaz. Olay desteklenmezse dinleyici birkaç saniyede bir yeni bildirim var mı diye bakar.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::Management::{UserNotificationListener, UserNotificationListenerAccessStatus};
use windows::UI::Notifications::{NotificationKinds, UserNotificationChangedEventArgs, UserNotificationChangedKind};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Dock'a: yeni bildirim (lParam: Box<String>, gönderenin AppUserModelID'si).
pub const WM_TOAST: u32 = WM_APP + 14;

static RUNNING: AtomicBool = AtomicBool::new(false);

fn post(dock: usize, aumid: String) {
    let b = Box::into_raw(Box::new(aumid));
    unsafe {
        if PostMessageW(Some(HWND(dock as *mut _)), WM_TOAST, WPARAM(0), LPARAM(b as isize)).is_err() {
            drop(Box::from_raw(b));
        }
    }
}

fn ids(l: &UserNotificationListener) -> Option<Vec<(u32, String)>> {
    let list = l.GetNotificationsAsync(NotificationKinds::Toast).ok()?.join().ok()?;
    Some(
        (0..list.Size().ok()?)
            .filter_map(|i| list.GetAt(i).ok())
            .filter_map(|n| Some((n.Id().ok()?, n.AppInfo().ok()?.AppUserModelId().ok()?.to_string())))
            .collect(),
    )
}

/// Dinlemeye başlar (kendi iş parçacığında). İzin yoksa sessizce vazgeçer.
pub fn start(dock: HWND) {
    if RUNNING.swap(true, Ordering::Relaxed) {
        return;
    }
    let dock = dock.0 as usize;
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let Ok(l) = UserNotificationListener::Current() else {
            RUNNING.store(false, Ordering::Relaxed);
            return;
        };
        let access = l.RequestAccessAsync().ok().and_then(|a| a.join().ok());
        if access != Some(UserNotificationListenerAccessStatus::Allowed) {
            crate::log!("hatter: bildirimlere erişim yok ({access:?})");
            RUNNING.store(false, Ordering::Relaxed);
            return;
        }
        let handler = TypedEventHandler::<UserNotificationListener, UserNotificationChangedEventArgs>::new(move |sender, args| {
            if let (Some(l), Some(a)) = (sender.as_ref(), args.as_ref())
                && a.ChangeKind()? == UserNotificationChangedKind::Added
                && let Ok(n) = l.GetNotification(a.UserNotificationId()?)
            {
                post(dock, n.AppInfo()?.AppUserModelId()?.to_string());
            }
            Ok(())
        });
        match l.NotificationChanged(&handler) {
            Ok(token) => {
                // Olay iş parçacığı havuzundan gelir; bu iş parçacığı yalnızca kaydı tutar.
                while RUNNING.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                let _ = l.RemoveNotificationChanged(token);
            }
            Err(e) => {
                // Paketli olmayan uygulamaya olay verilmeyebilir: yeni bildirim var mı diye bakılır.
                crate::log!("hatter: bildirim olayı yok ({e}), aralıklı bakılacak");
                let mut seen: HashSet<u32> = ids(&l).unwrap_or_default().into_iter().map(|(id, _)| id).collect();
                while RUNNING.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    let Some(now) = ids(&l) else { continue };
                    for (id, aumid) in &now {
                        if !seen.contains(id) {
                            post(dock, aumid.clone());
                        }
                    }
                    seen = now.into_iter().map(|(id, _)| id).collect();
                }
            }
        }
    });
}

pub fn stop() {
    RUNNING.store(false, Ordering::Relaxed);
}
