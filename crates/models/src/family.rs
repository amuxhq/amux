//! A parent on one host and its child on another: the family rule that
//! stops and deletes the child as the parent's row says, and the outbox
//! that carries the child's reports to the parent.
//!
//! Both hosts are the real `daemon_core::Daemon` over the real `Sqlite`,
//! each held as its checkpoint, its store's dump and its agents' journals
//! as bytes, laid out in a directory of this thread's for each step. The
//! parent's host runs the parent; the child's host trusts it, follows its
//! inventory and runs the child, whose turn ends and exit write the reports
//! its outbox owes the parent. The parent's host is not told to trust the
//! child's: the family rule is judged where the child lives.
//!
//! What the drivers do is the environment: they start the processes the
//! cores spawn and dial, carry the child host's calls to the parent's host
//! and the answers back in order, carry the parent's inventory to the
//! child's host, run every job at once, and fire a timer the core armed
//! for now at once and one armed for later when the model chooses to wait.
//! What the world does is the environment too: a person creates the child,
//! stops or deletes the parent; a process exits unasked; the child's turn
//! ends; the parent's process accepts a message written to it, writing its
//! acceptance to its journal; the link goes down and comes back; the
//! child's host unpairs from the parent's; and a daemon crashes and starts
//! again, adopting the processes still running.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use daemon_core::payload::{
    Answer, CallError, CarrierKind, ClientRequest, JobResult, MessageFrom, MessageRefusal,
    PeerFailure, PeerReply, PeerRequest, Response, Status,
};
use daemon_core::{
    AgentId, CallId, Create, Daemon, Ended, Fingerprint, HostId, Input, Job, LinkId, Output, Route,
    Settings, SourcePolicy, StartOptions, StepBudget, TimerId, Tried,
};
use prost::Message as _;
use store::{AgentKey, Sqlite};
use wire::{ErrorCode, InventoryEvent, Item, Lifecycle, Step, inventory_event};

use crate::held::{Held, Slot};
use crate::{Fault, Host, Machine, Property};

/// The parent's host and the child's.
const HOSTS: [HostId; 2] = [HostId(*b"parent-host-0000"), HostId(*b"child-host-00000")];
const PARENTS: usize = 0;
const CHILDS: usize = 1;
const PARENT: AgentId = AgentId(*b"parent-000000000");
const CHILD: AgentId = AgentId(*b"child-0000000000");
/// Where each host's core is told its agents' directories live whenever it
/// checkpoints, so its checkpoint names the same place on every thread.
const HOMES: [&str; 2] = ["/parent-host", "/child-host"];
/// Each agent's journal fits one segment.
const SEGMENT_BYTES: u64 = 1 << 20;
/// The person's calls: creating the child, stopping and deleting the
/// parent.
const CREATE: CallId = CallId(1);
const STOP: CallId = CallId(2);
const DELETE: CallId = CallId(3);

fn parent_key() -> AgentKey {
    AgentKey::new(HOSTS[PARENTS].0.to_vec(), PARENT.0.to_vec())
}

fn child_key() -> AgentKey {
    AgentKey::new(HOSTS[CHILDS].0.to_vec(), CHILD.0.to_vec())
}

/// The family and its deliveries, within their bounds.
#[derive(Clone, Debug)]
pub struct FamilyDeliveries {
    /// Turns the child ends, all told.
    pub turn_ends: u8,
    /// Times the parent's process exits without being asked to.
    pub parent_exits: u8,
    /// Times the child's process exits without being asked to.
    pub child_exits: u8,
    /// Times the link goes down, all told; it may come back once after each.
    pub downs: u8,
    /// Daemon crashes, all told.
    pub crashes: u8,
    /// Timers armed for later that fire, all told: the outbox's retry, the
    /// follower's backoff and a message's patience.
    pub waits: u8,
}

impl FamilyDeliveries {
    /// The bounds `just test` explores exhaustively: 396,826 states in 74 s
    /// on 4 threads of an M-series Mac. The child's unasked exit is left to
    /// the nightly bounds, as it alone nearly tripled the states; the child
    /// still exits when asked to, and its turn end still writes a report.
    pub const fn ci() -> Self {
        Self {
            turn_ends: 1,
            parent_exits: 1,
            child_exits: 0,
            downs: 1,
            crashes: 1,
            waits: 1,
        }
    }

    /// The larger bounds a nightly run explores.
    pub const fn nightly() -> Self {
        Self {
            child_exits: 1,
            downs: 2,
            waits: 2,
            ..Self::ci()
        }
    }
}

/// An agent's journal: each segment's start offset and its bytes.
type Journal = BTreeMap<u64, Vec<u8>>;

/// One host: its core and store, its agents' journals, and the timers its
/// core armed for later that the model may fire.
#[derive(Clone, Debug)]
struct HostState {
    held: Held,
    /// The core as a checker tells it apart, after its last step.
    print: Fingerprint,
    journals: BTreeMap<AgentId, Journal>,
    timers: BTreeSet<TimerId>,
    /// Starts of the daemon, which tells a verdict written to a process by
    /// an earlier run, whose connection is gone, from one by this run.
    run: u8,
}

/// An agent's process, as the world sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Process {
    running: bool,
    /// Its daemon's connection to it said Hello.
    connected: bool,
    /// The daemon asked it to stop.
    asked_stop: bool,
    /// Messages it took, by envelope id: one written to it again is
    /// answered and goes no further, as the interpreter does at intake.
    taken: BTreeSet<Vec<u8>>,
}

/// One report the child's outbox held.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Report {
    /// The parent incarnation it is for.
    parent_incarnation: u32,
    /// How many times the parent's process accepted it.
    reached: u8,
}

/// A call of the child's host on its way to the parent's host or back.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Reply {
    /// The parent's host accepted the message.
    Accepted,
    /// The parent's host refused it, with the error code its driver sends.
    Refused(i32),
}

