//! One daemon, its store and one agent's journal, under crash, power loss
//! and reboot.
//!
//! The daemon is the real `daemon_core::Daemon`; the state holds it as the
//! checkpoint it takes of itself, beside its store's dump and the journal's
//! segment files as bytes. Every transition rebuilds the core from that
//! checkpoint over the real store, lays the journal out in a directory of
//! this thread's, steps the core with the input the driver would feed it,
//! and reads all three back. The driver's part (running the checkpoint job,
//! carrying answers to the one subscriber) and the world's (the agent
//! writing its journal, the page cache writing it back, the process dying,
//! the machine losing power or rebooting) are the environment here.
//!
//! What the drive holds is tracked apart from what the running process
//! sees. Store commits are not synced one by one, so the drive has the
//! store as of its last passive WAL checkpoint, the first of a checkpoint's
//! two jobs, or of its last clean shutdown. Inputs go on arriving while
//! that job runs, and the core holds those that would write until it ends. The agent's journal is written with
//! plain writes, so the drive has it as of the page cache's last writeback.
//! A power loss takes both back to what the drive has; a crash loses
//! neither. The generation record is written durably at each start and
//! marked clean as the last write of a clean shutdown, as the installation
//! does. A step's only durable effects are one store transaction and the
//! deletion of whole journal segments, oldest first, so a crash inside a
//! step leaves a state a crash between two steps also reaches.
//!
//! One paired peer comes back on resumed links, each carrying a read of
//! the agent as early data, which the driver holds until the core has
//! decided the identity the link's ticket names; anyone holding those
//! packets may replay them on a connection of its own; and this host may
//! remove the peer's pin, which the store holds: the removal is the core's
//! trust change, synced to the drive in the step that makes it, after
//! which the driver closes the peer's link. A fresh handshake from a peer
//! whose pin was removed fails its pin check before the core hears of it,
//! so after the removal only a resumed link can reach the core.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use daemon_core::generation::{LastRun, at_start};
use daemon_core::payload::{
    CarrierKind, ClientRequest, JobResult, PeerRequest, PublicKey, Response,
};
use daemon_core::trust::{TrustChange, TrustChanged, TrustStore};
use daemon_core::{
    AgentId, CallId, Checkpoint, Daemon, DumpReason, HostId, Input, Job, JobId, LinkId, Output,
    Route, ShellWork, StartOptions, StepBudget, TimerId,
};
use store::{AgentKey, AgentRow, Dump, Sqlite};
use wire::{Item, Step, session_event};

use crate::{Machine, Property};

const OWN: &[u8; 16] = b"own-host-0000000";
const AGENT: AgentId = AgentId(*b"agent-0000000000");
/// Where the core is told agent directories live whenever it checkpoints,
/// so its checkpoint names the same place on every thread. A step points it
/// at this thread's directory first.
const AGENTS: &str = "/agents";
/// Each frame the agent writes is a segment of its own, so reclaiming a
/// segment is reclaiming a frame.
const SEGMENT_BYTES: u64 = 1;
/// Enough to hold every row the agent writes at any of the bounds.
const TAIL: u32 = 64;
const REFUSAL: &str = "the drive is full";
/// The paired peer.
const PEER: HostId = HostId(*b"peer-host-000000");
/// The peer's resumed link, and the connection a replay of its early data
/// arrives on.
const PEER_LINK: LinkId = LinkId(1);
const REPLAY_LINK: LinkId = LinkId(2);
/// The calls the driver numbers the peer's early reads with, apart from
/// the client's.
const EARLY_CALL: CallId = CallId(1 << 32);
const REPLAYED_CALL: CallId = CallId((1 << 32) + 1);

/// The key the peer's tickets name: the one this host pinned for it.
fn peer_key() -> PublicKey {
    let store = TrustStore::of_hosts([PEER]);
    let key = store
        .pubkey_for_host(uuid::Uuid::from_bytes(PEER.0))
        .expect("the peer is pinned");
    PublicKey(key.to_vec())
}

/// The peer's early read: a page of the agent's rows.
fn early_read() -> PeerRequest {
    PeerRequest::ListItems(wire::ListItemsRequest {
        agent_id: AGENT.0.to_vec(),
        before_order: None,
        page_size: TAIL,
    })
}

fn agent() -> AgentKey {
    AgentKey::new(OWN.to_vec(), AGENT.0.to_vec())
}

/// One daemon under crash, within its bounds.
#[derive(Clone, Debug)]
pub struct SingleDaemon {
    /// Frames the agent writes, all told.
    pub frames: u8,
    /// Checkpoints the daemon takes, all told.
    pub checkpoints: u8,
    /// Crashes, power losses and reboots, all told.
    pub downs: u8,
    /// Subscriptions a client opens, all told.
    pub opens: u8,
    /// Times a subscriber falls behind its queue, all told.
    pub lags: u8,
    /// Commits the drive refuses, all told.
    pub refusals: u8,
    /// Resumed links the peer comes back on, all told.
    pub resumes: u8,
    /// Replays of the peer's early data, all told.
    pub replays: u8,
    /// Whether this host may remove the peer's pin.
    pub unpin: bool,
}

impl SingleDaemon {
    /// The bounds `just test` explores exhaustively: enough frames that a
    /// checkpoint reclaims a segment past the ones kept for dumps, and one
    /// crash, power loss or reboot. A refused commit is left to the nightly
    /// bounds, and the peer to bounds of its own, which keeps the run
    /// inside its CI budget.
    pub const fn ci() -> Self {
        Self {
            frames: 4,
            checkpoints: 2,
            downs: 1,
            opens: 1,
            lags: 1,
            refusals: 0,
            resumes: 0,
            replays: 0,
            unpin: false,
        }
    }

