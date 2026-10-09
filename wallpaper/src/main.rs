// Sürüm derlemesinde konsol penceresi açılmaz; araç komutları üst konsola bağlanır.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod compile;
mod config;
mod desktop;
mod format;
mod gpu;
mod headless;
mod hub;
mod log;
mod png;
mod policy;
mod render;
mod setup;
mod shell;
mod timer;
mod tray;
mod update;
mod util;
mod weather;

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::core::w;

use util::Res;

const USAGE: &str = "\
cheshire — GPU shader duvar kâğıdı motoru

kullanım:
  cheshire                                        masaüstünde çalıştır (kurulu değilse önce kendini kurar)
  cheshire dosya.cheshire                         duvar kâğıdını ekle ve uygula
  cheshire --dogrula dosya.cheshire               derle, 120 kare çiz, GPU süresini raporla
  cheshire --onizleme dosya.cheshire cikti.png    PNG üret
        [--zaman 5] [--fare 0.5,0.5] [--basili] [--boyut 1280x720] [--param ad=deger]...
        [--hava bulut,yagmur,kar,sis]
  cheshire --agac                                 masaüstü pencere ağacını logla
  cheshire --kur                                  bu exe'yi kur (geliştirme kopyası için)
  cheshire --kaldir                               kaldır: kayıtlar, kısayol ve kurulum klasörü
  cheshire --hub <pencere>                        hive modu: tepsi simgesi yok, komutlar hive'dan";

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn pair(s: &str, sep: char) -> Option<(&str, &str)> {
    s.split_once(sep).map(|(a, b)| (a.trim(), b.trim()))
}

fn preview_options(args: &[String]) -> Result<headless::PreviewOptions, String> {
    let time = flag(args, "--zaman").map_or(Ok(5.0), |t| t.parse().map_err(|_| "--zaman sayı olmalı"))?;
    let mouse = match flag(args, "--fare") {
        Some(f) => {
            let (x, y) = pair(f, ',').ok_or("--fare x,y biçiminde olmalı")?;
            Some([x.parse().map_err(|_| "--fare sayı olmalı")?, y.parse().map_err(|_| "--fare sayı olmalı")?])
        }
        None => None,
    };
    let size = match flag(args, "--boyut") {
        Some(s) => {
            let (w, h) = pair(s, 'x').ok_or("--boyut GxY biçiminde olmalı")?;
            (w.parse().map_err(|_| "--boyut sayı olmalı")?, h.parse().map_err(|_| "--boyut sayı olmalı")?)
        }
        None => (1280, 720),
    };
    let mut params = Vec::new();
    for (i, a) in args.iter().enumerate() {
        if a == "--param" {
            let p = args.get(i + 1).ok_or("--param ad=deger bekliyor")?;
            let (k, v) = pair(p, '=').ok_or("--param ad=deger biçiminde olmalı")?;
            params.push((k.to_string(), v.to_string()));
        }
    }
    let mut weather = [0.0f32; 4];
    if let Some(h) = flag(args, "--hava") {
        for (w, v) in weather.iter_mut().zip(h.split(',')) {
            *w = v.trim().parse().map_err(|_| "--hava bulut,yağmur,kar,sis (0..1) olmalı")?;
        }
    }
    Ok(headless::PreviewOptions { time, mouse, pressed: args.iter().any(|a| a == "--basili"), size, params, weather })
}

/// Araç komutları: `Some(çıkış kodu)`; masaüstü modu için `None`.
fn tool(args: &[String]) -> Option<i32> {
    let result = match args.get(1).map(String::as_str) {
        Some("--dogrula") => match args.get(2) {
            Some(p) => headless::validate(&PathBuf::from(p)),
            None => Err("--dogrula dosya.cheshire".into()),
        },
        Some("--onizleme") => match (args.get(2), args.get(3), preview_options(args)) {
            (Some(p), Some(o), Ok(opts)) => headless::preview(&PathBuf::from(p), &PathBuf::from(o), &opts),
            (_, _, Err(e)) => Err(e.into()),
            _ => Err("--onizleme dosya.cheshire cikti.png".into()),
        },
        Some("--agac") => {
            let _ = desktop::WallpaperWindow::attach("auto").map(std::mem::forget);
            println!("{}", desktop::dump_tree());
            desktop::restore_static_wallpaper();
            Ok(true)
        }
        Some("--kur") => setup::install(None).map(|()| true),
        Some("--kaldir") => setup::uninstall(),
        Some("--yardim" | "-h" | "--help") => {
            println!("{USAGE}");
            Ok(true)
        }
        _ => return None,
    };
    Some(match result {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => {
            println!("HATA {e}");
            2
        }
    })
}

