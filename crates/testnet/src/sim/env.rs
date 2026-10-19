//! What the simulated net is made of, and the drivers between its cores.
//!
//! Every host is a real `daemon_core::Daemon` over a real in-memory store,
//! and every agent process a real `agent_core::Agent` over the test kind's
//! interpreter, held as values in one place. Everything the drivers do
//! between them is a delivery waiting in a lane: a peer's call on its way,
//! the answers coming back, a timer that fell due, a job's result, a dial
//! reaching an agent's process, a control frame, what a provider says.
//! Each lane delivers in order, as one stream or socket does; lanes
//! interleave freely, as streams and sockets do. One step picks a lane by
//! the seed and makes its first delivery, so a delivery is delayed while
//! others go first, deliveries on different lanes reorder, and, when the
//! faults say so, a call's message is lost, which breaks the call as a
//! reset stream does. Nothing else is chosen: one seed is one schedule.
//!
//! Time is the virtual clock's and moves only when the story advances it:
//! a timer armed for now is a delivery at once, one armed for later fires
//! when the clock passes it. Jobs are carried out by fakes: files are
//! written for real under the net's directory, a kill ends the process at
//! once, git reads find no repository. After every step that wrote a
//! host's store, the host's core checks every invariant it knows
//! (`check_all`); a broken one, or a core asking for a dump, fails the net
//! with its seed.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use agent_core::{Agent, ConnId, Resumed};
use daemon_core::payload::{
    Answer, CallError, CarrierKind, ChannelError, ClientRequest, GitRefusal, JobResult,
    MessageRefusal, PeerFailure, PeerReply, PeerRequest, Response, Status,
};
use daemon_core::{
    AgentId, CallId, Daemon, Ended, HostId, Input, Job, JobId, LinkId, Output, Presence, Route,
    Settings, StartOptions, StepBudget, TimerId, Tried,
};
use interpret::testkind::TestKind;
use provider_fakes::script::Step;
use store::Sqlite;
use wire::{CtlFrame, Lifecycle, ctl_frame};

use super::provider::Provider;
use super::rng::Rng;

type Kind = TestKind;

/// Where the virtual clock starts.
pub(crate) const START_MS: u64 = 1_000_000;
/// The version the agents' journals say wrote them.
const PRODUCER: &str = env!("CARGO_PKG_VERSION");
const SEGMENT_BYTES: u64 = 1 << 20;

/// The faults the environment injects, by the seed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Faults {
    /// How often, in a thousand, a call's request or answer is lost on its
    /// way, breaking the call.
    pub lose_per_mille: u16,
}

/// One host: its core while it runs, its store, and what its driver holds.
pub(crate) struct Host {
    pub(crate) name: String,
    pub(crate) id: HostId,
    pub(crate) daemon: Option<Daemon>,
    /// The store while the daemon is down: what reached the drive.
    store: Option<Sqlite>,
    pub(crate) dir: PathBuf,
    pub(crate) generation: u64,
    /// The hosts it trusts: the ones the topology links it to.
    trusted: Vec<HostId>,
    ticked: Option<u64>,
    /// Timers its core armed for later, by when they fall due.
    timers: BTreeMap<TimerId, u64>,
    /// The ids its driver gives calls arriving from clients and peers.
    next_call: u64,
    next_link: u64,
}

impl Host {
    pub(crate) fn agents_dir(&self) -> PathBuf {
        self.dir.join("agents")
    }
}

/// A link between two hosts, and each end's id for it while it is up.
pub(crate) struct Link {
    pub(crate) a: usize,
    pub(crate) b: usize,
    pub(crate) severed: bool,
    live: Option<(LinkId, LinkId)>,
}

/// One agent process: its core, its provider, its journal and its
/// connection to its daemon.
pub(crate) struct Process {
    pub(crate) name: String,
    pub(crate) host: usize,
    pub(crate) agent: AgentId,
    core: Agent<Kind>,
    provider: Provider,
    writer: journal::Writer,
    dir: PathBuf,
    pub(crate) running: bool,
    /// The connection to its daemon, as both ends know it.
    pub(crate) conn: Option<ConnId>,
    conns: u64,
    /// Agent messages the daemon wrote on the connection, awaiting their
    /// verdict.
    written: BTreeSet<Vec<u8>>,
    /// A person's inputs the shell relayed to it, by input id, with the
    /// verdict once it came.
    pub(crate) relayed: BTreeMap<Vec<u8>, Option<wire::SendInputResponse>>,
    timers: BTreeMap<agent_core::TimerId, u64>,
}

/// A call a host made of a peer, as the drivers between them carry it.
struct Outgoing {
    to: usize,
    /// The id the serving host's driver gave it.
    served: CallId,
    stream: bool,
    /// The serving host has stepped the request.
    arrived: bool,
    /// A unary call's reply, held until the call ends.
    reply: Option<PeerReply>,
    /// The reply a call that ends well with nothing said answers with.
    silent: Option<PeerReply>,
}

/// A client's call on a host: what came back, and how it ended.
#[derive(Default)]
pub(crate) struct ClientCall {
    pub(crate) responses: Vec<Response>,
    pub(crate) ended: Option<Status>,
}

/// One ordered way deliveries travel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Lane {
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
    /// What a process's provider says, one fact at a time.
    Provider(usize),
}

