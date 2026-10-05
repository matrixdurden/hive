use std::fs::File;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();
static FILE: Mutex<Option<File>> = Mutex::new(None);

/// Her açılışta %LOCALAPPDATA%\Programs\hive\<ad> dosyasını sıfırdan başlatır.
pub fn init(name: &str) {
    START.get_or_init(Instant::now);
    let dir = crate::util::data_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(f) = File::create(dir.join(name)) {
        *FILE.lock().unwrap() = Some(f);
    }
}

pub fn write(args: std::fmt::Arguments) {
    let t = START.get_or_init(Instant::now).elapsed().as_secs_f32();
    let line = format!("[{t:>8.2}] {args}\n");
    eprint!("{line}");
    if let Some(f) = FILE.lock().unwrap().as_mut() {
        let _ = f.write_all(line.as_bytes());
    }
}

#[macro_export]
macro_rules! log {
    ($($t:tt)*) => { $crate::log::write(format_args!($($t)*)) };
}
