//! The simulated net: a topology's daemons and agents as cores in one
//! process, driven by a seed, under the verbs a real net offers.
//!
//! [`SimNet`] starts a [`Topology`] as the composed-net model's state,
//! [`models::net::Net`]: every host's real daemon core over an in-memory
//! store and every agent's real agent core over the test kind's
//! interpreter, playing its script through a simulated provider or writing
//! a recorded headless Claude journal as its own, with the
//! links, jobs, clock and processes between them in memory. A simulated
//! net is that model walked by seed: each step makes the delivery the seed
//! picks among those waiting, and the story's verbs are the model's other
//! actions (a link severed, a daemon killed, a person's call, the clock
//! moved on), so the checker and the simulator explore one definition.
//! Nothing moves between a story's verbs;
//! a wait moves the net until its condition holds, and fails at once when
//! nothing is left to deliver, which on a real net is a deadline passing.
//!
//! One seed is one schedule: the same seed gives the same trace of
//! deliveries, and a failure, an invariant broken after a store write or
//! a core asking for a dump, is reported with the seed that reproduces it.
//!
//! What a simulated net does not have, it refuses at start rather than
//! pretending: the relay, the LAN and discovery, process and supervised
//! hosts, real providers, repositories, and agents of a kind other than
//! headless Claude, whose bodies the test kind speaks.

mod rng;
mod shrink;

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Display;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use daemon_core::payload::{ClientRequest, PeerReply, PeerRequest, Response};
use daemon_core::{AgentId, CallId, Create, HostId, Input, Resume};
use models::net::{Lane, Net};
use node::link_log::{LinkEvent, PathKind};
use rng::Rng;
pub use shrink::{Reproduction, same_failure, shrink};
use uuid::Uuid;
use wire::{InventoryEvent, Lifecycle, SessionEvent};

use crate::net::{Ack, NetAgent, NetError, prompt, submission};
use crate::observe::{Feed, InventoryObserver, Observer, ObserverOf, PATIENCE, Pumped, Stuck};
use crate::router::RouterKind;
use crate::topology::{AgentDecl, FakeKind, Provider, Topology, link_key};
use crate::world::World;
use crate::{invariant, probes};

/// How many deliveries one wait may make before it calls the net hung:
/// far more than any story needs, so only a livelock reaches it.
const MOST_STEPS: u64 = 1_000_000;

/// How long a simulated agent outlives its daemon, as on a real net.
const AGENT_GRACE_MS: u32 = 20_000;

/// The faults a simulated net injects, by the seed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Faults {
    /// How often, in a thousand, a call's request or answer is lost on its
    /// way, breaking the call.
    pub lose_per_mille: u16,
}

/// One delivery's choice: the place of the lane that delivered among those
/// waiting, and whether its message was lost instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Choice {
    pub lane: usize,
    pub lost: bool,
}

/// The composed net as the model holds it, and the seed's choices over it.
struct Sim {
    net: Net,
    seed: u64,
    /// Where the schedule's choices come from.
    rng: Rng,
    /// Where host and agent ids and the hosts' entropy come from, apart from
    /// the schedule, so a replayed schedule mints the same ids.
    ids: Rng,
    faults: Faults,
    /// Every choice made, in order: the ring a run replays from.
    ring: Vec<Choice>,
    /// Choices still to replay, when the net replays a ring; once they run
    /// out the net delivers in its own order, the first lane waiting.
    replay: Option<VecDeque<Choice>>,
}

impl Sim {
    /// Makes the one delivery the seed picks among those waiting, or loses
    /// it when the faults say so; false when nothing waits to be delivered,
    /// or the net has failed.
    fn step(&mut self) -> bool {
        if self.net.failure().is_some() {
            return false;
        }
        let lanes = self.net.deliverable();
        if lanes.is_empty() {
            return false;
        }
        let choice = match &mut self.replay {
            Some(replay) => replay.pop_front().unwrap_or(Choice {
                lane: 0,
                lost: false,
            }),
            None => {
                let lane = self.rng.below(lanes.len());
                let lost =
                    self.net.can_lose(lanes[lane]) && self.rng.chance(self.faults.lose_per_mille);
                Choice { lane, lost }
            }
        };
        // A ring cut short or replayed over a changed net may name a place
        // past the lanes waiting now: the last one stands in.
        let at = choice.lane.min(lanes.len() - 1);
        let lost = choice.lost && self.net.can_lose(lanes[at]);
        if lost {
            self.net.lose(lanes[at]);
        } else {
            self.net.deliver(lanes[at]);
        }
        self.ring.push(Choice { lane: at, lost });
        true
    }

    fn is_up(&self, h: usize) -> bool {
        self.net.is_up(h)
    }
}

/// Reads a net's ring, even after the story holding the net failed.
pub struct RingReader(Arc<Mutex<Sim>>);

impl RingReader {
    pub fn ring(&self) -> Vec<Choice> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ring
            .clone()
    }

    /// Every delivery the net made, as [`SimNet::trace`] reads it.
    pub fn trace(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .net
            .trace()
            .to_vec()
    }
}

/// What a simulated net runs under: its seed and its faults.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SimOptions {
    pub seed: u64,
    pub faults: Faults,
}

pub struct SimNet {
    sim: Arc<Mutex<Sim>>,
    /// Where the agents' directories and the hosts' files live.
    _root: tempfile::TempDir,
    root: std::path::PathBuf,
    topology: Topology,
    hosts: BTreeMap<String, usize>,
    links: BTreeMap<(String, String), usize>,
    agents: BTreeMap<String, NetAgent>,
}

