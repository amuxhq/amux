//! What a net is made of, and the drivers between its cores, as a value.
//!
//! Every host is a real `daemon_core::Daemon` over a real in-memory store,
//! held as its checkpoint and its store's dump, and every agent process a
//! real `agent_core::Agent` over the test kind's interpreter, held as its
//! value; an agent's journal is bytes on its host. A step restores the one
//! core it feeds over this thread's store and directories, steps it, and
//! takes it back, so a net clones, hashes and compares like any model's
//! state.
//!
//! Everything the drivers do between the cores is a delivery waiting in a
//! lane: a peer's call on its way, the answers coming back, a timer that
//! fell due, a job's result, a dial reaching an agent's process, a control
//! frame, what a provider says. Each lane delivers in order, as one stream
//! or socket does; lanes interleave freely, as streams and sockets do.
//! Delivering one lane's first delivery is the one thing that moves the net
//! on its own; which lane goes next is the explorer's choice, a checker's
//! or a seed's. A call's message may be lost on its way, which breaks the
//! call as a reset stream does.
//!
//! Time is the net's clock and moves only when it is advanced: a timer
//! armed for now is a delivery at once, one armed for later fires when the
//! clock passes it. Jobs are carried out by fakes: nothing outside the
//! agents' journals is written, a kill ends the process at once, git reads
//! find no repository. After every step that wrote a host's store, the
//! host's core checks every invariant it knows (`check_all`); a broken one,
//! or a core asking for a dump, fails the net.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt::{self, Debug};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agent_core::{Agent, AgentValue, ConnId, Resumed};
use daemon_core::payload::{
    Answer, CallError, CarrierKind, ChannelError, ClientRequest, GitRefusal, JobResult,
    MessageFrom, MessageRefusal, PeerFailure, PeerReply, PeerRequest, Response, Status,
};
use daemon_core::{
    AgentId, CallId, Daemon, Ended, Fingerprint, HostId, Input, Job, JobId, LinkId, Output,
    Presence, Route, Settings, StartOptions, StepBudget, TimerId, Tried,
};
use interpret::testkind::TestKind;
use prost::Message as _;
use provider_fakes::script::Step;
use store::Sqlite;
use wire::{CtlFrame, Lifecycle, ctl_frame};

use super::provider::{Provider, flatten, is_turn_end};
use crate::held::{Held, Slot};
use crate::lifecycle::{
    EXIT_CAUSE_LAST_STEP, EXITS_AT_TURN_END, cause_on_last_step, decided_in_its_time,
};
use crate::presence::{
    AWAY_ONLY_ON_PAYMENT_REFUSAL, CHANGES_ON_A_CONCLUDED_ATTEMPT, ONE_ATTEMPT_IN_FLIGHT,
    attempt_not_doubled, away_explained, change_prompted,
};
use crate::single_daemon::{
    COMMIT_BEFORE_BROADCAST, GENERATION_BUMPS_AFTER_UNCLEAN, GENERATION_BUMPS_ONLY_AFTER_UNCLEAN,
    answered_unheld, bumps_after_unclean, bumps_only_after_unclean,
};

type Kind = TestKind;

/// Where a net's clock starts.
pub const START_MS: u64 = 1_000_000;
/// The version the agents' journals say wrote them.
const PRODUCER: &str = env!("CARGO_PKG_VERSION");
/// The segment size the agents' journals rotate at.
const SEGMENT_BYTES: u64 = 1 << 20;

/// A value hashed by how it reads, for the core's messages that are not
/// hashable themselves; two that compare equal read alike.
#[derive(Clone, PartialEq)]
pub struct Dbg<T>(pub T);

impl<T: Debug> Hash for Dbg<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        format!("{:?}", self.0).hash(state);
    }
}

impl<T: PartialEq> Eq for Dbg<T> {}

impl<T: Debug> Debug for Dbg<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// An agent's lifecycle and interpreter, as the agent core gives them.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Core(AgentValue);

impl Debug for Core {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("agent core")
    }
}

/// An agent's journal: each segment's start offset and its bytes.
pub type Journal = BTreeMap<u64, Vec<u8>>;

/// The journal's end: the offset one past its last byte.
pub fn journal_end(journal: &Journal) -> u64 {
    journal
        .last_key_value()
        .map_or(0, |(start, bytes)| start + bytes.len() as u64)
}

/// Appends a framed step as the journal writer does: an empty step writes
/// nothing, and a frame that would take a non-empty segment past its size
/// starts the next.
fn append(journal: &mut Journal, step: &wire::Step) {
    if step.encoded_len() == 0 {
        return;
    }
    let frame = journal::encode_frame(step);
    let end = journal_end(journal);
    match journal.last_entry() {
        Some(mut last)
            if last.get().is_empty()
                || last.get().len() as u64 + frame.len() as u64 <= SEGMENT_BYTES =>
        {
            last.get_mut().extend_from_slice(&frame);
        }
        _ => {
            journal.insert(end, frame);
        }
    }
}

/// What a host's step broke of the promises judged as it steps, if it
/// broke one: what it answered that its store does not hold, and how it
/// moved another host's presence, each host having read as `views` before
/// the step and `concluded` the attempt the step concluded.
fn judge_step(
    daemon: &Daemon,
    out: &[Output],
    kind: &str,
    name: &str,
    others: &[(HostId, &str)],
    views: Vec<daemon_core::HostView>,
    concluded: Option<(HostId, Route)>,
) -> Option<(&'static str, String)> {
    if let Some(row) = answered_unheld(daemon, out) {
        let what = format!("{name} answered {row} on {kind} before its store held it");
        return Some((COMMIT_BEFORE_BROADCAST, what));
    }
    for (&(other, other_name), was) in others.iter().zip(views) {
        let now = daemon.presence().view(other);
        if now.presence == was.presence {
            continue;
        }
        let change = format!(
            "{name} read {other_name} {:?} -> {:?} on {kind}",
            was.presence, now.presence
        );
        let on = concluded
            .filter(|(host, _)| *host == other)
            .map(|(_, route)| route);
        // No call in the net waits for a channel: calls are carried
        // whenever their link is up.
        if !change_prompted(&now, on, false) {
            return Some((CHANGES_ON_A_CONCLUDED_ATTEMPT, change));
        }
        // Nor does any relay refuse one for the account's plan.
        if !away_explained(&now, false) {
            return Some((AWAY_ONLY_ON_PAYMENT_REFUSAL, change));
        }
    }
    None
}

/// Whether the process plays a recorded journal with steps still to write.
fn plays_on(plan: &Plan, w: &World, p: usize) -> bool {
    let agent = w.processes[p].agent;
    plan.recorded
        .get(&agent)
        .is_some_and(|journal| w.played.get(&agent).copied().unwrap_or(0) < journal.len())
}

/// The incarnation end the journal's newest step records, if it records
/// one: its cause and detail.
fn last_end(journal: &Journal) -> Option<(i32, Option<String>)> {
    let (_, bytes) = journal.iter().rev().find(|(_, bytes)| !bytes.is_empty())?;
    let mut newest = None;
    let mut at = 0;
    while let journal::Frame::Whole(step, length) = journal::decode_frame(&bytes[at..]) {
        newest = Some(step);
        at += length;
    }
    let end = newest?.incarnation_end?;
    Some((end.cause, end.detail))
}

/// Why a net failed: what its cores reported, or what its drivers could
/// not do.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Failure {
    /// A core asked for a dump: a step over its budget or a broken
    /// invariant its own step checks.
    Dump(String),
    /// A host's store broke an invariant its core checks after a write.
    Violation(String),
    /// The drivers met something no driver should: a spec naming no agent.
    Driver(String),
    /// A promise a machine's own model checks, judged here as the net
    /// steps, broke: the promise, and what broke it.
    Broke(&'static str, String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Dump(what) | Failure::Violation(what) | Failure::Driver(what) => {
                f.write_str(what)
            }
            Failure::Broke(promise, what) => write!(f, "{what}, breaking \"{promise}\""),
        }
    }
}

/// What does not change as a net runs: who its hosts are, and its agents'
/// names and scripts, or the recorded journals they play.
#[derive(Clone, Debug, Default)]
struct Plan {
    hosts: Vec<HostPlan>,
    names: BTreeMap<AgentId, String>,
    scripts: BTreeMap<AgentId, Vec<Step>>,
    /// The steps of a recorded journal an agent's process writes as its
    /// own, one at a time, in place of its core's.
    recorded: BTreeMap<AgentId, Vec<wire::Step>>,
    /// Whether deliveries are written down as they are made.
    tracing: bool,
}

#[derive(Clone, Debug)]
struct HostPlan {
    name: String,
    id: HostId,
    /// The hosts it trusts: the ones the topology links it to.
    trusted: Vec<HostId>,
    /// What its daemon is configured with at every start.
    settings: Settings,
}

/// One host: its core while it runs and its store, its agents' journals,
/// and what its driver holds.
#[derive(Clone, Debug)]
pub struct HostState {
    /// The core as its checkpoint, empty while the daemon is down, and the
    /// store as its dump: what reached the drive.
    held: Held,
    /// The core as a checker tells it apart, while it runs.
    print: Option<Fingerprint>,
    up: bool,
    /// The machine's boot: a new one after each unclean reboot.
    boot: u32,
    /// What the last start recorded of its run, as the installation keeps
    /// it beside the store, synced.
    run: Option<RunRecord>,
    ticked: Option<u64>,
    /// Timers its core armed for later, by when they fall due.
    timers: BTreeMap<TimerId, u64>,
    /// The ids its driver gives calls arriving from clients and peers.
    next_call: u64,
    next_link: u64,
    journals: BTreeMap<AgentId, Journal>,
}

/// The installation's record of its last run: the boot it started under,
/// whether it shut down cleanly, and the generation it ran under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RunRecord {
    boot: u32,
    clean: bool,
    counter: u64,
}

impl HostState {
    /// The generation the host runs under, or ran under last.
    fn generation(&self) -> u64 {
        self.run.map_or(1, |run| run.counter)
    }
}

/// A link between two hosts, and each end's id for it while it is up.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Link {
    a: usize,
    b: usize,
    severed: bool,
    live: Option<(LinkId, LinkId)>,
}

/// One agent process: its core, its provider and its connection to its
/// daemon.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Process {
    pub host: usize,
    pub agent: AgentId,
    /// Its core while it runs.
    core: Option<Core>,
    provider: Provider,
    pub running: bool,
    /// The connection to its daemon, as both ends know it.
    pub conn: Option<ConnId>,
    conns: u64,
    /// Agent messages the daemon wrote on the connection, awaiting their
    /// verdict.
    written: BTreeSet<Vec<u8>>,
    /// A person's inputs the shell relayed to it, by input id, with the
    /// verdict once it came.
    relayed: BTreeMap<Vec<u8>, Option<Dbg<wire::SendInputResponse>>>,
    timers: BTreeMap<agent_core::TimerId, u64>,
}

/// A call a host made of a peer, as the drivers between them carry it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Outgoing {
    to: usize,
    /// The id the serving host's driver gave it.
    served: CallId,
    stream: bool,
    /// The serving host has stepped the request.
    arrived: bool,
    /// A unary call's reply, held until the call ends.
    reply: Option<Dbg<PeerReply>>,
    /// The reply a call that ends well with nothing said answers with.
    silent: Option<Dbg<PeerReply>>,
}

/// A client's call on a host: what came back, and how it ended.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ClientCall {
    responses: Vec<Dbg<Response>>,
    ended: Option<Dbg<Status>>,
}

impl ClientCall {
    /// What the host answered, in order.
    pub fn responses(&self) -> impl Iterator<Item = &Response> {
        self.responses.iter().map(|response| &response.0)
    }

    pub fn response_count(&self) -> usize {
        self.responses.len()
    }

    /// How the call ended, once it has.
    pub fn ended(&self) -> Option<&Status> {
        self.ended.as_ref().map(|status| &status.0)
    }
}

