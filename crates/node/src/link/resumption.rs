//! Session tickets: what lets a link to a host it reached before skip the
//! certificate exchange and, once the client has a ticket, send its first
//! requests with its first packet.
//!
//! The server keeps the ticket's session itself and hands the client only
//! a random id. rustls allows 0-RTT only for tickets kept that way: a
//! ticket that is the encrypted session can be replayed, and RFC 8446
//! §8.1 asks a server accepting early data to honour each ticket once.
//! Taking an id out of the store is what makes it single-use.
//!
//! The store lives on disk, in the profile (the relay's beside its
//! certificate), so a restarted host still honours the tickets it issued.
//! Each entry is sealed with the key of the day it was issued; keys rotate
//! daily and are dropped after a week, so an entry is honoured for seven
//! days and no longer. The same directory keeps the secret quinn derives
//! its address-validation tokens from, so a restarted host does not make
//! every returning client prove its address again.
//!
//! A client keeps the tickets it is given in memory, per peer: rustls
//! offers a ticket only under the client configuration it was issued to,
//! so each peer's configuration is built once and reused.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom as _, SystemRandom};

use crate::HostId;
use crate::identity::{DeviceIdentity, IdentityError};
use crate::trust::SharedTrustStore;

/// The directory a profile keeps its ticket store in.
pub const TICKETS_DIR: &str = "tickets";
/// The file in the store holding the sealing keys and the token secret.
const KEYS_FILE: &str = "keys";
/// How long a ticket is honoured after it is issued.
pub const TICKET_LIFETIME: Duration = Duration::from_secs(7 * DAY_SECS);
/// How often a sealing key is replaced.
const DAY_SECS: u64 = 24 * 60 * 60;
/// How often the store forgets the tickets past their lifetime.
const SWEEP_EVERY: Duration = Duration::from_secs(60 * 60);
/// The most tickets a host keeps: one per link that has not come back,
/// and a host's peers are a handful of machines and phones.
pub const HOST_TICKETS: usize = 1 << 10;
/// The most tickets the relay keeps: one per signed-in host's last link.
pub const RELAY_TICKETS: usize = 1 << 16;
/// How many tickets a client's cache for one peer is sized for. Every
/// link to a peer names it the same way, so one server entry is used; but
/// rustls sizes its table at a server per eight tickets and evicts the
/// oldest entry whenever an insert fills it, so a table of one keeps
/// nothing.
const CLIENT_TICKETS: usize = 16;
const KEY_LEN: usize = 32;
const SECRET_LEN: usize = 32;
const ISSUED_LEN: usize = 8;

/// The keys a server seals the sessions behind its tickets with: one per
/// day, kept for as long as a ticket issued that day is honoured, so a key
/// dropped is every ticket sealed with it expired.
struct TicketKeys {
    /// The secret quinn's address-validation tokens are derived from. It
    /// never rotates: a token outlives a day.
    token_secret: [u8; SECRET_LEN],
    /// Each day's key, by the day since the epoch it was made for.
    days: Vec<(u64, [u8; KEY_LEN])>,
}

impl TicketKeys {
    fn generate(rng: &SystemRandom) -> io::Result<Self> {
        Ok(Self {
            token_secret: random(rng)?,
            days: Vec::new(),
        })
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let (secret, rest) = bytes.split_at_checked(SECRET_LEN)?;
        let (days, []) = rest.as_chunks::<{ 8 + KEY_LEN }>() else {
            return None;
        };
        let days = days
            .iter()
            .map(|chunk| {
                let (day, key) = chunk.split_at(8);
                (
                    u64::from_le_bytes(day.try_into().expect("eight bytes")),
                    key.try_into().expect("a key's bytes"),
                )
            })
            .collect();
        Some(Self {
            token_secret: secret.try_into().expect("a secret's bytes"),
            days,
        })
    }

    fn encode(&self) -> Vec<u8> {
        let mut bytes = self.token_secret.to_vec();
        for (day, key) in &self.days {
            bytes.extend_from_slice(&day.to_le_bytes());
            bytes.extend_from_slice(key);
        }
        bytes
    }

    fn key(&self, day: u64) -> Option<LessSafeKey> {
        self.days
            .iter()
            .find(|(kept, _)| *kept == day)
            .map(|(_, key)| sealing_key(key))
    }

