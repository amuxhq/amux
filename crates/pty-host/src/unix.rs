//! What only Unix has of hosting a pseudo-terminal: signalling the hosted
//! process group, and on macOS the setup a fork needs first.

/// Completes libnotify's one-time setup before this process first forks.
///
/// On macOS the child side of `fork` runs libSystem's atfork handlers, and
/// libnotify's reads its once-initialised globals: if another thread was
/// partway through that initialisation at the fork, the child finds the
/// once gate held by a thread it does not have and aborts before `exec`
/// ("os_once_t is corrupt ... crashed on child side of fork pre-exec"). The
/// hosted process then dies without output and its status reads as a
/// signal. The first use of libnotify can come from anything on another
/// thread (a passwd or DNS lookup, portable-pty resolving a home directory
/// with `HOME` unset), so it is finished here, once, before the first
/// spawn; a spawn racing it waits for it.
#[cfg(target_os = "macos")]
pub(crate) fn prepare_fork() {
    unsafe extern "C" {
        fn notify_register_check(name: *const libc::c_char, token: *mut libc::c_int) -> u32;
        fn notify_cancel(token: libc::c_int) -> u32;
    }
    static PREPARED: std::sync::Once = std::sync::Once::new();
    PREPARED.call_once(|| {
        let mut token = 0;
        // SAFETY: the name is a NUL-terminated string and the token a valid
        // out-pointer; a registered token is cancelled at once.
        unsafe {
            if notify_register_check(c"sh.amux.pty-host.prepare".as_ptr(), &mut token) == 0 {
                notify_cancel(token);
            }
        }
    });
}

pub(crate) fn signal_group(pid: u32, signal: libc::c_int) -> std::io::Result<()> {
    let pid = i32::try_from(pid).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("PTY process id {pid} does not fit in a signed process id"),
        )
    })?;
    // portable-pty creates a session whose leader is the spawned child, so a
    // negative pid addresses the child and every provider shim it launched.
    let result = unsafe { libc::kill(-pid, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(error)
}

#[cfg(test)]
#[path = "unix_tests.rs"]
mod tests;
