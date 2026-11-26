//! What the terminal client asks of the operating system: handing a link
//! or a file to the system's handler, putting the terminal back when a
//! signal ends the process, and reading the terminal's answers straight
//! off standard input. One API with an implementation per OS (`unix.rs`,
//! `windows.rs`), so the rest of the crate writes no `cfg`.

use std::path::Path;
use std::time::Duration;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

/// Hands `target`, a link or a path, to the system's own handler: the
/// browser for a link, the file's app for a file, the file manager for a
/// folder. Never through a shell, whose parser would read `&` or `|` in a
/// link as command separators.
pub(crate) fn system_open(target: &str) -> std::io::Result<()> {
    imp::system_open(target)
}

/// Shows the file at `path` in its folder: selected in the Finder on
/// macOS, the folder opened elsewhere.
pub(crate) fn reveal(path: &Path) -> std::io::Result<()> {
    imp::reveal(path)
}

/// Installs, once, what puts the terminal back as it was (cooked mode,
/// the main screen, the cursor shown: `bytes`) when the process is told to
/// stop, so a shell is never left in raw mode. Unix: an async-signal-safe
/// handler for SIGINT, SIGTERM and SIGHUP that restores the termios saved
/// at installation, writes `bytes`, then dies as the signal's default
/// would. Windows: nothing; a console restores its own input mode when
/// the process ends.
pub(crate) fn restore_on_signal(bytes: &'static [u8]) {
    imp::restore_on_signal(bytes);
}

/// Whether standard input has bytes to read within `timeout`.
/// Windows: always `false` (the terminal's answers are not read there).
pub(crate) fn stdin_readable(timeout: Duration) -> bool {
    imp::stdin_readable(timeout)
}

/// Reads what standard input has into `buffer`, answering how many bytes;
/// 0 at its end or on an error. Windows: always 0.
pub(crate) fn read_stdin(buffer: &mut [u8]) -> usize {
    imp::read_stdin(buffer)
}
