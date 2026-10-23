//! The TLS 1.3 tickets a client was given, kept on disk so a relaunched
//! process resumes its first link instead of starting a full handshake.
//!
//! This module is the only user of the rustls patch (docs/ARCHITECTURE.md,
//! "The rustls patch"): a ticket is saved as it is given, loaded when its
//! peer's client configuration is first built in a process, and deleted
//! from disk before it is handed to a handshake, so each one is offered
//! once, as TLS 1.3 asks. Without the patch the store keeps tickets in
//! memory only.

use std::collections::VecDeque;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use rustls::client::{ClientSessionMemoryCache, ClientSessionStore, Tls13ClientSessionValue};
use rustls::pki_types::ServerName;

/// The directory a profile keeps its client tickets in, one directory per
/// peer and pinned key beneath it.
pub const CLIENT_TICKETS_DIR: &str = "client-tickets";
const TICKET_SUFFIX: &str = ".ticket";

/// One peer's tickets: every link to a peer names it the same way, so the
/// store holds tickets for one server name.
pub(crate) struct ClientTickets {
    dir: Option<PathBuf>,
    bound: usize,
    /// The tickets, oldest first, each with the file it is saved in.
    tickets: Mutex<VecDeque<Kept>>,
    /// Key-exchange hints and TLS 1.2 sessions, which nothing persists.
    rest: ClientSessionMemoryCache,
    next: AtomicU64,
}

struct Kept {
    server: ServerName<'static>,
    value: Tls13ClientSessionValue,
    file: Option<PathBuf>,
}

impl std::fmt::Debug for ClientTickets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientTickets")
            .field("dir", &self.dir)
            .field("tickets", &lock(&self.tickets).len())
            .finish()
    }
}

impl ClientTickets {
    /// A store saving to `dir` when given, holding at most `bound` tickets.
    pub(crate) fn new(dir: Option<PathBuf>, bound: usize) -> Self {
        Self {
            dir,
            bound,
            tickets: Mutex::new(VecDeque::new()),
            rest: ClientSessionMemoryCache::new(bound),
            next: AtomicU64::new(0),
        }
    }

    /// Takes in the tickets saved in this store's directory, bound to
    /// `config`, which must be the configuration this store serves. A
    /// ticket that does not decode is deleted.
    pub(crate) fn load(&self, config: &rustls::ClientConfig, server: &ServerName<'static>) {
        let Some(dir) = &self.dir else { return };
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut saved: Vec<(u64, PathBuf)> = entries
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                let name = path.file_name()?.to_str()?;
                let order = name.strip_suffix(TICKET_SUFFIX)?.parse().ok()?;
                Some((order, path))
            })
            .collect();
        saved.sort();
        let mut tickets = lock(&self.tickets);
        for (order, path) in saved {
            self.next.fetch_max(order + 1, Ordering::Relaxed);
            let value = fs::read(&path)
                .ok()
                .and_then(|bytes| Tls13ClientSessionValue::from_slice(&bytes, config).ok());
            match value {
                Some(value) => tickets.push_back(Kept {
                    server: server.clone(),
                    value,
                    file: Some(path),
                }),
                None => forget(&path),
            }
        }
        while tickets.len() > self.bound {
            if let Some(Kept {
                file: Some(path), ..
            }) = tickets.pop_front()
            {
                forget(&path);
            }
        }
    }

    fn save(&self, value: &Tls13ClientSessionValue) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?;
        let path = dir.join(format!(
            "{}{TICKET_SUFFIX}",
            self.next.fetch_add(1, Ordering::Relaxed)
        ));
        let written = fs::create_dir_all(dir)
            .and_then(|()| private_file(&path))
            .and_then(|mut file| file.write_all(&value.encode()));
        match written {
            Ok(()) => Some(path),
            Err(error) => {
                // The ticket still serves this process.
                tracing::debug!(%error, "a client ticket could not be saved");
                forget(&path);
                None
            }
        }
    }
}

impl ClientSessionStore for ClientTickets {
    fn set_kx_hint(&self, server: ServerName<'static>, group: rustls::NamedGroup) {
        self.rest.set_kx_hint(server, group);
    }

    fn kx_hint(&self, server: &ServerName<'_>) -> Option<rustls::NamedGroup> {
        self.rest.kx_hint(server)
    }

    fn set_tls12_session(
        &self,
        server: ServerName<'static>,
        value: rustls::client::Tls12ClientSessionValue,
    ) {
        self.rest.set_tls12_session(server, value);
    }

    fn tls12_session(
        &self,
        server: &ServerName<'_>,
    ) -> Option<rustls::client::Tls12ClientSessionValue> {
        self.rest.tls12_session(server)
    }

    fn remove_tls12_session(&self, server: &ServerName<'static>) {
        self.rest.remove_tls12_session(server);
    }

    fn insert_tls13_ticket(&self, server: ServerName<'static>, value: Tls13ClientSessionValue) {
        let file = self.save(&value);
        let mut tickets = lock(&self.tickets);
        tickets.push_back(Kept {
            server,
            value,
            file,
        });
        while tickets.len() > self.bound {
            if let Some(Kept {
                file: Some(path), ..
            }) = tickets.pop_front()
            {
                forget(&path);
            }
        }
    }

    /// The newest ticket for `server`, gone from disk before the handshake
    /// sees it. One whose file cannot be deleted is not offered: a later
    /// process would offer it again.
    fn take_tls13_ticket(&self, server: &ServerName<'static>) -> Option<Tls13ClientSessionValue> {
        let mut tickets = lock(&self.tickets);
        let at = tickets.iter().rposition(|kept| kept.server == *server)?;
        let kept = tickets.remove(at)?;
        if let Some(path) = &kept.file
            && let Err(error) = fs::remove_file(path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "a client ticket could not be deleted; not offering it");
            return None;
        }
        Some(kept.value)
    }
}

/// How many tickets `dir` holds on disk, for tests.
#[cfg(test)]
pub(crate) fn saved(dir: &Path) -> usize {
    fs::read_dir(dir).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(TICKET_SUFFIX))
            .count()
    })
}

fn forget(path: &Path) {
    let _ = fs::remove_file(path);
}

/// A new file only this user reads. Not synced: a ticket a crash loses
/// costs one full handshake.
fn private_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
