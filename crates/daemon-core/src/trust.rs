//! The hosts this profile trusts: each paired host's pinned key, its name,
//! how it was last reached and what it last said of itself.
//!
//! The core holds the trust store and is its only writer. A change is
//! staged on a copy, written whole to the trust file by one
//! [`Job::Fsync`] at a time, and only then becomes the store the core
//! serves and the shell's handshakes read: a crash before the write leaves
//! the trust the drive held, and a change the drive refused never shows.
//! Changes queue behind the one under way, so a later change is never
//! written under an earlier one. The driver reads the file at start and
//! hands it over as [`Input::TrustRead`](crate::Input::TrustRead).
//!
//! Replacing a host's key closes everything opened under the old one
//! first: the old key stops being trusted at once, the driver tears the
//! host's connections down as [`Job::TeardownHost`], and the new key is
//! written after.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::SocketAddr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Daemon;
use crate::ids::{CallId, HostId, JobId, PairingId};
use crate::io::{Job, Output, ShellWork};
use crate::payload::{CallError, JobResult, PeerAddress, Response, Status};
use crate::serve::answered;

/// The length of an Ed25519 public key, the only key a host pins.
pub const PUBKEY_LEN: usize = 32;

/// How a pairing reached the host: through the account's relay, over
/// SSH to a destination, or at addresses.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Reachability {
    Cloud,
    Ssh(String),
    Direct { addrs: Vec<SocketAddr> },
}

/// What the profile knows of one paired host.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrustEntry {
    pub pubkey: Vec<u8>,
    pub name: String,
    pub paired_at: DateTime<Utc>,
    /// The account's relay reaches the host: it paired through the relay,
    /// or the relay has claimed it since. A launch's first flight then goes
    /// through the relay too, before the relay has said anything.
    pub on_account: bool,
    /// The addresses the host last answered at directly, dialled at launch
    /// beside what discovery finds. Only these reach a host paired by
    /// address across a network discovery cannot see.
    pub direct: Vec<SocketAddr>,
    /// The SSH destination the host is reached at, where it paired that
    /// way: the one way of reaching it the host cannot tell this one
    /// itself. `amux relay` there joins the profile whose id is the host's.
    pub ssh: Option<String>,
    pub signed_in: Option<bool>,
    /// The operating system the peer announced, last it said one.
    pub platform: wire::Platform,
    /// Every address the peer last listed for itself, with where each came
    /// from. The address it last answered at is its `Direct` reachability.
    pub listed: Vec<PeerAddress>,
}

/// The trust file's shapes, which are JSON's: an entry that leaves out
/// what it does not know. The checkpoint's format cannot skip a field, so
/// it gets the plain shape.
mod forms {
    use super::*;

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FileEntry {
        pubkey: Vec<u8>,
        name: String,
        paired_at: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        on_account: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        direct: Vec<SocketAddr>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ssh: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signed_in: Option<bool>,
        #[serde(default, skip_serializing_if = "unspecified")]
        platform: wire::Platform,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        listed: Vec<PeerAddress>,
    }

    #[derive(Serialize, Deserialize)]
    struct PlainEntry {
        pubkey: Vec<u8>,
        name: String,
        paired_at: DateTime<Utc>,
        on_account: bool,
        direct: Vec<SocketAddr>,
        ssh: Option<String>,
        signed_in: Option<bool>,
        platform: wire::Platform,
        listed: Vec<PeerAddress>,
    }

    fn unspecified(platform: &wire::Platform) -> bool {
        *platform == wire::Platform::Unspecified
    }

    macro_rules! entry {
        ($from:ident => $to:ident) => {
            impl From<$from> for $to {
                fn from(entry: $from) -> $to {
                    $to {
                        pubkey: entry.pubkey,
                        name: entry.name,
                        paired_at: entry.paired_at,
                        on_account: entry.on_account,
                        direct: entry.direct,
                        ssh: entry.ssh,
                        signed_in: entry.signed_in,
                        platform: entry.platform,
                        listed: entry.listed,
                    }
                }
            }
        };
    }
    entry!(TrustEntry => FileEntry);
    entry!(FileEntry => TrustEntry);
    entry!(TrustEntry => PlainEntry);
    entry!(PlainEntry => TrustEntry);