enum Delivery {
    Host(usize, Input),
    Agent(usize, agent_core::Input),
    /// The job runs when it is delivered, then its host hears its result.
    Job(usize, JobId, Job),
    /// The daemon's dial reaches the process: it connects, or finds it
    /// gone.
    Dial(usize),
    /// The process's frame reaches its daemon.
    Frame(usize, ConnId, CtlFrame),
    /// The process's end reaches its daemon as the connection closing.
    Closed(usize, ConnId),
    /// The process's provider says its next thing.
    Say(usize),
}

pub(crate) struct Sim {
    pub(crate) seed: u64,
    pub(crate) rng: Rng,
    faults: Faults,
    pub(crate) now_ms: u64,
    pub(crate) hosts: Vec<Host>,
    pub(crate) links: Vec<Link>,
    pub(crate) processes: Vec<Process>,
    /// Each agent's running process.
    running: BTreeMap<AgentId, usize>,
    /// What each agent's provider plays.
    pub(crate) scripts: BTreeMap<AgentId, Vec<Step>>,
    /// Each agent's name, for the trace.
    pub(crate) names: BTreeMap<AgentId, String>,
    /// Each agent's newest spec, for the next incarnation's.
    specs: BTreeMap<AgentId, wire::AgentSpec>,
    /// Where each agent's interpreter stood when its last process ended.
    resumes: BTreeMap<AgentId, Vec<u8>>,
    lanes: BTreeMap<Lane, VecDeque<Delivery>>,
    outgoing: BTreeMap<(usize, CallId), Outgoing>,
    /// Calls a host serves for a peer: the caller and its id for each.
    served: BTreeMap<(usize, CallId), (usize, CallId)>,
    pub(crate) clients: BTreeMap<(usize, CallId), ClientCall>,
    /// Waits for an agent's directory to come free: its host, the job, the
    /// agent, and when its patience runs out.
    unlocking: Vec<(usize, JobId, AgentId, u64)>,
    pub(crate) gates: BTreeSet<String>,
    pub(crate) trace: Vec<String>,
    pub(crate) failure: Option<String>,
    pub(crate) steps: u64,
}

impl Sim {
    pub(crate) fn new(seed: u64, faults: Faults) -> Self {
        Self {
            seed,
            rng: Rng::new(seed),
            faults,
            now_ms: START_MS,
            hosts: Vec::new(),
            links: Vec::new(),
            processes: Vec::new(),
            running: BTreeMap::new(),
            scripts: BTreeMap::new(),
            names: BTreeMap::new(),
            specs: BTreeMap::new(),
            resumes: BTreeMap::new(),
            lanes: BTreeMap::new(),
            outgoing: BTreeMap::new(),
            served: BTreeMap::new(),
            clients: BTreeMap::new(),
            unlocking: Vec::new(),
            gates: BTreeSet::new(),
            trace: Vec::new(),
            failure: None,
            steps: 0,
        }
    }

    pub(crate) fn add_host(&mut self, name: &str, id: HostId, dir: PathBuf, trusted: Vec<HostId>) {
        let store = Sqlite::open_in_memory(id.0).expect("a store opens in memory");
        self.hosts.push(Host {
            name: name.to_owned(),
            id,
            daemon: None,
            store: Some(store),
            dir,
            generation: 1,
            trusted,
            ticked: None,
            timers: BTreeMap::new(),
            next_call: 0,
            next_link: 0,
        });
    }

    pub(crate) fn add_link(&mut self, a: usize, b: usize) -> usize {
        self.links.push(Link {
            a,
            b,
            severed: false,
            live: None,
        });
        self.links.len() - 1
    }

    pub(crate) fn host_index(&self, id: HostId) -> Option<usize> {
        self.hosts.iter().position(|host| host.id == id)
    }

    pub(crate) fn process_of(&self, agent: AgentId) -> Option<usize> {
        self.running.get(&agent).copied()
    }

    fn fail(&mut self, what: String) {
        if self.failure.is_none() {
            self.trace.push(format!("failed: {what}"));
            self.failure = Some(format!("at step {}: {what}", self.steps));
        }
    }

    fn push(&mut self, lane: Lane, delivery: Delivery) {
        self.lanes.entry(lane).or_default().push_back(delivery);
    }

    // --- the scheduler ----------------------------------------------------

    /// Makes one delivery, chosen by the seed; false when nothing waits to
    /// be delivered, or the net has failed.
    pub(crate) fn step(&mut self) -> bool {
        if self.failure.is_some() {
            return false;
        }
        self.refill();
        if self.lanes.is_empty() {
            return false;
        }
        let at = self.rng.below(self.lanes.len());
        let lane = *self.lanes.keys().nth(at).expect("a lane at the index");
        let queue = self.lanes.get_mut(&lane).expect("the lane");
        let delivery = queue.pop_front().expect("no lane is kept empty");
        if queue.is_empty() {
            self.lanes.remove(&lane);
        }
        self.steps += 1;
        if let Lane::Request(caller, call) | Lane::Answer(caller, call) = lane
            && matches!(
                delivery,
                Delivery::Host(_, Input::PeerCall { .. } | Input::PeerAnswer { .. })
            )
            && self.outgoing.contains_key(&(caller, call))
            && self.rng.chance(self.faults.lose_per_mille)
        {
            self.trace.push(format!(
                "{}'s call {} lost a message",
                self.hosts[caller].name, call.0
            ));
            self.break_call(caller, call);
            return true;
        }
        let line = self.describe(&delivery);
        self.trace.push(line);
        self.deliver(lane, delivery);
        true
    }

