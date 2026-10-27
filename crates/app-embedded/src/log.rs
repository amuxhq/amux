//! The runtime's own log: what the daemon traces, written to the file the
//! app names, which a dump carries as the daemon's log.
//!
//! The file is capped as every amux log is ([`log_file`]): past
//! [`log_file::PROCESS_CAP`] it moves to `<name>.1` and starts again, which
//! keeps at least as much as a dump takes from its end.

use std::io::{self, Write};
use std::path::Path;
use std::sync::{Mutex, Once};

use log_file::LogFile;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Where every trace of the process goes: the newest runtime's log. The
/// subscriber is the process's one global one, so a later start moves the
/// file rather than adding a second.
static SINK: Mutex<Option<LogFile>> = Mutex::new(None);
static INSTALL: Once = Once::new();

/// Sends the process's traces to `path`, creating it if it is missing.
pub(crate) fn write_to(path: &Path) -> io::Result<()> {
    let log = LogFile::open(path, log_file::PROCESS_CAP)?;
    *SINK.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(log);
    INSTALL.call_once(|| {
        // `RUST_LOG` takes `level` and `target=level` directives, which is
        // all a journey sets through the simulator. `Targets` parses those
        // without the regex engine `EnvFilter` brings, which is size the
        // phone does not need.
        let filter = std::env::var("RUST_LOG")
            .ok()
            .and_then(|directives| directives.parse::<Targets>().ok())
            .unwrap_or_else(default_filter);
        // An app that set its own subscriber keeps it.
        let _ = tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(|| Sink),
            )
            .with(filter)
            .try_init();
    });
    Ok(())
}

/// What the log keeps unless `RUST_LOG` says otherwise: everything at
/// INFO and above, the link log among it.
fn default_filter() -> Targets {
    Targets::new().with_default(LevelFilter::INFO)
}

/// One event's writer: formatted events arrive whole, one write each.
struct Sink;

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match SINK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_mut()
        {
            Some(log) => log.write(bytes),
            None => Ok(bytes.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_filter_keeps_the_link_log() {
        let filter = default_filter();
        assert!(filter.would_enable(node::link_log::TARGET, &tracing::Level::INFO));
        assert!(!filter.would_enable(node::link_log::TARGET, &tracing::Level::DEBUG));
    }
}
