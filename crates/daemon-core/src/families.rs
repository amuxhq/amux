//! The registry and the family rule.
//!
//! This host's agents are created, resumed, stopped, killed, deleted and
//! renamed here, one operation at a time per agent, and each of them
//! follows what its parent's row says: a parent that exited stops its live
//! children here, and one gone for good deletes them. The processes
//! themselves are the driver's: it spawns what [`Output::SpawnAgent`]
//! names, dials what [`Output::DialAgent`] names, writes what
//! [`Output::WriteControl`] carries, and reports each connection's Hello,
//! each Nudge and each process's end as inputs. What the start's sweep
//! finds is reported the same way.
//!
//! An operation runs to its end whether or not its caller still waits: a
//! delete whose client went away still deletes, and a child following its
//! parent has no caller at all.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use prost::Message as _;
use serde::{Deserialize, Serialize};
use store::{AgentKey, AgentRow, Delivery, Marker, Notification, StoreError};
use wire::{
    Agent, AgentRemoved, AgentSpec, CtlFrame, ExitCause, Lifecycle, ctl_frame, inventory_event,
};

use crate::ids::{AgentId, CallId, JobId, TimerId};
use crate::io::{Job, Output, ParentSays, ShellWork};
use crate::payload::{CallError, JobResult, Response, Status};
use crate::serve::{inventory, to_wire};
use crate::{Daemon, codec};

/// How long an agent past its stop deadline has to act on a kill asked of
/// it over its control socket before its process is killed from outside.
pub const KILL_ASK_PATIENCE_MS: u64 = 2_000;
/// How long a stop waits after killing the process group before it gives up
/// on seeing the process end.
pub const KILL_PATIENCE_MS: u64 = 5_000;
/// Under the suspension experiment, how long an agent whose process ended
/// without its journal saying why waits before its exit is recorded. At a
/// shutdown the system signals agents and the daemon in no promised order,
/// and an exit recorded and published in between would have paired hosts
/// stop the agent's children; a daemon about to be signalled records
/// nothing within this.
pub const CAUSELESS_GRACE_MS: u64 = 3_000;
/// What an agent resumed after the machine restarted under its turn is
/// told first.
pub const CONTINUE_AFTER_RESTART: &str =
    "The machine restarted while you were working; carry on where you left off.";

/// What the registry remembers: the processes this run knows of, the
/// operation under way on each agent, and the children waiting to follow
/// their parents until the start's sweep is done.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FamilyState {
    /// This host's agents with a process this run started, adopted or
    /// waits on, by id.
    processes: BTreeMap<AgentId, Process>,
    /// Each agent's operation under way and those queued behind it.
    lanes: BTreeMap<AgentId, Lane>,
    /// Whether the start's sweep has reported every agent it found. Until
    /// it has, a child does not follow its parent: a stop then would find
    /// no process to end.
    swept: bool,
    /// Children waiting for the sweep to follow their parents.
    held: Vec<(AgentId, ParentSays)>,
    /// The daemon has been asked to stop, under the suspension experiment:
    /// it records no more exits.
    #[serde(default)]
    stopping: bool,
}

impl FamilyState {
    /// The jobs the operations under way wait on.
    pub(crate) fn jobs(&self, out: &mut Vec<JobId>) {
        out.extend(
            self.lanes
                .values()
                .filter_map(|lane| lane.current.as_ref()?.doing.job()),
        );
    }

    /// The registry with the jobs it waits on under canonical ids.
    pub(crate) fn canonical(&self, ids: &crate::fingerprint::Ids) -> FamilyState {
        let mut families = self.clone();
        for lane in families.lanes.values_mut() {
            if let Some(operation) = lane.current.as_mut() {
                match &mut operation.doing {
                    Doing::Worktree { job, .. }
                    | Doing::Planning { job, .. }
                    | Doing::AwaitUnlock { job, .. }
                    | Doing::AwaitSpec { job, .. }
                    | Doing::Removing { job }
                    | Doing::Ending {
                        phase: Phase::Killing { job: Some(job) },
                        ..
                    } => *job = ids.jobs.get(*job),
                    Doing::Starting
                    | Doing::Making { .. }
                    | Doing::AwaitLaunch { .. }
                    | Doing::AwaitExit { .. }
                    | Doing::Ending { .. }
                    | Doing::StoppingSubagent { .. } => {}
                }
            }
        }
        families
    }

    /// The agents with a process this run knows of.
    pub fn processes(&self) -> impl Iterator<Item = AgentId> + '_ {
        self.processes.keys().copied()
    }

    /// Whether the start's sweep has reported every agent it found.
    pub(crate) fn swept(&self) -> bool {
        self.swept
    }

    /// Whether the agent's process is known to be gone.
    pub(crate) fn ended(&self, agent: AgentId) -> bool {
        self.processes
            .get(&agent)
            .is_some_and(|process| process.ended.is_some())
    }

    /// Whether the agent's process is still to start in a workspace being
    /// made or claimed, or has started and not yet said Hello: a message
    /// for it waits in its lane until it takes input.
    pub(crate) fn launching(&self, agent: AgentId) -> bool {
        self.lanes
            .get(&agent)
            .and_then(|lane| lane.current.as_ref())
            .is_some_and(|operation| {
                matches!(
                    operation.doing,
                    Doing::Making { .. } | Doing::AwaitLaunch { .. } | Doing::Starting
                )
            })
    }

    /// Whether the agent's process has a connection that said Hello.
    pub(crate) fn connected(&self, agent: AgentId) -> bool {
        self.processes
            .get(&agent)
            .is_some_and(|process| process.hello && process.ended.is_none())
    }

    /// The agents whose process has a connection that said Hello, in id
    /// order.
    pub(crate) fn connected_agents(&self) -> Vec<AgentId> {
        self.processes
            .keys()
            .copied()
            .filter(|agent| self.connected(*agent))
            .collect()
    }
}

/// One agent's process, as the registry knows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Process {
    /// Its current connection said Hello.
    hello: bool,
    /// The stop the daemon asked for: a kill names the exit's cause, since
    /// a killed process writes nothing more.
    stopping: Option<Stopping>,
    /// It refused an input as exiting: it is on its way out.
    leaving: bool,
    /// Its live direct children with their incarnations, as last told.
    told: Vec<(Vec<u8>, u32)>,
    /// It is gone, and how the driver knows; its exit is recorded once its
    /// journal has been read to the end.
    ended: Option<Ended>,
    /// It ended without its journal saying why, under the suspension
    /// experiment: false while the grace runs, true once it is up.
    #[serde(default)]
    grace: Option<bool>,
}

/// What a gone process leaves its row as.
enum Gone {
    /// Live, with no process: it suspended, or the machine restarted
    /// under it while it was idle.
    Suspended,
    /// Suspended, then resumed at once: the machine restarted under its
    /// turn.
    Continued,
    /// Exited as stopped: the person closed it and it was on its way out
    /// when its process went.
    Stopped,
    /// Exited, with the daemon's own cause should its journal give none,
    /// unless it already says exited when `if_live`.
    Exited {
        fallback: wire::IncarnationEnd,
        if_live: bool,
    },
}

/// How the daemon is ending an agent's process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Stopping {
    /// The turn cancelled the provider's own way, then the agent exits.
    Stop,
    /// The process group ended at once; nothing more is written.
    Kill,
}

/// How the driver knows an agent's process is gone.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Ended {
    /// The process this run watched ended: its lock is free.
    Exited,
    /// The process could not be started, in the driver's words.
    CouldNotStart(String),
    /// The start's sweep found its directory unlocked: it ended while no
    /// daemon was there to see.
    Away,
    /// The start's sweep found no directory for it.
    NoDirectory,
}

/// A create, with what the profile's shell read of the world for it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Create {
    pub agent: AgentId,
    /// The working directory, a directory on this host; with
    /// `new_workspace`, the folder of the repository the workspace is made
    /// from.
    pub cwd: String,
    /// The name asked for, checked to be one an agent may have.
    pub given: Option<String>,
    /// What the agent's name must not be: the repository's branches. Read
    /// for an unnamed agent, and with `new_workspace` for a named one too.
    pub branches: BTreeSet<String>,
    /// Start in a new workspace of the repository at `cwd`, named by the
    /// agent's name: a worktree, or with the workspaces experiment a wt tree
    /// where wt is configured.
    pub new_workspace: bool,
    /// With the workspaces experiment and no new workspace, the workspace
    /// `cwd` is in, as the shell resolved it.
    #[serde(default)]
    pub workspace: Option<crate::workspaces::Resolved>,
    /// `cwd` as the file system resolves it, kept among the profile's
    /// recent directories once the agent is made; `None` when it does not
    /// resolve.
    pub recent: Option<String>,
    /// What the incarnation starts with but for what the registry decides:
    /// its name, working directory, creation time, number and base branch.
    /// Its parent, when it has one, is the new agent's.
    #[serde(with = "codec::message")]
    pub spec: AgentSpec,
    /// With a parent, the parent's incarnation making the create, as the
    /// parent's own host stated it: the first the child hears from, and
    /// what an exited copy of the parent's row is judged against. 0 with no
    /// parent.
    pub parent_incarnation: u32,
}

/// A resume, with what seeds its next incarnation's queue. Its spec is
/// built once the previous process has ended, so it carries the settings
/// that process's last snapshot recorded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resume {
    pub agent: AgentId,
    #[serde(with = "codec::optional")]
    pub bootstrap: Option<wire::Bootstrap>,
    /// A wake: it goes ahead only for an agent still suspended once its
    /// turn comes and any process it waited out is gone, else it is
    /// refused as [`Refusal::NotSuspended`]. A person's input to an agent
    /// with no process asks this, so an agent stopped in the meantime is
    /// not started again by something typed to it before the stop showed.
    #[serde(default)]
    pub if_suspended: bool,
}

/// Why the registry refused or could not finish an operation.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    /// No agent with that id.
    NotFound,
    /// An agent with that id exists.
    AlreadyExists,
    /// The agent is live; it is stopped before it is resumed.
    Live,
    /// A wake met an agent that is not suspended: exited, or started by
    /// something else first.
    NotSuspended,
    /// A wake met an agent the daemon is ending: a stop, close or delete
    /// under way or waiting its turn, or the daemon on its way out. It is
    /// answered at once rather than after the stop's deadline.
    Ending,
    /// Another agent on this host has the name.
    NameTaken(String),
    /// Only an agent with no parent is pinned.
    HasParent,
    /// The parent has ended and starts no more agents.
    ParentEnded(AgentId),
    StartTimeout,
    StopTimeout,
    /// The previous process still holds the agent's directory.
    StillLocked,
    /// The agent has no spec to resume from.
    NoSpec,
    /// The process could not be started, in the driver's words.
    CouldNotStart(String),
    /// The worktree could not be made, as the driver said it.
    Worktree(wire::Error),
    /// The agent's directory could not be removed, in the driver's words.
    Directory(String),
    /// The store failed, in its own words.
    Store(String),
    /// The agent is a provider subagent, which cannot do this, in words
    /// saying why: it has no process or directory of its own and goes with
    /// the agent that runs it.
    Subagent(String),
    /// The driver refused to build the next spec, in its own words to the
    /// caller: the agent's program is no longer configured or installed,
    /// or its conversation cannot be reopened.
    Spec(wire::Error),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Lane {
    current: Option<Operation>,
    queued: VecDeque<Queued>,
}

/// An operation waiting its turn on an agent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Queued {
    call: Caller,
    op: Op,
}

/// Who waits for an operation's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Caller {
    /// A client's call.
    Client(CallId),
    /// Nobody: a child following its parent.
    Family,
    /// Nobody any more: a create in a new workspace answered its caller
    /// once its row was written.
    Answered,
    /// The agent's message lane: a parent's message resumes it.
    Lane,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum Op {
    Create(Box<Create>),
    Resume(Box<Resume>),
    Stop,
    Kill,
    /// `discard`: the delete agreed to lose the uncommitted changes of the
    /// workspace it removes.
    Delete {
        discard: bool,
    },
    File(Filing),
}

/// How the person files an agent, apart from its lifecycle: the wire's
/// `Filing` without its unspecified value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Filing {
    Open,
    /// Kept at the top of the fleet; only an agent with no parent.
    Pinned,
    /// Done with: stopped when live, and its children closed with it.
    Closed,
}

impl Filing {
    /// The wire's filing, or nothing for one a client left unspecified.
    pub fn from_wire(filing: wire::Filing) -> Option<Self> {
        match filing {
            wire::Filing::Unspecified => None,
            wire::Filing::Open => Some(Self::Open),
            wire::Filing::Pinned => Some(Self::Pinned),
            wire::Filing::Closed => Some(Self::Closed),
        }
    }

    fn to_wire(self) -> wire::Filing {
        match self {
            Self::Open => wire::Filing::Open,
            Self::Pinned => wire::Filing::Pinned,
            Self::Closed => wire::Filing::Closed,
        }
    }
}

/// The operation under way on an agent, and whoever waits for its answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Operation {
    call: Caller,
    doing: Doing,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum Doing {
    /// A create waits for its worktree, with the name it chose.
    Worktree {
        job: JobId,
        create: Box<Create>,
        name: String,
    },
    /// A create in a new workspace waits for its plan, with the name it
    /// chose and those found taken before it.
    Planning {
        job: JobId,
        create: Box<Create>,
        name: String,
        taken: BTreeSet<String>,
    },
    /// A create's row is written and its caller answered; it waits for its
    /// workspace to be made and claimed before its process starts.
    Making {
        create: Box<Create>,
        name: String,
        planned: Box<crate::workspaces::Planned>,
    },
    /// A process waits for its workspace's claim before it starts.
    AwaitLaunch {
        #[serde(with = "codec::boxed_message")]
        spec: Box<AgentSpec>,
    },
    /// The process was started: waiting for its Hello or its end.
    Starting,
    /// A resume waits for the process on its way out to end.
    AwaitExit { resume: Box<Resume> },
    /// A resume waits for the directory's lock to be free.
    AwaitUnlock { job: JobId, resume: Box<Resume> },
    /// A resume waits for its next spec to be built.
    AwaitSpec { job: JobId, resume: Box<Resume> },
    /// A stop or a kill is ending the process.
    Ending { phase: Phase, then: Then },
    /// A delete removed the rows; the directory goes.
    Removing { job: JobId },
    /// A close asked a provider subagent's host to stop it, and waits for
    /// its end; the filing it had is restored should the stop deadline
    /// pass first.
    StoppingSubagent { filing: i32, filed_at_ms: i64 },
}

impl Doing {
    /// The job the operation waits on, if it waits on one.
    fn job(&self) -> Option<JobId> {
        match self {
            Doing::Worktree { job, .. }
            | Doing::Planning { job, .. }
            | Doing::AwaitUnlock { job, .. }
            | Doing::AwaitSpec { job, .. }
            | Doing::Removing { job }
            | Doing::Ending {
                phase: Phase::Killing { job: Some(job) },
                ..
            } => Some(*job),
            Doing::Starting
            | Doing::Making { .. }
            | Doing::AwaitLaunch { .. }
            | Doing::AwaitExit { .. }
            | Doing::Ending { .. }
            | Doing::StoppingSubagent { .. } => None,
        }
    }
}

/// How far ending a process has gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Phase {
    /// The stop was asked; the stop deadline is armed.
    Asked,
    /// The kill was asked of the agent; its patience is armed.
    KillAsked,
    /// The process group is being killed from outside, and then its end
    /// waited for.
    Killing { job: Option<JobId> },
}

/// What an ended process leads to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Then {
    /// The caller hears the agent's row.
    Answer,
    /// The agent's rows and directory go; `discard` as the delete said.
    Delete { discard: bool },
}

/// What an operation came to, for its caller.
pub(crate) type Outcome = Result<Option<Agent>, Refusal>;

/// How beginning an operation went: it waits on something, or it is
/// already over.
pub(crate) enum Began {
    Waiting(Doing),
    Finished(Box<Outcome>),
}

impl From<StoreError> for Refusal {
    fn from(error: StoreError) -> Self {
        Refusal::Store(error.to_string())
    }
}

impl Daemon {
    /// The registry, for reading.
    pub fn families(&self) -> &FamilyState {
        &self.families
    }

    pub(crate) fn own_key(&self, agent: AgentId) -> AgentKey {
        AgentKey::new(self.store.own_host(), agent.0)
    }

    /// The agent's directory, where the driver keeps its specs, sockets and
    /// journal.
    pub fn agent_dir(&self, agent: AgentId) -> PathBuf {
        crate::ingest::agent_dir(&self.options.agents, agent)
    }

    pub(crate) fn arm_after(&self, id: TimerId, after_ms: u64, out: &mut Vec<Output>) {
        out.push(Output::ArmTimer {
            id,
            at_ms: self.timers.now_ms.saturating_add(after_ms),
        });
    }

    // --- operations ------------------------------------------------------------

    /// A registry request: queued behind the agent's operation under way,
    /// or begun now.
    pub(crate) fn registry_call(
        &mut self,
        call: Caller,
        agent: AgentId,
        op: Op,
        out: &mut Vec<Output>,
    ) {
        let lane = self.families.lanes.entry(agent).or_default();
        lane.queued.push_back(Queued { call, op });
        if lane.current.is_none() {
            self.next_operation(agent, out);
        }
    }

    pub(crate) fn create(&mut self, call: CallId, create: Box<Create>, out: &mut Vec<Output>) {
        let agent = create.agent;
        self.registry_call(Caller::Client(call), agent, Op::Create(create), out);
    }

    pub(crate) fn resume(&mut self, call: CallId, resume: Box<Resume>, out: &mut Vec<Output>) {
        let agent = resume.agent;
        if resume.if_suspended && self.ending(agent) {
            self.reply(agent, Caller::Client(call), Err(Refusal::Ending), out);
            return;
        }
        self.registry_call(Caller::Client(call), agent, Op::Resume(resume), out);
    }