    impl Serialize for TrustEntry {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            if serializer.is_human_readable() {
                FileEntry::from(self.clone()).serialize(serializer)
            } else {
                PlainEntry::from(self.clone()).serialize(serializer)
            }
        }
    }

    impl<'de> Deserialize<'de> for TrustEntry {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            if deserializer.is_human_readable() {
                FileEntry::deserialize(deserializer).map(TrustEntry::from)
            } else {
                PlainEntry::deserialize(deserializer).map(TrustEntry::from)
            }
        }
    }
}

/// The paired hosts by id, and the hosts whose key is being replaced:
/// those are trusted under no key until the replacement is written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TrustStore {
    entries: BTreeMap<Uuid, TrustEntry>,
    replacement_pending: BTreeSet<Uuid>,
}

/// What a pairing did to the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PairingUpdate {
    Inserted,
    Updated,
    /// The host is paired under another key: its connections must close
    /// before the new key is written.
    PubkeyReplacementRequired,
    ReplacedPubkey,
}

/// Why a store or a change of it is refused.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrustError {
    /// A pinned key is not an Ed25519 public key.
    BadPubkey { actual: usize },
    /// Another host is paired under this key.
    DuplicatePubkey(Uuid),
    /// The host is not paired.
    NotTrusted(Uuid),
    /// The trust file could not be written, in the driver's words.
    Write(String),
    /// The trust file does not decode.
    Corrupt(String),
}

impl std::fmt::Display for TrustError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrustError::BadPubkey { actual } => write!(
                f,
                "invalid TrustEntry.pubkey length: expected {PUBKEY_LEN}, got {actual}"
            ),
            TrustError::DuplicatePubkey(host) => {
                write!(f, "public key is already trusted for host {host}")
            }
            TrustError::NotTrusted(host) => write!(f, "peer {host} is not trusted"),
            TrustError::Write(error) => write!(f, "cannot save the trust store: {error}"),
            TrustError::Corrupt(error) => write!(f, "cannot read the trust store: {error}"),
        }
    }
}

impl std::error::Error for TrustError {}

fn validate_pubkey(pubkey: &[u8]) -> Result<(), TrustError> {
    if pubkey.len() == PUBKEY_LEN {
        Ok(())
    } else {
        Err(TrustError::BadPubkey {
            actual: pubkey.len(),
        })
    }
}

impl TrustStore {
    /// The store a trust file holds; a pinned key of the wrong length
    /// refuses the whole file.
    pub fn from_file(bytes: &[u8]) -> Result<TrustStore, TrustError> {
        let entries: BTreeMap<Uuid, TrustEntry> = serde_json::from_slice(bytes)
            .map_err(|error| TrustError::Corrupt(error.to_string()))?;
        let store = TrustStore {
            entries,
            replacement_pending: BTreeSet::new(),
        };
        store.validate()?;
        Ok(store)
    }

    /// The trust file's bytes for this store.
    pub fn to_file(&self) -> Result<Vec<u8>, TrustError> {
        self.validate()?;
        Ok(serde_json::to_vec_pretty(&self.entries).expect("trust entries serialize"))
    }

    /// A store trusting each of `hosts` under a key made from its id, for
    /// tests and checkers that need trust and no pairing.
    pub fn of_hosts(hosts: impl IntoIterator<Item = HostId>) -> TrustStore {
        let entries = hosts
            .into_iter()
            .map(|host| {
                let mut pubkey = host.0.to_vec();
                pubkey.extend_from_slice(&host.0);
                (
                    Uuid::from_bytes(host.0),
                    TrustEntry {
                        pubkey,
                        name: Uuid::from_bytes(host.0).to_string(),
                        paired_at: DateTime::<Utc>::UNIX_EPOCH,
                        on_account: false,
                        direct: Vec::new(),
                        ssh: None,
                        signed_in: None,
                        platform: wire::Platform::Unspecified,
                        listed: Vec::new(),
                    },
                )
            })
            .collect();
        TrustStore {
            entries,
            replacement_pending: BTreeSet::new(),
        }
    }