/// One ordered way deliveries travel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lane {
    /// A timer of a host's core that fell due.
    Timer(usize, TimerId),
    /// A job a host's core asked for, and its result.
    Job(usize, JobId),
    /// What a host's driver hears of its link to another host, in order.
    Link(usize, usize),
    /// A host's own driver telling its core what it saw at once.
    Local(usize),
    /// A call a host made, on its way to the host it went to.
    Request(usize, CallId),
    /// What comes back of that call.
    Answer(usize, CallId),
    /// A daemon dialling a process.
    Dial(usize),
    /// A daemon's frames to a process on one connection.
    ToAgent(usize, ConnId),
    /// A process's frames to its daemon on one connection, then its close.
    FromAgent(usize, ConnId),
    AgentTimer(usize, agent_core::TimerId),
    AgentJob(usize, agent_core::JobId),
    /// What a process's provider says, one fact at a time, whenever it has
    /// something to say.
    Provider(usize),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Delivery {
    Host(usize, Dbg<Input>),
    Agent(usize, Dbg<agent_core::Input>),
    /// The job runs when it is delivered, then its host hears its result.
    Job(usize, JobId, Dbg<Job>),
    /// The daemon's dial reaches the process: it connects, or finds it
    /// gone.
    Dial(usize),
    /// The process's frame reaches its daemon.
    Frame(usize, ConnId, Dbg<CtlFrame>),
    /// The process's end reaches its daemon as the connection closing.
    Closed(usize, ConnId),
}

fn host(h: usize, input: Input) -> Delivery {
    Delivery::Host(h, Dbg(input))
}

fn agent(p: usize, input: agent_core::Input) -> Delivery {
    Delivery::Agent(p, Dbg(input))
}

/// Everything that changes as a net runs.
#[derive(Clone, Debug)]
struct World {
    now_ms: u64,
    hosts: Vec<HostState>,
    links: Vec<Link>,
    processes: Vec<Process>,
    /// Each agent's running process.
    running: BTreeMap<AgentId, usize>,
    /// Each agent's newest spec, for the next incarnation's.
    specs: BTreeMap<AgentId, Dbg<wire::AgentSpec>>,
    /// Where each agent's interpreter stood when its last process ended.
    resumes: BTreeMap<AgentId, Vec<u8>>,
    lanes: BTreeMap<Lane, VecDeque<Delivery>>,
    outgoing: BTreeMap<(usize, CallId), Outgoing>,
    /// Calls a host serves for a peer: the caller and its id for each.
    served: BTreeMap<(usize, CallId), (usize, CallId)>,
    clients: BTreeMap<(usize, CallId), ClientCall>,
    /// Waits for an agent's directory to come free: its host, the job, the
    /// agent, and when its patience runs out.
    unlocking: Vec<(usize, JobId, AgentId, u64)>,
    gates: BTreeSet<String>,
    /// How many steps of its recorded journal each agent playing one has
    /// written, in any of its processes.
    played: BTreeMap<AgentId, usize>,
    failure: Option<Failure>,
}

/// A topology's daemons and agents as cores, and what is on its way
/// between them: a value, compared and hashed by what changes as it runs.
#[derive(Clone)]
pub struct Net {
    plan: Arc<Plan>,
    w: World,
    /// One line per delivery, when tracing.
    trace: Vec<String>,
    /// Deliveries made.
    steps: u64,
}

/// What tells two nets apart: the cores by their fingerprints, the calls
/// a core made under the ids its fingerprint gives them wherever the
/// drivers hold them, and everything else as it is but the counters the
/// drivers mint ids from, since an id is only ever compared with the ids in
/// use.
#[derive(PartialEq, Eq, Hash)]
struct Canon<'a> {
    now_ms: u64,
    hosts: Vec<HostCanon<'a>>,
    links: &'a [Link],
    processes: Vec<ProcessCanon<'a>>,
    running: &'a BTreeMap<AgentId, usize>,
    specs: &'a BTreeMap<AgentId, Dbg<wire::AgentSpec>>,
    resumes: &'a BTreeMap<AgentId, Vec<u8>>,
    lanes: Vec<(Lane, Vec<DeliveryCanon<'a>>)>,
    outgoing: Vec<((usize, CallId), &'a Outgoing)>,
    served: Vec<((usize, CallId), (usize, CallId))>,
    clients: &'a BTreeMap<(usize, CallId), ClientCall>,
    unlocking: Vec<(usize, JobId, AgentId, u64)>,
    gates: &'a BTreeSet<String>,
    played: &'a BTreeMap<AgentId, usize>,
    failure: &'a Option<Failure>,
}

#[derive(PartialEq, Eq, Hash)]
struct HostCanon<'a> {
    print: Option<&'a [u8]>,
    store: &'a store::Dump,
    up: bool,
    boot: u32,
    run: Option<RunRecord>,
    ticked: Option<u64>,
    timers: &'a BTreeMap<TimerId, u64>,
    journals: &'a BTreeMap<AgentId, Journal>,
}

#[derive(PartialEq, Eq, Hash)]
struct ProcessCanon<'a> {
    host: usize,
    agent: AgentId,
    core: Option<&'a Core>,
    provider: &'a Provider,
    running: bool,
    conn: Option<ConnId>,
    written: &'a BTreeSet<Vec<u8>>,
    relayed: &'a BTreeMap<Vec<u8>, Option<Dbg<wire::SendInputResponse>>>,
    timers: &'a BTreeMap<agent_core::TimerId, u64>,
}

#[derive(PartialEq, Eq, Hash, Debug)]
enum DeliveryCanon<'a> {
    As(&'a Delivery),
    /// An answer to a call its host made, under the call's canonical id.
    Answer(usize, CallId, DbgRef<'a, Answer>),
    /// A job its host asked for, under the job's canonical id.
    Job(usize, JobId, &'a Dbg<Job>),
    /// A job's result, under the job's canonical id.
    JobDone(usize, JobId, DbgRef<'a, JobResult>),
}

/// A borrowed value hashed by how it reads.
#[derive(PartialEq, Debug)]
struct DbgRef<'a, T>(&'a T);

impl<T: Debug> Hash for DbgRef<'_, T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        format!("{:?}", self.0).hash(state);
    }
}

impl<T: PartialEq> Eq for DbgRef<'_, T> {}

impl World {
    fn canon(&self) -> Canon<'_> {
        let calls = |h: usize| self.hosts[h].print.as_ref().map(|print| &print.calls);
        let call = |h: usize, call: CallId| {
            calls(h).map_or(call, |calls| {
                calls.get(&call).copied().unwrap_or(CallId(u64::MAX))
            })
        };
        let job = |h: usize, job: JobId| {
            self.hosts[h].print.as_ref().map_or(job, |print| {
                print.jobs.get(&job).copied().unwrap_or(JobId(u64::MAX))
            })
        };
        let lane = |lane: Lane| match lane {
            Lane::Request(h, c) => Lane::Request(h, call(h, c)),
            Lane::Answer(h, c) => Lane::Answer(h, call(h, c)),
            Lane::Job(h, j) => Lane::Job(h, job(h, j)),
            other => other,
        };
        let mut lanes: Vec<(Lane, Vec<DeliveryCanon<'_>>)> = self
            .lanes
            .iter()
            .map(|(&at, queue)| {
                let queue = queue
                    .iter()
                    .map(|delivery| match delivery {
                        Delivery::Host(h, Dbg(Input::PeerAnswer { call: c, answer })) => {
                            DeliveryCanon::Answer(*h, call(*h, *c), DbgRef(answer))
                        }
                        Delivery::Job(h, j, op) => DeliveryCanon::Job(*h, job(*h, *j), op),
                        Delivery::Host(h, Dbg(Input::JobDone { job: j, result })) => {
                            DeliveryCanon::JobDone(*h, job(*h, *j), DbgRef(result))
                        }
                        other => DeliveryCanon::As(other),
                    })
                    .collect();
                (lane(at), queue)
            })
            .collect();
        lanes.sort_by_key(|(lane, _)| *lane);
        let mut outgoing: Vec<((usize, CallId), &Outgoing)> = self
            .outgoing
            .iter()
            .map(|(&(h, c), outgoing)| ((h, call(h, c)), outgoing))
            .collect();
        outgoing.sort_by_key(|(key, _)| *key);
        Canon {
            now_ms: self.now_ms,
            hosts: self
                .hosts
                .iter()
                .map(|host| HostCanon {
                    print: host.print.as_ref().map(|print| print.state.as_slice()),
                    store: &host.held.store,
                    up: host.up,
                    boot: host.boot,
                    run: host.run,
                    ticked: host.ticked,
                    timers: &host.timers,
                    journals: &host.journals,
                })
                .collect(),
            links: &self.links,
            processes: self
                .processes
                .iter()
                .map(|process| ProcessCanon {
                    host: process.host,
                    agent: process.agent,
                    core: process.core.as_ref(),
                    provider: &process.provider,
                    running: process.running,
                    conn: process.conn,
                    written: &process.written,
                    relayed: &process.relayed,
                    timers: &process.timers,
                })
                .collect(),
            running: &self.running,
            specs: &self.specs,
            resumes: &self.resumes,
            lanes,
            outgoing,
            served: self
                .served
                .iter()
                .map(|(&key, &(caller, c))| (key, (caller, call(caller, c))))
                .collect(),
            clients: &self.clients,
            unlocking: self
                .unlocking
                .iter()
                .map(|&(h, j, agent, until)| (h, job(h, j), agent, until))
                .collect(),
            gates: &self.gates,
            played: &self.played,
            failure: &self.failure,
        }
    }
}

impl PartialEq for Net {
    fn eq(&self, other: &Self) -> bool {
        self.w.canon() == other.w.canon()
    }
}

impl Eq for Net {}

impl Hash for Net {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.w.canon().hash(state);
    }
}

impl Debug for Net {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let lanes: Vec<String> = self
            .w
            .lanes
            .iter()
            .map(|(lane, queue)| format!("{lane:?} x{}", queue.len()))
            .collect();
        f.debug_struct("Net")
            .field("now_ms", &self.w.now_ms)
            .field(
                "up",
                &self.w.hosts.iter().map(|host| host.up).collect::<Vec<_>>(),
            )
            .field(
                "links",
                &self
                    .w
                    .links
                    .iter()
                    .map(|link| (link.severed, link.live.is_some()))
                    .collect::<Vec<_>>(),
            )
            .field(
                "processes",
                &self
                    .w
                    .processes
                    .iter()
                    .map(|process| (process.running, process.conn))
                    .collect::<Vec<_>>(),
            )
            .field("lanes", &lanes)
            .field(
                "timers",
                &self
                    .w
                    .hosts
                    .iter()
                    .map(|host| &host.timers)
                    .collect::<Vec<_>>(),
            )
            .field("failure", &self.w.failure)
            .finish()
    }
}

impl Net {
    /// An empty net; `tracing` writes each delivery down as it is made.
    pub fn new(tracing: bool) -> Self {
        crate::share_sqlite_across_threads();
        Self {
            plan: Arc::new(Plan {
                tracing,
                ..Plan::default()
            }),
            w: World {
                now_ms: START_MS,
                hosts: Vec::new(),
                links: Vec::new(),
                processes: Vec::new(),
                running: BTreeMap::new(),
                specs: BTreeMap::new(),
                resumes: BTreeMap::new(),
                played: BTreeMap::new(),
                lanes: BTreeMap::new(),
                outgoing: BTreeMap::new(),
                served: BTreeMap::new(),
                clients: BTreeMap::new(),
                unlocking: Vec::new(),
                gates: BTreeSet::new(),
                failure: None,
            },
            trace: Vec::new(),
            steps: 0,
        }
    }

    /// Adds a host, its daemon not yet started, trusting `trusted`.
    pub fn add_host(&mut self, name: &str, id: HostId, trusted: Vec<HostId>) -> usize {
        Arc::make_mut(&mut self.plan).hosts.push(HostPlan {
            name: name.to_owned(),
            id,
            trusted,
            settings: Settings::default(),
        });
        let store = Sqlite::open_in_memory(id.0).expect("a store opens in memory");
        let held = Held {
            core: Vec::new(),
            store: store.dump().expect("a store dumps"),
        };
        self.w.hosts.push(HostState {
            held,
            print: None,
            up: false,
            boot: 0,
            run: None,
            ticked: None,
            timers: BTreeMap::new(),
            next_call: 0,
            next_link: 0,
            journals: BTreeMap::new(),
        });
        self.w.hosts.len() - 1
    }