    /// The peer's bounds `just test` explores beside [`Self::ci`]: the peer
    /// resuming once, its early data replayed once and its pin removed,
    /// around a crash, power loss or reboot, a checkpoint and two frames,
    /// so an early read meets a checkpoint the ring replays and a restart.
    pub const fn peer() -> Self {
        Self {
            frames: 2,
            checkpoints: 1,
            opens: 0,
            lags: 0,
            resumes: 1,
            replays: 1,
            unpin: true,
            ..Self::ci()
        }
    }

    /// The peer around the CI bounds' journal and subscriber, run nightly.
    pub const fn peer_nightly() -> Self {
        Self {
            resumes: 1,
            replays: 1,
            unpin: true,
            ..Self::ci()
        }
    }

    /// The larger bounds a nightly run explores: two crashes, power losses
    /// or reboots, in any order, and a commit the drive refuses.
    pub const fn nightly() -> Self {
        Self {
            downs: 2,
            refusals: 1,
            ..Self::ci()
        }
    }
}

/// The journal's segment files: each one's start offset and its bytes.
type Journal = BTreeMap<u64, Vec<u8>>;

fn journal_end(journal: &Journal) -> u64 {
    journal
        .last_key_value()
        .map_or(0, |(start, bytes)| start + bytes.len() as u64)
}

/// The installation's generation record, as the drive holds it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Record {
    boot: u8,
    clean: bool,
    counter: u64,
}

/// A client's Subscribe stream on the agent, as the client reads it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Subscriber {
    call: CallId,
    /// Each row the stream carried, as its key and revision, in order.
    received: Vec<(String, u64)>,
    /// The stream said it fell behind and ended.
    lagged: bool,
    /// The stream ended otherwise.
    ended: bool,
}

impl Subscriber {
    fn open(&self) -> bool {
        !self.lagged && !self.ended
    }
}

/// The daemon while it runs.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Run {
    /// The core, as its own checkpoint.
    core: Vec<u8>,
    generation: u64,
    /// The generation the run before this one served.
    previous: Option<u64>,
    /// The machine restarted under a run that had not shut down cleanly
    /// since the run before this one started.
    owed: bool,
    /// The checkpoint's WAL job the driver runs for the core.
    job: Option<JobId>,
    /// The core handed the driver its checkpoint; the driver's word that
    /// the ring started there is owed.
    landing: bool,
    /// Inputs the core holds until its WAL checkpoint ends.
    held: usize,
    /// The timer for the agent's next ingest batch is armed.
    ingest_armed: bool,
    subscriber: Option<Subscriber>,
    calls: u64,
    /// The peer's resumed link is up and the core took it.
    peer_up: bool,
    /// The core published a store without the peer; the driver has yet
    /// to close the peer's link.
    revoking: bool,
}

/// How much of the bounds a path has used.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Used {
    checkpoints: u8,
    downs: u8,
    opens: u8,
    lags: u8,
    refusals: u8,
    resumes: u8,
    replays: u8,
    unpins: u8,
}

/// The newest checkpoint the driver's input ring holds, with what it was
/// taken beside, and every input stepped since with the world as that
/// step found it: the journal, and whether the drive refused the commit.
#[derive(Clone, Default)]
struct Ring {
    base: Option<(Vec<u8>, Dump, Journal)>,
    inputs: Vec<(Input, Journal, bool)>,
}

#[derive(Clone)]
pub struct State {
    /// The store as the running process, or the page cache, has it.
    store: Dump,
    /// The store as the drive has it.
    durable: Dump,
    /// The agent's cursor in each.
    cursor: u64,
    durable_cursor: u64,
    /// The store's rows, as key and revision, oldest revision first.
    rows: Vec<(String, u64)>,
    journal: Journal,
    /// The journal bytes the drive has.
    written_back: u64,
    /// Frames the agent has written, which names its next key.
    frames: u8,
    record: Option<Record>,
    boot: u8,
    run: Option<Run>,
    /// A run started since the last clean shutdown.
    unfinished: bool,
    /// The machine restarted under an unfinished run since the last start.
    owed: bool,
    last_generation: Option<u64>,
    used: Used,
    /// Ingest moved the cursor back to a journal that lost its tail.
    rewound: bool,
    /// A step answered a row the store did not hold once the step was done.
    broadcast_uncommitted: Option<String>,
    /// The core asked for a dump.
    dumped: Option<String>,
    /// The ring's replay came out other than the live daemon.
    diverged: Option<String>,
    /// The store pins the peer.
    pinned: bool,
    /// The core answered an early read of the peer's.
    early_answered: bool,
    /// The driver served an early call on a link the core did not list.
    early_unaccepted: Option<String>,
    /// The core answered the peer after its pin was removed.
    answered_unpinned: Option<String>,
    /// A replay of early data changed what the store holds.
    replay_changed: Option<String>,
    /// Carried along the path that first reached this state, and not part
    /// of the state's identity: two paths to one daemon are one state, and
    /// the replay checks the path the checker found first.
    ring: Ring,
}

type Identity<'a> = (
    (&'a Dump, &'a Dump, &'a Journal, u64, u8),
    (
        &'a Option<Record>,
        u8,
        &'a Option<Run>,
        bool,
        bool,
        Option<u64>,
    ),
    (
        &'a Used,
        bool,
        &'a Option<String>,
        &'a Option<String>,
        &'a Option<String>,
    ),
    (
        bool,
        bool,
        &'a Option<String>,
        &'a Option<String>,
        &'a Option<String>,
    ),
);

impl State {
    fn identity(&self) -> Identity<'_> {
        (
            (
                &self.store,
                &self.durable,
                &self.journal,
                self.written_back,
                self.frames,
            ),
            (
                &self.record,
                self.boot,
                &self.run,
                self.unfinished,
                self.owed,
                self.last_generation,
            ),
            (
                &self.used,
                self.rewound,
                &self.broadcast_uncommitted,
                &self.dumped,
                &self.diverged,
            ),
            (
                self.pinned,
                self.early_answered,
                &self.early_unaccepted,
                &self.answered_unpinned,
                &self.replay_changed,
            ),
        )
    }
}

impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
}

impl Eq for State {}

impl Hash for State {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.identity().hash(state);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// The agent writes its next frame to its journal.
    Append,
    /// The agent nudges the daemon: its journal grew.
    Nudge,
    /// The timer for the agent's next ingest batch fires.
    IngestAgain,
    /// The checkpoint timer fires, or someone asks for a checkpoint.
    Checkpoint,
    /// The driver's WAL job moves the WAL into the store's main file,
    /// syncs it and reports it is done; the core is serialized.
    WalDone,
    /// The WAL job could not move the whole WAL: a reader held it, or the
    /// drive refused.
    WalBusy,
    /// The driver started its ring at the checkpoint and says so; the core
    /// lets go of the journal below it.
    Reclaim,
    /// A client opens a Subscribe stream on the agent.
    SubscriberOpen,
    /// The subscriber's queue overflows: the driver reports it lagging.
    AnswerLagged,
    /// The page cache writes the journal back to the drive.
    Writeback,
    /// The daemon starts: it decides its generation, writes the record and
    /// serves.
    Start,
    /// The machine reboots cleanly: a running daemon shuts down first,
    /// flushing its store, and marks the record clean last.
    Reboot,
    /// The peer comes back on a resumed link carrying its early read; the
    /// driver serves the read once the core has taken the link.
    PeerResumes,
    /// Someone asks this host to remove the peer's pin.
    Unpin,
    /// The driver, told the peer is no longer trusted, closes its link as
    /// revoked.
    Revoke,
    /// The daemon's process dies at a step.
    Crash,
    /// The machine loses power, taking the store and the journal back to
    /// what the drive has.
    PowerLoss,
    /// The drive refuses the commit of the next nudge.
    WriteRefused,
    /// The peer's early data arrives again, replayed on a connection of its
    /// own.
    Duplicate,
}

impl Machine for SingleDaemon {
    type State = State;
    type Action = Action;

    fn init(&self) -> State {
        crate::share_sqlite_across_threads();
        let mut store = Sqlite::open_in_memory(OWN.to_vec()).expect("a store opens");
        store
            .put_agent(&AgentRow::new(agent(), wire::Kind::Codex, "/src"))
            .expect("the store takes its agent");
        TrustStore::of_hosts([PEER]).write_for_test(&mut store);
        let store = store.dump().expect("the store dumps");
        State {
            durable: store.clone(),
            store,
            cursor: 0,
            durable_cursor: 0,
            rows: Vec::new(),
            journal: Journal::new(),
            written_back: 0,
            frames: 0,
            record: None,
            boot: 0,
            run: None,
            unfinished: false,
            owed: false,
            last_generation: None,
            used: Used::default(),
            rewound: false,
            broadcast_uncommitted: None,
            dumped: None,
            diverged: None,
            pinned: true,
            early_answered: false,
            early_unaccepted: None,
            answered_unpinned: None,
            replay_changed: None,
            ring: Ring::default(),
        }
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        if s.frames < self.frames {
            out.push(Action::Append);
        }
        if s.written_back < journal_end(&s.journal) {
            out.push(Action::Writeback);
        }
        let Some(run) = &s.run else {
            out.push(Action::Start);
            if s.used.downs < self.downs {
                out.push(Action::PowerLoss);
                out.push(Action::Reboot);
            }
            return;
        };
        // A nudge is enabled while the journal and the cursor disagree,
        // which is when one could do something.
        if journal_end(&s.journal) != s.cursor {
            out.push(Action::Nudge);
            if s.used.refusals < self.refusals {
                out.push(Action::WriteRefused);
            }
        }
        if run.ingest_armed {
            out.push(Action::IngestAgain);
        }
        match &run.job {
            None if run.landing => out.push(Action::Reclaim),
            None if s.used.checkpoints < self.checkpoints => out.push(Action::Checkpoint),
            None => {}
            Some(_) => {
                out.push(Action::WalDone);
                out.push(Action::WalBusy);
            }
        }
        match &run.subscriber {
            Some(subscriber) if subscriber.open() => {
                if s.used.lags < self.lags {
                    out.push(Action::AnswerLagged);
                }
            }
            _ if s.used.opens < self.opens => out.push(Action::SubscriberOpen),
            _ => {}
        }
        if !run.peer_up && s.used.resumes < self.resumes {
            out.push(Action::PeerResumes);
        }
        // Only early data the peer sent can be replayed. It is replayed
        // while the core holds no input back, so what the store shows after
        // it is the replay's doing alone.
        if s.used.resumes > 0 && s.used.replays < self.replays && run.held == 0 {
            out.push(Action::Duplicate);
        }
        if self.unpin && s.used.unpins == 0 {
            out.push(Action::Unpin);
        }
        if run.revoking {
            out.push(Action::Revoke);
        }
        if s.used.downs < self.downs {
            out.push(Action::Crash);
            out.push(Action::PowerLoss);
            out.push(Action::Reboot);
        }
    }