/// The child's host's inventory stream on the parent's host.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Following {
    call: CallId,
    /// The events on their way, encoded, in order.
    queue: Vec<Vec<u8>>,
    /// The listing's CaughtUp has been delivered.
    caught_up: bool,
}

/// How much of the bounds a path has used.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Used {
    stopped: bool,
    deleted: bool,
    turn_ends: u8,
    /// Unasked exits, the parent's and the child's.
    exits: [u8; 2],
    downs: u8,
    /// Links that came up after a down.
    ups: u8,
    crashes: u8,
    unpaired: bool,
    waits: u8,
    /// The world has stopped changing.
    rested: bool,
}

/// What the properties judge, recorded as each transition sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Seen {
    /// The parent's row is gone from its host.
    parent_deleted: bool,
    /// The child's row was written on its host.
    child_created: bool,
    /// The child was made while its host had not caught up with the
    /// parent's host since they last connected, or held no row for the
    /// parent yet.
    child_made_unready: bool,
    /// The child's row went from its host.
    child_deleted: bool,
    /// The child's row went while its parent's was still held.
    stale_delete: Option<String>,
    /// An outbox row went that its parent had not accepted, for a parent
    /// that is still there on a host still paired.
    removed_unaccepted: Option<String>,
    /// A report was handed to the parent's host after it was unpaired.
    handed_after_unpair: Option<String>,
    /// A core asked for a dump.
    dumped: Option<String>,
    /// The child's process ended because its host asked it to, its parent
    /// having ended.
    child_followed: bool,
}

#[derive(Clone)]
pub struct State {
    hosts: [HostState; 2],
    parent: Process,
    child: Process,
    /// The link between the hosts, while it is up.
    link: Option<LinkId>,
    links: u64,
    following: Option<Following>,
    /// The child's host's SendMessage calls on their way, encoded.
    sent: Vec<(CallId, Vec<u8>)>,
    /// Calls the parent's host took and has not answered: the child host's
    /// id for each, and the one the parent's driver named it by.
    at_parent: Vec<(CallId, CallId)>,
    replies: Vec<(CallId, Reply)>,
    /// Envelope ids written to the parent's process that it has not
    /// answered, with the run of its daemon that wrote each.
    written: Vec<(Vec<u8>, u8)>,
    /// Every report the child's outbox held, by envelope id.
    reports: BTreeMap<Vec<u8>, Report>,
    /// The child's outbox as it stands.
    outbox: BTreeSet<Vec<u8>>,
    used: Used,
    seen: Seen,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// A person creates the child on its host, under the parent; its
    /// process starts and says Hello.
    Spawn,
    /// A person stops the parent.
    Stop,
    /// A person deletes the parent.
    Delete,
    /// The agent's process ends: asked to, or of its own accord.
    Exit(Who),
    /// The child's turn ends: its journal says so and its daemon is nudged.
    TurnEnd,
    /// The parent's inventory delivers its listing through its CaughtUp to
    /// the child's host.
    ListingCaughtUp,
    /// The parent's inventory delivers its next change after the listing.
    Hear,
    /// The parent's host takes the child host's next call.
    Carry,
    /// The child's host takes the next answer to one of its calls.
    Reply,
    /// The parent's process accepts the oldest message written to it: its
    /// journal holds the acceptance, its daemon is nudged and told.
    Accept,
    /// A timer the host's core armed for later fires.
    Fire(usize, TimerId),
    LinkDown,
    LinkUp,
    /// The child's host unpairs from the parent's.
    Unpair,
    /// The world's doing: a daemon crashes and starts again.
    Fault(Fault),
    /// The world stops changing: nobody creates, stops or deletes, no
    /// process exits unasked, no turn ends, the link stays as it is, the
    /// pairing stays and no daemon crashes. What the cores asked for still
    /// happens, so a path ends once they rest, and liveness is judged there.
    Rest,
}

type HostIdentity<'a> = (
    &'a [u8],
    &'a store::Dump,
    &'a BTreeMap<AgentId, Journal>,
    &'a BTreeSet<TimerId>,
);