impl SimNet {
    /// Starts `topology` simulated under `seed`, with no faults.
    pub fn start(topology: Topology, seed: u64) -> Result<Self, NetError> {
        Self::start_with(
            topology,
            SimOptions {
                seed,
                ..SimOptions::default()
            },
        )
    }

    pub fn start_with(topology: Topology, options: SimOptions) -> Result<Self, NetError> {
        Self::begin(topology, options, None)
    }

    /// Starts `topology` replaying `ring`, the choices a run under
    /// `options` recorded (see [`SimNet::ring`]), or any prefix of them:
    /// once the ring runs out, the net delivers in its own order, the
    /// first lane waiting, and loses nothing more.
    pub fn replay(
        topology: Topology,
        options: SimOptions,
        ring: Vec<Choice>,
    ) -> Result<Self, NetError> {
        Self::begin(topology, options, Some(ring.into()))
    }

    fn begin(
        mut topology: Topology,
        options: SimOptions,
        replay: Option<VecDeque<Choice>>,
    ) -> Result<Self, NetError> {
        topology.validate()?;
        refuse(&topology)?;
        let root = tempfile::tempdir()?;
        let mut sim = Sim {
            net: Net::new(true),
            seed: options.seed,
            rng: Rng::new(options.seed),
            ids: Rng::new(!options.seed),
            faults: options.faults,
            ring: Vec::new(),
            replay,
        };
        let ids: Vec<HostId> = topology
            .hosts
            .iter()
            .map(|_| HostId(sim.ids.bytes()))
            .collect();
        let hosts: BTreeMap<String, usize> = topology
            .hosts
            .iter()
            .enumerate()
            .map(|(at, host)| (host.name.clone(), at))
            .collect();
        for (at, host) in topology.hosts.iter().enumerate() {
            let trusted = topology
                .links
                .iter()
                .filter_map(|link| {
                    if link.a == host.name {
                        Some(ids[hosts[&link.b]])
                    } else if link.b == host.name {
                        Some(ids[hosts[&link.a]])
                    } else {
                        None
                    }
                })
                .collect();
            let h = sim.net.add_host(&host.name, ids[at], trusted);
            if let Some(k) = host.tail_rows {
                sim.net.configure(
                    h,
                    daemon_core::Settings {
                        tail_rows: k,
                        ..daemon_core::Settings::default()
                    },
                );
            }
        }
        let mut links = BTreeMap::new();
        for link in &topology.links {
            let at = sim.net.add_link(hosts[&link.a], hosts[&link.b]);
            links.insert(link_key(&link.a, &link.b), at);
        }
        for at in 0..topology.hosts.len() {
            let entropy = sim.ids.bytes();
            sim.net.start_host(at, entropy);
        }
        // As on a real net, the declared links are up at start: each end
        // reads the other online.
        for link in &topology.links {
            let (a, b) = (hosts[&link.a], hosts[&link.b]);
            let what = format!("link {} - {} up at start", link.a, link.b);
            wait(&mut sim, &what, |sim| {
                (sim.net.reads_direct(a, b) && sim.net.reads_direct(b, a))
                    .then_some(())
                    .ok_or_else(|| "not yet".to_owned())
            })?;
        }
        let agents = std::mem::take(&mut topology.agents);
        let path = root.path().to_path_buf();
        let mut net = Self {
            sim: Arc::new(Mutex::new(sim)),
            _root: root,
            root: path,
            topology,
            hosts,
            links,
            agents: BTreeMap::new(),
        };
        for agent in agents {
            net.spawn(agent)?;
        }
        Ok(net)
    }

    /// The seed this net runs under.
    pub fn seed(&self) -> u64 {
        self.sim().seed
    }

    /// Every choice made so far, in order: replayed by [`SimNet::replay`],
    /// it makes the same run.
    pub fn ring(&self) -> Vec<Choice> {
        self.sim().ring.clone()
    }

    /// What reads this net's ring after the net itself has gone to a story.
    pub fn ring_reader(&self) -> RingReader {
        RingReader(self.sim.clone())
    }

    /// Every delivery made so far, one line each, by host and agent name:
    /// the schedule the seed chose.
    pub fn trace(&self) -> Vec<String> {
        self.sim().net.trace().to_vec()
    }

    pub fn topology(&self) -> &Topology {
        &self.topology
    }