    fn next(&self, s: &State, action: &Action) -> Option<State> {
        let mut next = s.clone();
        match action {
            Action::Append => {
                with_scratch(&LIVE, |scratch| {
                    scratch.lay_out(&next.journal);
                    let mut writer = journal::Writer::open(scratch.journal_dir(), SEGMENT_BYTES)
                        .expect("the journal opens");
                    writer
                        .append(&frame(next.frames))
                        .expect("the journal takes a frame");
                    next.journal = scratch.read_back();
                });
                next.frames += 1;
            }
            Action::Writeback => next.written_back = journal_end(&next.journal),
            Action::Nudge => self.step(&mut next, Input::Nudge { agent: AGENT }, false),
            Action::WriteRefused => {
                next.used.refusals += 1;
                self.step(&mut next, Input::Nudge { agent: AGENT }, true);
            }
            Action::IngestAgain => self.step(
                &mut next,
                Input::Timer {
                    id: TimerId::Ingest(AGENT),
                },
                false,
            ),
            Action::Checkpoint => {
                next.used.checkpoints += 1;
                self.step(
                    &mut next,
                    Input::Timer {
                        id: TimerId::Checkpoint,
                    },
                    false,
                );
            }
            Action::Reclaim => {
                let run = next
                    .run
                    .as_mut()
                    .expect("the driver answers only while the daemon runs");
                assert!(run.landing, "a checkpoint to answer");
                run.landing = false;
                self.step(&mut next, Input::Checkpointed, false);
            }
            Action::WalDone | Action::WalBusy => {
                if *action == Action::WalDone {
                    // The core holds every write while the job runs, so the
                    // store the process has is what was committed before it.
                    next.durable = next.store.clone();
                    next.durable_cursor = next.cursor;
                }
                let run = next
                    .run
                    .as_mut()
                    .expect("a job runs only while the daemon does");
                let job = run.job.take().expect("a job to end");
                let result = match action {
                    Action::WalDone => JobResult::Done,
                    _ => JobResult::Failed("the WAL was busy".into()),
                };
                self.step(&mut next, Input::JobDone { job, result }, false);
            }
            Action::SubscriberOpen => {
                next.used.opens += 1;
                let run = next
                    .run
                    .as_mut()
                    .expect("a subscriber opens on a running daemon");
                run.calls += 1;
                let call = CallId(run.calls);
                run.subscriber = Some(Subscriber {
                    call,
                    received: Vec::new(),
                    lagged: false,
                    ended: false,
                });
                let request = ClientRequest::SubscribeSession {
                    agent_id: AGENT.0.to_vec(),
                    tail: TAIL,
                };
                let input = Input::ClientCall { call, request };
                self.step(&mut next, input, false);
            }
            Action::AnswerLagged => {
                next.used.lags += 1;
                let call = next
                    .run
                    .as_ref()
                    .and_then(|run| run.subscriber.as_ref())
                    .expect("a subscriber lags")
                    .call;
                self.step(&mut next, Input::AnswerLagged { call }, false);
            }
            Action::Start => {
                start(&mut next);
                self.step(&mut next, Input::Started, false);
            }
            Action::PeerResumes => {
                next.used.resumes += 1;
                if self.resumed(&mut next, PEER_LINK) {
                    next.run.as_mut().expect("a running daemon").peer_up = true;
                    self.serve_early(&mut next, PEER_LINK, EARLY_CALL);
                }
            }
            Action::Duplicate => {
                next.used.replays += 1;
                let before = next.store.clone();
                if self.resumed(&mut next, REPLAY_LINK) {
                    self.serve_early(&mut next, REPLAY_LINK, REPLAYED_CALL);
                }
                // The replayer cannot finish the handshake, and its
                // connection dies.
                let reason = wire::LinkCloseReason::UserShutdown;
                let down = Input::LinkDown {
                    link: REPLAY_LINK,
                    reason,
                };
                self.step(&mut next, down, false);
                if next.store != before && next.replay_changed.is_none() {
                    next.replay_changed = Some("the store changed".into());
                }
            }
            Action::Unpin => {
                next.used.unpins += 1;
                let host = uuid::Uuid::from_bytes(PEER.0);
                let remove = Input::ChangeTrust(TrustChange::Remove { host });
                self.step(&mut next, remove, false);
            }
            Action::Revoke => {
                let run = next.run.as_mut().expect("a running daemon");
                run.revoking = false;
                if std::mem::take(&mut run.peer_up) {
                    let reason = wire::LinkCloseReason::UserRevoked;
                    let down = Input::LinkDown {
                        link: PEER_LINK,
                        reason,
                    };
                    self.step(&mut next, down, false);
                }
            }
            Action::Crash => {
                next.used.downs += 1;
                next.run = None;
            }
            Action::PowerLoss => {
                next.used.downs += 1;
                next.run = None;
                next.store = next.durable.clone();
                next.cursor = next.durable_cursor;
                let kept = next.written_back;
                next.journal = std::mem::take(&mut next.journal)
                    .into_iter()
                    .filter_map(|(start, mut bytes)| {
                        (start < kept).then(|| {
                            bytes.truncate((kept - start).min(bytes.len() as u64) as usize);
                            (start, bytes)
                        })
                    })
                    .collect();
                next.rows = rows_of(&next.store);
                next.boot += 1;
                next.owed |= next.unfinished;
            }
            Action::Reboot => {
                next.used.downs += 1;
                if next.run.take().is_some() {
                    // The shutdown flushes every store, then marks the
                    // record clean as its last write.
                    next.unfinished = false;
                    if let Some(record) = &mut next.record {
                        record.clean = true;
                    }
                }
                // A clean reboot writes everything back on its way down.
                next.durable = next.store.clone();
                next.durable_cursor = next.cursor;
                next.written_back = journal_end(&next.journal);
                next.boot += 1;
                next.owed |= next.unfinished;
            }
        }
        Some(next)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut properties = vec![
            Property::always(COMMIT_BEFORE_BROADCAST, |_, s: &State| {
                s.broadcast_uncommitted.is_none()
            }),
            Property::always(
                "the open is one cut: an open subscriber has read every row the store holds exactly once, \
                 in the order the store revised them, and nothing else",
                |_, s: &State| {
                    s.run
                        .as_ref()
                        .and_then(|run| run.subscriber.as_ref())
                        .is_none_or(|subscriber| {
                            !subscriber.open() || subscriber.received == s.rows
                        })
                },
            ),
            Property::always(RECLAIMED_BELOW_CURSOR, |_, s: &State| {
                reclaimed_below_cursor(s.journal.keys().next().copied(), s.durable_cursor)
            }),
            Property::always(GENERATION_BUMPS_AFTER_UNCLEAN, |_, s: &State| {
                s.run
                    .as_ref()
                    .is_none_or(|run| bumps_after_unclean(run.owed, run.previous, run.generation))
            }),
            Property::always(GENERATION_BUMPS_ONLY_AFTER_UNCLEAN, |_, s: &State| {
                s.run.as_ref().is_none_or(|run| {
                    bumps_only_after_unclean(run.owed, run.previous, run.generation)
                })
            }),
            Property::always(REPRODUCES, |_, s: &State| s.diverged.is_none()),
            Property::always(EARLY_AFTER_ACCEPT, |_, s: &State| {
                s.early_unaccepted.is_none()
            }),
            Property::always(UNPINNED_NEVER_ANSWERED, |_, s: &State| {
                s.answered_unpinned.is_none()
            }),
            Property::always(REPLAY_CHANGES_NOTHING, |_, s: &State| {
                s.replay_changed.is_none()
            }),
            Property::always(NEVER_DUMPS, |_, s: &State| s.dumped.is_none()),
        ];
        // Each set of bounds judges the sometimes of what it explores: the
        // peer's bounds hold little journal, the journal's leave the peer
        // out.
        if self.resumes > 0 {
            properties.push(Property::sometimes(
                "a resumed peer's early read is answered",
                |_, s: &State| s.early_answered,
            ));
        } else {
            properties.extend([
                Property::sometimes("a subscriber lags", |_, s: &State| {
                    s.run
                        .as_ref()
                        .and_then(|run| run.subscriber.as_ref())
                        .is_some_and(|subscriber| subscriber.lagged)
                }),
                Property::sometimes(
                    "a subscriber reads a row committed after its opening",
                    |_, s: &State| {
                        s.run
                            .as_ref()
                            .and_then(|run| run.subscriber.as_ref())
                            .is_some_and(|subscriber| {
                                subscriber.open() && subscriber.received.len() > 1
                            })
                    },
                ),
                Property::sometimes(
                    "a write waits out a checkpoint's WAL job",
                    |_, s: &State| s.run.as_ref().is_some_and(|run| run.held > 0),
                ),
                Property::sometimes("a checkpoint reclaims a journal segment", |_, s: &State| {
                    s.journal
                        .first_key_value()
                        .is_some_and(|(oldest, _)| *oldest > 0)
                }),
                Property::sometimes(
                    "ingest rewinds the cursor to a journal that lost its tail",
                    |_, s: &State| s.rewound,
                ),
                Property::sometimes("a start serves the next generation", |_, s: &State| {
                    s.run.as_ref().is_some_and(|run| run.generation > 1)
                }),
            ]);
        }
        properties
    }
}