    /// Gives every provider with something to say a turn to say it.
    fn refill(&mut self) {
        for p in 0..self.processes.len() {
            let process = &self.processes[p];
            if process.running
                && process.provider.can_say(&self.gates)
                && !self.lanes.contains_key(&Lane::Provider(p))
            {
                self.push(Lane::Provider(p), Delivery::Say(p));
            }
        }
    }

    fn describe(&self, delivery: &Delivery) -> String {
        let host = |h: usize| self.hosts[h].name.as_str();
        let agent = |p: usize| self.processes[p].name.as_str();
        match delivery {
            Delivery::Host(h, input) => match input {
                Input::PeerCall { from, request, .. } => format!(
                    "{} {} from {}",
                    host(*h),
                    peer_request(request),
                    self.host_index(*from).map_or("?", host)
                ),
                Input::PeerAnswer { answer, .. } => {
                    format!("{} answered {}", host(*h), answer_kind(answer))
                }
                Input::Timer { id } => format!("{} timer {}", host(*h), timer_kind(id)),
                input => format!("{} {}", host(*h), input.kind()),
            },
            Delivery::Agent(p, input) => format!("{} {}", agent(*p), agent_input(input)),
            Delivery::Job(h, _, op) => format!("{} job {}", host(*h), job_kind(op)),
            Delivery::Dial(p) => format!("{} dialled", agent(*p)),
            Delivery::Frame(p, _, frame) => format!("{} says {}", agent(*p), frame_kind(frame)),
            Delivery::Closed(p, _) => format!("{} closed", agent(*p)),
            Delivery::Say(p) => format!("{} provider", agent(*p)),
        }
    }

    fn deliver(&mut self, lane: Lane, delivery: Delivery) {
        match delivery {
            Delivery::Host(h, input) => {
                match (&input, lane) {
                    (Input::PeerAnswer { call, answer }, _) if answer.is_last() => {
                        self.outgoing.remove(&(h, *call));
                    }
                    (Input::PeerCall { .. }, Lane::Request(caller, call)) => {
                        if let Some(outgoing) = self.outgoing.get_mut(&(caller, call)) {
                            outgoing.arrived = true;
                        }
                    }
                    _ => {}
                }
                self.feed(h, input);
            }
            Delivery::Agent(p, input) => self.feed_agent(p, input),
            Delivery::Job(h, id, op) => {
                if let Some(result) = self.run_job(h, id, op) {
                    self.feed(h, Input::JobDone { job: id, result });
                }
            }
            Delivery::Dial(p) => self.dialled(p),
            Delivery::Frame(p, conn, frame) => self.frame(p, conn, frame),
            Delivery::Closed(p, conn) => {
                let process = &mut self.processes[p];
                if process.conn == Some(conn) {
                    process.conn = None;
                    let (h, agent) = (process.host, process.agent);
                    let ended = Ended::Exited;
                    self.feed(h, Input::ControlClosed { agent, ended });
                }
            }
            Delivery::Say(p) => {
                let gates = &self.gates;
                if let Some(input) = self.processes[p].provider.say(gates) {
                    self.feed_agent(p, input);
                }
            }
        }
    }

    // --- hosts ------------------------------------------------------------

    /// Steps the host's core with one input, as its driver does: the clock
    /// first when it moved, the store checked whole after a write.
    pub(crate) fn feed(&mut self, h: usize, input: Input) {
        let now = self.now_ms;
        let host = &mut self.hosts[h];
        let Some(daemon) = host.daemon.as_mut() else {
            return;
        };
        let mut out = Vec::new();
        if host.ticked != Some(now) {
            host.ticked = Some(now);
            daemon.step(Input::Tick { now_ms: now }, &mut out);
        }
        let before = daemon.store().changes();
        let kind = input.kind();
        daemon.step(input, &mut out);
        if daemon.store().changes() != before
            && let Err(violation) = daemon.check_all()
        {
            let name = host.name.clone();
            self.fail(format!(
                "{name}'s store broke an invariant after {kind}: {violation}"
            ));
        }
        self.carry_out(h, out);
    }

    /// Starts the host's daemon over what its store holds: its settings,
    /// randomness, trust and own entry, then the start's sweep of its
    /// agents, then its links.
    pub(crate) fn start_host(&mut self, h: usize) {
        let entropy = self.rng.bytes::<32>();
        let host = &mut self.hosts[h];
        let Some(store) = host.store.take() else {
            return;
        };
        let options = StartOptions {
            generation: host.generation,
            agents: host.agents_dir(),
            replicas: host.dir.join("replicas"),
            drafts: host.dir.join("drafts"),
            trust: host.dir.join("trust.json"),
            budget: StepBudget::UNLIMITED,
        };
        host.daemon = Some(Daemon::new(store, options));
        host.ticked = None;
        let entry = wire::HostEntry {
            host_id: host.id.0.to_vec(),
            name: host.name.clone(),
            generation: host.generation,
            trust: wire::Trust::Trusted as i32,
            presence: wire::Presence::Online as i32,
            current: Some(true),
            ..wire::HostEntry::default()
        };
        let trusted = host.trusted.clone();
        let own = host.id;
        self.feed(h, Input::Configure(Settings::default()));
        self.feed(h, Input::Entropy(entropy));
        self.feed(
            h,
            Input::TrustRead {
                store: daemon_core::trust::TrustStore::of_hosts(trusted),
            },
        );
        self.feed(h, Input::OwnHost(Box::new(entry)));
        let live: Vec<AgentId> = self.hosts[h]
            .daemon
            .as_ref()
            .and_then(|daemon| daemon.store().agents().ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|row| row.agent.host == own.0 && row.lifecycle == Lifecycle::Live as i32)
            .filter_map(|row| AgentId::from_slice(&row.agent.agent))
            .collect();
        for agent in live {
            if self.running.contains_key(&agent) {
                self.feed(h, Input::Adopted { agent });
                self.feed(h, Input::Nudge { agent });
            } else {
                let ended = Ended::Away;
                self.feed(h, Input::ControlClosed { agent, ended });
            }
        }
        self.feed(h, Input::Swept);
        for l in 0..self.links.len() {
            if self.links[l].a == h || self.links[l].b == h {
                self.link_up(l);
            }
        }
    }

