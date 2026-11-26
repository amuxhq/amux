//! `amux-launch <args>`: runs `amux <args>` from the folder it is in, and
//! starts it again when it dies. It is what Windows' login item runs
//! (`amux-launch supervise --config <path>`), as launchd's `KeepAlive` and
//! systemd's `Restart=` are on macOS and Linux: the restart policy, with no
//! console of its own (it is built for the GUI subsystem) and no window for
//! what it starts (`os::process::background`).
//!
//! It knows no pid, reads no exit code and takes no lock. It starts amux
//! with the write end of a pipe named in `AMUX_LAUNCH_LINEAGE`, and reads
//! the pipe:
//!
//! - a byte, then the end: amux stopped on purpose (`amux server stop`),
//!   and the launcher exits;
//! - the end with nothing written: the line died, and amux is started again
//!   after a backoff, 1 s doubling to 60 s, and back to 1 s after a line
//!   that ran 10 minutes, unless amux is gone from the folder by then
//!   (uninstalled), when the launcher exits.
//!
//! `amux supervise` keeps the pipe for life and hands it to the successor
//! it starts at a self-update, so an update never reads as a death.
//!
//! Every start after the first carries `AMUX_LAUNCH_RESTART=1`. A stop
//! while the launcher waits finds no supervisor to tell, so `amux server
//! stop` leaves word in amux's root, which the launcher can't find; amux
//! started with this reads it, writes the byte and exits, and the launcher
//! exits too.
//!
//! The command line, `AMUX_LAUNCH_LINEAGE` and `AMUX_LAUNCH_RESTART` are
//! promised from the first release that ships this (AGENTS.md): a login
//! item written by one release starts whichever release is installed
//! later.

#![windows_subsystem = "windows"]

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Names the pipe's write end to amux, as a handle value (a descriptor on
/// Unix): `node::supervisor::LINEAGE_ENV`, which this crate can't link.
const LINEAGE_ENV: &str = "AMUX_LAUNCH_LINEAGE";

/// Tells amux a start is a restart after its line died:
/// `node::supervisor::RESTART_ENV`.
const RESTART_ENV: &str = "AMUX_LAUNCH_RESTART";

fn main() {
    os::process::harden_start();
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let Some(amux) = beside("amux") else {
        // Nowhere to say it: there is no console. Nothing can be started.
        std::process::exit(2);
    };
    // The folder it was started in, which a login item doesn't choose.
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
    let mut backoff = Backoff::default();
    let mut restart = false;
    loop {
        let started = Instant::now();
        if let Ok(Ended::Stopped) = run(&amux, &args, &cwd, restart) {
            return;
        }
        // A start that failed (amux moved aside mid-update, say) is a
        // death too, and is tried again.
        std::thread::sleep(backoff.after(started.elapsed()));
        // No amux beside it any more (uninstalled while it waited): there
        // is nothing to start, and no supervisor will ever end the line.
        if !amux.exists() {
            return;
        }
        restart = true;
    }
}

/// `name`, the executable, in the folder this one is in.
fn beside(name: &str) -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;
    Some(me.parent()?.join(os::paths::exe_name(name)))
}

/// How one line of amux ended.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// A byte came before the end: a deliberate stop.
    Stopped,
    /// The end came with nothing written.
    Died,
}

/// Starts `amux <args>` holding the lineage's write end, saying whether it
/// is a `restart`, and reads the read end until it ends.
fn run(amux: &Path, args: &[OsString], cwd: &Path, restart: bool) -> io::Result<Ended> {
    let (mut lineage, write) = os::process::pipe()?;
    let mut command = Command::new(amux);
    command
        .args(args)
        .env(LINEAGE_ENV, os::process::raw_for_child(&write).to_string());
    if restart {
        command.env(RESTART_ENV, "1");
    } else {
        command.env_remove(RESTART_ENV);
    }
    os::process::background(&mut command, os::process::Detach::Session, cwd);
    let given = os::process::Given {
        files: &[&write],
        ..os::process::Given::default()
    };
    let mut child = os::process::spawn_background(&mut command, given)?.child;
    // Only amux and its successors hold the write end now.
    drop(write);
    // Reaped as it exits (a zombie otherwise on Unix); nothing is read from
    // how it exited.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(read_to_end(&mut lineage))
}

/// Reads the lineage until it ends: [`Ended::Stopped`] if anything came
/// first. A read that fails is an end.
fn read_to_end(lineage: &mut impl io::Read) -> Ended {
    let mut said = false;
    let mut byte = [0u8; 64];
    loop {
        match lineage.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => said = true,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    if said { Ended::Stopped } else { Ended::Died }
}

/// The wait before a restart: [`Backoff::FIRST`], doubling to
/// [`Backoff::MAX`], and from the first again after a line that ran
/// [`Backoff::RESET`].
#[derive(Debug)]
struct Backoff {
    next: Duration,
}

impl Backoff {
    const FIRST: Duration = Duration::from_secs(1);
    const MAX: Duration = Duration::from_secs(60);
    const RESET: Duration = Duration::from_secs(10 * 60);

    /// The wait after a line that ran for `up`.
    fn after(&mut self, up: Duration) -> Duration {
        if up >= Self::RESET {
            self.next = Self::FIRST;
        }
        let wait = self.next;
        self.next = (self.next * 2).min(Self::MAX);
        wait
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self { next: Self::FIRST }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backoff_doubles_to_a_minute_and_starts_over_after_ten_minutes_up() {
        let mut backoff = Backoff::default();
        let quick = Duration::from_millis(10);
        let waits: Vec<u64> = (0..8).map(|_| backoff.after(quick).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(backoff.after(Duration::from_secs(9 * 60)).as_secs(), 60);
        assert_eq!(backoff.after(Duration::from_secs(10 * 60)).as_secs(), 1);
        assert_eq!(backoff.after(quick).as_secs(), 2);
    }

    #[test]
    fn a_byte_before_the_end_is_a_stop_and_the_end_alone_a_death() {
        assert_eq!(read_to_end(&mut &[0u8][..]), Ended::Stopped);
        assert_eq!(read_to_end(&mut &[][..]), Ended::Died);
    }

    /// The name amux reads: a launcher and a supervisor of any two
    /// releases meet through it.
    #[test]
    fn the_lineage_is_named_as_promised() {
        assert_eq!(LINEAGE_ENV, "AMUX_LAUNCH_LINEAGE");
        assert_eq!(RESTART_ENV, "AMUX_LAUNCH_RESTART");
    }
}
