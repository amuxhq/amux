//! `os::lock` on Windows: `LockFileEx` on one byte past any data.

use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle as _;

use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
use windows_sys::Win32::Storage::FileSystem::{
    LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx, UnlockFileEx,
};
use windows_sys::Win32::System::IO::OVERLAPPED;

use super::LOCKED_BYTE;

fn at_locked_byte() -> OVERLAPPED {
    let mut overlapped = OVERLAPPED::default();
    overlapped.Anonymous.Anonymous.Offset = LOCKED_BYTE as u32;
    overlapped.Anonymous.Anonymous.OffsetHigh = (LOCKED_BYTE >> 32) as u32;
    overlapped
}

/// Takes the lock, or `false` when another handle holds that byte, or any
/// range holding it (std's whole-file lock).
pub(super) fn try_lock(file: &File) -> io::Result<bool> {
    let mut overlapped = at_locked_byte();
    // SAFETY: `file` is open for reading or writing, synchronously, so the
    // call completes before `overlapped` goes.
    if unsafe {
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    } != 0
    {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        return Ok(false);
    }
    Err(error)
}

pub(super) fn unlock(file: &File) {
    let mut overlapped = at_locked_byte();
    // SAFETY: as in `try_lock`; unlocking a byte not locked fails harmlessly.
    unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut overlapped) };
}
