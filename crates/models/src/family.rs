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
//! What the world does is the environment too: the parent's create of the
//! child reaches the child's host, carried there unread from the parent's
//! as a call on that host whose caller is the parent, at any point, since
//! a call already on its way may land after the parent was stopped or the
//! link went; a person stops or deletes the parent; a process exits
//! unasked; the child's turn ends; the parent's process accepts a message
//! written to it, writing its acceptance to its journal; the link goes down
//! and comes back; the child's host unpairs from the parent's; and a daemon
//! crashes and starts again, adopting the processes still running.
//!
//! Under the suspension experiment the parent's process may also suspend
//! while nothing is written to it, exiting with its journal saying so, and
//! the parent's machine may restart: every process on it gone, its daemon
//! starting on a new boot, with the parent idle or in a turn as its journal
//! last said. A message to a suspended parent resumes it, its child's
//! report included, and the resumed process takes it as its first input.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::hash::{Hash, Hasher};

use daemon_core::payload::{
    Answer, CarrierKind, ClientRequest, JobResult, PeerFailure, PeerRead, PeerReply, PeerRequest,
    Response, Status, report_envelope,
};
use daemon_core::{
    AgentId, CallId, Create, Daemon, Ended, Fingerprint, HostId, Input, Job, LinkId, Output,
    Settings, SourcePolicy, StartOptions, StepBudget, TimerId,
};
use prost::Message as _;
use store::{AgentKey, Sqlite};
use wire::{DeliverReportRequest, InventoryEvent, Item, Lifecycle, Step, inventory_event};

use crate::held::{Dir, Held, Journal, Slot};
use crate::{Machine, Property};

/// The parent's host and the child's.
const HOSTS: [HostId; 2] = [HostId(*b"parent-host-0000"), HostId(*b"child-host-00000")];
const PARENTS: usize = 0;
const CHILDS: usize = 1;
const PARENT: AgentId = AgentId(*b"parent-000000000");
const CHILD: AgentId = AgentId(*b"child-0000000000");
/// Each agent's journal fits one segment.
const SEGMENT_BYTES: u64 = 1 << 20;
/// The calls the hosts' cores are handed: the parent's create of the
/// child on the child's host, and the person's stop and delete of the
/// parent.
const CREATE: CallId = CallId(1);
const STOP: CallId = CallId(2);
const DELETE: CallId = CallId(3);
const KILL: CallId = CallId(4);
/// The person's resume of the parent, and the parent's message to its child
/// as the child's host takes it: the incarnation its host states, then the
/// message.
const RESUME: CallId = CallId(5);
const HEARD: CallId = CallId(6);
const MESSAGE: CallId = CallId(7);

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
    /// Both cores run the suspension experiment.
    pub suspension: bool,
    /// Times the parent's process suspends.
    pub suspends: u8,
    /// Times the parent's machine restarts.
    pub restarts: u8,
    /// Times a person resumes the parent after it exited.
    pub resumes: u8,
    /// Messages the parent sends its child.
    pub messages: u8,
}

impl FamilyDeliveries {
    /// The bounds `just test` explores exhaustively, sized to a few minutes
    /// on CI's runner. Three things are left to the nightly bounds. Both
    /// processes' unasked exits: the child's alone nearly tripled the
    /// states, and the parent's nearly doubled them. And a timer firing (the
    /// outbox's retry, the follower's backoff, a message's patience), which
    /// tripled them again. Both processes still exit when asked to, the
    /// child's turn end still writes a report, and a link going down and a
    /// daemon crashing are still explored.
    pub const fn ci() -> Self {
        Self {
            turn_ends: 1,
            parent_exits: 0,
            child_exits: 0,
            downs: 1,
            crashes: 1,
            waits: 0,
            suspension: false,
            suspends: 0,
            restarts: 0,
            resumes: 0,
            messages: 0,
        }
    }

    /// The bounds `just test` explores with the parent's later lives: the
    /// person resumes the parent once it has exited, and the parent
    /// messages its child once, in either life. The daemon crash is left
    /// out to keep the run to minutes.
    pub const fn resuming() -> Self {
        Self {
            crashes: 0,
            resumes: 1,
            messages: 1,
            ..Self::ci()
        }
    }

    /// The bounds `just test` explores with the suspension experiment on:
    /// the parent suspends once and its machine restarts once, with the
    /// parent idle or in its turn. The daemon crash is left out, since a
    /// restart is a crash that takes the processes with it, and so is the
    /// link going down on its own: the restart takes the link down and up.
    pub const fn suspending() -> Self {
        Self {
            downs: 0,
            crashes: 0,
            suspension: true,
            suspends: 1,
            restarts: 1,
            ..Self::ci()
        }
    }