impl SingleDaemon {
    /// Steps the core with one input, as the driver does: recorded in the
    /// ring first, then stepped over the store and the journal, then what it
    /// asked for carried out.
    fn step(&self, next: &mut State, input: Input, refuse: bool) {
        self.step_reading(next, input, refuse);
    }

    /// The peer's link comes up resumed, its ticket naming the key this
    /// host pinned for it: whether the core took it.
    fn resumed(&self, next: &mut State, link: LinkId) -> bool {
        let input = Input::LinkUp {
            link,
            peer: PEER,
            kind: CarrierKind::Direct,
            resumed: Some(peer_key()),
        };
        let (_, out) = self.step_reading(next, input, false);
        !out.contains(&Output::RefuseResumed { link })
    }

    /// The driver serves the peer's early read held on `link`.
    fn serve_early(&self, next: &mut State, link: LinkId, call: CallId) {
        let input = Input::PeerCall {
            from: PEER,
            call,
            request: early_read(),
        };
        let (listed, out) = self.step_reading(next, input, false);
        if !listed.contains(&Route::Direct(link)) && next.early_unaccepted.is_none() {
            next.early_unaccepted = Some(format!("served on {link:?}, the core lists {listed:?}"));
        }
        let answered = out
            .iter()
            .any(|output| matches!(output, Output::Answer { call: to, .. } if *to == call));
        if answered {
            next.early_answered = true;
            if !next.pinned && next.answered_unpinned.is_none() {
                next.answered_unpinned = Some(format!("answered on {link:?}"));
            }
        }
    }