    /// Configures the host's daemon with `settings` from its next start
    /// on, where a story needs other than the product's.
    pub fn configure(&mut self, h: usize, settings: Settings) {
        Arc::make_mut(&mut self.plan).hosts[h].settings = settings;
    }

    /// Adds a link between two hosts, down until both ends run.
    pub fn add_link(&mut self, a: usize, b: usize) -> usize {
        self.w.links.push(Link {
            a,
            b,
            severed: false,
            live: None,
        });
        self.w.links.len() - 1
    }

    /// Names an agent and gives it the script its provider plays.
    pub fn add_agent(&mut self, agent: AgentId, name: &str, script: &[Step]) {
        let plan = Arc::make_mut(&mut self.plan);
        plan.names.insert(agent, name.to_owned());
        plan.scripts.insert(agent, flatten(script));
    }

    /// Names an agent whose process plays a recorded journal: on its
    /// provider's lane it writes the journal's next step and nudges its
    /// daemon, as a process whose provider said that much would, while its
    /// core keeps the control protocol and writes nothing of its own. The
    /// steps are the journal's from its initial snapshot on; its daemon
    /// reads them as the agent's whatever its core has heard.
    pub fn add_agent_playing(&mut self, agent: AgentId, name: &str, journal: Vec<wire::Step>) {
        let plan = Arc::make_mut(&mut self.plan);
        plan.names.insert(agent, name.to_owned());
        plan.recorded.insert(agent, journal);
    }

    pub fn hosts(&self) -> usize {
        self.w.hosts.len()
    }

    pub fn host_id(&self, h: usize) -> HostId {
        self.plan.hosts[h].id
    }

    pub fn host_name(&self, h: usize) -> &str {
        &self.plan.hosts[h].name
    }

    pub fn host_index(&self, id: HostId) -> Option<usize> {
        self.plan.hosts.iter().position(|host| host.id == id)
    }

    pub fn is_up(&self, h: usize) -> bool {
        self.w.hosts[h].up
    }

    pub fn generation(&self, h: usize) -> u64 {
        self.w.hosts[h].generation()
    }

    pub fn now_ms(&self) -> u64 {
        self.w.now_ms
    }

    /// Whether the net failed by breaking `promise`.
    pub fn broke(&self, promise: &str) -> bool {
        matches!(self.failure(), Some(Failure::Broke(broke, _)) if *broke == promise)
    }

    pub fn failure(&self) -> Option<&Failure> {
        self.w.failure.as_ref()
    }

    /// Every delivery made so far, one line each, when tracing.
    pub fn trace(&self) -> &[String] {
        &self.trace
    }

    pub fn steps(&self) -> u64 {
        self.steps
    }

    pub fn links(&self) -> usize {
        self.w.links.len()
    }

    /// The link's two hosts.
    pub fn link_ends(&self, l: usize) -> (usize, usize) {
        (self.w.links[l].a, self.w.links[l].b)
    }

    pub fn is_severed(&self, l: usize) -> bool {
        self.w.links[l].severed
    }

    /// Whether both ends of the link have heard everything their drivers
    /// had to tell them of it.
    pub fn link_heard(&self, l: usize) -> bool {
        let Link { a, b, .. } = self.w.links[l];
        !self.w.lanes.contains_key(&Lane::Link(a, b))
            && !self.w.lanes.contains_key(&Lane::Link(b, a))
    }

    pub fn is_live(&self, l: usize) -> bool {
        self.w.links[l].live.is_some()
    }

    pub fn processes(&self) -> &[Process] {
        &self.w.processes
    }

    /// The agent's running process.
    pub fn process_of(&self, agent: AgentId) -> Option<usize> {
        self.w.running.get(&agent).copied()
    }

    pub fn agent_name(&self, agent: AgentId) -> Option<&str> {
        self.plan.names.get(&agent).map(String::as_str)
    }

    pub fn client(&self, h: usize, call: CallId) -> Option<&ClientCall> {
        self.w.clients.get(&(h, call))
    }

    /// The client's call, taken from the net once its client is done
    /// reading it.
    pub fn take_client(&mut self, h: usize, call: CallId) -> Option<ClientCall> {
        self.w.clients.remove(&(h, call))
    }

    /// The verdict on a person's input relayed to the process, once it
    /// came.
    pub fn verdict(&self, p: usize, input_id: &[u8]) -> Option<&wire::SendInputResponse> {
        self.w.processes[p]
            .relayed
            .get(input_id)?
            .as_ref()
            .map(|verdict| &verdict.0)
    }

    /// The person's input ids relayed to the process, each with whether
    /// its verdict came.
    pub fn relayed(
        &self,
        p: usize,
    ) -> impl Iterator<Item = (&[u8], Option<&wire::SendInputResponse>)> {
        self.w.processes[p]
            .relayed
            .iter()
            .map(|(id, verdict)| (id.as_slice(), verdict.as_ref().map(|verdict| &verdict.0)))
    }

    /// Forgets a relayed input once its verdict has been read.
    pub fn forget_relayed(&mut self, p: usize, input_id: &[u8]) {
        self.w.processes[p].relayed.remove(input_id);
    }

    pub fn open_gate(&mut self, name: &str) {
        self.w.gates.insert(name.to_owned());
    }

    /// The agent's journal on its host, as the drive holds it.
    pub fn journal(&self, h: usize, agent: AgentId) -> Option<&Journal> {
        self.w.hosts[h].journals.get(&agent)
    }

    /// Reads a host's store while its daemon runs.
    pub fn read<R>(&self, h: usize, read: impl FnOnce(&Sqlite) -> R) -> Option<R> {
        self.w.hosts[h].up.then(|| self.read_held(h, read))
    }

    /// Reads a host's store as the drive holds it, whether its daemon runs
    /// or not.
    pub fn read_held<R>(&self, h: usize, read: impl FnOnce(&Sqlite) -> R) -> R {
        let id = self.plan.hosts[h].id;
        with_scratch(|scratch| scratch.of(id).0.read(&self.w.hosts[h].held.store, read))
    }

    /// Reads two hosts' stores side by side, as their drives hold them.
    pub fn read_two<R>(&self, a: usize, b: usize, read: impl FnOnce(&Sqlite, &Sqlite) -> R) -> R {
        let (ia, ib) = (self.plan.hosts[a].id, self.plan.hosts[b].id);
        assert_ne!(ia, ib, "two hosts read side by side are two");
        with_scratch(|scratch| {
            scratch.of(ia);
            let (mut slot_a, dir_a) = scratch.hosts.remove(&ia).expect("just made");
            let result = scratch
                .of(ib)
                .0
                .read(&self.w.hosts[b].held.store, |store_b| {
                    slot_a.read(&self.w.hosts[a].held.store, |store_a| {
                        read(store_a, store_b)
                    })
                });
            scratch.hosts.insert(ia, (slot_a, dir_a));
            result
        })
    }

    /// Whether `near` reads `far` online over a link of its own.
    pub fn reads_direct(&self, near: usize, far: usize) -> bool {
        let far = self.plan.hosts[far].id;
        self.with_daemon(near, |daemon| {
            let view = daemon.presence().view(far);
            view.presence == Presence::Online && view.route().is_some_and(|route| route.is_direct())
        })
        .unwrap_or(false)
    }

    /// Whether `near` reads `far` offline, or is down itself.
    pub fn reads_offline(&self, near: usize, far: usize) -> bool {
        let far = self.plan.hosts[far].id;
        self.with_daemon(near, |daemon| {
            daemon.presence().view(far).presence == Presence::Offline
        })
        .unwrap_or(true)
    }

    /// Reads a running host's core, restored for the reading.
    pub fn with_daemon<R>(&self, h: usize, read: impl FnOnce(&Daemon) -> R) -> Option<R> {
        if !self.w.hosts[h].up {
            return None;
        }
        let id = self.plan.hosts[h].id;
        let mut held = self.w.hosts[h].held.clone();
        Some(with_scratch(|scratch| {
            let (slot, dir) = scratch.of(id);
            let home = agents_home(id);
            slot.with_core(&mut held, (dir.path(), &home), |daemon| read(daemon))
                .0
        }))
    }

    /// Every lane with a delivery waiting, in a fixed order: the choices of
    /// what happens next.
    pub fn deliverable(&self) -> Vec<Lane> {
        let mut lanes: Vec<Lane> = self.w.lanes.keys().copied().collect();
        for (p, process) in self.w.processes.iter().enumerate() {
            let says = match self.plan.recorded.contains_key(&process.agent) {
                true => plays_on(&self.plan, &self.w, p),
                false => process.provider.can_say(&self.w.gates),
            };
            if process.running && says {
                lanes.push(Lane::Provider(p));
            }
        }
        lanes
    }

    /// Whether the lane's first delivery is a call's message that may be
    /// lost on its way.
    pub fn can_lose(&self, lane: Lane) -> bool {
        let (Lane::Request(caller, call) | Lane::Answer(caller, call)) = lane else {
            return false;
        };
        self.w.outgoing.contains_key(&(caller, call))
            && self
                .w
                .lanes
                .get(&lane)
                .and_then(VecDeque::front)
                .is_some_and(|delivery| {
                    matches!(
                        delivery,
                        Delivery::Host(_, Dbg(Input::PeerCall { .. } | Input::PeerAnswer { .. }))
                    )
                })
    }

    /// Makes the lane's first delivery; false when it has none, or the net
    /// has failed.
    pub fn deliver(&mut self, lane: Lane) -> bool {
        if self.w.failure.is_some() {
            return false;
        }
        self.steps += 1;
        self.transition(|t| t.deliver(lane))
    }

    /// Loses the call's message at the lane's front, breaking the call.
    pub fn lose(&mut self, lane: Lane) {
        let (Lane::Request(caller, call) | Lane::Answer(caller, call)) = lane else {
            return;
        };
        self.steps += 1;
        self.transition(|t| {
            if t.plan.tracing {
                let line = format!("{}'s call {} lost a message", t.name(caller), call.0);
                t.trace.push(line);
            }
            t.break_call(caller, call);
        });
    }

    /// Starts the host's daemon over what its store holds.
    pub fn start_host(&mut self, h: usize, entropy: [u8; 32]) {
        self.transition(|t| t.start_host(h, entropy, false));
    }

    /// The host's daemon dies; its store stays as it was.
    pub fn stop_host(&mut self, h: usize) {
        self.transition(|t| t.stop_host(h));
    }

    /// The host's machine reboots under its daemon, which never shuts down
    /// cleanly: the daemon and the agent processes go with the machine,
    /// and the daemon starts again on the new boot, under the generation
    /// the installation's record then decides.
    pub fn reboot_unclean(&mut self, h: usize, entropy: [u8; 32]) {
        self.transition(|t| {
            t.stop_host(h);
            for p in 0..t.w.processes.len() {
                if t.w.processes[p].host == h && t.w.processes[p].running {
                    t.w.processes[p].provider.kill();
                    t.exited(p);
                }
            }
            t.w.hosts[h].boot += 1;
            t.start_host(h, entropy, true);
        });
    }

    /// Cuts the link: both ends hear it go, and it stays down.
    pub fn sever(&mut self, l: usize) {
        self.transition(|t| {
            t.w.links[l].severed = true;
            t.link_down(l);
        });
    }

    /// Lets the link come back, up again at once if both ends run.
    pub fn restore(&mut self, l: usize) {
        self.transition(|t| {
            t.w.links[l].severed = false;
            t.link_up(l);
        });
    }

    /// A client's call on the host, stepped at once as its driver hands it
    /// over.
    pub fn client_call(&mut self, h: usize, request: ClientRequest) -> CallId {
        self.transition(|t| t.client_call(h, request))
    }

    /// The client lets its call go.
    pub fn cancel_client(&mut self, h: usize, call: CallId) {
        self.transition(|t| t.cancel_client(h, call));
    }

