//! The composed net: a topology's every daemon and agent as their real
//! cores, wired by the drivers' real glue, and what is on its way between
//! them.
//!
//! [`Net`] is the net as a value (see the `env` module). [`ComposedNet`] is
//! the model over it: its initial state is a [`Topology`] started and its
//! agents created; its actions are the environment's, a lane's next
//! delivery, a call's message lost, a link severed and restored, a daemon
//! killed and started again or rebooted uncleanly, the clock moved to the
//! next timer, and a person's calls, an input sent and a stop; its
//! properties are the promises of the single daemon, replication, presence
//! and the agent lifecycle that a net of two hosts and one agent can break,
//! judged by the predicates those machines' own models judge them by, beside
//! the promises that hold only between machines. The environment judges the
//! promises about a step as the step is made, so the simulated mode keeps
//! them too; the models crate's README names the promises the net leaves to
//! their own models, and why. Explored breadth first it is the composed
//! check; walked one seeded delivery at a time it is the testnet's
//! simulated mode.

mod env;
mod provider;

use daemon_core::payload::ClientRequest;
use daemon_core::{AgentId, Create, HostId};
pub use env::{
    ClientCall, Dbg, Failure, Journal, Lane, Net, Process, START_MS, journal_end, peer_message,
    wire_error,
};
pub use provider_fakes::script::Step;
use store::AgentKey;
use wire::Lifecycle;

use crate::lifecycle::{EXIT_CAUSE_LAST_STEP, EXITS_AT_TURN_END};
use crate::presence::PRESENCE_FROM_LINKS;
use crate::replication::{
    BLOCK_AGREES, BLOCK_CONTIGUOUS, CURSOR_COMPLETE, NO_OTHER_GENERATION, block_agrees,
    block_contiguous, cursor_complete, no_other_generation,
};
use crate::single_daemon::{
    COMMIT_BEFORE_BROADCAST, GENERATION_BUMPS_AFTER_UNCLEAN, GENERATION_BUMPS_ONLY_AFTER_UNCLEAN,
    RECLAIMED_BELOW_CURSOR, reclaimed_below_cursor,
};
use crate::{Machine, Property};

/// The hosts, links and agents a net starts with.
#[derive(Clone, Debug)]
pub struct Topology {
    pub hosts: Vec<String>,
    /// Links between hosts, by their places in `hosts`.
    pub links: Vec<(usize, usize)>,
    pub agents: Vec<AgentPlan>,
}

/// An agent a net starts with: its name, its host, the script its provider
/// plays and the prompt it is created with.
#[derive(Clone, Debug)]
pub struct AgentPlan {
    pub name: String,
    pub host: usize,
    pub script: Vec<Step>,
    pub prompt: Option<String>,
}

/// The spec a client's create hands a host for a headless Claude agent the
/// test kind speaks for: its first incarnation, its prompt queued.
pub fn agent_spec(
    agent: AgentId,
    host: HostId,
    cwd: &str,
    parent: Option<wire::AgentRef>,
    prompt: Option<&str>,
    grace_ms: u32,
    now_ms: u64,
) -> wire::AgentSpec {
    wire::AgentSpec {
        agent_id: agent.0.to_vec(),
        profile_id: host.0.to_vec(),
        kind: wire::Kind::ClaudeSdk as i32,
        cwd: cwd.to_owned(),
        parent,
        config: Some(wire::EffectiveConfig {
            grace_ms,
            // The checker's verdict is a function of the model alone.
            step_budget_micros: agent_core::StepBudget::UNLIMITED.micros,
            ..wire::EffectiveConfig::default()
        }),
        daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
        created_at_ms: now_ms as i64,
        incarnation: 1,
        bootstrap: prompt.map(|text| wire::Bootstrap {
            of: Some(wire::bootstrap::Of::Prompt(wire::PromptSubmission {
                input_id: b"first".to_vec(),
                prompt: Some(wire::PromptInput {
                    text: text.to_owned(),
                    attachments: Vec::new(),
                }),
            })),
        }),
        ..wire::AgentSpec::default()
    }
}

/// A client's create of the agent `spec` describes.
pub fn create(name: &str, spec: wire::AgentSpec) -> ClientRequest {
    let agent = AgentId::from_slice(&spec.agent_id).expect("a spec names its agent");
    ClientRequest::CreateAgent(Box::new(Create {
        agent,
        cwd: spec.cwd.clone(),
        given: Some(name.to_owned()),
        branches: Default::default(),
        new_worktree: false,
        recent: None,
        spec,
    }))
}

