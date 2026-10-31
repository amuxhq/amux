//! The daemon as one function of its state and an input.
//!
//! [`Daemon::step`] takes one decoded [`Input`] and appends the [`Output`]s
//! it wants done. Nothing inside a step locks, spawns, awaits or reads the
//! world: time and randomness arrive as inputs, sockets and codecs are the
//! driver's, and anything that only produces a result is a [`Job`] whose
//! outcome comes back as an input. Every step ends by checking the cheap
//! invariants, and a violation is reported as [`Output::Dump`] rather than
//! carried on from. See the principles in `docs/ARCHITECTURE.md`.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use store::Sqlite;

pub mod blobs;
pub mod catalogue;
pub mod checkpoints;
mod codec;
pub mod deliveries;
pub mod drafts;
mod families;
pub mod fingerprint;
pub mod generation;
pub mod host_set;
pub mod ids;
pub mod ingest;
mod io;
pub mod links;
pub mod names;
mod notifications;
pub mod pairing;
pub mod payload;
mod peers;
pub mod presence;
pub mod reach;
pub mod redact;
pub mod retention;
pub mod serve;
pub mod sources;
pub mod trust;

pub use blobs::Blobs;
pub use checkpoints::Checkpoints;
pub use deliveries::{Deliveries, DrainReport, delivery_envelope_id};
pub use families::{Create, Ended, FamilyState, Refusal, Resume};
pub use fingerprint::Fingerprint;
pub use ids::{
    AccountId, AgentId, AttemptId, CallId, ChannelId, HostId, JobId, LinkId, PairingId, TimerId,
};
pub use ingest::Journals;
pub use io::{
    IngestEnd, Ingested, Input, Job, Output, ParentSays, Recorded, Settings, ShellWork,
    SourcePolicy,
};
pub use links::LinkRegistry;
pub use notifications::Notifications;
pub use pairing::PairingState;
pub use presence::{HostView, Presence, PresenceState, Route};
pub use reach::{
    AttemptOutcome, DialPath, Dialling, Plan, Reach, RelayCarriers, RelayOutcome, RelayReach,
};
pub use redact::Redactor;
pub use retention::{Retain, Retained};
pub use serve::Answers;
pub use sources::{Sources, Standing, from_wire};

/// The daemon's whole state: the store owned outright, one machine per
/// concern, open calls by id, armed timers and jobs awaiting their result.
pub struct Daemon {
    store: Sqlite,
    routing: RoutingState,
    presence: PresenceState,
    host_set: host_set::HostSet,
    sources: Sources,
    families: FamilyState,
    pairing: PairingState,
    trust: trust::Trust,
    answers: Answers,
    journals: Journals,
    /// One open reader per agent journal this core has read, so a step
    /// neither lists the directory nor reopens the segment. Not state: a
    /// restored core opens them again as it reads.
    journal_readers: BTreeMap<AgentId, journal::Reader>,
    checkpoints: Checkpoints,
    notifications: Notifications,
    blobs: Blobs,
    deliveries: Deliveries,
    timers: Timers,
    pending: Pending,
    settings: Settings,
    options: StartOptions,
    /// What the step under way touched, for the cheap invariants at its
    /// end. Not part of the core's state: every step starts it empty.
    touched: Touched,
    /// The store's count of changed rows as of the last step's end, so a
    /// step that wrote anything is told apart. Not part of the core's
    /// state: a core over another connection counts from its own.
    changes: u64,
}

/// What a daemon runs under, fixed for the run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartOptions {
    /// The installation's generation as of this run: the one this host's
    /// own rows are served under.
    pub generation: u64,
    /// The directory holding each own agent's directory, named by its id.
    /// Not part of the checkpoint: a restored core is handed the place it
    /// reads its agents from, which a dump's replay unpacks elsewhere.
    #[serde(skip)]
    pub agents: PathBuf,
    /// The directory holding each paired host's replicas' files, by host
    /// and then by agent.
    pub replicas: PathBuf,
    /// How long one step may take before it is reported with a dump.
    pub budget: StepBudget,
}

