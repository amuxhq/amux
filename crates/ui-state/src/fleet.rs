//! The fleet: hosts and agent rows from the inventory stream, and the
//! families their parent edges form.

use std::collections::{BTreeMap, BTreeSet};

pub use model::{AgentKey, Attention, ExitCause, HostId};
use wire::{Agent, HostEntry, InventoryEvent, inventory_event};

use crate::session::Connection;

/// The key naming an agent's row.
pub fn agent_key(agent: &Agent) -> AgentKey {
    AgentKey {
        host: agent.host_id.clone(),
        agent: agent.agent_id.clone(),
    }
}

/// The key of an agent's parent, if it has one.
pub fn parent_key(agent: &Agent) -> Option<AgentKey> {
    agent.parent.as_ref().map(|parent| AgentKey {
        host: parent.host_id.clone(),
        agent: parent.agent_id.clone(),
    })
}

/// How loudly the row asks for the person.
pub fn attention(agent: &Agent) -> Attention {
    if agent.lifecycle() == wire::Lifecycle::Exited {
        return Attention::Exited;
    }
    match agent.phase() {
        wire::Phase::Starting => Attention::Starting,
        wire::Phase::Idle => Attention::Idle,
        wire::Phase::Working => Attention::Working,
        wire::Phase::NeedsYou => Attention::NeedsYou,
    }
}

/// How an agent ended, from the cause its host recorded: finished for a
/// one-shot agent that ended after its turn; ended for a stop, and for a
/// cause unknown or not given, since unknown is not failure; anything else
/// failed, in the cause's words followed by its own.
pub fn exit_cause(agent: &Agent) -> ExitCause {
    match wire::ExitCause::try_from(agent.exit_cause) {
        Ok(wire::ExitCause::Finished) => ExitCause::Finished,
        Ok(wire::ExitCause::Stopped | wire::ExitCause::Killed | wire::ExitCause::Unspecified)
        | Err(_) => ExitCause::Ended,
        Ok(_) => ExitCause::Failed(exit_words(agent.exit_cause, agent.exit_detail.as_deref())),
    }
}

