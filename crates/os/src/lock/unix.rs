//! `os::lock` on Unix: std's `flock` over the whole file.

use std::fs::{File, TryLockError};
use std::io;

/// Takes the lock, or `false` when another holds it.
pub(super) fn try_lock(file: &File) -> io::Result<bool> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

pub(super) fn unlock(file: &File) {
    let _ = file.unlock();
}
