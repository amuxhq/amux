//! The fleet: hosts and agent rows from the inventory stream, and the
//! families their parent edges form.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use wire::experimental::{AgentWorkspace, WorkspaceKind};
use wire::{Agent, ChangeTotals, HostEntry, InventoryEvent, inventory_event};

pub use crate::model::{AgentKey, Attention, ExitCause, HostId};
use crate::session::Connection;

/// The workspace entry a host running the workspaces experiment puts on
/// its rows. A client reads it whatever its own node runs: the node stores
/// and passes on its peers' entries as they came.
fn workspace_of(agent: &Agent) -> Option<AgentWorkspace> {
    wire::experimental::WORKSPACE.get(&agent.experimental)
}

/// What a delete removes with the agent: its workspace, and with it the
/// uncommitted changes the row last reported. A client says so before
/// confirming, and sends the delete with consent to discard them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceGoes {
    /// Its uncommitted changes, when it has any.
    pub uncommitted: Option<ChangeTotals>,
    /// The workspace's branch; none on a detached head or before the row's
    /// git facts were read.
    pub branch: Option<String>,
    /// The commits on the branch that no other branch, tag or remote holds,
    /// as the host counted them when asked: the branch keeps them, and with
    /// none it goes with the workspace. None until the host answers, or
    /// when it could not say.
    pub branch_commits: Option<u32>,
}

/// What a delete's confirmation says about the workspace that goes with the
/// agent, and so what confirming it agrees to. The row's git facts are kept
/// current only by the agent's process: for an agent without one,
/// suspended or exited, the confirmation asks the agent's host for the
/// workspace's changes against HEAD and says so until it answers. Whether
/// the branch has commits of its own the host is always asked, since a
/// merge made anywhere else can take them away
/// ([`DeleteWarning::asks_host`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DeleteWarning {
    /// What goes, from facts that are current: the row's while a process
    /// keeps them so, else the host's answer.
    Goes(WorkspaceGoes),
    /// Asking the host what is uncommitted; the branch is the row's.
    Checking(WorkspaceGoes),
    /// The host could not say: a worktree with changes stays on disk.
    Unknown(WorkspaceGoes),
}

impl DeleteWarning {
    /// Whether confirming agrees to lose uncommitted changes: only when the
    /// warning names them.
    pub fn discards(&self) -> bool {
        matches!(self, DeleteWarning::Goes(goes) if goes.uncommitted.is_some())
    }

    /// Whether the client asks the agent's host before the warning is
    /// settled: when it is still checking what is uncommitted, or when it
    /// names a branch whose own commits are not counted yet.
    pub fn asks_host(&self) -> bool {
        match self {
            DeleteWarning::Checking(_) => true,
            DeleteWarning::Goes(goes) => goes.branch.is_some() && goes.branch_commits.is_none(),
            DeleteWarning::Unknown(_) => false,
        }
    }

    /// The warning once the host answered with the workspace's changes
    /// against HEAD, or failed to. A `Checking` one then says what is
    /// uncommitted, or that the host could not say; any that names a branch
    /// takes the host's count of its own commits. A failure leaves the
    /// branch uncounted, which the warning says in words true either way.
    pub fn answered<E>(self, compared: Result<&wire::Diff, E>) -> DeleteWarning {
        let counted = |goes: WorkspaceGoes, diff: &wire::Diff| WorkspaceGoes {
            branch_commits: goes.branch.as_ref().and(diff.own_commits),
            ..goes
        };
        match (self, compared) {
            (DeleteWarning::Checking(goes), Ok(diff)) => {
                let totals = ChangeTotals {
                    files: diff.files.len() as u32,
                    added: diff.files.iter().map(|file| file.added).sum(),
                    removed: diff.files.iter().map(|file| file.removed).sum(),
                };
                DeleteWarning::Goes(WorkspaceGoes {
                    uncommitted: (totals.files > 0).then_some(totals),
                    ..counted(goes, diff)
                })
            }
            (DeleteWarning::Checking(goes), Err(_)) => DeleteWarning::Unknown(goes),
            // What is uncommitted stays the row's: it is what the warning
            // already drew, and so what confirming it agrees to.
            (DeleteWarning::Goes(goes), Ok(diff)) => DeleteWarning::Goes(counted(goes, diff)),
            (warning, _) => warning,
        }
    }
}

