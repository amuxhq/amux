//! The session and fleet drivers: the only place an amux chat does I/O.
//!
//! A [`Session`] owns one agent's Subscribe stream and feeds it into the
//! pure `ui_state::SessionState`; a [`Fleet`] does the same for the
//! inventory and holds the one session each live agent has. Both call the local runtime through a [`client::Client`], reconnect on
//! their own clock, and keep a bounded trace for dumps. Nothing is
//! persisted: every open rebuilds from the runtime's rows.

mod dump;
mod fleet;
pub mod inputs;
pub mod review;
mod session;
pub mod trace;

use std::io;
use std::path::{Component, Path};

pub use fleet::{Fleet, FleetGuard, HELD_ROWS, OpenError, Stop, Window};
pub use session::{
    Changes, InputError, PageError, Sent, Session, StateGuard, TRANSCRIPT_CHECK, TRANSCRIPT_DUMP,
};
pub use trace::{DriverEvent, DriverTrace, Structure, TRACE_EVENTS, TraceEvent, Traced};

/// The first wait before reconnecting to the local runtime.
pub const RECONNECT_FIRST_MS: i64 = 250;
/// The longest wait between reconnects: a daemon update takes a few
/// seconds, and a client should be back soon after.
pub const RECONNECT_MAX_MS: i64 = 5_000;

/// Reconnect waits: doubling from the first to the longest, and back to the
/// first once a stream has caught up.
#[derive(Debug)]
struct Backoff {
    next: i64,
}

impl Default for Backoff {
    fn default() -> Backoff {
        Backoff {
            next: RECONNECT_FIRST_MS,
        }
    }
}

impl Backoff {
    fn next_ms(&mut self) -> i64 {
        let wait = self.next;
        self.next = (self.next * 2).min(RECONNECT_MAX_MS);
        wait
    }

    fn reset(&mut self) {
        self.next = RECONNECT_FIRST_MS;
    }
}

/// Writes a client's dump part into a bundle directory the daemon wrote.
/// Names are relative paths inside the bundle; any other is refused.
pub fn write_part(bundle: &Path, part: &wire::DumpPart) -> io::Result<()> {
    for file in &part.files {
        let name = Path::new(&file.name);
        let inside = name
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
        if !inside {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("a dump part names {:?}, outside its bundle", file.name),
            ));
        }
        let path = bundle.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &file.contents)?;
    }
    Ok(())
}