impl Daemon {
    /// A daemon over `store` with nothing linked, open or armed.
    pub fn new(store: Sqlite, options: StartOptions) -> Self {
        Self {
            changes: store.changes(),
            trust: trust::Trust::load(&store),
            store,
            options,
            routing: RoutingState::default(),
            presence: PresenceState::default(),
            host_set: host_set::HostSet::default(),
            sources: Sources::default(),
            families: FamilyState::default(),
            pairing: PairingState::default(),
            answers: Answers::default(),
            journals: Journals::default(),
            journal_readers: BTreeMap::new(),
            checkpoints: Checkpoints::default(),
            notifications: Notifications::default(),
            blobs: Blobs::default(),
            deliveries: Deliveries::default(),
            timers: Timers::default(),
            pending: Pending::default(),
            settings: Settings::default(),
            touched: Touched::default(),
        }
    }

    /// One input in, outputs appended. Ends by checking the cheap
    /// invariants; a violation appends [`Output::Dump`], and so does a step
    /// that took longer than its budget.
    pub fn step(&mut self, input: Input, out: &mut Vec<Output>) {
        // The step's own duration is the one reading of the world a core
        // makes: it decides nothing but whether to ask for a dump.
        let started = Instant::now();
        let kind = input.kind();
        self.touched = Touched::default();
        // What waited out a WAL checkpoint that has ended goes first, so
        // writes land in the order they came.
        self.release_held(out);
        if let Some(input) = self.checkpoints.hold(input) {
            self.dispatch(input, out);
        }
        // A failed WAL checkpoint lets go of what waited at once; one that
        // is done serialized the core with it waiting, so it waits for the
        // next step, the driver's Checkpointed, which the ring segment
        // started at that state records.
        if !self.touched.serialized {
            self.release_held(out);
        }
        self.settle_deliveries(out);
        self.tell_presence();
        self.publish_host_set(out);
        self.follow_touched(out);
        self.arm_on_commit(out);
        let checked = self.check_step_invariants();
        let (budget, synced) = (self.options.budget, self.touched.synced_micros);
        for reason in finish_step(started, budget, synced, checked) {
            tracing::error!(%reason, input = kind, "a daemon core step asked for a dump");
            out.push(Output::Dump { reason });
        }
    }