    /// Makes `today`'s key if there is none and drops the keys of days
    /// whose tickets have all expired. Whether anything changed.
    fn rotate(&mut self, today: u64, rng: &SystemRandom) -> io::Result<bool> {
        let oldest = today.saturating_sub(TICKET_LIFETIME.as_secs() / DAY_SECS);
        let before = self.days.len();
        self.days.retain(|(day, _)| *day >= oldest && *day <= today);
        let mut changed = self.days.len() != before;
        if !self.days.iter().any(|(day, _)| *day == today) {
            self.days.push((today, random(rng)?));
            changed = true;
        }
        Ok(changed)
    }
}

fn sealing_key(key: &[u8; KEY_LEN]) -> LessSafeKey {
    LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).expect("a ChaCha20 key's length"))
}

fn random<const N: usize>(rng: &SystemRandom) -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    rng.fill(&mut bytes)
        .map_err(|_| io::Error::other("the system random source failed"))?;
    Ok(bytes)
}

/// A server's session tickets: the sessions behind the tickets it issued,
/// by ticket id, each honoured once and for seven days, kept across
/// restarts where the store has a directory.
pub struct SessionTickets {
    state: Mutex<Store>,
    /// Tickets presented and honoured.
    redeemed: AtomicU64,
    rng: SystemRandom,
    now: Box<dyn Fn() -> SystemTime + Send + Sync>,
}

struct Store {
    dir: Option<PathBuf>,
    keys: TicketKeys,
    entries: HashMap<Vec<u8>, Entry>,
    /// The entries by when they were issued, oldest first.
    by_age: BTreeSet<(u64, Vec<u8>)>,
    bound: usize,
    swept: Option<SystemTime>,
}

struct Entry {
    issued_secs: u64,
    /// The nonce and the sealed session.
    sealed: Vec<u8>,
}

impl std::fmt::Debug for SessionTickets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = lock(&self.state);
        f.debug_struct("SessionTickets")
            .field("dir", &state.dir)
            .field("entries", &state.entries.len())
            .finish_non_exhaustive()
    }
}

impl SessionTickets {
    /// The store kept in `dir`, holding at most `bound` tickets: what the
    /// last run of this host left there, or a new one.
    pub fn open(dir: &Path, bound: usize) -> io::Result<Arc<Self>> {
        Self::open_with_clock(Some(dir), bound, Box::new(SystemTime::now)).map(Arc::new)
    }

    /// A store that lives only as long as this process: tickets it issues
    /// are not honoured after a restart.
    pub fn in_memory(bound: usize) -> Arc<Self> {
        Arc::new(
            Self::open_with_clock(None, bound, Box::new(SystemTime::now))
                .expect("a store without a directory reads nothing"),
        )
    }

    fn open_with_clock(
        dir: Option<&Path>,
        bound: usize,
        now: Box<dyn Fn() -> SystemTime + Send + Sync>,
    ) -> io::Result<Self> {
        let rng = SystemRandom::new();
        let mut keys = None;
        let mut entries = HashMap::new();
        if let Some(dir) = dir {
            crate::install::private_dir(dir)?;
            keys = match fs::read(dir.join(KEYS_FILE)) {
                Ok(bytes) => TicketKeys::decode(&bytes),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            };
            if keys.is_none() {
                // Keys that do not read honour nothing: the tickets they
                // sealed go with them.
                for file in fs::read_dir(dir)? {
                    let path = file?.path();
                    if path.file_name() != Some(KEYS_FILE.as_ref()) {
                        let _ = fs::remove_file(path);
                    }
                }
            }
            for file in fs::read_dir(dir)? {
                let file = file?;
                let Some(id) = file.file_name().to_str().and_then(unhex) else {
                    continue;
                };
                let Ok(bytes) = fs::read(file.path()) else {
                    continue;
                };
                match bytes.split_at_checked(ISSUED_LEN) {
                    Some((issued, sealed)) if sealed.len() > NONCE_LEN => {
                        let issued_secs = u64::from_le_bytes(issued.try_into().expect("8 bytes"));
                        entries.insert(
                            id,
                            Entry {
                                issued_secs,
                                sealed: sealed.to_vec(),
                            },
                        );
                    }
                    // A write a crash cut short.
                    _ => {
                        let _ = fs::remove_file(file.path());
                    }
                }
            }
        }
        let keys = match keys {
            Some(keys) => keys,
            None => {
                let keys = TicketKeys::generate(&rng)?;
                if let Some(dir) = dir {
                    write_keys(dir, &keys)?;
                }
                keys
            }
        };
        let by_age = entries
            .iter()
            .map(|(id, entry)| (entry.issued_secs, id.clone()))
            .collect();
        let tickets = Self {
            state: Mutex::new(Store {
                dir: dir.map(Path::to_owned),
                keys,
                entries,
                by_age,
                bound,
                swept: None,
            }),
            redeemed: AtomicU64::new(0),
            rng,
            now,
        };
        tickets.sweep(&mut lock(&tickets.state));
        Ok(tickets)
    }