    /// Steps the host's core with one input from its shell.
    pub fn feed(&mut self, h: usize, input: Input) {
        self.transition(|t| t.feed(h, input));
    }

    /// Hands a person's input to the process on its connection, as the
    /// shell relays it; its verdict comes back as [`Net::verdict`].
    pub fn relay(&mut self, p: usize, input: wire::Input) -> Result<(), String> {
        self.transition(|t| t.relay(p, input))
    }

    /// Moves the clock on: every timer it passes falls due, in the order it
    /// was armed for.
    pub fn advance(&mut self, by_ms: u64) {
        self.transition(|t| t.advance(by_ms));
    }

    /// Whether a timer armed for later waits that is not a host's
    /// checkpoint: one whose firing moves what the net does, not only what
    /// its drives hold.
    pub fn waits_on_time(&self) -> bool {
        self.w
            .hosts
            .iter()
            .any(|host| host.timers.keys().any(|id| *id != TimerId::Checkpoint))
            || self
                .w
                .processes
                .iter()
                .any(|process| !process.timers.is_empty())
            || !self.w.unlocking.is_empty()
    }

    /// When the next timer armed for later falls due, if one is armed.
    pub fn next_due(&self) -> Option<u64> {
        let hosts = self
            .w
            .hosts
            .iter()
            .flat_map(|host| host.timers.values().copied());
        let processes = self
            .w
            .processes
            .iter()
            .flat_map(|process| process.timers.values().copied());
        let unlocking = self.w.unlocking.iter().map(|(.., until)| *until);
        hosts.chain(processes).chain(unlocking).min()
    }

    fn transition<R>(&mut self, f: impl FnOnce(&mut Transition<'_>) -> R) -> R {
        let steps = self.steps;
        let Self { plan, w, trace, .. } = self;
        with_scratch(|scratch| {
            let mut t = Transition {
                plan: plan.as_ref(),
                w,
                trace,
                scratch,
                steps,
            };
            f(&mut t)
        })
    }
}

/// Where a host's core is told its agents' directories live whenever it
/// is taken back, so its checkpoint names the same place on every thread.
fn agents_home(id: HostId) -> PathBuf {
    PathBuf::from(format!("/net/{}/agents", interpret::to_hex(&id.0)))
}

fn host_home(id: HostId) -> PathBuf {
    PathBuf::from(format!("/net/{}", interpret::to_hex(&id.0)))
}

/// One change to a net under way, over this thread's stores and
/// directories.
struct Transition<'a> {
    plan: &'a Plan,
    w: &'a mut World,
    trace: &'a mut Vec<String>,
    scratch: &'a mut Scratch,
    steps: u64,
}

