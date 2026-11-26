//! The terminal client's OS calls on Windows: the shell's own handler for
//! links and files. The terminal is never asked anything here, so nothing
//! reads its answers.

use std::path::Path;
use std::time::Duration;

use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// Through `ShellExecuteW`, never `cmd`, whose parser would read `&` or
/// `|` in a link as command separators.
pub(super) fn system_open(target: &str) -> std::io::Result<()> {
    let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (verb, file) = (wide("open"), wide(target));
    // SAFETY: both strings are NUL-terminated UTF-16 buffers that outlive the
    // call; the remaining pointers are documented as optional.
    let instance = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW reports success as a value greater than 32.
    if instance as isize > 32 {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "the system could not open it (code {})",
            instance as isize
        )))
    }
}

/// The folder opened in Explorer.
pub(super) fn reveal(path: &Path) -> std::io::Result<()> {
    let folder = path.parent().unwrap_or(path);
    system_open(&folder.to_string_lossy())
}

/// Nothing: a console puts its own input mode back when the process ends.
pub(super) fn restore_on_signal(_bytes: &'static [u8]) {}

/// The legacy console answers neither the colour queries nor device
/// attributes, and would print them.
pub(super) const ASKS_THE_TERMINAL: bool = false;

pub(super) fn stdin_readable(_timeout: Duration) -> bool {
    false
}

pub(super) fn read_stdin(_buffer: &mut [u8]) -> usize {
    0
}