    /// Creates an agent on its host, as a client does, and waits for its
    /// process to say Hello.
    pub fn spawn(&mut self, decl: AgentDecl) -> Result<wire::Agent, NetError> {
        if self.agents.contains_key(&decl.name) {
            return Err(NetError::AgentExists(decl.name));
        }
        refuse_agent(&decl)?;
        let h = self.host_index(&decl.host)?;
        let parent = match &decl.parent {
            Some(parent) => Some(self.agent_named(parent)?.parent()),
            None => None,
        };
        let parent_id = parent
            .as_ref()
            .and_then(|parent| AgentId::from_slice(&parent.agent_id));
        // A parent on another host makes its child as node does: its own
        // host forwards the create to the child's.
        let forwarded_by = match &decl.parent {
            Some(parent) if self.agent_named(parent)?.host != decl.host => {
                Some(self.host_index(&self.agent_named(parent)?.host.clone())?)
            }
            _ => None,
        };
        let steps = decl
            .script
            .as_ref()
            .map(|script| script.steps.clone())
            .unwrap_or_default();
        let cwd = self
            .root
            .join(&decl.host)
            .join("work")
            .join(decl.cwd.as_deref().unwrap_or_default());
        std::fs::create_dir_all(&cwd)?;
        let cwd = cwd.to_string_lossy().into_owned();
        let sim = self.sim.clone();
        let mut sim = sim.lock().unwrap();
        failed(&sim)?;
        if !sim.is_up(h) {
            return Err(NetError::Down(decl.host.clone()));
        }
        let agent = AgentId(sim.ids.bytes());
        let host_id = sim.net.host_id(h);
        let spec = wire::AgentSpec {
            agent_id: agent.0.to_vec(),
            profile_id: host_id.0.to_vec(),
            kind: FakeKind::ClaudeSdk.wire() as i32,
            cwd: cwd.clone(),
            parent,
            config: Some(wire::EffectiveConfig {
                grace_ms: AGENT_GRACE_MS,
                // A seed's run is a function of the seed alone.
                step_budget_micros: agent_core::StepBudget::UNLIMITED.micros,
                ..wire::EffectiveConfig::default()
            }),
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            created_at_ms: sim.net.now_ms() as i64,
            incarnation: 1,
            bootstrap: decl.prompt.as_deref().map(|text| wire::Bootstrap {
                of: Some(wire::bootstrap::Of::Prompt(submission(
                    b"testnet-first",
                    text,
                ))),
            }),
            ..wire::AgentSpec::default()
        };
        let create = Create {
            agent,
            cwd,
            given: Some(decl.name.clone()),
            branches: Default::default(),
            new_worktree: false,
            spec,
        };
        match &decl.journal {
            Some(name) => sim
                .net
                .add_agent_playing(agent, &decl.name, journal_fixture(name)?),
            None => sim.net.add_agent(agent, &decl.name, &steps),
        }
        let what = format!("{} to start on {}", decl.name, decl.host);
        let row = match forwarded_by {
            Some(from) => {
                let request = wire::CreateAgentRequest {
                    agent_id: agent.0.to_vec(),
                    name: create.given,
                    parent: create.spec.parent,
                    cwd: create.cwd,
                    initial_prompt: decl
                        .prompt
                        .as_deref()
                        .map(|text| submission(b"testnet-first", text)),
                    ..wire::CreateAgentRequest::default()
                };
                let forward = ClientRequest::Forward {
                    host: host_id,
                    request: Box::new(PeerRequest::CreateAgent(request)),
                };
                let row = registry_call(&mut sim, from, &what, forward)?;
                // The answer reaches the parent's provider through its
                // tools, after the frames its daemon wrote to its process
                // in the same step: those arrive before the provider can
                // go on.
                if let Some(p) = parent_id.and_then(|parent| sim.net.process_of(parent)) {
                    while let Some(lane) = sim
                        .net
                        .deliverable()
                        .into_iter()
                        .find(|lane| matches!(lane, Lane::ToAgent(to, _) if *to == p))
                    {
                        sim.net.deliver(lane);
                    }
                }
                row
            }
            None => registry_call(
                &mut sim,
                h,
                &what,
                ClientRequest::CreateAgent(Box::new(create)),
            )?,
        }
        .ok_or_else(|| answered_without_row(&decl.host, &what))?;
        drop(sim);
        self.agents.insert(
            decl.name.clone(),
            NetAgent {
                name: decl.name.clone(),
                host: decl.host.clone(),
                host_id: Uuid::from_bytes(host_id.0),
                id: Uuid::from_bytes(agent.0),
                kind: decl.kind,
                provider: Provider::Fake,
            },
        );
        self.topology.agents.push(decl);
        Ok(row)
    }

    fn sim(&self) -> MutexGuard<'_, Sim> {
        self.sim.lock().unwrap()
    }

    fn host_index(&self, host: &str) -> Result<usize, NetError> {
        self.hosts
            .get(host)
            .copied()
            .ok_or_else(|| NetError::NoHost(host.to_owned()))
    }

    fn agent_named(&self, name: &str) -> Result<&NetAgent, NetError> {
        self.agents
            .get(name)
            .ok_or_else(|| NetError::NoAgent(name.to_owned()))
    }

    fn link(&self, a: &str, b: &str) -> Result<usize, NetError> {
        self.links
            .get(&link_key(a, b))
            .copied()
            .ok_or_else(|| NetError::NoLink(a.to_owned(), b.to_owned()))
    }

    fn ack(&self, installed: String) -> Ack {
        Ack {
            installed,
            at_ms: self.sim().net.now_ms() as i64,
        }
    }

    /// The host whose store fences on `agent` read, and the agent's key.
    fn origin(&self, agent: &str) -> Result<(usize, store::AgentKey), NetError> {
        let at = self.agent_named(agent)?;
        Ok((self.host_index(&at.host)?, at.key()))
    }

    /// Waits on a host's store until `probe` answers what it found.
    fn on_store<T>(
        &self,
        host: usize,
        what: &str,
        probe: impl Fn(&store::Sqlite) -> Result<T, String>,
    ) -> Result<T, NetError> {
        let mut sim = self.sim();
        if !sim.is_up(host) {
            return Err(NetError::Down(sim.net.host_name(host).to_owned()));
        }
        wait(&mut sim, what, |sim| read(sim, host, &probe))
    }

    fn observe_call<E>(
        &self,
        host: &str,
        what: String,
        request: ClientRequest,
        pick: fn(&Response) -> Option<E>,
    ) -> Result<ObserverOf<E>, NetError>
    where
        E: crate::observe::Describe + Clone + Send + 'static,
    {
        let h = self.host_index(host)?;
        let mut sim = self.sim();
        failed(&sim)?;
        if !sim.is_up(h) {
            return Err(NetError::Down(host.to_owned()));
        }
        let call = sim.net.client_call(h, request);
        drop(sim);
        Ok(ObserverOf::simulated(
            what,
            Box::new(Stream {
                sim: self.sim.clone(),
                host: h,
                call,
                taken: 0,
                pick,
            }),
        ))
    }
}