    pub fn entry(&self, host_id: Uuid) -> Option<&TrustEntry> {
        self.entries.get(&host_id)
    }

    pub fn entries(&self) -> impl Iterator<Item = (Uuid, &TrustEntry)> + '_ {
        self.entries
            .iter()
            .map(|(host_id, entry)| (*host_id, entry))
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The paired hosts, a host whose key is being replaced among them.
    pub fn paired_hosts(&self) -> Vec<HostId> {
        self.entries
            .keys()
            .map(|host| HostId(*host.as_bytes()))
            .collect()
    }

    #[doc(hidden)]
    pub fn insert_for_test(&mut self, host_id: Uuid, entry: TrustEntry) {
        self.entries.insert(host_id, entry);
    }

    pub fn host_id_for_pubkey(&self, pubkey: &[u8]) -> Option<Uuid> {
        self.entries
            .iter()
            .filter(|(host_id, _)| !self.replacement_pending.contains(host_id))
            .find_map(|(host_id, entry)| (entry.pubkey == pubkey).then_some(*host_id))
    }

    pub fn pubkey_for_host(&self, host_id: Uuid) -> Option<&[u8]> {
        if self.replacement_pending.contains(&host_id) {
            return None;
        }
        self.entries
            .get(&host_id)
            .map(|entry| entry.pubkey.as_slice())
    }

    pub fn upsert_paired_peer(
        &mut self,
        host_id: Uuid,
        pubkey: Vec<u8>,
        name: String,
        reachability: impl Into<Option<Reachability>>,
        paired_at: DateTime<Utc>,
    ) -> Result<PairingUpdate, TrustError> {
        let reachability = reachability.into();
        validate_pubkey(&pubkey)?;
        if let Some(existing_host_id) = self.other_host_for_pubkey(host_id, &pubkey) {
            return Err(TrustError::DuplicatePubkey(existing_host_id));
        }
        if self.replacement_pending.contains(&host_id) {
            return Ok(PairingUpdate::PubkeyReplacementRequired);
        }
        match self.entries.get_mut(&host_id) {
            Some(entry) if entry.pubkey == pubkey => {
                entry.name = name;
                entry.paired_at = paired_at;
                if let Some(reachability) = reachability {
                    entry.reached(reachability);
                }
                Ok(PairingUpdate::Updated)
            }
            Some(_) => {
                self.replacement_pending.insert(host_id);
                Ok(PairingUpdate::PubkeyReplacementRequired)
            }
            None => {
                self.entries.insert(
                    host_id,
                    TrustEntry::paired(pubkey, name, paired_at, reachability),
                );
                Ok(PairingUpdate::Inserted)
            }
        }
    }

    pub fn replace_paired_peer_after_teardown(
        &mut self,
        host_id: Uuid,
        pubkey: Vec<u8>,
        name: String,
        reachability: impl Into<Option<Reachability>>,
        paired_at: DateTime<Utc>,
    ) -> Result<PairingUpdate, TrustError> {
        let reachability = reachability.into();
        validate_pubkey(&pubkey)?;
        if let Some(existing_host_id) = self.other_host_for_pubkey(host_id, &pubkey) {
            return Err(TrustError::DuplicatePubkey(existing_host_id));
        }
        self.replacement_pending.remove(&host_id);
        match self.entries.get_mut(&host_id) {
            Some(entry) if entry.pubkey == pubkey => {
                entry.name = name;
                entry.paired_at = paired_at;
                if let Some(reachability) = reachability {
                    entry.reached(reachability);
                }
                Ok(PairingUpdate::Updated)
            }
            Some(entry) => {
                *entry = TrustEntry::paired(pubkey, name, paired_at, reachability);
                Ok(PairingUpdate::ReplacedPubkey)
            }
            None => {
                self.entries.insert(
                    host_id,
                    TrustEntry::paired(pubkey, name, paired_at, reachability),
                );
                Ok(PairingUpdate::Inserted)
            }
        }
    }

    pub fn remove(&mut self, host_id: Uuid) -> Option<TrustEntry> {
        self.replacement_pending.remove(&host_id);
        self.entries.remove(&host_id)
    }

