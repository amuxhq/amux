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
//! store as of its last passive WAL checkpoint, which the checkpoint job
//! runs, or of its last clean shutdown. The agent's journal is written with
//! plain writes, so the drive has it as of the page cache's last writeback.
//! A power loss takes both back to what the drive has; a crash loses
//! neither. The generation record is written durably at each start and
//! marked clean as the last write of a clean shutdown, as the installation
//! does. A step's only durable effects are one store transaction and the
//! deletion of whole journal segments, oldest first, so a crash inside a
//! step leaves a state a crash between two steps also reaches.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use daemon_core::generation::{LastRun, at_start};
use daemon_core::payload::{ClientRequest, JobResult, Response};
use daemon_core::{
    AgentId, CallId, Checkpoint, Daemon, Input, Job, JobId, Output, StartOptions, StepBudget,
    TimerId,
};
use store::{AgentKey, AgentRow, Dump, Sqlite};
use wire::{Item, Step, session_event};

use crate::{Fault, Host, Machine, Property};

const OWN: &[u8; 16] = b"own-host-0000000";
const AGENT: AgentId = AgentId(*b"agent-0000000000");
/// The daemon, as the fault vocabulary names hosts.
const DAEMON: Host = Host(0);
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
}

impl SingleDaemon {
    /// The bounds `just test` explores exhaustively: enough frames that a
    /// checkpoint reclaims a segment past the ones kept for dumps, and one
    /// crash, power loss or reboot.
    pub const fn ci() -> Self {
        Self {
            frames: 4,
            checkpoints: 2,
            downs: 1,
            opens: 1,
            lags: 1,
            refusals: 1,
        }
    }