/// A person's prompt to a headless Claude agent.
pub fn prompt(input_id: &[u8], text: &str) -> wire::Input {
    wire::Input::new(
        input_id.to_vec(),
        wire::KindInput::ClaudeSdk(wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
                text: text.to_owned(),
                attachments: Vec::new(),
            })),
        }),
    )
}

/// How long an agent outlives its daemon, in the composed net.
pub(crate) const AGENT_GRACE_MS: u32 = 20_000;
/// Deliveries a start may take to settle before the model gives up on it.
const MOST_SETTLING: usize = 100_000;

/// A topology composed, within its bounds.
#[derive(Clone, Debug)]
pub struct ComposedNet {
    pub topology: Topology,
    /// Times a path makes a delivery other than the first one waiting:
    /// every schedule within this many reorderings of the net's own order
    /// is explored.
    pub reorders: u8,
    /// Messages of calls lost on their way, all told.
    pub losses: u8,
    /// Times a link is severed, all told; it may be restored after each.
    pub severs: u8,
    /// Daemon kills, all told; a killed daemon may start again.
    pub kills: u8,
    /// Unclean reboots, each starting the daemon under its next generation.
    pub reboots: u8,
    /// Times the clock moves on to the next timer.
    pub advances: u8,
    /// Prompts a person sends the agent.
    pub sends: u8,
    /// Stops a person asks of the agent.
    pub stops: u8,
}

impl ComposedNet {
    /// Two linked hosts, one agent on the first whose one turn writes three
    /// items (its prompt, a text and the turn's end), replicated to the
    /// second.
    pub fn two_hosts() -> Topology {
        Topology {
            hosts: vec!["desk".to_owned(), "laptop".to_owned()],
            links: vec![(0, 1)],
            agents: vec![AgentPlan {
                name: "worker".to_owned(),
                host: 0,
                script: vec![
                    Step::Text {
                        chunks: vec!["done".to_owned()],
                    },
                    Step::TurnEnd,
                ],
                prompt: Some("go".to_owned()),
            }],
        }
    }

    /// The two hosts and their agent with nothing happening to them but
    /// the net's own order of deliveries.
    pub fn quiet() -> Self {
        Self {
            topology: Self::two_hosts(),
            reorders: 0,
            losses: 0,
            severs: 0,
            kills: 0,
            reboots: 0,
            advances: 0,
            sends: 0,
            stops: 0,
        }
    }

    /// The origin reboots uncleanly, resetting the replica, while the link
    /// may be severed and restored around it: 33,025 states in 2 s on 12
    /// threads of an M-series Mac.
    pub fn reset_while_flipping() -> Self {
        Self {
            severs: 1,
            reboots: 1,
            ..Self::quiet()
        }
    }

    /// A call's message lost, a delivery out of the net's order, and the
    /// clock moved on to the backoffs: 9,559 states in 2 s on 12 threads.
    pub fn lost_messages() -> Self {
        Self {
            reorders: 1,
            losses: 1,
            advances: 1,
            ..Self::quiet()
        }
    }

    /// A person sends a prompt and asks for a stop while either daemon may
    /// be killed and started again: 216,320 states in 22 s on 12 threads.
    pub fn calls_through_a_crash() -> Self {
        Self {
            kills: 1,
            sends: 1,
            stops: 1,
            ..Self::quiet()
        }
    }

