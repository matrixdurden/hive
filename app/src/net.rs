//! HTTPS indirme (WinHTTP; yönlendirmeleri ve sistem proxy'sini kendisi izler) ve SHA-256.

use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
use windows::core::{PCWSTR, w};

use crate::util::{Res, wide};

const MAX_SIZE: usize = 128 << 20;

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

/// HTTPS GET: (durum kodu, gövde).
fn get(url: &str) -> Res<(u32, Vec<u8>)> {
    let rest = url.strip_prefix("https://").ok_or("yalnızca https")?;
    let (host, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
    let agent = wide(concat!("hive/", env!("CARGO_PKG_VERSION")));
    let (host, path) = (wide(host), wide(&path));
    unsafe {
        let session = Handle::new(WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ))?;
        let connect = Handle::new(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), INTERNET_DEFAULT_HTTPS_PORT, 0))?;
        let request = Handle::new(WinHttpOpenRequest(
            connect.0,
            w!("GET"),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ))?;
        WinHttpSendRequest(request.0, None, None, 0, 0, 0)?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())?;

        let (mut status, mut len) = (0u32, 4u32);
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut len,
            std::ptr::null_mut(),
        )?;
        let mut body = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut n = 0u32;
            WinHttpReadData(request.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut n)?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&buf[..n as usize]);
            if body.len() > MAX_SIZE {
                return Err("yanıt çok büyük".into());
            }
        }
        Ok((status, body))
    }
}

pub fn download(url: &str) -> Res<Vec<u8>> {
    match get(url)? {
        (200, body) => Ok(body),
        (status, _) => Err(format!("HTTP {status}: {url}").into()),
    }
}

pub fn sha256(data: &[u8]) -> Res<String> {
    let mut out = [0u8; 32];
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out).ok()? };
    Ok(out.iter().map(|b| format!("{b:02x}")).collect())
}
