//! The hosts this profile trusts: each paired host's pinned key, its name,
//! how it was last reached and what it last said of itself.
//!
//! The core holds the trust store and is its only writer. The store's
//! `trusted_hosts` table holds it; the core keeps a copy it serves and
//! publishes to the shell's handshakes. A change commits in the step that
//! makes it, before the copy changes, so a change the store refused never
//! shows. Pairing, unpairing and a pairing undone commit synced, so a
//! machine that loses power the moment after still holds them; the facts a
//! host keeps telling (its addresses, whether it is signed in) commit as
//! the store's other rows do. A change asked for while the store's WAL is
//! being moved into its main file waits, with any behind it, until the
//! move is done.
//!
//! Replacing a host's key closes everything opened under the old one
//! first: the old key stops being trusted at once, the driver tears the
//! host's connections down as [`Job::TeardownHost`], and the new key is
//! written after.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::SocketAddr;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use store::{Durability, Sqlite, TrustedHost};
use uuid::Uuid;

use crate::Daemon;
use crate::ids::{CallId, HostId, JobId, PairingId};
use crate::io::{Job, Output, ShellWork};
use crate::payload::{CallError, PeerAddress, Response, Status};
use crate::serve::answered;

/// The length of an Ed25519 public key, the only key a host pins.
pub const PUBKEY_LEN: usize = 32;

/// How a pairing reached the host: through the account's relay, through a
/// host both are paired with, over SSH to a destination, or at addresses.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Reachability {
    Cloud,
    Ssh(String),
    Direct { addrs: Vec<SocketAddr> },
}

/// What the profile knows of one paired host.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TrustEntry {
    pub pubkey: Vec<u8>,
    pub name: String,
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
    /// itself. `amux internal ssh-link` there joins the profile whose id is the host's.
    pub ssh: Option<String>,
    pub signed_in: Option<bool>,
    /// The operating system the peer announced, last it said one.
    pub platform: wire::Platform,
    /// Every address the peer last listed for itself, with where each came
    /// from. The address it last answered at is its `Direct` reachability.
    pub listed: Vec<PeerAddress>,
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
    /// The store refused the change, in its words.
    Write(String),
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
    /// The paired hosts the profile store holds. A row that does not read
    /// (which only a hand-edited store could hold) is left out and logged,
    /// so one bad row never stops the profile.
    pub fn load(store: &Sqlite) -> TrustStore {
        let rows = store.trusted_hosts().unwrap_or_else(|error| {
            tracing::error!(%error, "cannot read the paired hosts");
            Vec::new()
        });
        let entries = rows
            .into_iter()
            .filter_map(|row| match TrustEntry::from_row(&row) {
                Ok(entry) => Some(entry),
                Err(error) => {
                    tracing::error!(%error, "a paired host's row does not read; leaving it out");
                    None
                }
            })
            .collect();
        TrustStore {
            entries,
            replacement_pending: BTreeSet::new(),
        }
    }

    /// The store's row for `host_id`, or `None` when it is not paired.
    fn row(&self, host_id: Uuid) -> Option<TrustedHost> {
        self.entries
            .get(&host_id)
            .map(|entry| entry.to_row(host_id))
    }

    /// Writes this store's trust into `store`, for tests and checkers that
    /// start a core with hosts already paired.
    #[doc(hidden)]
    pub fn write_for_test(&self, store: &mut Sqlite) {
        for host in self.entries.keys() {
            let row = self.row(*host).expect("the entry is held");
            store
                .put_trusted_host(&row, Durability::Normal)
                .expect("the store takes the paired host");
        }
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
                self.entries
                    .insert(host_id, TrustEntry::paired(pubkey, name, reachability));
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
                if let Some(reachability) = reachability {
                    entry.reached(reachability);
                }
                Ok(PairingUpdate::Updated)
            }
            Some(entry) => {
                *entry = TrustEntry::paired(pubkey, name, reachability);
                Ok(PairingUpdate::ReplacedPubkey)
            }
            None => {
                self.entries
                    .insert(host_id, TrustEntry::paired(pubkey, name, reachability));
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
}

impl TrustEntry {
    fn to_row(&self, host_id: Uuid) -> TrustedHost {
        TrustedHost {
            host_id: host_id.as_bytes().to_vec(),
            pubkey: self.pubkey.clone(),
            name: self.name.clone(),
            on_account: self.on_account,
            signed_in: self.signed_in,
            platform: self.platform as i32,
            ssh: self.ssh.clone(),
            direct: serde_json::to_string(&self.direct).expect("addresses serialize"),
            listed: serde_json::to_string(&self.listed).expect("addresses serialize"),
        }
    }