    /// The key quinn derives this server's address-validation tokens
    /// from, the same across restarts.
    pub fn token_key(&self) -> Arc<dyn quinn::crypto::HandshakeTokenKey> {
        let secret = lock(&self.state).keys.token_secret;
        Arc::new(ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, &[]).extract(&secret))
    }

    /// How many tickets have been presented and honoured.
    pub fn redeemed(&self) -> u64 {
        self.redeemed.load(Ordering::Relaxed)
    }

    /// How many tickets the store holds.
    pub fn kept(&self) -> usize {
        lock(&self.state).entries.len()
    }

    fn now_secs(&self) -> u64 {
        (self.now)()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs())
    }

    /// Forgets the tickets past their lifetime, at most once an hour.
    fn sweep(&self, store: &mut Store) {
        let now = (self.now)();
        if store
            .swept
            .is_some_and(|at| now.duration_since(at).unwrap_or_default() < SWEEP_EVERY)
        {
            return;
        }
        store.swept = Some(now);
        let now_secs = self.now_secs();
        while let Some((issued, _)) = store.by_age.first()
            && !honoured(*issued, now_secs)
        {
            let (_, id) = store.by_age.pop_first().expect("a first entry");
            store.forget(&id);
        }
    }

    fn put(&self, id: Vec<u8>, session: Vec<u8>) -> io::Result<()> {
        let now_secs = self.now_secs();
        let mut store = lock(&self.state);
        self.sweep(&mut store);
        if store.keys.rotate(now_secs / DAY_SECS, &self.rng)?
            && let Some(dir) = &store.dir
        {
            write_keys(dir, &store.keys)?;
        }
        let key = store
            .keys
            .key(now_secs / DAY_SECS)
            .expect("today's key was just made");
        let nonce: [u8; NONCE_LEN] = random(&self.rng)?;
        let mut sealed = session;
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad(&id, now_secs)),
            &mut sealed,
        )
        .map_err(|_| io::Error::other("sealing a session failed"))?;
        let sealed = [nonce.as_slice(), &sealed].concat();
        if let Some(dir) = &store.dir {
            let mut file = private_file(&dir.join(hex(&id)))?;
            file.write_all(&now_secs.to_le_bytes())?;
            file.write_all(&sealed)?;
        }
        while store.entries.len() >= store.bound
            && let Some((_, oldest)) = store.by_age.pop_first()
        {
            store.forget(&oldest);
        }
        store.by_age.insert((now_secs, id.clone()));
        store.entries.insert(
            id,
            Entry {
                issued_secs: now_secs,
                sealed,
            },
        );
        Ok(())
    }

    fn take(&self, id: &[u8]) -> Option<Vec<u8>> {
        let now_secs = self.now_secs();
        let mut store = lock(&self.state);
        let entry = store.entries.get(id)?;
        let issued_secs = entry.issued_secs;
        let sealed = entry.sealed.clone();
        store.by_age.remove(&(issued_secs, id.to_vec()));
        store.forget(id);
        if !honoured(issued_secs, now_secs) {
            return None;
        }
        let key = store.keys.key(issued_secs / DAY_SECS)?;
        let (nonce, sealed) = sealed.split_at(NONCE_LEN);
        let mut session = sealed.to_vec();
        let opened = key
            .open_in_place(
                Nonce::try_assume_unique_for_key(nonce).ok()?,
                Aad::from(aad(id, issued_secs)),
                &mut session,
            )
            .ok()?
            .len();
        session.truncate(opened);
        self.redeemed.fetch_add(1, Ordering::Relaxed);
        Some(session)
    }

    /// A store reading the time from `now`, for tests of what expires.
    #[cfg(test)]
    pub(crate) fn with_clock(
        dir: Option<&Path>,
        bound: usize,
        now: impl Fn() -> SystemTime + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self::open_with_clock(dir, bound, Box::new(now)).unwrap())
    }
}