/// A cause as a person reads it after the word "exited", then its own
/// words when it has any ("the provider exited: exit code 1"). A cause this
/// build does not know shows only its own words: unknown is not failure.
pub fn exit_words(cause: i32, detail: Option<&str>) -> String {
    use wire::ExitCause;
    let words = match ExitCause::try_from(cause) {
        Ok(ExitCause::Unspecified) | Err(_) => "",
        Ok(ExitCause::Finished) => "finished",
        Ok(ExitCause::Stopped) => "stopped",
        Ok(ExitCause::Killed) => "killed",
        Ok(ExitCause::ProviderExited) => "the provider exited",
        // Both read as the person sees them: the agent ended while no
        // daemon was there, whether it drained or was found dead.
        Ok(ExitCause::DaemonLost | ExitCause::EndedWhileAway) => "while the daemon was away",
        Ok(ExitCause::Orphaned) => "orphaned while waiting for you",
        Ok(ExitCause::CouldNotStart) => "could not start",
        Ok(ExitCause::DirectoryFailed) => "its directory failed",
        Ok(ExitCause::EndedUnexpectedly) => "ended unexpectedly",
    };
    match detail.filter(|detail| !detail.is_empty()) {
        Some(detail) if words.is_empty() => detail.to_owned(),
        Some(detail) => format!("{words}: {detail}"),
        None => words.to_owned(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum FleetMsg {
    Event(Box<InventoryEvent>),
    Connection(Connection),
    /// This client's stop of the agent never left: its host could not be
    /// reached. It is held, and goes when the host is back.
    HoldStop(AgentKey),
    /// The person cancelled a held stop, or it went.
    DropStop(AgentKey),
}

/// Parent edges, derived from the agent rows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Families {
    children: BTreeMap<AgentKey, BTreeSet<AgentKey>>,
}

impl Families {
    pub fn children(&self, parent: &AgentKey) -> impl DoubleEndedIterator<Item = &AgentKey> {
        self.children.get(parent).into_iter().flatten()
    }
}

/// The fleet. No I/O; rebuilt from the inventory stream on every open.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FleetState {
    hosts: BTreeMap<HostId, HostEntry>,
    agents: BTreeMap<AgentKey, Agent>,
    families: Families,
    caught_up: bool,
    connection: Connection,
    /// Rows re-listed since the stream reopened; the rest are dropped at the
    /// next CaughtUp.
    relisted: Option<(BTreeSet<HostId>, BTreeSet<AgentKey>)>,
    /// Stops this client holds because the agent's host could not be
    /// reached, each with whether the host has been seen out of reach since:
    /// the stop goes again once it is seen back.
    held_stops: BTreeMap<AgentKey, bool>,
    /// Held stops whose host is back, for the driver to send again.
    released_stops: Vec<AgentKey>,
}

impl FleetState {
    pub fn new() -> FleetState {
        FleetState::default()
    }

    pub fn caught_up(&self) -> bool {
        self.caught_up
    }

    pub fn connection(&self) -> Connection {
        self.connection
    }

    pub fn hosts(&self) -> impl Iterator<Item = &HostEntry> {
        self.hosts.values()
    }

    pub fn host(&self, host: &[u8]) -> Option<&HostEntry> {
        self.hosts.get(host)
    }

    pub fn agents(&self) -> impl Iterator<Item = &Agent> {
        self.agents.values()
    }

    pub fn agent(&self, agent: &AgentKey) -> Option<&Agent> {
        self.agents.get(agent)
    }

    /// An agent by id alone, on whichever host holds it.
    pub fn find(&self, agent_id: &[u8]) -> Option<&Agent> {
        self.agents
            .values()
            .find(|agent| agent.agent_id == agent_id)
    }

    pub fn families(&self) -> &Families {
        &self.families
    }

    /// Whether this client holds a stop of the agent until its host is back.
    pub fn stop_held(&self, agent: &AgentKey) -> bool {
        self.held_stops.contains_key(agent)
    }

    /// The held stops whose host is back since this was last asked: each is
    /// no longer held, and the driver sends it again.
    pub fn take_released_stops(&mut self) -> Vec<AgentKey> {
        std::mem::take(&mut self.released_stops)
    }

    /// The agent's parent, when the fleet holds it.
    pub fn parent(&self, agent: &AgentKey) -> Option<&Agent> {
        let parent = parent_key(self.agents.get(agent)?)?;
        self.agents.get(&parent)
    }

    /// The top of the agent's family: the furthest ancestor the fleet holds.
    pub fn root(&self, agent: &AgentKey) -> AgentKey {
        let mut at = agent.clone();
        let mut seen = BTreeSet::new();
        while let Some(parent) = self.agents.get(&at).and_then(parent_key) {
            if !self.agents.contains_key(&parent) || !seen.insert(at.clone()) {
                break;
            }
            at = parent;
        }
        at
    }

    /// Agents that head a family: no parent, or a parent the fleet does not
    /// hold.
    pub fn roots(&self) -> impl Iterator<Item = &Agent> {
        self.agents.values().filter(|agent| {
            parent_key(agent).is_none_or(|parent| !self.agents.contains_key(&parent))
        })
    }

    /// The agent and every descendant, depth first.
    pub fn family(&self, agent: &AgentKey) -> Vec<&Agent> {
        let mut out = Vec::new();
        let mut stack = vec![agent.clone()];
        let mut seen = BTreeSet::new();
        while let Some(at) = stack.pop() {
            if !seen.insert(at.clone()) {
                continue;
            }
            if let Some(agent) = self.agents.get(&at) {
                out.push(agent);
            }
            stack.extend(self.families.children(&at).rev().cloned());
        }
        out
    }

    /// How loudly the agent's family asks for the person: its loudest
    /// member, the agent included.
    pub fn family_attention(&self, agent: &AgentKey) -> Option<Attention> {
        self.family(agent).into_iter().map(attention).max()
    }

    /// Applies one message and returns the agents whose card or family
    /// attention may differ.
    pub fn update(&mut self, msg: FleetMsg) -> Vec<AgentKey> {
        let mut changed = BTreeSet::new();
        match msg {
            FleetMsg::HoldStop(agent) => {
                let away = self
                    .agents
                    .get(&agent)
                    .and_then(|row| self.hosts.get(&row.host_id))
                    .is_none_or(|host| host.presence() != wire::Presence::Online);
                self.held_stops.insert(agent.clone(), away);
                changed.insert(agent);
            }
            FleetMsg::DropStop(agent) => {
                if self.held_stops.remove(&agent).is_some() {
                    changed.insert(agent);
                }
            }
            FleetMsg::Connection(connection) => {
                self.connection = connection;
                if connection == Connection::Reconnecting {
                    self.caught_up = false;
                    self.relisted = Some(Default::default());
                }
            }
            FleetMsg::Event(event) => {
                let Some(event) = event.of else {
                    return Vec::new();
                };
                match event {
                    inventory_event::Of::Host(host) => {
                        if let Some((hosts, _)) = &mut self.relisted {
                            hosts.insert(host.host_id.clone());
                        }
                        for agent in self
                            .agents
                            .keys()
                            .filter(|agent| agent.host == host.host_id)
                        {
                            changed.insert(agent.clone());
                        }
                        let online = host.presence() == wire::Presence::Online;
                        let mut back = Vec::new();
                        for (agent, away) in self
                            .held_stops
                            .iter_mut()
                            .filter(|(agent, _)| agent.host == host.host_id)
                        {
                            if !online {
                                *away = true;
                            } else if *away {
                                back.push(agent.clone());
                            }
                        }
                        for agent in back {
                            self.held_stops.remove(&agent);
                            self.released_stops.push(agent);
                        }
                        self.hosts.insert(host.host_id.clone(), host);
                    }
                    inventory_event::Of::HostRemoved(removed) => {
                        self.hosts.remove(&removed.host_id);
                        for agent in self
                            .agents
                            .keys()
                            .filter(|agent| agent.host == removed.host_id)
                        {
                            changed.insert(agent.clone());
                        }
                    }
                    inventory_event::Of::Agent(agent) => {
                        let at = agent_key(&agent);
                        if let Some((_, agents)) = &mut self.relisted {
                            agents.insert(at.clone());
                        }
                        self.put(at, Some(agent), &mut changed);
                    }
                    inventory_event::Of::AgentRemoved(removed) => {
                        let at = AgentKey {
                            host: removed.host_id,
                            agent: removed.agent_id,
                        };
                        self.put(at, None, &mut changed);
                    }
                    inventory_event::Of::Lagged(_) => {
                        // The stream closes and a fresh listing follows at
                        // once; what it no longer names goes at its CaughtUp.
                        self.caught_up = false;
                        self.relisted = Some(Default::default());
                    }
                    inventory_event::Of::CaughtUp(_) => {
                        self.caught_up = true;
                        if let Some((hosts, agents)) = self.relisted.take() {
                            self.hosts.retain(|host, _| hosts.contains(host));
                            let gone: Vec<AgentKey> = self
                                .agents
                                .keys()
                                .filter(|agent| !agents.contains(agent))
                                .cloned()
                                .collect();
                            for at in gone {
                                self.put(at, None, &mut changed);
                            }
                        }
                    }
                }
            }
        }
        changed.into_iter().collect()
    }

    /// Replaces or removes one row, keeping the parent edges, and marks the
    /// row and every ancestor whose family attention may move.
    fn put(&mut self, at: AgentKey, agent: Option<Agent>, changed: &mut BTreeSet<AgentKey>) {
        let old_parent = self.agents.get(&at).and_then(parent_key);
        if let Some(parent) = &old_parent {
            self.mark_ancestors(parent, changed);
            if let Some(children) = self.families.children.get_mut(parent) {
                children.remove(&at);
                if children.is_empty() {
                    self.families.children.remove(parent);
                }
            }
        }
        changed.insert(at.clone());
        // A held stop of an agent that has ended, or is gone, has nothing
        // left to stop.
        if agent
            .as_ref()
            .is_none_or(|agent| agent.lifecycle() == wire::Lifecycle::Exited)
        {
            self.held_stops.remove(&at);
        }
        match agent {
            Some(agent) => {
                let parent = parent_key(&agent);
                self.agents.insert(at.clone(), agent);
                if let Some(parent) = parent {
                    self.families
                        .children
                        .entry(parent.clone())
                        .or_default()
                        .insert(at.clone());
                    self.mark_ancestors(&parent, changed);
                }
            }
            None => {
                self.agents.remove(&at);
                // Children keep their edge: they head their own family until
                // the parent returns.
                for child in self.families.children(&at) {
                    changed.insert(child.clone());
                }
            }
        }
    }

    fn mark_ancestors(&self, from: &AgentKey, changed: &mut BTreeSet<AgentKey>) {
        let mut at = Some(from.clone());
        while let Some(agent) = at {
            if !changed.insert(agent.clone()) {
                break;
            }
            at = self.agents.get(&agent).and_then(parent_key);
        }
    }
}