impl State {
    /// What tells two states apart: the cores by their fingerprints, the
    /// child host's calls under the ids its fingerprint gives them, the
    /// messages written to the parent's process by whether its daemon's
    /// current run wrote them, and everything else as it is.
    #[expect(clippy::type_complexity)]
    fn identity(
        &self,
    ) -> (
        [HostIdentity<'_>; 2],
        (&Process, &Process, bool),
        (
            Option<(CallId, &Vec<Vec<u8>>, bool)>,
            Vec<(CallId, &Vec<u8>)>,
            Vec<(CallId, CallId)>,
            Vec<(CallId, &Reply)>,
            Vec<(&Vec<u8>, bool)>,
        ),
        (&BTreeMap<Vec<u8>, Report>, &BTreeSet<Vec<u8>>, &Used, &Seen),
    ) {
        let calls = &self.hosts[CHILDS].print.calls;
        let call = |call: &CallId| calls.get(call).copied().unwrap_or(CallId(u64::MAX));
        let host = |index: usize| {
            let host = &self.hosts[index];
            (
                host.print.state.as_slice(),
                &host.held.store,
                &host.journals,
                &host.timers,
            )
        };
        let run = self.hosts[PARENTS].run;
        (
            [host(PARENTS), host(CHILDS)],
            (&self.parent, &self.child, self.link.is_some()),
            (
                self.following.as_ref().map(|following| {
                    (call(&following.call), &following.queue, following.caught_up)
                }),
                self.sent
                    .iter()
                    .map(|(id, envelope)| (call(id), envelope))
                    .collect(),
                self.at_parent
                    .iter()
                    .map(|(id, there)| (call(id), *there))
                    .collect(),
                self.replies
                    .iter()
                    .map(|(id, reply)| (call(id), reply))
                    .collect(),
                self.written
                    .iter()
                    .map(|(id, by)| (id, *by == run))
                    .collect(),
            ),
            (&self.reports, &self.outbox, &self.used, &self.seen),
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

/// One of the two agents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Who {
    Parent,
    Child,
}

impl Machine for FamilyDeliveries {
    type State = State;
    type Action = Action;

    fn init(&self) -> State {
        crate::share_sqlite_across_threads();
        let host = |index: usize| {
            let store = Sqlite::open_in_memory(HOSTS[index].0).expect("a store opens");
            let daemon = Daemon::new(store, options(index));
            let print = daemon.fingerprint();
            let (held, _) = Held::of(daemon);
            HostState {
                held,
                print,
                journals: BTreeMap::new(),
                timers: BTreeSet::new(),
                run: 0,
            }
        };
        let mut s = State {
            hosts: [host(PARENTS), host(CHILDS)],
            parent: Process::default(),
            child: Process::default(),
            link: None,
            links: 0,
            following: None,
            sent: Vec::new(),
            at_parent: Vec::new(),
            replies: Vec::new(),
            written: Vec::new(),
            reports: BTreeMap::new(),
            outbox: BTreeSet::new(),
            used: Used::default(),
            seen: Seen::default(),
        };
        with_scratch(|scratch| {
            let mut t = Transition::new(scratch, &mut s);
            // The child's host follows the parent's before the parent is
            // made, so its first listing predates the parent: a child
            // spawned before that listing catches up is one the listing
            // must not judge, and its parent's row arrives on the live
            // stream after it.
            t.start(PARENTS);
            t.start(CHILDS);
            t.run();
            t.link_up();
            t.run();
            t.feed(
                PARENTS,
                Input::ClientCall {
                    call: CREATE,
                    request: ClientRequest::CreateAgent(Box::new(create(PARENT, "lead", None))),
                },
            );
            t.run();
        });
        assert!(s.parent.connected, "the parent starts");
        assert!(
            s.following.is_some(),
            "the child's host follows the parent's"
        );
        s
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        let world = !s.used.rested;
        if world {
            out.push(Action::Rest);
        }
        // A create refused leaves no child, and the person may try again.
        if world && !s.seen.child_created {
            out.push(Action::Spawn);
        }
        if world && !s.used.stopped {
            out.push(Action::Stop);
        }
        if world && !s.used.deleted {
            out.push(Action::Delete);
        }
        for (who, process, exits) in [
            (Who::Parent, &s.parent, self.parent_exits),
            (Who::Child, &s.child, self.child_exits),
        ] {
            if process.running
                && (process.asked_stop || world && s.used.exits[who as usize] < exits)
            {
                out.push(Action::Exit(who));
            }
        }
        if world && s.child.connected && s.used.turn_ends < self.turn_ends {
            out.push(Action::TurnEnd);
        }
        if let Some(following) = &s.following {
            if !following.caught_up && following.queue.iter().any(|event| is_caught_up(event)) {
                out.push(Action::ListingCaughtUp);
            }
            if following.caught_up && !following.queue.is_empty() {
                out.push(Action::Hear);
            }
        }
        if !s.sent.is_empty() {
            out.push(Action::Carry);
        }
        if !s.replies.is_empty() {
            out.push(Action::Reply);
        }
        if s.parent.running && !s.written.is_empty() {
            out.push(Action::Accept);
        }
        if s.used.waits < self.waits {
            for (index, host) in s.hosts.iter().enumerate() {
                out.extend(host.timers.iter().map(|timer| Action::Fire(index, *timer)));
            }
        }
        match s.link {
            Some(_) if world && s.used.downs < self.downs => out.push(Action::LinkDown),
            None if world && s.used.ups < s.used.downs => out.push(Action::LinkUp),
            _ => {}
        }
        if world && !s.used.unpaired {
            out.push(Action::Unpair);
        }
        if world && s.used.crashes < self.crashes {
            out.push(Action::Fault(Fault::Crash(Host(PARENTS as u8))));
            out.push(Action::Fault(Fault::Crash(Host(CHILDS as u8))));
        }
    }

    fn next(&self, s: &State, action: &Action) -> Option<State> {
        let mut next = s.clone();
        with_scratch(|scratch| {
            let mut t = Transition::new(scratch, &mut next);
            t.take(action);
            t.run();
        });
        Some(next)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("the cores never ask for a dump", |_, s: &State| {
                s.seen.dumped.is_none()
            }),
            Property::always(
                "no child is deleted by a stale listing: a child goes from its host only once its \
                 parent has gone from the parent's own host",
                |_, s: &State| s.seen.stale_delete.is_none(),
            ),
            Property::always(
                "an outbox row is deleted only on accept: a report goes from the child's outbox only \
                 once the parent's process accepted it, or the parent is gone from its host, or the \
                 child's host unpaired from it",
                |_, s: &State| s.seen.removed_unaccepted.is_none(),
            ),
            Property::always(
                "each report reaches its parent exactly once: no report is accepted by the parent's \
                 process twice",
                |_, s: &State| s.reports.values().all(|report| report.reached <= 1),
            ),
            Property::eventually(
                "each report reaches its parent exactly once: once the world rests, a report for the \
                 parent's live incarnation on a paired, linked host has been accepted by it",
                |model, s: &State| {
                    model.at_rest(s)
                        && (s.cut_short()
                            || !s.reachable()
                            || s.parent_ended()
                            || s.reports.values().all(|report| report.reached >= 1))
                },
            ),
            Property::always(
                "nothing is owed after unpair: no report is handed to the parent's host once the \
                 child's host has unpaired from it",
                |_, s: &State| s.seen.handed_after_unpair.is_none(),
            ),
            Property::eventually(
                "nothing is owed after unpair: once the world rests, the outbox of a host that \
                 unpaired holds nothing for the parent",
                |model, s: &State| {
                    model.at_rest(s) && (s.cut_short() || !s.used.unpaired || s.outbox.is_empty())
                },
            ),
            Property::eventually(
                "every descendant of a stopped parent eventually stops on every reachable host: once \
                 the world rests with the parent exited or deleted, a child on a paired, linked host \
                 is not running, unless it was made while its host had not caught up with its \
                 parent's host or held no row for the parent, and its parent's row there says \
                 nothing definitive: an orphan left to the later clean-up of leftover agents",
                |model: &Self, s: &State| {
                    model.at_rest(s)
                        && (s.cut_short()
                            || !s.reachable()
                            || !s.parent_ended()
                            || s.orphaned()
                            || !s.child.running && !s.child_live())
                },
            ),
            Property::sometimes("a child is sometimes deleted", |_, s: &State| {
                s.seen.child_deleted
            }),
            Property::sometimes("a report sometimes reaches its parent", |_, s: &State| {
                s.reports.values().any(|report| report.reached == 1)
            }),
            Property::sometimes(
                "a child sometimes stops because its parent did",
                |_, s: &State| s.seen.child_followed,
            ),
            Property::sometimes(
                "a child made before its host could judge its parent sometimes reports to it: a \
                 spawn while the child's host had not caught up with the parent's host, or held no \
                 row for the parent, is made, and its report reaches the parent",
                |_, s: &State| {
                    s.seen.child_made_unready
                        && s.reports.values().any(|report| report.reached == 1)
                },
            ),
        ]
    }
}

impl FamilyDeliveries {
    /// Nothing more can happen: the path ends here, and what must happen
    /// eventually is judged as it stands, unless it was cut short.
    fn at_rest(&self, s: &State) -> bool {
        let mut enabled = Vec::new();
        self.actions(s, &mut enabled);
        enabled.is_empty()
    }
}

impl State {
    /// A timer is still armed that the bound on waits will not let fire: a
    /// path that ends so was cut short by the bound, not rested, and what
    /// must happen eventually is left unjudged on it.
    fn cut_short(&self) -> bool {
        self.hosts.iter().any(|host| !host.timers.is_empty())
    }

    /// The child's host is paired with the parent's and linked to it.
    fn reachable(&self) -> bool {
        !self.used.unpaired && self.link.is_some()
    }

    /// The parent exited or is deleted, as its own host holds it.
    fn parent_ended(&self) -> bool {
        self.seen.parent_deleted || !self.parent.running
    }

    /// The child was made before its host caught up with the parent's or
    /// held the parent's row, and its host holds no row saying the parent
    /// exited: nothing definitive reached it, and the child is an orphan
    /// the later clean-up collects.
    fn orphaned(&self) -> bool {
        self.seen.child_made_unready
            && !with_scratch(|scratch| {
                scratch.slots[CHILDS].read(&self.hosts[CHILDS].held.store, |store| {
                    store
                        .agent(&parent_key())
                        .unwrap()
                        .is_some_and(|row| row.lifecycle == Lifecycle::Exited as i32)
                })
            })
    }

    /// The child's row on its host says live.
    fn child_live(&self) -> bool {
        with_scratch(|scratch| {
            scratch.slots[CHILDS].read(&self.hosts[CHILDS].held.store, |store| {
                store
                    .agent(&child_key())
                    .unwrap()
                    .is_some_and(|row| row.lifecycle == Lifecycle::Live as i32)
            })
        })
    }
}

/// The options both cores run under: no step budget, since the model is a
/// function of its actions alone and no step may ask for a dump because
/// the machine ran slowly.
fn options(index: usize) -> StartOptions {
    StartOptions {
        generation: 1,
        agents: PathBuf::from(HOMES[index]),
        budget: StepBudget::UNLIMITED,
        ..StartOptions::default()
    }
}

fn create(agent: AgentId, name: &str, parent: Option<(HostId, AgentId)>) -> Create {
    Create {
        agent,
        cwd: "/src".into(),
        given: Some(name.into()),
        branches: BTreeSet::new(),
        new_worktree: false,
        spec: wire::AgentSpec {
            agent_id: agent.0.to_vec(),
            kind: wire::Kind::Codex as i32,
            parent: parent.map(|(host, agent)| wire::AgentRef {
                host_id: host.0.to_vec(),
                agent_id: agent.0.to_vec(),
            }),
            ..wire::AgentSpec::default()
        },
    }
}

fn is_caught_up(event: &[u8]) -> bool {
    matches!(
        InventoryEvent::decode(event).expect("an event decodes").of,
        Some(inventory_event::Of::CaughtUp(_))
    )
}

/// One transition under way: the state it makes, this thread's stores and
/// directories, and the inputs the drivers have yet to feed.
struct Transition<'a> {
    scratch: &'a mut Scratch,
    next: &'a mut State,
    queue: VecDeque<(usize, Input)>,
}

impl<'a> Transition<'a> {
    fn new(scratch: &'a mut Scratch, next: &'a mut State) -> Self {
        Self {
            scratch,
            next,
            queue: VecDeque::new(),
        }
    }

