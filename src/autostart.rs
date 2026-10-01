//! Start at login: an XDG autostart entry on Linux, the per-user Run key on Windows.

use std::io;
use std::path::PathBuf;

/// What to start: the AppImage itself when running from one, since its mount path changes.
fn exe() -> io::Result<PathBuf> {
    match std::env::var_os("APPIMAGE") {
        Some(path) => Ok(path.into()),
        None => std::env::current_exe(),
    }
}

#[cfg(target_os = "linux")]
fn entry() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(config.join("autostart").join("yunta.desktop"))
}

#[cfg(target_os = "linux")]
pub fn enabled() -> bool {
    entry().is_some_and(|p| p.exists())
}

#[cfg(target_os = "linux")]
pub fn set(on: bool) -> io::Result<()> {
    let path = entry().ok_or_else(|| io::Error::other("no home directory"))?;
    if !on {
        return std::fs::remove_file(path).or_else(|e| if e.kind() == io::ErrorKind::NotFound { Ok(()) } else { Err(e) });
    }
    std::fs::create_dir_all(path.parent().expect("has a parent"))?;
    let exe = exe()?;
    std::fs::write(path, format!("[Desktop Entry]\nType=Application\nName=Yunta\nIcon=yunta\nExec=\"{}\"\nNoDisplay=true\n", exe.display()))
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    pub fn enabled() -> bool {
        let (key, name) = (wide(RUN), wide("Yunta"));
        let found = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        found == ERROR_SUCCESS
    }

    pub fn set(on: bool) -> std::io::Result<()> {
        let (key, name) = (wide(RUN), wide("Yunta"));
        let status = if on {
            let value = wide(&format!("\"{}\"", super::exe()?.display()));
            unsafe {
                RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), REG_SZ, value.as_ptr().cast(), (value.len() * 2) as u32)
            }
        } else {
            unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) }
        };
        match status {
            ERROR_SUCCESS => Ok(()),
            ERROR_FILE_NOT_FOUND if !on => Ok(()),
            code => Err(std::io::Error::from_raw_os_error(code as i32)),
        }
    }
}

#[cfg(windows)]
pub use win::{enabled, set};