    /// The larger bounds a nightly run explores.
    pub const fn nightly() -> Self {
        Self {
            parent_exits: 1,
            child_exits: 1,
            downs: 2,
            waits: 2,
            resumes: 1,
            messages: 1,
            ..Self::ci()
        }
    }
}

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
    /// The incarnation its spec named.
    incarnation: u32,
    /// Messages it took, by envelope id: one written to it again is
    /// answered and goes no further, as the interpreter does at intake.
    taken: BTreeSet<Vec<u8>>,
}

/// One report the child's outbox held.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Report {
    /// The parent incarnation its row was stamped for.
    parent_incarnation: u32,
    /// The parent incarnation that had last engaged the child, by its
    /// create or a message, when the report was written: the one it is
    /// for.
    engaged_by: u32,
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
    killed: bool,
    suspends: u8,
    restarts: u8,
    resumes: u8,
    messages: u8,
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
    /// The child was asked to stop while its parent's row on the parent's
    /// own host was live and open.
    child_stopped_unasked: Option<String>,
    /// A restart of the parent's machine left the parent's row exited when
    /// it was live, and not closed, before.
    exited_by_restart: Option<String>,
    /// The report the suspended parent was resumed with, by envelope id.
    woken_by: Option<Vec<u8>>,
    /// The parent was continued after its machine restarted under its turn.
    continued: bool,
    /// The parent's process took a report for an earlier incarnation.
    took_earlier: bool,
    /// The parent's row on its own host has said closed, or exited by a
    /// stop: the person's close or kill was stored.
    parent_closed: bool,
    /// The parent was started again after the person's close or stop was
    /// stored.
    revived_after_close: Option<String>,
    /// The person asked for the parent's resume and its process has not
    /// started yet.
    person_resuming: bool,
    /// The newest parent incarnation that engaged the child: the one its
    /// create landed under, or that sent it a message since.
    engaged: u32,
    /// The child was asked to stop for an exit of its parent older than the
    /// incarnation that last engaged it.
    stopped_for_an_earlier_life: Option<String>,
    /// The parent's newest incarnation its own host has written, kept once
    /// its row is gone.
    parent_lives: u32,
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
    /// The child's host's DeliverReport calls on their way, encoded.
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
    /// Both cores run the suspension experiment: the model's, fixed.
    suspension: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// The parent's create of the child arrives on the child's host, which
    /// makes the calling parent the child's; its process starts and says
    /// Hello.
    Spawn,
    /// A person stops the parent.
    Stop,
    /// A person deletes the parent.
    Delete,
    /// A person kills the parent, under the suspension experiment: a stop
    /// that leaves its row open.
    Kill,
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
    /// A person resumes the exited parent: its next incarnation, a new run.
    Resume,
    /// The parent sends its child a message, carried to the child's host
    /// over the link, which hears the incarnation the parent's host states
    /// and then the message. The child's process takes it at once, or is
    /// resumed with it.
    Message,
    /// The world's doing: the host's daemon crashes and starts again.
    Crash(usize),
    /// The parent's process suspends: what was written to it and is
    /// unanswered it refuses as exiting, its journal ends saying so, and it
    /// exits. It may have been asked to stop as it did.
    Suspend,
    /// The parent's machine restarts, the parent's journal last saying it
    /// was idle, in its turn or suspending: every process on it is gone,
    /// and its daemon starts on a new boot.
    Restart {
        last: Last,
    },
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