impl Store {
    /// Drops an entry from the map and the disk; the caller has taken it
    /// out of `by_age`.
    fn forget(&mut self, id: &[u8]) {
        self.entries.remove(id);
        if let Some(dir) = &self.dir {
            let _ = fs::remove_file(dir.join(hex(id)));
        }
    }
}

impl rustls::server::StoresServerSessions for SessionTickets {
    fn put(&self, id: Vec<u8>, session: Vec<u8>) -> bool {
        match SessionTickets::put(self, id, session) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "could not keep a session ticket");
                false
            }
        }
    }

    /// Only TLS 1.2 reads a session without consuming it; every link is
    /// TLS 1.3.
    fn get(&self, _id: &[u8]) -> Option<Vec<u8>> {
        None
    }

    fn take(&self, id: &[u8]) -> Option<Vec<u8>> {
        SessionTickets::take(self, id)
    }

    fn can_cache(&self) -> bool {
        true
    }
}

fn honoured(issued_secs: u64, now_secs: u64) -> bool {
    now_secs.saturating_sub(issued_secs) < TICKET_LIFETIME.as_secs()
}

/// What a sealed session is bound to: its ticket id and when it was
/// issued, so neither can be swapped on disk.
fn aad(id: &[u8], issued_secs: u64) -> Vec<u8> {
    [id, &issued_secs.to_le_bytes()].concat()
}

fn write_keys(dir: &Path, keys: &TicketKeys) -> io::Result<()> {
    crate::identity::atomic_replace_private(&dir.join(KEYS_FILE), &keys.encode()).map_err(|error| {
        match error {
            IdentityError::Io(error) => error,
            error => io::Error::other(error.to_string()),
        }
    })
}

/// A new file only this user reads. Not synced: a ticket a crash loses
/// costs its client one full handshake.
fn private_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || text.is_empty() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The client configurations a profile dials its peers with, one per peer
/// and pinned key, each holding the tickets that peer gave it. A peer
/// paired again under a new key gets a new configuration: a ticket from
/// the old key's holder resumes no link to the new one.
#[derive(Default)]
pub(crate) struct PeerResumption {
    configs: Mutex<HashMap<HostId, (Vec<u8>, quinn::ClientConfig)>>,
}

impl std::fmt::Debug for PeerResumption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerResumption")
            .field("peers", &lock(&self.configs).len())
            .finish()
    }
}

impl PeerResumption {
    /// The configuration to dial `peer` with: the one the last dial used
    /// while the peer's pinned key is unchanged, so its ticket is offered.
    pub(crate) fn client_config(
        &self,
        identity: &DeviceIdentity,
        trust: &SharedTrustStore,
        peer: HostId,
    ) -> Result<quinn::ClientConfig, IdentityError> {
        let pinned = trust
            .read()
            .map_err(|_| IdentityError::TrustStorePoisoned)?
            .pubkey_for_host(peer)
            .map(<[u8]>::to_vec);
        let Some(pinned) = pinned else {
            // An unpinned peer fails its handshake; nothing to keep.
            return identity.quic_client_config_for_peer(trust.clone(), peer);
        };
        let mut configs = lock(&self.configs);
        if let Some((key, config)) = configs.get(&peer)
            && *key == pinned
        {
            return Ok(config.clone());
        }
        let config = identity.quic_client_config_for_peer(trust.clone(), peer)?;
        configs.insert(peer, (pinned, config.clone()));
        Ok(config)
    }
}

/// How many tickets each client configuration keeps.
pub(crate) fn client_resumption() -> rustls::client::Resumption {
    rustls::client::Resumption::in_memory_sessions(CLIENT_TICKETS)
}

/// A wall clock tests move by hand, for what expires.
#[cfg(test)]
pub(crate) struct TestClock(Arc<AtomicU64>);

#[cfg(test)]
impl TestClock {
    pub(crate) fn new() -> Self {
        Self(Arc::new(AtomicU64::new(1_800_000_000)))
    }

    pub(crate) fn advance(&self, by: Duration) {
        self.0.fetch_add(by.as_secs(), Ordering::SeqCst);
    }