impl Transition<'_> {
    fn name(&self, h: usize) -> &str {
        &self.plan.hosts[h].name
    }

    fn agent_name(&self, p: usize) -> String {
        let agent = self.w.processes[p].agent;
        self.plan
            .names
            .get(&agent)
            .cloned()
            .unwrap_or_else(|| interpret::to_hex(&agent.0[..4]))
    }

    fn host_index(&self, id: HostId) -> Option<usize> {
        self.plan.hosts.iter().position(|host| host.id == id)
    }

    fn process_of(&self, agent: AgentId) -> Option<usize> {
        self.w.running.get(&agent).copied()
    }

    fn fail(&mut self, failure: Failure) {
        if self.w.failure.is_none() {
            if self.plan.tracing {
                self.trace.push(format!("failed: {failure}"));
            }
            let at = self.steps;
            self.w.failure = Some(match failure {
                Failure::Dump(what) => Failure::Dump(format!("at step {at}: {what}")),
                Failure::Violation(what) => Failure::Violation(format!("at step {at}: {what}")),
                Failure::Driver(what) => Failure::Driver(format!("at step {at}: {what}")),
                Failure::Broke(promise, what) => {
                    Failure::Broke(promise, format!("at step {at}: {what}"))
                }
            });
        }
    }

    fn push(&mut self, lane: Lane, delivery: Delivery) {
        self.w.lanes.entry(lane).or_default().push_back(delivery);
    }

    fn deliver(&mut self, lane: Lane) -> bool {
        if let Lane::Provider(p) = lane
            && self.plan.recorded.contains_key(&self.w.processes[p].agent)
        {
            return self.play(p);
        }
        if let Lane::Provider(p) = lane {
            let process = &mut self.w.processes[p];
            if !process.running {
                return false;
            }
            let script = self
                .plan
                .scripts
                .get(&process.agent)
                .map_or(&[][..], Vec::as_slice);
            let Some(input) = process.provider.say(script, &self.w.gates) else {
                return false;
            };
            if self.plan.tracing {
                let line = format!("{} provider", self.agent_name(p));
                self.trace.push(line);
            }
            self.feed_agent(p, input);
            return true;
        }
        let Some(queue) = self.w.lanes.get_mut(&lane) else {
            return false;
        };
        let delivery = queue.pop_front().expect("no lane is kept empty");
        if queue.is_empty() {
            self.w.lanes.remove(&lane);
        }
        if self.plan.tracing {
            let line = self.describe(&delivery);
            self.trace.push(line);
        }
        match delivery {
            Delivery::Host(h, Dbg(input)) => {
                match (&input, lane) {
                    (Input::PeerAnswer { call, answer }, _) if answer.is_last() => {
                        self.w.outgoing.remove(&(h, *call));
                    }
                    (Input::PeerCall { .. }, Lane::Request(caller, call)) => {
                        if let Some(outgoing) = self.w.outgoing.get_mut(&(caller, call)) {
                            outgoing.arrived = true;
                        }
                    }
                    _ => {}
                }
                let input = self.served_by_the_shell(h, input);
                self.feed(h, input);
            }
            Delivery::Agent(p, Dbg(input)) => self.feed_agent(p, input),
            Delivery::Job(h, id, Dbg(op)) => {
                if let Some(result) = self.run_job(h, id, op) {
                    self.feed(h, Input::JobDone { job: id, result });
                }
            }
            Delivery::Dial(p) => self.dialled(p),
            Delivery::Frame(p, conn, Dbg(frame)) => self.frame(p, conn, frame),
            Delivery::Closed(p, conn) => {
                let process = &mut self.w.processes[p];
                if process.conn == Some(conn) {
                    process.conn = None;
                    let (h, agent) = (process.host, process.agent);
                    let ended = Ended::Exited;
                    self.feed(h, Input::ControlClosed { agent, ended });
                }
            }
        }
        true
    }

    /// A process playing a recorded journal writes its next step, and
    /// nudges its daemon if it is connected, as its core does after a step
    /// that wrote something.
    fn play(&mut self, p: usize) -> bool {
        if !self.w.processes[p].running || !plays_on(self.plan, self.w, p) {
            return false;
        }
        let process = &self.w.processes[p];
        let (h, agent_id, conn) = (process.host, process.agent, process.conn);
        let played = self.w.played.entry(agent_id).or_default();
        let step = &self.plan.recorded[&agent_id][*played];
        *played += 1;
        append(self.w.hosts[h].journals.entry(agent_id).or_default(), step);
        if self.plan.tracing {
            let line = format!("{} plays its journal", self.agent_name(p));
            self.trace.push(line);
        }
        if let Some(connection) = conn {
            let nudge = CtlFrame {
                of: Some(ctl_frame::Of::Nudge(wire::Nudge::default())),
            };
            self.push(
                Lane::FromAgent(p, connection),
                Delivery::Frame(p, connection, Dbg(nudge)),
            );
        }
        true
    }

    fn describe(&self, delivery: &Delivery) -> String {
        let host = |h: usize| self.name(h).to_owned();
        match delivery {
            Delivery::Host(h, Dbg(input)) => match input {
                Input::PeerCall { from, request, .. } => format!(
                    "{} {} from {}",
                    host(*h),
                    peer_request(request),
                    self.host_index(*from).map_or("?".to_owned(), host)
                ),
                Input::PeerAnswer { answer, .. } => {
                    format!("{} answered {}", host(*h), answer_kind(answer))
                }
                Input::Timer { id } => format!("{} timer {}", host(*h), timer_kind(id)),
                input => format!("{} {}", host(*h), input.kind()),
            },
            Delivery::Agent(p, Dbg(input)) => {
                format!("{} {}", self.agent_name(*p), agent_input(input))
            }
            Delivery::Job(h, _, Dbg(op)) => format!("{} job {}", host(*h), job_kind(op)),
            Delivery::Dial(p) => format!("{} dialled", self.agent_name(*p)),
            Delivery::Frame(p, _, Dbg(frame)) => {
                format!("{} says {}", self.agent_name(*p), frame_kind(frame))
            }
            Delivery::Closed(p, _) => format!("{} closed", self.agent_name(*p)),
        }
    }

    // --- hosts ------------------------------------------------------------

    /// Steps the host's core with each input in turn over this thread's
    /// store and directory, checking the store whole after each write and
    /// what each step answered and made of the other hosts' presence, and
    /// returns what the steps asked for, its paths named at the host's
    /// home.
    fn step_host(&mut self, h: usize, inputs: Vec<Input>) -> Vec<Output> {
        let id = self.plan.hosts[h].id;
        let name = &self.plan.hosts[h].name;
        let others: Vec<(HostId, &str)> = self
            .plan
            .hosts
            .iter()
            .filter(|other| other.id != id)
            .map(|other| (other.id, other.name.as_str()))
            .collect();
        let host = &mut self.w.hosts[h];
        let (slot, dir) = self.scratch.of(id);
        dir.lay_out(&host.journals);
        let home = agents_home(id);
        let mut violation = None;
        let mut broke = None;
        let (out, print) = slot.with_core(&mut host.held, (dir.path(), &home), |daemon| {
            let mut out = Vec::new();
            for input in inputs {
                let before = daemon.store().changes();
                let kind = input.kind();
                let views: Vec<_> = others
                    .iter()
                    .map(|&(other, _)| daemon.presence().view(other))
                    .collect();
                // An attempt whose link closed under it concludes on no
                // route: the route went before it did.
                let concluded = match &input {
                    Input::AttemptConcluded { host, route, tried }
                        if !matches!(tried, Tried::Failed(ChannelError::LinkClosed)) =>
                    {
                        Some((*host, *route))
                    }
                    _ => None,
                };
                let from = out.len();
                daemon.step(input, &mut out);
                if daemon.store().changes() != before
                    && let Err(broken) = daemon.check_all()
                {
                    violation.get_or_insert(format!(
                        "{name}'s store broke an invariant after {kind}: {broken}"
                    ));
                }
                if broke.is_none() {
                    broke = judge_step(daemon, &out[from..], kind, name, &others, views, concluded);
                }
            }
            out
        });
        host.print = Some(print);
        dir.reclaimed(&mut host.journals);
        let scratch = dir.path().to_path_buf();
        if let Some(violation) = violation {
            self.fail(Failure::Violation(violation));
        }
        if let Some((promise, what)) = broke {
            self.fail(Failure::Broke(promise, what));
        }
        out.into_iter()
            .map(|output| rehome(output, &scratch, &home))
            .collect()
    }

    /// Steps the host's core with one input, as its driver does: the clock
    /// first when it moved.
    fn feed(&mut self, h: usize, input: Input) {
        let now = self.w.now_ms;
        let host = &mut self.w.hosts[h];
        if !host.up {
            return;
        }
        let mut inputs = Vec::with_capacity(2);
        if host.ticked != Some(now) {
            host.ticked = Some(now);
            inputs.push(Input::Tick { now_ms: now });
        }
        inputs.push(input);
        let out = self.step_host(h, inputs);
        self.carry_out(h, out);
    }

    /// Starts the host's daemon over what its store holds: its settings,
    /// randomness, trust and own entry, then the start's sweep of its
    /// agents, then its links. `owed` says the machine restarted under a
    /// run that had not shut down cleanly, which the generation it serves
    /// must answer for.
    fn start_host(&mut self, h: usize, entropy: [u8; 32], owed: bool) {
        if self.w.hosts[h].up {
            return;
        }
        let now = self.w.now_ms;
        let plan = &self.plan.hosts[h];
        let id = plan.id;
        let home = host_home(id);
        let agents = agents_home(id);
        let host = &mut self.w.hosts[h];
        host.up = true;
        host.ticked = Some(now);
        // As the installation decides it at start, and records it synced
        // before anything is served.
        let boot_id = |boot: u32| format!("boot-{boot}");
        let last = host.run.map(|run| (boot_id(run.boot), run));
        let generation = daemon_core::generation::at_start(
            last.as_ref()
                .map(|(boot, run)| daemon_core::generation::LastRun {
                    boot_id: boot,
                    clean: run.clean,
                    counter: run.counter,
                }),
            &boot_id(host.boot),
        );
        let previous = host.run.map(|run| run.counter);
        let promise = [
            (
                GENERATION_BUMPS_AFTER_UNCLEAN,
                bumps_after_unclean as fn(_, _, _) -> _,
            ),
            (
                GENERATION_BUMPS_ONLY_AFTER_UNCLEAN,
                bumps_only_after_unclean,
            ),
        ]
        .into_iter()
        .find(|(_, keeps)| !keeps(owed, previous, generation));
        let broke = promise.map(|(promise, _)| {
            let after = if owed { "an unclean reboot" } else { "a restart on the same boot" };
            let what = format!(
                "{} served generation {generation} after {after}, its last run having served {previous:?}",
                plan.name
            );
            (promise, what)
        });
        host.run = Some(RunRecord {
            boot: host.boot,
            clean: false,
            counter: generation,
        });
        let options = StartOptions {
            generation,
            agents: agents.clone(),
            replicas: home.join("replicas"),
            drafts: home.join("drafts"),
            trust: home.join("trust.json"),
            budget: StepBudget::UNLIMITED,
        };
        let entry = wire::HostEntry {
            host_id: id.0.to_vec(),
            name: plan.name.clone(),
            generation,
            trust: wire::Trust::Trusted as i32,
            presence: wire::Presence::Online as i32,
            current: Some(true),
            ..wire::HostEntry::default()
        };
        let trusted = plan.trusted.clone();
        let settings = plan.settings;
        let running = &self.w.running;
        let (slot, dir) = self.scratch.of(id);
        dir.lay_out(&host.journals);
        let name = &plan.name;
        let mut violation = None;
        let (out, print) = slot.start_with(
            &mut host.held,
            (dir.path(), &agents),
            |store| Daemon::new(store, options),
            |daemon| {
                let mut out = Vec::new();
                let mut step = |daemon: &mut Daemon, input: Input| {
                    let before = daemon.store().changes();
                    let kind = input.kind();
                    daemon.step(input, &mut out);
                    if daemon.store().changes() != before
                        && let Err(broken) = daemon.check_all()
                    {
                        violation.get_or_insert(format!(
                            "{name}'s store broke an invariant after {kind}: {broken}"
                        ));
                    }
                };
                step(daemon, Input::Tick { now_ms: now });
                step(daemon, Input::Configure(settings));
                step(daemon, Input::Entropy(entropy));
                step(
                    daemon,
                    Input::TrustRead {
                        store: daemon_core::trust::TrustStore::of_hosts(trusted),
                    },
                );
                step(daemon, Input::OwnHost(Box::new(entry)));
                let live: Vec<AgentId> = daemon
                    .store()
                    .agents()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|row| row.agent.host == id.0 && row.lifecycle == Lifecycle::Live as i32)
                    .filter_map(|row| AgentId::from_slice(&row.agent.agent))
                    .collect();
                for agent in live {
                    if running.contains_key(&agent) {
                        step(daemon, Input::Adopted { agent });
                        step(daemon, Input::Nudge { agent });
                    } else {
                        let ended = Ended::Away;
                        step(daemon, Input::ControlClosed { agent, ended });
                    }
                }
                step(daemon, Input::Swept);
                out
            },
        );
        host.print = Some(print);
        dir.reclaimed(&mut host.journals);
        let scratch = dir.path().to_path_buf();
        if let Some(violation) = violation {
            self.fail(Failure::Violation(violation));
        }
        if let Some((promise, what)) = broke {
            self.fail(Failure::Broke(promise, what));
        }
        let out = out
            .into_iter()
            .map(|output| rehome(output, &scratch, &agents))
            .collect();
        self.carry_out(h, out);
        for l in 0..self.w.links.len() {
            if self.w.links[l].a == h || self.w.links[l].b == h {
                self.link_up(l);
            }
        }
    }

    /// The host's daemon dies: its core and timers go, its links and calls
    /// with it, and its agents see their connections close. Its store stays
    /// as it was.
    fn stop_host(&mut self, h: usize) {
        let host = &mut self.w.hosts[h];
        if !host.up {
            return;
        }
        host.up = false;
        host.held.core = Vec::new();
        host.print = None;
        host.ticked = None;
        host.timers.clear();
        for l in 0..self.w.links.len() {
            if self.w.links[l].a == h || self.w.links[l].b == h {
                self.link_down(l);
            }
        }
        let calls: Vec<(usize, CallId)> = self
            .w
            .outgoing
            .iter()
            .filter(|((caller, _), outgoing)| *caller == h || outgoing.to == h)
            .map(|(key, _)| *key)
            .collect();
        for (caller, call) in calls {
            self.break_call(caller, call);
        }
        self.w
            .served
            .retain(|(server, _), (caller, _)| *server != h && *caller != h);
        for client in self
            .w
            .clients
            .iter_mut()
            .filter(|((host, _), _)| *host == h)
            .map(|(_, client)| client)
        {
            client
                .ended
                .get_or_insert(Dbg(Status::Failed(CallError::Peer(PeerFailure::Uncertain))));
        }
        self.w.unlocking.retain(|(host, ..)| *host != h);
        for p in 0..self.w.processes.len() {
            let process = &mut self.w.processes[p];
            if process.host != h {
                continue;
            }
            process.written.clear();
            if let Some(connection) = process.conn.take()
                && process.running
            {
                self.push(
                    Lane::ToAgent(p, connection),
                    agent(p, agent_core::Input::DaemonGone { connection }),
                );
            }
        }
        // What was on its way to the dead daemon reaches nobody; its next
        // run starts from its store alone.
        let processes = &self.w.processes;
        for queue in self.w.lanes.values_mut() {
            queue.retain(|delivery| addressed_to(processes, delivery) != Some(h));
        }
        self.w.lanes.retain(|_, queue| !queue.is_empty());
    }

    // --- links ------------------------------------------------------------

    /// Brings the link up at both ends, if both run and it is not severed.
    fn link_up(&mut self, l: usize) {
        let Link {
            a,
            b,
            severed,
            live,
        } = &self.w.links[l];
        let (a, b) = (*a, *b);
        if *severed || live.is_some() || !self.w.hosts[a].up || !self.w.hosts[b].up {
            return;
        }
        let mut id = |h: usize| {
            self.w.hosts[h].next_link += 1;
            LinkId(self.w.hosts[h].next_link)
        };
        let ids = (id(a), id(b));
        self.w.links[l].live = Some(ids);
        for (near, far, link) in [(a, b, ids.0), (b, a, ids.1)] {
            let peer = self.plan.hosts[far].id;
            let kind = CarrierKind::Direct;
            self.push(
                Lane::Link(near, far),
                host(
                    near,
                    Input::LinkUp {
                        link,
                        peer,
                        kind,
                        resumed: None,
                    },
                ),
            );
        }
    }

    /// The link goes as a dead connection does: each end that runs hears
    /// it go, and every call between them breaks.
    fn link_down(&mut self, l: usize) {
        let Some(ids) = self.w.links[l].live.take() else {
            return;
        };
        let (a, b) = (self.w.links[l].a, self.w.links[l].b);
        for (near, far, link) in [(a, b, ids.0), (b, a, ids.1)] {
            if self.w.hosts[near].up {
                let reason = wire::LinkCloseReason::Unspecified;
                self.push(
                    Lane::Link(near, far),
                    host(near, Input::LinkDown { link, reason }),
                );
            }
        }
        let calls: Vec<(usize, CallId)> = self
            .w
            .outgoing
            .iter()
            .filter(|((caller, _), outgoing)| {
                (*caller, outgoing.to) == (a, b) || (*caller, outgoing.to) == (b, a)
            })
            .map(|(key, _)| *key)
            .collect();
        for (caller, call) in calls {
            self.break_call(caller, call);
        }
    }

    /// The link `near` reaches `far` by, by `near`'s id for it, while it is
    /// up.
    fn live_link(&self, near: usize, far: usize) -> Option<LinkId> {
        self.w.links.iter().find_map(|link| {
            let (at_a, at_b) = link.live?;
            if (link.a, link.b) == (near, far) {
                Some(at_a)
            } else if (link.a, link.b) == (far, near) {
                Some(at_b)
            } else {
                None
            }
        })
    }

    // --- calls ------------------------------------------------------------

    fn client_call(&mut self, h: usize, request: ClientRequest) -> CallId {
        let host = &mut self.w.hosts[h];
        host.next_call += 1;
        let call = CallId(host.next_call);
        self.w.clients.insert((h, call), ClientCall::default());
        self.feed(h, Input::ClientCall { call, request });
        call
    }

    fn cancel_client(&mut self, h: usize, call: CallId) {
        if let Some(client) = self.w.clients.remove(&(h, call))
            && client.ended.is_none()
        {
            self.feed(h, Input::CallCancelled { call });
        }
    }

    /// A call the host's core makes of a peer goes out on the link to it,
    /// or fails at once as never sent.
    fn call(&mut self, h: usize, to: HostId, call: CallId, request: PeerRequest) {
        let reachable = self
            .host_index(to)
            .filter(|&to| self.live_link(h, to).is_some() && self.w.hosts[to].up);
        let Some(to) = reachable else {
            let answer = Answer::Failed(PeerFailure::NotSent);
            return self.push(
                Lane::Answer(h, call),
                host(h, Input::PeerAnswer { call, answer }),
            );
        };
        let server = &mut self.w.hosts[to];
        server.next_call += 1;
        let served = CallId(server.next_call);
        let stream = matches!(
            request,
            PeerRequest::Subscribe(_) | PeerRequest::SubscribeInventory
        );
        let silent = match request {
            PeerRequest::StopAgent(_) | PeerRequest::DeleteAgent(_) => Some(PeerReply::Done),
            PeerRequest::SendMessage(_) => {
                Some(PeerReply::SendMessage(wire::SendMessageResponse::default()))
            }
            _ => None,
        };
        self.w.outgoing.insert(
            (h, call),
            Outgoing {
                to,
                served,
                stream,
                arrived: false,
                reply: None,
                silent: silent.map(Dbg),
            },
        );
        self.w.served.insert((to, served), (h, call));
        let from = self.plan.hosts[h].id;
        self.push(
            Lane::Request(h, call),
            host(
                to,
                Input::PeerCall {
                    from,
                    call: served,
                    request,
                },
            ),
        );
    }

    /// A paired host's call as node's peer edge hands it to the core: a
    /// message as [`peer_message`] makes it, a create as [`peer_create`]
    /// does.
    fn served_by_the_shell(&self, h: usize, input: Input) -> Input {
        match input {
            Input::PeerCall {
                call,
                request: PeerRequest::SendMessage(envelope),
                ..
            } => Input::ClientCall {
                call,
                request: peer_message(envelope),
            },
            Input::PeerCall {
                from,
                call,
                request: PeerRequest::CreateAgent(create),
            } => match peer_create(self.plan.hosts[h].id, create.clone(), self.w.now_ms) {
                Some(request) => Input::ClientCall { call, request },
                None => Input::PeerCall {
                    from,
                    call,
                    request: PeerRequest::CreateAgent(create),
                },
            },
            other => other,
        }
    }

    /// The host's core wants nothing more of a call it made.
    fn hang_up(&mut self, h: usize, call: CallId) {
        let Some(outgoing) = self.w.outgoing.remove(&(h, call)) else {
            return;
        };
        self.w.lanes.remove(&Lane::Answer(h, call));
        if self
            .w
            .served
            .remove(&(outgoing.to, outgoing.served))
            .is_none()
        {
            return;
        }
        if outgoing.arrived {
            let served = outgoing.served;
            self.push(
                Lane::Request(h, call),
                host(outgoing.to, Input::CallCancelled { call: served }),
            );
        } else {
            self.w.lanes.remove(&Lane::Request(h, call));
        }
    }

    /// A call breaks under its link: the caller hears it is uncertain, and
    /// the host serving it lets it go.
    fn break_call(&mut self, caller: usize, call: CallId) {
        let Some(outgoing) = self.w.outgoing.remove(&(caller, call)) else {
            return;
        };
        self.w.lanes.remove(&Lane::Request(caller, call));
        self.w.lanes.remove(&Lane::Answer(caller, call));
        if self
            .w
            .served
            .remove(&(outgoing.to, outgoing.served))
            .is_some()
            && outgoing.arrived
        {
            let served = outgoing.served;
            self.push(
                Lane::Local(outgoing.to),
                host(outgoing.to, Input::CallCancelled { call: served }),
            );
        }
        let answer = Answer::Failed(PeerFailure::Uncertain);
        self.push(
            Lane::Answer(caller, call),
            host(caller, Input::PeerAnswer { call, answer }),
        );
    }

    fn answered(&mut self, h: usize, call: CallId, response: Response) {
        if let Some(&(caller, made)) = self.w.served.get(&(h, call)) {
            let answer = match response {
                Response::Session(event) => Answer::Session((*event).clone()),
                Response::Inventory(event) => Answer::Inventory((*event).clone()),
                other => {
                    if let Some(outgoing) = self.w.outgoing.get_mut(&(caller, made)) {
                        outgoing.reply = reply_of(other).map(Dbg);
                    }
                    return;
                }
            };
            let call = made;
            self.push(
                Lane::Answer(caller, call),
                host(caller, Input::PeerAnswer { call, answer }),
            );
        } else if let Some(client) = self.w.clients.get_mut(&(h, call)) {
            client.responses.push(Dbg(response));
        }
    }

    fn ended(&mut self, h: usize, call: CallId, status: Status) {
        if let Some((caller, made)) = self.w.served.remove(&(h, call)) {
            let Some(outgoing) = self.w.outgoing.get(&(caller, made)) else {
                return;
            };
            let answer = match status {
                Status::Ok if outgoing.stream => Answer::Ended,
                Status::Ok => match outgoing.reply.clone().or_else(|| outgoing.silent.clone()) {
                    Some(Dbg(reply)) => Answer::Reply(Box::new(reply)),
                    None => Answer::Failed(PeerFailure::Uncertain),
                },
                Status::Failed(error) => Answer::Failed(PeerFailure::Refused(wire_error(&error))),
            };
            let call = made;
            self.push(
                Lane::Answer(caller, call),
                host(caller, Input::PeerAnswer { call, answer }),
            );
        } else if let Some(client) = self.w.clients.get_mut(&(h, call)) {
            client.ended = Some(Dbg(status));
        }
    }

    // --- the drivers ------------------------------------------------------

    /// Carries out what a host's core asked for, as its driver does.
    fn carry_out(&mut self, h: usize, out: Vec<Output>) {
        for output in out {
            match output {
                Output::ArmTimer { id, at_ms } => {
                    self.w.lanes.remove(&Lane::Timer(h, id));
                    if at_ms <= self.w.now_ms {
                        self.w.hosts[h].timers.remove(&id);
                        self.push(Lane::Timer(h, id), host(h, Input::Timer { id }));
                    } else {
                        self.w.hosts[h].timers.insert(id, at_ms);
                    }
                }
                Output::CancelTimer { id } => {
                    self.w.hosts[h].timers.remove(&id);
                    self.w.lanes.remove(&Lane::Timer(h, id));
                }
                Output::Call {
                    host,
                    call,
                    request,
                    ..
                } => self.call(h, host, call, request),
                Output::HangUp { call } => self.hang_up(h, call),
                Output::Answer { call, response } => self.answered(h, call, response),
                Output::EndCall { call, status } => self.ended(h, call, status),
                Output::Attempt { host: to, route } => {
                    let far = self.host_index(to);
                    let lanes = [far.map(|far| Lane::Link(h, far)), Some(Lane::Local(h))];
                    let in_flight: BTreeSet<Route> = lanes
                        .iter()
                        .flatten()
                        .filter_map(|lane| self.w.lanes.get(lane))
                        .flatten()
                        .filter_map(|delivery| match delivery {
                            Delivery::Host(_, Dbg(Input::AttemptConcluded { host, route, .. }))
                                if *host == to =>
                            {
                                Some(*route)
                            }
                            _ => None,
                        })
                        .collect();
                    if !attempt_not_doubled(&in_flight, route) {
                        let what = format!(
                            "{} asked for a second attempt at {} on {route:?}",
                            self.name(h),
                            far.map_or("a host it does not know", |far| self.name(far)),
                        );
                        self.fail(Failure::Broke(ONE_ATTEMPT_IN_FLIGHT, what));
                    }
                    let tried = match (route, far.and_then(|far| self.live_link(h, far))) {
                        (Route::Direct(link), Some(live)) if link == live => Tried::Opened,
                        _ => Tried::Failed(ChannelError::LinkClosed),
                    };
                    let lane = far.map_or(Lane::Local(h), |far| Lane::Link(h, far));
                    self.push(
                        lane,
                        host(
                            h,
                            Input::AttemptConcluded {
                                host: to,
                                route,
                                tried,
                            },
                        ),
                    );
                }
                Output::DialAgent { agent } => match self.process_of(agent) {
                    Some(p) => self.push(Lane::Dial(p), Delivery::Dial(p)),
                    None => {
                        let ended = Ended::Exited;
                        self.push(
                            Lane::Local(h),
                            host(h, Input::ControlClosed { agent, ended }),
                        );
                    }
                },
                Output::WriteControl { agent, frame } => self.write_control(h, agent, frame),
                Output::SpawnAgent { spec, .. } => self.spawn_process(h, *spec),
                Output::Job { id, op } => {
                    self.push(Lane::Job(h, id), Delivery::Job(h, id, Dbg(op)));
                }
                // The driver's ring starts at the state; its word follows
                // at once, as the driver feeds it to its own core.
                Output::Checkpoint { .. } => {
                    self.push(Lane::Local(h), host(h, Input::Checkpointed));
                }
                Output::Dump { reason } => {
                    let name = self.name(h).to_owned();
                    self.fail(Failure::Dump(format!(
                        "{name}'s core asked for a dump: {reason:?}"
                    )));
                }
                // Channels are the links' own here: a call is carried
                // whenever its link is up. Pairing, relays and the shell's
                // bookkeeping have no part in a net of cores.
                Output::OpenChannel { .. }
                | Output::ChannelRefused { .. }
                | Output::CloseChannel { .. }
                | Output::DropRoute { .. }
                | Output::Dial { .. }
                | Output::NextRoute { .. }
                | Output::CloseLinks { .. }
                | Output::LinkControl { .. }
                | Output::SendFile { .. }
                | Output::ForgetAgent { .. }
                | Output::Ingested { .. }
                | Output::Drained { .. }
                | Output::Retained(_)
                | Output::Shell(_)
                | Output::Pair { .. }
                | Output::PairPending { .. }
                | Output::PairEnded { .. }
                | Output::RefuseResumed { .. }
                | Output::PeerAddresses { .. }
                | Output::ObservedAddress { .. } => {}
            }
        }
    }

    /// A frame for the agent's process goes on its connection, or is
    /// dropped when it has none; a message dropped so has no verdict.
    fn write_control(&mut self, h: usize, agent_id: AgentId, frame: CtlFrame) {
        let message = match &frame.of {
            Some(ctl_frame::Of::AgentMessage(envelope)) => Some(envelope.id.clone()),
            _ => None,
        };
        let connected = self
            .process_of(agent_id)
            .and_then(|p| Some((p, self.w.processes[p].conn?)));
        let Some((p, connection)) = connected else {
            if let Some(input_id) = message {
                self.push(
                    Lane::Local(h),
                    host(
                        h,
                        Input::Verdict {
                            agent: agent_id,
                            input_id,
                            verdict: None,
                        },
                    ),
                );
            }
            return;
        };
        if let Some(id) = message {
            self.w.processes[p].written.insert(id);
        }
        self.push(
            Lane::ToAgent(p, connection),
            agent(p, agent_core::Input::Control { connection, frame }),
        );
    }

    /// Runs a job the way the drivers' fakes do; none when its result
    /// waits on something else.
    fn run_job(&mut self, h: usize, id: JobId, op: Job) -> Option<JobResult> {
        Some(match op {
            Job::RemoveDir { path } => {
                let home = agents_home(self.plan.hosts[h].id);
                self.w.hosts[h]
                    .journals
                    .retain(|agent, _| daemon_core::ingest::agent_dir(&home, *agent) != path);
                JobResult::Done
            }
            Job::KillAgent { agent } => match self.process_of(agent) {
                Some(p) => {
                    self.w.processes[p].provider.kill();
                    self.exited(p);
                    JobResult::Done
                }
                None => JobResult::Failed("no process to kill".to_owned()),
            },
            Job::AwaitUnlocked { agent, patience_ms } => {
                if self.process_of(agent).is_none() {
                    JobResult::Done
                } else {
                    let until = self.w.now_ms.saturating_add(patience_ms);
                    self.w.unlocking.push((h, id, agent, until));
                    return None;
                }
            }
            Job::NextSpec {
                agent, bootstrap, ..
            } => JobResult::Spec(self.w.specs.get(&agent).map(|Dbg(spec)| {
                Box::new(wire::AgentSpec {
                    incarnation: spec.incarnation + 1,
                    bootstrap,
                    ..spec.clone()
                })
            })),
            Job::Compare { .. } => JobResult::CompareFailed(GitRefusal::NotARepository),
            Job::MakeWorktree { .. } => JobResult::Failed("no repository here".to_owned()),
            // Nothing reads back what these write: the trust file, the
            // store's main file, blobs and drafts.
            Job::Fsync { .. }
            | Job::WriteBlob { .. }
            | Job::CheckpointWal
            | Job::TeardownHost { .. }
            | Job::RefreshToken { .. }
            | Job::SendPush(_)
            | Job::Upload(_) => JobResult::Done,
        })
    }

    // --- agent processes --------------------------------------------------

    /// Starts an agent's process from its spec: a fresh one for its first
    /// incarnation, one continuing its interpreter for a later one.
    fn spawn_process(&mut self, h: usize, spec: wire::AgentSpec) {
        let Some(agent_id) = AgentId::from_slice(&spec.agent_id) else {
            return self.fail(Failure::Driver(format!(
                "a spec named agent {:?}",
                spec.agent_id
            )));
        };
        if let Some(p) = self.process_of(agent_id) {
            self.w.processes[p].provider.kill();
            self.exited(p);
        }
        self.w.hosts[h].journals.entry(agent_id).or_default();
        let resumed = self
            .w
            .resumes
            .get(&agent_id)
            .filter(|_| spec.incarnation > 1)
            .and_then(|state| interpret::decode_checkpoint(state).ok())
            .map(|state| Resumed {
                state,
                inputs: Vec::new(),
            });
        let mut out = Vec::new();
        let core = Agent::<Kind>::start(&spec, resumed, PRODUCER, self.w.now_ms, &mut out);
        self.w.specs.insert(agent_id, Dbg(spec));
        let p = self.w.processes.len();
        self.w.processes.push(Process {
            host: h,
            agent: agent_id,
            core: Some(Core(core.value())),
            provider: Provider::start(),
            running: true,
            conn: None,
            conns: 0,
            written: BTreeSet::new(),
            relayed: BTreeMap::new(),
            timers: BTreeMap::new(),
        });
        self.w.running.insert(agent_id, p);
        self.carry_out_agent(p, out);
    }

    /// Steps the process's core with one input, the clock first.
    fn feed_agent(&mut self, p: usize, input: agent_core::Input) {
        let now = self.w.now_ms;
        let process = &mut self.w.processes[p];
        if !process.running {
            return;
        }
        let Some(Core(value)) = &process.core else {
            return;
        };
        let mut core = Agent::<Kind>::from_value(value).expect("an agent's value restores");
        let deciding = core.exiting().is_none();
        let at_turn_end =
            matches!(&input, agent_core::Input::Provider(fact) if is_turn_end(&fact.payload));
        let at_turn_end_or_deadline =
            at_turn_end || matches!(input, agent_core::Input::Timer(agent_core::TimerId::Drain));
        // The provider has said what it says by now: a turn end it just
        // said ends a turn that ran until this input.
        let turn_running = at_turn_end || process.provider.turn_running();
        let kind = agent_input(&input);
        let mut out = Vec::new();
        core.step(agent_core::Input::Tick { now_ms: now }, &mut out);
        core.step(input, &mut out);
        let early = core
            .exiting()
            .filter(|cause| {
                deciding && !decided_in_its_time(cause, at_turn_end_or_deadline, turn_running)
            })
            .map(|cause| format!("decided to exit {cause:?} on {kind} while a turn ran"));
        process.core = Some(Core(core.value()));
        if let Some(early) = early {
            let what = format!("{} {early}", self.agent_name(p));
            self.fail(Failure::Broke(EXITS_AT_TURN_END, what));
        }
        self.carry_out_agent(p, out);
    }

    /// Carries out what a process's core asked for, as the agent's driver
    /// does: its journal written inline, its provider handed what it is
    /// given, its frames sent on its connection.
    fn carry_out_agent(&mut self, p: usize, out: Vec<agent_core::Output>) {
        use agent_core::Output as Out;
        for output in out {
            if !self.w.processes[p].running {
                return;
            }
            match output {
                // Nothing reads back the facts ring or the blobs.
                Out::Record(_) | Out::WriteBlob { .. } => {}
                // A process playing a recorded journal writes that instead.
                Out::AppendJournal(_)
                    if self.plan.recorded.contains_key(&self.w.processes[p].agent) => {}
                Out::AppendJournal(step) => {
                    let process = &self.w.processes[p];
                    let journal = self.w.hosts[process.host]
                        .journals
                        .entry(process.agent)
                        .or_default();
                    append(journal, &step);
                }
                Out::WriteProvider(effect) => {
                    let process = &mut self.w.processes[p];
                    let script = self
                        .plan
                        .scripts
                        .get(&process.agent)
                        .map_or(&[][..], Vec::as_slice);
                    process.provider.take(script, effect);
                }
                Out::WriteControl { connection, frame } => {
                    if self.w.processes[p].conn == Some(connection) {
                        self.push(
                            Lane::FromAgent(p, connection),
                            Delivery::Frame(p, connection, Dbg(frame)),
                        );
                    }
                }
                Out::ArmTimer { id, at_ms } => {
                    self.w.lanes.remove(&Lane::AgentTimer(p, id));
                    if at_ms <= self.w.now_ms {
                        self.w.processes[p].timers.remove(&id);
                        self.push(
                            Lane::AgentTimer(p, id),
                            agent(p, agent_core::Input::Timer(id)),
                        );
                    } else {
                        self.w.processes[p].timers.insert(id, at_ms);
                    }
                }
                Out::CancelTimer { id } => {
                    self.w.processes[p].timers.remove(&id);
                    self.w.lanes.remove(&Lane::AgentTimer(p, id));
                }
                Out::Job {
                    id,
                    op: agent_core::AgentJob::ReadGit,
                } => {
                    let result = agent_core::JobResult::Git(None);
                    self.push(
                        Lane::AgentJob(p, id),
                        agent(p, agent_core::Input::JobDone { job: id, result }),
                    );
                }
                Out::Job {
                    op: agent_core::AgentJob::DumpPart { .. },
                    ..
                } => {}
                Out::CloseProvider => self.w.processes[p].provider.close(),
                Out::KillProvider => self.w.processes[p].provider.kill(),
                Out::Exit(cause) => {
                    let process = &self.w.processes[p];
                    let journal = self.w.hosts[process.host].journals.get(&process.agent);
                    let end = journal.and_then(last_end);
                    // A recorded journal is not the core's to end.
                    let own = !self.plan.recorded.contains_key(&process.agent);
                    if own && !cause_on_last_step(&cause, end.as_ref()) {
                        let what = format!(
                            "{} exited {cause:?}, its journal's newest step recording {end:?}",
                            self.agent_name(p)
                        );
                        self.fail(Failure::Broke(EXIT_CAUSE_LAST_STEP, what));
                    }
                    self.exited(p);
                }
                Out::Dump { reason } => {
                    let name = self.agent_name(p);
                    self.fail(Failure::Dump(format!(
                        "{name}'s core asked for a dump: {reason:?}"
                    )));
                }
            }
        }
    }

    /// The process is gone: what it said on its connection reaches its
    /// daemon, then the connection closes, leaving the messages written on
    /// it without a verdict.
    fn exited(&mut self, p: usize) {
        let process = &mut self.w.processes[p];
        if !process.running {
            return;
        }
        process.running = false;
        process.timers.clear();
        let (h, agent_id) = (process.host, process.agent);
        if let Some(Core(value)) = process.core.take() {
            let core = Agent::<Kind>::from_value(&value).expect("an agent's value restores");
            self.w
                .resumes
                .insert(agent_id, core.checkpoint().state.clone());
        }
        if self.w.running.get(&agent_id) == Some(&p) {
            self.w.running.remove(&agent_id);
        }
        for queue in self.w.lanes.values_mut() {
            queue.retain(|delivery| addressed_to_process(delivery) != Some(p));
        }
        self.w.lanes.retain(|_, queue| !queue.is_empty());
        let process = &mut self.w.processes[p];
        match process.conn {
            Some(connection) => {
                let written = std::mem::take(&mut process.written);
                for input_id in written {
                    self.push(
                        Lane::FromAgent(p, connection),
                        host(
                            h,
                            Input::Verdict {
                                agent: agent_id,
                                input_id,
                                verdict: None,
                            },
                        ),
                    );
                }
                self.push(
                    Lane::FromAgent(p, connection),
                    Delivery::Closed(p, connection),
                );
            }
            // Its daemon dials it still, and finds it gone.
            None if self.w.lanes.contains_key(&Lane::Dial(p)) => {}
            None => {
                if self.w.hosts[h].up {
                    let ended = Ended::Exited;
                    self.push(
                        Lane::Local(h),
                        host(
                            h,
                            Input::ControlClosed {
                                agent: agent_id,
                                ended,
                            },
                        ),
                    );
                }
            }
        }
        let unlocked: Vec<(usize, JobId)> = self
            .w
            .unlocking
            .iter()
            .filter(|(.., waits, _)| *waits == agent_id)
            .map(|(host, job, ..)| (*host, *job))
            .collect();
        self.w.unlocking.retain(|(.., waits, _)| *waits != agent_id);
        for (at, job) in unlocked {
            self.push(
                Lane::Job(at, job),
                host(
                    at,
                    Input::JobDone {
                        job,
                        result: JobResult::Done,
                    },
                ),
            );
        }
    }

    /// The daemon's dial reaches the process: it connects, unless it is
    /// connected already, or finds the process gone.
    fn dialled(&mut self, p: usize) {
        let process = &mut self.w.processes[p];
        let (h, agent_id) = (process.host, process.agent);
        if !self.w.hosts[h].up || process.conn.is_some() {
            return;
        }
        if !process.running {
            let ended = Ended::Exited;
            return self.feed(
                h,
                Input::ControlClosed {
                    agent: agent_id,
                    ended,
                },
            );
        }
        process.conns += 1;
        let connection = ConnId(process.conns);
        process.conn = Some(connection);
        self.feed_agent(p, agent_core::Input::DaemonDials { connection });
    }

    /// A frame from the process reaches its daemon, which hears what it
    /// says of the core's: its Hello, its nudges, and its verdicts on the
    /// messages the core wrote. A person's input's verdict is the shell's.
    fn frame(&mut self, p: usize, connection: ConnId, frame: CtlFrame) {
        let process = &mut self.w.processes[p];
        if process.conn != Some(connection) {
            return;
        }
        let (h, agent_id) = (process.host, process.agent);
        match frame.of {
            // As the daemon's driver does, a Hello is followed by a read of
            // the journal: what the process wrote before it dialled was
            // nudged to nobody.
            Some(ctl_frame::Of::Hello(hello)) => {
                self.feed(
                    h,
                    Input::AgentHello {
                        agent: agent_id,
                        hello,
                    },
                );
                self.feed(h, Input::Nudge { agent: agent_id });
            }
            Some(ctl_frame::Of::Nudge(_)) => self.feed(h, Input::Nudge { agent: agent_id }),
            Some(ctl_frame::Of::Reply(wire::Verdict { id, verdict })) => {
                if let Some(waiting) = process.relayed.get_mut(&id) {
                    *waiting = verdict.map(Dbg);
                } else if process.written.remove(&id) {
                    self.feed(
                        h,
                        Input::Verdict {
                            agent: agent_id,
                            input_id: id,
                            verdict,
                        },
                    );
                }
            }
            _ => {}
        }
    }

    fn relay(&mut self, p: usize, input: wire::Input) -> Result<(), String> {
        let process = &mut self.w.processes[p];
        let Some(connection) = process.conn.filter(|_| process.running) else {
            return Err("the agent's process is not connected".to_owned());
        };
        process.relayed.insert(input.input_id.clone(), None);
        let frame = CtlFrame {
            of: Some(ctl_frame::Of::Input(input)),
        };
        self.push(
            Lane::ToAgent(p, connection),
            agent(p, agent_core::Input::Control { connection, frame }),
        );
        Ok(())
    }

    // --- time -------------------------------------------------------------

    fn advance(&mut self, by_ms: u64) {
        self.w.now_ms = self.w.now_ms.saturating_add(by_ms);
        let now = self.w.now_ms;
        let mut due: Vec<(u64, Lane, Delivery)> = Vec::new();
        for (h, state) in self.w.hosts.iter_mut().enumerate() {
            let fired: Vec<(TimerId, u64)> = state
                .timers
                .iter()
                .filter(|(_, at)| **at <= now)
                .map(|(id, at)| (*id, *at))
                .collect();
            for (id, at) in fired {
                state.timers.remove(&id);
                due.push((at, Lane::Timer(h, id), host(h, Input::Timer { id })));
            }
        }
        for (p, process) in self.w.processes.iter_mut().enumerate() {
            let fired: Vec<(agent_core::TimerId, u64)> = process
                .timers
                .iter()
                .filter(|(_, at)| **at <= now)
                .map(|(id, at)| (*id, *at))
                .collect();
            for (id, at) in fired {
                process.timers.remove(&id);
                due.push((
                    at,
                    Lane::AgentTimer(p, id),
                    agent(p, agent_core::Input::Timer(id)),
                ));
            }
        }
        let expired: Vec<(usize, JobId)> = self
            .w
            .unlocking
            .iter()
            .filter(|(.., until)| *until <= now)
            .map(|(h, job, ..)| (*h, *job))
            .collect();
        self.w.unlocking.retain(|(.., until)| *until > now);
        for (h, job) in expired {
            let result = JobResult::Failed("the directory is still locked".to_owned());
            self.push(Lane::Job(h, job), host(h, Input::JobDone { job, result }));
        }
        due.sort_by_key(|(at, lane, _)| (*at, *lane));
        for (_, lane, delivery) in due {
            self.push(lane, delivery);
        }
    }
}

