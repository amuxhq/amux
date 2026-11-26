//! macOS and iOS.

use std::io;

/// The kernel's id for this boot session.
pub fn boot_id() -> io::Result<String> {
    let name = c"kern.bootsessionuuid";
    let mut buf = [0u8; 64];
    let mut len = buf.len();
    // SAFETY: `buf` and `len` describe a writable buffer that outlives
    // the call; the name is a NUL-terminated string.
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
        return Err(io::Error::last_os_error());
    }
    let text = &buf[..len.min(buf.len())];
    let text = text.split(|byte| *byte == 0).next().unwrap_or_default();
    Ok(String::from_utf8_lossy(text).into_owned())
}