/// What the parent's journal last said when its machine restarted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Last {
    Idle,
    Busy,
    /// Its incarnation ended suspended, and the daemon had not yet seen it
    /// gone.
    Suspending,
    /// The shutdown ended its idle provider first: the incarnation ended
    /// with the provider signalled. One that cut a turn off reads starting
    /// and is continued as a busy one is.
    ProviderGone,
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
            let mut store = Sqlite::open_in_memory(HOSTS[index].0).expect("a store opens");
            settle(&mut store, self.suspension);
            let daemon = Daemon::new(store, options(self.suspension, false));
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
            suspension: self.suspension,
        };
        with_scratch(|scratch| {
            let mut t = Transition::new(scratch, &mut s);
            // The child's host follows the parent's before the parent is
            // made, so its first listing predates the parent: a child
            // spawned before that listing catches up is one the listing
            // must not judge, and its parent's row arrives on the live
            // stream after it.
            t.start(PARENTS, false);
            t.start(CHILDS, false);
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
        // A create refused leaves no child, and the parent may ask again.
        if world && !s.seen.child_created {
            out.push(Action::Spawn);
        }
        if world && !s.used.stopped {
            out.push(Action::Stop);
        }
        if world && !s.used.deleted {
            out.push(Action::Delete);
        }
        if world && self.suspension && !s.used.killed {
            out.push(Action::Kill);
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
        if world && s.used.resumes < self.resumes && !s.parent.running && s.parent_exited() {
            out.push(Action::Resume);
        }
        if world
            && s.used.messages < self.messages
            && s.parent.connected
            && s.link.is_some()
            && s.seen.child_created
            && !s.seen.child_deleted
        {
            out.push(Action::Message);
        }
        if world && s.used.crashes < self.crashes {
            out.push(Action::Crash(PARENTS));
            out.push(Action::Crash(CHILDS));
        }
        // An agent suspends only once it has taken its first input; what
        // reached it as it exits it refuses.
        if world
            && s.used.suspends < self.suspends
            && s.parent.connected
            && s.written.iter().all(|(_, by)| *by != u8::MAX)
        {
            out.push(Action::Suspend);
        }
        if world && s.used.restarts < self.restarts {
            for last in [Last::Idle, Last::Busy, Last::Suspending, Last::ProviderGone] {
                out.push(Action::Restart { last });
            }
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
        let mut properties = self.family_properties();
        if self.resumes > 0 && self.messages > 0 {
            properties.push(Property::sometimes(
                "a parent the person resumed sometimes hears from the child it messaged",
                |_, s: &State| {
                    s.reports
                        .values()
                        .any(|report| report.engaged_by > 1 && report.reached == 1)
                },
            ));
        }
        if self.suspension {
            properties.extend([
                Property::always(
                    "a machine restart leaves no row exited that was live and open before it",
                    |_, s: &State| s.seen.exited_by_restart.is_none(),
                ),
                Property::sometimes(
                    "a suspended parent is sometimes resumed by its child's report and takes it",
                    |_, s: &State| {
                        s.seen.woken_by.as_ref().is_some_and(|id| {
                            s.reports.get(id).is_some_and(|report| report.reached == 1)
                        })
                    },
                ),
                Property::sometimes(
                    "a parent in its turn is sometimes continued after its machine restarts, and \
                     later takes a report sent to its earlier incarnation",
                    |_, s: &State| s.seen.continued && s.seen.took_earlier,
                ),
            ]);
        }
        properties
    }
}

impl FamilyDeliveries {
    fn family_properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("the cores never ask for a dump", |_, s: &State| {
                s.seen.dumped.is_none()
            }),
            Property::always(
                "a parent the person closed or stopped is never started again: once its own host \
                 stored the close or the stop, no message, report or restart resumes it",
                |_, s: &State| s.seen.revived_after_close.is_none(),
            ),
            Property::always(
                "a parent's suspension, or its machine's restart, stops none of its children: the \
                 child is asked to stop only once its parent's row on the parent's own host is \
                 exited, closed or gone, or has moved on to a later life than the one that last \
                 engaged the child",
                |_, s: &State| s.seen.child_stopped_unasked.is_none(),
            ),
            Property::always(
                "a child is never stopped by an exit older than its last engagement: the exit \
                 its host stops it for is of the parent incarnation that last created or messaged \
                 it, or a later one",
                |_, s: &State| s.seen.stopped_for_an_earlier_life.is_none(),
            ),
            Property::always(
                "each report is for the parent incarnation that last engaged the child: its row \
                 is stamped with the incarnation whose create or message the child last heard",
                |_, s: &State| {
                    s.reports
                        .values()
                        .all(|report| report.parent_incarnation == report.engaged_by)
                },
            ),
            Property::always(
                "no child is deleted by a stale listing: a child goes from its host only once its \
                 parent has gone from the parent's own host",
                |_, s: &State| s.seen.stale_delete.is_none(),
            ),
            Property::always(
                "an outbox row is deleted only on accept: a report goes from the child's outbox only \
                 once the parent's process accepted it, or the parent is gone from its host, or has \
                 exited there, or is in a run the report's incarnation is not of, or the child's \
                 host unpaired from it",
                |_, s: &State| s.seen.removed_unaccepted.is_none(),
            ),
            Property::always(
                "each report reaches its parent exactly once: no report is accepted by the parent's \
                 process twice",
                |_, s: &State| s.reports.values().all(|report| report.reached <= 1),
            ),
            Property::eventually(
                "each report reaches its parent exactly once: once the world rests, a report for the \
                 current run of a live parent on a paired, linked host has been accepted by it, \
                 without waiting on the outbox's retry",
                |model, s: &State| {
                    model.at_rest(s)
                        && (s.cut_short()
                            || !s.reachable()
                            || s.parent_ended()
                            || s.reports.values().all(|report| {
                                report.reached >= 1 || !s.of_parents_run(report.engaged_by)
                            }))
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
    ///
    /// The outbox's retry on the child's host does not excuse a report
    /// while the link is up and the parent live, unless some other timer
    /// fired on the path first: a report then goes at once, and one left
    /// for the retry is a report that waited for nothing.
    fn cut_short(&self) -> bool {
        let retry_excused = self.used.waits > 0 || self.link.is_none() || self.parent_ended();
        self.hosts.iter().enumerate().any(|(index, host)| {
            host.timers
                .iter()
                .any(|timer| retry_excused || !(index == CHILDS && *timer == TimerId::Deliveries))
        })
    }

    /// The parent's row on its own host says exited.
    fn parent_exited(&self) -> bool {
        with_scratch(|scratch| {
            scratch.slots[PARENTS].read(&self.hosts[PARENTS].held.store, |store| {
                store
                    .agent(&parent_key())
                    .unwrap()
                    .is_some_and(|row| row.lifecycle == Lifecycle::Exited as i32)
            })
        })
    }

    /// Whether a report for parent incarnation `incarnation` is of the run
    /// the parent's row on its own host is in: from the incarnation the run
    /// began at, as the suspension experiment keeps it, else exactly the
    /// current one.
    fn of_parents_run(&self, incarnation: u32) -> bool {
        with_scratch(|scratch| {
            scratch.slots[PARENTS].read(&self.hosts[PARENTS].held.store, |store| {
                store.agent(&parent_key()).unwrap().is_some_and(|row| {
                    match store.run_start(&PARENT.0).unwrap() {
                        Some(start) => (start..=row.incarnation).contains(&incarnation),
                        None => incarnation == row.incarnation,
                    }
                })
            })
        })
    }

    /// The child's host is paired with the parent's and linked to it.
    fn reachable(&self) -> bool {
        !self.used.unpaired && self.link.is_some()
    }

    /// The parent exited or is deleted, as its own host holds it: a
    /// suspended parent has no process and has not ended.
    fn parent_ended(&self) -> bool {
        self.seen.parent_deleted
            || with_scratch(|scratch| {
                scratch.slots[PARENTS].read(&self.hosts[PARENTS].held.store, |store| {
                    store
                        .agent(&parent_key())
                        .unwrap()
                        .is_none_or(|row| row.lifecycle == Lifecycle::Exited as i32)
                })
            })
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
/// the machine ran slowly; the suspension experiment as the model runs it,
/// and whether the machine booted since the daemon before.
fn options(suspension: bool, rebooted: bool) -> StartOptions {
    StartOptions {
        generation: 1,
        budget: StepBudget::UNLIMITED,
        suspension,
        rebooted,
        ..StartOptions::default()
    }
}

/// The experiments' tables a store opened by its daemon has: the
/// suspension experiment's, when the model runs it.
fn settle(store: &mut Sqlite, suspension: bool) {
    let on: &[&str] = if suspension {
        &[wire::experimental::SUSPENSION.version()]
    } else {
        &[]
    };
    store
        .settle_experiments(on)
        .expect("the experiments' tables are made");
}

fn create(agent: AgentId, name: &str, parent: Option<(HostId, AgentId, u32)>) -> Create {
    Create {
        agent,
        parent_incarnation: parent.map_or(0, |(_, _, incarnation)| incarnation),
        cwd: "/src".into(),
        given: Some(name.into()),
        branches: BTreeSet::new(),
        new_workspace: false,
        workspace: None,
        recent: None,
        spec: wire::AgentSpec {
            agent_id: agent.0.to_vec(),
            kind: wire::Kind::Codex as i32,
            parent: parent.map(|(host, agent, _)| wire::AgentRef {
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
        let before = state.held.store.clone();
        let (out, print) =
            self.scratch.slots[host].step_reading(&mut state.held, input, Daemon::fingerprint);
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
            let row = store.agent(&parent_key()).unwrap();
            if let Some(row) = &row {
                let seen = &mut self.next.seen;
                seen.parent_lives = seen.parent_lives.max(row.incarnation);
            }
            match row {
                None => self.next.seen.parent_deleted = true,
                // Closed, or ended by the person's stop: nothing resumes
                // it from here but the person.
                Some(row)
                    if row.is_closed()
                        || row.lifecycle == Lifecycle::Exited as i32
                            && [wire::ExitCause::Stopped, wire::ExitCause::Killed]
                                .iter()
                                .any(|cause| row.exit_cause == *cause as i32) =>
                {
                    self.next.seen.parent_closed = true;
                }
                Some(_) => {}
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
        // A report is written in the step that ends the child's turn or
        // incarnation: the incarnation that engaged the child last before it
        // is the one it is for.
        let engaged = self.next.seen.engaged;
        for (id, parent_incarnation) in &rows {
            self.next
                .reports
                .entry(id.clone())
                .or_insert(Report {
                    parent_incarnation: *parent_incarnation,
                    engaged_by: engaged,
                    reached: 0,
                })
                .parent_incarnation = *parent_incarnation;
        }
        // Exited on its own host, or in a run the report is not of: nothing
        // there waits for it. The run as the parent's row and its run's
        // start say.
        let parent =
            self.scratch.slots[PARENTS].read(&self.next.hosts[PARENTS].held.store, |store| {
                store.agent(&parent_key()).unwrap().map(|row| {
                    (
                        row.lifecycle == Lifecycle::Exited as i32,
                        store.run_start(&PARENT.0).unwrap(),
                        row.incarnation,
                    )
                })
            });
        let parent_exited = parent.is_some_and(|(exited, _, _)| exited);
        let of_run = |incarnation: u32| {
            parent.is_some_and(|(_, start, current)| match start {
                Some(start) => (start..=current).contains(&incarnation),
                None => incarnation == current,
            })
        };
        for id in &self.next.outbox {
            if rows.contains_key(id) {
                continue;
            }
            let report = self.next.reports.get(id);
            let accepted = report.is_some_and(|r| r.reached > 0);
            let stopped_since =
                parent_exited || report.is_some_and(|report| !of_run(report.engaged_by));
            let seen = &mut self.next.seen;
            if !accepted
                && !stopped_since
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
                        TimerId::Deliveries
                            | TimerId::Follow(_)
                            | TimerId::Lane(_)
                            | TimerId::Grace(_)
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
                        incarnation: spec.incarnation,
                        ..Process::default()
                    };
                    // The person's own resume is the one start a close or
                    // stop allows.
                    if spec.agent_id == PARENT.0
                        && std::mem::take(&mut self.next.seen.person_resuming)
                    {
                        self.next.seen.parent_closed = false;
                    } else if spec.agent_id == PARENT.0 && self.next.seen.parent_closed {
                        self.next
                            .seen
                            .revived_after_close
                            .get_or_insert_with(|| "the closed parent started again".into());
                    }
                    // A resume's first input: a message the process takes
                    // as any written to it, with no verdict to give.
                    match spec.bootstrap.and_then(|bootstrap| bootstrap.of) {
                        Some(wire::bootstrap::Of::AgentMessage(envelope))
                            if spec.agent_id == CHILD.0 =>
                        {
                            self.child_takes(envelope.id, false);
                        }
                        Some(wire::bootstrap::Of::AgentMessage(envelope))
                            if spec.agent_id == PARENT.0 =>
                        {
                            if self.next.reports.contains_key(&envelope.id) {
                                self.next.seen.woken_by = Some(envelope.id.clone());
                            }
                            self.next.written.push((envelope.id, u8::MAX));
                        }
                        Some(wire::bootstrap::Of::Prompt(prompt)) if prompt.continued => {
                            self.next.seen.continued = true;
                        }
                        _ => {}
                    }
                }
                Output::DialAgent { agent } => {
                    let process = self.process(&agent.0);
                    if process.running && !process.connected {
                        process.connected = true;
                        self.feed(host, Input::AgentHello { agent });
                    }
                }
                Output::WriteControl { agent, frame } => {
                    let run = self.next.hosts[host].run;
                    let process = self.process(&agent.0);
                    if !process.connected {
                        continue;
                    }
                    match frame.of {
                        Some(wire::ctl_frame::Of::Stop(_)) => {
                            process.asked_stop = true;
                            if agent == CHILD {
                                self.child_asked_to_stop();
                            }
                        }
                        Some(wire::ctl_frame::Of::AgentMessage(envelope)) if agent == PARENT => {
                            self.next.written.push((envelope.id, run));
                        }
                        Some(wire::ctl_frame::Of::AgentMessage(envelope)) if agent == CHILD => {
                            self.child_takes(envelope.id, true);
                        }
                        _ => {}
                    }
                }
                Output::Job { id, op } => {
                    let result = match op {
                        Job::NextSpec {
                            agent, bootstrap, ..
                        } => {
                            let last = self.scratch.slots[host].read(
                                &self.next.hosts[host].held.store,
                                |store| {
                                    store
                                        .agent(&AgentKey::new(
                                            HOSTS[host].0.to_vec(),
                                            agent.0.to_vec(),
                                        ))
                                        .unwrap()
                                        .map_or(0, |row| row.incarnation)
                                },
                            );
                            JobResult::Spec(Some(Box::new(wire::AgentSpec {
                                agent_id: agent.0.to_vec(),
                                kind: wire::Kind::Codex as i32,
                                incarnation: last + 1,
                                bootstrap,
                                ..wire::AgentSpec::default()
                            })))
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
                _ => {}
            }
        }
    }

    /// The child's host asked the child to stop: only the parent's row on
    /// its own host being exited, closed or gone, or in a life later than
    /// the one that last engaged the child, may have made it; and the exit
    /// the child's host stopped it for is never of an earlier life than
    /// that one.
    fn child_asked_to_stop(&mut self) {
        let engaged = self.next.seen.engaged;
        let parent_live =
            self.scratch.slots[PARENTS].read(&self.next.hosts[PARENTS].held.store, |store| {
                store.agent(&parent_key()).unwrap().is_some_and(|row| {
                    row.lifecycle == Lifecycle::Live as i32
                        && !row.is_closed()
                        && row.incarnation <= engaged
                })
            });
        if parent_live {
            self.next
                .seen
                .child_stopped_unasked
                .get_or_insert_with(|| "the child was asked to stop under a live parent".into());
        }
        let heard_exited_at =
            self.scratch.slots[CHILDS].read(&self.next.hosts[CHILDS].held.store, |store| {
                store
                    .agent(&parent_key())
                    .unwrap()
                    .filter(|row| row.lifecycle == Lifecycle::Exited as i32)
                    .map(|row| row.incarnation)
            });
        let parent_gone = self.next.seen.parent_deleted;
        if !parent_gone && heard_exited_at.is_some_and(|at| at < engaged) {
            self.next
                .seen
                .stopped_for_an_earlier_life
                .get_or_insert_with(|| {
                    format!(
                        "the child was stopped for its parent's exit at {heard_exited_at:?} after \
                         incarnation {engaged} engaged it"
                    )
                });
        }
    }

    /// The child's process takes the message `id` it was written or
    /// resumed with: its journal holds the acceptance, and its daemon is
    /// nudged, then told when it wrote the message on a connection.
    fn child_takes(&mut self, id: Vec<u8>, verdict: bool) {
        self.append(
            CHILDS,
            CHILD,
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
        self.feed(CHILDS, Input::Nudge { agent: CHILD });
        if verdict {
            self.feed(
                CHILDS,
                Input::Verdict {
                    agent: CHILD,
                    input_id: id,
                    verdict: Some(wire::SendInputResponse {
                        of: Some(wire::send_input_response::Of::Accepted(wire::Accepted {
                            queued: false,
                        })),
                    }),
                },
            );
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
        if matches!(request, PeerRequest::DeliverReport(_)) && self.next.used.unpaired {
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
            PeerRequest::FollowAgents => {
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
                        request: PeerRead::FollowAgents,
                    },
                );
            }
            PeerRequest::DeliverReport(report) => {
                self.next.sent.push((call, report.encode_to_vec()));
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
                // The create was made by the parent's newest incarnation,
                // which its host states; it engages the child once made.
                let incarnation = self.next.seen.parent_lives;
                let child = create(CHILD, "child", Some((HOSTS[PARENTS], PARENT, incarnation)));
                self.feed(
                    CHILDS,
                    Input::ClientCall {
                        call: CREATE,
                        request: ClientRequest::CreateAgent(Box::new(child)),
                    },
                );
                self.run();
                if self.next.seen.child_created {
                    let seen = &mut self.next.seen;
                    seen.engaged = seen.engaged.max(incarnation);
                }
            }
            Action::Stop => {
                self.next.used.stopped = true;
                self.feed(
                    PARENTS,
                    Input::ClientCall {
                        call: STOP,
                        request: ClientRequest::SetFiling {
                            agent: PARENT,
                            filing: daemon_core::Filing::Closed,
                        },
                    },
                );
            }
            Action::Kill => {
                self.next.used.killed = true;
                self.feed(
                    PARENTS,
                    Input::ClientCall {
                        call: KILL,
                        request: ClientRequest::KillAgent { agent: PARENT },
                    },
                );
            }
            Action::Delete => {
                self.next.used.deleted = true;
                self.feed(
                    PARENTS,
                    Input::ClientCall {
                        call: DELETE,
                        request: ClientRequest::DeleteAgent {
                            agent: PARENT,
                            discard: false,
                        },
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
                let (call, report) = self.next.sent.remove(0);
                let report =
                    DeliverReportRequest::decode(report.as_slice()).expect("a report decodes");
                // The parent's driver hands the report on as a message
                // from the child, of the host the link authenticated.
                let envelope = report_envelope(&HOSTS[CHILDS].0, &HOSTS[PARENTS].0, report);
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
                        },
                    },
                );
            }
            Action::Reply => {
                let (call, reply) = self.next.replies.remove(0);
                let answer = match reply {
                    // DeliverReport answers with nothing.
                    Reply::Accepted => Answer::Reply(Box::new(PeerReply::Done)),
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
                        if report.parent_incarnation < self.next.parent.incarnation {
                            self.next.seen.took_earlier = true;
                        }
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
                    Input::ChangeTrust(daemon_core::trust::TrustChange::Remove {
                        host: uuid::Uuid::from_bytes(HOSTS[PARENTS].0),
                    }),
                );
            }
            Action::Crash(host) => {
                self.next.used.crashes += 1;
                self.crash(*host);
            }
            Action::Rest => self.next.used.rested = true,
            Action::Resume => {
                self.next.used.resumes += 1;
                self.next.seen.person_resuming = true;
                self.feed(
                    PARENTS,
                    Input::ClientCall {
                        call: RESUME,
                        request: ClientRequest::ResumeAgent(Box::new(daemon_core::Resume {
                            agent: PARENT,
                            bootstrap: None,
                            if_suspended: false,
                        })),
                    },
                );
            }
            Action::Message => {
                self.next.used.messages += 1;
                let incarnation = self.next.parent.incarnation;
                let seen = &mut self.next.seen;
                seen.engaged = seen.engaged.max(incarnation);
                let id = format!("message-from-{incarnation:03}").into_bytes();
                self.feed(
                    CHILDS,
                    Input::ClientCall {
                        call: HEARD,
                        request: ClientRequest::HeardFromCaller {
                            agent: CHILD,
                            caller: (HOSTS[PARENTS], PARENT),
                            incarnation,
                        },
                    },
                );
                let envelope = wire::Envelope {
                    id,
                    context: None,
                    from: Some(wire::Sender {
                        value: Some(wire::sender::Value::Agent(wire::AgentSender {
                            agent: Some(wire::AgentRef {
                                host_id: HOSTS[PARENTS].0.to_vec(),
                                agent_id: PARENT.0.to_vec(),
                            }),
                            name: "lead".into(),
                        })),
                    }),
                    to: Some(wire::AgentRef {
                        host_id: HOSTS[CHILDS].0.to_vec(),
                        agent_id: CHILD.0.to_vec(),
                    }),
                    body: Some(wire::envelope::Body::Message(wire::MessageBody {
                        text: "and the flaky test".into(),
                        attachments: Vec::new(),
                    })),
                };
                self.feed(
                    CHILDS,
                    Input::ClientCall {
                        call: MESSAGE,
                        request: ClientRequest::SendMessage {
                            envelope: Box::new(envelope),
                        },
                    },
                );
            }
            Action::Suspend => {
                self.next.used.suspends += 1;
                let run = self.next.hosts[PARENTS].run;
                for (id, by) in std::mem::take(&mut self.next.written) {
                    if by == run {
                        self.feed(
                            PARENTS,
                            Input::Verdict {
                                agent: PARENT,
                                input_id: id,
                                verdict: Some(wire::SendInputResponse {
                                    of: Some(wire::send_input_response::Of::Rejected(
                                        wire::Rejected {
                                            refusal: wire::Refusal::Exiting as i32,
                                            detail: None,
                                        },
                                    )),
                                }),
                            },
                        );
                    }
                }
                self.append(
                    PARENTS,
                    PARENT,
                    &Step {
                        incarnation_end: Some(wire::IncarnationEnd {
                            cause: wire::ExitCause::Suspended as i32,
                            detail: None,
                        }),
                        ..Default::default()
                    },
                );
                self.next.parent = Process::default();
                self.feed(
                    PARENTS,
                    Input::ControlClosed {
                        agent: PARENT,
                        ended: Ended::Exited,
                    },
                );
            }
            Action::Restart { last } => {
                self.next.used.restarts += 1;
                self.restart(*last);
            }
        }
    }

    /// The parent's machine restarts: its journal last said the parent was
    /// in its turn or idle, suspending, or that the shutdown ended its
    /// provider first, every process on it goes without a word, and
    /// its daemon starts on a new boot. Its link goes with it and comes
    /// back. Whatever the parent's row said live it still says after.
    fn restart(&mut self, last: Last) {
        if self.next.parent.running {
            let step = match last {
                Last::Idle | Last::Busy => Step {
                    snapshot: Some(wire::Snapshot {
                        kind: wire::Kind::Codex as i32,
                        // As its interpreter writes it: Codex resumes its thread.
                        capabilities: Some(Box::new(wire::AgentCapabilities::codex())),
                        status: wire::AgentStatus::in_phase(if last == Last::Busy {
                            wire::Phase::Working
                        } else {
                            wire::Phase::Idle
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Last::Suspending => Step {
                    incarnation_end: Some(wire::IncarnationEnd {
                        cause: wire::ExitCause::Suspended as i32,
                        detail: None,
                    }),
                    ..Default::default()
                },
                // As an interpreter writes it for a provider that was idle.
                Last::ProviderGone => Step {
                    snapshot: Some(wire::Snapshot {
                        kind: wire::Kind::Codex as i32,
                        // As its interpreter writes it: Codex resumes its thread.
                        capabilities: Some(Box::new(wire::AgentCapabilities::codex())),
                        status: wire::AgentStatus::in_phase(wire::Phase::Idle),
                        ..Default::default()
                    }),
                    incarnation_end: Some(wire::IncarnationEnd {
                        cause: wire::ExitCause::ProviderExited as i32,
                        detail: Some(wire::PROVIDER_SIGNALLED.to_owned()),
                    }),
                    ..Default::default()
                },
            };
            self.append(PARENTS, PARENT, &step);
        }
        let lifecycle = |t: &mut Self| {
            t.scratch.slots[PARENTS].read(&t.next.hosts[PARENTS].held.store, |store| {
                store
                    .agent(&parent_key())
                    .unwrap()
                    .map(|row| (row.lifecycle, row.is_closed()))
            })
        };
        // A row the person closed is on its way out already.
        let before = lifecycle(self)
            .filter(|(_, closed)| !closed)
            .map(|(lifecycle, _)| lifecycle);
        let linked = self.next.link.is_some();
        self.link_down(Some(PARENTS));
        self.run();
        self.next.parent = Process::default();
        self.next.written.clear();
        self.start(PARENTS, true);
        if linked {
            self.link_up();
        }
        self.run();
        if before == Some(Lifecycle::Live as i32)
            && lifecycle(self).map(|(lifecycle, _)| lifecycle) == Some(Lifecycle::Exited as i32)
        {
            self.next
                .seen
                .exited_by_restart
                .get_or_insert_with(|| "the parent's row read exited after the restart".into());
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
                lan: false,
                resumed: None,
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
        self.start(host, false);
        if linked {
            self.link_up();
        }
    }

    /// The host's daemon starts over its store: its settings, then what
    /// its start's sweep finds, then the sweep's end; `rebooted` when the
    /// machine booted since the daemon before.
    fn start(&mut self, host: usize, rebooted: bool) {
        let suspension = self.next.suspension;
        let state = &mut self.next.hosts[host];
        state.run += 1;
        state.timers.clear();
        let mut print = None;
        let first = state.run == 1;
        self.scratch.slots[host].start(&mut state.held, |mut store| {
            // The child's host has paired with the parent's before it
            // first starts; from then on the store holds what it trusts.
            if first && host == CHILDS {
                daemon_core::trust::TrustStore::of_hosts([HOSTS[PARENTS]])
                    .write_for_test(&mut store);
            }
            settle(&mut store, suspension);
            let daemon = Daemon::new(store, options(suspension, rebooted));
            print = Some(daemon.fingerprint());
            daemon
        });
        state.print = print.expect("the daemon started");
        self.feed(host, Input::Configure(Settings::default()));
        if host == CHILDS {
            self.feed(host, Input::Policy(SourcePolicy::OnDemand));
        }
        self.feed(host, Input::Started);
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

/// What the parent's driver sends back for a report whose message ended
/// with `status`: the reply, or the error code a refusal travels as.
fn reply_of(status: &Status) -> Reply {
    match status {
        Status::Ok => Reply::Accepted,
        Status::Failed(error) => Reply::Refused(crate::net::wire_error(error).code),
    }
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
        let scratch = scratch.get_or_insert_with(|| {
            let dirs = [Dir::new(), Dir::new()];
            Scratch {
                slots: [
                    Slot::beside(&HOSTS[PARENTS].0, &dirs[PARENTS]),
                    Slot::beside(&HOSTS[CHILDS].0, &dirs[CHILDS]),
                ],
                dirs,
            }
        });
        f(scratch)
    })
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