/// An output whose path names an agent's directory under this thread's
/// directory, named under the host's home instead, so what the net holds
/// reads the same on every thread.
fn rehome(output: Output, scratch: &Path, home: &Path) -> Output {
    let moved = |path: PathBuf| match path.strip_prefix(scratch) {
        Ok(rest) => home.join(rest),
        Err(_) => path,
    };
    match output {
        Output::Job {
            id,
            op: Job::RemoveDir { path },
        } => Output::Job {
            id,
            op: Job::RemoveDir { path: moved(path) },
        },
        Output::SpawnAgent { dir, spec } => Output::SpawnAgent {
            dir: moved(dir),
            spec,
        },
        // The core's checkpoint names the directory it was taken in, and
        // nothing here reads the state back.
        Output::Checkpoint { .. } => Output::Checkpoint { state: Vec::new() },
        other => other,
    }
}

/// A create a paired host forwarded, as node's peer edge hands it to the
/// core through the client service: its spec built as a client's create's
/// is, with the parent and the first prompt the request names. None when
/// the request names no agent id.
fn peer_create(
    host: HostId,
    request: wire::CreateAgentRequest,
    now_ms: u64,
) -> Option<ClientRequest> {
    let agent = AgentId::from_slice(&request.agent_id)?;
    let mut spec = super::agent_spec(
        agent,
        host,
        &request.cwd,
        request.parent,
        None,
        super::AGENT_GRACE_MS,
        now_ms,
    );
    spec.bootstrap = request.initial_prompt.map(|prompt| wire::Bootstrap {
        of: Some(wire::bootstrap::Of::Prompt(prompt)),
    });
    Some(ClientRequest::CreateAgent(Box::new(daemon_core::Create {
        agent,
        cwd: request.cwd,
        given: request.name,
        branches: BTreeSet::new(),
        new_worktree: request.new_worktree,
        spec,
    })))
}

