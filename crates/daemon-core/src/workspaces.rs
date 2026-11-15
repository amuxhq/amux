//! The workspaces experiment (`workspaces/1`): every own agent is a member
//! of the workspace its folder is in, and the lifecycle of those workspaces
//! is [`workspace_core`]'s, composed here.
//!
//! This module feeds the workspace core what happens to the registry's
//! agents (one created in a new workspace or an existing one, about to
//! launch, no longer awake, deleted), turns its outputs into the driver's
//! jobs, and hands back "this member may launch" to the registry, which
//! waits for it before it spawns a process. It also writes each member's
//! workspace into the experiment's store table, from which every row read
//! carries it as `Agent.experimental["workspaces/1"]`.
//!
//! As the daemon starts nothing of the workspace core carries over: once
//! the start's sweep is done, each own agent's folder is resolved to its
//! workspace again (one job per distinct folder, those with a running
//! process first), each agent added as it is resolved, and then the
//! workspaces this host made on disk are swept, each without a member
//! removed. A launch asked for before its agent is resolved waits for the
//! resolution.
//!
//! With the experiment off none of this runs: the registry spawns at once
//! and a new worktree is made before the row, as without the experiment.

use std::collections::{BTreeMap, BTreeSet};

use prost::Message as _;
use serde::{Deserialize, Serialize};
use store::StoreError;
use workspace_core::{Input as Core, MemberId, Output as Wants, View, Workspaces};
pub use workspace_core::{Key, Kind, Plan, Removal};

use crate::families::Refusal;
use crate::ids::{AgentId, JobId};
use crate::io::{Job, Output};
use crate::payload::JobResult;
use crate::{Daemon, codec};

/// The experiment version whose shapes this module reads and writes.
pub const VERSION: &str = store::experiments::WORKSPACES_1.version;

/// A folder's workspace, as the driver resolved it from what is on disk.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Resolved {
    pub workspace: Key,
    /// The workspace says on disk that this host made it.
    pub made_here: bool,
}

/// A new workspace as the driver planned it, without touching the network.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Planned {
    /// The workspace it will be: its kind, and its root as the agent's
    /// folder.
    pub workspace: Key,
    pub plan: Plan,
    /// The branch the new one starts from, as the agent's git facts measure
    /// it: what the creating folder has checked out.
    pub base_branch: Option<String>,
}

/// Why a new workspace could not be planned.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PlanRefusal {
    /// Its name, folder or branch is taken: the next free name is tried.
    Taken(String),
    /// It cannot be made, as the driver says it to the caller.
    Refused(#[serde(with = "codec::message")] wire::Error),
}

/// What the driver does for the workspaces, each answered as
/// [`JobResult::Workspace`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceJob {
    /// Plan a new workspace of the repository `folder` is in, named `name`.
    Plan { folder: String, name: String },
    /// Resolve `folder` to its workspace.
    Resolve { folder: String },
    /// List the workspaces this host made, as they say on disk.
    Owned,
    /// Make the workspace by its plan.
    Make { workspace: Key, plan: Plan },
    /// Take amux's claim on the workspace: registered before the driver
    /// performs anything the core asked for after it, and answered once
    /// held.
    TakeClaim { workspace: Key },
    /// Release amux's claim, at once; not answered.
    ReleaseClaim { workspace: Key },
    /// Run its start hook.
    Start { workspace: Key },
    /// Stop it once nothing else holds it.
    StopWhenUnheld { workspace: Key },
    /// Remove it, keeping it and clearing its owner when anyone holds it or
    /// it has changes and `discard` is false.
    Delete { workspace: Key, discard: bool },
}

/// What a [`WorkspaceJob`] came to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum WorkspaceDone {
    Planned(Result<Planned, PlanRefusal>),
    Resolved {
        folder: String,
        resolved: Resolved,
    },
    Owned(Vec<Key>),
    Made {
        workspace: Key,
        result: Result<(), String>,
    },
    Claimed {
        workspace: Key,
        result: Result<(), String>,
    },
    Started {
        workspace: Key,
        result: Result<(), String>,
    },
    Stopped {
        workspace: Key,
        result: Result<(), String>,
    },
    Deleted {
        workspace: Key,
        removal: Removal,
    },
}

