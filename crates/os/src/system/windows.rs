//! `os::system` on Windows: the version Windows records for itself.
//!
//! `GetVersionEx` answers what the program's manifest says it supports,
//! so the registry is read instead: `CurrentMajorVersionNumber`,
//! `CurrentMinorVersionNumber`, `CurrentBuildNumber` and `UBR` (the
//! update's revision) under `HKLM\SOFTWARE\Microsoft\Windows
//! NT\CurrentVersion`, which every Windows 10 and 11 keeps.

use std::ptr;

use windows_sys::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};

const CURRENT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

pub(super) fn version() -> Option<String> {
    let major = dword("CurrentMajorVersionNumber")?;
    let minor = dword("CurrentMinorVersionNumber")?;
    let build = string("CurrentBuildNumber")?;
    Some(match dword("UBR") {
        Some(revision) => format!("{major}.{minor}.{build}.{revision}"),
        None => format!("{major}.{minor}.{build}"),
    })
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn dword(name: &str) -> Option<u32> {
    let (key, name) = (wide(CURRENT_VERSION), wide(name));
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: both names are NUL-terminated; `value` holds the `size`
    // bytes a DWORD is.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            ptr::null_mut(),
            (&raw mut value).cast(),
            &mut size,
        )
    };
    (status == 0).then_some(value)
}

fn string(name: &str) -> Option<String> {
    let (key, name) = (wide(CURRENT_VERSION), wide(name));
    let mut buffer = [0u16; 64];
    let mut size = size_of_val(&buffer) as u32;
    // SAFETY: both names are NUL-terminated; `buffer` holds `size` bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let length = (size as usize / 2).min(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..length]);
    Some(text.trim_end_matches('\0').to_owned())
}