/// A message a paired host sent, as node's peer edge hands it to the
/// core: a client call from the agent the sending host stamped as its
/// sender, or from the person when it names none.
pub fn peer_message(envelope: wire::Envelope) -> ClientRequest {
    let from = match envelope.from.clone().and_then(|from| from.value) {
        Some(wire::sender::Value::Agent(sender)) => MessageFrom::Peer(sender),
        _ => MessageFrom::PeerPerson,
    };
    ClientRequest::SendMessage {
        envelope: Box::new(envelope),
        from,
    }
}

/// The host a delivery is for, when it is for a host's daemon.
fn addressed_to(processes: &[Process], delivery: &Delivery) -> Option<usize> {
    match delivery {
        Delivery::Host(h, _) | Delivery::Job(h, ..) => Some(*h),
        Delivery::Dial(p) | Delivery::Frame(p, ..) | Delivery::Closed(p, _) => {
            Some(processes[*p].host)
        }
        Delivery::Agent(..) => None,
    }
}

/// The process a delivery is for, when it is for an agent's process.
fn addressed_to_process(delivery: &Delivery) -> Option<usize> {
    match delivery {
        Delivery::Agent(p, _) => Some(*p),
        _ => None,
    }
}

/// A served call's unary reply, as the peer service carries it.
fn reply_of(response: Response) -> Option<PeerReply> {
    match response {
        Response::Fetched(fetched) => Some(PeerReply::Fetched(fetched)),
        Response::Item(item) => Some(PeerReply::Item(item)),
        Response::Agent(agent) => Some(PeerReply::Agent(*agent)),
        Response::Peer(reply) => Some(*reply),
        _ => None,
    }
}