/// The experiment's state in the daemon core.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceState {
    core: Workspaces,
    /// Where the start's resolution of every own agent's folder is.
    start: Start,
    /// Launches asked for before their agent was resolved.
    deferred: BTreeSet<AgentId>,
    /// The jobs outstanding, so a checker can renumber them.
    jobs: BTreeSet<JobId>,
}

/// The start's resolution of every own agent's folder.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
enum Start {
    /// The start's sweep is not done yet.
    #[default]
    Before,
    /// Each folder's resolve job outstanding, with the agents in it, and
    /// the owned workspaces once listed.
    Resolving {
        folders: BTreeMap<String, Vec<AgentId>>,
        owned: Option<Vec<Key>>,
    },
    /// Every own agent is a member, and the sweep of what this host made is
    /// done.
    Done,
}

impl WorkspaceState {
    /// The jobs the experiment waits on.
    pub(crate) fn jobs(&self, out: &mut Vec<JobId>) {
        out.extend(self.jobs.iter().copied());
    }

    /// The state with the jobs it waits on under canonical ids.
    pub(crate) fn canonical(&self, ids: &crate::fingerprint::Ids) -> WorkspaceState {
        WorkspaceState {
            jobs: self.jobs.iter().map(|job| ids.jobs.get(*job)).collect(),
            ..self.clone()
        }
    }

    /// The workspace core, for reading.
    pub fn core(&self) -> &Workspaces {
        &self.core
    }
}

fn member(agent: AgentId) -> MemberId {
    MemberId(agent.0)
}

/// The wire's shape of a workspace as its members' rows show it.
pub fn to_entry(view: &View) -> wire::experimental::AgentWorkspace {
    use wire::experimental::WorkspaceKind;
    wire::experimental::AgentWorkspace {
        kind: match view.workspace.kind {
            Kind::Folder => WorkspaceKind::Folder,
            Kind::Git => WorkspaceKind::Git,
            Kind::Wt => WorkspaceKind::Wt,
        } as i32,
        id: view.workspace.id.clone(),
        made_by_amux: view.made_here,
        creating: view.creating,
        problem: view.problem.clone(),
    }
}

/// The member's workspace as its row carries it, if it carries one.
pub fn entry_of(row: &store::AgentRow) -> Option<wire::experimental::AgentWorkspace> {
    let bytes = row.experimental.get(VERSION)?;
    wire::experimental::AgentWorkspace::decode(bytes.as_slice()).ok()
}

/// Whether an agent about to launch may.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// At once.
    Now,
    /// Once its workspace is made or claimed:
    /// [`Daemon::workspace_launched`] or [`Daemon::workspace_not_made`]
    /// follows.
    Later,
    /// Never: its workspace could not be made, or is gone, as `detail`
    /// says.
    Never(String),
}

impl Daemon {
    /// Whether this daemon runs the workspaces experiment.
    pub(crate) fn workspaces_on(&self) -> bool {
        self.options.workspaces
    }

    /// The experiment's state, for reading.
    pub fn workspaces(&self) -> &WorkspaceState {
        &self.workspaces
    }

    fn workspace_job(&mut self, job: WorkspaceJob, out: &mut Vec<Output>) -> JobId {
        let id = self.pending.mint();
        // A release is not answered: nothing waits on it.
        if !matches!(job, WorkspaceJob::ReleaseClaim { .. }) {
            self.workspaces.jobs.insert(id);
        }
        out.push(Output::Job {
            id,
            op: Job::Workspace(job),
        });
        id
    }

    /// Asks the driver to plan the new workspace a create names, for the
    /// registry's lane, which waits on the job.
    pub(crate) fn plan_workspace(
        &mut self,
        folder: &str,
        name: &str,
        out: &mut Vec<Output>,
    ) -> JobId {
        let job = WorkspaceJob::Plan {
            folder: folder.to_owned(),
            name: name.to_owned(),
        };
        let id = self.workspace_job(job, out);
        // The registry's lane answers to it, not this module.
        self.workspaces.jobs.remove(&id);
        id
    }