impl World for SimNet {
    fn agent(&self, name: &str) -> Result<&NetAgent, NetError> {
        self.agent_named(name)
    }

    fn is_up(&self, host: &str) -> bool {
        self.host_index(host).is_ok_and(|h| self.sim().is_up(h))
    }

    fn generation(&self, host: &str) -> Result<u64, NetError> {
        let h = self.host_index(host)?;
        Ok(self.sim().net.generation(h))
    }

    fn now_ms(&self) -> i64 {
        self.sim().net.now_ms() as i64
    }

    fn advance(&self, by: Duration) -> Result<Ack, NetError> {
        self.sim().net.advance(by.as_millis() as u64);
        Ok(self.ack(format!("policy time +{}ms", by.as_millis())))
    }

    fn open_gate(&self, name: &str) -> Result<Ack, NetError> {
        self.sim().net.open_gate(name);
        Ok(self.ack(format!("gate {name} open")))
    }

    fn journal_end(&self, agent: &str) -> Result<u64, NetError> {
        let at = self.agent_named(agent)?;
        let h = self.host_index(&at.host)?;
        let agent = AgentId(at.id.into_bytes());
        Ok(self
            .sim()
            .net
            .journal(h, agent)
            .map_or(0, models::net::journal_end))
    }

    fn sever_link(&mut self, a: &str, b: &str) -> Result<Ack, NetError> {
        let l = self.link(a, b)?;
        self.sim().net.sever(l);
        Ok(self.ack(format!("link {a} - {b} severed")))
    }

    // The simulated net's links are messages between cores: no socket,
    // path or link log stands under them.
    fn set_router(&mut self, _: &str, _: RouterKind) -> Result<Ack, NetError> {
        Err(NetError::Unsupported("a router in a simulated net"))
    }

    fn change_network(&self, _: &str) -> Result<Ack, NetError> {
        Err(NetError::Unsupported(
            "changing a host's network in a simulated net",
        ))
    }

    fn bind_ipv6_only(&mut self, _: &str) -> Result<Ack, NetError> {
        Err(NetError::Unsupported(
            "binding IPv6 only in a simulated net",
        ))
    }

    fn carrying_path(&self, _: &str, _: &str) -> Result<Option<PathKind>, NetError> {
        Err(NetError::Unsupported(
            "the carrying path in a simulated net",
        ))
    }

    fn link_log(&self, _: &str) -> Result<Vec<LinkEvent>, NetError> {
        Err(NetError::Unsupported("the link log in a simulated net"))
    }

    async fn restore_link(&mut self, a: &str, b: &str) -> Result<Ack, NetError> {
        let l = self.link(a, b)?;
        let (ha, hb) = (self.host_index(a)?, self.host_index(b)?);
        let mut sim = self.sim();
        // As on a real net, a severed link comes back once both ends have
        // seen it go.
        if sim.net.is_severed(l) {
            let what = format!("the severed link {a} - {b} to go at both ends");
            wait(&mut sim, &what, |sim| {
                (!sim.net.reads_direct(ha, hb) && !sim.net.reads_direct(hb, ha))
                    .then_some(())
                    .ok_or_else(|| "an end still routes direct".to_owned())
            })?;
        }
        sim.net.restore(l);
        drop(sim);
        Ok(self.ack(format!("link {a} - {b} restored")))
    }

    async fn link_up(&self, a: &str, b: &str) -> bool {
        let (Ok(a), Ok(b)) = (self.host_index(a), self.host_index(b)) else {
            return false;
        };
        let sim = self.sim();
        sim.net.reads_direct(a, b) && sim.net.reads_direct(b, a)
    }

    async fn wait_link(&self, a: &str, b: &str, up: bool) -> Result<(), NetError> {
        let (ha, hb) = (self.host_index(a)?, self.host_index(b)?);
        let what = format!("link {a} - {b} {}", if up { "up" } else { "down" });
        let mut sim = self.sim();
        wait(&mut sim, &what, |sim| {
            let (ab, ba) = if up {
                (sim.net.reads_direct(ha, hb), sim.net.reads_direct(hb, ha))
            } else {
                (sim.net.reads_offline(ha, hb), sim.net.reads_offline(hb, ha))
            };
            (ab && ba)
                .then_some(())
                .ok_or_else(|| format!("{a} -> {b} {ab}, {b} -> {a} {ba}"))
        })
    }

    async fn kill_daemon(&mut self, host: &str) -> Result<Ack, NetError> {
        let h = self.host_index(host)?;
        let mut sim = self.sim();
        failed(&sim)?;
        if !sim.is_up(h) {
            return Err(NetError::Down(host.to_owned()));
        }
        sim.net.stop_host(h);
        drop(sim);
        Ok(self.ack(format!("{host}'s daemon killed")))
    }

