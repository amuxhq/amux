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

use serde::{Deserialize, Serialize};
use store::{AgentKey, AgentRow, Delivery, Marker, StoreError};
use wire::{
    Agent, AgentRemoved, AgentSpec, CtlFrame, ExitCause, Lifecycle, RemovalReason, ctl_frame,
    inventory_event,
};

use crate::ids::{AgentId, CallId, JobId, TimerId};
use crate::io::{Job, Output, ParentSays};
use crate::payload::{CallError, JobResult, Response, Status};
use crate::serve::{inventory, to_wire};
use crate::{Daemon, codec};

/// How long an agent past its stop deadline has to act on a kill asked of
/// it over its control socket before its process is killed from outside.
pub const KILL_ASK_PATIENCE_MS: u64 = 2_000;
/// How long a stop waits after killing the process group before it gives up
/// on seeing the process end.
pub const KILL_PATIENCE_MS: u64 = 5_000;

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
                    | Doing::AwaitUnlock { job, .. }
                    | Doing::Removing { job }
                    | Doing::Ending {
                        phase: Phase::Killing { job: Some(job) },
                        ..
                    } => *job = ids.jobs.get(*job),
                    Doing::Starting | Doing::AwaitExit { .. } | Doing::Ending { .. } => {}
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

    /// Whether the agent's process has a connection that said Hello.
    pub(crate) fn connected(&self, agent: AgentId) -> bool {
        self.processes
            .get(&agent)
            .is_some_and(|process| process.hello && process.ended.is_none())
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
    /// `new_worktree`, the repository the worktree is made from.
    pub cwd: String,
    /// The name asked for, checked to be one an agent may have.
    pub given: Option<String>,
    /// What an unnamed agent's name must not be: the repository's branches.
    pub branches: BTreeSet<String>,
    /// Start in a new worktree of the repository at `cwd`, named by the
    /// agent's name.
    pub new_worktree: bool,
    /// What the incarnation starts with but for what the registry decides:
    /// its name, working directory, creation time, number and base branch.
    /// Its parent, when it has one, is the new agent's.
    #[serde(with = "codec::message")]
    pub spec: AgentSpec,
}

/// A resume, with the spec its next incarnation starts with, carried from
/// its newest: none when it has none.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resume {
    pub agent: AgentId,
    #[serde(with = "codec::optional")]
    pub spec: Option<AgentSpec>,
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
    /// Another agent on this host has the name.
    NameTaken(String),
    /// The parent has ended and starts no more agents.
    ParentEnded(AgentId),
    /// The parent lives on another host whose listing this host has not
    /// caught up with since it last connected, or whose row for the parent
    /// has not reached this host yet, so whether the parent has ended is
    /// not known; the same create succeeds once it is.
    ParentHostNotCaughtUp(crate::HostId),
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
    /// The agent's message lane: a parent's message resumes it.
    Lane,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum Op {
    Create(Box<Create>),
    Resume(Box<Resume>),
    Stop,
    Kill,
    Delete,
}

/// The operation under way on an agent, and whoever waits for its answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Operation {
    call: Caller,
    doing: Doing,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Doing {
    /// A create waits for its worktree, with the name it chose.
    Worktree {
        job: JobId,
        create: Box<Create>,
        name: String,
    },
    /// The process was started: waiting for its Hello or its end.
    Starting,
    /// A resume waits for the process on its way out to end.
    AwaitExit { resume: Box<Resume> },
    /// A resume waits for the directory's lock to be free.
    AwaitUnlock { job: JobId, resume: Box<Resume> },
    /// A stop or a kill is ending the process.
    Ending { phase: Phase, then: Then },
    /// A delete removed the rows; the directory goes.
    Removing { job: JobId },
}

impl Doing {
    /// The job the operation waits on, if it waits on one.
    fn job(&self) -> Option<JobId> {
        match self {
            Doing::Worktree { job, .. }
            | Doing::AwaitUnlock { job, .. }
            | Doing::Removing { job }
            | Doing::Ending {
                phase: Phase::Killing { job: Some(job) },
                ..
            } => Some(*job),
            Doing::Starting | Doing::AwaitExit { .. } | Doing::Ending { .. } => None,
        }
    }
}

/// How far ending a process has gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
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
enum Then {
    /// The caller hears the agent's row.
    Answer,
    /// The agent's rows and directory go.
    Delete,
}

