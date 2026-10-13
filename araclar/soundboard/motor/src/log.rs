//! Motorun logu: arayüz kendi log dosyasına yönlendirir (`set_sink`); verilmezse stderr.

use std::sync::OnceLock;

static SINK: OnceLock<fn(std::fmt::Arguments)> = OnceLock::new();

pub fn set_sink(f: fn(std::fmt::Arguments)) {
    let _ = SINK.set(f);
}

pub fn write(args: std::fmt::Arguments) {
    match SINK.get() {
        Some(f) => f(args),
        None => eprintln!("{args}"),
    }
}

#[macro_export]
macro_rules! log {
    ($($t:tt)*) => { $crate::log::write(format_args!($($t)*)) };
}