    pub fn replace_direct_addrs(&mut self, host_id: Uuid, addrs: Vec<SocketAddr>) -> bool {
        let Some(entry) = self.entries.get_mut(&host_id) else {
            return false;
        };
        if entry.direct == addrs {
            return false;
        }
        entry.direct = addrs;
        true
    }

    /// Replaces what `host_id` listed for itself; false when it is not
    /// trusted or listed the same.
    pub fn replace_listed(&mut self, host_id: Uuid, listed: Vec<PeerAddress>) -> bool {
        let Some(entry) = self.entries.get_mut(&host_id) else {
            return false;
        };
        if entry.listed == listed {
            return false;
        }
        entry.listed = listed;
        true
    }

    /// Keeps that `host_id` can be reached through the relay; false when it
    /// is not trusted or already could.
    pub fn remember_on_the_account(&mut self, host_id: Uuid) -> bool {
        let Some(entry) = self.entries.get_mut(&host_id) else {
            return false;
        };
        !std::mem::replace(&mut entry.on_account, true)
    }

    pub fn remember_signed_in(&mut self, host_id: Uuid, signed_in: Option<bool>) -> bool {
        let Some(entry) = self.entries.get_mut(&host_id) else {
            return false;
        };
        if entry.signed_in == signed_in {
            return false;
        }
        entry.signed_in = signed_in;
        true
    }

    /// Keeps the operating system a peer announced, so an offline peer
    /// still says what it is. A peer that announces none leaves the last.
    pub fn remember_platform(&mut self, host_id: Uuid, platform: wire::Platform) -> bool {
        let Some(entry) = self.entries.get_mut(&host_id) else {
            return false;
        };
        if platform == wire::Platform::Unspecified || entry.platform == platform {
            return false;
        }
        entry.platform = platform;
        true
    }

    fn other_host_for_pubkey(&self, host_id: Uuid, pubkey: &[u8]) -> Option<Uuid> {
        self.entries.iter().find_map(|(candidate_host_id, entry)| {
            (*candidate_host_id != host_id && entry.pubkey == pubkey).then_some(*candidate_host_id)
        })
    }

    fn validate(&self) -> Result<(), TrustError> {
        for entry in self.entries.values() {
            validate_pubkey(&entry.pubkey)?;
        }
        Ok(())
    }
}

impl TrustEntry {
    /// A host just paired, reached as `reachability`.
    fn paired(
        pubkey: Vec<u8>,
        name: String,
        paired_at: DateTime<Utc>,
        reachability: Option<Reachability>,
    ) -> TrustEntry {
        let mut entry = TrustEntry {
            pubkey,
            name,
            paired_at,
            on_account: false,
            direct: Vec::new(),
            ssh: None,
            signed_in: None,
            platform: wire::Platform::Unspecified,
            listed: Vec::new(),
        };
        if let Some(reachability) = reachability {
            entry.reached(reachability);
        }
        entry
    }

    /// Keeps how a pairing reached the host, beside what was kept before:
    /// addresses join the direct ones, and an SSH target replaces the last.
    fn reached(&mut self, reachability: Reachability) {
        match reachability {
            Reachability::Cloud => self.on_account = true,
            Reachability::Ssh(target) => self.ssh = Some(target),
            Reachability::Direct { addrs } => {
                for addr in addrs {
                    if !self.direct.contains(&addr) {
                        self.direct.push(addr);
                    }
                }
            }
        }
    }
}

/// A change of the trust store, as someone asks for it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrustChange {
    /// Trust `host` under `pubkey`, reached as `reachability`: a finished
    /// pairing's commit.
    Pair {
        host: Uuid,
        pubkey: Vec<u8>,
        name: String,
        reachability: Option<Reachability>,
    },
    /// Forget `host`.
    Remove { host: Uuid },
    /// `host` answered at `addrs`: they replace the direct addresses kept.
    DirectAddrs { host: Uuid, addrs: Vec<SocketAddr> },
    /// `host` listed itself at `listed`, which replaces what it listed
    /// before.
    Listed {
        host: Uuid,
        listed: Vec<PeerAddress>,
    },
    /// What `host` last said of itself.
    Remember {
        host: Uuid,
        signed_in: Option<bool>,
        platform: wire::Platform,
    },
    /// Put `host`'s entry back as it was: a pairing's commit undone.
    Restore {
        host: Uuid,
        entry: Option<TrustEntry>,
    },
    /// The relay said `host` is on this profile's account, so a launch
    /// may reach it through the relay.
    OnTheAccount { host: Uuid },
}