    fn feed(&mut self, host: usize, input: Input) {
        self.queue.push_back((host, input));
    }

    /// Steps every input the drivers have to feed, in order, carrying out
    /// what each step asks for.
    fn run(&mut self) {
        while let Some((host, input)) = self.queue.pop_front() {
            let out = self.step(host, input);
            self.carry_out(host, out);
        }
    }

    fn step(&mut self, host: usize, input: Input) -> Vec<Output> {
        let state = &mut self.next.hosts[host];
        let scratch = &mut self.scratch.dirs[host];
        scratch.lay_out(&state.journals);
        let home = PathBuf::from(HOMES[host]);
        let place = Some((scratch.path(), home.as_path()));
        let before = state.held.store.clone();
        let (out, print) =
            self.scratch.slots[host].step_in(&mut state.held, input, place, Daemon::fingerprint);
        state.print = print;
        if state.held.store != before {
            self.store_changed(host);
        }
        out
    }

    /// What a step that wrote a store did to what the properties judge.
    fn store_changed(&mut self, host: usize) {
        let store = self.scratch.slots[host].store();
        if host == PARENTS {
            if store.agent(&parent_key()).unwrap().is_none() {
                self.next.seen.parent_deleted = true;
            }
            return;
        }
        let held = store.agent(&child_key()).unwrap().is_some();
        let parent_held = store.agent(&parent_key()).unwrap().is_some();
        let ready = self
            .next
            .following
            .as_ref()
            .is_some_and(|following| following.caught_up);
        let seen = &mut self.next.seen;
        if held && !seen.child_created {
            seen.child_made_unready = !ready || !parent_held;
        }
        if held {
            seen.child_created = true;
        } else if seen.child_created && !seen.child_deleted {
            seen.child_deleted = true;
            if !seen.parent_deleted && seen.stale_delete.is_none() {
                seen.stale_delete = Some("the child went while its parent's row was held".into());
            }
        }
        let rows: BTreeMap<Vec<u8>, u32> = store
            .deliveries()
            .unwrap()
            .into_iter()
            .map(|row| {
                (
                    daemon_core::delivery_envelope_id(&row),
                    row.parent_incarnation,
                )
            })
            .collect();
        for (id, parent_incarnation) in &rows {
            self.next
                .reports
                .entry(id.clone())
                .or_insert(Report {
                    parent_incarnation: *parent_incarnation,
                    reached: 0,
                })
                .parent_incarnation = *parent_incarnation;
        }
        for id in &self.next.outbox {
            if rows.contains_key(id) {
                continue;
            }
            let accepted = self.next.reports.get(id).is_some_and(|r| r.reached > 0);
            let seen = &mut self.next.seen;
            if !accepted
                && !seen.parent_deleted
                && !self.next.used.unpaired
                && seen.removed_unaccepted.is_none()
            {
                seen.removed_unaccepted = Some(format!(
                    "report {} went unaccepted",
                    hex(&id[..4.min(id.len())])
                ));
            }
        }
        self.next.outbox = rows.into_keys().collect();
    }