    /// Steps one input: the work of [`Daemon::step`] between the hold and
    /// the checks.
    fn dispatch(&mut self, input: Input, out: &mut Vec<Output>) {
        match input {
            Input::Tick { now_ms, link_ms } => {
                self.timers.now_ms = now_ms;
                self.timers.link_ms = link_ms;
            }
            Input::ClientCall { call, request } => self.client_call(call, request, out),
            Input::PeerCall {
                from,
                call,
                request,
            } => self.peer_call(from, call, request, out),
            Input::PeerAnswer { call, answer } => self.peer_answered(call, answer, out),
            Input::CallCancelled { call } => {
                self.cancelled(call);
                self.message_cancelled(call, out);
            }
            Input::AnswerLagged { call } => self.lagged(call, out),
            Input::JobDone { job, result } if self.is_checkpoint(job) => {
                self.checkpoint_done(result, out)
            }
            Input::JobDone { job, result } => {
                if !self.push_done(job, &result, out)
                    && !self.blob_done(job, &result, out)
                    && !self.trust_job_done(job, out)
                {
                    self.registry_job(job, result, out);
                }
            }
            Input::Nudge { agent } => self.ingest(agent, out),
            Input::AgentHello { agent } => {
                self.hello(agent);
                self.process_hello(agent, out);
            }
            Input::ControlClosed { agent, ended } => self.control_closed(agent, ended, out),
            Input::Adopted { agent } => self.adopted(agent, out),
            Input::Exiting { agent } => self.exiting(agent),
            Input::Retain(retain) => self.retain(retain, out),
            Input::Swept => {
                self.swept(out);
                self.lanes_swept(out);
                self.deliveries_due();
                self.drain_notifications(out);
            }
            Input::Checkpointed => self.checkpointed(),
            Input::Verdict {
                agent,
                input_id,
                verdict,
            } => self.verdict(agent, &input_id, verdict, out),
            Input::Timer {
                id: TimerId::Lane(agent),
            } => self.lane_timer(agent, out),
            Input::Timer {
                id: id @ (TimerId::Start(_) | TimerId::Exit(_)),
            } => self.registry_timer(id, out),
            Input::Timer {
                id: TimerId::Ingest(agent),
            } => self.ingest_again(agent, out),
            Input::Timer {
                id: TimerId::Deliveries,
            } => self.deliveries_due(),
            Input::Timer {
                id: TimerId::Checkpoint,
            } => self.take_checkpoint(out),
            Input::Timer {
                id: TimerId::Notifications,
            } => self.notifications_due(out),
            Input::Timer {
                id: TimerId::Follow(host),
            } => self.follow_due(host, out),
            Input::Timer {
                id: TimerId::Source(host, agent),
            } => self.source_due(host, agent, out),
            Input::Configure(settings) => self.settings = settings,
            Input::ShellSession { agent, event } => self.publish_session(&agent, event, out),
            Input::LinkUp {
                link,
                peer,
                kind,
                resumed,
            } => self.link_up(link, peer, kind, resumed.as_ref(), out),
            Input::LinkDown { link, reason } => self.link_down(link, reason, out),
            Input::LinkCarried { link, kind } => self.link_carried(link, kind),
            Input::LinkControl { link, frame } => self.link_control(link, frame, out),
            Input::Credential {
                link,
                expires_at_ms,
                fetched_at_ms,
                free_every_ms,
            } => self.credential(link, expires_at_ms, fetched_at_ms, free_every_ms, out),
            Input::Timer {
                id: TimerId::Refresh(link),
            } => self.refresh_due(link, out),
            Input::WantChannel { link, peer } => self.want_channel(link, peer, out),
            Input::ChannelClosed { channel } => self.channel_closed(channel),
            Input::PlanChanged => self.plan_changed(out),

            Input::Started => self.trust_started(out),
            Input::ChangeTrust(change) => self.change_trust(change, None, out),
            Input::Policy(policy) => self.set_policy(policy, out),
            Input::HostForgotten { host } => {
                self.forget_addresses(host);
                self.forget_host(host);
                self.stop_reaching(host, out);
            }
            Input::Dialling(dialling) => self.dialling(dialling, out),
            Input::Listening(listening) => self.listening(listening),
            Input::Wanted { host } => self.want_host(host, out),
            Input::AttemptEnded {
                host,
                attempt,
                outcome,
            } => self.attempt_ended(host, attempt, outcome, out),
            Input::Timer {
                id: TimerId::Attempt(host),
            } => self.attempt_due(host, out),
            Input::Timer {
                id: TimerId::Retry(host),
            } => self.retry_due(host, out),
            Input::NetworkChanged => self.relay_network_changed(out),
            Input::RelayWanted(wanted) => self.relay_wanted(wanted, out),
            Input::RelayPrepared { attempt, host } => self.relay_prepared(attempt, &host),
            Input::RelayUdp { host, reached } => self.relay_udp(host, reached),
            Input::RelayLinked { attempt } => self.relay_linked(attempt),
            Input::RelayEnded { attempt, outcome } => self.relay_ended(attempt, outcome, out),
            Input::Timer {
                id: TimerId::RelayRetry,
            } => self.relay_retry_due(out),
            Input::PairingOpened { session, opened } => self.pairing_opened(session, opened, out),
            Input::PairingMessage { session, message } => {
                self.pairing_message(session, message, out)
            }
            Input::PairingClosed { session, status } => self.pairing_closed(session, status, out),
            Input::Timer {
                id: TimerId::Pairing(session),
            } => self.pairing_timer(session, out),
            Input::Timer {
                id: TimerId::PairWindow,
            } => self.window_timer(out),
            Input::Entropy(fresh) => self.entropy(fresh),
            Input::PeerRevoked { host } => self.peer_revoked(host),
            Input::ChannelAnswered { channel } => self.channel_answered(channel, out),
            Input::OwnHost(entry) => self.own_host_said(*entry),
            Input::Candidates { found } => self.candidates_found(found, out),
        }
    }