    async fn stop_daemon(&mut self, host: &str) -> Result<Ack, NetError> {
        let h = self.host_index(host)?;
        let mut sim = self.sim();
        failed(&sim)?;
        if !sim.is_up(h) {
            return Err(NetError::Down(host.to_owned()));
        }
        // In memory, what a clean stop flushes is already where a crash
        // leaves it.
        sim.net.stop_host(h);
        drop(sim);
        Ok(self.ack(format!("{host}'s daemon stopped")))
    }

    async fn restart_daemon(&mut self, host: &str) -> Result<Ack, NetError> {
        let h = self.host_index(host)?;
        let mut sim = self.sim();
        failed(&sim)?;
        if sim.is_up(h) {
            return Err(NetError::Running(host.to_owned()));
        }
        let entropy = sim.ids.bytes();
        sim.net.start_host(h, entropy);
        failed(&sim)?;
        drop(sim);
        Ok(self.ack(format!("{host}'s daemon started")))
    }

    async fn send(&self, agent: &str, text: &str) -> Result<wire::SendInputResponse, NetError> {
        let kind = self.agent_named(agent)?.kind;
        let id = self.sim().ids.bytes::<16>();
        self.input(agent, prompt(kind, &id, text)).await
    }

    async fn input(
        &self,
        agent: &str,
        input: wire::Input,
    ) -> Result<wire::SendInputResponse, NetError> {
        let at = self.agent_named(agent)?.clone();
        let h = self.host_index(&at.host)?;
        let key = at.key();
        let id = AgentId(at.id.into_bytes());
        let mut sim = self.sim();
        failed(&sim)?;
        let exited = read(&sim, h, |store| {
            Ok(store
                .agent(&key)
                .map_err(|error| error.to_string())?
                .is_none_or(|row| row.lifecycle == Lifecycle::Exited as i32))
        })
        .map_err(|_| NetError::Down(at.host.clone()))?;
        if exited {
            return Ok(interpret::reply::rejected(wire::Refusal::Exited));
        }
        // The shell waits for the process's connection before it writes.
        let what = format!("{agent}'s process to be connected");
        let p = wait(&mut sim, &what, |sim| {
            sim.net
                .process_of(id)
                .filter(|&p| sim.net.processes()[p].conn.is_some())
                .ok_or_else(|| "not connected".to_owned())
        })?;
        let input_id = input.input_id.clone();
        sim.net.relay(p, input).map_err(|error| NetError::Host {
            host: at.host.clone(),
            error,
        })?;
        let what = format!("{agent}'s verdict on its input");
        let verdict = wait(&mut sim, &what, |sim| {
            let process = &sim.net.processes()[p];
            if !process.running || process.conn.is_none() {
                return Ok(None);
            }
            match sim.net.verdict(p, &input_id) {
                Some(verdict) => Ok(Some(verdict.clone())),
                None => Err("no verdict yet".to_owned()),
            }
        })?;
        sim.net.forget_relayed(p, &input_id);
        let Some(verdict) = verdict else {
            return Err(NetError::Host {
                host: at.host,
                error: "the agent's answer was lost with its connection".to_owned(),
            });
        };
        if let Some(wire::send_input_response::Of::Rejected(rejected)) = &verdict.of
            && rejected.refusal() == wire::Refusal::Exiting
        {
            sim.net.feed(h, Input::Exiting { agent: id });
        }
        failed(&sim)?;
        Ok(verdict)
    }

    async fn spawn_child(
        &mut self,
        parent: &str,
        decl: AgentDecl,
    ) -> Result<wire::Agent, NetError> {
        self.spawn(decl.parent(parent))
    }

    async fn delete(&mut self, agent: &str) -> Result<Ack, NetError> {
        let at = self.agent_named(agent)?;
        let h = self.host_index(&at.host)?;
        let id = AgentId(at.id.into_bytes());
        let mut sim = self.sim();
        failed(&sim)?;
        if !sim.is_up(h) {
            return Err(NetError::Down(at.host.clone()));
        }
        let what = format!("{agent} to be deleted");
        registry_call(&mut sim, h, &what, ClientRequest::DeleteAgent { agent: id })?;
        drop(sim);
        self.agents.remove(agent);
        Ok(self.ack(format!("{agent} deleted")))
    }

    async fn stop(&self, agent: &str) -> Result<Ack, NetError> {
        let at = self.agent_named(agent)?;
        let h = self.host_index(&at.host)?;
        let id = AgentId(at.id.into_bytes());
        let mut sim = self.sim();
        failed(&sim)?;
        if !sim.is_up(h) {
            return Err(NetError::Down(at.host.clone()));
        }
        let what = format!("{agent} to stop");
        registry_call(&mut sim, h, &what, ClientRequest::StopAgent { agent: id })?;
        drop(sim);
        Ok(self.ack(format!("{agent} stopped")))
    }