    /// The configurations `just test` explores exhaustively.
    pub fn ci() -> [(&'static str, Self); 3] {
        [
            ("a reset while presence flips", Self::reset_while_flipping()),
            ("lost messages", Self::lost_messages()),
            (
                "a person's calls through a crash",
                Self::calls_through_a_crash(),
            ),
        ]
    }

    /// The larger configurations a nightly run explores: every fault but a
    /// kill with a reordering (953,190 states in 55 s on 12 threads), and
    /// the person's calls through a crash with a lost message (1,393,830
    /// states in 124 s).
    pub fn nightly() -> [(&'static str, Self); 2] {
        [
            (
                "every fault but a kill",
                Self {
                    reorders: 1,
                    losses: 1,
                    severs: 1,
                    reboots: 1,
                    ..Self::quiet()
                },
            ),
            (
                "a person's calls through a crash, with a lost message",
                Self {
                    losses: 1,
                    ..Self::calls_through_a_crash()
                },
            ),
        ]
    }

    fn origin(&self) -> usize {
        self.topology.agents[0].host
    }

    /// The host that replicates the first agent: the first other host.
    fn replica(&self) -> usize {
        usize::from(self.origin() == 0)
    }
}

/// A host's id in the composed net: its name, padded.
pub fn host_id(name: &str) -> HostId {
    let mut id = [b'-'; 16];
    for (at, byte) in name.bytes().take(16).enumerate() {
        id[at] = byte;
    }
    HostId(id)
}

/// An agent's id in the composed net: its name, padded.
pub fn agent_id(name: &str) -> AgentId {
    let mut id = [b'_'; 16];
    for (at, byte) in name.bytes().take(16).enumerate() {
        id[at] = byte;
    }
    AgentId(id)
}

/// How much of the bounds a path has used.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Used {
    reorders: u8,
    losses: u8,
    severs: u8,
    kills: u8,
    reboots: u8,
    advances: u8,
    sends: u8,
    stops: u8,
}

/// What the properties judge that the net does not keep, recorded as each
/// action sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Seen {
    /// The origin rebooted under a new generation while the link was down
    /// or before the replica took the new generation, and the link has gone
    /// down or come back since.
    reset_while_flipping: bool,
    /// The origin rebooted, and the replica has not yet taken the new
    /// generation.
    resetting: bool,
    /// The replica read the agent exited while the origin's row said live.
    exited_early: bool,
}

/// What the properties read of a state's stores and cores, read once as
/// the transition that made it ends.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Judged {
    /// The replica's block was taken under the generation the origin runs.
    same_generation: bool,
    /// The replica is current with the origin under its generation.
    current: bool,
    /// Each of the four replication promises over the two stores, judged
    /// under the origin's generation.
    block: [bool; 4],
    /// The agent's journal on its host still holds every frame from the
    /// cursor its host's store has.
    journal_kept: bool,
    /// The agent's row on its own host and on the replica.
    origin_row: Option<i32>,
    replica_row: Option<i32>,
    /// Prompts the agent accepted.
    accepted: usize,
    /// A prompt the agent accepted has no row on its host.
    unrowed_input: bool,
    /// Nothing is left to deliver, and every link that is up has each end
    /// reading the other online over it.
    online: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub net: Net,
    pub used: Used,
    seen: Seen,
    judged: Judged,
}

impl State {
    fn key(&self, model: &ComposedNet) -> AgentKey {
        AgentKey::new(
            self.net.host_id(model.origin()).0.to_vec(),
            agent_id(&model.topology.agents[0].name).0.to_vec(),
        )
    }

    /// Reads what the properties judge, once.
    fn judge(&mut self, model: &ComposedNet) {
        let key = self.key(model);
        let generation = self.net.generation(model.origin());
        let accepted = self.accepted();
        let oldest = (self
            .net
            .journal(model.origin(), agent_id(&model.topology.agents[0].name)))
        .and_then(|journal| journal.keys().next().copied());
        let mut judged = self
            .net
            .read_two(model.origin(), model.replica(), |origin, replica| {
                let same_generation = replica.source_generation(&key).unwrap() == Some(generation);
                let block = [
                    block_contiguous,
                    block_agrees,
                    no_other_generation,
                    cursor_complete,
                ]
                .map(|promise| !same_generation || promise(origin, replica, &key));
                Judged {
                    same_generation,
                    current: same_generation && replica.current_with(&key, origin).unwrap(),
                    block,
                    journal_kept: reclaimed_below_cursor(oldest, origin.cursor(&key).unwrap()),
                    origin_row: origin.agent(&key).unwrap().map(|row| row.lifecycle),
                    replica_row: replica.agent(&key).unwrap().map(|row| row.lifecycle),
                    accepted: accepted.len(),
                    unrowed_input: accepted
                        .iter()
                        .any(|id| origin.item_by_input(&key, id).unwrap().is_none()),
                    online: false,
                }
            });
        judged.online = self.quiet()
            && (0..self.net.links()).all(|l| {
                let (a, b) = self.net.link_ends(l);
                self.net.reads_direct(a, b) && self.net.reads_direct(b, a)
            });
        self.judged = judged;
    }