impl TrustChange {
    fn host(&self) -> Uuid {
        match self {
            TrustChange::Pair { host, .. }
            | TrustChange::Remove { host }
            | TrustChange::DirectAddrs { host, .. }
            | TrustChange::Listed { host, .. }
            | TrustChange::Remember { host, .. }
            | TrustChange::Restore { host, .. }
            | TrustChange::OnTheAccount { host } => *host,
        }
    }
}

/// What a change did, once it is on the drive.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrustChanged {
    /// A pairing's commit, and what it did to the store.
    Paired(PairingUpdate),
    /// The host is forgotten; its entry was this.
    Removed(TrustEntry),
    /// A remembered fact or address.
    Remembered,
    /// A pairing's commit undone.
    Restored,
    /// Nothing needed writing.
    Unchanged,
}

/// Who waits for a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum Waiter {
    Call(CallId),
    Pairing(PairingId),
    /// An SSH pairing's call, for `host`, this host the side it was.
    Ssh {
        call: CallId,
        host: HostId,
        role: crate::pairing::Role,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Queued {
    change: TrustChange,
    waiter: Option<Waiter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Stage {
    /// The host's connections under its old key are closing.
    TearingDown,
    /// The staged store is being written.
    Writing,
}

/// The change under way.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Changing {
    queued: Queued,
    job: JobId,
    stage: Stage,
    staged: TrustStore,
    changed: TrustChanged,
}

/// The trust store as the drive holds it, the change under way and the
/// changes waiting behind it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Trust {
    store: TrustStore,
    changing: Option<Changing>,
    queue: VecDeque<Queued>,
}

impl Trust {
    pub fn store(&self) -> &TrustStore {
        &self.store
    }

    pub(crate) fn jobs(&self, out: &mut Vec<JobId>) {
        out.extend(self.changing.as_ref().map(|changing| changing.job));
    }

    pub(crate) fn canonical(&self, ids: &crate::fingerprint::Ids) -> Trust {
        let mut trust = self.clone();
        if let Some(changing) = &mut trust.changing {
            changing.job = ids.jobs.get(changing.job);
        }
        trust
    }
}

impl Daemon {
    /// The trust file as the driver read it at start: the store the drive
    /// holds, so nothing of it is written again.
    pub(crate) fn trust_read(&mut self, store: TrustStore, out: &mut Vec<Output>) {
        self.trust.store = store;
        self.remember_addresses();
        self.trust_published(None, out);
    }

    /// The trust store the core serves.
    pub fn trust(&self) -> &TrustStore {
        &self.trust.store
    }

    /// Whether the removal of `host` is being written.
    pub(crate) fn trust_removing(&self, host: Uuid) -> bool {
        self.trust.changing.as_ref().is_some_and(|changing| {
            changing.stage == Stage::Writing
                && changing.queued.change == TrustChange::Remove { host }
        })
    }

    /// A change asked for by a call, answered once it is on the drive.
    pub(crate) fn change_trust_call(
        &mut self,
        call: CallId,
        change: TrustChange,
        out: &mut Vec<Output>,
    ) {
        self.change_trust(change, Some(Waiter::Call(call)), out);
    }

    /// Queues `change` behind the one under way, or starts it.
    pub(crate) fn change_trust(
        &mut self,
        change: TrustChange,
        waiter: Option<Waiter>,
        out: &mut Vec<Output>,
    ) {
        self.trust.queue.push_back(Queued { change, waiter });
        self.next_trust_change(out);
    }

    fn next_trust_change(&mut self, out: &mut Vec<Output>) {
        while self.trust.changing.is_none()
            && let Some(queued) = self.trust.queue.pop_front()
        {
            self.start_trust_change(queued, out);
        }
    }

