//! One agent process around its interpreter: when it stays, when it ends,
//! and what a message refused on its way out becomes.
//!
//! The agent is the real `agent_core::Agent` over the test kind's
//! interpreter, held between steps as its value. What its driver does is
//! the environment: it feeds the clock before every input, runs the git
//! read at once, arms and cancels the timers the core asks for, and hands
//! the provider what the core writes to it. The provider is a small fake:
//! it records a prompt it is handed at once and runs a turn on it, takes an
//! injected message at once (starting a turn when none runs), and ends its
//! turn, opens an ask or exits when the model chooses. The daemon dials in,
//! goes away (a new connection may replace an old one before the old one
//! closes), hands a person's input and an agent message, stops or kills
//! the agent, and resumes the
//! agent with a message from its parent that it refused while exiting,
//! again until the journal records it taken, as the daemon's message lane
//! does. Time moves only when a timer the core
//! armed fires.
//!
//! An agent without a parent whose spec carries the suspension delay
//! suspends itself once it has been idle that long. For it the world also
//! attaches and detaches terminal clients, and its provider runs background
//! jobs in a turn (each one's end reported in a turn the provider then
//! starts itself), schedules a wake-up in a turn (which later fires as a
//! turn of its own), and is granted a permission for its session when the
//! daemon answers an ask. The daemon resumes a suspended agent with a
//! message: one it refused as it suspended, or a new one.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::hash::{Hash, Hasher};

use agent_core::{
    Agent, AgentValue, ConnId, ExitCause, Input, JobResult, Output, Resumed, TimerId,
};
use interpret::ring::Entry;
use interpret::testkind::TestKind;
use interpret::{Channel, Effect, Fact, FixtureInput, Interpreter};
use serde_json::json;
use wire::{AgentRef, AgentSpec, Bootstrap, CtlFrame, Envelope, ctl_frame};

use crate::{Machine, Property};

type Kind = TestKind;

const START_MS: u64 = 1_000;
const GRACE_MS: u64 = 60_000;
const DRAIN_MS: u64 = 30_000;
/// The suspension delay, apart from the other timers' so a property can
/// tell them apart.
const SUSPEND_AFTER_MS: u64 = 90_000;
/// The one-shot child's spawn prompt.
const SPAWN: &[u8] = b"spawn";

/// One agent's lifecycle, within its bounds.
#[derive(Clone, Debug)]
pub struct AgentLifecycle {
    /// The agent has a parent: it exits when its work is done, and its
    /// parent's message refused while it exits resumes it.
    pub one_shot: bool,
    /// Connections from a daemon, all told; each may close once.
    pub dials: u8,
    /// A person's inputs the daemon hands, all told.
    pub inputs: u8,
    /// Agent messages the daemon hands, all told.
    pub messages: u8,
    /// Asks the provider opens, all told.
    pub asks: u8,
    /// Times the provider exits without being asked to.
    pub provider_exits: u8,
    /// Stops and kills the daemon asks for, all told.
    pub stops: u8,
    /// The spec carries the suspension delay: an agent without a parent
    /// suspends once idle that long.
    pub suspends: bool,
    /// Terminal clients that attach, all told; each may leave.
    pub attaches: u8,
    /// Background jobs the provider starts in a turn, all told.
    pub jobs: u8,
    /// Wake-ups the provider schedules in a turn, all told.
    pub wakeups: u8,
    /// Asks the daemon answers with a grant for the session, all told.
    pub grants: u8,
}

impl AgentLifecycle {
    /// The one-shot child at the bounds `just test` explores exhaustively:
    /// 2,106,869 states, in about 5 s on an M-series Mac's cores.
    pub const fn ci() -> Self {
        Self {
            one_shot: true,
            dials: 3,
            inputs: 3,
            messages: 2,
            asks: 2,
            provider_exits: 1,
            stops: 2,
            suspends: false,
            attaches: 0,
            jobs: 0,
            wakeups: 0,
            grants: 0,
        }
    }

    /// An agent without a parent that suspends, with every input to its
    /// idleness once: a terminal client, a background job, a wake-up and
    /// a grant for the session, at bounds that keep `just test` quick:
    /// 1,819,692 states, in about 5 s on an M-series Mac's cores.
    pub const fn suspending() -> Self {
        Self {
            one_shot: false,
            dials: 2,
            inputs: 1,
            messages: 2,
            asks: 1,
            provider_exits: 1,
            stops: 1,
            suspends: true,
            attaches: 1,
            jobs: 1,
            wakeups: 1,
            grants: 1,
        }
    }

    /// An agent with no parent at the same bounds: it stays after its
    /// turns, and leaves only when it is stopped or killed, its daemon is
    /// lost or its provider exits. 148,634 states in about a second.
    pub const fn parentless() -> Self {
        Self {
            one_shot: false,
            ..Self::ci()
        }
    }