    /// Steps the workspace core with one input and carries out what it
    /// wants. What it says of `launching` is returned rather than handed to
    /// the registry: the registry is about to spawn that agent itself.
    fn feed(&mut self, input: Core, launching: Option<AgentId>, out: &mut Vec<Output>) -> Verdict {
        let mut wants = Vec::new();
        self.workspaces.core.step(input, &mut wants);
        let mut verdict = Verdict::Later;
        for want in wants {
            match want {
                Wants::Launch { member } if Some(AgentId(member.0)) == launching => {
                    verdict = Verdict::Now;
                }
                Wants::NotMade { member, detail } if Some(AgentId(member.0)) == launching => {
                    verdict = Verdict::Never(detail);
                }
                want => self.perform(want, out),
            }
        }
        if let Err(broken) = self.workspaces.core.check_invariants() {
            self.touched.workspace_broken = Some(broken);
        }
        verdict
    }

    fn perform(&mut self, want: Wants, out: &mut Vec<Output>) {
        match want {
            Wants::Make { workspace, plan } => {
                self.workspace_job(WorkspaceJob::Make { workspace, plan }, out);
            }
            Wants::TakeClaim { workspace } => {
                self.workspace_job(WorkspaceJob::TakeClaim { workspace }, out);
            }
            Wants::ReleaseClaim { workspace } => {
                self.workspace_job(WorkspaceJob::ReleaseClaim { workspace }, out);
            }
            Wants::Start { workspace } => {
                self.workspace_job(WorkspaceJob::Start { workspace }, out);
            }
            Wants::StopWhenUnheld { workspace } => {
                self.workspace_job(WorkspaceJob::StopWhenUnheld { workspace }, out);
            }
            Wants::Delete { workspace, discard } => {
                self.workspace_job(WorkspaceJob::Delete { workspace, discard }, out);
            }
            Wants::Launch { member } => self.workspace_launched(AgentId(member.0), out),
            Wants::NotMade { member, detail } => {
                self.workspace_not_made(AgentId(member.0), detail, out)
            }
            Wants::Publish { view } => {
                for member in self.workspaces.core.members(&view.workspace) {
                    self.sync_entry(AgentId(member.0), out);
                }
            }
        }
    }