    fn start_trust_change(&mut self, queued: Queued, out: &mut Vec<Output>) {
        let mut staged = self.trust.store.clone();
        let paired_at = DateTime::<Utc>::from_timestamp_millis(self.timers.now_ms as i64)
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
        let changed = match &queued.change {
            TrustChange::Pair {
                host,
                pubkey,
                name,
                reachability,
            } => staged
                .upsert_paired_peer(
                    *host,
                    pubkey.clone(),
                    name.clone(),
                    reachability.clone(),
                    paired_at,
                )
                .map(TrustChanged::Paired),
            TrustChange::Remove { host } => staged
                .remove(*host)
                .map(TrustChanged::Removed)
                .ok_or(TrustError::NotTrusted(*host)),
            TrustChange::DirectAddrs { host, addrs } => {
                Ok(if staged.replace_direct_addrs(*host, addrs.clone()) {
                    TrustChanged::Remembered
                } else {
                    TrustChanged::Unchanged
                })
            }
            TrustChange::Listed { host, listed } => {
                Ok(if staged.replace_listed(*host, listed.clone()) {
                    TrustChanged::Remembered
                } else {
                    TrustChanged::Unchanged
                })
            }
            TrustChange::Remember {
                host,
                signed_in,
                platform,
            } => {
                let signed_in = staged.remember_signed_in(*host, *signed_in);
                let platform = staged.remember_platform(*host, *platform);
                Ok(if signed_in || platform {
                    TrustChanged::Remembered
                } else {
                    TrustChanged::Unchanged
                })
            }
            TrustChange::OnTheAccount { host } => Ok(if staged.remember_on_the_account(*host) {
                TrustChanged::Remembered
            } else {
                TrustChanged::Unchanged
            }),
            TrustChange::Restore { host, entry } => {
                staged.replacement_pending.remove(host);
                match entry {
                    Some(entry) => staged.entries.insert(*host, entry.clone()),
                    None => staged.entries.remove(host),
                };
                Ok(TrustChanged::Restored)
            }
        };
        let changed = match changed {
            Ok(TrustChanged::Unchanged) => {
                self.trust_change_ended(queued.waiter, Ok(TrustChanged::Unchanged), out);
                return;
            }
            Ok(changed) => changed,
            Err(error) => {
                self.trust_change_ended(queued.waiter, Err(error), out);
                return;
            }
        };
        let job = self.pending.mint();
        if changed == TrustChanged::Paired(PairingUpdate::PubkeyReplacementRequired) {
            // The old key stops being trusted now, before anything opened
            // under it is closed.
            let host = queued.change.host();
            self.trust.store.replacement_pending.insert(host);
            self.begin_replacement(HostId(*host.as_bytes()));
            self.trust_published(None, out);
            out.push(Output::Job {
                id: job,
                op: Job::TeardownHost {
                    host: HostId(*host.as_bytes()),
                },
            });
            self.trust.changing = Some(Changing {
                queued,
                job,
                stage: Stage::TearingDown,
                staged,
                changed,
            });
            return;
        }
        self.write_trust(queued, job, staged, changed, out);
    }

    fn write_trust(
        &mut self,
        queued: Queued,
        job: JobId,
        staged: TrustStore,
        changed: TrustChanged,
        out: &mut Vec<Output>,
    ) {
        let bytes = match staged.to_file() {
            Ok(bytes) => bytes,
            Err(error) => {
                self.trust_change_ended(queued.waiter, Err(error), out);
                return;
            }
        };
        out.push(Output::Job {
            id: job,
            op: Job::Fsync {
                path: self.options.trust.clone(),
                bytes,
            },
        });
        self.trust.changing = Some(Changing {
            queued,
            job,
            stage: Stage::Writing,
            staged,
            changed,
        });
    }