    /// The larger bounds a nightly run explores.
    pub const fn nightly(one_shot: bool) -> Self {
        Self {
            one_shot,
            dials: 4,
            inputs: 3,
            messages: 3,
            asks: 2,
            provider_exits: 2,
            stops: 2,
            suspends: false,
            attaches: 0,
            jobs: 0,
            wakeups: 0,
            grants: 0,
        }
    }

    /// The suspending agent at the larger bounds a nightly run explores:
    /// 32,945,447 states in about four minutes on an M-series Mac's cores.
    pub const fn nightly_suspending() -> Self {
        Self {
            dials: 3,
            stops: 2,
            jobs: 2,
            ..Self::suspending()
        }
    }

    fn spec(&self, incarnation: u32, bootstrap: Option<Bootstrap>) -> AgentSpec {
        use prost::Message as _;
        let mut experimental = std::collections::BTreeMap::new();
        if self.suspends && !self.one_shot {
            experimental.insert(
                agent_core::SUSPENSION.to_owned(),
                wire::experimental::SuspensionSpec {
                    after_ms: SUSPEND_AFTER_MS,
                }
                .encode_to_vec(),
            );
        }
        AgentSpec {
            experimental,
            agent_id: b"agent".to_vec(),
            incarnation,
            parent: self.one_shot.then(AgentRef::default),
            config: Some(wire::EffectiveConfig {
                grace_ms: GRACE_MS as u32,
                drain_ms: DRAIN_MS as u32,
                // The checker's verdict is a function of the model alone.
                step_budget_micros: agent_core::StepBudget::UNLIMITED.micros,
                ..Default::default()
            }),
            bootstrap,
            ..Default::default()
        }
    }

    /// Whether nothing more can happen: the world has done all it may.
    fn at_rest(&self, state: &State) -> bool {
        let mut actions = Vec::new();
        self.actions(state, &mut actions);
        actions.is_empty()
    }
}

/// What the model does or lets happen.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// A daemon connects to the control socket, replacing any connection
    /// still open.
    DaemonDials,
    /// A daemon's connection closes.
    DaemonGone(ConnId),
    /// The provider ends the turn it runs.
    TurnEnd,
    /// The daemon hands a person's prompt.
    Input,
    /// The daemon hands an agent message: from the parent, for a one-shot
    /// child.
    AgentMessage,
    /// The provider opens an ask in the turn it runs.
    Ask,
    /// The daemon stops the agent; with `kill`, it ends the process group
    /// at once.
    Stop { kill: bool },
    /// The grace timer fires.
    GraceExpires,
    /// The drain timer fires.
    DrainExpires,
    /// The provider's process exits: asked to finish, or on its own.
    Exit,
    /// The daemon resumes the exited agent with the message it refused, or
    /// a suspended one with a new message.
    Resume,
    /// A terminal client attaches to the agent's terminal.
    Attach,
    /// A terminal client leaves.
    Leave,
    /// The provider starts background job `n` in the turn it runs.
    JobStart(u8),
    /// Background job `n` ends; between turns, the provider owes a turn
    /// reporting it.
    JobEnd(u8),
    /// The provider starts the turn it owes for a job's end.
    ProviderTurn,
    /// The provider schedules wake-up `n` in the turn it runs.
    Schedule(u8),
    /// Wake-up `n` fires between turns, starting a turn of its own.
    WakeupFires(u8),
    /// The daemon answers the open ask with a grant for the session.
    Grant,
    /// The suspension timer fires.
    SuspendExpires,
}

/// The provider, as the world sees it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Provider {
    alive: bool,
    /// A turn runs.
    turn: bool,
    /// An ask is open in the turn.
    asking: bool,
    /// The agent asked it to finish.
    asked_to_close: bool,
    /// The daemon stopped the agent, which cancels the turn the
    /// provider's way: the turn may end with its ask still open.
    cancelled: bool,
    /// The background jobs it runs, by number.
    jobs: BTreeSet<u8>,
    /// A job ended between turns: it starts a turn reporting it.
    owes_turn: bool,
    /// The wake-ups it has scheduled and not run, by number.
    wakeups: BTreeSet<u8>,
    /// It holds a grant for its session.
    granted: bool,
}

impl Provider {
    const STARTED: Provider = Provider {
        alive: true,
        turn: false,
        asking: false,
        asked_to_close: false,
        cancelled: false,
        jobs: BTreeSet::new(),
        owes_turn: false,
        wakeups: BTreeSet::new(),
        granted: false,
    };

    /// Nothing runs, nothing is asked, and it holds nothing that ending it
    /// would lose or cut short.
    fn quiet(&self) -> bool {
        self.alive
            && !self.turn
            && !self.asking
            && self.jobs.is_empty()
            && !self.owes_turn
            && self.wakeups.is_empty()
            && !self.granted
    }
}