/// Whether the agent has a process running, which keeps its row's git facts
/// current: not exited, and not suspended.
pub fn has_process(agent: &Agent) -> bool {
    agent.lifecycle() == wire::Lifecycle::Live && agent.exit_cause() != wire::ExitCause::Suspended
}

/// The key naming an agent's row.
/// What a person reads the agent as: its title when it has one, else its
/// name. Addressing an agent, or renaming it, still takes the name.
pub fn shown_name(agent: &Agent) -> &str {
    agent.title.as_deref().unwrap_or(&agent.name)
}

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
        // A phase from a newer build reads as the quietest one.
        wire::Phase::Idle | wire::Phase::Unspecified => Attention::Idle,
        wire::Phase::Working => Attention::Working,
        wire::Phase::NeedsYou => Attention::NeedsYou,
    }
}

/// How an agent ended, from the cause its host recorded: finished for a
/// one-shot agent that ended after its turn; ended for a stop, and for a
/// cause unknown or not given, since unknown is not failure; anything else
/// failed, in the cause's words followed by its own.
pub fn exit_cause(agent: &Agent) -> ExitCause {
    if wire::ExitCause::failed(agent.exit_cause) {
        ExitCause::Failed(exit_words(agent.exit_cause, agent.exit_detail.as_deref()))
    } else if agent.exit_cause == wire::ExitCause::Finished as i32 {
        ExitCause::Finished
    } else {
        ExitCause::Ended
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
        Ok(ExitCause::Quit) => "quit",
        Ok(ExitCause::ProviderExited) => "the provider exited",
        // Both read as the person sees them: the agent ended while no
        // daemon was there, whether it drained or was found dead.
        Ok(ExitCause::DaemonLost | ExitCause::EndedWhileAway) => "while the daemon was away",
        Ok(ExitCause::Orphaned) => "orphaned while waiting for you",
        Ok(ExitCause::CouldNotStart) => "could not start",
        Ok(ExitCause::DirectoryFailed) => "its directory failed",
        Ok(ExitCause::EndedUnexpectedly) => "ended unexpectedly",
        Ok(ExitCause::Suspended) => "suspended",
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
    /// This client's close of the agent never left: its host could not be
    /// reached. It is held, and goes when the host is back. `listing` is
    /// [`FleetState::listings`] as the close's call left: a listing finished
    /// since then is word that the host is back.
    HoldClose {
        agent: AgentKey,
        listing: u64,
    },
    /// The person cancelled a held close, or it went.
    DropClose(AgentKey),
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
    /// Closes this client holds because the agent's host could not be
    /// reached: each goes again once the host is next listed online.
    held_closes: BTreeSet<AgentKey>,
    /// Listings finished: every CaughtUp, the first included.
    listings: u64,
    /// Held closes whose host is back, for the driver to send again.
    released_closes: Vec<AgentKey>,
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

    /// Whether this client holds a close of the agent until its host is back.
    pub fn close_held(&self, agent: &AgentKey) -> bool {
        self.held_closes.contains(agent)
    }

    /// How many listings of the inventory have finished, the first
    /// included; a close's call notes it as it leaves.
    pub fn listings(&self) -> u64 {
        self.listings
    }

    fn host_online(&self, host: &[u8]) -> bool {
        self.hosts
            .get(host)
            .is_some_and(|host| host.online().is_some())
    }

    /// The held closes whose host is back since this was last asked: each is
    /// no longer held, and the driver sends it again.
    pub fn take_released_closes(&mut self) -> Vec<AgentKey> {
        std::mem::take(&mut self.released_closes)
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

    /// Whether deleting the agent removes its workspace, and what it loses
    /// with it: none when the workspace stays. It goes with its last member
    /// when its host made it. The agent's own descendants go with it, so a
    /// member among them does not keep the workspace; any other member does.
    /// What it reports is the row's last reading; the host checks again
    /// when it removes the workspace.
    pub fn delete_removes_workspace(&self, agent: &AgentKey) -> Option<WorkspaceGoes> {
        let row = self.agents.get(agent)?;
        let entry = workspace_of(row)?;
        if !entry.made_by_amux || entry.kind() == WorkspaceKind::Folder {
            return None;
        }
        let going: BTreeSet<AgentKey> = self.family(agent).into_iter().map(agent_key).collect();
        let shared = self.agents.iter().any(|(key, other)| {
            key.host == agent.host
                && !going.contains(key)
                && workspace_of(other)
                    .is_some_and(|theirs| theirs.kind == entry.kind && theirs.id == entry.id)
        });
        if shared {
            return None;
        }
        let git = row.git.as_ref();
        let uncommitted = git
            .and_then(|git| git.uncommitted)
            .filter(|totals| totals.files > 0);
        // Named whenever the row has one: equal totals on the branch and
        // uncommitted do not prove it has no commits of its own (a line
        // committed, then changed again uncommitted), and the row carries
        // no count of commits; the host's answer says how many.
        let branch = git.and_then(|git| git.branch.clone());
        Some(WorkspaceGoes {
            uncommitted,
            branch,
            branch_commits: None,
        })
    }

    /// What a delete's confirmation starts by saying about the agent's
    /// workspace: none when it stays; `Checking` when the agent has no
    /// process to keep its row's facts current, so the client must ask the
    /// host ([`DeleteWarning::answered`]).
    pub fn delete_warning(&self, agent: &AgentKey) -> Option<DeleteWarning> {
        let goes = self.delete_removes_workspace(agent)?;
        let row = self.agents.get(agent)?;
        Some(if has_process(row) {
            DeleteWarning::Goes(goes)
        } else {
            DeleteWarning::Checking(WorkspaceGoes {
                uncommitted: None,
                ..goes
            })
        })
    }

    /// The agent's children as a fleet lists them. A closed child is done
    /// with: its parent stopped it, or the person closed it. It is kept
    /// only so its parent can start it again, until retention reclaims it,
    /// so no client lists it under its family or counts it there.
    pub fn listed_children(&self, parent: &AgentKey) -> impl DoubleEndedIterator<Item = &AgentKey> {
        self.families.children(parent).filter(|child| {
            self.agents
                .get(*child)
                .is_none_or(|child| child.filing() != wire::Filing::Closed)
        })
    }

    /// The agent and every descendant a fleet lists, depth first: its
    /// family without the closed children, and what is under them.
    pub fn listed_family(&self, agent: &AgentKey) -> Vec<&Agent> {
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
            stack.extend(self.listed_children(&at).rev().cloned());
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
            // The call failed for want of the host, so the host counts as
            // away from then on whatever its row reads: this client's own
            // daemon restarting fails the call while the host still reads
            // online, and the reopened stream may only ever say online.
            // A listing finished since the call left already said it is
            // back, and the close goes at once.
            FleetMsg::HoldClose { agent, listing } => {
                if listing != self.listings && self.caught_up && self.host_online(&agent.host) {
                    self.released_closes.push(agent.clone());
                } else {
                    self.held_closes.insert(agent.clone());
                }
                changed.insert(agent);
            }
            FleetMsg::DropClose(agent) => {
                if self.held_closes.remove(&agent) {
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
                            hosts.insert(host.host_id().to_vec());
                        }
                        for agent in self
                            .agents
                            .keys()
                            .filter(|agent| agent.host == host.host_id())
                        {
                            changed.insert(agent.clone());
                        }
                        let online = host.online().is_some();
                        let host_id = host.host_id().to_vec();
                        self.hosts.insert(host.host_id().to_vec(), host);
                        if online {
                            self.release_closes(|agent| agent.host == host_id, &mut changed);
                        }
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
                        self.listings += 1;
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
                        // A close held after this listing's host row went by
                        // goes once the listing ends.
                        let online: BTreeSet<HostId> = self
                            .hosts
                            .keys()
                            .filter(|host| self.host_online(host))
                            .cloned()
                            .collect();
                        self.release_closes(|agent| online.contains(&agent.host), &mut changed);
                    }
                }
            }
        }
        changed.into_iter().collect()
    }

    /// Hands the held closes `which` picks to the driver to send again, and
    /// marks each agent's row changed: it no longer reads held.
    fn release_closes(
        &mut self,
        which: impl Fn(&AgentKey) -> bool,
        changed: &mut BTreeSet<AgentKey>,
    ) {
        let back: Vec<AgentKey> = self
            .held_closes
            .iter()
            .filter(|agent| which(agent))
            .cloned()
            .collect();
        for agent in back {
            self.held_closes.remove(&agent);
            changed.insert(agent.clone());
            self.released_closes.push(agent);
        }
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
        // A held close of an agent already closed, or gone, has nothing
        // left to do.
        if agent
            .as_ref()
            .is_none_or(|agent| agent.filing() == wire::Filing::Closed)
        {
            self.held_closes.remove(&at);
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