    /// A job of the trust change under way finished; returns whether `job`
    /// was one.
    pub(crate) fn trust_job_done(
        &mut self,
        job: JobId,
        result: &JobResult,
        out: &mut Vec<Output>,
    ) -> bool {
        if self
            .trust
            .changing
            .as_ref()
            .is_none_or(|changing| changing.job != job)
        {
            return false;
        }
        let Changing {
            queued,
            stage,
            staged,
            changed,
            ..
        } = self.trust.changing.take().expect("a change is under way");
        let host = queued.change.host();
        match (stage, result) {
            (Stage::TearingDown, _) => {
                // Whatever the teardown said, nothing of the old key is
                // trusted any more: the replacement is written on.
                let TrustChange::Pair {
                    pubkey,
                    name,
                    reachability,
                    ..
                } = &queued.change
                else {
                    unreachable!("only a pairing replaces a key");
                };
                let paired_at = DateTime::<Utc>::from_timestamp_millis(self.timers.now_ms as i64)
                    .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
                let mut replaced = self.trust.store.clone();
                match replaced.replace_paired_peer_after_teardown(
                    host,
                    pubkey.clone(),
                    name.clone(),
                    reachability.clone(),
                    paired_at,
                ) {
                    Ok(update) => {
                        let job = self.pending.mint();
                        self.write_trust(queued, job, replaced, TrustChanged::Paired(update), out);
                    }
                    Err(error) => {
                        self.end_replacement(host, out);
                        self.trust_change_ended(queued.waiter, Err(error), out);
                    }
                }
            }
            (Stage::Writing, JobResult::Done) => {
                self.trust.store = staged;
                self.trust_published(Some((host, changed.clone())), out);
                let id = HostId(*host.as_bytes());
                if let TrustChanged::Paired(_) | TrustChanged::Removed(_) = &changed {
                    self.trust_committed(id, out);
                }
                if changed == TrustChanged::Paired(PairingUpdate::ReplacedPubkey) {
                    self.finish_replacement(id);
                }
                self.trust_change_ended(queued.waiter, Ok(changed), out);
            }
            (Stage::Writing, result) => {
                let error = match result {
                    JobResult::Failed(error) => error.clone(),
                    other => format!("{other:?}"),
                };
                tracing::warn!(%error, %host, "cannot save the trust store");
                if self.trust.store.replacement_pending.contains(&host) {
                    self.end_replacement(host, out);
                }
                self.trust_change_ended(queued.waiter, Err(TrustError::Write(error)), out);
            }
        }
        self.next_trust_change(out);
        true
    }

    /// A key replacement that will not be written: the old key is trusted
    /// again, and the host's connections may open under it.
    fn end_replacement(&mut self, host: Uuid, out: &mut Vec<Output>) {
        self.trust.store.replacement_pending.remove(&host);
        self.finish_replacement(HostId(*host.as_bytes()));
        self.trust_published(Some((host, TrustChanged::Unchanged)), out);
        out.push(Output::Shell(ShellWork::ReplacementEnded {
            host: HostId(*host.as_bytes()),
        }));
    }

    /// The shell's handshakes read trust from the store as published, and
    /// the replicas follow the paired hosts.
    fn trust_published(&mut self, changed: Option<(Uuid, TrustChanged)>, out: &mut Vec<Output>) {
        self.touched.host_set = true;
        out.push(Output::Shell(ShellWork::Trust {
            store: Box::new(self.trust.store.clone()),
            changed: changed.map(|(host, changed)| (HostId(*host.as_bytes()), changed)),
        }));
        self.trusted_if_moved(self.trust.store.paired_hosts(), out);
    }

    fn trust_change_ended(
        &mut self,
        waiter: Option<Waiter>,
        result: Result<TrustChanged, TrustError>,
        out: &mut Vec<Output>,
    ) {
        match waiter {
            None => {
                if let Err(error) = result {
                    tracing::warn!(%error, "a trust change was refused");
                }
            }
            Some(Waiter::Call(call)) => match result {
                Ok(changed) => answered(call, Response::Trust(changed), out),
                Err(error) => out.push(Output::EndCall {
                    call,
                    status: Status::Failed(CallError::Trust(error)),
                }),
            },
            Some(Waiter::Pairing(session)) => self.pairing_committed(session, result, out),
            Some(Waiter::Ssh { call, host, role }) => {
                self.ssh_committed(call, host, role, result, out)
            }
        }
    }
}

#[cfg(test)]
mod tests;