    /// Whether the daemon is ending the agent: a wake would only wait for
    /// the end and then find nothing to wake.
    fn ending(&self, agent: AgentId) -> bool {
        let ends = |op: &Op| {
            matches!(
                op,
                Op::Stop | Op::Kill | Op::Delete { .. } | Op::File(Filing::Closed)
            )
        };
        self.families.stopping
            || self
                .families
                .processes
                .get(&agent)
                .is_some_and(|process| process.stopping.is_some())
            || self.families.lanes.get(&agent).is_some_and(|lane| {
                lane.current.as_ref().is_some_and(|operation| {
                    matches!(
                        operation.doing,
                        Doing::Ending { .. } | Doing::Removing { .. }
                    )
                }) || lane.queued.iter().any(|queued| ends(&queued.op))
            })
    }

    pub(crate) fn file(
        &mut self,
        call: CallId,
        agent: AgentId,
        filing: Filing,
        out: &mut Vec<Output>,
    ) {
        self.registry_call(Caller::Client(call), agent, Op::File(filing), out);
    }

    pub(crate) fn kill(&mut self, call: CallId, agent: AgentId, out: &mut Vec<Output>) {
        self.registry_call(Caller::Client(call), agent, Op::Kill, out);
    }

    pub(crate) fn delete(
        &mut self,
        call: CallId,
        agent: AgentId,
        discard: bool,
        out: &mut Vec<Output>,
    ) {
        self.registry_call(Caller::Client(call), agent, Op::Delete { discard }, out);
    }

    /// Begins the agent's next queued operation, and the one after it while
    /// each finishes at once.
    fn next_operation(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        loop {
            let Some(lane) = self.families.lanes.get_mut(&agent) else {
                return;
            };
            if lane.current.is_some() {
                return;
            }
            let Some(Queued { call, op }) = lane.queued.pop_front() else {
                self.families.lanes.remove(&agent);
                return;
            };
            let began = match self.subagent_operation(agent, call, &op, out) {
                Some(began) => began,
                None => match op {
                    Op::Create(create) => self.begin_create(create, out),
                    Op::Resume(resume) => self.begin_resume(resume, out),
                    Op::Stop => self.begin_ending(agent, false, Then::Answer, out),
                    Op::Kill => self.begin_ending(agent, true, Then::Answer, out),
                    Op::Delete { discard } => self.begin_delete(agent, discard, out),
                    Op::File(filing) => self.begin_file(agent, filing, out),
                },
            };
            match began {
                Ok(Began::Waiting(doing)) => {
                    if let Some(lane) = self.families.lanes.get_mut(&agent) {
                        lane.current = Some(Operation { call, doing });
                    }
                    return;
                }
                Ok(Began::Finished(outcome)) => self.reply(agent, call, *outcome, out),
                Err(refusal) => self.reply(agent, call, Err(refusal), out),
            }
        }
    }

    /// Ends the agent's operation under way with `outcome`, and begins the
    /// next.
    pub(crate) fn finish(&mut self, agent: AgentId, outcome: Outcome, out: &mut Vec<Output>) {
        let call = self
            .families
            .lanes
            .get_mut(&agent)
            .and_then(|lane| lane.current.take())
            .map_or(Caller::Family, |operation| operation.call);
        self.reply(agent, call, outcome, out);
        self.next_operation(agent, out);
    }

    /// Answers whoever waits, or says why an operation nobody waits for
    /// did not finish.
    fn reply(&mut self, agent: AgentId, call: Caller, outcome: Outcome, out: &mut Vec<Output>) {
        let call = match call {
            Caller::Client(call) => call,
            Caller::Family => {
                if let Err(refusal) = outcome
                    && refusal != Refusal::NotFound
                    && refusal != Refusal::NotSuspended
                {
                    tracing::warn!(agent = %crate::hex(&agent.0), ?refusal, "following its parent failed");
                }
                return;
            }
            Caller::Lane => return self.lane_resumed(agent, outcome.map(|_| ()), out),
            Caller::Answered => {
                if let Err(refusal) = outcome {
                    tracing::debug!(agent = %crate::hex(&agent.0), ?refusal, "a create in a new workspace did not start");
                }
                return;
            }
        };
        match outcome {
            Ok(Some(agent)) => crate::serve::answered(call, Response::Agent(Box::new(agent)), out),
            Ok(None) => out.push(Output::EndCall {
                call,
                status: Status::Ok,
            }),
            Err(refusal) => out.push(Output::EndCall {
                call,
                status: Status::Failed(CallError::Registry(refusal)),
            }),
        }
    }

    /// The agent's row as the inventory carries it, or that it has none.
    pub(crate) fn row_outcome(&self, agent: AgentId) -> Outcome {
        match self.store.agent(&self.own_key(agent))? {
            Some(row) => Ok(Some(to_wire(&row))),
            None => Err(Refusal::NotFound),
        }
    }

    // --- create and resume -----------------------------------------------------

    /// Writes the new agent's row and starts its process, or asks for its
    /// worktree first. Its parent is checked and its name chosen in the
    /// same step as the row is written, so a parent that ends after this
    /// sees the child and stops it, and two creates never take one name.
    fn begin_create(
        &mut self,
        create: Box<Create>,
        out: &mut Vec<Output>,
    ) -> Result<Began, Refusal> {
        let key = self.own_key(create.agent);
        if self.store.agent(&key)?.is_some() {
            return Err(Refusal::AlreadyExists);
        }
        self.parent_gate(&create)?;
        let name = self.choose_name(&create, &BTreeSet::new())?;
        let mut create = create;
        create.spec.created_at_ms = self.timers.now_ms as i64;
        if create.new_workspace && self.workspaces_on() {
            // The workspace is planned before the row, without touching the
            // network: a folder it cannot be made in fails the create with
            // no agent made, and a name whose folder or branch is taken
            // gets the next free suffix.
            let job = self.plan_workspace(&create.cwd, &name, out);
            return Ok(Began::Waiting(Doing::Planning {
                job,
                create,
                name,
                taken: BTreeSet::new(),
            }));
        }
        if create.new_workspace {
            // A worktree is made before the row: a folder it cannot be made
            // in fails the create with no agent made. Two unnamed creates
            // at once in one repository could pick the same pair; the
            // second then finds the branch taken and fails.
            let job = self.pending.mint();
            out.push(Output::Job {
                id: job,
                op: Job::MakeWorktree {
                    repo: PathBuf::from(&create.cwd),
                    name: name.clone(),
                },
            });
            return Ok(Began::Waiting(Doing::Worktree { job, create, name }));
        }
        // The row comes first: a crash after it leaves a row the sweep
        // marks exited, never a directory nothing lists.
        let cwd = create.cwd.clone();
        let row = self.new_row(&create, &name, &cwd);
        self.put_own_row(&row, out)?;
        self.remember_directory(&create, out);
        if self.workspaces_on()
            && let Some(resolved) = &create.workspace
        {
            self.member_added(create.agent, resolved, out);
        }
        Ok(Began::Waiting(
            self.start_incarnation(create, name, cwd, None, out)?,
        ))
    }

    /// The new agent's name. Names are unique on this host, counting exited
    /// agents, which stay addressable by name: a given name already taken
    /// gets the next free suffix, and the caller learns the name from the
    /// agent it gets back. An unnamed agent's, and with the workspaces
    /// experiment a new workspace's, is no branch of the repository either,
    /// nor any name `taken` holds: one whose workspace's folder or branch
    /// turned out to be taken.
    fn choose_name(&self, create: &Create, taken: &BTreeSet<String>) -> Result<String, StoreError> {
        let names = self.own_names()?;
        let branches = create.given.is_none() || (create.new_workspace && self.workspaces_on());
        let taken = |candidate: &str| {
            names.contains(candidate)
                || taken.contains(candidate)
                || (branches && create.branches.contains(candidate))
        };
        Ok(match &create.given {
            Some(name) => crate::names::free_suffix(name, &taken),
            None => crate::names::assign_name(u128::from_be_bytes(create.agent.0), &taken),
        })
    }

    /// The new workspace a create waited for is planned, or could not be:
    /// the row is written with the planned folder as its cwd, the caller is
    /// answered with it, and the workspace is made, after which the process
    /// starts. A name found taken is planned again with the next.
    fn workspace_planned(
        &mut self,
        agent: AgentId,
        create: Box<Create>,
        name: String,
        mut taken: BTreeSet<String>,
        result: JobResult,
        out: &mut Vec<Output>,
    ) {
        use crate::workspaces::{PlanRefusal, WorkspaceDone};
        let planned = match result {
            JobResult::Workspace(WorkspaceDone::Planned(Ok(planned))) => planned,
            JobResult::Workspace(WorkspaceDone::Planned(Err(PlanRefusal::Taken(why)))) => {
                return self.plan_again(agent, create, name, taken, why, out);
            }
            JobResult::Workspace(WorkspaceDone::Planned(Err(refusal))) => {
                let refusal = crate::workspaces::could_not_plan(refusal);
                return self.finish(agent, Err(refusal), out);
            }
            other => {
                let error = wire::Error {
                    code: wire::ErrorCode::Internal as i32,
                    message: format!("starting in a new workspace: {other:?}"),
                    details: Vec::new(),
                };
                return self.finish(agent, Err(Refusal::Worktree(error)), out);
            }
        };
        // Judged again as the row is written, since the parent may have
        // ended, and another create taken the name, while this one planned.
        if let Err(refusal) = self.parent_gate(&create) {
            return self.finish(agent, Err(refusal), out);
        }
        match self.own_names() {
            Ok(names) if names.contains(&name) => {
                taken.insert(name.clone());
                let why = format!("another agent took the name {name}");
                return self.plan_again(agent, create, name, taken, why, out);
            }
            Ok(_) => {}
            Err(error) => return self.finish(agent, Err(error.into()), out),
        }
        let cwd = planned.workspace.id.clone();
        let row = self.new_row(&create, &name, &cwd);
        if let Err(error) = self.put_own_row(&row, out) {
            return self.finish(agent, Err(error.into()), out);
        }
        self.remember_directory(&create, out);
        // The caller hears of the agent now, its workspace still being made;
        // whatever comes of the making is on the row.
        let caller = self
            .families
            .lanes
            .get_mut(&agent)
            .and_then(|lane| lane.current.as_mut())
            .map(|operation| std::mem::replace(&mut operation.call, Caller::Answered));
        self.set_doing(
            agent,
            Doing::Making {
                create,
                name,
                planned: Box::new(planned.clone()),
            },
        );
        self.member_created(agent, &planned, out);
        self.current_before_launch(agent, out);
        if let Some(caller) = caller {
            let outcome = self.row_outcome(agent);
            self.reply(agent, caller, outcome, out);
        }
    }

    /// Plans the create's workspace again under the next free name, after
    /// `name`'s folder or branch was found taken; gives up after a few.
    fn plan_again(
        &mut self,
        agent: AgentId,
        create: Box<Create>,
        name: String,
        mut taken: BTreeSet<String>,
        why: String,
        out: &mut Vec<Output>,
    ) {
        /// How many taken names a create walks past before it gives up: a
        /// branch template that renders every name to one branch would
        /// otherwise plan forever.
        const TRIES: usize = 16;
        taken.insert(name);
        if taken.len() >= TRIES {
            let refusal =
                crate::workspaces::could_not_plan(crate::workspaces::PlanRefusal::Taken(why));
            return self.finish(agent, Err(refusal), out);
        }
        let name = match self.choose_name(&create, &taken) {
            Ok(name) => name,
            Err(error) => return self.finish(agent, Err(error.into()), out),
        };
        let job = self.plan_workspace(&create.cwd, &name, out);
        self.set_doing(
            agent,
            Doing::Planning {
                job,
                create,
                name,
                taken,
            },
        );
    }

    /// The workspace core lets the agent's process start: its workspace is
    /// made, and claimed.
    pub(crate) fn workspace_launched(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        let doing = match self.doing(agent).cloned() {
            Some(Doing::Making {
                create,
                name,
                planned,
            }) => {
                let base_branch = planned.base_branch.clone();
                self.spawn_incarnation(create, name, planned.workspace.id, base_branch, out)
            }
            Some(Doing::AwaitLaunch { spec }) => self.spawn_process(agent, *spec, out),
            // Nothing waits to launch it any more: it is not awake.
            _ => return self.member_slept(agent, out),
        };
        self.set_doing(agent, doing);
    }

    /// The workspace core says the agent's process cannot start: its
    /// workspace could not be made, or is gone. Its row exits as could not
    /// start, saying why.
    pub(crate) fn workspace_not_made(
        &mut self,
        agent: AgentId,
        detail: String,
        out: &mut Vec<Output>,
    ) {
        let doing = self.doing(agent).cloned();
        if !matches!(
            doing,
            Some(Doing::Making { .. } | Doing::AwaitLaunch { .. })
        ) {
            return;
        }
        if let Err(error) = self.could_not_start(agent, &detail, out) {
            tracing::warn!(agent = %crate::hex(&agent.0), %error, "marking an agent exited failed");
        }
        let outcome = match doing {
            Some(Doing::Making { .. }) => self.row_outcome(agent),
            _ => Err(Refusal::CouldNotStart(detail)),
        };
        self.finish(agent, outcome, out);
    }

    /// Marks an agent whose process never started exited as could not
    /// start, with `detail`; a message waiting for it is judged again
    /// against the exited row.
    fn could_not_start(
        &mut self,
        agent: AgentId,
        detail: &str,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        let fallback = wire::IncarnationEnd {
            cause: ExitCause::CouldNotStart as i32,
            detail: Some(detail.to_owned()),
        };
        self.mark_exited(agent, fallback, false, out)?;
        self.lane_exited(agent, out);
        Ok(())
    }

    /// Whether a child may be written under the create's parent now: run
    /// as the row is written, so the parent's host and its row are judged
    /// in the step that makes the child.
    fn parent_gate(&self, create: &Create) -> Result<(), Refusal> {
        let Some(parent) = create.spec.parent.as_ref() else {
            return Ok(());
        };
        let parent = AgentKey::new(parent.host_id.clone(), parent.agent_id.clone());
        // A parent on another host is judged only from a caught-up row. A
        // child made before this host caught up with the parent's host, or
        // before the parent's row arrived, is made: an exited parent's
        // listing stops it, and one that outlives a deleted parent finishes
        // its work and exits, left for the clean-up of leftover agents. A
        // row older than the incarnation the parent's host says is calling
        // is a copy from before a resume, and says nothing of the caller.
        if self
            .parent_ended(&parent, create.parent_incarnation)?
            .is_some()
        {
            let id = AgentId::from_slice(&parent.agent).unwrap_or(AgentId([0; 16]));
            return Err(Refusal::ParentEnded(id));
        }
        Ok(())
    }

    /// Keeps the directory an agent was made in among the recent ones. A
    /// failed write is logged: an agent that was made is not a failed
    /// create for want of history.
    fn remember_directory(&mut self, create: &Create, out: &mut Vec<Output>) {
        let Some(path) = &create.recent else {
            return;
        };
        let at_ms = self.timers.now_ms as i64;
        match self.store.record_recent_directory(path, at_ms) {
            Ok(()) => out.push(Output::Shell(ShellWork::RecentDirectory)),
            Err(error) => tracing::warn!(%error, "cannot keep the recent directory"),
        }
    }

    fn new_row(&self, create: &Create, name: &str, cwd: &str) -> AgentRow {
        let mut row = AgentRow::new(self.own_key(create.agent), create.spec.kind(), cwd);
        row.name = name.to_owned();
        row.name_given = create.given.is_some();
        row.provider = create.spec.provider.clone();
        row.parent = create
            .spec
            .parent
            .as_ref()
            .map(|parent| AgentKey::new(parent.host_id.clone(), parent.agent_id.clone()));
        row.created_at_ms = create.spec.created_at_ms;
        row.incarnation = 1;
        row.parent_incarnation = create.parent_incarnation;
        row
    }

    /// The worktree a create waited for is made, or could not be.
    fn worktree_made(
        &mut self,
        agent: AgentId,
        create: Box<Create>,
        name: String,
        result: JobResult,
        out: &mut Vec<Output>,
    ) {
        let (path, base_branch) = match result {
            JobResult::Worktree { path, base_branch } => (path, base_branch),
            JobResult::Refused(error) => {
                return self.finish(agent, Err(Refusal::Worktree(error)), out);
            }
            _ => {
                let error = wire::Error {
                    code: wire::ErrorCode::Internal as i32,
                    message: format!("starting in a new worktree: {result:?}"),
                    details: Vec::new(),
                };
                return self.finish(agent, Err(Refusal::Worktree(error)), out);
            }
        };
        // The worktree took a while: the parent is judged again as the row
        // is written, since its host may have fallen behind or the parent
        // ended meanwhile. A refusal leaves the worktree for the caller.
        if let Err(refusal) = self.parent_gate(&create) {
            return self.finish(agent, Err(refusal), out);
        }
        let row = self.new_row(&create, &name, &path);
        if let Err(error) = self.put_own_row(&row, out) {
            return self.finish(agent, Err(error.into()), out);
        }
        self.remember_directory(&create, out);
        let doing = self.spawn_incarnation(create, name, path, Some(base_branch), out);
        if let Some(lane) = self.families.lanes.get_mut(&agent)
            && let Some(operation) = lane.current.as_mut()
        {
            operation.doing = doing;
        }
    }

    /// Asks the driver for the agent's first process, once its workspace
    /// lets it launch.
    fn start_incarnation(
        &mut self,
        create: Box<Create>,
        name: String,
        cwd: String,
        base_branch: Option<String>,
        out: &mut Vec<Output>,
    ) -> Result<Doing, Refusal> {
        let agent = create.agent;
        let spec = first_spec(create, name, cwd, base_branch);
        let doing = self.launch(agent, spec, out)?;
        if matches!(doing, Doing::AwaitLaunch { .. }) {
            self.current_before_launch(agent, out);
        }
        Ok(doing)
    }

