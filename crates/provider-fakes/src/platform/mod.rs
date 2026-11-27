//! What the fakes ask of the operating system and nothing else does: the
//! terminal a terminal fake draws in, the Unix-domain sockets Codex's
//! attach and terminal Claude's messaging are served on, and the session a
//! scripted tool shell leads. One API with an implementation per OS
//! (`unix.rs`, `windows.rs`), so the rest of the crate writes no `cfg`.
//!
//! Windows leaves attach and terminal Claude out, and amux hosts Codex
//! there over stdio, so the sockets there are none: binding or connecting
//! one is [`std::io::ErrorKind::Unsupported`], and their stream types have
//! no values.

use std::io;
use std::path::Path;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

/// Puts the terminal on standard input into raw mode, so keys arrive byte
/// for byte and output leaves unchanged; one that isn't a terminal (a test
/// driving stdio) is left alone. Unix: `cfmakeraw`. Windows: the console's
/// line, echo and Ctrl+C processing off, and its virtual terminal input on.
pub fn raw_mode() {
    imp::raw_mode();
}

/// The size of the terminal on standard output, rows then columns; `None`
/// when it isn't one. Windows: the console window's, not its buffer's.
pub fn terminal_size() -> Option<(u16, u16)> {
    imp::terminal_size()
}

/// Calls `changed` on a thread of its own each time the terminal may have
/// a new size, until it returns `false`; listening before this returns.
/// Unix: on each SIGWINCH. Windows, whose console signals nothing on a
/// resize: when a look every 20 ms finds a size other than the last.
pub fn on_resize(changed: impl FnMut() -> bool + Send + 'static) {
    imp::on_resize(Box::new(changed));
}

/// Readies `cmd` to lead a session of its own, as Claude's and Codex's
/// tool shells do, so a kill of its starter's process group misses it.
/// Unix: `setsid`. Windows: nothing; what a tool shell starts there stays
/// in its starter's job, as Claude's and Codex's own shells do.
pub fn own_session(cmd: &mut std::process::Command) {
    imp::own_session(cmd);
}

/// Whether this OS has the Unix-domain sockets below.
pub const UNIX_SOCKETS: bool = cfg!(unix);

/// A connection on a Unix-domain socket, tokio's: `AsyncRead`,
/// `AsyncWrite`, `Unpin` and `Send`. Windows: none is ever made.
pub type SocketStream = imp::Stream;

/// A Unix-domain socket listening at a path, for tokio.
pub struct SocketListener(imp::Listener);

impl SocketListener {
    /// Binds `path`, which must not exist. Windows: `Unsupported`.
    pub fn bind(path: &Path) -> io::Result<Self> {
        imp::Listener::bind(path).map(Self)
    }

    pub async fn accept(&self) -> io::Result<SocketStream> {
        self.0.accept().await
    }
}

/// Connects to the Unix-domain socket at `path`. Windows: `Unsupported`.
pub async fn connect(path: &Path) -> io::Result<SocketStream> {
    imp::connect(path).await
}

/// A connection on a Unix-domain socket, blocking: `Read` and `Send`.
/// Windows: none is ever made.
pub type BlockingStream = imp::BlockingStream;

/// A Unix-domain socket listening at a path, blocking.
pub struct BlockingListener(imp::BlockingListener);

impl BlockingListener {
    /// Binds `path`, which must not exist. Windows: `Unsupported`.
    pub fn bind(path: &Path) -> io::Result<Self> {
        imp::BlockingListener::bind(path).map(Self)
    }

    pub fn accept(&self) -> io::Result<BlockingStream> {
        self.0.accept()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_socket_binds_and_connects_where_there_are_sockets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake.sock");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        match BlockingListener::bind(&path) {
            Ok(listener) => serve_one(listener, &runtime, &path),
            Err(error) => {
                if UNIX_SOCKETS {
                    panic!("binding {}: {error}", path.display());
                }
                assert_eq!(error.kind(), io::ErrorKind::Unsupported);
                let connected = runtime.block_on(connect(&path)).map(drop);
                assert_eq!(
                    connected.err().map(|error| error.kind()),
                    Some(io::ErrorKind::Unsupported)
                );
                assert_eq!(
                    SocketListener::bind(&path).err().map(|error| error.kind()),
                    Some(io::ErrorKind::Unsupported)
                );
            }
        }
    }

    /// A client connected to `listener` and accepted. Windows never calls
    /// it: its listener has no value.
    #[allow(unreachable_code, unused_variables)]
    fn serve_one(listener: BlockingListener, runtime: &tokio::runtime::Runtime, path: &Path) {
        let accepted = std::thread::spawn(move || listener.accept().map(drop));
        runtime.block_on(connect(path)).unwrap();
        accepted.join().unwrap().unwrap();
    }
}