    async fn resume(&mut self, agent: &str, prompt: Option<&str>) -> Result<wire::Agent, NetError> {
        let at = self.agent_named(agent)?;
        let h = self.host_index(&at.host)?;
        let id = AgentId(at.id.into_bytes());
        let mut sim = self.sim();
        failed(&sim)?;
        if !sim.is_up(h) {
            return Err(NetError::Down(at.host.clone()));
        }
        let input_id = sim.ids.bytes::<16>();
        let bootstrap = prompt.map(|text| wire::Bootstrap {
            of: Some(wire::bootstrap::Of::Prompt(submission(&input_id, text))),
        });
        let what = format!("{agent} to resume");
        registry_call(
            &mut sim,
            h,
            &what,
            ClientRequest::ResumeAgent(Box::new(Resume {
                agent: id,
                bootstrap,
            })),
        )?
        .ok_or_else(|| answered_without_row(&at.host, &what))
    }

    async fn peer_message(
        &self,
        from: &str,
        to: &str,
        envelope: wire::Envelope,
    ) -> Result<wire::SendMessageResponse, NetError> {
        let (f, h) = (self.host_index(from)?, self.host_index(to)?);
        let link = self.link(from, to)?;
        let mut sim = self.sim();
        failed(&sim)?;
        if !(sim.is_up(f) && sim.is_up(h) && sim.net.is_live(link)) {
            return Err(NetError::Host {
                host: from.to_owned(),
                error: format!("no channel to {to}"),
            });
        }
        let envelope_id = envelope.id.clone();
        let what = format!("{from}'s message to {to}");
        let call = sim.net.client_call(h, models::net::peer_message(envelope));
        let ended = wait(&mut sim, &what, |sim| {
            sim.net
                .client(h, call)
                .and_then(|client| client.ended().cloned())
                .ok_or_else(|| "not yet answered".to_owned())
        })?;
        sim.net.take_client(h, call);
        match ended {
            daemon_core::payload::Status::Ok => Ok(wire::SendMessageResponse { envelope_id }),
            daemon_core::payload::Status::Failed(error) => Err(NetError::Refused(Box::new(
                node::status(models::net::wire_error(&error)),
            ))),
        }
    }

    async fn observe(&self, host: &str, agent: &str, tail: u32) -> Result<Observer, NetError> {
        let id = self.agent_named(agent)?.id;
        self.observe_call(
            host,
            format!("{agent} observed at {host}"),
            ClientRequest::SubscribeSession {
                agent_id: id.as_bytes().to_vec(),
                tail,
            },
            |response| match response {
                Response::Session(event) => Some(SessionEvent::clone(event)),
                _ => None,
            },
        )
    }

    async fn observe_inventory(&self, host: &str) -> Result<InventoryObserver, NetError> {
        self.observe_call(
            host,
            format!("inventory at {host}"),
            ClientRequest::SubscribeInventory,
            |response| match response {
                Response::Inventory(event) => Some(InventoryEvent::clone(event)),
                _ => None,
            },
        )
    }

    async fn fence(&self, host: &str, agent: &str, order: u64) -> Result<(), NetError> {
        let (_, key) = self.origin(agent)?;
        let what = format!("{host} to hold {agent}'s row at order {order}");
        self.on_store(self.host_index(host)?, &what, |store| {
            probes::holds_order(store, &key, order)
        })
    }

    async fn current(&self, host: &str, agent: &str) -> Result<(), NetError> {
        let (origin, key) = self.origin(agent)?;
        let replica = self.host_index(host)?;
        let what = format!("{agent}'s replica at {host} to be current");
        let mut sim = self.sim();
        wait(&mut sim, &what, |sim| {
            let newest = read(sim, origin, |store| probes::origin_newest(store, &key))?;
            read(sim, replica, |store| {
                probes::replica_current(store, &key, newest)
            })?;
            let replica_block = read(sim, replica, |store| invariant::block(store, &key))?;
            let origin_block = read(sim, origin, |store| invariant::block(store, &key))?;
            invariant::check(&replica_block, &origin_block).map_err(|error| error.to_string())
        })
    }

    async fn turn_ended(&self, agent: &str, k: usize) -> Result<u64, NetError> {
        assert!(k >= 1, "turns count from one");
        let (origin, key) = self.origin(agent)?;
        let what = format!("{agent}'s turn {k} to end");
        self.on_store(origin, &what, |store| probes::turn_end(store, &key, k))
    }

    async fn input_settled(&self, agent: &str, input_id: &[u8]) -> Result<u64, NetError> {
        let (origin, key) = self.origin(agent)?;
        let what = format!("{agent}'s input {} to settle", interpret::to_hex(input_id));
        self.on_store(origin, &what, |store| {
            probes::settled(store, &key, input_id)
        })
    }