    /// A new agent whose first process waits for its workspace has written
    /// nothing yet, so its chat is current as it stands: it is caught up at
    /// once, its composer sends, and what is sent waits in the lane until
    /// the process takes input. Making a workspace can take seconds (a
    /// fetch, a checkout, a copy of build output), and a chat that waited
    /// for the process's Hello said it was catching up all that while. The
    /// Hello announces it again once the journal is read.
    fn current_before_launch(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        let key = self.own_key(agent);
        if let Err(error) = self.announce_caught_up(&key, out) {
            tracing::warn!(agent = %crate::hex(&agent.0), %error, "announcing a new agent caught up failed");
        }
    }

    /// Asks the driver for the agent's first process now: its workspace
    /// already let it launch.
    fn spawn_incarnation(
        &mut self,
        create: Box<Create>,
        name: String,
        cwd: String,
        base_branch: Option<String>,
        out: &mut Vec<Output>,
    ) -> Doing {
        let agent = create.agent;
        let spec = first_spec(create, name, cwd, base_branch);
        self.spawn_process(agent, spec, out)
    }

    /// Starts the agent's process once its workspace lets it: at once, or
    /// once the workspace is claimed. One whose workspace is gone exits as
    /// could not start.
    fn launch(
        &mut self,
        agent: AgentId,
        spec: AgentSpec,
        out: &mut Vec<Output>,
    ) -> Result<Doing, Refusal> {
        match self.may_launch(agent, out) {
            crate::workspaces::Verdict::Now => Ok(self.spawn_process(agent, spec, out)),
            crate::workspaces::Verdict::Later => Ok(Doing::AwaitLaunch {
                spec: Box::new(spec),
            }),
            crate::workspaces::Verdict::Never(detail) => {
                self.could_not_start(agent, &detail, out)?;
                Err(Refusal::CouldNotStart(detail))
            }
        }
    }

    /// Starts the agent's next incarnation: waits for a process on its way
    /// out to end and release the directory, then writes the row and starts
    /// the process.
    fn begin_resume(
        &mut self,
        resume: Box<Resume>,
        out: &mut Vec<Output>,
    ) -> Result<Began, Refusal> {
        let agent = resume.agent;
        let Some(row) = self.store.agent(&self.own_key(agent))? else {
            return Err(Refusal::NotFound);
        };
        if let Some(process) = self.families.processes.get(&agent) {
            // A cause on a live row with a process is its journal's last
            // step, written as the agent begins to exit: it is on its way
            // out though it may have refused nothing yet, and once it
            // finishes it no longer reads input. The cause is committed
            // with the step that makes the row read as suspended, so a
            // wake for a row a client saw suspended always finds it here;
            // read from the row rather than kept beside the process, it
            // holds for a process adopted by a restarted daemon too.
            let journal_ended = row.exit_cause != ExitCause::Unspecified as i32;
            let leaving = process.leaving
                || process.stopping.is_some()
                || process.ended.is_some()
                || journal_ended;
            if !leaving {
                return Err(Refusal::Live);
            }
            self.arm_after(TimerId::Exit(agent), self.settings.stop_deadline_ms, out);
            return Ok(Began::Waiting(Doing::AwaitExit { resume }));
        }
        // Answered now rather than after the unlock and the spec; the
        // relaunch checks again.
        if resume.if_suspended && !self.suspended(agent, &row) {
            return Err(Refusal::NotSuspended);
        }
        Ok(Began::Waiting(self.await_unlock(resume, out)))
    }

    /// The process may be gone from the registry and still dying: the new
    /// one cannot take a lock the old one holds.
    fn await_unlock(&mut self, resume: Box<Resume>, out: &mut Vec<Output>) -> Doing {
        let job = self.pending.mint();
        out.push(Output::Job {
            id: job,
            op: Job::AwaitUnlocked {
                agent: resume.agent,
                patience_ms: self.settings.stop_deadline_ms,
            },
        });
        Doing::AwaitUnlock { job, resume }
    }

    /// The directory is free, so the previous process has ended and its
    /// journal's tail is committed: the next spec is built from the
    /// snapshot as it stands now, carrying the settings that process last
    /// ran with.
    fn ask_spec(
        &mut self,
        resume: Box<Resume>,
        out: &mut Vec<Output>,
    ) -> Result<Doing, StoreError> {
        let snapshot = self
            .store
            .cut(&self.own_key(resume.agent), 0)?
            .snapshot
            .map(Box::new);
        let job = self.pending.mint();
        out.push(Output::Job {
            id: job,
            op: Job::NextSpec {
                agent: resume.agent,
                bootstrap: resume.bootstrap.clone(),
                snapshot,
            },
        });
        Ok(Doing::AwaitSpec { job, resume })
    }

    /// The next spec is built: the row says live under the next incarnation
    /// and its process starts.
    fn resume_unlocked(
        &mut self,
        resume: &Resume,
        spec: Option<AgentSpec>,
        out: &mut Vec<Output>,
    ) -> Result<Doing, Refusal> {
        let agent = resume.agent;
        let key = self.own_key(agent);
        // Looked up again: retention may have removed an exited agent while
        // the resume waited, and a stop or a process it waited out may have
        // left it exited.
        let mut row = self.store.agent(&key)?.ok_or(Refusal::NotFound)?;
        if resume.if_suspended && !self.suspended(agent, &row) {
            return Err(Refusal::NotSuspended);
        }
        let mut spec = spec.ok_or(Refusal::NoSpec)?;
        // A resume of a suspended agent continues its run; any other
        // starts a new one. A run kept since before the table held it
        // began no later than the incarnation it continues.
        let run = if self.suspended(agent, &row) {
            self.store.run_start(&key.agent)?.unwrap_or(row.incarnation)
        } else {
            spec.incarnation
        };
        self.store.put_run_start(&key.agent, Some(run))?;
        // The name it has now: a rename while it was not running reaches
        // the agent this way.
        spec.name = row.name.clone();
        spec.name_given = row.name_given;
        spec.title = row.title.clone();
        spec.created_at_ms = self.timers.now_ms as i64;
        row.lifecycle = Lifecycle::Live as i32;
        row.exit_cause = ExitCause::Unspecified as i32;
        row.exit_detail = None;
        row.incarnation = spec.incarnation;
        self.store.remove_notifications(&row.agent.agent)?;
        // Resumed by the person or its parent: no longer done with.
        if row.is_closed() {
            row.filing = wire::Filing::Open as i32;
            row.filed_at_ms = self.timers.now_ms as i64;
        }
        self.put_own_row(&row, out)?;
        // The next CaughtUp comes from this incarnation's journal.
        self.store.set_marker(&key, None);
        self.launch(agent, spec, out)
    }

    fn spawn_process(&mut self, agent: AgentId, spec: AgentSpec, out: &mut Vec<Output>) -> Doing {
        self.families.processes.insert(agent, Process::default());
        out.push(Output::SpawnAgent {
            dir: self.agent_dir(agent),
            spec: Box::new(spec),
            workspace: self.workspace_of(agent),
        });
        out.push(Output::DialAgent { agent });
        self.arm_after(TimerId::Start(agent), self.settings.start_deadline_ms, out);
        Doing::Starting
    }

    // --- stop, kill and delete -------------------------------------------------

    /// Stops the agent's process: the running turn is cancelled the
    /// provider's own way and the agent exits, resumable; a stop that
    /// overruns the stop deadline kills. A kill asks the agent to kill its
    /// provider's group and exit, and only if it does not is its process
    /// killed from outside. An agent with no process is left as it is,
    /// unless it is suspended: then it is marked exited as stopped.
    fn begin_ending(
        &mut self,
        agent: AgentId,
        kill: bool,
        then: Then,
        out: &mut Vec<Output>,
    ) -> Result<Began, Refusal> {
        if !self.families.processes.contains_key(&agent) {
            return match then {
                Then::Answer => {
                    self.stop_suspended(agent, out)?;
                    Ok(Began::Finished(Box::new(self.row_outcome(agent))))
                }
                Then::Delete { discard } => {
                    self.remove_agent(agent, discard, out)
                        .map(|doing| match doing {
                            Some(doing) => Began::Waiting(doing),
                            None => Began::Finished(Box::new(Ok(None))),
                        })
                }
            };
        }
        if kill {
            return Ok(Began::Waiting(self.ask_kill(agent, then, out)));
        }
        if let Some(process) = self.families.processes.get_mut(&agent) {
            process.stopping = Some(Stopping::Stop);
        }
        // A frame that cannot be written means the connection is already
        // going; the deadline still applies.
        out.push(Output::WriteControl {
            agent,
            frame: stop(false),
        });
        self.arm_after(TimerId::Exit(agent), self.settings.stop_deadline_ms, out);
        Ok(Began::Waiting(Doing::Ending {
            phase: Phase::Asked,
            then,
        }))
    }

    /// Files the agent as the person asked. Closing writes the filing
    /// first, so its children here and on every host that follows its row
    /// are closed with it, then stops it when it is live and answers once
    /// it has ended.
    fn begin_file(
        &mut self,
        agent: AgentId,
        filing: Filing,
        out: &mut Vec<Output>,
    ) -> Result<Began, Refusal> {
        let key = self.own_key(agent);
        let mut row = self.store.agent(&key)?.ok_or(Refusal::NotFound)?;
        if filing == Filing::Pinned && row.parent.is_some() {
            return Err(Refusal::HasParent);
        }
        let wire = filing.to_wire() as i32;
        if row.filing != wire {
            row.filing = wire;
            row.filed_at_ms = self.timers.now_ms as i64;
            self.put_own_row(&row, out)?;
        }
        if filing != Filing::Closed {
            return Ok(Began::Finished(Box::new(Ok(Some(to_wire(&row))))));
        }
        self.parent_changed(&key, ParentSays::Closed, out)?;
        self.begin_ending(agent, false, Then::Answer, out)
    }

    fn ask_kill(&mut self, agent: AgentId, then: Then, out: &mut Vec<Output>) -> Doing {
        if let Some(process) = self.families.processes.get_mut(&agent) {
            process.stopping = Some(Stopping::Kill);
        }
        out.push(Output::WriteControl {
            agent,
            frame: stop(true),
        });
        self.arm_after(TimerId::Exit(agent), KILL_ASK_PATIENCE_MS, out);
        Doing::Ending {
            phase: Phase::KillAsked,
            then,
        }
    }

    /// Deletes an agent: stops its process if live, then removes its rows
    /// and its directory. Nothing about it reaches a subscriber until its
    /// removal does, so none reads the stop as an exit it could resume
    /// from.
    fn begin_delete(
        &mut self,
        agent: AgentId,
        discard: bool,
        out: &mut Vec<Output>,
    ) -> Result<Began, Refusal> {
        let key = self.own_key(agent);
        if self.store.agent(&key)?.is_none() {
            return Err(Refusal::NotFound);
        }
        self.answers.mute(&key);
        self.begin_ending(agent, false, Then::Delete { discard }, out)
    }

    /// Removes a stopped agent's rows, then its directory. Its children, on
    /// this host or another, follow by the family rule once their host
    /// sees it gone.
    fn remove_agent(
        &mut self,
        agent: AgentId,
        discard: bool,
        out: &mut Vec<Output>,
    ) -> Result<Option<Doing>, Refusal> {
        let key = self.own_key(agent);
        let mut native = false;
        let removed = (|| {
            let row = self.store.agent(&key)?;
            native = row.as_ref().is_some_and(|row| row.native.is_some());
            let parent = row.and_then(|row| row.parent);
            // Rows first: a crash between the two leaves a directory with
            // no row, which the next sweep removes.
            self.store.delete_agent(&key)?;
            self.member_removed(agent, discard, out);
            let removed = AgentRemoved {
                host_id: key.host.clone(),
                agent_id: key.agent.clone(),
            };
            let removed = inventory(inventory_event::Of::AgentRemoved(removed));
            self.agent_removed(&key, Arc::new(removed), out);
            self.recount_from(parent, out)?;
            self.parent_changed(&key, ParentSays::Deleted, out)
        })();
        if let Err(error) = removed {
            self.unmute_published(&key, out);
            return Err(error.into());
        }
        self.journal_readers.remove(&agent);
        // A provider subagent has no directory: its files are its host's.
        if native {
            self.answers.unmute(&key);
            self.forget_draft(&agent.0);
            return Ok(None);
        }
        let job = self.pending.mint();
        out.push(Output::Job {
            id: job,
            op: Job::RemoveDir {
                path: self.agent_dir(agent),
            },
        });
        Ok(Some(Doing::Removing { job }))
    }

    /// Lets the agent's changes reach subscribers again, telling them the
    /// row as it now stands: what changed while it was muted reached none.
    fn unmute_published(&mut self, key: &AgentKey, out: &mut Vec<Output>) {
        self.answers.unmute(key);
        if let Err(error) = self.publish_row(key, out) {
            tracing::warn!(%error, "publishing an agent's row failed");
        }
    }

    // --- rename ----------------------------------------------------------------

    /// Renames an agent. The row takes the name now and the next
    /// incarnation's spec when it is resumed; a spec already written keeps
    /// the name it was spawned with. A live Codex agent is told, so its
    /// thread takes the name; a missed word is made good at the next
    /// resume.
    pub(crate) fn rename(
        &mut self,
        call: CallId,
        agent: AgentId,
        name: String,
        out: &mut Vec<Output>,
    ) {
        let outcome = (|| {
            let key = self.own_key(agent);
            let mut row = self.store.agent(&key)?.ok_or(Refusal::NotFound)?;
            if row.name != name && self.own_names()?.contains(&name) {
                return Err(Refusal::NameTaken(name));
            }
            row.name = name;
            row.name_given = true;
            row.title = None;
            self.put_own_row(&row, out)?;
            if self
                .families
                .processes
                .get(&agent)
                .is_some_and(|process| process.hello)
            {
                out.push(Output::WriteControl {
                    agent,
                    frame: CtlFrame {
                        of: Some(ctl_frame::Of::Rename(wire::Rename {
                            name: row.name.clone(),
                        })),
                    },
                });
            }
            Ok(Some(to_wire(&row)))
        })();
        self.reply(agent, Caller::Client(call), outcome, out);
    }

    // --- what the driver reports ----------------------------------------------

    /// A connection to the agent's process said Hello: the process is live
    /// and hears which of its children are, and a start waiting on it is
    /// answered. A process this daemon has just started has no children to
    /// hear of and is told nothing. One it found running is told even when
    /// it has none: it may hold a spawn its last daemon took and went away
    /// without answering, which only this daemon's word settles.
    pub(crate) fn process_hello(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        let children = self.children_of(agent).unwrap_or_default();
        let found_running = self.doing(agent) != Some(&Doing::Starting);
        let process = self.families.processes.entry(agent).or_default();
        process.hello = true;
        process.told = children;
        if found_running || !process.told.is_empty() {
            out.push(Output::WriteControl {
                agent,
                frame: children_frame(&process.told),
            });
        }
        if self.doing(agent) == Some(&Doing::Starting) {
            out.push(Output::CancelTimer {
                id: TimerId::Start(agent),
            });
            let outcome = self.row_outcome(agent);
            self.finish(agent, outcome, out);
        }
        self.lane_connected(agent, out);
    }

    /// The start's sweep found a process holding the agent's directory that
    /// has not answered yet: the driver keeps dialing it.
    pub(crate) fn adopted(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        if self.is_subagent(agent) {
            return;
        }
        self.families.processes.entry(agent).or_default();
        out.push(Output::DialAgent { agent });
    }

    /// The agent refused an input as exiting: a resume waits for it rather
    /// than refusing it as live.
    pub(crate) fn exiting(&mut self, agent: AgentId) {
        if let Some(process) = self.families.processes.get_mut(&agent) {
            process.leaving = true;
        }
    }

    /// The agent's process is gone: what its journal holds past the cursor
    /// is read, and then its exit recorded.
    pub(crate) fn control_closed(&mut self, agent: AgentId, ended: Ended, out: &mut Vec<Output>) {
        // A provider subagent has no process: one reported ended is a
        // driver's mistake, and its host's process is what ends it.
        if self.is_subagent(agent) {
            tracing::warn!(agent = %crate::hex(&agent.0), "a provider subagent was reported ended");
            return;
        }
        self.families.processes.entry(agent).or_default().ended = Some(ended);
        self.ingest(agent, out);
    }

    /// Ingest read the journal of a process that is gone to its end, or
    /// could not: its exit is recorded, the driver lets go of it, and the
    /// operation waiting on it goes on.
    pub(crate) fn journal_read(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        let Some(ended) = self
            .families
            .processes
            .get(&agent)
            .and_then(|process| process.ended.clone())
        else {
            return;
        };
        // On its way out the daemon records nothing: the next start's
        // sweep decides, as for an agent that ended while it was away.
        if self.families.stopping {
            return;
        }
        let row = self.store.agent(&self.own_key(agent)).ok().flatten();
        if self.awaits_grace(agent, &ended, row.as_ref(), out) {
            return;
        }
        let process = self.families.processes.remove(&agent).unwrap_or_default();
        let gone = self.gone(&ended, &process, row.as_ref());
        let continued = matches!(gone, Gone::Continued);
        let marked = match gone {
            Gone::Suspended | Gone::Continued => self.mark_suspended(agent, &ended, out),
            Gone::Stopped => self.mark_stopped(agent, out),
            Gone::Exited { fallback, if_live } => self.mark_exited(agent, fallback, if_live, out),
        };
        if let Err(error) = marked {
            tracing::warn!(agent = %crate::hex(&agent.0), %error, "marking an agent's end failed");
        }
        out.push(Output::ForgetAgent { agent });
        self.member_slept(agent, out);
        // The operation that waited on the process is done first: a stop
        // that met a suspension leaves the row exited before a message
        // waiting out the exit is judged against it again, so it resumes
        // nothing the person stopped.
        self.after_gone(agent, ended, out);
        self.lane_exited(agent, out);
        if continued && let Some(row) = &row {
            // A wake: a stop the person asked for while the journal was
            // still being read has left it exited, and it stays so.
            let resume = Resume {
                agent,
                bootstrap: Some(continue_after_restart(row)),
                if_suspended: true,
            };
            self.registry_call(Caller::Family, agent, Op::Resume(Box::new(resume)), out);
        }
    }