/// The daemon, as the world sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Daemon {
    /// Connections open to this process.
    open: BTreeSet<ConnId>,
    /// The connection the daemon writes on.
    newest: Option<ConnId>,
    dialled: u8,
    inputs: u8,
    messages: u8,
    /// The parent's message the agent refused as exiting, which the daemon
    /// resumes it with once the process has exited, and again after each
    /// incarnation that exits before its journal records the message taken.
    held: Option<Vec<u8>>,
}

/// The lifecycle's decision to end the incarnation, and what led to it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Decided {
    cause: ExitCause,
    by: Action,
    /// The provider ran a turn when the action happened.
    turn_running: bool,
    /// How long the world had been quiet, as the model sees it, when the
    /// action happened; none when it was not.
    quiet_for: Option<u64>,
}

/// What the properties judge, as it happened.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Seen {
    /// Inputs and messages this incarnation accepted, by id.
    accepted: BTreeSet<Vec<u8>>,
    /// Ids of inputs and messages the journal recorded as taken, in any
    /// incarnation.
    taken: BTreeSet<Vec<u8>>,
    /// Inputs and messages any incarnation accepted, by id.
    ever_accepted: BTreeSet<Vec<u8>>,
    /// The queue the journal's newest snapshot states, by input id.
    queued: BTreeSet<Vec<u8>>,
    /// Parent's messages refused because the agent was exiting, in any
    /// incarnation.
    refused_exiting: BTreeSet<Vec<u8>>,
    /// The incarnation end the journal's newest step records, if it
    /// records one: its cause and detail.
    last_end: Option<(i32, Option<String>)>,
    decided: Option<Decided>,
    /// Why this incarnation's process exited.
    exited: Option<ExitCause>,
    asks: u8,
    provider_exits: u8,
    stops: u8,
    /// An incarnation after the first started.
    resumed: bool,
    /// Since when the world has been quiet without a break, as the model
    /// sees it: the provider alive, running and holding nothing, nobody at
    /// the terminal, and every input and message accepted taken.
    quiet_since: Option<u64>,
    /// Messages the daemon resumed a suspended agent with that it had not
    /// refused, in any incarnation.
    woke_with: BTreeSet<Vec<u8>>,
    /// Incarnations that suspended.
    suspensions: u8,
    /// A suspended incarnation was resumed by a message.
    resumed_from_suspension: bool,
    /// The provider was interrupted while its agent was exiting suspended.
    interrupted_suspending: bool,
    attaches: u8,
    jobs: u8,
    wakeups: u8,
    grants: u8,
    /// A resume found the facts ring replaying to another interpreter than
    /// the one that recorded it.
    ring_diverged: bool,
    dumped: Option<String>,
}

/// The facts ring since the incarnation started: the interpreter's
/// checkpoint at its start and every event recorded after it, which is what
/// a resume replays. Left out of a state's identity: the property that the
/// ring replays to the live interpreter makes it a function of the rest.
#[derive(Clone, Debug, Default)]
struct Ring {
    checkpoint: Vec<u8>,
    entries: Vec<Entry>,
}

impl PartialEq for Ring {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Ring {}

impl Hash for Ring {
    fn hash<H: Hasher>(&self, _: &mut H) {}
}

impl Ring {
    /// The interpreter the ring replays to.
    fn replayed(&self) -> <Kind as Interpreter>::State {
        let mut state = interpret::decode_checkpoint(&self.checkpoint).expect("it decodes");
        for event in self.entries.iter().cloned().filter_map(Entry::event) {
            let _ = Kind::step(&mut state, event);
        }
        state
    }
}

/// The agent, its timers, its provider and its daemon.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    agent: AgentValue,
    ring: Ring,
    now_ms: u64,
    timers: BTreeMap<TimerId, u64>,
    provider: Provider,
    daemon: Daemon,
    /// Terminal clients attached to this incarnation's terminal.
    attached: u8,
    seen: Seen,
}

impl State {
    fn exited(&self) -> bool {
        self.seen.exited.is_some()
    }

    /// The incarnation's journal records it ending suspended.
    fn suspending(&self) -> bool {
        self.seen
            .last_end
            .as_ref()
            .is_some_and(|(cause, _)| *cause == wire::ExitCause::Suspended as i32)
    }

    /// The world is quiet, as the model sees it, independent of the core.
    fn quiet(&self) -> bool {
        !self.exited()
            && self.provider.quiet()
            && self.attached == 0
            && self.seen.queued.is_empty()
            && self.seen.accepted.is_subset(&self.seen.taken)
    }

    fn connected(&self) -> Option<ConnId> {
        self.daemon
            .newest
            .filter(|newest| self.daemon.open.contains(newest))
    }
}

/// The lifecycle's promises the composed net judges too, each with the
/// predicate both judge it by.
pub(crate) const EXITS_AT_TURN_END: &str = "an agent exits only at a turn end, a drain deadline or its \
     suspension deadline: the lifecycle decides to end an incarnation only as a turn ends, while no turn \
     runs, or as its drain deadline passes, and suspends it only as its suspension deadline passes with \
     no turn running";
