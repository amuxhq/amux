//! `os::wake` on Unix: a socket pair, both ends non-blocking.

use std::io::{Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::net::UnixStream;

pub(super) struct Descriptor {
    reader: UnixStream,
    writer: UnixStream,
}

impl Descriptor {
    pub(super) fn new() -> std::io::Result<Descriptor> {
        let (reader, writer) = UnixStream::pair()?;
        reader.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        Ok(Descriptor { reader, writer })
    }

    pub(super) fn raw(&self) -> i32 {
        self.reader.as_raw_fd()
    }

    /// A full buffer is readable already.
    pub(super) fn ring(&self) {
        let _ = (&self.writer).write(&[1]);
    }

    pub(super) fn drain(&self) {
        let mut drained = [0_u8; 64];
        while matches!((&self.reader).read(&mut drained), Ok(read) if read > 0) {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::Descriptor;

    /// Whether the descriptor polls readable now.
    fn readable(wake: &Descriptor) -> bool {
        let mut fd = libc::pollfd {
            fd: wake.raw(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `fd` is one valid `pollfd`.
        let ready = unsafe { libc::poll(&mut fd, 1, 0) };
        assert!(ready >= 0, "{}", std::io::Error::last_os_error());
        fd.revents & libc::POLLIN != 0
    }

    #[test]
    fn a_ring_is_readable_until_drained() {
        let wake = Descriptor::new().unwrap();
        assert!(wake.raw() >= 0);
        assert!(!readable(&wake));
        wake.ring();
        assert!(readable(&wake));
        wake.ring();
        wake.drain();
        assert!(!readable(&wake));
    }

    /// Rings past what the buffer holds neither block nor go unseen.
    #[test]
    fn a_full_buffer_is_still_a_ring() {
        let wake = Descriptor::new().unwrap();
        for _ in 0..100_000 {
            wake.ring();
        }
        assert!(readable(&wake));
        wake.drain();
        assert!(!readable(&wake));
    }
}
