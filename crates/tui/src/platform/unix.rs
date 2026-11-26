//! The terminal client's OS calls on Unix: `open` and `xdg-open`, termios
//! and signals, and `poll` and `read` on standard input.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Once, OnceLock};
use std::time::Duration;

pub(super) fn system_open(target: &str) -> std::io::Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    detached(Command::new(opener).arg(target))
}

pub(super) fn reveal(path: &Path) -> std::io::Result<()> {
    if cfg!(target_os = "macos") {
        return detached(Command::new("open").arg("-R").arg(path));
    }
    let folder = path.parent().unwrap_or(path);
    system_open(&folder.to_string_lossy())
}

/// Starts `command` with nothing on its standard streams, not waited for.
fn detached(command: &mut Command) -> std::io::Result<()> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Async-signal-safe terminal restore on SIGINT/SIGTERM/SIGHUP.
///
/// A tokio signal arm in the chrome loop would leave the process deaf to
/// signals while suspended for an attach (tokio's handler stays installed
/// process-wide, but nothing polls the stream mid-attach). Instead: a
/// low-level handler that restores the saved cooked termios (`tcsetattr`
/// is async-signal-safe), writes the leave-alt-screen/show-cursor bytes
/// (raw `write` is async-signal-safe), then re-raises the default
/// disposition so the process still dies exactly as expected. The restore
/// is deliberately unconditional: on a terminal that is already sane it is
/// a no-op, and that is what lets it cover both chrome mode and the
/// mid-attach passthrough phase.
pub(super) fn restore_on_signal(bytes: &'static [u8]) {
    static INSTALL: Once = Once::new();
    static SAVED_TERMIOS: OnceLock<libc::termios> = OnceLock::new();
    INSTALL.call_once(|| {
        // Save the cooked termios once, before raw mode first engages.
        let mut term = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut term) } == 0 {
            let _ = SAVED_TERMIOS.set(term);
        }
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            // Safety: the handler body calls only async-signal-safe
            // functions (tcsetattr, write) and signal_hook's re-raise.
            unsafe {
                let _ = signal_hook::low_level::register(sig, move || {
                    if let Some(term) = SAVED_TERMIOS.get() {
                        libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, term);
                    }
                    libc::write(libc::STDOUT_FILENO, bytes.as_ptr().cast(), bytes.len());
                    let _ = signal_hook::low_level::emulate_default_handler(sig);
                });
            }
        }
    });
}

pub(super) const ASKS_THE_TERMINAL: bool = true;

pub(super) fn stdin_readable(timeout: Duration) -> bool {
    let mut poll = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: `poll` reads and writes one valid pollfd for the duration of
    // the call.
    let ready = unsafe { libc::poll(&mut poll, 1, millis) };
    ready > 0 && poll.revents & libc::POLLIN != 0
}

pub(super) fn read_stdin(buffer: &mut [u8]) -> usize {
    // SAFETY: the buffer is valid and writable for its whole length, and
    // `read` writes at most that many bytes.
    let read = unsafe {
        libc::read(
            libc::STDIN_FILENO,
            buffer.as_mut_ptr().cast::<libc::c_void>(),
            buffer.len(),
        )
    };
    usize::try_from(read).unwrap_or(0)
}