    /// Leaves the gone process's row live with no process: cause suspended,
    /// phase idle, since a provider's exit can leave it starting, and
    /// caught up, since nothing more comes until it is resumed. The
    /// provider subagents its process ran and its journal never ended (a
    /// reboot killed it first) end now, with the cause its row would have
    /// taken without the experiment: none runs without its host's process.
    fn mark_suspended(
        &mut self,
        agent: AgentId,
        ended: &Ended,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        let key = self.own_key(agent);
        let Some(mut row) = self.store.agent(&key)? else {
            return Ok(());
        };
        let own_cause = row.exit_cause != ExitCause::Unspecified as i32
            && row.exit_cause != ExitCause::Suspended as i32;
        let host_end = if own_cause {
            wire::IncarnationEnd {
                cause: row.exit_cause,
                detail: row.exit_detail.clone(),
            }
        } else {
            let cause = match ended {
                Ended::Away => ExitCause::EndedWhileAway,
                _ => ExitCause::EndedUnexpectedly,
            };
            wire::IncarnationEnd {
                cause: cause as i32,
                detail: None,
            }
        };
        let subagents = self.subagents_ending_with(&key, &host_end)?;
        if !subagents.is_empty() {
            self.store.put_agents(&subagents)?;
            self.subagents_ended_with_host(&key, &subagents, out)?;
        }
        if row.phase() != wire::Phase::Idle {
            self.store
                .set_phase(&key, wire::Phase::Idle, self.timers.now_ms as i64)?;
        }
        if row.exit_cause != ExitCause::Suspended as i32 {
            row.exit_cause = ExitCause::Suspended as i32;
            row.exit_detail = None;
            self.put_own_row(&row, out)?;
        } else {
            self.publish_row(&key, out)?;
        }
        self.journal_readers.remove(&agent);
        if self.store.markers().get(&key) == Some(&Marker::CaughtUp) {
            return Ok(());
        }
        self.announce_caught_up(&key, out)
    }

    /// Marks the gone process's row exited as stopped, whatever cause its
    /// journal ended with, and caught up.
    fn mark_stopped(&mut self, agent: AgentId, out: &mut Vec<Output>) -> Result<(), StoreError> {
        let key = self.own_key(agent);
        let Some(mut row) = self.store.agent(&key)? else {
            return Ok(());
        };
        row.exit_cause = ExitCause::Stopped as i32;
        row.exit_detail = None;
        let stopped = wire::IncarnationEnd {
            cause: ExitCause::Stopped as i32,
            detail: None,
        };
        self.record_exit(row, stopped, out)?;
        self.journal_readers.remove(&agent);
        if self.store.markers().get(&key) == Some(&Marker::CaughtUp) {
            return Ok(());
        }
        self.announce_caught_up(&key, out)
    }

    /// Whether the agent's row is live with no process: suspended, or left
    /// so by a restart of the machine. A message resumes it, and a stop
    /// marks it exited.
    pub(crate) fn suspended(&self, agent: AgentId, row: &AgentRow) -> bool {
        row.lifecycle == Lifecycle::Live as i32
            && row.exit_cause == ExitCause::Suspended as i32
            && !self.families.processes.contains_key(&agent)
    }

    /// Marks a suspended agent exited as stopped: it has no process to stop.
    /// Its children follow it as they follow any exit.
    fn stop_suspended(&mut self, agent: AgentId, out: &mut Vec<Output>) -> Result<(), StoreError> {
        let Some(mut row) = self.store.agent(&self.own_key(agent))? else {
            return Ok(());
        };
        if !self.suspended(agent, &row) {
            return Ok(());
        }
        row.exit_cause = ExitCause::Stopped as i32;
        row.exit_detail = None;
        let stopped = wire::IncarnationEnd {
            cause: ExitCause::Stopped as i32,
            detail: None,
        };
        self.record_exit(row, stopped, out)
    }

    /// The daemon has been asked to stop: under the suspension experiment
    /// it records no more exits from here on.
    pub(crate) fn daemon_stopping(&mut self) {
        if self.options.suspension {
            self.families.stopping = true;
        }
    }

    /// Whether the agent's causeless end waits out its grace first; arms it
    /// when it begins.
    fn awaits_grace(
        &mut self,
        agent: AgentId,
        ended: &Ended,
        row: Option<&AgentRow>,
        out: &mut Vec<Output>,
    ) -> bool {
        let starting = self.doing(agent) == Some(&Doing::Starting);
        let suspension = self.options.suspension;
        let Some(process) = self.families.processes.get_mut(&agent) else {
            return false;
        };
        if let Some(over) = process.grace {
            return !over;
        }
        // The grace only defers the end to the next start, should the daemon
        // be stopped meanwhile, which decides as for an agent that ended
        // while it was away.
        let causeless =
            row.is_some_and(|row| row.lifecycle == Lifecycle::Live as i32 && shutdown_like(row));
        // A stop the daemon asked for, or a start that failed, is no news
        // a shutdown could get ahead of.
        if !suspension
            || *ended != Ended::Exited
            || process.stopping.is_some()
            || starting
            || !causeless
        {
            return false;
        }
        process.grace = Some(false);
        self.arm_after(TimerId::Grace(agent), CAUSELESS_GRACE_MS, out);
        true
    }

    /// What the gone process leaves its row as. A row its journal left
    /// suspended stays so, with the experiment on or not. After a restart
    /// of the machine, under the experiment, an agent whose journal gives
    /// no cause ended with the machine: idle, it is suspended; in a turn,
    /// it is continued.
    fn gone(&self, ended: &Ended, process: &Process, row: Option<&AgentRow>) -> Gone {
        let live = row.filter(|row| row.lifecycle == Lifecycle::Live as i32);
        let restarted = *ended == Ended::Away && self.options.suspension && self.options.rebooted;
        // One the person closed was on its way out; it has no turn to carry
        // on and nothing may wake it: a restart that came between the close
        // and the end of a suspension it met leaves it so.
        if let Some(row) = live
            && row.is_closed()
            && (row.exit_cause == ExitCause::Suspended as i32 || restarted && shutdown_like(row))
        {
            return Gone::Stopped;
        }
        if live.is_some_and(|row| row.exit_cause == ExitCause::Suspended as i32) {
            return Gone::Suspended;
        }
        // The provider's own end can come first in a shutdown, its journal
        // then saying it was signalled, or that it quit as it was asked to:
        // with the machine restarted since, that is the shutdown. An agent
        // whose conversation no new process can reopen ended for good with
        // it, as any end of its process does: suspending it would offer a
        // resume that cannot happen.
        if restarted
            && let Some(row) = live
            && shutdown_like(row)
            && reopens(row)
        {
            // A provider subagent still running is its work under way, as
            // a turn is.
            let runs_subagents = self.store.agents_of_parent(&row.agent).is_ok_and(|rows| {
                rows.iter()
                    .any(|sub| sub.native.is_some() && sub.lifecycle == Lifecycle::Live as i32)
            });
            return if row.phase() == wire::Phase::Idle && !runs_subagents {
                Gone::Suspended
            } else {
                Gone::Continued
            };
        }
        // Unless the journal, now read to its end, said why.
        let (cause, detail, if_live) = match ended {
            Ended::Exited if process.stopping == Some(Stopping::Kill) => {
                (ExitCause::Killed, None, false)
            }
            Ended::Exited => (ExitCause::EndedUnexpectedly, None, false),
            Ended::CouldNotStart(words) => (
                ExitCause::CouldNotStart,
                Some(format!("the agent process: {words}")),
                false,
            ),
            Ended::Away => (ExitCause::EndedWhileAway, None, true),
            // A create the daemon before was making a workspace for when it
            // went: nothing of its process was ever written.
            Ended::NoDirectory if row.is_some_and(|row| self.row_was_creating(row)) => (
                ExitCause::CouldNotStart,
                Some("its workspace was being made when the daemon restarted".to_owned()),
                true,
            ),
            Ended::NoDirectory => (
                ExitCause::DirectoryFailed,
                Some("it is gone".to_owned()),
                true,
            ),
        };
        Gone::Exited {
            fallback: wire::IncarnationEnd {
                cause: cause as i32,
                detail,
            },
            if_live,
        }
    }

    /// The operation that waited on the gone process goes on.
    fn after_gone(&mut self, agent: AgentId, ended: Ended, out: &mut Vec<Output>) {
        match self.doing(agent).cloned() {
            Some(Doing::Starting) => {
                out.push(Output::CancelTimer {
                    id: TimerId::Start(agent),
                });
                let outcome = match ended {
                    Ended::CouldNotStart(words) => Err(Refusal::CouldNotStart(words)),
                    _ => self.row_outcome(agent),
                };
                self.finish(agent, outcome, out);
            }
            Some(Doing::AwaitExit { resume }) => {
                out.push(Output::CancelTimer {
                    id: TimerId::Exit(agent),
                });
                let doing = self.await_unlock(resume, out);
                self.set_doing(agent, doing);
            }
            Some(Doing::Ending { then, .. }) => {
                out.push(Output::CancelTimer {
                    id: TimerId::Exit(agent),
                });
                match then {
                    Then::Answer => {
                        // A stop asked of a process that went its own way,
                        // suspending as it was asked, still stops it.
                        if let Err(error) = self.stop_suspended(agent, out) {
                            tracing::warn!(agent = %crate::hex(&agent.0), %error, "marking a suspended agent stopped failed");
                        }
                        let outcome = self.row_outcome(agent);
                        self.finish(agent, outcome, out);
                    }
                    Then::Delete { discard } => match self.remove_agent(agent, discard, out) {
                        Ok(Some(doing)) => self.set_doing(agent, doing),
                        Ok(None) => self.finish(agent, Ok(None), out),
                        Err(refusal) => self.finish(agent, Err(refusal), out),
                    },
                }
            }
            Some(
                Doing::Worktree { .. }
                | Doing::Planning { .. }
                | Doing::Making { .. }
                | Doing::AwaitLaunch { .. }
                | Doing::AwaitUnlock { .. }
                | Doing::AwaitSpec { .. }
                | Doing::Removing { .. }
                | Doing::StoppingSubagent { .. },
            )
            | None => {}
        }
    }

    /// A registry timer fired.
    pub(crate) fn registry_timer(&mut self, id: TimerId, out: &mut Vec<Output>) {
        match id {
            TimerId::Start(agent) => {
                if self.doing(agent) == Some(&Doing::Starting) {
                    let job = self.pending.mint();
                    out.push(Output::Job {
                        id: job,
                        op: Job::KillAgent { agent },
                    });
                    self.finish(agent, Err(Refusal::StartTimeout), out);
                }
            }
            TimerId::Exit(agent) => match self.doing(agent).cloned() {
                // Past the deadline the stop becomes a kill.
                Some(Doing::Ending {
                    phase: Phase::Asked,
                    then,
                }) => {
                    let doing = self.ask_kill(agent, then, out);
                    self.set_doing(agent, doing);
                }
                Some(Doing::Ending {
                    phase: Phase::KillAsked,
                    then,
                }) => {
                    let job = self.pending.mint();
                    out.push(Output::Job {
                        id: job,
                        op: Job::KillAgent { agent },
                    });
                    self.set_doing(
                        agent,
                        Doing::Ending {
                            phase: Phase::Killing { job: Some(job) },
                            then,
                        },
                    );
                }
                Some(Doing::Ending {
                    phase: Phase::Killing { job: None },
                    then,
                }) => self.stop_timed_out(agent, then, out),
                Some(Doing::AwaitExit { .. }) => self.finish(agent, Err(Refusal::StillLocked), out),
                Some(Doing::StoppingSubagent {
                    filing,
                    filed_at_ms,
                }) => self.subagent_stop_timed_out(agent, filing, filed_at_ms, out),
                _ => {}
            },
            TimerId::Grace(agent) => {
                if let Some(process) = self.families.processes.get_mut(&agent)
                    && process.grace == Some(false)
                {
                    process.grace = Some(true);
                    self.journal_read(agent, out);
                }
            }
            _ => {}
        }
    }

    fn stop_timed_out(&mut self, agent: AgentId, then: Then, out: &mut Vec<Output>) {
        if matches!(then, Then::Delete { .. }) {
            let key = self.own_key(agent);
            self.unmute_published(&key, out);
        }
        self.finish(agent, Err(Refusal::StopTimeout), out);
    }

    /// A registry job finished. Returns whether one waited on it.
    pub(crate) fn registry_job(
        &mut self,
        job: JobId,
        result: JobResult,
        out: &mut Vec<Output>,
    ) -> bool {
        let Some((agent, doing)) = self.families.lanes.iter().find_map(|(agent, lane)| {
            let doing = &lane.current.as_ref()?.doing;
            let waits = match doing {
                Doing::Worktree { job: waiting, .. }
                | Doing::Planning { job: waiting, .. }
                | Doing::AwaitUnlock { job: waiting, .. }
                | Doing::AwaitSpec { job: waiting, .. }
                | Doing::Removing { job: waiting } => *waiting == job,
                Doing::Ending {
                    phase: Phase::Killing { job: Some(waiting) },
                    ..
                } => *waiting == job,
                _ => false,
            };
            waits.then(|| (*agent, doing.clone()))
        }) else {
            return false;
        };
        match doing {
            Doing::Worktree { create, name, .. } => {
                self.worktree_made(agent, create, name, result, out)
            }
            Doing::Planning {
                create,
                name,
                taken,
                ..
            } => self.workspace_planned(agent, create, name, taken, result, out),
            Doing::AwaitUnlock { resume, .. } => match result {
                JobResult::Done => match self.ask_spec(resume, out) {
                    Ok(doing) => self.set_doing(agent, doing),
                    Err(error) => self.finish(agent, Err(error.into()), out),
                },
                _ => self.finish(agent, Err(Refusal::StillLocked), out),
            },
            Doing::AwaitSpec { resume, .. } => match result {
                JobResult::Spec(spec) => match self.resume_unlocked(&resume, spec.map(|s| *s), out)
                {
                    Ok(doing) => self.set_doing(agent, doing),
                    Err(refusal) => self.finish(agent, Err(refusal), out),
                },
                JobResult::Refused(error) => self.finish(agent, Err(Refusal::Spec(error)), out),
                JobResult::Failed(words) => {
                    self.finish(agent, Err(Refusal::CouldNotStart(words)), out)
                }
                other => self.finish(
                    agent,
                    Err(Refusal::CouldNotStart(format!("{other:?}"))),
                    out,
                ),
            },
            Doing::Ending { then, .. } => match result {
                JobResult::Done => {
                    self.arm_after(TimerId::Exit(agent), KILL_PATIENCE_MS, out);
                    self.set_doing(
                        agent,
                        Doing::Ending {
                            phase: Phase::Killing { job: None },
                            then,
                        },
                    );
                }
                _ => self.stop_timed_out(agent, then, out),
            },
            Doing::Removing { .. } => {
                let key = self.own_key(agent);
                self.answers.unmute(&key);
                let outcome = match result {
                    JobResult::Done => {
                        self.forget_draft(&agent.0);
                        Ok(None)
                    }
                    other => Err(Refusal::Directory(format!("{other:?}"))),
                };
                self.finish(agent, outcome, out);
            }
            Doing::Starting
            | Doing::Making { .. }
            | Doing::AwaitLaunch { .. }
            | Doing::AwaitExit { .. }
            | Doing::StoppingSubagent { .. } => {}
        }
        true
    }

    /// The start's sweep has reported every agent it found: the children
    /// waiting to follow their parents do.
    pub(crate) fn swept(&mut self, out: &mut Vec<Output>) {
        self.families.swept = true;
        self.resolve_members(out);
        for (child, says) in std::mem::take(&mut self.families.held) {
            self.follow(child, says, out);
        }
    }

    pub(crate) fn doing(&self, agent: AgentId) -> Option<&Doing> {
        Some(&self.families.lanes.get(&agent)?.current.as_ref()?.doing)
    }

    fn set_doing(&mut self, agent: AgentId, doing: Doing) {
        if let Some(operation) = self
            .families
            .lanes
            .get_mut(&agent)
            .and_then(|lane| lane.current.as_mut())
        {
            operation.doing = doing;
        }
    }

    // --- the family rule ---------------------------------------------------------

    /// Each of this host's own agents under `parent`, whose row now says
    /// `says`, follows it: exited stops each live child, closed closes
    /// each child not yet closed, deleted deletes each child. A parent that is merely out of reach says neither, so
    /// its children carry on and their reports wait in the outbox. An exit
    /// or a close is of the incarnation the parent's row holds here, and a
    /// child that has since heard from a later incarnation of its parent
    /// was engaged after it: word of it came late, and the child carries
    /// on.
    pub(crate) fn parent_changed(
        &mut self,
        parent: &AgentKey,
        says: ParentSays,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        let said_at = match says {
            ParentSays::Exited | ParentSays::Closed => {
                self.store.agent(parent)?.map(|row| row.incarnation)
            }
            ParentSays::Deleted => None,
        };
        for child in self.store.agents_of_parent(parent)? {
            if child.agent.host != self.store.own_host()
                || (says == ParentSays::Exited && child.lifecycle != Lifecycle::Live as i32)
                || said_at.is_some_and(|said| child.parent_incarnation > said)
                || (says == ParentSays::Closed && child.is_closed())
            {
                continue;
            }
            if let Some(child) = AgentId::from_slice(&child.agent.agent) {
                if says == ParentSays::Deleted {
                    let caught_up = crate::HostId::from_slice(&parent.host).is_some_and(|host| {
                        self.sources.ready(host) || self.touched.judging == Some(host)
                    });
                    self.touched
                        .deleted_parents
                        .push((parent.clone(), caught_up));
                }
                self.follow(child, says, out);
            }
        }
        Ok(())
    }

