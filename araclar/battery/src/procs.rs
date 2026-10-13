//! Süreçler: listeleme ve uygulama başına ekran kartı tercihi.

use windows::Win32::Foundation::*;
use windows::Win32::System::Diagnostics::ToolHelp::*;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::*;
use windows::core::{PCWSTR, PWSTR};

use ortak::util::{self, wide};

#[derive(Clone, Debug)]
pub struct Proc {
    pub pid: u32,
    /// Exe adı, ".exe" olmadan.
    pub name: String,
}

fn stem(name: &str) -> String {
    match name.len() > 4 && name[name.len() - 4..].eq_ignore_ascii_case(".exe") {
        true => name[..name.len() - 4].to_string(),
        false => name.to_string(),
    }
}

pub fn list() -> Vec<Proc> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                out.push(Proc { pid: e.th32ProcessID, name: stem(&String::from_utf16_lossy(&e.szExeFile[..len])) });
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Sürecin exe yolu (erişilemeyen sistem süreçleri için `None`).
pub fn path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

// --- Uygulama başına ekran kartı tercihi (Ayarlar › Ekran › Grafik) ---

const GPU_KEY: &str = r"Software\Microsoft\DirectX\UserGpuPreferences";
/// "Güç tasarrufu" = tümleşik ekran kartı.
pub const GPU_POWER_SAVING: &str = "GpuPreference=1;";

pub fn gpu_pref(path: &str) -> Option<String> {
    util::reg_string(HKEY_CURRENT_USER, GPU_KEY, path)
}

/// `None` değeri siler. Uygulama bir sonraki açılışında yeni tercihle başlar.
pub fn set_gpu_pref(path: &str, value: Option<&str>) -> bool {
    let (k, n) = (wide(GPU_KEY), wide(path));
    unsafe {
        match value {
            Some(v) => {
                let d = wide(v);
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    PCWSTR(k.as_ptr()),
                    PCWSTR(n.as_ptr()),
                    REG_SZ.0,
                    Some(d.as_ptr().cast()),
                    (d.len() * 2) as u32,
                )
                .0 == 0
            }
            None => RegDeleteKeyValueW(HKEY_CURRENT_USER, PCWSTR(k.as_ptr()), PCWSTR(n.as_ptr())).0 == 0,
        }
    }
}

fn push_unique(list: &mut Vec<String>, p: String) {
    if !list.iter().any(|x| x.eq_ignore_ascii_case(&p)) {
        list.push(p);
    }
}

/// Adı listede olan uygulamaların exe yolları: çalışanlardan ve bilinen kurulum yerlerinden.
pub fn igpu_targets(names: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for p in list() {
        if names.iter().any(|n| n.eq_ignore_ascii_case(&p.name))
            && let Some(path) = path(p.pid)
        {
            push_unique(&mut out, path);
        }
    }
    let var = |k: &str| std::env::var_os(k).map(std::path::PathBuf::from);
    let has = |n: &str| names.iter().any(|x| x.eq_ignore_ascii_case(n));
    let mut known = Vec::new();
    if has("brave") {
        for pf in ["ProgramFiles", "ProgramFiles(x86)"].into_iter().filter_map(var) {
            known.push(pf.join(r"BraveSoftware\Brave-Browser\Application\brave.exe"));
        }
    }
    if has("Spotify")
        && let Some(a) = var("APPDATA")
    {
        known.push(a.join(r"Spotify\Spotify.exe"));
    }
    if has("Code")
        && let Some(l) = var("LOCALAPPDATA")
    {
        known.push(l.join(r"Programs\Microsoft VS Code\Code.exe"));
    }
    if has("Discord")
        && let Some(l) = var("LOCALAPPDATA")
    {
        // Her güncelleme yeni bir app-x.y.z klasörü açar.
        for e in std::fs::read_dir(l.join("Discord")).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with("app-") {
                known.push(e.path().join("Discord.exe"));
            }
        }
    }
    for k in known.into_iter().filter(|p| p.is_file()) {
        push_unique(&mut out, k.display().to_string());
    }
    out
}