    /// The cheap predicates production checks after every step, at the
    /// boundaries the step crossed: each costs constant time per row the
    /// step touched and never scans the block.
    pub fn check_step_invariants(&self) -> Result<(), Violation> {
        for page in &self.touched.paged {
            let PageJoined {
                agent,
                before_order,
                lowest,
                highest,
                boundary,
            } = page;
            // Joined where the block began, the block now begins at the
            // page's lowest item, or where it did for an empty page.
            let joined_below = highest.is_none_or(|highest| highest < *before_order);
            if !joined_below || *boundary != Some(lowest.unwrap_or(*before_order)) {
                return Err(Violation {
                    invariant: PAGE_OFF_THE_BOUNDARY,
                    detail: format!(
                        "a page below order {before_order} holding orders {lowest:?}..={highest:?} \
                         joined replica {}'s block, whose lower boundary is now {boundary:?}",
                        hex(&agent.agent)
                    ),
                });
            }
        }
        for (parent, caught_up) in &self.touched.deleted_parents {
            if parent.host != self.store.own_host() && !caught_up {
                return Err(Violation {
                    invariant: DELETED_UNDER_A_STALE_LISTING,
                    detail: format!(
                        "children of {} were deleted with it before host {}'s listing caught up",
                        hex(&parent.agent),
                        hex(&parent.host)
                    ),
                });
            }
        }
        for (parent, id, parent_incarnation, removal) in &self.touched.outbox_removed {
            let held = match removal {
                deliveries::Removal::Accepted => self
                    .store
                    .item_by_input(parent, id)
                    .ok()
                    .flatten()
                    .is_some(),
                deliveries::Removal::Stale => self
                    .store
                    .agent(parent)
                    .ok()
                    .flatten()
                    .is_none_or(|row| row.incarnation != *parent_incarnation),
                deliveries::Removal::Unpaired => {
                    HostId::from_slice(&parent.host).is_some_and(|host| !self.sources.trusts(host))
                }
                deliveries::Removal::AcceptedThere | deliveries::Removal::StaleThere => true,
            };
            if !held {
                return Err(Violation {
                    invariant: OUTBOX_ROW_DELETED_UNACCEPTED,
                    detail: format!(
                        "the report {} to {} was deleted as {removal:?}, which its parent's row \
                         and items do not bear out",
                        hex(id),
                        hex(&parent.agent)
                    ),
                });
            }
        }
        for (id, already) in &self.touched.handed {
            if *already {
                return Err(Violation {
                    invariant: REPORT_HANDED_TWICE,
                    detail: format!(
                        "the report {} was handed to its parent while it was already in hand",
                        hex(id)
                    ),
                });
            }
        }
        for (agent, revision) in &self.touched.answered {
            let committed = match self.store.agent(agent) {
                Ok(Some(row)) => row.next_revision.saturating_sub(1),
                // Deleted later in the step, which ended its streams: what
                // they were answered was committed then.
                Ok(None) => continue,
                Err(_) => 0,
            };
            if *revision > committed {
                return Err(Violation {
                    invariant: ANSWERED_UNCOMMITTED,
                    detail: format!(
                        "an answer about agent {} carried revision {revision}; the store has \
                         committed through {committed}",
                        hex(&agent.agent)
                    ),
                });
            }
        }
        for (agent, below) in &self.touched.reclaimed {
            let durable = self.checkpoints.durable_cursor(*agent);
            if durable.is_none_or(|durable| *below > durable) {
                return Err(Violation {
                    invariant: RECLAIMED_ABOVE_CHECKPOINT,
                    detail: format!(
                        "agent {}'s journal was reclaimed below {below}; the newest checkpoint on \
                         the drive holds its cursor at {durable:?}",
                        hex(&agent.0)
                    ),
                });
            }
        }
        if let Some((opened, before)) = self.touched.opened_under
            && opened < before
        {
            return Err(Violation {
                invariant: GENERATION_FELL,
                detail: format!(
                    "a stream opened under generation {opened} after one under {before}"
                ),
            });
        }
        if self.touched.committed_while_held {
            return Err(Violation {
                invariant: NO_COMMIT_WHILE_HELD,
                detail: format!(
                    "the store committed while its WAL was being moved, {} inputs waiting",
                    self.checkpoints.held()
                ),
            });
        }
        Ok(())
    }

    /// The core as of this step, serialized. With the store's main file
    /// after a passive WAL checkpoint that no commit has followed, it is
    /// the daemon exactly. A checkpoint is read only by the build that
    /// wrote it: rings are reset with the stores they sit beside, so no
    /// other layout is ever met.
    pub fn checkpoint(&self) -> Vec<u8> {
        let core = (
            &self.routing,
            &self.presence,
            &self.sources,
            &self.families,
            (&self.pairing, &self.trust),
            (&self.answers, &self.host_set),
            &self.journals,
            &self.checkpoints,
            &self.notifications,
            &self.blobs,
            &self.deliveries,
            &self.timers,
            &self.pending,
            &self.settings,
            &self.options,
        );
        postcard::to_allocvec(&core).expect("the core's state serializes")
    }