    /// Steps the core as [`Self::step`] does, and says which routes it
    /// listed to the peer before the step and what the step asked for.
    fn step_reading(
        &self,
        next: &mut State,
        input: Input,
        refuse: bool,
    ) -> (Vec<Route>, Vec<Output>) {
        let journal_before = next.journal.clone();
        let cursor_before = next.cursor;
        let run = next.run.as_mut().expect("a step needs a running daemon");
        let (listed, out, uncommitted, core, store, journal) = with_scratch(&LIVE, |scratch| {
            let mut daemon = scratch.daemon(&run.core, &next.store, &next.journal);
            let listed = daemon.links().routes_to(PEER);
            let mut out = Vec::new();
            stepped(
                &mut daemon,
                input.clone(),
                refuse.then_some(REFUSAL),
                &mut out,
            );
            run.held = daemon.checkpoints().held();
            // Each row a step answered, and whether the store holds it.
            let uncommitted = answered_uncommitted(&out, daemon.store(), &[agent()]);
            let (core, store) = scratch.put_back(daemon);
            let cursor = store.cursor(&agent()).unwrap();
            let rows = rows_in(store);
            let dump = store.dump().expect("the store dumps");
            scratch.holds = dump.clone();
            (
                listed,
                out,
                uncommitted,
                core,
                (dump, cursor, rows),
                scratch.read_back(),
            )
        });
        run.core = core;
        (next.store, next.cursor, next.rows) = store;
        next.journal = journal;
        if next.cursor < cursor_before {
            next.rewound = true;
        }
        next.ring.inputs.push((input, journal_before, refuse));

        if next.broadcast_uncommitted.is_none() {
            next.broadcast_uncommitted = uncommitted;
        }
        for output in &out {
            match output {
                Output::Answer {
                    call,
                    response: Response::Session(event),
                } => {
                    let Some(subscriber) = run.subscriber.as_mut().filter(|s| s.call == *call)
                    else {
                        continue;
                    };
                    match &event.of {
                        Some(session_event::Of::Item(item)) => {
                            subscriber.received.push((item.key.clone(), item.revision))
                        }
                        Some(session_event::Of::Lagged(_)) => subscriber.lagged = true,
                        _ => {}
                    }
                }
                Output::EndCall { call, .. } => {
                    if let Some(subscriber) = run.subscriber.as_mut().filter(|s| s.call == *call) {
                        subscriber.ended = true;
                    }
                }
                Output::ArmTimer {
                    id: TimerId::Ingest(_),
                    ..
                } => run.ingest_armed = true,
                Output::CancelTimer {
                    id: TimerId::Ingest(_),
                } => run.ingest_armed = false,
                Output::Job {
                    id,
                    op: Job::CheckpointWal,
                } => run.job = Some(*id),
                Output::Shell(ShellWork::Trust {
                    changed: Some((PEER, TrustChanged::Removed(_))),
                    ..
                }) => {
                    // The removal was synced in the step that made it, and
                    // every commit before it with it.
                    next.pinned = false;
                    next.durable = next.store.clone();
                    next.durable_cursor = next.cursor;
                    run.revoking = true;
                }
                Output::Checkpoint { state } => {
                    run.landing = true;
                    // The driver's ring starts again at this checkpoint.
                    next.ring = Ring {
                        base: Some((state.clone(), next.store.clone(), next.journal.clone())),
                        inputs: Vec::new(),
                    };
                }
                Output::Dump { reason } => {
                    next.dumped.get_or_insert_with(|| format!("{reason:?}"));
                }
                _ => {}
            }
        }
        if next.diverged.is_none() {
            next.diverged = replay(next);
        }
        (listed, out)
    }
}

/// Steps the core with one input, its store refusing every write with
/// `refuse` when the drive does. The model and a dump's replay both step
/// the core through here.
pub fn stepped(daemon: &mut Daemon, input: Input, refuse: Option<&str>, out: &mut Vec<Output>) {
    if let Some(why) = refuse {
        let why = why.replace('\'', "''");
        daemon
            .store()
            .connection()
            .execute_batch(&format!(
                "CREATE TEMP TRIGGER refuse BEFORE INSERT ON agents \
                 BEGIN SELECT RAISE(ABORT, '{why}'); END;"
            ))
            .expect("the refusal installs");
    }
    daemon.step(input, out);
    if refuse.is_some() {
        daemon
            .store()
            .connection()
            .execute_batch("DROP TRIGGER refuse;")
            .expect("the refusal goes");
    }
}

/// The daemon starts: the generation is decided from the record and the
/// boot as the installation decides it, the record is rewritten durably,
/// and the core starts over the store as the process finds it.
fn start(next: &mut State) {
    let boot_id = |boot: u8| format!("boot-{boot}");
    let last = next
        .record
        .as_ref()
        .map(|record| (boot_id(record.boot), record));
    let generation = at_start(
        last.as_ref().map(|(boot, record)| LastRun {
            boot_id: boot,
            clean: record.clean,
            counter: record.counter,
        }),
        &boot_id(next.boot),
    );
    next.record = Some(Record {
        boot: next.boot,
        clean: false,
        counter: generation,
    });
    let core = with_scratch(&LIVE, |scratch| {
        scratch.load(&next.store);
        let store = scratch.store.take().expect("the scratch store is home");
        let daemon = Daemon::new(
            store,
            StartOptions {
                generation,
                agents: PathBuf::from(AGENTS),
                // The model is a function of its actions alone, so no step
                // may ask for a dump because the machine ran slowly.
                budget: StepBudget::UNLIMITED,
                ..StartOptions::default()
            },
        );
        scratch.put_back(daemon).0
    });
    next.ring = Ring {
        base: Some((core.clone(), next.store.clone(), next.journal.clone())),
        inputs: Vec::new(),
    };
    next.run = Some(Run {
        core,
        generation,
        previous: next.last_generation,
        owed: next.owed,
        job: None,
        landing: false,
        held: 0,
        ingest_armed: false,
        subscriber: None,
        calls: 0,
        peer_up: false,
        revoking: false,
    });
    next.unfinished = true;
    next.owed = false;
    next.last_generation = Some(generation);
}

/// Replays the ring: the core restored from its newest checkpoint over the
/// store it was taken beside, then stepped through every input since, each
/// against the journal as the live step found it. Says how the outcome
/// differs from the live daemon, if it does.
fn replay(live: &State) -> Option<String> {
    let (core, store, journal) = live.ring.base.as_ref()?;
    let live_core = &live.run.as_ref()?.core;
    with_scratch(&REPLAY, |scratch| {
        let mut daemon = scratch.daemon(core, store, journal);
        let mut out = Vec::new();
        for (input, journal, refuse) in &live.ring.inputs {
            scratch.lay_out(journal);
            stepped(
                &mut daemon,
                input.clone(),
                refuse.then_some(REFUSAL),
                &mut out,
            );
            out.clear();
            // The step may have reclaimed segments.
            scratch.read_back();
        }
        let (core, store) = scratch.put_back(daemon);
        let store = store.dump().expect("the store dumps");
        scratch.holds = store.clone();
        let journal = scratch.read_back();
        if core != *live_core {
            Some(format!(
                "after {} inputs the core differs:\n         replayed {}\n         live     {}",
                live.ring.inputs.len(),
                summary(&core),
                summary(live_core)
            ))
        } else if store != live.store {
            Some(format!(
                "after {} inputs the store differs",
                live.ring.inputs.len()
            ))
        } else if journal != live.journal {
            Some(format!(
                "after {} inputs the journal differs: replayed {:?}, live {:?}",
                live.ring.inputs.len(),
                journal.keys().collect::<Vec<_>>(),
                live.journal.keys().collect::<Vec<_>>()
            ))
        } else {
            None
        }
    })
}