pub(crate) const EXIT_CAUSE_LAST_STEP: &str = "the exit cause is the journal's last step: an incarnation \
     that exits having written its end records its cause on its newest step";

/// The agent lifecycle's promise a replay of a dump judges too.
pub const RESUME_CONTINUES: &str = "a resume continues where the last incarnation was: the facts ring \
     replays to the interpreter that recorded it";

/// Whether a decision to end an incarnation for `cause` came in its time:
/// as a turn ended or the drain deadline passed (`at_turn_end_or_deadline`),
/// or while no turn ran, unless it was not the lifecycle's to make.
/// A suspension, decided only by the lifecycle, comes only as its own
/// deadline passes, with no turn running.
pub(crate) fn decided_in_its_time(
    cause: &ExitCause,
    at_turn_end_or_deadline: bool,
    at_suspension_deadline: bool,
    turn_running: bool,
) -> bool {
    match cause {
        ExitCause::Suspended => at_suspension_deadline && !turn_running,
        cause => !cause.decided_by_the_lifecycle() || at_turn_end_or_deadline || !turn_running,
    }
}

/// Whether an incarnation that exited for `cause` recorded it on its
/// journal's newest step, whose incarnation end, if any, is `last_end`: its
/// cause and detail. A kill or a failed write leaves no end to record.
pub(crate) fn cause_on_last_step(
    cause: &ExitCause,
    last_end: Option<&(i32, Option<String>)>,
) -> bool {
    match cause {
        ExitCause::Killed | ExitCause::WriteFailed(_) => true,
        cause => {
            let end = cause.end();
            last_end == Some(&(end.cause, end.detail))
        }
    }
}

fn fact(value: serde_json::Value) -> Input {
    Input::Provider(Fact {
        channel: Channel::Stream,
        payload: serde_json::to_vec(&value).expect("json"),
    })
}

fn message(id: &[u8]) -> Envelope {
    Envelope {
        id: id.to_vec(),
        body: Some(wire::envelope::Body::Message(wire::MessageBody {
            text: format!("message {}", String::from_utf8_lossy(id)),
            ..Default::default()
        })),
        ..Default::default()
    }
}

impl AgentLifecycle {
    /// An incarnation the driver has just started, with what its start
    /// asked for: the facts ring begins at its checkpoint, and its provider
    /// says it is ready.
    fn started(&self, state: &mut State, mut agent: Agent<Kind>, out: Vec<Output>) {
        state.ring = Ring {
            checkpoint: agent.checkpoint(),
            entries: Vec::new(),
        };
        let mut fed = VecDeque::new();
        for output in out {
            self.observe(state, output, &mut fed);
        }
        fed.push_back(fact(json!({"type": "ready"})));
        self.drive(state, &mut agent, fed);
        state.agent = agent.value();
    }

    /// Feeds each input with the clock before it, and what the driver and
    /// the provider answer, until nothing more follows.
    fn drive(&self, state: &mut State, agent: &mut Agent<Kind>, mut fed: VecDeque<Input>) {
        while let Some(input) = fed.pop_front() {
            if agent.exited().is_some() {
                break;
            }
            let mut out = Vec::new();
            agent.step(
                Input::Tick {
                    now_ms: state.now_ms,
                },
                &mut out,
            );
            agent.step(input, &mut out);
            for output in out {
                self.observe(state, output, &mut fed);
            }
        }
    }