/// A refusal as the peer service sends it.
/// The wire error a call ending in `error` reaches a caller as, by
/// node's mapping.
pub fn wire_error(error: &CallError) -> wire::Error {
    use wire::ErrorCode;
    let code = match error {
        CallError::NoAgent
        | CallError::NoItem(_)
        | CallError::NoBlob(_)
        | CallError::Message(MessageRefusal::NoAgent | MessageRefusal::Stale) => {
            ErrorCode::NotFound
        }
        CallError::OriginUnreachable | CallError::Message(MessageRefusal::Unavailable) => {
            ErrorCode::Unavailable
        }
        CallError::Message(MessageRefusal::Lost) => ErrorCode::Aborted,
        CallError::Message(MessageRefusal::Rejected(_)) => ErrorCode::FailedPrecondition,
        CallError::NotServed | CallError::Message(MessageRefusal::OtherHost) => {
            ErrorCode::Unimplemented
        }
        _ => ErrorCode::Internal,
    };
    wire::Error {
        code: code as i32,
        message: format!("{error:?}"),
        details: Vec::new(),
    }
}

fn peer_request(request: &PeerRequest) -> &'static str {
    match request {
        PeerRequest::Subscribe(_) => "Subscribe",
        PeerRequest::SubscribeInventory => "SubscribeInventory",
        PeerRequest::Fetch(_) => "Fetch",
        PeerRequest::Get(_) => "Get",
        PeerRequest::SendInput(_) => "SendInput",
        PeerRequest::CreateAgent(_) => "CreateAgent",
        PeerRequest::RenameAgent(_) => "RenameAgent",
        PeerRequest::StopAgent(_) => "StopAgent",
        PeerRequest::ResumeAgent(_) => "ResumeAgent",
        PeerRequest::DeleteAgent(_) => "DeleteAgent",
        PeerRequest::SendMessage(_) => "SendMessage",
        PeerRequest::PutBlob(_) => "PutBlob",
        PeerRequest::GetBlob(_) => "GetBlob",
        PeerRequest::Diff(_) => "Diff",
        PeerRequest::GetCatalogue(_) => "GetCatalogue",
        PeerRequest::ListRepositories(_) => "ListRepositories",
        PeerRequest::Dump(_) => "Dump",
    }
}

fn answer_kind(answer: &Answer) -> String {
    match answer {
        Answer::Reply(_) => "Reply".to_owned(),
        Answer::Session(_) => "Session".to_owned(),
        Answer::Inventory(_) => "Inventory".to_owned(),
        Answer::Ended => "Ended".to_owned(),
        Answer::Failed(PeerFailure::Refused(error)) => {
            format!("Failed: {:?} {}", error.code(), error.message)
        }
        Answer::Failed(failure) => format!("Failed: {failure:?}"),
    }
}

fn timer_kind(id: &TimerId) -> &'static str {
    match id {
        TimerId::Ingest(_) => "Ingest",
        TimerId::Deliveries => "Deliveries",
        TimerId::Lane(_) => "Lane",
        TimerId::Notifications => "Notifications",
        TimerId::Checkpoint => "Checkpoint",
        TimerId::Retry(_) => "Retry",
        TimerId::Refresh(_) => "Refresh",
        TimerId::Follow(_) => "Follow",
        TimerId::Source(..) => "Source",
        TimerId::Start(_) => "Start",
        TimerId::Exit(_) => "Exit",
        TimerId::PairWindow => "PairWindow",
        TimerId::Pairing(_) => "Pairing",
    }
}

fn agent_input(input: &agent_core::Input) -> &'static str {
    use agent_core::Input as In;
    match input {
        In::Provider(_) => "Provider",
        In::ProviderUnstarted { .. } => "ProviderUnstarted",
        In::ProviderExited { .. } => "ProviderExited",
        In::DaemonDials { .. } => "DaemonDials",
        In::DaemonGone { .. } => "DaemonGone",
        In::Control { frame, .. } => match &frame.of {
            Some(ctl_frame::Of::Children(_)) => "Control Children",
            Some(ctl_frame::Of::AgentMessage(_)) => "Control AgentMessage",
            Some(ctl_frame::Of::Stop(_)) => "Control Stop",
            _ => "Control",
        },
        In::Timer(_) => "Timer",
        In::Tick { .. } => "Tick",
        In::WriteFailed { .. } => "WriteFailed",
        In::JobDone { .. } => "JobDone",
    }
}

fn job_kind(job: &Job) -> &'static str {
    match job {
        Job::Fsync { .. } => "Fsync",
        Job::TeardownHost { .. } => "TeardownHost",
        Job::CheckpointWal => "CheckpointWal",
        Job::WriteBlob { .. } => "WriteBlob",
        Job::RefreshToken { .. } => "RefreshToken",
        Job::SendPush(_) => "SendPush",
        Job::NextSpec { .. } => "NextSpec",
        Job::Upload(_) => "Upload",
        Job::KillAgent { .. } => "KillAgent",
        Job::AwaitUnlocked { .. } => "AwaitUnlocked",
        Job::RemoveDir { .. } => "RemoveDir",
        Job::Compare { .. } => "Compare",
        Job::MakeWorktree { .. } => "MakeWorktree",
    }
}

fn frame_kind(frame: &CtlFrame) -> &'static str {
    match &frame.of {
        Some(ctl_frame::Of::Hello(_)) => "Hello",
        Some(ctl_frame::Of::Nudge(_)) => "Nudge",
        Some(ctl_frame::Of::Reply(_)) => "Reply",
        Some(ctl_frame::Of::Dump(_)) => "Dump",
        _ => "another frame",
    }
}

/// This thread's stores and agents' directories, one of each per host it
/// has stepped.
struct Scratch {
    hosts: HashMap<HostId, (Slot, Dir)>,
}

impl Scratch {
    fn of(&mut self, id: HostId) -> &mut (Slot, Dir) {
        self.hosts
            .entry(id)
            .or_insert_with(|| (Slot::new(&id.0), Dir::new()))
    }
}

fn with_scratch<R>(f: impl FnOnce(&mut Scratch) -> R) -> R {
    thread_local! {
        static SCRATCH: RefCell<Scratch> = RefCell::new(Scratch { hosts: HashMap::new() });
    }
    SCRATCH.with(|scratch| f(&mut scratch.borrow_mut()))
}

/// A directory holding one host's agents' journals, laid out only where
/// they differ from what a step starts from.
struct Dir {
    dir: tempfile::TempDir,
    holds: BTreeMap<AgentId, Journal>,
}

impl Dir {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("a directory for agents"),
            holds: BTreeMap::new(),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn lay_out(&mut self, journals: &BTreeMap<AgentId, Journal>) {
        if self.holds == *journals {
            return;
        }
        let agents: BTreeSet<AgentId> = self.holds.keys().chain(journals.keys()).copied().collect();
        for agent in agents {
            let (held, wanted) = (self.holds.get(&agent), journals.get(&agent));
            if held == wanted {
                continue;
            }
            let dir = daemon_core::ingest::journal_dir(self.dir.path(), agent);
            let _ = std::fs::remove_dir_all(&dir);
            if let Some(journal) = wanted {
                std::fs::create_dir_all(&dir).expect("the journal directory");
                for (start, bytes) in journal {
                    std::fs::write(journal::segment_path(&dir, *start), bytes)
                        .expect("a segment is written");
                }
            }
        }
        self.holds = journals.clone();
    }

    /// Drops from `journals` the segments a step reclaimed.
    fn reclaimed(&mut self, journals: &mut BTreeMap<AgentId, Journal>) {
        let mut changed = false;
        for (agent, journal) in journals.iter_mut() {
            if journal.len() <= 1 {
                continue;
            }
            let dir = daemon_core::ingest::journal_dir(self.dir.path(), *agent);
            let left: BTreeSet<u64> = journal::segments(&dir)
                .unwrap_or_default()
                .into_iter()
                .collect();
            let before = journal.len();
            journal.retain(|start, _| left.contains(start));
            changed |= journal.len() != before;
        }
        if changed {
            self.holds = journals.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_journal_appended_in_memory_reads_as_the_writer_writes_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = journal::Writer::open(dir.path(), 64).unwrap();
        let mut journal = Journal::new();
        for at in 0..6u64 {
            let step = wire::Step {
                items: vec![wire::Item {
                    key: format!("item:{at}"),
                    text: "twenty bytes of text".to_owned(),
                    ..wire::Item::default()
                }],
                ..wire::Step::default()
            };
            writer.append(&step).unwrap();
            append(&mut journal, &step);
        }
        drop(writer);
        let written: Journal = journal::segments(dir.path())
            .unwrap()
            .into_iter()
            .map(|start| {
                let bytes = std::fs::read(journal::segment_path(dir.path(), start)).unwrap();
                (start, bytes)
            })
            .collect();
        // The writer's segments are 64 bytes here; the net's rotate at a
        // mebibyte, so compare the bytes the two hold end to end.
        let flat = |journal: &Journal| journal.values().flatten().copied().collect::<Vec<u8>>();
        assert_eq!(flat(&journal), flat(&written));
        assert_eq!(journal_end(&journal), journal::end(dir.path()).unwrap());
    }
}