    /// Nothing is left to deliver.
    fn quiet(&self) -> bool {
        self.net.deliverable().is_empty()
    }

    /// The world has settled on its own: nothing is left to deliver, no
    /// timer but a host's checkpoint is armed, every daemon is up and every
    /// link is whole, so nothing more happens until a fault or a person
    /// acts. A host's checkpoint timer alone does not hold the world back:
    /// what it does is write the drive, which no promise here reads. A path
    /// that ends with a timer the bound on advances will not let fire, a
    /// daemon down or a link severed was cut short and is not judged.
    fn at_rest(&self) -> bool {
        self.quiet()
            && !self.net.waits_on_time()
            && (0..self.net.hosts()).all(|h| self.net.is_up(h))
            && (0..self.net.links()).all(|l| !self.net.is_severed(l))
    }

    /// The person's inputs the agent accepted.
    fn accepted(&self) -> Vec<Vec<u8>> {
        (0..self.net.processes().len())
            .flat_map(|p| {
                self.net
                    .relayed(p)
                    .filter(|(_, verdict)| {
                        verdict.is_some_and(|verdict| {
                            matches!(verdict.of, Some(wire::send_input_response::Of::Accepted(_)))
                        })
                    })
                    .map(|(id, _)| id.to_vec())
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// The lane's next delivery is made.
    Deliver(Lane),
    /// The call's message at the lane's front is lost.
    Lose(Lane),
    Sever(usize),
    Restore(usize),
    Kill(usize),
    Restart(usize),
    RebootUnclean(usize),
    /// The clock moves on to the next timer armed for later.
    Advance,
    /// A person sends the agent a prompt.
    Send,
    /// A person asks the agent's host to stop it.
    Stop,
}

impl Machine for ComposedNet {
    type State = State;
    type Action = Action;

    fn init(&self) -> State {
        let topology = &self.topology;
        let mut net = Net::new(false);
        let ids: Vec<HostId> = topology.hosts.iter().map(|name| host_id(name)).collect();
        for (h, name) in topology.hosts.iter().enumerate() {
            let trusted = topology
                .links
                .iter()
                .filter_map(|&(a, b)| match (a == h, b == h) {
                    (true, _) => Some(ids[b]),
                    (_, true) => Some(ids[a]),
                    _ => None,
                })
                .collect();
            net.add_host(name, ids[h], trusted);
        }
        for &(a, b) in &topology.links {
            net.add_link(a, b);
        }
        for h in 0..topology.hosts.len() {
            net.start_host(h, [h as u8; 32]);
        }
        settle(&mut net, |_| false);
        for plan in &topology.agents {
            let agent = agent_id(&plan.name);
            net.add_agent(agent, &plan.name, &plan.script);
            let spec = agent_spec(
                agent,
                ids[plan.host],
                "/work",
                None,
                plan.prompt.as_deref(),
                AGENT_GRACE_MS,
                net.now_ms(),
            );
            let call = net.client_call(plan.host, create(&plan.name, spec));
            settle(&mut net, |net| {
                net.client(plan.host, call)
                    .is_some_and(|client| client.ended().is_some())
            });
            net.take_client(plan.host, call);
        }
        let mut state = State {
            net,
            used: Used::default(),
            seen: Seen::default(),
            judged: Judged::default(),
        };
        // A start that failed may have created nothing to judge: its
        // failure is all the properties read of it.
        if state.net.failure().is_none() {
            state.judge(self);
        } else {
            state.judged.block = [true; 4];
            state.judged.journal_kept = true;
        }
        state
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        if s.net.failure().is_some() {
            return;
        }
        for (at, lane) in s.net.deliverable().into_iter().enumerate() {
            if at == 0 || s.used.reorders < self.reorders {
                out.push(Action::Deliver(lane));
            }
            if s.used.losses < self.losses && s.net.can_lose(lane) {
                out.push(Action::Lose(lane));
            }
        }
        for l in 0..s.net.links() {
            if s.net.is_severed(l) {
                // As on a real net, a severed link comes back once both
                // ends have heard it go.
                if s.net.link_heard(l) {
                    out.push(Action::Restore(l));
                }
            } else if s.used.severs < self.severs {
                out.push(Action::Sever(l));
            }
        }
        for h in 0..s.net.hosts() {
            if !s.net.is_up(h) {
                out.push(Action::Restart(h));
                continue;
            }
            if s.used.kills < self.kills {
                out.push(Action::Kill(h));
            }
            if s.used.reboots < self.reboots && h == self.origin() {
                out.push(Action::RebootUnclean(h));
            }
        }
        if s.used.advances < self.advances && s.net.next_due().is_some() {
            out.push(Action::Advance);
        }
        let agent = agent_id(&self.topology.agents[0].name);
        let connected = s
            .net
            .process_of(agent)
            .is_some_and(|p| s.net.processes()[p].conn.is_some());
        if s.used.sends < self.sends && connected {
            out.push(Action::Send);
        }
        if s.used.stops < self.stops && s.net.is_up(self.origin()) {
            out.push(Action::Stop);
        }
    }

    fn next(&self, s: &State, action: &Action) -> Option<State> {
        let mut next = s.clone();
        let origin = self.origin();
        let agent = agent_id(&self.topology.agents[0].name);
        let flips = matches!(action, Action::Sever(_) | Action::Restore(_));
        match action {
            Action::Deliver(lane) => {
                if s.net.deliverable().first() != Some(lane) {
                    next.used.reorders += 1;
                }
                next.net.deliver(*lane);
            }
            Action::Lose(lane) => {
                next.used.losses += 1;
                next.net.lose(*lane);
            }
            Action::Sever(l) => {
                next.used.severs += 1;
                next.net.sever(*l);
            }
            Action::Restore(l) => next.net.restore(*l),
            Action::Kill(h) => {
                next.used.kills += 1;
                next.net.stop_host(*h);
            }
            Action::Restart(h) => next.net.start_host(*h, [0x5a; 32]),
            Action::RebootUnclean(h) => {
                next.used.reboots += 1;
                let severed = (0..next.net.links()).any(|l| next.net.is_severed(l));
                next.net.reboot_unclean(*h, [0xa5; 32]);
                next.seen.resetting = true;
                next.seen.reset_while_flipping |= severed;
            }
            Action::Advance => {
                next.used.advances += 1;
                let due = next.net.next_due()?;
                next.net.advance(due.saturating_sub(next.net.now_ms()));
            }
            Action::Send => {
                next.used.sends += 1;
                let p = next.net.process_of(agent)?;
                let id = format!("send-{}", next.used.sends).into_bytes();
                next.net.relay(p, prompt(&id, "more")).ok()?;
            }
            Action::Stop => {
                next.used.stops += 1;
                next.net
                    .client_call(origin, ClientRequest::StopAgent { agent });
            }
        }
        next.judge(self);
        if next.seen.resetting {
            next.seen.reset_while_flipping |= flips;
            if next.judged.same_generation {
                next.seen.resetting = false;
            }
        }
        let exited = Some(Lifecycle::Exited as i32);
        next.seen.exited_early |=
            next.judged.replica_row == exited && next.judged.origin_row != exited;
        Some(next)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut properties = vec![
            Property::always(
                "no core asks for a dump: every daemon's and agent's own step checks hold",
                |_, s: &State| !matches!(s.net.failure(), Some(Failure::Dump(_))),
            ),
            Property::always(
                "every host's store keeps every invariant its core checks after a write",
                |_, s: &State| !matches!(s.net.failure(), Some(Failure::Violation(_))),
            ),
            Property::always(
                "the drivers never meet what no driver should",
                |_, s: &State| !matches!(s.net.failure(), Some(Failure::Driver(_))),
            ),
            Property::always(BLOCK_CONTIGUOUS, |_, s: &State| s.judged.block[0]),
            Property::always(BLOCK_AGREES, |_, s: &State| s.judged.block[1]),
            Property::always(NO_OTHER_GENERATION, |_, s: &State| s.judged.block[2]),
            Property::always(CURSOR_COMPLETE, |_, s: &State| s.judged.block[3]),
            Property::always(COMMIT_BEFORE_BROADCAST, |_, s: &State| {
                !s.net.broke(COMMIT_BEFORE_BROADCAST)
            }),
            Property::always(RECLAIMED_BELOW_CURSOR, |_, s: &State| s.judged.journal_kept),
            Property::always(GENERATION_BUMPS_AFTER_UNCLEAN, |_, s: &State| {
                !s.net.broke(GENERATION_BUMPS_AFTER_UNCLEAN)
            }),
            Property::always(GENERATION_BUMPS_ONLY_AFTER_UNCLEAN, |_, s: &State| {
                !s.net.broke(GENERATION_BUMPS_ONLY_AFTER_UNCLEAN)
            }),
            Property::always(EXITS_AT_TURN_END, |_, s: &State| {
                !s.net.broke(EXITS_AT_TURN_END)
            }),
            Property::always(EXIT_CAUSE_LAST_STEP, |_, s: &State| {
                !s.net.broke(EXIT_CAUSE_LAST_STEP)
            }),
            Property::always(PRESENCE_FROM_LINKS, |_, s: &State| {
                !s.net.broke(PRESENCE_FROM_LINKS)
            }),
            Property::always(
                "a replica reads an agent exited only once the agent's own host does: the order \
                 a far host's family listing relies on, with no family here to act on it",
                |_, s: &State| !s.seen.exited_early,
            ),
            // What must hold once the world rests is judged at every state
            // where it has: an eventually would be discharged by the first
            // state on a path where its predicate held, before any fault.
            Property::always(
                "a replica whose origin and link fall quiet becomes current",
                |_, s: &State| !s.at_rest() || s.judged.current || s.judged.origin_row.is_none(),
            ),
            Property::always(
                "hosts on a working link read each other online over it once the world rests",
                |_, s: &State| !s.at_rest() || s.judged.online,
            ),
            Property::always(
                "a reset arriving while presence flips still leaves the replica current under \
                 the origin's newest generation once the world rests",
                |_, s: &State| !s.seen.reset_while_flipping || !s.at_rest() || s.judged.current,
            ),
            Property::always(
                "an accepted input is never lost: once the world rests, every prompt the agent \
                 accepted has a row on its host, unless the agent has exited and it waits for \
                 the next incarnation",
                |_, s: &State| {
                    !s.at_rest()
                        || s.judged.origin_row != Some(Lifecycle::Live as i32)
                        || !s.judged.unrowed_input
                },
            ),
            Property::sometimes("the replica is current", |_, s: &State| s.judged.current),
        ];
        // What each configuration's faults and calls should reach, so no
        // promise above passes on paths that never get there.
        if self.reboots > 0 {
            properties.push(Property::sometimes(
                "a replica takes the origin's next generation",
                |model: &Self, s: &State| {
                    s.net.generation(model.origin()) > 1 && s.judged.same_generation
                },
            ));
        }
        if self.reboots > 0 && self.severs > 0 {
            properties.push(Property::sometimes(
                "a reset arrives while presence flips, and the world comes to rest after",
                |_, s: &State| s.seen.reset_while_flipping && s.at_rest(),
            ));
        }
        if self.losses > 0 {
            properties.push(Property::sometimes(
                "the world comes to rest after a call's message is lost",
                |_, s: &State| s.used.losses > 0 && s.at_rest(),
            ));
        }
        if self.sends > 0 {
            properties.push(Property::sometimes(
                "a person's prompt is accepted and has its row once the world rests",
                |_, s: &State| s.judged.accepted > 0 && !s.judged.unrowed_input && s.at_rest(),
            ));
        }
        if self.stops > 0 {
            properties.push(Property::sometimes(
                "a stopped agent reads exited on its own host and on the replica",
                |_, s: &State| {
                    let exited = Some(Lifecycle::Exited as i32);
                    s.judged.origin_row == exited && s.judged.replica_row == exited
                },
            ));
        }
        if self.kills > 0 {
            properties.push(Property::sometimes(
                "a daemon killed and started again leaves the replica current once the world \
                 rests",
                |_, s: &State| s.used.kills > 0 && s.at_rest() && s.judged.current,
            ));
        }
        properties
    }
}

/// Makes the first lane's delivery until `done` holds, nothing is left to
/// deliver or the net fails: the one schedule a model starts from. A start
/// that fails is the initial state, judged like any other.
fn settle(net: &mut Net, done: impl Fn(&Net) -> bool) {
    for _ in 0..MOST_SETTLING {
        if done(net) || net.failure().is_some() {
            return;
        }
        let Some(&lane) = net.deliverable().first() else {
            return;
        };
        net.deliver(lane);
    }
    panic!("a net's start did not settle in {MOST_SETTLING} deliveries");
}