    async fn read_store<T: Send + 'static>(
        &self,
        host: &str,
        read: impl FnOnce(&store::Sqlite) -> T + Send + 'static,
    ) -> Result<T, NetError> {
        let h = self.host_index(host)?;
        let sim = self.sim();
        failed(&sim)?;
        sim.net
            .read(h, read)
            .ok_or_else(|| NetError::Down(host.to_owned()))
    }

    async fn block_whole(&self, host: &str, agent: &str) -> Result<(), NetError> {
        let (origin, key) = self.origin(agent)?;
        let replica = self.host_index(host)?;
        let sim = self.sim();
        failed(&sim)?;
        let block = |h: usize| {
            read(&sim, h, |store| invariant::block(store, &key)).map_err(|error| NetError::Host {
                host: sim.net.host_name(h).to_owned(),
                error,
            })
        };
        Ok(invariant::check(&block(replica)?, &block(origin)?)?)
    }

    async fn fetch(
        &self,
        host: &str,
        agent: &str,
        before_order: Option<u64>,
        limit: u32,
    ) -> Result<wire::ListItemsResponse, NetError> {
        let id = self.agent_named(agent)?.id;
        let h = self.host_index(host)?;
        let mut sim = self.sim();
        failed(&sim)?;
        if !sim.is_up(h) {
            return Err(NetError::Down(host.to_owned()));
        }
        let call = sim.net.client_call(
            h,
            ClientRequest::ListItems(wire::ListItemsRequest {
                agent_id: id.as_bytes().to_vec(),
                before_order,
                page_size: limit,
            }),
        );
        let what = format!("a page of {agent} at {host}");
        let answer = wait(&mut sim, &what, |sim| {
            let Some(answered) = sim.net.client(h, call) else {
                return Ok(Err(daemon_core::payload::Status::Ok));
            };
            if let Some(Response::Fetched(page)) = answered.responses().next() {
                return Ok(Ok(page.clone()));
            }
            match answered.ended() {
                Some(status) => Ok(Err(status.clone())),
                None => Err("no page yet".to_owned()),
            }
        });
        sim.net.take_client(h, call);
        Ok(answer?.map_err(node::ServeError::from)?)
    }

    fn set_source_policy(&self, host: &str, policy: node::SourcePolicy) -> Result<Ack, NetError> {
        let h = self.host_index(host)?;
        let mut sim = self.sim();
        failed(&sim)?;
        if !sim.is_up(h) {
            return Err(NetError::Down(host.to_owned()));
        }
        sim.net.feed(h, Input::Policy(policy));
        drop(sim);
        Ok(self.ack(format!("{host}'s source policy {policy:?}")))
    }

    async fn open_sources(&self, host: &str) -> Result<Vec<store::AgentKey>, NetError> {
        let h = self.host_index(host)?;
        let sim = self.sim();
        failed(&sim)?;
        let mut open = sim
            .net
            .with_daemon(h, |daemon| daemon.sources().open().collect::<Vec<_>>())
            .ok_or_else(|| NetError::Down(host.to_owned()))?;
        open.sort();
        Ok(open)
    }

    async fn host_ready(&self, at: &str, host: &str) -> Result<bool, NetError> {
        let (h, of) = (self.host_index(at)?, self.host_index(host)?);
        let sim = self.sim();
        failed(&sim)?;
        let id = sim.net.host_id(of);
        sim.net
            .with_daemon(h, |daemon| daemon.sources().ready(id))
            .ok_or_else(|| NetError::Down(at.to_owned()))
    }

    async fn until<T, E, F>(&self, what: &str, mut probe: impl FnMut() -> F) -> Result<T, NetError>
    where
        E: Display,
        F: Future<Output = Result<T, E>>,
    {
        for _ in 0..MOST_STEPS {
            let seen = match probe().await {
                Ok(found) => return Ok(found),
                Err(seen) => seen.to_string(),
            };
            let mut sim = self.sim();
            failed(&sim)?;
            if !sim.step() {
                failed(&sim)?;
                return Err(quiet(&sim, what, seen));
            }
        }
        Err(hung(what))
    }

    async fn holds_for<F>(
        &self,
        what: &str,
        window: Duration,
        mut check: impl FnMut() -> F,
    ) -> Result<(), NetError>
    where
        F: Future<Output = bool>,
    {
        for _ in 0..MOST_STEPS {
            if !check().await {
                return Err(Stuck::Broke {
                    what: what.to_owned(),
                    after: Duration::ZERO,
                    window,
                }
                .into());
            }
            let mut sim = self.sim();
            failed(&sim)?;
            if !sim.step() {
                return failed(&sim);
            }
        }
        // A net that never settles is a livelock, not a window that held.
        Err(hung(what))
    }

    async fn shutdown(self) -> Result<(), NetError> {
        failed(&self.sim())
    }
}

/// A simulated Subscribe or SubscribeInventory stream: a client's call on
/// a host, read as the net moves.
struct Stream<E> {
    sim: Arc<Mutex<Sim>>,
    host: usize,
    call: CallId,
    taken: usize,
    pick: fn(&Response) -> Option<E>,
}

impl<E: Send> Feed<E> for Stream<E> {
    fn pump(&mut self, step: bool) -> Pumped<E> {
        let mut sim = self.sim.lock().unwrap();
        let quiet = step && !sim.step();
        let failure = sim.net.failure().map(ToString::to_string);
        let Some(client) = sim.net.client(self.host, self.call) else {
            return Pumped {
                events: Vec::new(),
                closed: true,
                quiet,
                failure,
            };
        };
        let events = client
            .responses()
            .skip(self.taken)
            .filter_map(self.pick)
            .collect();
        self.taken = client.response_count();
        Pumped {
            events,
            closed: client.ended().is_some(),
            quiet,
            failure,
        }
    }
}

impl<E> Drop for Stream<E> {
    fn drop(&mut self) {
        if let Ok(mut sim) = self.sim.lock() {
            sim.net.cancel_client(self.host, self.call);
        }
    }
}

/// Moves the net until `probe` holds, failing as soon as nothing is left
/// to deliver.
fn wait<T>(
    sim: &mut Sim,
    what: &str,
    mut probe: impl FnMut(&mut Sim) -> Result<T, String>,
) -> Result<T, NetError> {
    for _ in 0..MOST_STEPS {
        failed(sim)?;
        let seen = match probe(sim) {
            Ok(found) => return Ok(found),
            Err(seen) => seen,
        };
        if !sim.step() {
            failed(sim)?;
            return Err(quiet(sim, what, seen));
        }
    }
    Err(hung(what))
}