    /// What the driver, the provider and the daemon make of one output.
    fn observe(&self, state: &mut State, output: Output, fed: &mut VecDeque<Input>) {
        match output {
            Output::Record(entry) => state.ring.entries.push(entry),
            Output::WriteBlob { .. } => {}
            Output::AppendJournal(step) => {
                state.seen.taken.extend(
                    step.items
                        .iter()
                        .filter(|item| !item.input_id.is_empty())
                        .map(|item| item.input_id.clone()),
                );
                state.seen.last_end = step.incarnation_end.map(|end| (end.cause, end.detail));
                if let Some(snapshot) = &step.snapshot {
                    state.seen.queued = snapshot
                        .queue
                        .iter()
                        .map(|queued| queued.input_id.clone())
                        .collect();
                }
                // The lane holds a message until the recipient's journal
                // records it taken.
                if state
                    .daemon
                    .held
                    .as_ref()
                    .is_some_and(|held| state.seen.taken.contains(held))
                {
                    state.daemon.held = None;
                }
            }
            Output::WriteProvider(effect) => {
                if state.provider.alive {
                    if state.suspending()
                        && matches!(&effect, Effect::ProviderWrite(bytes)
                            if bytes.windows(9).any(|window| window == b"interrupt"))
                    {
                        state.seen.interrupted_suspending = true;
                    }
                    self.provider_takes(state, effect, fed);
                }
            }
            Output::WriteControl { connection, frame } => {
                if !state.daemon.open.contains(&connection) {
                    return;
                }
                if let Some(ctl_frame::Of::Reply(verdict)) = frame.of {
                    match verdict.verdict.and_then(|verdict| verdict.of) {
                        // An answer is no input the provider takes; it closes
                        // its ask.
                        Some(wire::send_input_response::Of::Accepted(_))
                            if !verdict.id.starts_with(b"g") =>
                        {
                            state.seen.ever_accepted.insert(verdict.id.clone());
                            state.seen.accepted.insert(verdict.id);
                        }
                        Some(wire::send_input_response::Of::Rejected(rejected))
                            if rejected.refusal() == wire::Refusal::Exiting
                                && (self.one_shot || state.suspending())
                                && verdict.id.starts_with(b"m") =>
                        {
                            state.seen.refused_exiting.insert(verdict.id.clone());
                            state.daemon.held = Some(verdict.id);
                        }
                        _ => {}
                    }
                }
            }
            Output::ArmTimer { id, at_ms } => {
                state.timers.insert(id, at_ms);
            }
            Output::CancelTimer { id } => {
                state.timers.remove(&id);
            }
            Output::Job { id, op } => match op {
                agent_core::AgentJob::ReadGit => fed.push_back(Input::JobDone {
                    job: id,
                    result: JobResult::Git(None),
                }),
                agent_core::AgentJob::DumpPart { .. } => {}
            },
            Output::CloseProvider => state.provider.asked_to_close = true,
            Output::KillProvider => {
                state.provider.alive = false;
                state.provider.turn = false;
                state.provider.asking = false;
            }
            Output::Exit(cause) => {
                // The process is gone, and its sockets and timers with it.
                state.seen.exited = Some(cause);
                state.daemon.open.clear();
                state.daemon.newest = None;
                state.timers.clear();
            }
            Output::Dump { reason } => {
                state.seen.dumped.get_or_insert(format!("{reason:?}"));
            }
        }
    }

    /// The provider records a prompt it is handed and runs a turn on it,
    /// and takes an injected message at once, starting a turn when none
    /// runs.
    fn provider_takes(&self, state: &mut State, effect: Effect, fed: &mut VecDeque<Input>) {
        match effect {
            Effect::ProviderWrite(bytes) => {
                let Ok(written) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                    return;
                };
                // An answer to the open ask, which the provider closes.
                if written["type"] == "answer" {
                    if state.provider.asking {
                        state.provider.asking = false;
                        state.provider.granted = true;
                        fed.push_back(fact(json!({"type": "ask_closed", "id": written["ask"]})));
                    }
                    return;
                }
                if written["type"] != "user" {
                    return;
                }
                let text = written["text"].as_str().unwrap_or_default().to_owned();
                fed.push_back(fact(
                    json!({"type": "user", "id": format!("user:{text}"), "text": text}),
                ));
                state.provider.turn = true;
                // Any turn reports a job that ended before it.
                state.provider.owes_turn = false;
            }
            Effect::Inject { id, .. } => {
                let envelope = String::from_utf8_lossy(&id).into_owned();
                fed.push_back(fact(json!({"type": "user", "envelope": envelope})));
                if !state.provider.turn {
                    fed.push_back(fact(json!({"type": "turn_start"})));
                    state.provider.turn = true;
                    state.provider.owes_turn = false;
                }
            }
            _ => {}
        }
    }
}

impl Machine for AgentLifecycle {
    type State = State;
    type Action = Action;