/// A core's checkpoint, as a counterexample shows it.
fn summary(core: &[u8]) -> String {
    match Checkpoint::from_bytes(core) {
        Ok(checkpoint) => format!("{:02x?}", checkpoint.state),
        Err(error) => error.to_string(),
    }
}

/// Every row a step answered to any call, as the call, the key and the
/// revision.
/// The single daemon's promises the composed net judges too, each with
/// the predicate both judge it by.
pub const COMMIT_BEFORE_BROADCAST: &str = "commit before broadcast: every row a step answers \
     is in the store once the step is done, at the revision answered or a later one the step wrote over it";
pub(crate) const RECLAIMED_BELOW_CURSOR: &str = "a segment is reclaimed only below the last checkpoint's \
     cursor: the journal still holds every frame from the cursor the drive's store has";
pub(crate) const GENERATION_BUMPS_AFTER_UNCLEAN: &str = "the generation bumps after an unclean reboot: a \
     start after the machine restarted under a daemon that had not shut down cleanly serves a higher \
     generation than the run before";
pub(crate) const GENERATION_BUMPS_ONLY_AFTER_UNCLEAN: &str = "the generation bumps only after an unclean \
     reboot: a start after a crash on the same boot, or after a clean shutdown, serves the generation of \
     the run before";

/// The single daemon's promises a replay of a dump judges too.
pub const REPRODUCES: &str = "a checkpoint plus the inputs since reproduces the state: the core restored \
     from the newest checkpoint and stepped through every input since, against the journal as each found \
     it, is the live core, store and journal";
pub const NEVER_DUMPS: &str = "the core never asks for a dump";

/// The promises early data keeps on a resumed link; replication judges
/// them too.
pub(crate) const EARLY_AFTER_ACCEPT: &str =
    "early calls are served only after the core accepts the resumed identity";
pub(crate) const UNPINNED_NEVER_ANSWERED: &str =
    "a host whose pin was removed never answers, resumed or not";
pub(crate) const REPLAY_CHANGES_NOTHING: &str = "replayed early data changes no state";

/// A promise one step broke, and what broke it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Broken {
    pub promise: &'static str,
    pub detail: String,
}

/// What one step of a core running anywhere broke of the promises a step
/// is judged by alone, `out` being what it asked for: a dump it asked for
/// (named by the invariant that failed, when one did), or a row it
/// answered that the store does not hold. The
/// rest of the single daemon's promises need the world the model tracks
/// around the core: what reached the drive, the run before.
pub fn judge_step(daemon: &Daemon, out: &[Output]) -> Option<Broken> {
    if let Some(reason) = out.iter().find_map(|output| match output {
        Output::Dump { reason } => Some(reason),
        _ => None,
    }) {
        return Some(match reason {
            DumpReason::Violation(violation) => Broken {
                promise: violation.invariant.sentence,
                detail: violation.detail.clone(),
            },
            DumpReason::OverBudget { .. } => Broken {
                promise: NEVER_DUMPS,
                detail: reason.to_string(),
            },
        });
    }

    answered_unheld(daemon, out).map(|row| Broken {
        promise: COMMIT_BEFORE_BROADCAST,
        detail: format!("answered {row} before the store held it"),
    })
}

/// The first row `out` answers on a session that the daemon's store does
/// not hold, under any agent it has a row for.
pub(crate) fn answered_unheld(daemon: &Daemon, out: &[Output]) -> Option<String> {
    let answers = out.iter().any(|output| {
        matches!(
            output,
            Output::Answer {
                response: Response::Session(_),
                ..
            }
        )
    });
    if !answers {
        return None;
    }
    let store = daemon.store();
    let agents: Vec<AgentKey> = store
        .agents()
        .unwrap()
        .into_iter()
        .map(|row| row.agent)
        .collect();
    answered_uncommitted(out, store, &agents)
}

/// The first row `out` answers on a session that `store` does not hold at
/// the revision answered or a later one, under any of `agents`. A step that
/// ingests a batch revising one row twice answers both revisions and
/// commits them in one transaction, which leaves the later.
pub(crate) fn answered_uncommitted(
    out: &[Output],
    store: &Sqlite,
    agents: &[AgentKey],
) -> Option<String> {
    out.iter()
        .filter_map(|output| match output {
            Output::Answer {
                response: Response::Session(event),
                ..
            } => match &event.of {
                Some(session_event::Of::Item(item)) => Some((&item.key, item.revision)),
                _ => None,
            },
            _ => None,
        })
        .find(|(key, revision)| {
            !agents.iter().any(|agent| {
                store
                    .get(agent, key)
                    .unwrap()
                    .is_some_and(|row| row.revision >= *revision)
            })
        })
        .map(|(key, revision)| format!("{key} r{revision}"))
}

/// Whether a journal whose oldest segment starts at `oldest` still holds
/// every frame from `cursor`.
pub(crate) fn reclaimed_below_cursor(oldest: Option<u64>, cursor: u64) -> bool {
    oldest.is_none_or(|oldest| oldest <= cursor)
}

/// Whether a start serving `generation` after a run that served
/// `previous` bumped it if `owed`: the machine restarted under a run that
/// had not shut down cleanly.
pub(crate) fn bumps_after_unclean(owed: bool, previous: Option<u64>, generation: u64) -> bool {
    !owed || previous.is_none_or(|previous| generation > previous)
}