    /// Carries out what a host's core asked for, as its driver does.
    fn carry_out(&mut self, host: usize, out: Vec<Output>) {
        for output in out {
            match output {
                Output::Dump { reason } => {
                    self.next
                        .seen
                        .dumped
                        .get_or_insert_with(|| format!("{reason:?}"));
                }
                Output::ArmTimer { id, at_ms } => {
                    if at_ms == 0 {
                        self.feed(host, Input::Timer { id });
                    } else if matches!(
                        id,
                        TimerId::Deliveries | TimerId::Follow(_) | TimerId::Lane(_)
                    ) {
                        self.next.hosts[host].timers.insert(id);
                    }
                }
                Output::CancelTimer { id } => {
                    self.next.hosts[host].timers.remove(&id);
                }
                Output::SpawnAgent { spec, .. } => {
                    let process = self.process(&spec.agent_id);
                    *process = Process {
                        running: true,
                        ..Process::default()
                    };
                }
                Output::DialAgent { agent } => {
                    let process = self.process(&agent.0);
                    if process.running && !process.connected {
                        process.connected = true;
                        self.feed(
                            host,
                            Input::AgentHello {
                                agent,
                                hello: wire::AgentHello {
                                    agent_id: agent.0.to_vec(),
                                },
                            },
                        );
                    }
                }
                Output::WriteControl { agent, frame } => {
                    let run = self.next.hosts[host].run;
                    let process = self.process(&agent.0);
                    if !process.connected {
                        continue;
                    }
                    match frame.of {
                        Some(wire::ctl_frame::Of::Stop(_)) => process.asked_stop = true,
                        Some(wire::ctl_frame::Of::AgentMessage(envelope)) if agent == PARENT => {
                            self.next.written.push((envelope.id, run));
                        }
                        _ => {}
                    }
                }
                Output::Job { id, op } => {
                    let result = match op {
                        Job::NextSpec { .. } => {
                            JobResult::Spec(Some(Box::new(wire::AgentSpec::default())))
                        }
                        _ => JobResult::Done,
                    };
                    self.feed(host, Input::JobDone { job: id, result });
                }
                Output::Call { call, request, .. } => self.call(call, request),
                Output::HangUp { call } => self.hang_up(call),
                Output::Answer {
                    call,
                    response: Response::Inventory(event),
                } if host == PARENTS => {
                    if let Some(following) = &mut self.next.following
                        && following.call == call
                    {
                        following.queue.push(event.encode_to_vec());
                    }
                }
                Output::EndCall { call, status } if host == PARENTS => {
                    if let Some(at) = self
                        .next
                        .at_parent
                        .iter()
                        .position(|(_, there)| *there == call)
                    {
                        let (call, _) = self.next.at_parent.remove(at);
                        self.next.replies.push((call, reply_of(&status)));
                    }
                }
                Output::Attempt { host: to, route } => {
                    // The driver tries the route: the link carries a channel
                    // while it is up.
                    let tried = match (self.next.link, route) {
                        (Some(link), Route::Direct(on)) if link == on => Tried::Opened,
                        _ => Tried::Failed(daemon_core::payload::ChannelError::LinkClosed),
                    };
                    self.feed(
                        host,
                        Input::AttemptConcluded {
                            host: to,
                            route,
                            tried,
                        },
                    );
                }
                _ => {}
            }
        }
    }