/// Makes a registry call on the host as a client does, waits until it
/// ends, and returns the agent's row it was answered with, if any.
fn registry_call(
    sim: &mut Sim,
    h: usize,
    what: &str,
    request: ClientRequest,
) -> Result<Option<wire::Agent>, NetError> {
    let call = sim.net.client_call(h, request);
    let ended = wait(sim, what, |sim| {
        sim.net
            .client(h, call)
            .and_then(|client| client.ended().cloned())
            .ok_or_else(|| "not yet answered".to_owned())
    })?;
    let client = sim.net.take_client(h, call).expect("the registry call");
    let row = client.responses().find_map(|response| match response {
        Response::Agent(row) => Some((**row).clone()),
        Response::Peer(reply) => match &**reply {
            PeerReply::Agent(row) => Some(row.clone()),
            _ => None,
        },
        _ => None,
    });
    match ended {
        daemon_core::payload::Status::Ok => Ok(row),
        status => Err(NetError::Host {
            host: sim.net.host_name(h).to_owned(),
            error: format!("{what}: the call ended {status:?}"),
        }),
    }
}

fn answered_without_row(host: &str, what: &str) -> NetError {
    NetError::Host {
        host: host.to_owned(),
        error: format!("{what}: answered without the agent's row"),
    }
}

/// What a wait reports when nothing is left to deliver and its condition
/// does not hold: it never will, unless the story acts.
fn quiet(sim: &Sim, what: &str, seen: String) -> NetError {
    Stuck::Deadline {
        what: format!(
            "{what} (nothing left to deliver after {} steps under seed {})",
            sim.net.steps(),
            sim.seed
        ),
        waited: PATIENCE,
        seen,
    }
    .into()
}

fn hung(what: &str) -> NetError {
    Stuck::Hung {
        what: format!("{what}: {MOST_STEPS} deliveries without an end"),
        waited: PATIENCE,
    }
    .into()
}

fn failed(sim: &Sim) -> Result<(), NetError> {
    match sim.net.failure() {
        Some(failure) => Err(NetError::Simulated {
            seed: sim.seed,
            failure: failure.to_string(),
        }),
        None => Ok(()),
    }
}

/// Reads a host's store, or says it is down.
fn read<T>(
    sim: &Sim,
    host: usize,
    probe: impl FnOnce(&store::Sqlite) -> Result<T, String>,
) -> Result<T, String> {
    sim.net
        .read(host, probe)
        .unwrap_or_else(|| Err(format!("{} is down", sim.net.host_name(host))))
}

/// What a simulated net cannot hold, refused before anything starts.
fn refuse(topology: &Topology) -> Result<(), NetError> {
    if topology.relay.is_some() {
        return Err(NetError::Unsupported("a relay in a simulated net"));
    }
    for host in &topology.hosts {
        if host.lan || host.discovery || host.bonjour {
            return Err(NetError::Unsupported(
                "the LAN and discovery in a simulated net",
            ));
        }
        if host.runs_as_process() {
            return Err(NetError::Unsupported("a process host in a simulated net"));
        }
        if host.provider == Provider::Real {
            return Err(NetError::Unsupported("a real provider in a simulated net"));
        }
        if host.account.is_some() || host.script.is_some() || !host.repositories.is_empty() {
            return Err(NetError::Unsupported(
                "accounts, host scripts and repositories in a simulated net",
            ));
        }
    }
    topology.agents.iter().try_for_each(refuse_agent)
}

fn refuse_agent(agent: &AgentDecl) -> Result<(), NetError> {
    if agent.kind != FakeKind::ClaudeSdk {
        return Err(NetError::Unsupported(
            "agents other than headless Claude in a simulated net, whose agents speak the test kind",
        ));
    }
    if agent.provider == Some(Provider::Real) {
        return Err(NetError::Unsupported("a real provider in a simulated net"));
    }
    if agent.script_file.is_some() || agent.repository.is_some() || agent.branch.is_some() {
        return Err(NetError::Unsupported(
            "script files, repositories and branches in a simulated net",
        ));
    }
    if agent.journal.is_some() && agent.script.is_some() {
        return Err(NetError::Unsupported(
            "a script and a recorded journal for one agent",
        ));
    }
    Ok(())
}

/// The steps of the named journal fixture, from its initial snapshot on.
/// Its agent is created as headless Claude, so the journal must be one.
fn journal_fixture(name: &str) -> Result<Vec<wire::Step>, NetError> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/journals")
        .join(format!("{name}.frames"));
    let bytes = std::fs::read(&path)?;
    let mut steps = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        match journal::decode_frame(&bytes[at..]) {
            journal::Frame::Whole(step, length) => {
                steps.push(*step);
                at += length;
            }
            other => {
                return Err(NetError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "{}: byte {at} is not a whole frame: {other:?}",
                        path.display()
                    ),
                )));
            }
        }
    }
    let kind = steps
        .first()
        .and_then(|step| step.snapshot.as_ref())
        .map(|snapshot| snapshot.kind);
    if kind != Some(FakeKind::ClaudeSdk.wire() as i32) {
        return Err(NetError::Unsupported(
            "a recorded journal of another kind than headless Claude",
        ));
    }
    Ok(steps)
}