    /// The host's daemon dies: its core and timers go, its links and calls
    /// with it, and its agents see their connections close. Its store stays
    /// as it was.
    pub(crate) fn stop_host(&mut self, h: usize) {
        let Some(daemon) = self.hosts[h].daemon.take() else {
            return;
        };
        let host = &mut self.hosts[h];
        host.store = Some(daemon.into_store());
        host.ticked = None;
        host.timers.clear();
        for l in 0..self.links.len() {
            if self.links[l].a == h || self.links[l].b == h {
                self.link_down(l);
            }
        }
        let calls: Vec<(usize, CallId)> = self
            .outgoing
            .iter()
            .filter(|((caller, _), outgoing)| *caller == h || outgoing.to == h)
            .map(|(key, _)| *key)
            .collect();
        for (caller, call) in calls {
            self.break_call(caller, call);
        }
        self.served
            .retain(|(server, _), (caller, _)| *server != h && *caller != h);
        for client in self
            .clients
            .iter_mut()
            .filter(|((host, _), _)| *host == h)
            .map(|(_, client)| client)
        {
            client
                .ended
                .get_or_insert(Status::Failed(CallError::Peer(PeerFailure::Uncertain)));
        }
        self.unlocking.retain(|(host, ..)| *host != h);
        for p in 0..self.processes.len() {
            let process = &mut self.processes[p];
            if process.host != h {
                continue;
            }
            process.written.clear();
            if let Some(connection) = process.conn.take()
                && process.running
            {
                self.push(
                    Lane::ToAgent(p, connection),
                    Delivery::Agent(p, agent_core::Input::DaemonGone { connection }),
                );
            }
        }
        // What was on its way to the dead daemon reaches nobody; its next
        // run starts from its store alone.
        let processes = &self.processes;
        for queue in self.lanes.values_mut() {
            queue.retain(|delivery| addressed_to(processes, delivery) != Some(h));
        }
        self.lanes.retain(|_, queue| !queue.is_empty());
    }

    // --- links ------------------------------------------------------------

    /// Brings the link up at both ends, if both run and it is not severed.
    pub(crate) fn link_up(&mut self, l: usize) {
        let Link {
            a,
            b,
            severed,
            live,
        } = &self.links[l];
        let (a, b) = (*a, *b);
        if *severed
            || live.is_some()
            || self.hosts[a].daemon.is_none()
            || self.hosts[b].daemon.is_none()
        {
            return;
        }
        let mut id = |h: usize| {
            self.hosts[h].next_link += 1;
            LinkId(self.hosts[h].next_link)
        };
        let ids = (id(a), id(b));
        self.links[l].live = Some(ids);
        for (near, far, link) in [(a, b, ids.0), (b, a, ids.1)] {
            let peer = self.hosts[far].id;
            let kind = CarrierKind::Direct;
            self.push(
                Lane::Link(near, far),
                Delivery::Host(near, Input::LinkUp { link, peer, kind }),
            );
        }
    }