/// What an operation came to, for its caller.
type Outcome = Result<Option<Agent>, Refusal>;

/// How beginning an operation went: it waits on something, or it is
/// already over.
enum Began {
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
        self.registry_call(Caller::Client(call), agent, Op::Resume(resume), out);
    }

    pub(crate) fn stop(&mut self, call: CallId, agent: AgentId, out: &mut Vec<Output>) {
        self.registry_call(Caller::Client(call), agent, Op::Stop, out);
    }

    pub(crate) fn kill(&mut self, call: CallId, agent: AgentId, out: &mut Vec<Output>) {
        self.registry_call(Caller::Client(call), agent, Op::Kill, out);
    }

    pub(crate) fn delete(&mut self, call: CallId, agent: AgentId, out: &mut Vec<Output>) {
        self.registry_call(Caller::Client(call), agent, Op::Delete, out);
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
            let began = match op {
                Op::Create(create) => self.begin_create(create, out),
                Op::Resume(resume) => self.begin_resume(resume, out),
                Op::Stop => self.begin_ending(agent, false, Then::Answer, out),
                Op::Kill => self.begin_ending(agent, true, Then::Answer, out),
                Op::Delete => self.begin_delete(agent, out),
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
    fn finish(&mut self, agent: AgentId, outcome: Outcome, out: &mut Vec<Output>) {
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
                {
                    tracing::warn!(agent = %crate::hex(&agent.0), ?refusal, "following its parent failed");
                }
                return;
            }
            Caller::Lane => return self.lane_resumed(agent, outcome.map(|_| ()), out),
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
    fn row_outcome(&self, agent: AgentId) -> Outcome {
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
        if let Some(parent) = create.spec.parent.as_ref() {
            let parent = AgentKey::new(parent.host_id.clone(), parent.agent_id.clone());
            // A child made under another host's parent before that host's
            // listing caught up would be judged by neither: the listing
            // judges only agents that predate it, and the live stream after
            // it says nothing of a parent that had already ended. A parent
            // missing from a caught-up host is deleted, or made so lately
            // its row is still on the way; a retry tells them apart.
            if parent.host != self.store.own_host() {
                let host = crate::HostId::from_slice(&parent.host);
                if !host.is_some_and(|host| self.sources.ready(host))
                    || self.store.agent(&parent)?.is_none()
                {
                    return Err(Refusal::ParentHostNotCaughtUp(
                        host.unwrap_or(crate::HostId([0; 16])),
                    ));
                }
            }
            if self.parent_ended(&parent)?.is_some() {
                let id = AgentId::from_slice(&parent.agent).unwrap_or(AgentId([0; 16]));
                return Err(Refusal::ParentEnded(id));
            }
        }
        // Names are unique on this host, counting exited agents, which stay
        // addressable by name: a given name already taken gets the next
        // free suffix, and the caller learns the name from the agent it
        // gets back.
        let names = self.own_names()?;
        let name = match &create.given {
            Some(name) => crate::names::free_suffix(name, &|candidate| names.contains(candidate)),
            None => crate::names::assign_name(u128::from_be_bytes(create.agent.0), &|candidate| {
                names.contains(candidate) || create.branches.contains(candidate)
            }),
        };
        let mut create = create;
        create.spec.created_at_ms = self.timers.now_ms as i64;
        if create.new_worktree {
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
        Ok(Began::Waiting(
            self.start_incarnation(create, name, cwd, None, out),
        ))
    }

    fn new_row(&self, create: &Create, name: &str, cwd: &str) -> AgentRow {
        let mut row = AgentRow::new(self.own_key(create.agent), create.spec.kind(), cwd);
        row.name = name.to_owned();
        row.parent = create
            .spec
            .parent
            .as_ref()
            .map(|parent| AgentKey::new(parent.host_id.clone(), parent.agent_id.clone()));
        row.created_at_ms = create.spec.created_at_ms;
        row.incarnation = 1;
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
        let row = self.new_row(&create, &name, &path);
        if let Err(error) = self.put_own_row(&row, out) {
            return self.finish(agent, Err(error.into()), out);
        }
        // The worktree took a while; a parent that ended meanwhile is
        // followed now, after this start, rather than leave the worktree
        // behind.
        if let Some(parent) = row.parent.as_ref()
            && let Ok(Some(says)) = self.parent_ended(parent)
        {
            self.follow(agent, says, out);
        }
        let doing = self.start_incarnation(create, name, path, Some(base_branch), out);
        if let Some(lane) = self.families.lanes.get_mut(&agent)
            && let Some(operation) = lane.current.as_mut()
        {
            operation.doing = doing;
        }
    }

    /// Asks the driver for the agent's first process.
    fn start_incarnation(
        &mut self,
        create: Box<Create>,
        name: String,
        cwd: String,
        base_branch: Option<String>,
        out: &mut Vec<Output>,
    ) -> Doing {
        let mut spec = create.spec;
        spec.name = name;
        spec.cwd = cwd;
        spec.incarnation = 1;
        spec.base_branch = base_branch;
        self.spawn_process(create.agent, spec, out)
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
        if self.store.agent(&self.own_key(agent))?.is_none() {
            return Err(Refusal::NotFound);
        }
        if let Some(process) = self.families.processes.get(&agent) {
            let leaving = process.leaving || process.stopping.is_some() || process.ended.is_some();
            if !leaving {
                return Err(Refusal::Live);
            }
            self.arm_after(TimerId::Exit(agent), self.settings.stop_deadline_ms, out);
            return Ok(Began::Waiting(Doing::AwaitExit { resume }));
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

    /// The directory is free: the row says live under the next incarnation
    /// and its process starts.
    fn resume_unlocked(
        &mut self,
        resume: Box<Resume>,
        out: &mut Vec<Output>,
    ) -> Result<Doing, Refusal> {
        let key = self.own_key(resume.agent);
        // Looked up again: retention may have removed an exited agent while
        // the resume waited.
        let mut row = self.store.agent(&key)?.ok_or(Refusal::NotFound)?;
        let mut spec = resume.spec.ok_or(Refusal::NoSpec)?;
        // The name it has now: a rename while it was not running reaches
        // its Codex thread this way.
        spec.name = row.name.clone();
        spec.created_at_ms = self.timers.now_ms as i64;
        row.lifecycle = Lifecycle::Live as i32;
        row.exit_cause = ExitCause::Unspecified as i32;
        row.exit_detail = None;
        row.incarnation = spec.incarnation;
        self.put_own_row(&row, out)?;
        // The next CaughtUp comes from this incarnation's journal.
        self.store.set_marker(&key, None);
        Ok(self.spawn_process(resume.agent, spec, out))
    }

    fn spawn_process(&mut self, agent: AgentId, spec: AgentSpec, out: &mut Vec<Output>) -> Doing {
        self.families.processes.insert(agent, Process::default());
        out.push(Output::SpawnAgent {
            dir: self.agent_dir(agent),
            spec: Box::new(spec),
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
    /// killed from outside. An agent with no process is left as it is.
    fn begin_ending(
        &mut self,
        agent: AgentId,
        kill: bool,
        then: Then,
        out: &mut Vec<Output>,
    ) -> Result<Began, Refusal> {
        if !self.families.processes.contains_key(&agent) {
            return match then {
                Then::Answer => Ok(Began::Finished(Box::new(self.row_outcome(agent)))),
                Then::Delete => self.remove_agent(agent, out).map(Began::Waiting),
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
    fn begin_delete(&mut self, agent: AgentId, out: &mut Vec<Output>) -> Result<Began, Refusal> {
        let key = self.own_key(agent);
        if self.store.agent(&key)?.is_none() {
            return Err(Refusal::NotFound);
        }
        self.answers.mute(&key);
        self.begin_ending(agent, false, Then::Delete, out)
    }

    /// Removes a stopped agent's rows, then its directory. Its children, on
    /// this host or another, follow by the family rule once their host
    /// sees it gone.
    fn remove_agent(&mut self, agent: AgentId, out: &mut Vec<Output>) -> Result<Doing, Refusal> {
        let key = self.own_key(agent);
        let removed = (|| {
            let parent = self.store.agent(&key)?.and_then(|row| row.parent);
            // Rows first: a crash between the two leaves a directory with
            // no row, which the next sweep removes.
            self.store.delete_agent(&key)?;
            let removed = AgentRemoved {
                host_id: key.host.clone(),
                agent_id: key.agent.clone(),
                reason: RemovalReason::Deleted as i32,
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
        let job = self.pending.mint();
        out.push(Output::Job {
            id: job,
            op: Job::RemoveDir {
                path: self.agent_dir(agent),
            },
        });
        Ok(Doing::Removing { job })
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
            self.put_own_row(&row, out)?;
            if row.kind == wire::Kind::Codex as i32
                && self
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
    /// answered.
    pub(crate) fn process_hello(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        let children = self.children_of(agent).unwrap_or_default();
        let process = self.families.processes.entry(agent).or_default();
        process.hello = true;
        process.told = children;
        if !process.told.is_empty() {
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
        let process = self.families.processes.remove(&agent).unwrap_or_default();
        // Unless the journal, now read to its end, said why.
        let (cause, detail, if_live) = match &ended {
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
            Ended::NoDirectory => (
                ExitCause::DirectoryFailed,
                Some("it is gone".to_owned()),
                true,
            ),
        };
        let fallback = wire::IncarnationEnd {
            cause: cause as i32,
            detail,
        };
        if let Err(error) = self.mark_exited(agent, fallback, if_live, out) {
            tracing::warn!(agent = %crate::hex(&agent.0), %error, "marking an agent exited failed");
        }
        out.push(Output::ForgetAgent { agent });
        self.lane_exited(agent, out);
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
                        let outcome = self.row_outcome(agent);
                        self.finish(agent, outcome, out);
                    }
                    Then::Delete => match self.remove_agent(agent, out) {
                        Ok(doing) => self.set_doing(agent, doing),
                        Err(refusal) => self.finish(agent, Err(refusal), out),
                    },
                }
            }
            Some(Doing::Worktree { .. } | Doing::AwaitUnlock { .. } | Doing::Removing { .. })
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
                _ => {}
            },
            _ => {}
        }
    }

    fn stop_timed_out(&mut self, agent: AgentId, then: Then, out: &mut Vec<Output>) {
        if then == Then::Delete {
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
                | Doing::AwaitUnlock { job: waiting, .. }
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
            Doing::AwaitUnlock { resume, .. } => match result {
                JobResult::Done => match self.resume_unlocked(resume, out) {
                    Ok(doing) => self.set_doing(agent, doing),
                    Err(refusal) => self.finish(agent, Err(refusal), out),
                },
                _ => self.finish(agent, Err(Refusal::StillLocked), out),
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
                        self.forget_draft(&agent.0, out);
                        Ok(None)
                    }
                    other => Err(Refusal::Directory(format!("{other:?}"))),
                };
                self.finish(agent, outcome, out);
            }
            Doing::Starting | Doing::AwaitExit { .. } => {}
        }
        true
    }

    /// The start's sweep has reported every agent it found: the children
    /// waiting to follow their parents do.
    pub(crate) fn swept(&mut self, out: &mut Vec<Output>) {
        self.families.swept = true;
        for (child, says) in std::mem::take(&mut self.families.held) {
            self.follow(child, says, out);
        }
    }

    fn doing(&self, agent: AgentId) -> Option<&Doing> {
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
    /// `says`, follows it: exited stops each live child, deleted deletes
    /// each child. A parent that is merely out of reach says neither, so
    /// its children carry on and their reports wait in the outbox.
    pub(crate) fn parent_changed(
        &mut self,
        parent: &AgentKey,
        says: ParentSays,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        for child in self.store.agents_of_parent(parent)? {
            if child.agent.host != self.store.own_host()
                || (says == ParentSays::Exited && child.lifecycle != Lifecycle::Live as i32)
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
            ParentSays::Deleted => Op::Delete,
        };
        self.registry_call(Caller::Family, child, op, out);
    }

    /// Whether `parent` has already ended as this host sees it: exited by
    /// its row, or deleted. Another host's row counts only once that host's
    /// listing has caught up, since a row kept from before may be stale;
    /// and a missing row means deleted only for this host's own agents,
    /// since another host's row may simply not have arrived yet.
    fn parent_ended(&self, parent: &AgentKey) -> Result<Option<ParentSays>, StoreError> {
        let own = parent.host == self.store.own_host();
        if !own
            && !crate::HostId::from_slice(&parent.host).is_some_and(|host| self.sources.ready(host))
        {
            return Ok(None);
        }
        Ok(match self.store.agent(parent)? {
            Some(row) if row.lifecycle == Lifecycle::Exited as i32 => Some(ParentSays::Exited),
            None if own => Some(ParentSays::Deleted),
            _ => None,
        })
    }

    /// The names this host's own agents hold, exited ones included.
    fn own_names(&self) -> Result<BTreeSet<String>, StoreError> {
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
    fn put_own_row(&mut self, row: &AgentRow, out: &mut Vec<Output>) -> Result<(), StoreError> {
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
        }
        if self.store.markers().get(&key) == Some(&Marker::CaughtUp) {
            // A subscriber that joined since read it at its cut, and one
            // that joined before was told then.
            return Ok(());
        }
        self.announce_caught_up(&key, out)
    }

    /// Marks the row exited, then settles what the process leaves: its
    /// unsent notifications go, since no push may name an agent that is
    /// gone, and a child whose incarnation ended without sending its last
    /// report tells its parent it failed: its turn was open, or it ended
    /// idle, as a sub-agent stopped while it waits for its own child does.
    /// A turn end marked last already told it.
    ///
    /// The row keeps the cause its journal's last step put there at commit;
    /// `fallback` is the daemon's own, for an incarnation whose journal
    /// stops without saying why.
    fn record_exit(
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
        let cause = wire::IncarnationEnd {
            cause: row.exit_cause,
            detail: row.exit_detail.clone(),
        };
        self.put_own_row(&row, out)?;
        self.parent_changed(&row.agent, ParentSays::Exited, out)?;
        self.store.remove_notifications(&row.agent.agent)?;
        let Some(parent) = &row.parent else {
            return Ok(());
        };
        // As committed: the phase at exit says nothing here, since a
        // provider's exit leaves it starting whether or not the turn ended.
        let held = self.store.agent(&row.agent)?.unwrap_or(row.clone());
        if !held.owes_report {
            return Ok(());
        }
        // Zero when the parent's row is not held: stamped by the drain.
        let parent_incarnation = self
            .store
            .agent(parent)?
            .map_or(0, |parent| parent.incarnation);
        self.store.put_delivery(&Delivery {
            child_id: row.agent.agent.clone(),
            incarnation: row.incarnation,
            turn_id: 0,
            parent: parent.clone(),
            parent_incarnation,
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
    /// nothing changed.
    fn children_of(&self, agent: AgentId) -> Result<Vec<(Vec<u8>, u32)>, StoreError> {
        let mut children: Vec<(Vec<u8>, u32)> = self
            .store
            .agents_of_parent(&self.own_key(agent))?
            .into_iter()
            .filter(|row| row.lifecycle == Lifecycle::Live as i32)
            .map(|row| (row.agent.agent, row.incarnation))
            .collect();
        children.sort();
        Ok(children)
    }
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

#[cfg(test)]
mod tests {
    use store::Sqlite;

    use super::*;
    use crate::payload::ClientRequest;
    use crate::{Input, StartOptions};

    const OWN: [u8; 16] = *b"own-host-0000000";
    const PARENT: AgentId = AgentId(*b"parent-000000000");
    const CHILD: AgentId = AgentId(*b"child-0000000000");

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
            let out = self.step(Input::AgentHello {
                agent,
                hello: Default::default(),
            });
            assert!(answer_of(&out, call).is_some(), "{out:?}");
        }
    }

    fn create(agent: AgentId, name: &str, parent: Option<AgentId>) -> ClientRequest {
        ClientRequest::CreateAgent(Box::new(Create {
            agent,
            cwd: "/src".into(),
            given: Some(name.into()),
            branches: BTreeSet::new(),
            new_worktree: false,
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

    #[test]
    fn a_create_writes_the_row_starts_the_process_and_answers_at_its_hello() {
        let mut rig = rig();
        let (call, out) = rig.call(create(PARENT, "lead", None));
        let row = rig.row(PARENT).expect("the row comes first");
        assert_eq!(row.name, "lead");
        assert_eq!(row.lifecycle, Lifecycle::Live as i32);
        let spawned = out.iter().find_map(|output| match output {
            Output::SpawnAgent { spec, .. } => Some(spec.clone()),
            _ => None,
        });
        let spawned = spawned.expect("the process starts");
        assert_eq!((spawned.name.as_str(), spawned.incarnation), ("lead", 1));
        assert!(out.contains(&Output::DialAgent { agent: PARENT }));
        assert!(
            answer_of(&out, call).is_none(),
            "answered only once it said Hello"
        );

        let hello = rig.step(Input::AgentHello {
            agent: PARENT,
            hello: Default::default(),
        });
        assert_eq!(
            answer_of(&hello, call).map(|agent| agent.name),
            Some("lead".into())
        );
        assert!(hello.contains(&Output::CancelTimer {
            id: TimerId::Start(PARENT)
        }));

        let (again, out) = rig.call(create(PARENT, "lead", None));
        assert_eq!(refusal_of(&out, again), Some(Refusal::AlreadyExists));
        rig.call(create(CHILD, "lead", None));
        assert_eq!(rig.row(CHILD).map(|row| row.name), Some("lead-2".into()));
    }

    #[test]
    fn a_stop_past_its_deadline_kills_and_the_exit_records_why() {
        let mut rig = rig();
        rig.live(PARENT, "lead", None);
        let (call, out) = rig.call(ClientRequest::StopAgent { agent: PARENT });
        assert_eq!(stops(&out, PARENT), vec![false]);

        let late = rig.step(Input::Timer {
            id: TimerId::Exit(PARENT),
        });
        assert_eq!(stops(&late, PARENT), vec![true], "the stop becomes a kill");
        let ignored = rig.step(Input::Timer {
            id: TimerId::Exit(PARENT),
        });
        let Some((job, Job::KillAgent { agent })) = job_of(&ignored) else {
            panic!("an ignored kill is done from outside: {ignored:?}");
        };
        assert_eq!(agent, PARENT);
        rig.step(Input::JobDone {
            job,
            result: JobResult::Done,
        });

        let gone = rig.step(Input::ControlClosed {
            agent: PARENT,
            ended: Ended::Exited,
        });
        let answered = answer_of(&gone, call).expect("the stop is answered once it ended");
        assert_eq!(answered.lifecycle, Lifecycle::Exited as i32);
        assert_eq!(answered.exit_cause, ExitCause::Killed as i32);
        assert!(gone.contains(&Output::ForgetAgent { agent: PARENT }));
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

        // Told the far parent is gone before its host's listing caught up,
        // as only a shell handing over another host's removal could.
        let out = rig.step(Input::FromShell(crate::FromShell::Removed {
            agent: parent,
            event: Default::default(),
        }));
        let violations: Vec<&str> = out
            .iter()
            .filter_map(|output| match output {
                Output::Dump {
                    reason: crate::DumpReason::Violation(violation),
                } => Some(violation.invariant),
                _ => None,
            })
            .collect();
        assert_eq!(violations, vec![crate::DELETED_UNDER_A_STALE_LISTING]);
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
        rig.step(Input::AgentHello {
            agent: PARENT,
            hello: Default::default(),
        });
        rig.call(ClientRequest::StopAgent { agent: PARENT });
        rig.call(ClientRequest::DeleteAgent { agent: PARENT });

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
    fn a_delete_removes_the_rows_then_the_directory_and_its_children_follow() {
        let mut rig = rig();
        rig.live(PARENT, "lead", None);
        rig.live(CHILD, "scout", Some(PARENT));
        rig.step(Input::Drafts {
            drafts: vec![(PARENT.0.to_vec(), b"draft".to_vec())],
        });
        rig.step(Input::ControlClosed {
            agent: PARENT,
            ended: Ended::Exited,
        });

        let (call, out) = rig.call(ClientRequest::DeleteAgent { agent: PARENT });
        assert!(
            rig.row(PARENT).is_none(),
            "an agent with no process goes at once"
        );
        let Some((job, Job::RemoveDir { path })) = job_of(&out) else {
            panic!("its directory goes: {out:?}");
        };
        assert_eq!(path, rig.daemon.agent_dir(PARENT));

        let done = rig.step(Input::JobDone {
            job,
            result: JobResult::Done,
        });
        assert!(done.contains(&Output::EndCall {
            call,
            status: Status::Ok,
        }));
        assert!(
            !rig.daemon.drafts.holds(&PARENT.0),
            "its draft goes with it"
        );

        // The child was stopping with its exited parent; its delete waits
        // behind that stop, and goes once the child has ended.
        assert!(rig.row(CHILD).is_some());
        let ended = rig.step(Input::ControlClosed {
            agent: CHILD,
            ended: Ended::Exited,
        });
        assert!(
            rig.row(CHILD).is_none(),
            "the child follows its deleted parent"
        );
        assert!(matches!(job_of(&ended), Some((_, Job::RemoveDir { .. }))));
    }
}
