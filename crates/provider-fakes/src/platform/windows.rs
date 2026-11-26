//! Windows: the console's modes and window for the terminal, a look every
//! 20 ms for its resizes, and no Unix-domain sockets or sessions.

use std::io;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
    ENABLE_VIRTUAL_TERMINAL_INPUT, GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
};

pub(super) fn raw_mode() {
    // A console reads whole echoed lines and turns Ctrl-C into a signal
    // until those modes are off; then keys arrive as the bytes typed.
    // SAFETY: console mode calls on this process's standard input handle,
    // with a mode owned by this frame.
    unsafe {
        let input = GetStdHandle(STD_INPUT_HANDLE);
        let mut mode = 0;
        if GetConsoleMode(input, &mut mode) != 0 {
            mode &= !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT);
            SetConsoleMode(input, mode | ENABLE_VIRTUAL_TERMINAL_INPUT);
        }
    }
}

pub(super) fn terminal_size() -> Option<(u16, u16)> {
    // SAFETY: the zeroed buffer info owned by this frame is filled by the
    // call on this process's standard output handle.
    let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
    if unsafe { GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) } == 0 {
        return None;
    }
    let window = info.srWindow;
    let rows = u16::try_from(window.Bottom - window.Top + 1).ok()?;
    let columns = u16::try_from(window.Right - window.Left + 1).ok()?;
    Some((rows, columns))
}

pub(super) fn on_resize(mut changed: Box<dyn FnMut() -> bool + Send>) {
    let mut seen = terminal_size();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(20));
            let size = terminal_size();
            if size != seen {
                seen = size;
                if !changed() {
                    return;
                }
            }
        }
    });
}

pub(super) fn own_session(_cmd: &mut std::process::Command) {}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "the fakes serve Unix-domain sockets only for what Windows leaves out: \
         attach, and terminal Claude's messaging",
    )
}

/// No value: no socket is ever bound or connected.
pub enum Stream {}

impl tokio::io::AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match *self {}
    }
}

impl tokio::io::AsyncWrite for Stream {
    fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, _: &[u8]) -> Poll<io::Result<usize>> {
        match *self {}
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        match *self {}
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        match *self {}
    }
}

pub(super) enum Listener {}

impl Listener {
    pub(super) fn bind(_: &Path) -> io::Result<Self> {
        Err(unsupported())
    }

    pub(super) async fn accept(&self) -> io::Result<Stream> {
        match *self {}
    }
}

pub(super) async fn connect(_: &Path) -> io::Result<Stream> {
    Err(unsupported())
}

/// No value: no socket is ever bound.
pub enum BlockingStream {}

impl io::Read for BlockingStream {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        match *self {}
    }
}

pub(super) enum BlockingListener {}

impl BlockingListener {
    pub(super) fn bind(_: &Path) -> io::Result<Self> {
        Err(unsupported())
    }

    pub(super) fn accept(&self) -> io::Result<BlockingStream> {
        match *self {}
    }
}