    fn process(&mut self, agent: &[u8]) -> &mut Process {
        if agent == PARENT.0 {
            &mut self.next.parent
        } else {
            &mut self.next.child
        }
    }

    /// A call the child's host makes of the parent's: the inventory is
    /// answered there at once, a report goes on the link; with no link the
    /// call never leaves.
    fn call(&mut self, call: CallId, request: PeerRequest) {
        if matches!(request, PeerRequest::SendMessage(_)) && self.next.used.unpaired {
            self.next
                .seen
                .handed_after_unpair
                .get_or_insert_with(|| "a report was handed over".into());
        }
        if self.next.link.is_none() {
            return self.feed(
                CHILDS,
                Input::PeerAnswer {
                    call,
                    answer: Answer::Failed(PeerFailure::NotSent),
                },
            );
        }
        match request {
            request @ PeerRequest::SubscribeInventory => {
                if let Some(old) = self.next.following.take() {
                    self.feed(PARENTS, Input::CallCancelled { call: old.call });
                }
                self.next.following = Some(Following {
                    call,
                    queue: Vec::new(),
                    caught_up: false,
                });
                self.feed(
                    PARENTS,
                    Input::PeerCall {
                        from: HOSTS[CHILDS],
                        call,
                        request,
                    },
                );
            }
            PeerRequest::SendMessage(envelope) => {
                self.next.sent.push((call, envelope.encode_to_vec()));
            }
            other => unreachable!("the child's host makes no {other:?} call here"),
        }
    }

    /// The child's host lets one of its calls go.
    fn hang_up(&mut self, call: CallId) {
        if self.next.following.as_ref().is_some_and(|f| f.call == call) {
            self.next.following = None;
            self.feed(PARENTS, Input::CallCancelled { call });
        }
        self.next.sent.retain(|(sent, _)| *sent != call);
        if let Some(at) = self
            .next
            .at_parent
            .iter()
            .position(|(sent, _)| *sent == call)
        {
            let (_, there) = self.next.at_parent.remove(at);
            self.feed(PARENTS, Input::CallCancelled { call: there });
        }
        self.next.replies.retain(|(replied, _)| *replied != call);
    }