    fn from_row(row: &TrustedHost) -> Result<(Uuid, TrustEntry), String> {
        let host = Uuid::from_slice(&row.host_id).map_err(|error| error.to_string())?;
        validate_pubkey(&row.pubkey).map_err(|error| error.to_string())?;
        let entry = TrustEntry {
            pubkey: row.pubkey.clone(),
            name: row.name.clone(),
            on_account: row.on_account,
            direct: serde_json::from_str(&row.direct).map_err(|error| error.to_string())?,
            ssh: row.ssh.clone(),
            signed_in: row.signed_in,
            platform: wire::Platform::try_from(row.platform).unwrap_or_default(),
            listed: serde_json::from_str(&row.listed).map_err(|error| error.to_string())?,
        };
        Ok((host, entry))
    }

    /// A host just paired, reached as `reachability`.
    fn paired(pubkey: Vec<u8>, name: String, reachability: Option<Reachability>) -> TrustEntry {
        let mut entry = TrustEntry {
            pubkey,
            name,
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

/// What a change did, once the store holds it.
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

/// A key replacement waiting for the host's connections under the old key
/// to close.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Replacing {
    queued: Queued,
    job: JobId,
}

/// The paired hosts as the store holds them, a key replacement under way
/// and the changes waiting behind it. The paired hosts are the store's, so
/// a checkpoint leaves them out and a restored core reads them from the
/// store it was taken beside; which keys are being replaced it keeps.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Trust {
    store: TrustStore,
    replacing: Option<Replacing>,
    queue: VecDeque<Queued>,
}

/// What a checkpoint keeps of [`Trust`].
#[derive(Serialize, Deserialize)]
struct TrustCheckpoint {
    replacement_pending: BTreeSet<Uuid>,
    replacing: Option<Replacing>,
    queue: VecDeque<Queued>,
}

impl Serialize for Trust {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        TrustCheckpoint {
            replacement_pending: self.store.replacement_pending.clone(),
            replacing: self.replacing.clone(),
            queue: self.queue.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Trust {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let checkpoint = TrustCheckpoint::deserialize(deserializer)?;
        Ok(Trust {
            store: TrustStore {
                entries: BTreeMap::new(),
                replacement_pending: checkpoint.replacement_pending,
            },
            replacing: checkpoint.replacing,
            queue: checkpoint.queue,
        })
    }
}

impl Trust {
    /// The trust `store` holds, nothing being changed.
    pub(crate) fn load(store: &Sqlite) -> Trust {
        Trust {
            store: TrustStore::load(store),
            ..Trust::default()
        }
    }

    /// A checkpoint's trust, its paired hosts read again from `store`.
    pub(crate) fn restored(mut self, store: &Sqlite) -> Trust {
        self.store.entries = TrustStore::load(store).entries;
        self
    }

    pub fn store(&self) -> &TrustStore {
        &self.store
    }

    pub(crate) fn jobs(&self, out: &mut Vec<JobId>) {
        out.extend(self.replacing.as_ref().map(|replacing| replacing.job));
    }

    pub(crate) fn canonical(&self, ids: &crate::fingerprint::Ids) -> Trust {
        let mut trust = self.clone();
        if let Some(replacing) = &mut trust.replacing {
            replacing.job = ids.jobs.get(replacing.job);
        }
        trust
    }
}

impl Daemon {
    /// The core is starting: its paired hosts are what the store holds.
    pub(crate) fn trust_started(&mut self, out: &mut Vec<Output>) {
        self.remember_addresses();
        self.trust_published(None, out);
    }

    /// Puts `store` in the profile store and serves it, for tests and
    /// checkers that start a core with hosts already paired; the next
    /// [`Input::Started`](crate::Input::Started) publishes it.
    #[doc(hidden)]
    pub fn trust_for_test(&mut self, store: TrustStore) {
        store.write_for_test(&mut self.store);
        // Written beneath the core, as a store that already held them: no
        // step of its committed them.
        self.changes = self.store.changes();
        self.trust.store = store;
    }

    /// The trust store the core serves.
    pub fn trust(&self) -> &TrustStore {
        &self.trust.store
    }

    /// A change asked for by a call, answered once the store holds it.
    pub(crate) fn change_trust_call(
        &mut self,
        call: CallId,
        change: TrustChange,
        out: &mut Vec<Output>,
    ) {
        self.change_trust(change, Some(Waiter::Call(call)), out);
    }

    /// Queues `change` behind any waiting, and starts what may start.
    pub(crate) fn change_trust(
        &mut self,
        change: TrustChange,
        waiter: Option<Waiter>,
        out: &mut Vec<Output>,
    ) {
        self.trust.queue.push_back(Queued { change, waiter });
        self.next_trust_change(out);
    }

    /// Makes the changes waiting, in order, until one must wait: for a key
    /// replacement's teardown, or for the WAL to finish moving into the
    /// main file (no commit may land while it moves, nor in the step that
    /// serialized the core beside it).
    pub(crate) fn next_trust_change(&mut self, out: &mut Vec<Output>) {
        while self.trust.replacing.is_none()
            && !self.checkpoints.holding()
            && !self.touched.serialized
            && let Some(queued) = self.trust.queue.pop_front()
        {
            self.start_trust_change(queued, out);
        }
    }