    pub(crate) fn reader(&self) -> impl Fn() -> SystemTime + Send + Sync + 'static {
        let secs = self.0.clone();
        move || SystemTime::UNIX_EPOCH + Duration::from_secs(secs.load(Ordering::SeqCst))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ticket_is_honoured_once() {
        let tickets = SessionTickets::in_memory(HOST_TICKETS);
        tickets.put(b"id".to_vec(), b"session".to_vec()).unwrap();
        assert_eq!(tickets.take(b"id"), Some(b"session".to_vec()));
        assert_eq!(tickets.take(b"id"), None);
        assert_eq!(tickets.redeemed(), 1);
    }

    #[test]
    fn a_ticket_is_honoured_by_the_store_reopened_from_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let first = SessionTickets::open(dir.path(), HOST_TICKETS).unwrap();
        first.put(b"id".to_vec(), b"session".to_vec()).unwrap();
        let secret = lock(&first.state).keys.token_secret;
        drop(first);

        let reopened = SessionTickets::open(dir.path(), HOST_TICKETS).unwrap();
        assert_eq!(lock(&reopened.state).keys.token_secret, secret);
        assert_eq!(reopened.take(b"id"), Some(b"session".to_vec()));
        drop(reopened);
        let again = SessionTickets::open(dir.path(), HOST_TICKETS).unwrap();
        assert_eq!(again.take(b"id"), None, "a taken ticket leaves the disk");
    }

    #[test]
    fn a_ticket_sealed_eight_days_ago_is_not_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let clock = TestClock::new();
        let tickets = SessionTickets::with_clock(Some(dir.path()), HOST_TICKETS, clock.reader());
        tickets.put(b"week".to_vec(), b"session".to_vec()).unwrap();
        tickets.put(b"eight".to_vec(), b"session".to_vec()).unwrap();
        clock.advance(TICKET_LIFETIME - Duration::from_secs(60));
        assert_eq!(tickets.take(b"week"), Some(b"session".to_vec()));
        clock.advance(Duration::from_secs(DAY_SECS + 60));
        assert_eq!(tickets.take(b"eight"), None);
    }

    #[test]
    fn ticket_keys_rotate_daily_and_are_dropped_after_a_week() {
        let dir = tempfile::tempdir().unwrap();
        let clock = TestClock::new();
        let tickets = SessionTickets::with_clock(Some(dir.path()), HOST_TICKETS, clock.reader());
        for day in 0..10 {
            tickets.put(vec![day], b"session".to_vec()).unwrap();
            clock.advance(Duration::from_secs(DAY_SECS));
        }
        let days: Vec<u64> = lock(&tickets.state)
            .keys
            .days
            .iter()
            .map(|(day, _)| *day)
            .collect();
        assert_eq!(days.len(), 8, "today's key and the last seven: {days:?}");
        let reopened = SessionTickets::with_clock(Some(dir.path()), HOST_TICKETS, clock.reader());
        assert_eq!(
            lock(&reopened.state).keys.days.len(),
            8,
            "the keys are kept on disk"
        );
        // The sweep at reopening forgot the tickets past their week.
        assert_eq!(reopened.kept(), 6);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 7);
    }

    #[test]
    fn the_ticket_store_keeps_its_bound_by_forgetting_the_oldest() {
        let clock = TestClock::new();
        let tickets = SessionTickets::with_clock(None, 3, clock.reader());
        for id in 0..5 {
            tickets.put(vec![id], b"session".to_vec()).unwrap();
            clock.advance(Duration::from_secs(1));
        }
        assert_eq!(tickets.kept(), 3);
        assert_eq!(tickets.take(&[1]), None);
        assert_eq!(tickets.take(&[2]), Some(b"session".to_vec()));
    }

    #[test]
    fn a_ticket_store_with_unreadable_keys_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let tickets = SessionTickets::open(dir.path(), HOST_TICKETS).unwrap();
        tickets.put(b"id".to_vec(), b"session".to_vec()).unwrap();
        drop(tickets);
        fs::write(dir.path().join(KEYS_FILE), b"short").unwrap();
        let reopened = SessionTickets::open(dir.path(), HOST_TICKETS).unwrap();
        assert_eq!(reopened.kept(), 0);
        assert_eq!(reopened.take(b"id"), None);
    }
}