    /// One action, as the people, the processes, the drivers and the world
    /// carry it out.
    fn take(&mut self, action: &Action) {
        match action {
            Action::Spawn => {
                let child = create(CHILD, "child", Some((HOSTS[PARENTS], PARENT)));
                self.feed(
                    CHILDS,
                    Input::ClientCall {
                        call: CREATE,
                        request: ClientRequest::CreateAgent(Box::new(child)),
                    },
                );
            }
            Action::Stop => {
                self.next.used.stopped = true;
                self.feed(
                    PARENTS,
                    Input::ClientCall {
                        call: STOP,
                        request: ClientRequest::StopAgent { agent: PARENT },
                    },
                );
            }
            Action::Delete => {
                self.next.used.deleted = true;
                self.feed(
                    PARENTS,
                    Input::ClientCall {
                        call: DELETE,
                        request: ClientRequest::DeleteAgent { agent: PARENT },
                    },
                );
            }
            Action::Exit(who) => {
                let (host, agent, process) = match who {
                    Who::Parent => (PARENTS, PARENT, &mut self.next.parent),
                    Who::Child => (CHILDS, CHILD, &mut self.next.child),
                };
                if !process.asked_stop {
                    self.next.used.exits[*who as usize] += 1;
                } else if *who == Who::Child {
                    self.next.seen.child_followed = true;
                }
                *process = Process::default();
                // The connection ends first: every message written on it
                // that the process did not answer has no verdict.
                if *who == Who::Parent {
                    let run = self.next.hosts[PARENTS].run;
                    for (id, by) in std::mem::take(&mut self.next.written) {
                        if by == run {
                            self.feed(
                                PARENTS,
                                Input::Verdict {
                                    agent,
                                    input_id: id,
                                    verdict: None,
                                },
                            );
                        }
                    }
                }
                self.feed(
                    host,
                    Input::ControlClosed {
                        agent,
                        ended: Ended::Exited,
                    },
                );
            }
            Action::TurnEnd => {
                self.next.used.turn_ends += 1;
                let turn = u64::from(self.next.used.turn_ends);
                self.append(
                    CHILDS,
                    CHILD,
                    &Step {
                        turn_end: Some(wire::TurnEnd {
                            turn_id: turn,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                );
                self.feed(CHILDS, Input::Nudge { agent: CHILD });
            }
            Action::ListingCaughtUp | Action::Hear => {
                let Some(following) = &mut self.next.following else {
                    unreachable!("heard without a stream")
                };
                let call = following.call;
                let upto = if *action == Action::Hear {
                    1
                } else {
                    following
                        .queue
                        .iter()
                        .position(|event| is_caught_up(event))
                        .expect("the listing has its CaughtUp")
                        + 1
                };
                following.caught_up = true;
                let events: Vec<Vec<u8>> = following.queue.drain(..upto).collect();
                for event in events {
                    let event = InventoryEvent::decode(event.as_slice()).expect("an event decodes");
                    self.feed(
                        CHILDS,
                        Input::PeerAnswer {
                            call,
                            answer: Answer::Inventory(event),
                        },
                    );
                }
            }
            Action::Carry => {
                let (call, envelope) = self.next.sent.remove(0);
                let envelope =
                    wire::Envelope::decode(envelope.as_slice()).expect("an envelope decodes");
                // The parent's driver takes the sender the child's host
                // stamped.
                let from = match envelope.from.clone().and_then(|from| from.value) {
                    Some(wire::sender::Value::Agent(sender)) => MessageFrom::Peer(sender),
                    _ => MessageFrom::PeerPerson,
                };
                // The parent's driver names the call by the lowest id it has
                // free.
                let there = (10..)
                    .map(CallId)
                    .find(|id| self.next.at_parent.iter().all(|(_, held)| held != id))
                    .expect("an id is free");
                self.next.at_parent.push((call, there));
                self.feed(
                    PARENTS,
                    Input::ClientCall {
                        call: there,
                        request: ClientRequest::SendMessage {
                            envelope: Box::new(envelope),
                            from,
                        },
                    },
                );
            }
            Action::Reply => {
                let (call, reply) = self.next.replies.remove(0);
                let answer = match reply {
                    Reply::Accepted => Answer::Reply(Box::new(PeerReply::SendMessage(
                        wire::SendMessageResponse::default(),
                    ))),
                    Reply::Refused(code) => Answer::Failed(PeerFailure::Refused(wire::Error {
                        code,
                        message: String::new(),
                        details: Vec::new(),
                    })),
                };
                self.feed(CHILDS, Input::PeerAnswer { call, answer });
            }
            Action::Accept => {
                let (id, by) = self.next.written.remove(0);
                let new = self.next.parent.taken.insert(id.clone());
                if new {
                    if let Some(report) = self.next.reports.get_mut(&id) {
                        report.reached += 1;
                    }
                    // Keyed by the message, as the interpreter keys the item
                    // it writes on accepting one.
                    self.append(
                        PARENTS,
                        PARENT,
                        &Step {
                            items: vec![Item {
                                key: format!("message-{}", hex(&id)),
                                input_id: id.clone(),
                                text: "accepted".into(),
                                ..Default::default()
                            }],
                            ..Default::default()
                        },
                    );
                }
                if self.next.parent.connected {
                    if new {
                        self.feed(PARENTS, Input::Nudge { agent: PARENT });
                    }
                    if by == self.next.hosts[PARENTS].run {
                        self.feed(
                            PARENTS,
                            Input::Verdict {
                                agent: PARENT,
                                input_id: id,
                                verdict: Some(wire::SendInputResponse {
                                    of: Some(wire::send_input_response::Of::Accepted(
                                        wire::Accepted { queued: false },
                                    )),
                                }),
                            },
                        );
                    }
                }
            }
            Action::Fire(host, timer) => {
                self.next.used.waits += 1;
                self.next.hosts[*host].timers.remove(timer);
                self.feed(*host, Input::Timer { id: *timer });
            }
            Action::LinkDown => {
                self.next.used.downs += 1;
                self.link_down(None);
            }
            Action::LinkUp => {
                self.next.used.ups += 1;
                self.link_up();
            }
            Action::Unpair => {
                self.next.used.unpaired = true;
                self.feed(
                    CHILDS,
                    Input::TrustRead {
                        store: daemon_core::trust::TrustStore::of_hosts(Vec::new()),
                    },
                );
            }
            Action::Fault(Fault::Crash(Host(host))) => {
                self.next.used.crashes += 1;
                self.crash(usize::from(*host));
            }
            Action::Fault(fault) => unreachable!("the family never offers {fault:?}"),
            Action::Rest => self.next.used.rested = true,
        }
    }

    /// The agent's process appends `step` to its journal.
    fn append(&mut self, host: usize, agent: AgentId, step: &Step) {
        let state = &mut self.next.hosts[host];
        let scratch = &mut self.scratch.dirs[host];
        scratch.lay_out(&state.journals);
        let dir = daemon_core::ingest::journal_dir(scratch.path(), agent);
        let mut writer = journal::Writer::open(&dir, SEGMENT_BYTES).expect("the journal opens");
        writer.append(step).expect("the journal takes a frame");
        drop(writer);
        let journal = scratch.read_back(agent);
        state.journals.insert(agent, journal);
    }

    /// The link goes: every call on it fails, and the parent's host lets go
    /// of the ones it holds.
    fn link_down(&mut self, dying: Option<usize>) {
        let Some(link) = self.next.link.take() else {
            return;
        };
        // A daemon that is dying hears nothing more.
        let tell = |t: &mut Self, host: usize, input: Input| {
            if dying != Some(host) {
                t.feed(host, input);
            }
        };
        tell(
            self,
            CHILDS,
            Input::LinkDown {
                link,
                reason: wire::LinkCloseReason::Unspecified,
            },
        );
        let lost = |call| Input::PeerAnswer {
            call,
            answer: Answer::Failed(PeerFailure::Uncertain),
        };
        if let Some(following) = self.next.following.take() {
            let call = following.call;
            tell(self, PARENTS, Input::CallCancelled { call });
            tell(self, CHILDS, lost(call));
        }
        for (call, _) in std::mem::take(&mut self.next.sent) {
            tell(self, CHILDS, lost(call));
        }
        for (call, there) in std::mem::take(&mut self.next.at_parent) {
            tell(self, PARENTS, Input::CallCancelled { call: there });
            tell(self, CHILDS, lost(call));
        }
        for (call, _) in std::mem::take(&mut self.next.replies) {
            tell(self, CHILDS, lost(call));
        }
    }

    fn link_up(&mut self) {
        self.next.links += 1;
        let link = LinkId(self.next.links);
        self.next.link = Some(link);
        self.feed(
            CHILDS,
            Input::LinkUp {
                link,
                peer: HOSTS[PARENTS],
                kind: CarrierKind::Direct,
            },
        );
    }

    /// The host's daemon dies and starts again over its store and its
    /// agents' journals; the processes still running are adopted. Its link
    /// goes with it and comes back.
    fn crash(&mut self, host: usize) {
        let linked = self.next.link.is_some();
        self.link_down(Some(host));
        self.run();
        if host == PARENTS {
            // Messages the dead daemon wrote stay with the process, which
            // may still accept them; their verdicts reach nobody.
            self.next.parent.connected = false;
        } else {
            self.next.child.connected = false;
        }
        self.start(host);
        if linked {
            self.link_up();
        }
    }

    /// The host's daemon starts over its store: its settings, then what
    /// its start's sweep finds, then the sweep's end.
    fn start(&mut self, host: usize) {
        let state = &mut self.next.hosts[host];
        state.run += 1;
        state.timers.clear();
        let mut print = None;
        self.scratch.slots[host].start(&mut state.held, |store| {
            let daemon = Daemon::new(store, options(host));
            print = Some(daemon.fingerprint());
            daemon
        });
        state.print = print.expect("the daemon started");
        self.feed(host, Input::Configure(Settings::default()));
        if host == CHILDS {
            self.feed(host, Input::Policy(SourcePolicy::OnDemand));
            let hosts = if self.next.used.unpaired {
                Vec::new()
            } else {
                vec![HOSTS[PARENTS]]
            };
            self.feed(
                host,
                Input::TrustRead {
                    store: daemon_core::trust::TrustStore::of_hosts(hosts),
                },
            );
        }
        let (agent, process) = if host == PARENTS {
            (PARENT, &self.next.parent)
        } else {
            (CHILD, &self.next.child)
        };
        let live = self.scratch.slots[host].read(&self.next.hosts[host].held.store, |store| {
            store
                .agent(&AgentKey::new(HOSTS[host].0.to_vec(), agent.0.to_vec()))
                .unwrap()
                .is_some_and(|row| row.lifecycle == Lifecycle::Live as i32)
        });
        if process.running {
            self.feed(host, Input::Adopted { agent });
        } else if live {
            self.feed(
                host,
                Input::ControlClosed {
                    agent,
                    ended: Ended::Away,
                },
            );
        }
        self.feed(host, Input::Swept);
    }
}

/// What the parent's driver sends back for a message call that ended with
/// `status`: the reply, or the error code a refusal travels as.
fn reply_of(status: &Status) -> Reply {
    let code = match status {
        Status::Ok => return Reply::Accepted,
        Status::Failed(CallError::NoAgent)
        | Status::Failed(CallError::Message(MessageRefusal::NoAgent | MessageRefusal::Stale)) => {
            ErrorCode::NotFound
        }
        Status::Failed(CallError::Message(MessageRefusal::Unavailable)) => ErrorCode::Unavailable,
        Status::Failed(CallError::Message(MessageRefusal::Lost)) => ErrorCode::Aborted,
        Status::Failed(CallError::Message(MessageRefusal::Rejected(_))) => {
            ErrorCode::FailedPrecondition
        }
        Status::Failed(CallError::Message(MessageRefusal::OtherHost)) => ErrorCode::Unimplemented,
        Status::Failed(_) => ErrorCode::Internal,
    };
    Reply::Refused(code as i32)
}

/// This thread's stores and agents' directories, one of each per host.
struct Scratch {
    slots: [Slot; 2],
    dirs: [Dir; 2],
}

fn with_scratch<R>(f: impl FnOnce(&mut Scratch) -> R) -> R {
    thread_local! {
        static SCRATCH: RefCell<Option<Scratch>> = const { RefCell::new(None) };
    }
    SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        let scratch = scratch.get_or_insert_with(|| Scratch {
            slots: [Slot::new(&HOSTS[PARENTS].0), Slot::new(&HOSTS[CHILDS].0)],
            dirs: [Dir::new(), Dir::new()],
        });
        f(scratch)
    })
}

/// A directory holding one host's agents' journals, laid out only where
/// they differ from what a transition starts from.
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