    fn init(&self) -> State {
        let bootstrap = self.one_shot.then(|| Bootstrap {
            of: Some(wire::bootstrap::Of::Prompt(wire::PromptSubmission {
                input_id: SPAWN.to_vec(),
                prompt: Some(wire::PromptInput {
                    text: "spawn".to_owned(),
                    ..Default::default()
                }),
            })),
        });
        let mut out = Vec::new();
        let agent = Agent::<Kind>::start(&self.spec(1, bootstrap), None, START_MS, &mut out);
        let mut state = State {
            agent: agent.value(),
            ring: Ring::default(),
            now_ms: START_MS,
            timers: BTreeMap::new(),
            provider: Provider::STARTED,
            daemon: Daemon::default(),
            attached: 0,
            seen: Seen::default(),
        };
        self.started(&mut state, agent, out);
        state.seen.quiet_since = (self.suspends && state.quiet()).then_some(state.now_ms);
        state
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        if s.exited() {
            let woken =
                s.seen.exited == Some(ExitCause::Suspended) && s.daemon.messages < self.messages;
            if s.daemon.held.is_some() || woken {
                out.push(Action::Resume);
            }
            return;
        }
        if s.seen.attaches < self.attaches {
            out.push(Action::Attach);
        }
        if s.attached > 0 {
            out.push(Action::Leave);
        }
        if s.timers.contains_key(&TimerId::Suspend) {
            out.push(Action::SuspendExpires);
        }
        let provider = &s.provider;
        if provider.alive {
            if provider.turn && s.seen.jobs < self.jobs {
                out.push(Action::JobStart(s.seen.jobs + 1));
            }
            out.extend(provider.jobs.iter().copied().map(Action::JobEnd));
            if !provider.turn && provider.owes_turn {
                out.push(Action::ProviderTurn);
            }
            if provider.turn && s.seen.wakeups < self.wakeups {
                out.push(Action::Schedule(s.seen.wakeups + 1));
            }
            if !provider.turn {
                out.extend(provider.wakeups.iter().copied().map(Action::WakeupFires));
            }
            if provider.asking && s.seen.grants < self.grants && s.connected().is_some() {
                out.push(Action::Grant);
            }
        }
        if s.daemon.dialled < self.dials {
            out.push(Action::DaemonDials);
        }
        out.extend(s.daemon.open.iter().copied().map(Action::DaemonGone));
        if s.connected().is_some() {
            if s.daemon.inputs < self.inputs {
                out.push(Action::Input);
            }
            if s.daemon.messages < self.messages && s.daemon.held.is_none() {
                out.push(Action::AgentMessage);
            }
            if s.seen.stops < self.stops {
                out.push(Action::Stop { kill: false });
                out.push(Action::Stop { kill: true });
            }
        }
        let provider = &s.provider;
        if provider.alive && provider.turn {
            if !provider.asking || provider.cancelled {
                out.push(Action::TurnEnd);
            }
            if !provider.asking && s.seen.asks < self.asks {
                out.push(Action::Ask);
            }
        }
        if s.timers.contains_key(&TimerId::Grace) {
            out.push(Action::GraceExpires);
        }
        if s.timers.contains_key(&TimerId::Drain) {
            out.push(Action::DrainExpires);
        }
        if provider.alive
            && (provider.asked_to_close || s.seen.provider_exits < self.provider_exits)
        {
            out.push(Action::Exit);
        }
    }

    fn next(&self, s: &State, action: &Action) -> Option<State> {
        let mut state = self.step(s, action)?;
        // Quiet is unbroken only while it holds after every action.
        state.seen.quiet_since = match (self.suspends && state.quiet(), s.seen.quiet_since) {
            (false, _) => None,
            (true, Some(since)) if *action != Action::Resume => Some(since),
            (true, _) => Some(state.now_ms),
        };
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        self.promises()
    }
}