/// `.wallpaper` dosyasını `duvarlar` klasörüne kopyalar, dosya adını döndürür.
fn install(src: &Path) -> Res<String> {
    let name = src.file_name().ok_or("geçersiz dosya yolu")?.to_string_lossy().into_owned();
    app::ensure_wallpapers();
    let dst = util::wallpapers_dir().join(&name);
    if std::fs::canonicalize(src).ok() != std::fs::canonicalize(&dst).ok() {
        std::fs::copy(src, &dst)?;
    }
    log!("kuruldu: {name}");
    Ok(name)
}

/// Masaüstü modunun argümanları: `[dosya.wallpaper] [--kuruldu] [--guncellendi] [--sonra PID]`.
#[derive(Default)]
struct Launch<'a> {
    file: Option<&'a str>,
    installed: bool,
    updated: bool,
    after: Option<u32>,
    /// hive penceresi (`--hub`).
    hub: Option<isize>,
}

fn launch_args(args: &[String]) -> Launch<'_> {
    let mut l = Launch::default();
    let mut it = args.iter().skip(1).map(String::as_str);
    while let Some(a) = it.next() {
        match a {
            "--kuruldu" => l.installed = true,
            "--guncellendi" => l.updated = true,
            "--sonra" => l.after = it.next().and_then(|p| p.parse().ok()),
            "--hub" => l.hub = it.next().and_then(|p| p.parse().ok()),
            a if !a.starts_with("--") && l.file.is_none() => l.file = Some(a),
            _ => {}
        }
    }
    l
}

fn desktop(args: &[String]) -> Res<()> {
    let launch = launch_args(args);
    // İndirilen exe: kendini kur, kurulu kopyayı başlat, çık. hive kendi kurar.
    if launch.hub.is_none() && !shell::installed_copy() && !shell::dev_copy() {
        return setup::install(launch.file);
    }
    if shell::installed_copy() {
        update::cleanup();
    }
    setup::migrate();

    let installed = launch.file.map(|f| install(Path::new(f))).transpose()?;
    // Tek örnek: motor zaten çalışıyorsa dosyayı ona ilet ve çık.
    let _mutex = unsafe { CreateMutexW(None, true, w!("Local\\cheshire-tek-kopya")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        if let Some(name) = &installed {
            tray::send_to_running(name);
        }
        if let Some(h) = launch.hub {
            tray::send_command(&format!("hub {h}"));
        }
        return Ok(());
    }
    // Log ancak tek kopya olduğumuz kesinleşince açılır: dosya iletmek için açılan ikinci kopya
    // çalışan motorun logunu sıfırlamasın.
    log::init();
    if let Some(h) = launch.hub {
        hub::set(h);
    }
    if let Some(name) = installed {
        let mut cfg = config::Config::load();
        cfg.wallpaper = name;
        cfg.save();
    }
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let app = app::App::new()?;
    if launch.installed {
        app.announce("cheshire kuruldu", "Duvar kâğıdını ve ayarları buradaki simgeden seçebilirsin.");
    } else if launch.updated {
        app.announce(concat!("cheshire ", env!("CARGO_PKG_VERSION"), " sürümüne güncellendi"), env!("CARGO_PKG_REPOSITORY"));
    }
    app.run()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // Güncellemeden sonra: eski süreç çıksın, tek örnek kilidini ve log dosyasını bıraksın.
    if let Some(pid) = launch_args(&args).after {
        setup::wait_for(pid);
    }
    if args.get(1).is_some_and(|a| a.starts_with("--") || a == "-h") {
        // GUI alt sisteminde stdout yoksa çağıran konsola bağlan (cmd/PowerShell).
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
    // Araç komutları (önizleme vb.) çalışan motorun logunu sıfırlamasın.
    if let Some(code) = tool(&args) {
        std::process::exit(code);
    }
    if let Err(e) = desktop(&args) {
        log!("hata: {e}");
        util::error_box(&format!("cheshire başlatılamadı:\n\n{e}"));
    }
}
