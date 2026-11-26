//! What only Unix has of the bridge's waking: the stop latch a watcher
//! polls beside the runtime's wake descriptor.

/// A runtime's stop, signalled once and readable for good: nothing ever
/// reads it. The runtime's own wake descriptor cannot carry a stop, because
/// every step reads it empty: a step racing the stop would swallow the wake
/// and leave the watcher in its poll, holding the runtime the stop waits for.
pub(crate) struct StopLatch {
    reader: std::os::unix::net::UnixStream,
    writer: std::os::unix::net::UnixStream,
}

impl StopLatch {
    pub(crate) fn new() -> Result<Self, String> {
        let (reader, writer) = std::os::unix::net::UnixStream::pair()
            .and_then(|(reader, writer)| {
                reader.set_nonblocking(true)?;
                writer.set_nonblocking(true)?;
                Ok((reader, writer))
            })
            .map_err(|error| format!("failed to create the runtime's stop latch: {error}"))?;
        Ok(Self { reader, writer })
    }

    pub(crate) fn set(&self) {
        use std::io::Write as _;
        // A full buffer is readable already.
        let _ = (&self.writer).write(&[1]);
    }

    /// Waits for the runtime's wake descriptor or `timeout_ms` (-1 for
    /// ever): -1 once the latch is set or on failure, 1 when `wake_fd` is
    /// readable, 0 on timeout or a signal.
    pub(crate) fn wait(&self, wake_fd: i32, timeout_ms: i32) -> i32 {
        use std::os::fd::AsRawFd as _;
        if wake_fd < 0 {
            return -1;
        }
        let watched = |fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let mut fds = [watched(wake_fd), watched(self.reader.as_raw_fd())];
        // SAFETY: `fds` holds two valid `pollfd` entries.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
        if rc < 0 {
            let interrupted = std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR);
            return if interrupted { 0 } else { -1 };
        }
        let ready = libc::POLLIN | libc::POLLERR | libc::POLLHUP;
        if fds[1].revents != 0 {
            -1
        } else if fds[0].revents & ready != 0 {
            1
        } else {
            0
        }
    }
}

#[cfg(test)]
#[path = "unix_tests.rs"]
mod tests;