impl AgentLifecycle {
    fn step(&self, s: &State, action: &Action) -> Option<State> {
        let mut state = s.clone();
        if *action == Action::Resume {
            // The message it refused as it exited, or, for a suspended
            // agent, a new one, which the daemon starts it with.
            let held = match state.daemon.held.clone() {
                Some(held) => held,
                None => {
                    state.daemon.messages += 1;
                    let id = format!("m{}", state.daemon.messages).into_bytes();
                    state.seen.woke_with.insert(id.clone());
                    id
                }
            };
            if s.seen.exited == Some(ExitCause::Suspended) {
                state.seen.resumed_from_suspension = true;
            }
            let agent = Agent::<Kind>::from_value(&s.agent).expect("the value restores");
            if interpret::encode_checkpoint(&s.ring.replayed()) != agent.checkpoint() {
                state.seen.ring_diverged = true;
            }
            let resumed = Resumed {
                state: interpret::decode_checkpoint(&s.ring.checkpoint).expect("it decodes"),
                inputs: s.ring.entries.clone(),
            };
            let spec = self.spec(
                agent.incarnation() + 1,
                Some(Bootstrap {
                    of: Some(wire::bootstrap::Of::AgentMessage(message(&held))),
                }),
            );
            state.provider = Provider::STARTED;
            state.attached = 0;
            state.seen.accepted.clear();
            state.seen.decided = None;
            state.seen.exited = None;
            state.seen.resumed = true;
            let mut out = Vec::new();
            let agent = Agent::<Kind>::start(&spec, Some(resumed), state.now_ms, &mut out);
            self.started(&mut state, agent, out);
            return Some(state);
        }

        let mut agent = Agent::<Kind>::from_value(&s.agent).expect("the value restores");
        let deciding = agent.exiting().is_none();
        let turn_running = s.provider.turn;
        let fed = match action {
            Action::Attach | Action::Leave => {
                if *action == Action::Attach {
                    state.seen.attaches += 1;
                    state.attached += 1;
                } else {
                    state.attached -= 1;
                }
                VecDeque::from([Input::Attached {
                    clients: u32::from(state.attached),
                }])
            }
            Action::JobStart(n) => {
                state.seen.jobs += 1;
                state.provider.jobs.insert(*n);
                VecDeque::from([fact(json!({"type": "job_started", "id": format!("j{n}")}))])
            }
            Action::JobEnd(n) => {
                state.provider.jobs.remove(n);
                state.provider.owes_turn |= !state.provider.turn;
                VecDeque::from([fact(
                    json!({"type": "job_ended", "id": format!("j{n}"), "reported": true}),
                )])
            }
            Action::ProviderTurn => {
                state.provider.owes_turn = false;
                state.provider.turn = true;
                VecDeque::from([fact(json!({"type": "turn_start"}))])
            }
            Action::Schedule(n) => {
                state.seen.wakeups += 1;
                state.provider.wakeups.insert(*n);
                VecDeque::from([fact(json!({"type": "wakeup", "id": format!("w{n}")}))])
            }
            Action::WakeupFires(n) => {
                state.provider.wakeups.remove(n);
                state.provider.turn = true;
                state.provider.owes_turn = false;
                VecDeque::from([fact(json!({"type": "wakeup_fired", "id": format!("w{n}")}))])
            }
            Action::Grant => {
                state.seen.grants += 1;
                let answer = wire::Input::new(
                    format!("g{}", state.seen.grants).into_bytes(),
                    wire::ClaudeSdkInput {
                        of: Some(wire::claude_sdk_input::Of::Answer(wire::ClaudeAnswer {
                            ask_key: format!("a{}", state.seen.asks),
                            of: Some(wire::claude_answer::Of::Permission(
                                wire::PermissionAnswer {
                                    of: Some(wire::permission_answer::Of::Allow(
                                        wire::PermissionAllow { scope: Some(0) },
                                    )),
                                },
                            )),
                        })),
                    },
                );
                VecDeque::from([Input::Control {
                    connection: state.connected()?,
                    frame: CtlFrame {
                        of: Some(ctl_frame::Of::Input(answer)),
                    },
                }])
            }
            Action::SuspendExpires => {
                let at_ms = state.timers.remove(&TimerId::Suspend)?;
                state.now_ms = state.now_ms.max(at_ms);
                VecDeque::from([Input::Timer(TimerId::Suspend)])
            }
            Action::DaemonDials => {
                state.daemon.dialled += 1;
                let connection = ConnId(u64::from(state.daemon.dialled));
                state.daemon.open.insert(connection);
                state.daemon.newest = Some(connection);
                VecDeque::from([Input::DaemonDials { connection }])
            }
            Action::DaemonGone(connection) => {
                state.daemon.open.remove(connection);
                VecDeque::from([Input::DaemonGone {
                    connection: *connection,
                }])
            }
            Action::TurnEnd => {
                state.provider.turn = false;
                let mut fed = VecDeque::new();
                // A cancelled turn ends with its ask.
                if std::mem::take(&mut state.provider.asking) {
                    let ask = format!("a{}", state.seen.asks);
                    fed.push_back(fact(json!({"type": "ask_closed", "id": ask})));
                }
                fed.push_back(fact(json!({"type": "turn_end"})));
                fed
            }
            Action::Input => {
                state.daemon.inputs += 1;
                let id = format!("i{}", state.daemon.inputs);
                let input = interpret::claude_sdk_input(
                    id.clone().into_bytes(),
                    &FixtureInput::Prompt {
                        text: format!("input {id}"),
                    },
                )
                .expect("the test kind takes prompts");
                VecDeque::from([Input::Control {
                    connection: state.connected()?,
                    frame: CtlFrame {
                        of: Some(ctl_frame::Of::Input(input)),
                    },
                }])
            }
            Action::AgentMessage => {
                state.daemon.messages += 1;
                let id = format!("m{}", state.daemon.messages);
                VecDeque::from([Input::Control {
                    connection: state.connected()?,
                    frame: CtlFrame {
                        of: Some(ctl_frame::Of::AgentMessage(message(id.as_bytes()))),
                    },
                }])
            }
            Action::Ask => {
                state.seen.asks += 1;
                state.provider.asking = true;
                VecDeque::from([fact(json!({
                    "type": "ask",
                    "id": format!("a{}", state.seen.asks),
                    "tool": "t",
                    "tool_name": "Bash",
                }))])
            }
            Action::Stop { kill } => {
                state.seen.stops += 1;
                state.provider.cancelled = true;
                VecDeque::from([Input::Control {
                    connection: state.connected()?,
                    frame: CtlFrame {
                        of: Some(ctl_frame::Of::Stop(wire::Stop { kill: *kill })),
                    },
                }])
            }
            Action::GraceExpires | Action::DrainExpires => {
                let id = if *action == Action::GraceExpires {
                    TimerId::Grace
                } else {
                    TimerId::Drain
                };
                let at_ms = state.timers.remove(&id)?;
                state.now_ms = state.now_ms.max(at_ms);
                VecDeque::from([Input::Timer(id)])
            }
            Action::Exit => {
                if !state.provider.asked_to_close {
                    state.seen.provider_exits += 1;
                }
                state.provider = Provider {
                    alive: false,
                    ..Provider::STARTED
                };
                state.attached = 0;
                VecDeque::from([Input::ProviderExited { code: Some(0) }])
            }
            Action::Resume => unreachable!("taken above"),
        };
        self.drive(&mut state, &mut agent, fed);
        if deciding && let Some(cause) = agent.exiting() {
            if *cause == ExitCause::Suspended {
                state.seen.suspensions += 1;
            }
            state.seen.decided = Some(Decided {
                cause: cause.clone(),
                by: action.clone(),
                turn_running,
                quiet_for: s
                    .seen
                    .quiet_since
                    .map(|since| state.now_ms.saturating_sub(since)),
            });
        }
        state.agent = agent.value();
        Some(state)
    }

