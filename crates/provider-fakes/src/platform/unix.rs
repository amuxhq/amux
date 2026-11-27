//! Unix: termios and `TIOCGWINSZ` for the terminal, SIGWINCH for its
//! resizes, Unix-domain sockets, and `setsid`.

use std::io;
use std::os::unix::process::CommandExt as _;
use std::path::Path;

pub(super) fn raw_mode() {
    // SAFETY: tcgetattr/tcsetattr on stdin with a zeroed, then filled,
    // termios owned by this frame.
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut termios) == 0 {
            libc::cfmakeraw(&mut termios);
            libc::tcsetattr(0, libc::TCSANOW, &termios);
        }
    }
}

pub(super) fn terminal_size() -> Option<(u16, u16)> {
    // SAFETY: TIOCGWINSZ fills the zeroed winsize owned by this frame.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    (unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut size) } == 0)
        .then_some((size.ws_row, size.ws_col))
}

pub(super) fn on_resize(mut changed: Box<dyn FnMut() -> bool + Send>) {
    let (ready, listening) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            let _ = ready.send(());
            return;
        };
        runtime.block_on(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let resized = signal(SignalKind::window_change());
            let _ = ready.send(());
            let Ok(mut resized) = resized else {
                return;
            };
            while resized.recv().await.is_some() {
                if !changed() {
                    return;
                }
            }
        });
    });
    let _ = listening.recv();
}

pub(super) fn own_session(cmd: &mut std::process::Command) {
    // SAFETY: setsid is async-signal-safe and touches nothing shared.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

pub type Stream = tokio::net::UnixStream;

pub(super) struct Listener(tokio::net::UnixListener);

impl Listener {
    pub(super) fn bind(path: &Path) -> io::Result<Self> {
        tokio::net::UnixListener::bind(path).map(Self)
    }

    pub(super) async fn accept(&self) -> io::Result<Stream> {
        self.0.accept().await.map(|(stream, _)| stream)
    }
}

pub(super) async fn connect(path: &Path) -> io::Result<Stream> {
    tokio::net::UnixStream::connect(path).await
}

pub type BlockingStream = std::os::unix::net::UnixStream;

pub(super) struct BlockingListener(std::os::unix::net::UnixListener);

impl BlockingListener {
    pub(super) fn bind(path: &Path) -> io::Result<Self> {
        std::os::unix::net::UnixListener::bind(path).map(Self)
    }

    pub(super) fn accept(&self) -> io::Result<BlockingStream> {
        self.0.accept().map(|(stream, _)| stream)
    }
}