    /// Rebuilds a core from a checkpoint and the store it was taken beside,
    /// its agents' directories under `agents`. A checkpoint that does not
    /// decode is refused, never half applied.
    pub fn restore(
        checkpoint: &[u8],
        store: Sqlite,
        agents: PathBuf,
    ) -> Result<Daemon, RestoreError> {
        let (
            routing,
            presence,
            sources,
            families,
            (pairing, trust),
            (answers, host_set),
            journals,
            checkpoints,
            notifications,
            blobs,
            deliveries,
            timers,
            pending,
            settings,
            mut options,
        ) = checkpoint_parts(checkpoint)?;
        options.agents = agents;
        let trust = trust.restored(&store);
        Ok(Daemon {
            routing,
            presence,
            sources,
            families,
            pairing,
            trust,
            answers,
            host_set,
            journals,
            checkpoints,
            notifications,
            blobs,
            deliveries,
            timers,
            pending,
            settings,
            ..Daemon::new(store, options)
        })
    }

    /// What the core remembers about its checkpoints.
    pub fn checkpoints(&self) -> &Checkpoints {
        &self.checkpoints
    }

    /// The store the core owns, for reading.
    pub fn store(&self) -> &Sqlite {
        &self.store
    }

    /// The store the core owned, once the core is done with.
    pub fn into_store(self) -> Sqlite {
        self.store
    }

    /// Sets how long one step may take before it is reported: a replay of
    /// a checkpoint the daemon took under its own budget runs under none,
    /// so that its steps are a function of their inputs alone.
    pub fn set_budget(&mut self, budget: StepBudget) {
        self.options.budget = budget;
    }
}

/// The single daemon's cheap invariants, as the sentences a violation
/// names.
pub const ANSWERED_UNCOMMITTED: Check = Check {
    id: "daemon.answered_uncommitted",
    sentence: "no answer carries a revision of this host's agent that the store has not committed",
};
pub const RECLAIMED_ABOVE_CHECKPOINT: Check = Check {
    id: "daemon.reclaimed_above_checkpoint",
    sentence: "an agent's journal is reclaimed only below the cursor of the newest checkpoint on the drive",
};
pub const GENERATION_FELL: Check = Check {
    id: "daemon.generation_fell",
    sentence: "the generation this host's streams open under only ever rises",
};

/// No store commit lands while a checkpoint's WAL job moves the WAL into
/// the main file, so the main file then holds exactly what the checkpoint
/// serializes beside it.
pub const NO_COMMIT_WHILE_HELD: Check = Check {
    id: "daemon.no_commit_while_held",
    sentence: "nothing is committed while a checkpoint moves the WAL into the main file",
};

/// The sources' and families' cheap invariants.
pub const PAGE_OFF_THE_BOUNDARY: Check = Check {
    id: "daemon.page_off_the_boundary",
    sentence: "a page from the origin joins a replica's block only at the block's lower boundary",
};
pub const DELETED_UNDER_A_STALE_LISTING: Check = Check {
    id: "daemon.deleted_under_a_stale_listing",
    sentence: "a child is deleted for its parent on another host only under that host's caught-up listing",
};

/// The deliveries' cheap invariants.
pub const OUTBOX_ROW_DELETED_UNACCEPTED: Check = Check {
    id: "daemon.outbox_row_deleted_unaccepted",
    sentence: "an outbox row is deleted only once its parent \
     accepted it, or once the parent's incarnation or pairing it was for is gone",
};
pub const REPORT_HANDED_TWICE: Check = Check {
    id: "daemon.report_handed_twice",
    sentence: "a report is handed to its parent at most once at a time",
};

/// Every check the daemon core makes at the end of a step.
pub const CHECKS: &[Check] = &[
    ANSWERED_UNCOMMITTED,
    RECLAIMED_ABOVE_CHECKPOINT,
    GENERATION_FELL,
    NO_COMMIT_WHILE_HELD,
    PAGE_OFF_THE_BOUNDARY,
    DELETED_UNDER_A_STALE_LISTING,
    OUTBOX_ROW_DELETED_UNACCEPTED,
    REPORT_HANDED_TWICE,
];