    /// The larger bounds a nightly run explores: two crashes, power losses
    /// or reboots, in any order.
    pub const fn nightly() -> Self {
        Self {
            downs: 2,
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

/// The checkpoint job the driver runs for the core.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CheckpointJob {
    id: JobId,
    /// The store's WAL and the core's state are on the drive.
    written: bool,
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
    job: Option<CheckpointJob>,
    /// The timer for the agent's next ingest batch is armed.
    ingest_armed: bool,
    subscriber: Option<Subscriber>,
    calls: u64,
}

/// How much of the bounds a path has used.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Used {
    checkpoints: u8,
    downs: u8,
    opens: u8,
    lags: u8,
    refusals: u8,
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
    /// The driver's checkpoint job puts the store's WAL into its main file
    /// and the core's state beside it, on the drive.
    CheckpointWritten,
    /// The checkpoint job reports it is done, and the core lets go of the
    /// journal below it.
    Reclaim,
    /// The checkpoint job reports it failed before writing anything.
    CheckpointFails,
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
    /// The world's doing: the daemon's process dies at a step; the machine
    /// loses power, taking the store and the journal back to what the drive
    /// has; or the drive refuses the commit of the next nudge.
    Fault(Fault),
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
                out.push(Action::Fault(Fault::PowerLoss(DAEMON)));
                out.push(Action::Reboot);
            }
            return;
        };
        // A nudge is enabled while the journal and the cursor disagree,
        // which is when one could do something.
        if journal_end(&s.journal) != s.cursor {
            out.push(Action::Nudge);
            if s.used.refusals < self.refusals {
                out.push(Action::Fault(Fault::WriteRefused(DAEMON)));
            }
        }
        if run.ingest_armed {
            out.push(Action::IngestAgain);
        }
        match &run.job {
            None if s.used.checkpoints < self.checkpoints => out.push(Action::Checkpoint),
            None => {}
            Some(CheckpointJob { written: false, .. }) => {
                out.push(Action::CheckpointWritten);
                out.push(Action::CheckpointFails);
            }
            Some(CheckpointJob { written: true, .. }) => out.push(Action::Reclaim),
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
        if s.used.downs < self.downs {
            out.push(Action::Fault(Fault::Crash(DAEMON)));
            out.push(Action::Fault(Fault::PowerLoss(DAEMON)));
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
            Action::Fault(Fault::WriteRefused(_)) => {
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
            Action::CheckpointWritten => {
                let run = next
                    .run
                    .as_mut()
                    .expect("a job runs only while the daemon does");
                run.job.as_mut().expect("a job to write").written = true;
                next.durable = next.store.clone();
                next.durable_cursor = next.cursor;
            }
            Action::Reclaim | Action::CheckpointFails => {
                let run = next
                    .run
                    .as_mut()
                    .expect("a job runs only while the daemon does");
                let job = run.job.take().expect("a job to end");
                let result = match action {
                    Action::Reclaim => JobResult::Done,
                    _ => JobResult::Failed(REFUSAL.into()),
                };
                self.step(
                    &mut next,
                    Input::JobDone {
                        job: job.id,
                        result,
                    },
                    false,
                );
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
                let request = ClientRequest::Subscribe {
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
            Action::Start => start(&mut next),
            Action::Fault(Fault::Crash(_)) => {
                next.used.downs += 1;
                next.run = None;
            }
            Action::Fault(Fault::PowerLoss(_)) => {
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
            Action::Fault(fault) => unreachable!("the single daemon never offers {fault:?}"),
        }
        Some(next)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(
                "commit before broadcast: every row a step answers is in the store once the step is done",
                |_, s: &State| s.broadcast_uncommitted.is_none(),
            ),
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
            Property::always(
                "a segment is reclaimed only below the last checkpoint's cursor: the journal still holds \
                 every frame from the cursor the drive's store has",
                |_, s: &State| {
                    s.journal
                        .first_key_value()
                        .is_none_or(|(oldest, _)| *oldest <= s.durable_cursor)
                },
            ),
            Property::always(
                "the generation bumps after an unclean reboot: a start after the machine restarted under a \
                 daemon that had not shut down cleanly serves a higher generation than the run before",
                |_, s: &State| {
                    s.run.as_ref().is_none_or(|run| {
                        !run.owed
                            || run
                                .previous
                                .is_none_or(|previous| run.generation > previous)
                    })
                },
            ),
            Property::always(
                "the generation bumps only after an unclean reboot: a start after a crash on the same boot, \
                 or after a clean shutdown, serves the generation of the run before",
                |_, s: &State| {
                    s.run
                        .as_ref()
                        .is_none_or(|run| run.owed || run.generation == run.previous.unwrap_or(1))
                },
            ),
            Property::always(
                "a checkpoint plus the inputs since reproduces the state: the core restored from the newest \
                 checkpoint and stepped through every input since, against the journal as each found it, \
                 is the live core, store and journal",
                |_, s: &State| s.diverged.is_none(),
            ),
            Property::always("the core never asks for a dump", |_, s: &State| {
                s.dumped.is_none()
            }),
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
        ]
    }
}

impl SingleDaemon {
    /// Steps the core with one input, as the driver does: recorded in the
    /// ring first, then stepped over the store and the journal, then what it
    /// asked for carried out.
    fn step(&self, next: &mut State, input: Input, refuse: bool) {
        let journal_before = next.journal.clone();
        let cursor_before = next.cursor;
        let run = next.run.as_mut().expect("a step needs a running daemon");
        let (out, held, core, store, journal) = with_scratch(&LIVE, |scratch| {
            let mut daemon = scratch.daemon(&run.core, &next.store, &next.journal);
            let mut out = Vec::new();
            stepped(&mut daemon, input.clone(), refuse, &mut out);
            // Each row a step answered, and whether the store holds it.
            let held: Vec<bool> = answered(&out)
                .map(|(_, key, revision)| {
                    daemon
                        .store()
                        .get(&agent(), &key)
                        .unwrap()
                        .is_some_and(|row| row.revision == revision)
                })
                .collect();
            let (core, store) = scratch.put_back(daemon);
            let cursor = store.cursor(&agent()).unwrap();
            let rows = rows_in(store);
            let dump = store.dump().expect("the store dumps");
            scratch.holds = dump.clone();
            (out, held, core, (dump, cursor, rows), scratch.read_back())
        });
        run.core = core;
        (next.store, next.cursor, next.rows) = store;
        next.journal = journal;
        if next.cursor < cursor_before {
            next.rewound = true;
        }
        next.ring.inputs.push((input, journal_before, refuse));

        for ((_, key, revision), held) in answered(&out).zip(held) {
            if !held && next.broadcast_uncommitted.is_none() {
                next.broadcast_uncommitted = Some(format!("{key} r{revision}"));
            }
        }
        for output in out {
            match output {
                Output::Answer {
                    call,
                    response: Response::Session(event),
                } => {
                    let Some(subscriber) = run.subscriber.as_mut().filter(|s| s.call == call)
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
                    if let Some(subscriber) = run.subscriber.as_mut().filter(|s| s.call == call) {
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
                    op: Job::Checkpoint { state },
                } => {
                    run.job = Some(CheckpointJob { id, written: false });
                    // The driver's ring starts again at this checkpoint.
                    next.ring = Ring {
                        base: Some((state, next.store.clone(), next.journal.clone())),
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
    }
}

/// Steps the core with one input, its store refusing every write when the
/// drive does.
fn stepped(daemon: &mut Daemon, input: Input, refuse: bool, out: &mut Vec<Output>) {
    if refuse {
        daemon
            .store()
            .connection()
            .execute_batch(&format!(
                "CREATE TEMP TRIGGER refuse BEFORE INSERT ON agents \
                 BEGIN SELECT RAISE(ABORT, '{REFUSAL}'); END;"
            ))
            .expect("the refusal installs");
    }
    daemon.step(input, out);
    if refuse {
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
        ingest_armed: false,
        subscriber: None,
        calls: 0,
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
            stepped(&mut daemon, input.clone(), *refuse, &mut out);
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
fn answered(out: &[Output]) -> impl Iterator<Item = (CallId, String, u64)> + '_ {
    out.iter().filter_map(|output| match output {
        Output::Answer {
            call,
            response: Response::Session(event),
        } => match &event.of {
            Some(session_event::Of::Item(item)) => Some((*call, item.key.clone(), item.revision)),
            _ => None,
        },
        _ => None,
    })
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