    fn read_back(&mut self, agent: AgentId) -> Journal {
        let dir = daemon_core::ingest::journal_dir(self.dir.path(), agent);
        let journal: Journal = journal::segments(&dir)
            .expect("the journal lists")
            .into_iter()
            .map(|start| {
                let bytes =
                    std::fs::read(journal::segment_path(&dir, start)).expect("a segment reads");
                (start, bytes)
            })
            .collect();
        self.holds.insert(agent, journal.clone());
        journal
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rows = with_scratch(|scratch| {
            let mut rows = Vec::new();
            for (index, key) in [(PARENTS, parent_key()), (CHILDS, child_key())] {
                rows.push(
                    scratch.slots[index].read(&self.hosts[index].held.store, |store| {
                        store.agent(&key).unwrap().map(|row| {
                            if row.lifecycle == Lifecycle::Live as i32 {
                                "live"
                            } else {
                                "exited"
                            }
                        })
                    }),
                );
            }
            rows
        });
        let mut debug = f.debug_struct("State");
        debug
            .field("parent_row", &rows[0])
            .field("child_row", &rows[1])
            .field("parent", &self.parent)
            .field("child", &self.child)
            .field("link", &self.link)
            .field(
                "following",
                &self
                    .following
                    .as_ref()
                    .map(|f| (f.queue.len(), f.caught_up)),
            )
            .field("sent", &self.sent.len())
            .field("at_parent", &self.at_parent.len())
            .field("replies", &self.replies)
            .field("written", &self.written.len())
            .field(
                "reports",
                &self
                    .reports
                    .iter()
                    .map(|(id, r)| (hex(&id[..2]), r.reached))
                    .collect::<Vec<_>>(),
            )
            .field("outbox", &self.outbox.len())
            .field("timers", &[&self.hosts[0].timers, &self.hosts[1].timers])
            .field("used", &self.used);
        for (name, value) in [
            ("stale_delete", &self.seen.stale_delete),
            ("removed_unaccepted", &self.seen.removed_unaccepted),
            ("handed_after_unpair", &self.seen.handed_after_unpair),
            ("dumped", &self.seen.dumped),
        ] {
            if let Some(value) = value {
                debug.field(name, value);
            }
        }
        debug.finish()
    }
}