/// Whether a start serving `generation` after a run that served
/// `previous` kept it unless `owed`.
pub(crate) fn bumps_only_after_unclean(owed: bool, previous: Option<u64>, generation: u64) -> bool {
    owed || generation == previous.unwrap_or(1)
}

/// The frame the agent writes `n`th: one row under a key of its own.
fn frame(n: u8) -> Step {
    Step {
        items: vec![Item {
            key: format!("k{n}"),
            text: "x".into(),
            ..Item::default()
        }],
        ..Step::default()
    }
}

fn rows_in(store: &Sqlite) -> Vec<(String, u64)> {
    let mut rows: Vec<(String, u64)> = store
        .rows(&agent())
        .unwrap()
        .into_iter()
        .map(|item| (item.key, item.revision))
        .collect();
    rows.sort_by_key(|(_, revision)| *revision);
    rows
}

fn rows_of(dump: &Dump) -> Vec<(String, u64)> {
    with_scratch(&LIVE, |scratch| {
        scratch.load(dump);
        rows_in(scratch.store.as_ref().expect("the scratch store is home"))
    })
}

/// A thread's store and agent directory, to take a transition in. They are
/// loaded only when they do not already hold what the transition starts
/// from, which saves most loads: a checker takes every transition from one
/// state in turn.
struct Scratch {
    store: Option<Sqlite>,
    holds: Dump,
    agents: tempfile::TempDir,
    journal: Journal,
}

thread_local! {
    static LIVE: RefCell<Option<Scratch>> = const { RefCell::new(None) };
    static REPLAY: RefCell<Option<Scratch>> = const { RefCell::new(None) };
}

fn with_scratch<R>(
    which: &'static std::thread::LocalKey<RefCell<Option<Scratch>>>,
    f: impl FnOnce(&mut Scratch) -> R,
) -> R {
    which.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        let scratch = scratch.get_or_insert_with(|| Scratch {
            store: Some(Sqlite::open_in_memory(OWN.to_vec()).expect("a store opens")),
            holds: Dump::default(),
            agents: tempfile::tempdir().expect("a directory for agents"),
            journal: Journal::new(),
        });
        f(scratch)
    })
}

impl Scratch {
    fn journal_dir(&self) -> PathBuf {
        daemon_core::ingest::journal_dir(self.agents.path(), AGENT)
    }

    fn load(&mut self, dump: &Dump) {
        if self.holds != *dump {
            self.store
                .as_mut()
                .expect("the scratch store is home")
                .load(dump)
                .expect("a store loads");
            self.holds = dump.clone();
        }
    }

    /// Makes the agent's journal directory hold exactly `journal`.
    fn lay_out(&mut self, journal: &Journal) {
        if self.journal == *journal {
            return;
        }
        let dir = self.journal_dir();
        std::fs::create_dir_all(&dir).expect("the journal directory");
        for (start, bytes) in &self.journal {
            if journal.get(start) != Some(bytes) {
                std::fs::remove_file(journal::segment_path(&dir, *start)).expect("a segment goes");
            }
        }
        for (start, bytes) in journal {
            if self.journal.get(start) != Some(bytes) {
                std::fs::write(journal::segment_path(&dir, *start), bytes)
                    .expect("a segment is written");
            }
        }
        self.journal = journal.clone();
    }

    /// The journal as the directory now holds it. A segment holds one
    /// frame and is never written again once it is, and the core only ever
    /// deletes whole segments, so only a segment new since the last look is
    /// read.
    fn read_back(&mut self) -> Journal {
        let dir = self.journal_dir();
        let journal: Journal = journal::segments(&dir)
            .expect("the journal lists")
            .into_iter()
            .map(|start| {
                let bytes = self.journal.get(&start).cloned().unwrap_or_else(|| {
                    std::fs::read(journal::segment_path(&dir, start)).expect("a segment reads")
                });
                (start, bytes)
            })
            .collect();
        self.journal = journal.clone();
        journal
    }

    /// The core restored from `core` over `store`, reading `journal` here.
    fn daemon(&mut self, core: &[u8], store: &Dump, journal: &Journal) -> Daemon {
        self.load(store);
        self.lay_out(journal);
        let checkpoint = Checkpoint::from_bytes(core).expect("a checkpoint reads");
        let store = self.store.take().expect("the scratch store is home");
        let mut daemon = Daemon::restore(checkpoint, store).expect("a checkpoint restores");
        daemon.move_agents(self.agents.path().to_path_buf());
        daemon
    }

    /// Takes the store back from a core, and the core's checkpoint as it
    /// names the agents' place on every thread. The store is then this
    /// scratch's, holding whatever the core left in it.
    fn put_back(&mut self, mut daemon: Daemon) -> (Vec<u8>, &Sqlite) {
        daemon.move_agents(PathBuf::from(AGENTS));
        let core = daemon.checkpoint().to_bytes();
        self.store = Some(daemon.into_store());
        self.holds = Dump::default();
        (
            core,
            self.store.as_ref().expect("the scratch store is home"),
        )
    }
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("State");
        match &self.run {
            Some(run) => debug
                .field("generation", &run.generation)
                .field("job", &run.job)
                .field("subscriber", &run.subscriber),
            None => debug.field("down", &true),
        };
        debug
            .field("rows", &self.rows)
            .field("cursor", &self.cursor)
            .field("durable_cursor", &self.durable_cursor)
            .field("segments", &self.journal.keys().collect::<Vec<_>>())
            .field("journal_end", &journal_end(&self.journal))
            .field("written_back", &self.written_back)
            .field("record", &self.record)
            .field("boot", &self.boot);
        for (name, value) in [
            ("broadcast_uncommitted", &self.broadcast_uncommitted),
            ("dumped", &self.dumped),
            ("diverged", &self.diverged),
        ] {
            if let Some(value) = value {
                debug.field(name, value);
            }
        }
        debug.finish()
    }
}