    /// Stops or deletes one of this host's own agents because its parent's
    /// row said so, once the start's sweep has adopted every running agent.
    pub(crate) fn follow(&mut self, child: AgentId, says: ParentSays, out: &mut Vec<Output>) {
        if !self.families.swept {
            self.families.held.push((child, says));
            return;
        }
        let op = match says {
            ParentSays::Exited => Op::Stop,
            ParentSays::Closed => Op::File(Filing::Closed),
            // A child goes with its parent keeping its changes.
            ParentSays::Deleted => Op::Delete { discard: false },
        };
        self.registry_call(Caller::Family, child, op, out);
    }

    /// Whether `parent`, its host saying incarnation `calling` is calling,
    /// has already ended as this host sees it: exited by its row, or
    /// deleted. Another host's row counts only once that host's listing has
    /// caught up, and only at the calling incarnation or later, since a row
    /// kept from before may be stale; and a missing row means deleted only
    /// for this host's own agents, since another host's row may simply not
    /// have arrived yet.
    fn parent_ended(
        &self,
        parent: &AgentKey,
        calling: u32,
    ) -> Result<Option<ParentSays>, StoreError> {
        let own = parent.host == self.store.own_host();
        if !own
            && !crate::HostId::from_slice(&parent.host).is_some_and(|host| self.sources.ready(host))
        {
            return Ok(None);
        }
        Ok(match self.store.agent(parent)? {
            Some(row)
                if row.lifecycle == Lifecycle::Exited as i32 && row.incarnation >= calling =>
            {
                Some(ParentSays::Exited)
            }
            None if own => Some(ParentSays::Deleted),
            _ => None,
        })
    }

    /// An agent calling on one of this host's agents, its host saying it is
    /// incarnation `incarnation`: when it is the agent's parent, the row
    /// keeps the newest incarnation of its parent heard from, which the
    /// agent's reports are for and its parent's wait counts. A person's or
    /// another agent's call never moves it.
    pub(crate) fn heard_from_caller(
        &mut self,
        call: CallId,
        agent: AgentId,
        caller: (crate::HostId, AgentId),
        incarnation: u32,
        out: &mut Vec<Output>,
    ) -> Result<(), CallError> {
        let caller = AgentKey::new(caller.0.0.to_vec(), caller.1.0.to_vec());
        if let Some(mut row) = self.store.agent(&self.own_key(agent))?
            && row.parent.as_ref() == Some(&caller)
            && incarnation > row.parent_incarnation
        {
            row.parent_incarnation = incarnation;
            self.put_own_row(&row, out)?;
        }
        out.push(Output::EndCall {
            call,
            status: Status::Ok,
        });
        Ok(())
    }

    /// The names this host's own agents hold, exited ones included.
    pub(crate) fn own_names(&self) -> Result<BTreeSet<String>, StoreError> {
        let own = self.store.own_host();
        Ok(self
            .store
            .agents()?
            .into_iter()
            .filter(|row| row.agent.host == own)
            .map(|row| row.name)
            .collect())
    }

    /// Writes an own row's registry fields and tells inventory subscribers.
    /// A lifecycle or incarnation change can make a waiting delivery
    /// deliverable, or stale.
    pub(crate) fn put_own_row(
        &mut self,
        row: &AgentRow,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        self.store.put_agent(row)?;
        self.deliveries_due();
        self.publish_row(&row.agent, out)?;
        self.recount_from(row.parent.clone(), out)
    }

    /// Marks the row exited, unless `if_live` and it already says so, and
    /// sets its CaughtUp marker: everything the process wrote is
    /// committed, and nothing more comes.
    fn mark_exited(
        &mut self,
        agent: AgentId,
        fallback: wire::IncarnationEnd,
        if_live: bool,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        let key = self.own_key(agent);
        let Some(row) = self.store.agent(&key)? else {
            return Ok(());
        };
        if !if_live || row.lifecycle != Lifecycle::Exited as i32 {
            self.record_exit(row, fallback, out)?;
        } else {
            self.end_subagents_of(&row, out)?;
        }
        // Nothing more comes, so its journal's file is not held open: a
        // daemon may hold the history of many agents that have ended.
        self.journal_readers.remove(&agent);
        if self.store.markers().get(&key) == Some(&Marker::CaughtUp) {
            // A subscriber that joined since read it at its cut, and one
            // that joined before was told then.
            return Ok(());
        }
        self.announce_caught_up(&key, out)
    }

    /// Tells the person a top-level agent's process died, once the delay is
    /// up: one resumed by then has nothing to report. An end somebody meant,
    /// or whose cause is not known, is news to nobody; unknown is not failure.
    fn owe_news_of_a_death(
        &mut self,
        row: &AgentRow,
        cause: &wire::IncarnationEnd,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        // With nobody to tell, a row would only hold a timer armed to find
        // that out.
        if !ExitCause::failed(cause.cause) || self.push_recipients().is_empty() {
            return Ok(());
        }
        self.store.put_notification(&Notification {
            agent_id: row.agent.agent.clone(),
            revision: row.next_revision,
            due_at_ms: self.timers.now_ms as i64 + self.settings.notify_delay_ms,
            name: row.title.clone().unwrap_or_else(|| row.name.clone()),
            notice: wire::Notice {
                of: Some(wire::notice::Of::Died(wire::DiedNotice {
                    ended: Some(cause.clone()),
                })),
            }
            .encode_to_vec(),
        })?;
        self.drain_notifications(out);
        Ok(())
    }

    /// Marks the row exited, then closes what the process leaves: its
    /// unsent notifications go, since no push may name an agent that is
    /// gone, a top-level agent that died is news, and a child whose incarnation ended without sending its last
    /// report tells its parent it failed: its turn was open, or it ended
    /// idle, as a sub-agent stopped while it waits for its own child does.
    /// A turn end marked last already told it.
    ///
    /// The row keeps the cause its journal's last step put there at commit;
    /// `fallback` is the daemon's own, for an incarnation whose journal
    /// stops without saying why.
    pub(crate) fn record_exit(
        &mut self,
        mut row: AgentRow,
        fallback: wire::IncarnationEnd,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        row.lifecycle = Lifecycle::Exited as i32;
        if row.exit_cause == ExitCause::Unspecified as i32 {
            row.exit_cause = fallback.cause;
            row.exit_detail = fallback.detail;
        }
        // A one-shot child that did its work is done with, as its parent
        // closing it would make it.
        if row.exit_cause == ExitCause::Finished as i32 && !row.is_closed() {
            row.filing = wire::Filing::Closed as i32;
            row.filed_at_ms = self.timers.now_ms as i64;
        }
        let cause = wire::IncarnationEnd {
            cause: row.exit_cause,
            detail: row.exit_detail.clone(),
        };
        // The provider subagents its process ran end with it, in the same
        // write: none outlives the process that ran it, even for a moment
        // a subscriber could see.
        let subagents = match row.native {
            None => self.subagents_ending_with(&row.agent, &cause)?,
            Some(_) => Vec::new(),
        };
        if subagents.is_empty() {
            self.put_own_row(&row, out)?;
        } else {
            let mut rows = vec![row.clone()];
            rows.extend(subagents.iter().cloned());
            self.store.put_agents(&rows)?;
            self.deliveries_due();
            self.publish_row(&row.agent, out)?;
            self.subagents_ended_with_host(&row.agent, &subagents, out)?;
            self.recount_from(row.parent.clone(), out)?;
        }
        self.parent_changed(&row.agent, ParentSays::Exited, out)?;
        self.store.remove_notifications(&row.agent.agent)?;
        let Some(parent) = &row.parent else {
            self.owe_news_of_a_death(&row, &cause, out)?;
            return Ok(());
        };
        // As committed: the phase at exit says nothing here, since a
        // provider's exit leaves it starting whether or not the turn ended.
        let held = self.store.agent(&row.agent)?.unwrap_or(row.clone());
        // A provider subagent's answer went back through its parent's
        // provider: it never owes a report.
        if !held.owes_report || held.native.is_some() {
            return Ok(());
        }
        // For the parent incarnation the child last heard from, as a turn
        // end's report is.
        self.store.put_delivery(&Delivery {
            child_id: row.agent.agent.clone(),
            incarnation: row.incarnation,
            turn_id: 0,
            parent: parent.clone(),
            parent_incarnation: held.parent_incarnation,
            end: wire::child_report::End::Exited(wire::Exited { ended: Some(cause) }),
        })?;
        self.deliveries_due();
        Ok(())
    }

    /// Counts the live descendants of `parent` again, and of each own
    /// ancestor above it, writing and answering every count that changed;
    /// a count that did not change leaves those above it unchanged too. A
    /// replica ancestor's count is its origin's. A running own `parent`
    /// also hears which of its children are live, when that changed.
    pub(crate) fn recount_from(
        &mut self,
        mut parent: Option<AgentKey>,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        let own = self.store.own_host().to_vec();
        if let Some(key) = &parent
            && key.host == own
            && let Some(agent) = AgentId::from_slice(&key.agent)
        {
            self.tell_children(agent, out)?;
        }
        while let Some(key) = parent {
            if key.host != own {
                break;
            }
            let Some(row) = self.store.agent(&key)? else {
                break;
            };
            let count = self.store.live_descendants(&key)?;
            if count == row.descendants {
                break;
            }
            self.store.set_descendants(&key, count)?;
            self.publish_row(&key, out)?;
            parent = row.parent;
        }
        Ok(())
    }

    /// Tells a running agent which agents directly below it are now live,
    /// when that changed: a one-shot agent waits for the last report of
    /// every child incarnation it was told of.
    fn tell_children(&mut self, agent: AgentId, out: &mut Vec<Output>) -> Result<(), StoreError> {
        if !self.families.processes.contains_key(&agent) {
            return Ok(());
        }
        let children = self.children_of(agent)?;
        let Some(process) = self.families.processes.get_mut(&agent) else {
            return Ok(());
        };
        if process.told == children {
            return Ok(());
        }
        process.told = children;
        if process.hello {
            out.push(Output::WriteControl {
                agent,
                frame: children_frame(&process.told),
            });
        }
        Ok(())
    }

    /// The live agents directly below `agent` on any host with their
    /// incarnations, in agent id order so two lists compare equal when
    /// nothing changed. A child on another host is here once its host lists
    /// it; until then the agent's own process holds it, having carried the
    /// create itself, and a list from here only ever adds to what it holds.
    /// Only a child last engaged by an incarnation of the agent's current
    /// run is named: one an earlier life engaged reports to that life,
    /// which is gone, so the agent never hears its last report.
    fn children_of(&self, agent: AgentId) -> Result<Vec<(Vec<u8>, u32)>, StoreError> {
        let key = self.own_key(agent);
        let Some(parent) = self.store.agent(&key)? else {
            return Ok(Vec::new());
        };
        let rows = self.store.agents_of_parent(&key)?;
        // A provider subagent sends no report, so a parent told of one would
        // wait for it forever; the parent's own process follows those it
        // runs from its journal.
        let mut children = Vec::new();
        for row in rows {
            if row.lifecycle == Lifecycle::Live as i32
                && row.native.is_none()
                && self.of_current_run(&parent, row.parent_incarnation)?
            {
                children.push((row.agent.agent.clone(), row.incarnation));
            }
        }
        children.sort();
        Ok(children)
    }
}

/// What an agent the machine restarted under in its turn is resumed with:
/// a prompt saying so, marked a continuation for the interpreter to draw as
/// one rather than as a person's message.
fn continue_after_restart(row: &AgentRow) -> wire::Bootstrap {
    wire::Bootstrap {
        of: Some(wire::bootstrap::Of::Prompt(wire::PromptSubmission {
            input_id: continued_input_id(&row.agent.agent, row.incarnation + 1),
            prompt: Some(wire::PromptInput {
                text: CONTINUE_AFTER_RESTART.to_owned(),
                ..Default::default()
            }),
            continued: true,
        })),
    }
}

/// The input id of the continuation that begins `incarnation` of `agent`:
/// sixteen bytes, as every input id is, unique to the incarnation, so a
/// retried resume is the same input.
fn continued_input_id(agent: &[u8], incarnation: u32) -> Vec<u8> {
    let mut id = b"continue".to_vec();
    id.extend(incarnation.to_be_bytes());
    id.extend(agent.iter().copied().chain(std::iter::repeat(0)).take(4));
    id
}

/// A Stop frame: a kill ends the provider's group at once.
fn stop(kill: bool) -> CtlFrame {
    CtlFrame {
        of: Some(ctl_frame::Of::Stop(wire::Stop { kill })),
    }
}

/// What a `Children` frame says.
fn children_frame(children: &[(Vec<u8>, u32)]) -> CtlFrame {
    CtlFrame {
        of: Some(ctl_frame::Of::Children(wire::Children {
            children: children
                .iter()
                .map(|(agent_id, incarnation)| wire::LiveChild {
                    agent_id: agent_id.clone(),
                    incarnation: *incarnation,
                })
                .collect(),
        })),
    }
}

/// A create's first spec, with what the registry decided for it.
fn first_spec(
    create: Box<Create>,
    name: String,
    cwd: String,
    base_branch: Option<String>,
) -> AgentSpec {
    let mut spec = create.spec;
    spec.name_given = create.given.is_some();
    spec.name = name;
    spec.cwd = cwd;
    spec.incarnation = 1;
    spec.base_branch = base_branch;
    spec
}

/// Whether a new process of the agent's program can carry its conversation
/// on, as its newest snapshot's capabilities say. One whose program cannot
/// reopen it, or that has not said it can, ends with its process, so it is
/// never suspended, continued or resumed in a way that would start a fresh
/// conversation as the old one.
pub fn reopens(row: &AgentRow) -> bool {
    row.snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.capabilities.as_ref())
        .is_some_and(|capabilities| capabilities.resumable)
}

