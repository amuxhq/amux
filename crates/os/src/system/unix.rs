//! `os::system` on Unix: `sysctl` on Apple systems, the system property
//! on Android, `uname` elsewhere; `gethostname`, and on macOS `scutil`'s
//! Computer Name.

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(super) fn version() -> Option<String> {
    let name = c"kern.osproductversion";
    let mut buf = [0u8; 64];
    let mut len = buf.len();
    // SAFETY: `buf` and `len` describe a writable buffer that outlives the
    // call; the name is a NUL-terminated string.
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return None;
    }
    let text = &buf[..len.min(buf.len())];
    let text = text.split(|byte| *byte == 0).next().unwrap_or_default();
    Some(String::from_utf8_lossy(text).into_owned())
}

/// The Android release ("14"), not the kernel's, which says nothing of the
/// system a person has.
#[cfg(target_os = "android")]
pub(super) fn version() -> Option<String> {
    let mut value = [0 as std::ffi::c_char; libc::PROP_VALUE_MAX as usize];
    // SAFETY: the name is NUL-terminated, and `value` holds the
    // PROP_VALUE_MAX bytes the call may write, NUL included.
    let len = unsafe {
        libc::__system_property_get(c"ro.build.version.release".as_ptr(), value.as_mut_ptr())
    };
    if len <= 0 {
        return None;
    }
    // SAFETY: the call NUL-terminates what it writes.
    let release = unsafe { std::ffi::CStr::from_ptr(value.as_ptr()) };
    Some(release.to_string_lossy().into_owned())
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
pub(super) fn version() -> Option<String> {
    // SAFETY: uname fills the zeroed struct it is handed.
    let mut name: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut name) } != 0 {
        return None;
    }
    // SAFETY: uname NUL-terminates each field.
    let release = unsafe { std::ffi::CStr::from_ptr(name.release.as_ptr()) };
    Some(release.to_string_lossy().into_owned())
}

/// The Computer Name a Mac keeps apart from its host name.
#[cfg(target_os = "macos")]
pub(super) fn computer_name() -> Option<String> {
    let output = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8(output.stdout).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Every other Unix has only its host name.
#[cfg(not(target_os = "macos"))]
pub(super) fn computer_name() -> Option<String> {
    None
}

pub(super) fn host_name() -> Option<String> {
    // POSIX bounds a host name at 255 bytes, so this holds one and its NUL.
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is writable for the `buf.len()` bytes handed over.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return None;
    }
    let end = buf.iter().position(|byte| *byte == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..end].to_vec()).ok()
}