/// An id as a violation names it.
fn hex(id: &[u8]) -> String {
    id.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A page from the origin that joined a replica's block, as the store held
/// the row after it.
#[derive(Debug)]
struct PageJoined {
    agent: store::AgentKey,
    /// The order the page was asked below.
    before_order: u64,
    /// The page's lowest and highest orders; none for an empty page.
    lowest: Option<u64>,
    highest: Option<u64>,
    /// The block's lower boundary after the page joined.
    boundary: Option<u64>,
}

/// What the step under way touched, as the cheap invariants judge it.
#[derive(Debug, Default)]
struct Touched {
    /// Each own agent answered about, with the newest revision an answer
    /// carried.
    answered: Vec<(store::AgentKey, u64)>,
    /// Each agent whose journal was reclaimed, and the cursor below which.
    reclaimed: Vec<(AgentId, u64)>,
    /// The generation an own stream opened under, and the highest one
    /// opened under before it.
    opened_under: Option<(u64, u64)>,
    /// Each host whose routes or presence the step moved.
    hosts: Vec<HostId>,
    /// Something a host set entry is made of moved.
    host_set: bool,
    /// Each page that joined a replica's block.
    paged: Vec<PageJoined>,
    /// Each parent whose deletion this host's children followed, and
    /// whether its host's listing had caught up then.
    deleted_parents: Vec<(store::AgentKey, bool)>,
    /// The host whose listing this step reads at its CaughtUp.
    judging: Option<HostId>,
    /// Something may have made an outbox row deliverable or stale.
    deliveries_due: bool,
    /// Each outbox row deleted: its parent, envelope id, the parent
    /// incarnation it was for, and why.
    outbox_removed: Vec<(store::AgentKey, Vec<u8>, u32, deliveries::Removal)>,
    /// Each report handed to its parent, by envelope id, and whether one
    /// with that id was already in hand.
    handed: Vec<(Vec<u8>, bool)>,
    /// The step began moving the WAL into the main file.
    hold_began: bool,
    /// The step serialized the core for a checkpoint.
    serialized: bool,
    /// The store committed while the WAL was being moved.
    committed_while_held: bool,
    /// How long the step waited on the drive for a synced write, which its
    /// budget leaves out.
    synced_micros: u64,
}

impl Touched {
    fn answered(&mut self, agent: &store::AgentKey, revision: u64) {
        match self.answered.iter_mut().find(|(seen, _)| seen == agent) {
            Some((_, newest)) => *newest = (*newest).max(revision),
            None => self.answered.push((agent.clone(), revision)),
        }
    }

    fn host(&mut self, host: HostId) {
        if !self.hosts.contains(&host) {
            self.hosts.push(host);
        }
    }
}

/// Links and the routes they make.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingState {
    links: LinkRegistry,
    catalogue: catalogue::Catalogue,
    /// The addresses each trusted host last listed for itself, each with
    /// where it came from: the trust file's, until the host says them
    /// again.
    addresses: std::collections::BTreeMap<HostId, Vec<payload::PeerAddress>>,
    /// The address the relay last saw this host's packets come from.
    observed: Option<std::net::SocketAddr>,
    /// Each host being dialled, and when it is tried next.
    reach: reach::Reach,
}

/// The clock as last told and the timers armed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timers {
    now_ms: u64,
    link_ms: u64,
    /// The timers armed that the core keeps count of, so one cancelled
    /// after it rang is told apart.
    armed: std::collections::BTreeSet<TimerId>,
}

/// Jobs and calls to peers awaiting their result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pending {
    /// The last job id minted.
    next_job: u64,
    /// The calls made of peers.
    peers: peers::PeerCalls,
}

pub use core_ring::{Check, DumpReason, StepBudget, Violation, finish_step};

/// The core's state as a checkpoint holds it, in the order
/// [`Daemon::checkpoint`] writes it.
type Parts = (
    RoutingState,
    PresenceState,
    Sources,
    FamilyState,
    (PairingState, trust::Trust),
    (Answers, host_set::HostSet),
    Journals,
    Checkpoints,
    Notifications,
    Blobs,
    Deliveries,
    Timers,
    Pending,
    Settings,
    StartOptions,
);

/// A checkpoint's state read back.
fn checkpoint_parts(checkpoint: &[u8]) -> Result<Parts, RestoreError> {
    postcard::from_bytes(checkpoint).map_err(RestoreError::of)
}

/// Why a checkpoint could not be restored: its bytes do not decode under
/// this build's layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreError(String);

impl RestoreError {
    fn of(error: postcard::Error) -> Self {
        Self(error.to_string())
    }
}

impl fmt::Display for RestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the checkpoint does not decode: {}", self.0)
    }
}

impl std::error::Error for RestoreError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_checkpoint_that_does_not_decode_is_refused() {
        let store = Sqlite::open_in_memory(*b"own-host-0000000").expect("a store opens");
        assert!(Daemon::restore(&[0xff], store, PathBuf::new()).is_err());
    }
}