/// Whether the end a row's journal gave is one a machine shutting down can
/// give before the daemon sees the process go: none at all, or the
/// provider's own end by a signal, killed by it or exiting with its status
/// (see [`wire::provider_signalled`]). amux's stops and kills end with
/// causes of their own, so a signalled provider was ended from outside. A
/// clean exit counts too, since a provider that exits cleanly when the
/// machine signals it reads as a person quitting it. Only terminal Claude
/// can be quit by a person, in its terminal, so its quit counts only with
/// no turn under way (its last snapshot idle): a person who quit in a turn
/// wanted it to end, so it is never continued. A headless provider has no
/// terminal to quit from (Codex's terminal is a view that leaves its app
/// server running), so its clean exit, as Codex's app server gives on
/// SIGTERM mid-turn, counts whatever the phase. The grace only defers any of these,
/// and the next start takes them for a shutdown only if the machine
/// restarted, recording them as they are otherwise.
fn shutdown_like(row: &AgentRow) -> bool {
    match ExitCause::try_from(row.exit_cause) {
        Ok(ExitCause::Unspecified) => true,
        Ok(ExitCause::Quit) => {
            row.kind != wire::Kind::ClaudePty as i32 || row.phase() == wire::Phase::Idle
        }
        Ok(ExitCause::ProviderExited) => wire::provider_signalled(row.exit_detail.as_deref()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use store::Sqlite;

    use super::*;
    use crate::payload::ClientRequest;
    use crate::{Input, StartOptions};

    const OWN: [u8; 16] = *b"own-host-0000000";
    const PARENT: AgentId = AgentId(*b"parent-000000000");
    const CHILD: AgentId = AgentId(*b"child-0000000000");
    const OTHER: AgentId = AgentId(*b"other-0000000000");

    struct Rig {
        daemon: Daemon,
        next_call: u64,
        _dir: tempfile::TempDir,
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let daemon = Daemon::new(
            Sqlite::open_in_memory(OWN).expect("a store opens"),
            StartOptions {
                generation: 1,
                agents: dir.path().to_path_buf(),
                budget: crate::StepBudget::UNLIMITED,
                ..StartOptions::default()
            },
        );
        let mut rig = Rig {
            daemon,
            next_call: 0,
            _dir: dir,
        };
        rig.step(Input::Swept);
        rig
    }

    impl Rig {
        fn step(&mut self, input: Input) -> Vec<Output> {
            let mut out = Vec::new();
            self.daemon.step(input, &mut out);
            out
        }

        fn call(&mut self, request: ClientRequest) -> (CallId, Vec<Output>) {
            self.next_call += 1;
            let call = CallId(self.next_call);
            (call, self.step(Input::ClientCall { call, request }))
        }

        fn row(&self, agent: AgentId) -> Option<AgentRow> {
            self.daemon
                .store
                .agent(&self.daemon.own_key(agent))
                .unwrap()
        }

        /// Creates `agent` under `parent` and has its process say Hello.
        fn live(&mut self, agent: AgentId, name: &str, parent: Option<AgentId>) {
            let (call, out) = self.call(create(agent, name, parent));
            assert!(out.contains(&Output::DialAgent { agent }), "{out:?}");
            let out = self.step(Input::AgentHello { agent });
            assert!(answer_of(&out, call).is_some(), "{out:?}");
        }
    }

    /// A rig whose daemon runs the suspension experiment, its store holding
    /// the experiment's table, with `rebooted` as the start found it; the
    /// sweep not yet reported.
    fn suspension_rig(rebooted: bool) -> Rig {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let mut store = Sqlite::open_in_memory(OWN).expect("a store opens");
        store
            .settle_experiments(&[wire::experimental::SUSPENSION.version()])
            .expect("the experiment's tables are made");
        let daemon = Daemon::new(
            store,
            StartOptions {
                generation: 1,
                agents: dir.path().to_path_buf(),
                suspension: true,
                rebooted,
                budget: crate::StepBudget::UNLIMITED,
                ..StartOptions::default()
            },
        );
        Rig {
            daemon,
            next_call: 0,
            _dir: dir,
        }
    }

    impl Rig {
        /// The agent's process exits as suspended: its journal's last step
        /// put the cause on the row, as a commit does, then it is gone.
        fn suspend(&mut self, agent: AgentId) -> Vec<Output> {
            let mut row = self.row(agent).unwrap();
            row.exit_cause = ExitCause::Suspended as i32;
            row.status.phase = wire::Phase::Starting as i32;
            self.daemon.store.put_agent(&row).unwrap();
            self.step(Input::ControlClosed {
                agent,
                ended: Ended::Exited,
            })
        }

        /// An own row as a store left it: no process, nothing under way.
        fn left(&mut self, agent: AgentId, phase: wire::Phase, cause: ExitCause) {
            let mut row = AgentRow::new(self.daemon.own_key(agent), wire::Kind::Codex, "/src");
            row.incarnation = 1;
            row.status.phase = phase as i32;
            row.exit_cause = cause as i32;
            // Its last snapshot, which says its program can reopen it.
            row.snapshot = Some(wire::Snapshot {
                kind: wire::Kind::Codex as i32,
                status: wire::AgentStatus::in_phase(phase),
                capabilities: Some(Box::new(wire::AgentCapabilities::codex(
                    wire::Platform::here(),
                ))),
                ..wire::Snapshot::default()
            });
            self.daemon.store.put_agent(&row).unwrap();
        }

        /// Runs a resume the outputs began through its jobs, to the start of
        /// `incarnation`; returns the bootstrap the spec was asked with.
        fn resume_through(&mut self, out: &[Output], incarnation: u32) -> Option<wire::Bootstrap> {
            let Some((unlock, Job::AwaitUnlocked { agent, .. })) = job_of(out) else {
                panic!("the resume waits for the directory: {out:?}");
            };
            let unlocked = self.step(Input::JobDone {
                job: unlock,
                result: JobResult::Done,
            });
            let Some((spec, Job::NextSpec { bootstrap, .. })) = job_of(&unlocked) else {
                panic!("the spec is asked for: {unlocked:?}");
            };
            self.step(Input::JobDone {
                job: spec,
                result: JobResult::Spec(Some(Box::new(AgentSpec {
                    agent_id: agent.0.to_vec(),
                    kind: wire::Kind::Codex as i32,
                    incarnation,
                    ..AgentSpec::default()
                }))),
            });
            bootstrap
        }
    }

    fn create(agent: AgentId, name: &str, parent: Option<AgentId>) -> ClientRequest {
        ClientRequest::CreateAgent(Box::new(Create {
            parent_incarnation: u32::from(parent.is_some()),
            agent,
            cwd: "/src".into(),
            given: Some(name.into()),
            branches: BTreeSet::new(),
            new_workspace: false,
            workspace: None,
            recent: None,
            spec: AgentSpec {
                agent_id: agent.0.to_vec(),
                kind: wire::Kind::Codex as i32,
                parent: parent.map(|parent| wire::AgentRef {
                    host_id: OWN.to_vec(),
                    agent_id: parent.0.to_vec(),
                }),
                ..AgentSpec::default()
            },
        }))
    }

    /// The row a call was answered with.
    fn answer_of(out: &[Output], to: CallId) -> Option<Agent> {
        out.iter().find_map(|output| match output {
            Output::Answer {
                call,
                response: Response::Agent(agent),
            } if *call == to => Some(*agent.clone()),
            _ => None,
        })
    }

    fn refusal_of(out: &[Output], to: CallId) -> Option<Refusal> {
        out.iter().find_map(|output| match output {
            Output::EndCall {
                call,
                status: Status::Failed(CallError::Registry(refusal)),
            } if *call == to => Some(refusal.clone()),
            _ => None,
        })
    }

    fn job_of(out: &[Output]) -> Option<(JobId, Job)> {
        out.iter().find_map(|output| match output {
            Output::Job { id, op } => Some((*id, op.clone())),
            _ => None,
        })
    }

    fn stops(out: &[Output], agent: AgentId) -> Vec<bool> {
        out.iter()
            .filter_map(|output| match output {
                Output::WriteControl {
                    agent: to,
                    frame:
                        CtlFrame {
                            of: Some(ctl_frame::Of::Stop(stop)),
                        },
                } if *to == agent => Some(stop.kill),
                _ => None,
            })
            .collect()
    }

    /// The child's row keeps the newest incarnation of its parent heard
    /// from the parent itself: another agent's call never moves it, and an
    /// older one never lowers it.
    #[test]
    fn a_child_records_only_its_parents_newest_incarnation_heard_from_it() {
        let mut rig = rig();
        rig.live(PARENT, "lead", None);
        rig.live(OTHER, "other", None);
        rig.live(CHILD, "helper", Some(PARENT));
        assert_eq!(
            rig.row(CHILD).unwrap().parent_incarnation,
            1,
            "from the create"
        );
        let heard = |caller: AgentId, incarnation| ClientRequest::HeardFromCaller {
            agent: CHILD,
            caller: (crate::HostId(OWN), caller),
            incarnation,
        };
        for (caller, incarnation, kept) in [(OTHER, 5, 1), (PARENT, 3, 3), (PARENT, 2, 3)] {
            let (call, out) = rig.call(heard(caller, incarnation));
            assert!(
                out.contains(&Output::EndCall {
                    call,
                    status: Status::Ok
                }),
                "{out:?}"
            );
            assert_eq!(rig.row(CHILD).unwrap().parent_incarnation, kept);
        }
    }

    /// Word of a far parent's exit stops a child that last heard from that
    /// incarnation or an earlier one, and not one a later incarnation has
    /// engaged since: the word came late.
    #[test]
    fn a_far_parents_exit_stops_only_a_child_engaged_no_later_than_it() {
        const FAR: [u8; 16] = *b"far-host-0000000";
        for (heard, stopped) in [(1, true), (2, false)] {
            let mut rig = rig();
            let (call, out) = rig.call(ClientRequest::CreateAgent(Box::new(Create {
                parent_incarnation: heard,
                agent: CHILD,
                cwd: "/src".into(),
                given: Some("helper".into()),
                branches: BTreeSet::new(),
                new_workspace: false,
                workspace: None,
                recent: None,
                spec: AgentSpec {
                    agent_id: CHILD.0.to_vec(),
                    kind: wire::Kind::Codex as i32,
                    parent: Some(wire::AgentRef {
                        host_id: FAR.to_vec(),
                        agent_id: PARENT.0.to_vec(),
                    }),
                    ..AgentSpec::default()
                },
            })));
            assert!(out.contains(&Output::DialAgent { agent: CHILD }), "{out:?}");
            let out = rig.step(Input::AgentHello { agent: CHILD });
            assert!(answer_of(&out, call).is_some(), "{out:?}");
            let parent = AgentKey::new(FAR.to_vec(), PARENT.0.to_vec());
            let mut row = AgentRow::new(parent.clone(), wire::Kind::Codex, "/src");
            row.lifecycle = Lifecycle::Exited as i32;
            row.incarnation = 1;
            rig.daemon.store.put_agent(&row).unwrap();
            let mut out = Vec::new();
            rig.daemon
                .parent_changed(&parent, ParentSays::Exited, &mut out)
                .unwrap();
            assert_eq!(
                !stops(&out, CHILD).is_empty(),
                stopped,
                "heard from {heard}: {out:?}"
            );
        }
    }

    #[test]
    fn closing_a_parent_files_it_and_its_child_and_stops_both() {
        let mut rig = rig();
        rig.live(PARENT, "lead", None);
        rig.live(CHILD, "helper", Some(PARENT));
        let (call, out) = rig.call(ClientRequest::SetFiling {
            agent: PARENT,
            filing: Filing::Closed,
        });
        assert!(rig.row(PARENT).unwrap().is_closed());
        assert!(rig.row(CHILD).unwrap().is_closed(), "{out:?}");
        assert_eq!(stops(&out, PARENT), [false], "{out:?}");
        assert_eq!(stops(&out, CHILD), [false], "{out:?}");
        assert!(answer_of(&out, call).is_none(), "answered once it ended");
        let gone = rig.step(Input::ControlClosed {
            agent: PARENT,
            ended: Ended::Exited,
        });
        let row = answer_of(&gone, call).expect("the settle is answered with its row");
        assert_eq!(
            (row.lifecycle(), row.filing()),
            (Lifecycle::Exited, wire::Filing::Closed)
        );
    }

    #[test]
    fn only_an_agent_with_no_parent_is_pinned_and_a_pin_stops_nothing() {
        let mut rig = rig();
        rig.live(PARENT, "lead", None);
        rig.live(CHILD, "helper", Some(PARENT));
        let (call, out) = rig.call(ClientRequest::SetFiling {
            agent: CHILD,
            filing: Filing::Pinned,
        });
        assert_eq!(refusal_of(&out, call), Some(Refusal::HasParent));
        let (call, out) = rig.call(ClientRequest::SetFiling {
            agent: PARENT,
            filing: Filing::Pinned,
        });
        let row = answer_of(&out, call).expect("a pin answers at once");
        assert_eq!(row.filing(), wire::Filing::Pinned);
        assert!(stops(&out, PARENT).is_empty(), "{out:?}");
        assert_eq!(row.lifecycle(), Lifecycle::Live);
    }

    #[test]
    fn a_resume_files_a_closed_agent_active_again() {
        let mut rig = rig();
        rig.live(PARENT, "lead", None);
        rig.call(ClientRequest::SetFiling {
            agent: PARENT,
            filing: Filing::Closed,
        });
        rig.step(Input::ControlClosed {
            agent: PARENT,
            ended: Ended::Exited,
        });
        assert!(rig.row(PARENT).unwrap().is_closed());
        let (_, out) = rig.call(ClientRequest::ResumeAgent(Box::new(Resume {
            agent: PARENT,
            bootstrap: None,
            if_suspended: false,
        })));
        let Some((unlock, Job::AwaitUnlocked { .. })) = job_of(&out) else {
            panic!("the resume waits for the directory: {out:?}");
        };
        let unlocked = rig.step(Input::JobDone {
            job: unlock,
            result: JobResult::Done,
        });
        let Some((spec, Job::NextSpec { .. })) = job_of(&unlocked) else {
            panic!("the spec is asked for: {unlocked:?}");
        };
        rig.step(Input::JobDone {
            job: spec,
            result: JobResult::Spec(Some(Box::new(AgentSpec {
                agent_id: PARENT.0.to_vec(),
                kind: wire::Kind::Codex as i32,
                incarnation: 2,
                ..AgentSpec::default()
            }))),
        });
        let row = rig.row(PARENT).unwrap();
        assert_eq!(
            (row.lifecycle, row.filing),
            (Lifecycle::Live as i32, wire::Filing::Open as i32)
        );
    }

    #[test]
    fn an_agent_that_ended_holds_no_journal_file_open() {
        let mut rig = rig();
        rig.live(PARENT, "lead", None);
        let mut journal = journal::Writer::open(rig.daemon.journal_dir(PARENT), 1 << 20)
            .expect("the journal opens");
        journal
            .append(&wire::Step::default())
            .expect("a frame is written");
        rig.step(Input::Nudge { agent: PARENT });
        assert!(
            rig.daemon.journal_readers.contains_key(&PARENT),
            "a live agent's reader is held between reads"
        );
        rig.step(Input::ControlClosed {
            agent: PARENT,
            ended: Ended::Exited,
        });
        assert!(
            !rig.daemon.journal_readers.contains_key(&PARENT),
            "an ended agent's reader is closed"
        );
    }

    /// A resume issued while the process is still on its way out builds
    /// its spec only once the exit is recorded, from the snapshot the
    /// journal's tail left, so it carries the settings the agent last ran
    /// with.
    #[test]
    fn a_resume_behind_a_stop_asks_for_its_spec_from_the_snapshot_the_exit_committed() {
        let mut rig = rig();
        rig.live(PARENT, "lead", None);
        let mut journal = journal::Writer::open(rig.daemon.journal_dir(PARENT), 1 << 20)
            .expect("the journal opens");
        let mut said = |body: &[u8]| {
            journal
                .append(&wire::Step {
                    snapshot: Some(wire::Snapshot {
                        kind: wire::Kind::Codex as i32,
                        body: body.to_vec(),
                        ..wire::Snapshot::default()
                    }),
                    ..wire::Step::default()
                })
                .expect("a frame is written");
        };
        said(b"model a");
        rig.step(Input::Nudge { agent: PARENT });

        rig.call(ClientRequest::SetFiling {
            agent: PARENT,
            filing: Filing::Closed,
        });
        let (resume, out) = rig.call(ClientRequest::ResumeAgent(Box::new(Resume {
            agent: PARENT,
            bootstrap: None,
            if_suspended: false,
        })));
        assert_eq!(job_of(&out), None, "it waits behind the stop: {out:?}");
        // The last snapshot is written as the agent ends, and read with the
        // rest of its journal at the exit.
        said(b"model b");
        let gone = rig.step(Input::ControlClosed {
            agent: PARENT,
            ended: Ended::Exited,
        });
        let Some((unlock, Job::AwaitUnlocked { .. })) = job_of(&gone) else {
            panic!("the resume waits for the directory: {gone:?}");
        };
        let unlocked = rig.step(Input::JobDone {
            job: unlock,
            result: JobResult::Done,
        });
        let Some((
            spec_job,
            Job::NextSpec {
                agent: PARENT,
                snapshot,
                ..
            },
        )) = job_of(&unlocked)
        else {
            panic!("the spec is asked for once the directory is free: {unlocked:?}");
        };
        assert_eq!(
            snapshot.as_deref().map(|snapshot| snapshot.body.as_slice()),
            Some(&b"model b"[..])
        );

        let built = rig.step(Input::JobDone {
            job: spec_job,
            result: JobResult::Spec(Some(Box::new(AgentSpec {
                agent_id: PARENT.0.to_vec(),
                kind: wire::Kind::Codex as i32,
                incarnation: 2,
                ..AgentSpec::default()
            }))),
        });
        let spawned = built.iter().find_map(|output| match output {
            Output::SpawnAgent { spec, .. } => Some(spec.clone()),
            _ => None,
        });
        assert_eq!(
            spawned.map(|spec| (spec.name, spec.incarnation)),
            Some(("lead".into(), 2)),
            "{built:?}"
        );
        assert_eq!(rig.row(PARENT).unwrap().incarnation, 2);
        let hello = rig.step(Input::AgentHello { agent: PARENT });
        assert!(answer_of(&hello, resume).is_some(), "{hello:?}");
    }

    fn children_told(out: &[Output], agent: AgentId) -> Option<Vec<Vec<u8>>> {
        out.iter().find_map(|output| match output {
            Output::WriteControl {
                agent: to,
                frame:
                    CtlFrame {
                        of: Some(ctl_frame::Of::Children(children)),
                    },
            } if *to == agent => Some(
                children
                    .children
                    .iter()
                    .map(|child| child.agent_id.clone())
                    .collect(),
            ),
            _ => None,
        })
    }

    /// A process the daemon has just started is told nothing of children
    /// it cannot have. One that says Hello with no start waiting on it was
    /// found running, as a restarted daemon finds it: it is told it has
    /// none, which settles a spawn its last daemon never answered.
    #[test]
    fn a_process_found_running_hears_its_children_even_when_it_has_none() {
        let mut rig = rig();
        let (_, out) = rig.call(create(PARENT, "lead", None));
        assert!(out.contains(&Output::DialAgent { agent: PARENT }));
        let started = rig.step(Input::AgentHello { agent: PARENT });
        assert_eq!(children_told(&started, PARENT), None, "{started:?}");

        let found = rig.step(Input::AgentHello { agent: PARENT });
        assert_eq!(children_told(&found, PARENT), Some(Vec::new()), "{found:?}");
    }

    #[test]
    fn a_parents_exit_stops_its_live_children_here_once_the_sweep_is_done() {
        let mut rig = rig();
        rig.daemon.families.swept = false;
        rig.live(PARENT, "lead", None);
        rig.live(CHILD, "scout", Some(PARENT));

        let gone = rig.step(Input::ControlClosed {
            agent: PARENT,
            ended: Ended::Exited,
        });
        assert!(stops(&gone, CHILD).is_empty(), "no stop before the sweep");
        let swept = rig.step(Input::Swept);
        assert_eq!(stops(&swept, CHILD), vec![false], "the child follows");

        let (late, out) = rig.call(create(AgentId(*b"late-00000000000"), "late", Some(PARENT)));
        assert_eq!(refusal_of(&out, late), Some(Refusal::ParentEnded(PARENT)));
    }

    #[test]
    fn a_child_deleted_for_a_parent_whose_hosts_listing_has_not_caught_up_asks_for_a_dump() {
        let mut rig = rig();
        rig.step(Input::Swept);
        let parent = AgentKey::new(vec![9; 16], b"far-parent-00000".to_vec());
        let mut child = AgentRow::new(rig.daemon.own_key(CHILD), 1, "/src");
        child.parent = Some(parent.clone());
        rig.daemon.store.put_agent(&child).unwrap();

        // Removed for good before its host's listing caught up, as no input
        // removes another host's agent: the boundary's check names it.
        rig.daemon.touched = crate::Touched::default();
        rig.daemon.removed_for_good(&parent, &mut Vec::new());
        let violation = rig.daemon.check_step_invariants().unwrap_err();
        assert_eq!(violation.invariant, crate::DELETED_UNDER_A_STALE_LISTING);
    }

    #[test]
    fn a_stop_and_the_delete_queued_behind_it_ending_in_one_step_ask_for_no_dump() {
        let mut rig = rig();
        rig.live(PARENT, "lead", None);
        let mut journal = journal::Writer::open(rig.daemon.journal_dir(PARENT), 1 << 20)
            .expect("the journal opens");
        journal
            .append(&wire::Step {
                items: vec![wire::Item {
                    key: "said".into(),
                    text: "hello".into(),
                    ..wire::Item::default()
                }],
                ..wire::Step::default()
            })
            .expect("a frame is written");
        rig.step(Input::Nudge { agent: PARENT });
        // A daemon that started again adopts the process, whose Hello makes
        // the next read to the journal's end announce its CaughtUp.
        rig.step(Input::AgentHello { agent: PARENT });
        rig.call(ClientRequest::SetFiling {
            agent: PARENT,
            filing: Filing::Closed,
        });
        rig.call(ClientRequest::DeleteAgent {
            agent: PARENT,
            discard: false,
        });

        // The exit announces the CaughtUp, answers the stop, and the delete
        // behind it removes the row, all in one step.
        let gone = rig.step(Input::ControlClosed {
            agent: PARENT,
            ended: Ended::Exited,
        });
        assert!(rig.row(PARENT).is_none(), "the delete went ahead: {gone:?}");
        assert!(
            !gone
                .iter()
                .any(|output| matches!(output, Output::Dump { .. })),
            "an answer about an agent the step went on to delete was committed: {gone:?}"
        );
    }

    #[test]
    fn a_suspended_exit_leaves_the_row_live_and_idle_and_stops_no_child() {
        let mut rig = rig();
        rig.live(PARENT, "lead", None);
        rig.live(CHILD, "scout", Some(PARENT));
        let out = rig.suspend(PARENT);
        let row = rig.row(PARENT).unwrap();
        assert_eq!(
            (row.lifecycle, row.exit_cause, row.phase()),
            (
                Lifecycle::Live as i32,
                ExitCause::Suspended as i32,
                wire::Phase::Idle
            ),
            "{out:?}"
        );
        assert!(stops(&out, CHILD).is_empty(), "{out:?}");
        assert_eq!(rig.row(CHILD).unwrap().lifecycle, Lifecycle::Live as i32);
        assert!(out.contains(&Output::ForgetAgent { agent: PARENT }));
    }

    #[test]
    fn stopping_a_suspended_agent_marks_it_exited_as_stopped_and_its_children_follow() {
        let mut rig = rig();
        rig.live(PARENT, "lead", None);
        rig.live(CHILD, "scout", Some(PARENT));
        rig.suspend(PARENT);
        let (call, out) = rig.call(ClientRequest::KillAgent { agent: PARENT });
        let answered = answer_of(&out, call).expect("answered at once");
        assert_eq!(answered.lifecycle, Lifecycle::Exited as i32);
        assert_eq!(answered.exit_cause(), ExitCause::Stopped);
        assert_eq!(stops(&out, CHILD), [false], "{out:?}");
    }

    #[test]
    fn closing_a_suspended_agent_closes_it_and_marks_it_exited() {
        let mut rig = rig();
        rig.live(PARENT, "lead", None);
        rig.suspend(PARENT);
        let (call, out) = rig.call(ClientRequest::SetFiling {
            agent: PARENT,
            filing: Filing::Closed,
        });
        let answered = answer_of(&out, call).expect("answered at once");
        assert_eq!(
            (answered.lifecycle, answered.exit_cause()),
            (Lifecycle::Exited as i32, ExitCause::Stopped)
        );
        assert!(rig.row(PARENT).unwrap().is_closed());
    }

    #[test]
    fn deleting_a_suspended_agent_removes_it_without_a_stop() {
        let mut rig = rig();
        rig.live(PARENT, "lead", None);
        rig.suspend(PARENT);
        let (_, out) = rig.call(ClientRequest::DeleteAgent {
            agent: PARENT,
            discard: false,
        });
        assert!(stops(&out, PARENT).is_empty(), "{out:?}");
        assert!(rig.row(PARENT).is_none(), "{out:?}");
        assert!(
            matches!(job_of(&out), Some((_, Job::RemoveDir { .. }))),
            "{out:?}"
        );
    }

    #[test]
    fn a_stop_that_meets_a_suspension_still_leaves_the_agent_stopped() {
        let mut rig = rig();
        rig.live(PARENT, "lead", None);
        let (call, _) = rig.call(ClientRequest::KillAgent { agent: PARENT });
        let out = rig.suspend(PARENT);
        let answered = answer_of(&out, call).expect("answered once it ends");
        assert_eq!(
            (answered.lifecycle, answered.exit_cause()),
            (Lifecycle::Exited as i32, ExitCause::Stopped)
        );
    }

    fn wake(agent: AgentId, text: &str) -> ClientRequest {
        ClientRequest::ResumeAgent(Box::new(Resume {
            agent,
            bootstrap: Some(wire::Bootstrap {
                of: Some(wire::bootstrap::Of::Prompt(wire::PromptSubmission {
                    continued: false,
                    input_id: text.as_bytes().to_vec(),
                    prompt: Some(wire::PromptInput {
                        text: text.into(),
                        attachments: Vec::new(),
                    }),
                })),
            }),
            if_suspended: true,
        }))
    }

    /// A person's input to an agent with no process wakes it only if it is
    /// suspended, with the input as the next incarnation's first: one the
    /// person stopped is refused, and one running is left to take the
    /// input itself.
    #[test]
    fn a_wake_resumes_only_a_suspended_agent_with_its_input_first() {
        let mut rig = rig();
        rig.live(PARENT, "lead", None);
        rig.live(CHILD, "scout", None);
        rig.suspend(PARENT);
        let (call, out) = rig.call(wake(PARENT, "go on"));
        let bootstrap = rig.resume_through(&out, 2);
        assert!(
            matches!(
                bootstrap.and_then(|bootstrap| bootstrap.of),
                Some(wire::bootstrap::Of::Prompt(prompt)) if prompt.input_id == b"go on"
            ),
            "the input is the first"
        );
        let out = rig.step(Input::AgentHello { agent: PARENT });
        let row = answer_of(&out, call).expect("answered once it is up");
        assert_eq!(
            (row.lifecycle(), row.incarnation),
            (Lifecycle::Live, 2),
            "{out:?}"
        );

        let (call, out) = rig.call(wake(CHILD, "go on"));
        assert_eq!(refusal_of(&out, call), Some(Refusal::Live), "{out:?}");
        rig.call(ClientRequest::KillAgent { agent: CHILD });
        rig.step(Input::ControlClosed {
            agent: CHILD,
            ended: Ended::Exited,
        });
        let (call, out) = rig.call(wake(CHILD, "go on"));
        assert_eq!(
            refusal_of(&out, call),
            Some(Refusal::NotSuspended),
            "{out:?}"
        );
        assert_eq!(job_of(&out), None, "{out:?}");
    }

    /// A wake that meets an agent on its way out waits for the exit, then
    /// resumes it only if the exit left it suspended: typed as it was
    /// suspending it starts again, typed as a stop was under way it is
    /// refused at once.
    #[test]
    fn a_wake_waiting_out_an_exit_resumes_a_suspension_and_not_a_stop() {
        let mut rig = rig();
        rig.live(PARENT, "lead", None);
        rig.step(Input::Exiting { agent: PARENT });
        let (call, out) = rig.call(wake(PARENT, "go on"));
        assert_eq!(job_of(&out), None, "it waits for the exit: {out:?}");
        let gone = rig.suspend(PARENT);
        rig.resume_through(&gone, 2);
        let out = rig.step(Input::AgentHello { agent: PARENT });
        assert_eq!(answer_of(&out, call).map(|row| row.incarnation), Some(2));

        rig.live(CHILD, "scout", None);
        let (stop, _) = rig.call(ClientRequest::KillAgent { agent: CHILD });
        let (call, out) = rig.call(wake(CHILD, "go on"));
        assert_eq!(
            refusal_of(&out, call),
            Some(Refusal::Ending),
            "refused, not waiting behind the stop: {out:?}"
        );
        let gone = rig.suspend(CHILD);
        assert_eq!(
            answer_of(&gone, stop).map(|row| row.exit_cause()),
            Some(ExitCause::Stopped)
        );
        assert_eq!(job_of(&gone), None, "nothing starts: {gone:?}");

        // Its process ending some other way leaves nothing to wake either,
        // though only the relaunch can tell.
        rig.live(OTHER, "other", None);
        rig.step(Input::Exiting { agent: OTHER });
        let (call, _) = rig.call(wake(OTHER, "go on"));
        let gone = rig.step(Input::ControlClosed {
            agent: OTHER,
            ended: Ended::Exited,
        });
        let Some((unlock, Job::AwaitUnlocked { .. })) = job_of(&gone) else {
            panic!("the wake waits for the directory: {gone:?}");
        };
        let unlocked = rig.step(Input::JobDone {
            job: unlock,
            result: JobResult::Done,
        });
        let Some((spec, Job::NextSpec { .. })) = job_of(&unlocked) else {
            panic!("the spec is asked for: {unlocked:?}");
        };
        let out = rig.step(Input::JobDone {
            job: spec,
            result: JobResult::Spec(Some(Box::new(AgentSpec {
                agent_id: OTHER.0.to_vec(),
                kind: wire::Kind::Codex as i32,
                incarnation: 2,
                ..AgentSpec::default()
            }))),
        });
        assert_eq!(
            refusal_of(&out, call),
            Some(Refusal::NotSuspended),
            "{out:?}"
        );
        assert_eq!(rig.row(OTHER).unwrap().lifecycle, Lifecycle::Exited as i32);
    }

    /// The journal's last step puts the suspension on the row while the
    /// process is still finishing, so a person sees it suspended and types
    /// before it is gone, and before it ever refused anything as exiting.
    /// The wake waits for the exit and then resumes it. Refused as live,
    /// the input went to a process that no longer read it, and its sender
    /// heard the answer was lost.
    #[test]
    fn a_wake_after_the_journal_said_it_suspends_waits_for_the_process() {
        let mut rig = rig();
        rig.live(PARENT, "lead", None);
        let mut row = rig.row(PARENT).unwrap();
        row.exit_cause = ExitCause::Suspended as i32;
        rig.daemon.store.put_agent(&row).unwrap();
        let (call, out) = rig.call(wake(PARENT, "go on"));
        assert_eq!(refusal_of(&out, call), None, "it waits: {out:?}");
        assert_eq!(job_of(&out), None, "it waits for the exit: {out:?}");
        let gone = rig.step(Input::ControlClosed {
            agent: PARENT,
            ended: Ended::Exited,
        });
        rig.resume_through(&gone, 2);
        let out = rig.step(Input::AgentHello { agent: PARENT });
        assert_eq!(answer_of(&out, call).map(|row| row.incarnation), Some(2));
    }

    /// A wake for an agent the daemon is stopping, or about to, is refused
    /// at once instead of waiting out the stop's deadline behind it.
    #[test]
    fn a_wake_for_an_agent_the_daemon_is_ending_is_refused_at_once() {
        let mut rig = rig();
        rig.live(PARENT, "lead", None);
        rig.call(ClientRequest::KillAgent { agent: PARENT });
        let (call, out) = rig.call(wake(PARENT, "go on"));
        assert_eq!(refusal_of(&out, call), Some(Refusal::Ending), "{out:?}");

        rig.live(CHILD, "scout", None);
        rig.step(Input::Exiting { agent: CHILD });
        let (first, _) = rig.call(wake(CHILD, "go on"));
        rig.call(ClientRequest::SetFiling {
            agent: CHILD,
            filing: Filing::Closed,
        });
        let (call, out) = rig.call(wake(CHILD, "and on"));
        assert_eq!(
            refusal_of(&out, call),
            Some(Refusal::Ending),
            "a close waits its turn: {out:?}"
        );
        assert_eq!(refusal_of(&out, first), None, "the first wake still waits");
    }

    /// Switched off or on, the start's sweep finds a suspended agent's
    /// directory unlocked, as it is, and leaves its row as it was.
    #[test]
    fn the_sweep_leaves_a_suspended_row_alone_with_the_experiment_on_or_off() {
        for mut rig in [suspension_rig(true), {
            let mut rig = rig();
            rig.daemon.families.swept = false;
            rig
        }] {
            rig.left(PARENT, wire::Phase::Idle, ExitCause::Suspended);
            rig.step(Input::ControlClosed {
                agent: PARENT,
                ended: Ended::Away,
            });
            rig.step(Input::Swept);
            let row = rig.row(PARENT).unwrap();
            assert_eq!(
                (row.lifecycle, row.exit_cause),
                (Lifecycle::Live as i32, ExitCause::Suspended as i32)
            );
            // And a stop still marks it exited.
            let (call, out) = rig.call(ClientRequest::KillAgent { agent: PARENT });
            assert_eq!(
                answer_of(&out, call).map(|agent| agent.exit_cause()),
                Some(ExitCause::Stopped)
            );
        }
    }

    #[test]
    fn after_a_reboot_an_idle_agent_is_suspended_one_in_a_turn_continued_and_one_ended_exited() {
        const BUSY: AgentId = AgentId(*b"busy-00000000000");
        const ASKING: AgentId = AgentId(*b"asking-000000000");
        const DONE: AgentId = AgentId(*b"done-00000000000");
        let mut rig = suspension_rig(true);
        rig.left(PARENT, wire::Phase::Idle, ExitCause::Unspecified);
        rig.left(BUSY, wire::Phase::Working, ExitCause::Unspecified);
        rig.left(ASKING, wire::Phase::NeedsYou, ExitCause::Unspecified);
        rig.left(DONE, wire::Phase::Idle, ExitCause::Finished);
        let mut resumes = Vec::new();
        for agent in [PARENT, BUSY, ASKING, DONE] {
            let out = rig.step(Input::ControlClosed {
                agent,
                ended: Ended::Away,
            });
            if job_of(&out).is_some() {
                resumes.push((agent, out));
            }
        }
        rig.step(Input::Swept);

        let row = rig.row(PARENT).unwrap();
        assert_eq!(
            (row.lifecycle, row.exit_cause),
            (Lifecycle::Live as i32, ExitCause::Suspended as i32),
            "idle: suspended"
        );
        assert_eq!(rig.row(DONE).unwrap().lifecycle, Lifecycle::Exited as i32);
        assert_eq!(
            resumes.iter().map(|(agent, _)| *agent).collect::<Vec<_>>(),
            [BUSY, ASKING],
            "in a turn: continued at once"
        );
        for (agent, out) in resumes {
            let bootstrap = rig.resume_through(&out, 2).expect("resumed with a prompt");
            let Some(wire::bootstrap::Of::Prompt(prompt)) = bootstrap.of else {
                panic!("a prompt: {bootstrap:?}");
            };
            assert!(prompt.continued);
            assert_eq!(prompt.prompt.unwrap().text, CONTINUE_AFTER_RESTART);
            let row = rig.row(agent).unwrap();
            assert_eq!(
                (row.lifecycle, row.incarnation),
                (Lifecycle::Live as i32, 2)
            );
            assert_eq!(
                rig.daemon.store.run_start(&agent.0).unwrap(),
                Some(1),
                "the continue keeps the run"
            );
        }
    }

    /// An ACP agent ends for good with the machine unless its newest
    /// snapshot says its program offered to reopen the conversation: one
    /// whose program offered neither, and one whose answer was never seen,
    /// are exited as ended while away, idle or in a turn, neither suspended
    /// nor continued; one whose program resumes is suspended as Codex is.
    #[test]
    fn after_a_reboot_an_acp_agent_reopens_only_where_its_program_said_it_can() {
        const NEITHER: AgentId = AgentId(*b"neither-00000000");
        const UNSEEN: AgentId = AgentId(*b"unseen-000000000");
        const RESUMES: AgentId = AgentId(*b"resumes-00000000");
        let mut rig = suspension_rig(true);
        rig.left(PARENT, wire::Phase::Idle, ExitCause::Unspecified);
        for (agent, phase, resumable) in [
            (NEITHER, wire::Phase::Idle, Some(false)),
            (UNSEEN, wire::Phase::Working, None),
            (RESUMES, wire::Phase::Idle, Some(true)),
        ] {
            let mut row = AgentRow::new(rig.daemon.own_key(agent), wire::Kind::Acp, "/src");
            row.incarnation = 1;
            row.status.phase = phase as i32;
            row.snapshot = Some(wire::Snapshot {
                kind: wire::Kind::Acp as i32,
                status: wire::AgentStatus::in_phase(phase),
                conversation: Some("s".into()),
                capabilities: resumable
                    .map(|resumable| wire::AgentCapabilities::acp(resumable, false, false))
                    .map(Box::new),
                ..Default::default()
            });
            rig.daemon.store.put_agent(&row).unwrap();
        }
        for agent in [PARENT, NEITHER, UNSEEN, RESUMES] {
            let out = rig.step(Input::ControlClosed {
                agent,
                ended: Ended::Away,
            });
            assert!(job_of(&out).is_none(), "nothing resumes {agent:?}: {out:?}");
        }
        rig.step(Input::Swept);
        for agent in [PARENT, RESUMES] {
            let row = rig.row(agent).unwrap();
            assert_eq!(
                (row.lifecycle, row.exit_cause),
                (Lifecycle::Live as i32, ExitCause::Suspended as i32),
                "{agent:?} reopens, so it is suspended"
            );
        }
        for agent in [NEITHER, UNSEEN] {
            let row = rig.row(agent).unwrap();
            assert_eq!(
                (row.lifecycle, row.exit_cause),
                (Lifecycle::Exited as i32, ExitCause::EndedWhileAway as i32),
                "{agent:?}"
            );
        }
    }

    /// A resume whose next spec the driver refuses, as it refuses one for
    /// an agent that cannot reopen its conversation, answers the caller in
    /// the driver's own words and leaves the row exited as it was.
    #[test]
    fn a_resume_whose_spec_is_refused_says_why_in_the_drivers_words() {
        let mut rig = suspension_rig(false);
        rig.left(PARENT, wire::Phase::Idle, ExitCause::ProviderExited);
        let mut row = rig.row(PARENT).unwrap();
        row.lifecycle = Lifecycle::Exited as i32;
        rig.daemon.store.put_agent(&row).unwrap();
        let (call, out) = rig.call(ClientRequest::ResumeAgent(Box::new(Resume {
            agent: PARENT,
            bootstrap: None,
            if_suspended: false,
        })));
        let Some((unlock, Job::AwaitUnlocked { .. })) = job_of(&out) else {
            panic!("the resume waits for the directory: {out:?}");
        };
        let unlocked = rig.step(Input::JobDone {
            job: unlock,
            result: JobResult::Done,
        });
        let Some((spec, Job::NextSpec { .. })) = job_of(&unlocked) else {
            panic!("the spec is asked for: {unlocked:?}");
        };
        let refused = wire::Error {
            code: wire::ErrorCode::FailedPrecondition as i32,
            message: "it cannot reopen its conversation".into(),
            details: Vec::new(),
        };
        let out = rig.step(Input::JobDone {
            job: spec,
            result: JobResult::Refused(refused.clone()),
        });
        assert_eq!(refusal_of(&out, call), Some(Refusal::Spec(refused)));
        assert_eq!(rig.row(PARENT).unwrap().lifecycle, Lifecycle::Exited as i32);
    }

    /// A stop the person asks for while the start is still reading the
    /// journal of an agent the restart cut off in its turn stops it: the
    /// continuation that follows the read is a wake, which the stopped row
    /// does not take, so the agent is not started again.
    #[test]
    fn a_stop_while_a_restart_reads_the_journal_is_not_undone_by_the_continuation() {
        let mut rig = suspension_rig(true);
        rig.left(PARENT, wire::Phase::Working, ExitCause::Unspecified);
        let mut journal =
            journal::synthetic::SyntheticWriter::open(rig.daemon.journal_dir(PARENT), 1 << 30)
                .expect("the journal opens");
        for n in 0..crate::ingest::INGEST_BATCH + 10 {
            journal
                .append(&wire::Step {
                    items: vec![wire::Item {
                        key: format!("k{n:03}"),
                        ..wire::Item::default()
                    }],
                    ..wire::Step::default()
                })
                .expect("a frame is written");
        }
        let gone = rig.step(Input::ControlClosed {
            agent: PARENT,
            ended: Ended::Away,
        });
        assert!(
            gone.contains(&Output::ArmTimer {
                id: TimerId::Ingest(PARENT),
                at_ms: rig.daemon.timers.now_ms,
            }),
            "the journal takes more than one batch: {gone:?}"
        );
        rig.step(Input::Swept);
        let (stop, _) = rig.call(ClientRequest::KillAgent { agent: PARENT });
        let read = rig.step(Input::Timer {
            id: TimerId::Ingest(PARENT),
        });
        let row = rig.row(PARENT).unwrap();
        assert_eq!(
            (row.lifecycle, row.exit_cause),
            (Lifecycle::Exited as i32, ExitCause::Stopped as i32),
            "{read:?}"
        );
        assert!(
            answer_of(&read, stop).is_some(),
            "the stop is answered: {read:?}"
        );
        assert!(
            !read.iter().any(|output| matches!(
                output,
                Output::Job {
                    op: Job::AwaitUnlocked { .. },
                    ..
                }
            )),
            "nothing is started again: {read:?}"
        );
    }

    #[test]
    fn without_a_reboot_or_the_experiment_an_agent_found_gone_ended_while_away() {
        let mut quiet = suspension_rig(false);
        let mut off = rig();
        off.daemon.families.swept = false;
        off.daemon.options.rebooted = true;
        for rig in [&mut quiet, &mut off] {
            rig.left(PARENT, wire::Phase::Working, ExitCause::Unspecified);
            let out = rig.step(Input::ControlClosed {
                agent: PARENT,
                ended: Ended::Away,
            });
            assert!(job_of(&out).is_none(), "nothing resumed: {out:?}");
            let row = rig.row(PARENT).unwrap();
            assert_eq!(
                (row.lifecycle, row.exit_cause),
                (Lifecycle::Exited as i32, ExitCause::EndedWhileAway as i32)
            );
        }
    }

    #[test]
    fn an_end_its_journal_gives_no_cause_for_is_recorded_after_a_grace() {
        let mut rig = suspension_rig(false);
        rig.step(Input::Swept);
        rig.live(PARENT, "lead", None);
        rig.live(CHILD, "scout", Some(PARENT));
        let out = rig.step(Input::ControlClosed {
            agent: PARENT,
            ended: Ended::Exited,
        });
        assert!(
            out.iter().any(|output| matches!(
                output,
                Output::ArmTimer { id: TimerId::Grace(PARENT), at_ms } if *at_ms == CAUSELESS_GRACE_MS
            )),
            "{out:?}"
        );
        assert_eq!(rig.row(PARENT).unwrap().lifecycle, Lifecycle::Live as i32);
        assert!(stops(&out, CHILD).is_empty());
        let out = rig.step(Input::Timer {
            id: TimerId::Grace(PARENT),
        });
        let row = rig.row(PARENT).unwrap();
        assert_eq!(
            (row.lifecycle, row.exit_cause),
            (
                Lifecycle::Exited as i32,
                ExitCause::EndedUnexpectedly as i32
            )
        );
        assert_eq!(stops(&out, CHILD), [false], "{out:?}");
    }

    /// What the journal leaves on the row when the provider ended first.
    fn provider_ended(
        rig: &mut Rig,
        agent: AgentId,
        phase: wire::Phase,
        cause: ExitCause,
        detail: Option<&str>,
    ) {
        let mut row = rig.row(agent).unwrap();
        row.exit_cause = cause as i32;
        row.exit_detail = detail.map(str::to_owned);
        rig.daemon.store.put_agent(&row).unwrap();
        rig.daemon.store.set_phase(&row.agent, phase, 0).unwrap();
    }

    /// A shutdown can end the provider before the agent process: the
    /// journal then says the provider was signalled (killed by the signal,
    /// or exiting 128 and its number, as headless Claude exits 143 on
    /// SIGTERM), or that it quit cleanly, as Codex's app server does on
    /// SIGTERM even mid-turn. Each gets the grace a causeless end gets and
    /// is recorded as it is once the grace is up. A provider that failed is
    /// no shutdown and is recorded at once, and so is terminal Claude
    /// quitting in a turn, which a person did in its terminal.
    #[test]
    fn a_provider_signalled_or_quitting_first_gets_the_grace_and_one_that_failed_does_not() {
        let idle = wire::Phase::Idle;
        let starting = wire::Phase::Starting;
        let (codex, pty) = (wire::Kind::Codex, wire::Kind::ClaudePty);
        let signalled = Some(wire::PROVIDER_SIGNALLED);
        let exited = ExitCause::ProviderExited;
        for (kind, phase, cause, detail, grace) in [
            (codex, idle, exited, signalled, true),
            (codex, idle, exited, Some("exit code 143"), true),
            (codex, idle, exited, Some("exit code 129"), true),
            (codex, idle, exited, Some("exit code 130"), true),
            (codex, starting, exited, Some("exit code 137"), true),
            (codex, idle, ExitCause::Quit, None, true),
            (codex, starting, ExitCause::Quit, None, true),
            (pty, idle, ExitCause::Quit, None, true),
            (pty, starting, ExitCause::Quit, None, false),
            (codex, idle, exited, Some("exit code 1"), false),
            (codex, idle, exited, Some("exit code 134"), false),
        ] {
            let mut rig = suspension_rig(false);
            rig.step(Input::Swept);
            rig.live(PARENT, "lead", None);
            rig.live(CHILD, "scout", Some(PARENT));
            let mut row = rig.row(PARENT).unwrap();
            row.kind = kind as i32;
            rig.daemon.store.put_agent(&row).unwrap();
            provider_ended(&mut rig, PARENT, phase, cause, detail);
            let out = rig.step(Input::ControlClosed {
                agent: PARENT,
                ended: Ended::Exited,
            });
            let armed = out.iter().any(|output| {
                matches!(
                    output,
                    Output::ArmTimer {
                        id: TimerId::Grace(PARENT),
                        ..
                    }
                )
            });
            assert_eq!(
                armed, grace,
                "{kind:?} {phase:?} {cause:?} {detail:?}: {out:?}"
            );
            if grace {
                assert_eq!(rig.row(PARENT).unwrap().lifecycle, Lifecycle::Live as i32);
                assert!(stops(&out, CHILD).is_empty(), "{out:?}");
                rig.step(Input::Timer {
                    id: TimerId::Grace(PARENT),
                });
            }
            let row = rig.row(PARENT).unwrap();
            assert_eq!(
                (row.lifecycle, row.exit_cause, row.exit_detail.as_deref()),
                (Lifecycle::Exited as i32, cause as i32, detail),
                "recorded as it is once the grace is up"
            );
        }
    }

    /// After a reboot, an agent whose provider the shutdown ended first is
    /// treated as one whose journal gave no cause: idle, suspended; cut off
    /// in its turn, continued. That holds for headless Claude, which exits
    /// 143 on SIGTERM, and for Codex, whose app server exits cleanly on
    /// SIGTERM even mid-turn. A clean quit counts only because the machine
    /// restarted: without a reboot it is a quit. Terminal Claude quitting in
    /// a turn is the person's quit either way and is never continued. A
    /// provider that failed stays failed either way.
    #[test]
    fn after_a_reboot_a_provider_ended_first_is_suspended_or_continued() {
        const BUSY: AgentId = AgentId(*b"busy-00000000000");
        const QUIT: AgentId = AgentId(*b"quit-00000000000");
        const QUIT_BUSY: AgentId = AgentId(*b"quit-busy-000000");
        const CUT_OFF: AgentId = AgentId(*b"cut-off-00000000");
        const TERMED: AgentId = AgentId(*b"termed-000000000");
        const FAILED: AgentId = AgentId(*b"failed-000000000");
        let signalled = Some(wire::PROVIDER_SIGNALLED.to_owned());
        for rebooted in [true, false] {
            let mut rig = suspension_rig(rebooted);
            rig.left(PARENT, wire::Phase::Idle, ExitCause::ProviderExited);
            rig.left(BUSY, wire::Phase::Starting, ExitCause::ProviderExited);
            rig.left(QUIT, wire::Phase::Idle, ExitCause::Quit);
            rig.left(QUIT_BUSY, wire::Phase::Starting, ExitCause::Quit);
            // Quit in a turn: in terminal Claude a person did it, while
            // Codex's app server exits cleanly on SIGTERM mid-turn.
            let mut row = rig.row(QUIT_BUSY).unwrap();
            row.kind = wire::Kind::ClaudePty as i32;
            rig.daemon.store.put_agent(&row).unwrap();
            rig.left(CUT_OFF, wire::Phase::Starting, ExitCause::Quit);
            rig.left(TERMED, wire::Phase::Idle, ExitCause::ProviderExited);
            rig.left(FAILED, wire::Phase::Idle, ExitCause::ProviderExited);
            for (agent, detail) in [
                (PARENT, signalled.clone()),
                (BUSY, signalled.clone()),
                (TERMED, Some("exit code 143".to_owned())),
                (FAILED, Some("exit code 1".to_owned())),
            ] {
                let mut row = rig.row(agent).unwrap();
                row.exit_detail = detail;
                rig.daemon.store.put_agent(&row).unwrap();
            }
            let mut resumed = Vec::new();
            for agent in [PARENT, BUSY, QUIT, QUIT_BUSY, CUT_OFF, TERMED, FAILED] {
                let out = rig.step(Input::ControlClosed {
                    agent,
                    ended: Ended::Away,
                });
                if job_of(&out).is_some() {
                    resumed.push(agent);
                }
            }
            rig.step(Input::Swept);
            let read = |rig: &Rig, agent| {
                let row = rig.row(agent).unwrap();
                (row.lifecycle, row.exit_cause)
            };
            let live = Lifecycle::Live as i32;
            let exited = Lifecycle::Exited as i32;
            if rebooted {
                for agent in [PARENT, QUIT, TERMED] {
                    assert_eq!(read(&rig, agent), (live, ExitCause::Suspended as i32));
                }
                assert_eq!(resumed, [BUSY, CUT_OFF], "cut off in its turn: continued");
            } else {
                for agent in [QUIT, CUT_OFF] {
                    assert_eq!(read(&rig, agent), (exited, ExitCause::Quit as i32));
                }
                for agent in [PARENT, TERMED] {
                    assert_eq!(
                        read(&rig, agent),
                        (exited, ExitCause::ProviderExited as i32)
                    );
                }
                assert!(resumed.is_empty(), "{resumed:?}");
            }
            assert_eq!(
                read(&rig, QUIT_BUSY),
                (exited, ExitCause::Quit as i32),
                "the person quit in a turn: never continued"
            );
            assert_eq!(
                read(&rig, FAILED),
                (exited, ExitCause::ProviderExited as i32)
            );
        }
    }

    /// A provider subagent runs inside its host's provider process, so it
    /// ends with that process however the host's row is left: a reboot
    /// that kills the host before its journal could say so leaves no
    /// subagent live under a row with no process. One still running is
    /// the host's work under way, so an idle host with one is continued
    /// as one cut off in its turn; an idle host whose subagents had ended
    /// is suspended, with its ended subagents as they were.
    #[test]
    fn after_a_reboot_a_hosts_live_provider_subagents_end_and_it_is_continued() {
        const SUB: AgentId = AgentId(*b"sub-000000000000");
        const DONE_SUB: AgentId = AgentId(*b"done-sub-0000000");
        let mut rig = suspension_rig(true);
        rig.left(PARENT, wire::Phase::Idle, ExitCause::Unspecified);
        rig.left(OTHER, wire::Phase::Idle, ExitCause::Unspecified);
        let subagent = |rig: &Rig, agent: AgentId, host: AgentId, live: bool| {
            let mut row = AgentRow::new(rig.daemon.own_key(agent), wire::Kind::Codex, "/src");
            row.parent = Some(rig.daemon.own_key(host));
            row.native = Some(wire::NativeChild {
                provider_child_id: format!("thread-{}", agent.0[0]),
            });
            row.status.phase = wire::Phase::Working as i32;
            if !live {
                row.lifecycle = Lifecycle::Exited as i32;
                row.exit_cause = ExitCause::Finished as i32;
            }
            row
        };
        let rows = [
            subagent(&rig, SUB, PARENT, true),
            subagent(&rig, DONE_SUB, OTHER, false),
        ];
        rig.daemon.store.put_agents(&rows).unwrap();
        let mut resumed = Vec::new();
        for agent in [PARENT, OTHER] {
            let out = rig.step(Input::ControlClosed {
                agent,
                ended: Ended::Away,
            });
            if job_of(&out).is_some() {
                resumed.push(agent);
            }
        }
        rig.step(Input::Swept);

        assert_eq!(resumed, [PARENT], "its subagent's work was cut off");
        let sub = rig.row(SUB).unwrap();
        assert_eq!(
            (sub.lifecycle, sub.exit_cause),
            (Lifecycle::Exited as i32, ExitCause::EndedWhileAway as i32),
            "the subagent ended with its host's process"
        );
        let other = rig.row(OTHER).unwrap();
        assert_eq!(
            (other.lifecycle, other.exit_cause),
            (Lifecycle::Live as i32, ExitCause::Suspended as i32)
        );
        let done = rig.row(DONE_SUB).unwrap();
        assert_eq!(
            done.exit_cause,
            ExitCause::Finished as i32,
            "left as it was"
        );
    }

    #[test]
    fn a_daemon_asked_to_stop_records_no_more_exits() {
        let mut rig = suspension_rig(false);
        rig.step(Input::Swept);
        rig.live(PARENT, "lead", None);
        rig.live(CHILD, "scout", Some(PARENT));
        rig.step(Input::ControlClosed {
            agent: PARENT,
            ended: Ended::Exited,
        });
        rig.step(Input::Stopping);
        let out = rig.step(Input::Timer {
            id: TimerId::Grace(PARENT),
        });
        let out_too = rig.step(Input::ControlClosed {
            agent: CHILD,
            ended: Ended::Exited,
        });
        for agent in [PARENT, CHILD] {
            assert_eq!(rig.row(agent).unwrap().lifecycle, Lifecycle::Live as i32);
        }
        assert!(stops(&out, CHILD).is_empty() && stops(&out_too, CHILD).is_empty());
    }

    #[test]
    fn a_resume_after_a_suspension_keeps_the_run_and_any_other_starts_a_new_one() {
        let mut rig = suspension_rig(false);
        rig.step(Input::Swept);
        rig.live(PARENT, "lead", None);
        rig.suspend(PARENT);
        let (_, out) = rig.call(ClientRequest::ResumeAgent(Box::new(Resume {
            agent: PARENT,
            bootstrap: None,
            if_suspended: false,
        })));
        rig.resume_through(&out, 2);
        assert_eq!(rig.daemon.store.run_start(&PARENT.0).unwrap(), Some(1));
        rig.step(Input::AgentHello { agent: PARENT });

        rig.call(ClientRequest::KillAgent { agent: PARENT });
        rig.step(Input::ControlClosed {
            agent: PARENT,
            ended: Ended::Exited,
        });
        assert_eq!(
            rig.row(PARENT).unwrap().exit_cause,
            ExitCause::Killed as i32
        );
        let (_, out) = rig.call(ClientRequest::ResumeAgent(Box::new(Resume {
            agent: PARENT,
            bootstrap: None,
            if_suspended: false,
        })));
        rig.resume_through(&out, 3);
        assert_eq!(rig.daemon.store.run_start(&PARENT.0).unwrap(), Some(3));
    }

    /// A restart caught a close after its filing was stored and before the
    /// suspension it met was finished: the row is live, closed and
    /// suspended. The start marks it exited as stopped, so nothing resumes
    /// it and undoes the close, with the experiment on or off.
    #[test]
    fn a_closed_row_left_suspended_is_marked_stopped_at_start() {
        for mut rig in [suspension_rig(true), suspension_rig(false), {
            let mut rig = rig();
            rig.daemon.families.swept = false;
            rig
        }] {
            rig.left(PARENT, wire::Phase::Idle, ExitCause::Suspended);
            let mut row = rig.row(PARENT).unwrap();
            row.filing = wire::Filing::Closed as i32;
            rig.daemon.store.put_agent(&row).unwrap();
            rig.step(Input::ControlClosed {
                agent: PARENT,
                ended: Ended::Away,
            });
            let row = rig.row(PARENT).unwrap();
            assert_eq!(
                (row.lifecycle, row.exit_cause),
                (Lifecycle::Exited as i32, ExitCause::Stopped as i32)
            );
            assert!(row.is_closed());
        }
    }
}