    /// The link goes as a dead connection does: each end that runs hears
    /// it go, and every call between them breaks.
    pub(crate) fn link_down(&mut self, l: usize) {
        let Some(ids) = self.links[l].live.take() else {
            return;
        };
        let (a, b) = (self.links[l].a, self.links[l].b);
        for (near, far, link) in [(a, b, ids.0), (b, a, ids.1)] {
            if self.hosts[near].daemon.is_some() {
                let reason = wire::LinkCloseReason::Unspecified;
                self.push(
                    Lane::Link(near, far),
                    Delivery::Host(near, Input::LinkDown { link, reason }),
                );
            }
        }
        let calls: Vec<(usize, CallId)> = self
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
        self.links.iter().find_map(|link| {
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

    /// Whether `near` reads `far` online over a link of its own.
    pub(crate) fn reads_direct(&self, near: usize, far: usize) -> bool {
        let Some(daemon) = &self.hosts[near].daemon else {
            return false;
        };
        let view = daemon.presence().view(self.hosts[far].id);
        view.presence == Presence::Online && view.route().is_some_and(|route| route.is_direct())
    }

    /// Whether `near` reads `far` offline, or is down itself.
    pub(crate) fn reads_offline(&self, near: usize, far: usize) -> bool {
        self.hosts[near].daemon.as_ref().is_none_or(|daemon| {
            daemon.presence().view(self.hosts[far].id).presence == Presence::Offline
        })
    }

    // --- calls ------------------------------------------------------------

    /// A client's call on the host, stepped at once as its driver hands it
    /// over.
    pub(crate) fn client_call(&mut self, h: usize, request: ClientRequest) -> CallId {
        let host = &mut self.hosts[h];
        host.next_call += 1;
        let call = CallId(host.next_call);
        self.clients.insert((h, call), ClientCall::default());
        self.feed(h, Input::ClientCall { call, request });
        call
    }

    /// The client lets its call go.
    pub(crate) fn cancel_client(&mut self, h: usize, call: CallId) {
        if let Some(client) = self.clients.remove(&(h, call))
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
            .filter(|&to| self.live_link(h, to).is_some() && self.hosts[to].daemon.is_some());
        let Some(to) = reachable else {
            let answer = Answer::Failed(PeerFailure::NotSent);
            return self.push(
                Lane::Answer(h, call),
                Delivery::Host(h, Input::PeerAnswer { call, answer }),
            );
        };
        let server = &mut self.hosts[to];
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
        self.outgoing.insert(
            (h, call),
            Outgoing {
                to,
                served,
                stream,
                arrived: false,
                reply: None,
                silent,
            },
        );
        self.served.insert((to, served), (h, call));
        let from = self.hosts[h].id;
        self.push(
            Lane::Request(h, call),
            Delivery::Host(
                to,
                Input::PeerCall {
                    from,
                    call: served,
                    request,
                },
            ),
        );
    }

    /// The host's core wants nothing more of a call it made.
    fn hang_up(&mut self, h: usize, call: CallId) {
        let Some(outgoing) = self.outgoing.remove(&(h, call)) else {
            return;
        };
        self.lanes.remove(&Lane::Answer(h, call));
        if self
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
                Delivery::Host(outgoing.to, Input::CallCancelled { call: served }),
            );
        } else {
            self.lanes.remove(&Lane::Request(h, call));
        }
    }

    /// A call breaks under its link: the caller hears it is uncertain, and
    /// the host serving it lets it go.
    fn break_call(&mut self, caller: usize, call: CallId) {
        let Some(outgoing) = self.outgoing.remove(&(caller, call)) else {
            return;
        };
        self.lanes.remove(&Lane::Request(caller, call));
        self.lanes.remove(&Lane::Answer(caller, call));
        if self
            .served
            .remove(&(outgoing.to, outgoing.served))
            .is_some()
            && outgoing.arrived
        {
            let served = outgoing.served;
            self.push(
                Lane::Local(outgoing.to),
                Delivery::Host(outgoing.to, Input::CallCancelled { call: served }),
            );
        }
        let answer = Answer::Failed(PeerFailure::Uncertain);
        self.push(
            Lane::Answer(caller, call),
            Delivery::Host(caller, Input::PeerAnswer { call, answer }),
        );
    }

    fn answered(&mut self, h: usize, call: CallId, response: Response) {
        if let Some(&(caller, made)) = self.served.get(&(h, call)) {
            let answer = match response {
                Response::Session(event) => Answer::Session((*event).clone()),
                Response::Inventory(event) => Answer::Inventory((*event).clone()),
                other => {
                    if let Some(outgoing) = self.outgoing.get_mut(&(caller, made)) {
                        outgoing.reply = reply_of(other);
                    }
                    return;
                }
            };
            let call = made;
            self.push(
                Lane::Answer(caller, call),
                Delivery::Host(caller, Input::PeerAnswer { call, answer }),
            );
        } else if let Some(client) = self.clients.get_mut(&(h, call)) {
            client.responses.push(response);
        }
    }

    fn ended(&mut self, h: usize, call: CallId, status: Status) {
        if let Some((caller, made)) = self.served.remove(&(h, call)) {
            let Some(outgoing) = self.outgoing.get(&(caller, made)) else {
                return;
            };
            let answer = match status {
                Status::Ok if outgoing.stream => Answer::Ended,
                Status::Ok => match outgoing.reply.clone().or_else(|| outgoing.silent.clone()) {
                    Some(reply) => Answer::Reply(Box::new(reply)),
                    None => Answer::Failed(PeerFailure::Uncertain),
                },
                Status::Failed(error) => Answer::Failed(PeerFailure::Refused(wire_error(&error))),
            };
            let call = made;
            self.push(
                Lane::Answer(caller, call),
                Delivery::Host(caller, Input::PeerAnswer { call, answer }),
            );
        } else if let Some(client) = self.clients.get_mut(&(h, call)) {
            client.ended = Some(status);
        }
    }

    // --- the drivers ------------------------------------------------------

    /// Carries out what a host's core asked for, as its driver does.
    fn carry_out(&mut self, h: usize, out: Vec<Output>) {
        for output in out {
            match output {
                Output::ArmTimer { id, at_ms } => {
                    self.lanes.remove(&Lane::Timer(h, id));
                    if at_ms <= self.now_ms {
                        self.hosts[h].timers.remove(&id);
                        self.push(Lane::Timer(h, id), Delivery::Host(h, Input::Timer { id }));
                    } else {
                        self.hosts[h].timers.insert(id, at_ms);
                    }
                }
                Output::CancelTimer { id } => {
                    self.hosts[h].timers.remove(&id);
                    self.lanes.remove(&Lane::Timer(h, id));
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
                Output::Attempt { host, route } => {
                    let far = self.host_index(host);
                    let tried = match (route, far.and_then(|far| self.live_link(h, far))) {
                        (Route::Direct(link), Some(live)) if link == live => Tried::Opened,
                        _ => Tried::Failed(ChannelError::LinkClosed),
                    };
                    let lane = far.map_or(Lane::Local(h), |far| Lane::Link(h, far));
                    self.push(
                        lane,
                        Delivery::Host(h, Input::AttemptConcluded { host, route, tried }),
                    );
                }
                Output::DialAgent { agent } => match self.process_of(agent) {
                    Some(p) => self.push(Lane::Dial(p), Delivery::Dial(p)),
                    None => {
                        let ended = Ended::Exited;
                        self.push(
                            Lane::Local(h),
                            Delivery::Host(h, Input::ControlClosed { agent, ended }),
                        );
                    }
                },
                Output::WriteControl { agent, frame } => self.write_control(h, agent, frame),
                Output::SpawnAgent { dir, spec } => self.spawn_process(h, &dir, *spec),
                Output::Job { id, op } => self.push(Lane::Job(h, id), Delivery::Job(h, id, op)),
                Output::Dump { reason } => {
                    let name = self.hosts[h].name.clone();
                    self.fail(format!("{name}'s core asked for a dump: {reason:?}"));
                }
                // Channels are the links' own here: a call is carried
                // whenever its link is up. Pairing, relays and the shell's
                // bookkeeping have no part in a simulated net.
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
                | Output::PairEnded { .. } => {}
            }
        }
    }

    /// A frame for the agent's process goes on its connection, or is
    /// dropped when it has none; a message dropped so has no verdict.
    fn write_control(&mut self, h: usize, agent: AgentId, frame: CtlFrame) {
        let message = match &frame.of {
            Some(ctl_frame::Of::AgentMessage(envelope)) => Some(envelope.id.clone()),
            _ => None,
        };
        let connected = self
            .process_of(agent)
            .and_then(|p| Some((p, self.processes[p].conn?)));
        let Some((p, connection)) = connected else {
            if let Some(input_id) = message {
                self.push(
                    Lane::Local(h),
                    Delivery::Host(
                        h,
                        Input::Verdict {
                            agent,
                            input_id,
                            verdict: None,
                        },
                    ),
                );
            }
            return;
        };
        if let Some(id) = message {
            self.processes[p].written.insert(id);
        }
        self.push(
            Lane::ToAgent(p, connection),
            Delivery::Agent(p, agent_core::Input::Control { connection, frame }),
        );
    }

    /// Runs a job the way the drivers' fakes do; none when its result
    /// waits on something else.
    fn run_job(&mut self, h: usize, id: JobId, op: Job) -> Option<JobResult> {
        let done = |result: std::io::Result<()>| match result {
            Ok(()) => JobResult::Done,
            Err(error) => JobResult::Failed(error.to_string()),
        };
        Some(match op {
            Job::Fsync { path, bytes } => done(write_file(&path, &bytes)),
            Job::WriteBlob { dir, hash, bytes } => {
                done(write_file(&dir.join(interpret::to_hex(&hash)), &bytes))
            }
            Job::RemoveDir { path } => done(match std::fs::remove_dir_all(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            }),
            Job::KillAgent { agent } => match self.process_of(agent) {
                Some(p) => {
                    self.processes[p].provider.kill();
                    self.exited(p);
                    JobResult::Done
                }
                None => JobResult::Failed("no process to kill".to_owned()),
            },
            Job::AwaitUnlocked { agent, patience_ms } => {
                if self.process_of(agent).is_none() {
                    JobResult::Done
                } else {
                    let until = self.now_ms.saturating_add(patience_ms);
                    self.unlocking.push((h, id, agent, until));
                    return None;
                }
            }
            Job::NextSpec {
                agent, bootstrap, ..
            } => JobResult::Spec(self.specs.get(&agent).map(|spec| {
                Box::new(wire::AgentSpec {
                    incarnation: spec.incarnation + 1,
                    bootstrap,
                    ..spec.clone()
                })
            })),
            Job::Compare { .. } => JobResult::CompareFailed(GitRefusal::NotARepository),
            Job::MakeWorktree { .. } => JobResult::Failed("no repository here".to_owned()),
            Job::Checkpoint { .. }
            | Job::TeardownHost { .. }
            | Job::RefreshToken { .. }
            | Job::SendPush(_)
            | Job::Upload(_) => JobResult::Done,
        })
    }

    // --- agent processes --------------------------------------------------

    /// Starts an agent's process from its spec: a fresh one for its first
    /// incarnation, one continuing its interpreter for a later one.
    fn spawn_process(&mut self, h: usize, dir: &Path, spec: wire::AgentSpec) {
        let Some(agent) = AgentId::from_slice(&spec.agent_id) else {
            return self.fail(format!("a spec named agent {:?}", spec.agent_id));
        };
        if let Some(p) = self.process_of(agent) {
            self.processes[p].provider.kill();
            self.exited(p);
        }
        let writer = std::fs::create_dir_all(dir)
            .and_then(|()| journal::Writer::open(dir.join(agent_dir::JOURNAL), SEGMENT_BYTES));
        let writer = match writer {
            Ok(writer) => writer,
            Err(error) => return self.fail(format!("an agent's journal did not open: {error}")),
        };
        let resumed = self
            .resumes
            .get(&agent)
            .filter(|_| spec.incarnation > 1)
            .and_then(|state| interpret::decode_checkpoint(state).ok())
            .map(|state| Resumed {
                state,
                inputs: Vec::new(),
            });
        let mut out = Vec::new();
        let core = Agent::<Kind>::start(&spec, resumed, PRODUCER, self.now_ms, &mut out);
        let steps = self.scripts.get(&agent).cloned().unwrap_or_default();
        let name = self
            .names
            .get(&agent)
            .cloned()
            .unwrap_or_else(|| interpret::to_hex(&agent.0[..4]));
        self.specs.insert(agent, spec);
        let p = self.processes.len();
        self.processes.push(Process {
            name,
            host: h,
            agent,
            core,
            provider: Provider::start(&steps),
            writer,
            dir: dir.to_path_buf(),
            running: true,
            conn: None,
            conns: 0,
            written: BTreeSet::new(),
            relayed: BTreeMap::new(),
            timers: BTreeMap::new(),
        });
        self.running.insert(agent, p);
        self.carry_out_agent(p, out);
    }

    /// Steps the process's core with one input, the clock first.
    fn feed_agent(&mut self, p: usize, input: agent_core::Input) {
        let now = self.now_ms;
        let process = &mut self.processes[p];
        if !process.running {
            return;
        }
        let mut out = Vec::new();
        process
            .core
            .step(agent_core::Input::Tick { now_ms: now }, &mut out);
        process.core.step(input, &mut out);
        self.carry_out_agent(p, out);
    }

    /// Carries out what a process's core asked for, as the agent's driver
    /// does: its journal and blobs written inline, its provider handed
    /// what it is given, its frames sent on its connection.
    fn carry_out_agent(&mut self, p: usize, out: Vec<agent_core::Output>) {
        use agent_core::Output as Out;
        for output in out {
            if !self.processes[p].running {
                return;
            }
            match output {
                Out::Record(_) => {}
                Out::WriteBlob { hash, bytes } => {
                    let dir = self.processes[p].dir.join(agent_dir::BLOBS);
                    let _ = write_file(&dir.join(interpret::to_hex(&hash)), &bytes);
                }
                Out::AppendJournal(step) => {
                    if let Err(error) = self.processes[p].writer.append(&step) {
                        let name = self.processes[p].name.clone();
                        self.fail(format!("{name}'s journal did not take a step: {error}"));
                    }
                }
                Out::WriteProvider(effect) => self.processes[p].provider.take(effect),
                Out::WriteControl { connection, frame } => {
                    if self.processes[p].conn == Some(connection) {
                        self.push(
                            Lane::FromAgent(p, connection),
                            Delivery::Frame(p, connection, frame),
                        );
                    }
                }
                Out::ArmTimer { id, at_ms } => {
                    self.lanes.remove(&Lane::AgentTimer(p, id));
                    if at_ms <= self.now_ms {
                        self.processes[p].timers.remove(&id);
                        self.push(
                            Lane::AgentTimer(p, id),
                            Delivery::Agent(p, agent_core::Input::Timer(id)),
                        );
                    } else {
                        self.processes[p].timers.insert(id, at_ms);
                    }
                }
                Out::CancelTimer { id } => {
                    self.processes[p].timers.remove(&id);
                    self.lanes.remove(&Lane::AgentTimer(p, id));
                }
                Out::Job {
                    id,
                    op: agent_core::AgentJob::ReadGit,
                } => {
                    let result = agent_core::JobResult::Git(None);
                    self.push(
                        Lane::AgentJob(p, id),
                        Delivery::Agent(p, agent_core::Input::JobDone { job: id, result }),
                    );
                }
                Out::Job {
                    op: agent_core::AgentJob::DumpPart { .. },
                    ..
                } => {}
                Out::CloseProvider => self.processes[p].provider.close(),
                Out::KillProvider => self.processes[p].provider.kill(),
                Out::Exit(_) => self.exited(p),
                Out::Dump { reason } => {
                    let name = self.processes[p].name.clone();
                    self.fail(format!("{name}'s core asked for a dump: {reason:?}"));
                }
            }
        }
    }

    /// The process is gone: what it said on its connection reaches its
    /// daemon, then the connection closes, leaving the messages written on
    /// it without a verdict.
    fn exited(&mut self, p: usize) {
        let process = &mut self.processes[p];
        if !process.running {
            return;
        }
        process.running = false;
        process.timers.clear();
        let (h, agent) = (process.host, process.agent);
        self.resumes
            .insert(agent, process.core.checkpoint().state.clone());
        if self.running.get(&agent) == Some(&p) {
            self.running.remove(&agent);
        }
        for queue in self.lanes.values_mut() {
            queue.retain(|delivery| addressed_to_process(delivery) != Some(p));
        }
        self.lanes.retain(|_, queue| !queue.is_empty());
        let process = &mut self.processes[p];
        match process.conn {
            Some(connection) => {
                let written = std::mem::take(&mut process.written);
                for input_id in written {
                    self.push(
                        Lane::FromAgent(p, connection),
                        Delivery::Host(
                            h,
                            Input::Verdict {
                                agent,
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
            None if self.lanes.contains_key(&Lane::Dial(p)) => {}
            None => {
                if self.hosts[h].daemon.is_some() {
                    let ended = Ended::Exited;
                    self.push(
                        Lane::Local(h),
                        Delivery::Host(h, Input::ControlClosed { agent, ended }),
                    );
                }
            }
        }
        let unlocked: Vec<(usize, JobId)> = self
            .unlocking
            .iter()
            .filter(|(.., waits, _)| *waits == agent)
            .map(|(host, job, ..)| (*host, *job))
            .collect();
        self.unlocking.retain(|(.., waits, _)| *waits != agent);
        for (host, job) in unlocked {
            self.push(
                Lane::Job(host, job),
                Delivery::Host(
                    host,
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
        let process = &mut self.processes[p];
        let (h, agent) = (process.host, process.agent);
        if self.hosts[h].daemon.is_none() || process.conn.is_some() {
            return;
        }
        if !process.running {
            let ended = Ended::Exited;
            return self.feed(h, Input::ControlClosed { agent, ended });
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
        let process = &mut self.processes[p];
        if process.conn != Some(connection) {
            return;
        }
        let (h, agent) = (process.host, process.agent);
        match frame.of {
            Some(ctl_frame::Of::Hello(hello)) => self.feed(h, Input::AgentHello { agent, hello }),
            Some(ctl_frame::Of::Nudge(_)) => self.feed(h, Input::Nudge { agent }),
            Some(ctl_frame::Of::Reply(wire::Verdict { id, verdict })) => {
                if let Some(waiting) = process.relayed.get_mut(&id) {
                    *waiting = verdict;
                } else if process.written.remove(&id) {
                    self.feed(
                        h,
                        Input::Verdict {
                            agent,
                            input_id: id,
                            verdict,
                        },
                    );
                }
            }
            _ => {}
        }
    }

    /// Hands a person's input to the process on its connection, as the
    /// shell relays it; its verdict comes back into `relayed`.
    pub(crate) fn relay(&mut self, p: usize, input: wire::Input) -> Result<(), String> {
        let process = &mut self.processes[p];
        let Some(connection) = process.conn.filter(|_| process.running) else {
            return Err("the agent's process is not connected".to_owned());
        };
        process.relayed.insert(input.input_id.clone(), None);
        let frame = CtlFrame {
            of: Some(ctl_frame::Of::Input(input)),
        };
        self.push(
            Lane::ToAgent(p, connection),
            Delivery::Agent(p, agent_core::Input::Control { connection, frame }),
        );
        Ok(())
    }

    // --- time -------------------------------------------------------------

    /// Moves the clock on: every timer it passes falls due, in the order
    /// it was armed for.
    pub(crate) fn advance(&mut self, by_ms: u64) {
        self.now_ms = self.now_ms.saturating_add(by_ms);
        let now = self.now_ms;
        let mut due: Vec<(u64, Lane, Delivery)> = Vec::new();
        for (h, host) in self.hosts.iter_mut().enumerate() {
            let fired: Vec<(TimerId, u64)> = host
                .timers
                .iter()
                .filter(|(_, at)| **at <= now)
                .map(|(id, at)| (*id, *at))
                .collect();
            for (id, at) in fired {
                host.timers.remove(&id);
                due.push((
                    at,
                    Lane::Timer(h, id),
                    Delivery::Host(h, Input::Timer { id }),
                ));
            }
        }
        for (p, process) in self.processes.iter_mut().enumerate() {
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
                    Delivery::Agent(p, agent_core::Input::Timer(id)),
                ));
            }
        }
        let expired: Vec<(usize, JobId)> = self
            .unlocking
            .iter()
            .filter(|(.., until)| *until <= now)
            .map(|(h, job, ..)| (*h, *job))
            .collect();
        self.unlocking.retain(|(.., until)| *until > now);
        for (h, job) in expired {
            let result = JobResult::Failed("the directory is still locked".to_owned());
            self.push(
                Lane::Job(h, job),
                Delivery::Host(h, Input::JobDone { job, result }),
            );
        }
        due.sort_by_key(|(at, lane, _)| (*at, *lane));
        for (_, lane, delivery) in due {
            self.push(lane, delivery);
        }
    }
}

/// The host a delivery is for, when it is for a host's daemon.
fn addressed_to(processes: &[Process], delivery: &Delivery) -> Option<usize> {
    match delivery {
        Delivery::Host(h, _) | Delivery::Job(h, ..) => Some(*h),
        Delivery::Dial(p) | Delivery::Frame(p, ..) | Delivery::Closed(p, _) => {
            Some(processes[*p].host)
        }
        Delivery::Agent(..) | Delivery::Say(_) => None,
    }
}

/// The process a delivery is for, when it is for an agent's process.
fn addressed_to_process(delivery: &Delivery) -> Option<usize> {
    match delivery {
        Delivery::Agent(p, _) | Delivery::Say(p) => Some(*p),
        _ => None,
    }
}

fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
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
fn wire_error(error: &CallError) -> wire::Error {
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

fn answer_kind(answer: &Answer) -> &'static str {
    match answer {
        Answer::Reply(_) => "Reply",
        Answer::Session(_) => "Session",
        Answer::Inventory(_) => "Inventory",
        Answer::Ended => "Ended",
        Answer::Failed(_) => "Failed",
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
        In::Control { .. } => "Control",
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
        Job::Checkpoint { .. } => "Checkpoint",
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
