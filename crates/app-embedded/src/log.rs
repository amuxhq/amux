//! The runtime's own log: what the daemon traces, written to the file the
//! app names, which a dump carries as the daemon's log.
//!
//! A phone keeps no log it does not need, so the file is capped: once a
//! write would take it past [`LOG_CAP`] it is cut to its newest
//! [`LOG_KEEP`] bytes, from a line start. That keeps at least as much as a
//! dump takes from its end.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once};

use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// The most the log file holds.
pub const LOG_CAP: u64 = 8 << 20;
/// What a cut keeps of it, from its end.
pub const LOG_KEEP: u64 = 4 << 20;

/// Where every trace of the process goes: the newest runtime's log. The
/// subscriber is the process's one global one, so a later start moves the
/// file rather than adding a second.
static SINK: Mutex<Option<Capped>> = Mutex::new(None);
static INSTALL: Once = Once::new();

/// Sends the process's traces to `path`, creating it if it is missing.
pub(crate) fn write_to(path: &Path) -> io::Result<()> {
    let capped = Capped::open(path, LOG_CAP, LOG_KEEP)?;
    *SINK.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(capped);
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
            Some(capped) => capped.write(bytes),
            None => Ok(bytes.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A log file that never grows past its cap.
struct Capped {
    path: PathBuf,
    file: File,
    len: u64,
    cap: u64,
    keep: u64,
}

impl Capped {
    fn open(path: &Path, cap: u64, keep: u64) -> io::Result<Capped> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let len = file.metadata()?.len();
        Ok(Capped {
            path: path.to_owned(),
            file,
            len,
            cap,
            keep,
        })
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.len + bytes.len() as u64 > self.cap {
            self.cut()?;
        }
        self.file.write_all(bytes)?;
        self.len += bytes.len() as u64;
        Ok(bytes.len())
    }

    /// Keeps the newest `keep` bytes, from the first line start in them.
    fn cut(&mut self) -> io::Result<()> {
        let bytes = std::fs::read(&self.path)?;
        let from = bytes.len().saturating_sub(self.keep as usize);
        let mut tail = &bytes[from..];
        if from > 0 && bytes[from - 1] != b'\n' {
            let newline = tail.iter().position(|byte| *byte == b'\n');
            tail = newline.map_or(&[][..], |newline| &tail[newline + 1..]);
        }
        let mut cut = self.path.clone().into_os_string();
        cut.push(".cut");
        std::fs::write(&cut, tail)?;
        std::fs::rename(&cut, &self.path)?;
        self.file = OpenOptions::new().append(true).open(&self.path)?;
        self.len = tail.len() as u64;
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

    #[test]
    fn a_log_past_its_cap_keeps_its_newest_whole_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime.log");
        let mut log = Capped::open(&path, 100, 40).unwrap();
        for n in 0..50 {
            log.write(format!("line {n:02}\n").as_bytes()).unwrap();
            let len = std::fs::metadata(&path).unwrap().len();
            assert!(len <= 100, "{len} bytes after line {n}");
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("line "), "{text:?}");
        assert!(text.ends_with("line 49\n"), "{text:?}");
        assert!(text.lines().count() > 1, "{text:?}");

        // Reopened, it counts what the file already holds.
        let mut log = Capped::open(&path, 100, 40).unwrap();
        log.write(&[b'x'; 30]).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() <= 100);
    }
}
