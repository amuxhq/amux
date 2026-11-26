//! `os::system` on Unix: `sysctl` on Apple systems, the system property
//! on Android, `uname` elsewhere.

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