    /// Writes the member's workspace into the experiment's table as the
    /// core now sees it, and tells subscribers its row, when that changed.
    fn sync_entry(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        let Some(view) = self.workspaces.core.view_of(member(agent)) else {
            return;
        };
        let key = self.own_key(agent);
        let written = (|| {
            let Some(row) = self.store.agent(&key)? else {
                return Ok(false);
            };
            let entry = to_entry(&view).encode_to_vec();
            if row.experimental.get(VERSION) == Some(&entry) {
                return Ok(false);
            }
            self.store.put_experimental(&key, VERSION, Some(&entry))?;
            Ok::<_, StoreError>(true)
        })();
        match written {
            Ok(true) => {
                if let Err(error) = self.publish_row(&key, out) {
                    tracing::warn!(%error, "publishing an agent's workspace failed");
                }
            }
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, "writing an agent's workspace failed"),
        }
    }

    // --- what the registry tells it ------------------------------------------

    /// A new agent's row is written in a workspace its create is to make by
    /// `planned`; it launches once the workspace is made.
    pub(crate) fn member_created(
        &mut self,
        agent: AgentId,
        planned: &Planned,
        out: &mut Vec<Output>,
    ) {
        self.feed(
            Core::Create {
                member: member(agent),
                workspace: planned.workspace.clone(),
                plan: planned.plan.clone(),
            },
            None,
            out,
        );
        self.sync_entry(agent, out);
        self.feed(
            Core::Launching {
                member: member(agent),
            },
            None,
            out,
        );
    }

    /// A new agent's row is written in an existing workspace, as the shell
    /// resolved its folder.
    pub(crate) fn member_added(
        &mut self,
        agent: AgentId,
        resolved: &Resolved,
        out: &mut Vec<Output>,
    ) {
        self.feed(
            Core::Added {
                member: member(agent),
                workspace: resolved.workspace.clone(),
                made_here: resolved.made_here,
                running: false,
            },
            None,
            out,
        );
        self.sync_entry(agent, out);
    }

    /// The registry is about to launch the agent's process. Always at once
    /// with the experiment off.
    pub(crate) fn may_launch(&mut self, agent: AgentId, out: &mut Vec<Output>) -> Verdict {
        if !self.workspaces_on() {
            return Verdict::Now;
        }
        if self.workspaces.core.workspace_of(member(agent)).is_none() {
            if self.workspaces.start != Start::Done {
                self.workspaces.deferred.insert(agent);
                return Verdict::Later;
            }
            // Every own agent is a member once the start is done; one that
            // is not launches as without the experiment.
            tracing::warn!(agent = %crate::hex(&agent.0), "launching an agent in no workspace");
            return Verdict::Now;
        }
        self.feed(
            Core::Launching {
                member: member(agent),
            },
            Some(agent),
            out,
        )
    }

    /// The agent's process is gone, or a launch it waited for was given up.
    pub(crate) fn member_slept(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        if !self.workspaces_on() {
            return;
        }
        self.workspaces.deferred.remove(&agent);
        self.feed(
            Core::Slept {
                member: member(agent),
            },
            None,
            out,
        );
    }

    /// The agent's row was deleted: a delete, its parent's or retention's,
    /// none of which may discard its workspace's changes yet.
    pub(crate) fn member_removed(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        if !self.workspaces_on() {
            return;
        }
        self.workspaces.deferred.remove(&agent);
        self.feed(
            Core::Removed {
                member: member(agent),
                discard: false,
            },
            None,
            out,
        );
    }

    /// A launch read the workspace's environment, or it would not load.
    pub(crate) fn workspace_environment(
        &mut self,
        agent: AgentId,
        result: Result<(), String>,
        out: &mut Vec<Output>,
    ) {
        if !self.workspaces_on() {
            return;
        }
        let Some(workspace) = self.workspaces.core.workspace_of(member(agent)).cloned() else {
            return;
        };
        self.feed(Core::Environment { workspace, result }, None, out);
    }

    /// The workspace the agent is in, while the core holds it.
    pub(crate) fn workspace_of(&self, agent: AgentId) -> Option<Key> {
        if !self.workspaces_on() {
            return None;
        }
        self.workspaces.core.workspace_of(member(agent)).cloned()
    }

    /// Whether the agent's row says its workspace is still being made, as
    /// the row carries it: what a restarted daemon finds of a create it was
    /// making.
    pub(crate) fn row_was_creating(&self, row: &store::AgentRow) -> bool {
        self.workspaces_on() && entry_of(row).is_some_and(|entry| entry.creating)
    }

    // --- the start ---------------------------------------------------------------

    /// The start's sweep has reported every agent: each own agent's folder
    /// is resolved, those with a running process first, and the workspaces
    /// this host made are listed.
    pub(crate) fn resolve_members(&mut self, out: &mut Vec<Output>) {
        if !self.workspaces_on() || self.workspaces.start != Start::Before {
            return;
        }
        let own = self.store.own_host().to_vec();
        let rows = match self.store.agents() {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "reading the agents to resolve their workspaces failed");
                Vec::new()
            }
        };
        let mut folders: BTreeMap<String, Vec<AgentId>> = BTreeMap::new();
        for row in rows.into_iter().filter(|row| row.agent.host == own) {
            let Some(agent) = AgentId::from_slice(&row.agent.agent) else {
                continue;
            };
            // Created since the daemon started: it joined as it was made.
            if self.workspaces.core.workspace_of(member(agent)).is_some() {
                continue;
            }
            folders.entry(row.cwd).or_default().push(agent);
        }
        let running: BTreeSet<AgentId> = self.families.processes().collect();
        let (mut awake, mut asleep): (Vec<_>, Vec<_>) = folders
            .keys()
            .cloned()
            .partition(|folder| folders[folder].iter().any(|agent| running.contains(agent)));
        awake.append(&mut asleep);
        for folder in awake {
            self.workspace_job(WorkspaceJob::Resolve { folder }, out);
        }
        self.workspace_job(WorkspaceJob::Owned, out);
        self.workspaces.start = Start::Resolving {
            folders,
            owned: None,
        };
    }

    /// Each agent in a folder just resolved joins its workspace, running if
    /// its process still is.
    fn folder_resolved(&mut self, folder: String, resolved: Resolved, out: &mut Vec<Output>) {
        let Start::Resolving { folders, .. } = &mut self.workspaces.start else {
            return;
        };
        let Some(agents) = folders.remove(&folder) else {
            return;
        };
        for agent in agents {
            // Deleted while its folder was resolved.
            if !matches!(self.store.agent(&self.own_key(agent)), Ok(Some(_))) {
                continue;
            }
            let running = self.families.processes().any(|known| known == agent)
                && !self.families.ended(agent);
            self.feed(
                Core::Added {
                    member: member(agent),
                    workspace: resolved.workspace.clone(),
                    made_here: resolved.made_here,
                    running,
                },
                None,
                out,
            );
            self.sync_entry(agent, out);
        }
        self.start_done(out);
    }

    /// Once every folder is resolved and the owned workspaces are listed,
    /// those without a member are removed, and the launches that waited
    /// are asked for.
    fn start_done(&mut self, out: &mut Vec<Output>) {
        let Start::Resolving {
            folders,
            owned: Some(owned),
        } = &self.workspaces.start
        else {
            return;
        };
        if !folders.is_empty() {
            return;
        }
        let owned = owned.clone();
        self.workspaces.start = Start::Done;
        self.feed(Core::Swept { owned }, None, out);
        for agent in std::mem::take(&mut self.workspaces.deferred) {
            if self.workspaces.core.workspace_of(member(agent)).is_none() {
                // Not resolved at the start, so it launches as without the
                // experiment.
                self.workspace_launched(agent, out);
                continue;
            }
            self.feed(
                Core::Launching {
                    member: member(agent),
                },
                None,
                out,
            );
        }
    }

    // --- what the driver reports ---------------------------------------------

    /// A workspace job finished. Returns whether it was one.
    pub(crate) fn workspace_job_done(
        &mut self,
        job: JobId,
        result: &JobResult,
        out: &mut Vec<Output>,
    ) -> bool {
        let JobResult::Workspace(done) = result else {
            return false;
        };
        if matches!(done, WorkspaceDone::Planned(_)) {
            // The registry's lane waits on a plan.
            return false;
        }
        if !self.workspaces.jobs.remove(&job) {
            return true;
        }
        match done.clone() {
            WorkspaceDone::Planned(_) => {}
            WorkspaceDone::Resolved { folder, resolved } => {
                self.folder_resolved(folder, resolved, out)
            }
            WorkspaceDone::Owned(owned) => {
                if let Start::Resolving { owned: listed, .. } = &mut self.workspaces.start {
                    *listed = Some(owned);
                }
                self.start_done(out);
            }
            WorkspaceDone::Made { workspace, result } => {
                self.feed(Core::Made { workspace, result }, None, out);
            }
            WorkspaceDone::Claimed { workspace, result } => {
                self.feed(Core::Claimed { workspace, result }, None, out);
            }
            WorkspaceDone::Started { workspace, result } => {
                if let Err(problem) = &result {
                    tracing::warn!(id = %workspace.id, %problem, "a workspace's start hook failed");
                }
                self.feed(Core::Started { workspace, result }, None, out);
            }
            WorkspaceDone::Stopped { workspace, result } => {
                if let Err(problem) = &result {
                    tracing::warn!(id = %workspace.id, %problem, "a workspace's stop hook failed");
                }
                self.feed(Core::Stopped { workspace, result }, None, out);
            }
            WorkspaceDone::Deleted { workspace, removal } => {
                self.feed(Core::Deleted { workspace, removal }, None, out);
            }
        }
        true
    }
}

/// Why a create in a new workspace failed, as its caller reads it.
pub(crate) fn could_not_plan(refusal: PlanRefusal) -> Refusal {
    match refusal {
        PlanRefusal::Refused(error) => Refusal::Worktree(error),
        PlanRefusal::Taken(why) => Refusal::Worktree(wire::Error {
            code: wire::ErrorCode::AlreadyExists as i32,
            message: format!("starting in a new workspace: {why}"),
            details: Vec::new(),
        }),
    }
}

#[cfg(test)]
mod tests;