    fn promises(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("the agent core never asks for a dump", |_, s: &State| {
                s.seen.dumped.is_none()
            }),
            Property::always(RESUME_CONTINUES, |_, s: &State| !s.seen.ring_diverged),
            Property::always(EXITS_AT_TURN_END, |_, s: &State| {
                s.seen.decided.as_ref().is_none_or(|decided| {
                    decided_in_its_time(
                        &decided.cause,
                        matches!(decided.by, Action::TurnEnd | Action::DrainExpires),
                        decided.by == Action::SuspendExpires,
                        decided.turn_running,
                    )
                })
            }),
            Property::always(
                "a one-shot child runs a queued follow-up before exiting: when it decides it has \
                 finished, every input and message it accepted has been taken",
                |_, s: &State| {
                    s.seen.decided.as_ref().is_none_or(|decided| {
                        decided.cause != ExitCause::Finished
                            || s.seen.accepted.is_subset(&s.seen.taken)
                    })
                },
            ),
            Property::always(EXIT_CAUSE_LAST_STEP, |_, s: &State| {
                s.seen
                    .exited
                    .as_ref()
                    .is_none_or(|cause| cause_on_last_step(cause, s.seen.last_end.as_ref()))
            }),
            Property::eventually(
                "a message after the exit decision resumes: once the world rests, every message \
                 the agent refused as exiting (its parent's, or any as it suspended) has been taken \
                 by a later incarnation",
                |model, s: &State| {
                    model.at_rest(s) && s.seen.refused_exiting.is_subset(&s.seen.taken)
                },
            ),
            Property::always(
                "an agent suspends only after unbroken quiet: when it decides to suspend, the world \
                 has had no turn, nothing queued or pending, no background job, no turn owed, no \
                 wake-up, no grant for the session and no terminal attached for the whole delay",
                |_, s: &State| {
                    s.seen.decided.as_ref().is_none_or(|decided| {
                        decided.cause != ExitCause::Suspended
                            || decided
                                .quiet_for
                                .is_some_and(|quiet| quiet >= SUSPEND_AFTER_MS)
                    })
                },
            ),
            Property::always(
                "a suspension never ends a turn and never interrupts the provider",
                |_, s: &State| {
                    !s.seen.interrupted_suspending
                        && s.seen.decided.as_ref().is_none_or(|decided| {
                            decided.cause != ExitCause::Suspended || !decided.turn_running
                        })
                },
            ),
            Property::always(
                "only an agent without a parent whose spec carries the delay suspends",
                |model, s: &State| s.seen.suspensions == 0 || (model.suspends && !model.one_shot),
            ),
            Property::eventually(
                "a message that resumes a suspended agent is taken: once the world rests, every \
                 message the daemon resumed it with has been taken",
                |model, s: &State| model.at_rest(s) && s.seen.woke_with.is_subset(&s.seen.taken),
            ),
            Property::eventually(
                "an accepted input is never lost: once the world rests, every input and message an \
                 incarnation accepted has been taken, or waits in the queue for the next",
                |model, s: &State| {
                    model.at_rest(s)
                        && s.seen
                            .ever_accepted
                            .iter()
                            .all(|id| s.seen.taken.contains(id) || s.seen.queued.contains(id))
                },
            ),
            Property::sometimes("an agent sometimes exits", |_, s: &State| s.exited()),
            Property::sometimes(
                "an agent sometimes exits orphaned, its ask unanswered past the drain deadline",
                |_, s: &State| s.seen.exited == Some(ExitCause::Orphaned),
            ),
            Property::sometimes(
                "an agent sometimes exits stopped, once the turn the stop cut short has ended",
                |_, s: &State| s.seen.exited == Some(ExitCause::Stopped),
            ),
            Property::sometimes(
                "a one-shot child sometimes resumes with a message it refused as exiting",
                |model, s: &State| !model.one_shot || s.seen.resumed,
            ),
            Property::sometimes(
                "an agent that suspends sometimes does",
                |model, s: &State| !model.suspends || s.seen.suspensions > 0,
            ),
            Property::sometimes(
                "a suspended agent is sometimes resumed by a message and takes it",
                |model, s: &State| {
                    !model.suspends
                        || (s.seen.resumed_from_suspension
                            && !s.seen.woke_with.is_empty()
                            && s.seen.woke_with.is_subset(&s.seen.taken))
                },
            ),
            Property::sometimes(
                "a suspending agent sometimes takes a message it refused as it suspended",
                |model, s: &State| {
                    !model.suspends
                        || s.seen
                            .refused_exiting
                            .iter()
                            .any(|id| s.seen.taken.contains(id))
                },
            ),
        ]
    }
}