    fn start_trust_change(&mut self, queued: Queued, out: &mut Vec<Output>) {
        let mut staged = self.trust.store.clone();
        let changed = match &queued.change {
            TrustChange::Pair {
                host,
                pubkey,
                name,
                reachability,
            } => {
                let paired = staged.upsert_paired_peer(
                    *host,
                    pubkey.clone(),
                    name.clone(),
                    reachability.clone(),
                );
                // A host the relay claimed before this profile trusted it,
                // as one on the account is from sign-in, is not claimed
                // again when the pairing lands: the relay reaching it is
                // kept now, or the next launch's first flight leaves the
                // relay out.
                if paired.is_ok() && self.relay_claims(HostId(*host.as_bytes())) {
                    staged.remember_on_the_account(*host);
                }
                paired.map(TrustChanged::Paired)
            }
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
        if changed == TrustChanged::Paired(PairingUpdate::PubkeyReplacementRequired) {
            // The old key stops being trusted now, before anything opened
            // under it is closed.
            let host = queued.change.host();
            let job = self.pending.mint();
            self.trust.store.replacement_pending.insert(host);
            self.begin_replacement(HostId(*host.as_bytes()));
            self.trust_published(None, out);
            out.push(Output::Job {
                id: job,
                op: Job::TeardownHost {
                    host: HostId(*host.as_bytes()),
                },
            });
            self.trust.replacing = Some(Replacing { queued, job });
            return;
        }
        self.commit_trust(queued, staged, changed, out);
    }

    /// Commits the host the change is about as `staged` holds it, then
    /// serves `staged`; a commit the store refuses ends the change with
    /// nothing changed.
    fn commit_trust(
        &mut self,
        queued: Queued,
        staged: TrustStore,
        changed: TrustChanged,
        out: &mut Vec<Output>,
    ) {
        let host = queued.change.host();
        // Whether the host is trusted at all, and under which key, is what
        // must outlive the machine losing power; what it last said of
        // itself it says again.
        let durability = match queued.change {
            TrustChange::Pair { .. } | TrustChange::Remove { .. } | TrustChange::Restore { .. } => {
                Durability::Synced
            }
            _ => Durability::Normal,
        };
        // The sync waits on the drive, which the step's budget, kept for
        // work a step should not be doing, leaves out: the one other
        // reading of the clock a step makes.
        let started = Instant::now();
        let written = match staged.row(host) {
            Some(row) => self.store.put_trusted_host(&row, durability),
            None => self.store.remove_trusted_host(host.as_bytes(), durability),
        };
        if durability == Durability::Synced {
            self.touched.synced_micros += started.elapsed().as_micros() as u64;
        }
        if let Err(error) = written {
            let error = error.to_string();
            tracing::warn!(%error, %host, "cannot save the trust store");
            if self.trust.store.replacement_pending.contains(&host) {
                self.end_replacement(host, out);
            }
            self.trust_change_ended(queued.waiter, Err(TrustError::Write(error)), out);
            return;
        }
        self.trust.store = staged;
        self.trust_published(Some((host, changed.clone())), out);
        let id = HostId(*host.as_bytes());
        if let TrustChanged::Paired(_) | TrustChanged::Removed(_) = &changed {
            self.trust_committed(id, out);
        }
        // Where the host may be reached is the trust store's in part: an
        // address it answered at, an SSH destination, the account.
        self.tell_paths(id, out);
        self.reconsider(id, out);
        if changed == TrustChanged::Paired(PairingUpdate::ReplacedPubkey) {
            self.finish_replacement(id);
        }
        self.trust_change_ended(queued.waiter, Ok(changed), out);
    }

    /// A key replacement's teardown finished; returns whether `job` was
    /// one. Whatever the teardown said, nothing of the old key is trusted
    /// any more: the replacement is written on.
    pub(crate) fn trust_job_done(&mut self, job: JobId, out: &mut Vec<Output>) -> bool {
        if self
            .trust
            .replacing
            .as_ref()
            .is_none_or(|replacing| replacing.job != job)
        {
            return false;
        }
        let Replacing { queued, .. } = self.trust.replacing.take().expect("a replacement");
        let host = queued.change.host();
        let TrustChange::Pair {
            pubkey,
            name,
            reachability,
            ..
        } = &queued.change
        else {
            unreachable!("only a pairing replaces a key");
        };
        let mut replaced = self.trust.store.clone();
        match replaced.replace_paired_peer_after_teardown(
            host,
            pubkey.clone(),
            name.clone(),
            reachability.clone(),
        ) {
            Ok(update) => self.commit_trust(queued, replaced, TrustChanged::Paired(update), out),
            Err(error) => {
                self.end_replacement(host, out);
                self.trust_change_ended(queued.waiter, Err(error), out);
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
