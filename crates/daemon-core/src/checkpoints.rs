//! Checkpoints: when the core takes one, and the journal it lets go of once
//! one is on the drive.
//!
//! A checkpoint is the core's state serialized at one step, paired with
//! the store's main file. Taking one begins with a passive WAL checkpoint,
//! a job on a connection of its own, since moving the WAL into the main
//! file can take tens of milliseconds and nothing slow runs inside a step.
//! From the step that starts it until its end, every input that may write
//! the store waits in the core, in the order it came, so the main file
//! then holds exactly what was committed before the hold; reads,
//! subscriptions, presence, routing and peers' reads step on. The core is
//! serialized at the step that hears the copy is done, with what waits
//! still waiting, and handed to the driver, whose input ring starts a
//! segment at it; the driver then feeds one input back, and that step
//! lets what waited through, so the segment records it. With SQLite's own
//! checkpoints off, every commit after that stays in the WAL until the next
//! checkpoint, so the main file and the state are the daemon as of one
//! step. Store commits are not synced one by one, but the WAL move syncs
//! the main file, so a journal segment may go once a checkpoint has put a
//! cursor past it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use store::AgentKey;

use crate::Daemon;
use crate::ids::{AgentId, JobId, TimerId};
use crate::ingest::KEPT_SEGMENTS;
use crate::io::{Input, Job, Output};
use crate::payload::{ClientRequest, JobResult, PeerRequest};

/// What the core remembers about its checkpoints.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Checkpoints {
    /// The checkpoint timer is armed: something was committed since the
    /// last checkpoint was taken.
    armed: bool,
    /// The checkpoint being taken.
    taking: Option<Taking>,
    /// Each agent's cursor as of the newest checkpoint on the drive: the
    /// store's main file holds at least this much of its journal, so the
    /// segments below it may go.
    durable: BTreeMap<AgentId, u64>,
    /// Agents committed since their cursors were last taken into a
    /// checkpoint.
    committed: BTreeSet<AgentId>,
    /// The cursors the checkpoint just serialized put on the drive, taken
    /// as durable at the driver's [`Input::Checkpointed`].
    landing: Option<BTreeMap<AgentId, u64>>,
    /// Inputs that may write the store, stepped while the WAL was being
    /// moved into the main file, in the order they came.
    held: VecDeque<Input>,
}

/// A checkpoint in flight: the WAL job under way, during which store
/// writes wait, and the cursors the checkpoint puts on the drive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Taking {
    job: JobId,
    cursors: BTreeMap<AgentId, u64>,
}

impl Checkpoints {
    pub(crate) fn jobs(&self, out: &mut Vec<JobId>) {
        out.extend(self.taking.as_ref().map(|taking| taking.job));
        for input in &self.held {
            if let Input::JobDone { job, .. } = input {
                out.push(*job);
            }
        }
    }

    pub(crate) fn canonical(&self, ids: &crate::fingerprint::Ids) -> Checkpoints {
        let mut checkpoints = self.clone();
        if let Some(taking) = checkpoints.taking.as_mut() {
            taking.job = ids.jobs.get(taking.job);
        }
        for input in &mut checkpoints.held {
            match input {
                Input::JobDone { job, .. } => *job = ids.jobs.get(*job),
                Input::PeerAnswer { call, .. } => *call = ids.calls.get(*call),
                _ => {}
            }
        }
        checkpoints
    }

    /// The cursor below which the agent's journal may be reclaimed: its
    /// cursor at the newest checkpoint on the drive.
    pub fn durable_cursor(&self, agent: AgentId) -> Option<u64> {
        self.durable.get(&agent).copied()
    }

    /// Whether a checkpoint is being taken: its WAL job runs, or its
    /// serialized state waits for the driver's word.
    pub fn taking(&self) -> bool {
        self.taking.is_some() || self.landing.is_some()
    }

    /// Whether store writes wait: the WAL is being moved into the main
    /// file.
    pub fn holding(&self) -> bool {
        self.taking.is_some()
    }

    /// Inputs waiting for the WAL checkpoint to end.
    pub fn held(&self) -> usize {
        self.held.len()
    }

    /// Keeps `input` for after the WAL checkpoint, if one is under way and
    /// the input may write the store; gives it back otherwise. One the same
    /// as an input already waiting is the same news twice (a second nudge
    /// of one journal) and waits as that one.
    pub(crate) fn hold(&mut self, input: Input) -> Option<Input> {
        match &self.taking {
            Some(Taking { job, .. }) if writes(&input, *job) => {
                if !self.held.contains(&input) {
                    self.held.push_back(input);
                }
                None
            }
            _ => Some(input),
        }
    }
}

/// Whether stepping `input` may write the store, as far as a checkpoint's
/// hold goes: everything but the clock, the checkpoint's own timer and its
/// WAL job's end (`copying`), reads and subscriptions, presence and
/// routing, and peers' reads. Writes to files beside the store (trust,
/// drafts, blobs) count as writes, so the core the checkpoint serializes
/// has asked for none of them since its main file was cut.
fn writes(input: &Input, copying: JobId) -> bool {
    match input {
        Input::JobDone { job, .. } => *job != copying,
        Input::ClientCall { request, .. } => match request {
            ClientRequest::SubscribeSession { .. }
            | ClientRequest::ListItems(_)
            | ClientRequest::GetItem(_)
            | ClientRequest::SubscribeInventory
            | ClientRequest::GetBlob(_)
            | ClientRequest::GetDiff(_) => false,
            ClientRequest::Forward { request, .. } => peer_writes(request),
            _ => true,
        },
        Input::PeerCall { request, .. } => peer_writes(request),
        Input::Tick { .. }
        | Input::Timer {
            id: TimerId::Checkpoint | TimerId::Refresh(_) | TimerId::Attempt(_) | TimerId::Retry(_),
        }
        | Input::AnswerLagged { .. }
        | Input::LinkUp { .. }
        | Input::LinkDown { .. }
        | Input::LinkCarried { .. }
        | Input::LinkControl { .. }
        | Input::Credential { .. }
        | Input::WantChannel { .. }
        | Input::ChannelClosed { .. }
        | Input::ChannelAnswered { .. }
        | Input::PlanChanged
        | Input::TryAgain { .. }
        | Input::Candidates { .. }
        | Input::Dialling(_)
        | Input::RecoverRelay
        | Input::Listening(_)
        | Input::Wanted { .. }
        | Input::Entropy(_)
        | Input::Configure(_)
        | Input::Checkpointed => false,
        _ => true,
    }
}

fn peer_writes(request: &PeerRequest) -> bool {
    !matches!(
        request,
        PeerRequest::SubscribeSession(_)
            | PeerRequest::SubscribeInventory
            | PeerRequest::ListItems(_)
            | PeerRequest::GetItem(_)
            | PeerRequest::GetBlob(_)
            | PeerRequest::GetDiff(_)
            | PeerRequest::GetCatalogue(_)
            | PeerRequest::ListRepositories(_)
    )
}

impl Daemon {
    /// Takes a checkpoint: asks for the WAL to be moved into the store's
    /// main file, holding store writes until it is. One being taken
    /// already makes this one due an interval on.
    pub(crate) fn take_checkpoint(&mut self, out: &mut Vec<Output>) {
        if std::mem::take(&mut self.checkpoints.armed) {
            out.push(Output::CancelTimer {
                id: TimerId::Checkpoint,
            });
        }
        if self.checkpoints.taking() {
            self.arm_checkpoint(out);
            return;
        }
        // Nothing commits until the copy is done, so these cursors are the
        // ones the main file will hold.
        let mut cursors = BTreeMap::new();
        for agent in std::mem::take(&mut self.checkpoints.committed) {
            let key = AgentKey::new(self.store.own_host(), agent.0);
            match self.store.cursor(&key) {
                Ok(cursor) => {
                    cursors.insert(agent, cursor);
                }
                Err(error) => {
                    // Its journal waits for a later checkpoint.
                    tracing::warn!(%error, ?agent, "reading a cursor to checkpoint failed");
                    self.checkpoints.committed.insert(agent);
                }
            }
        }
        let job = self.pending.mint();
        self.checkpoints.taking = Some(Taking { job, cursors });
        self.touched.hold_began = true;
        out.push(Output::Job {
            id: job,
            op: Job::CheckpointWal,
        });
    }

    /// Whether `job` is the checkpoint's job under way.
    pub(crate) fn is_checkpoint(&self, job: JobId) -> bool {
        self.checkpoints
            .taking
            .as_ref()
            .is_some_and(|taking| taking.job == job)
    }

    /// The checkpoint's WAL job ended. The WAL in the main file, the core
    /// is serialized as of this step and handed to the driver; failed, the
    /// next is tried an interval on.
    pub(crate) fn checkpoint_done(&mut self, result: JobResult, out: &mut Vec<Output>) {
        let Some(Taking { cursors, .. }) = self.checkpoints.taking.take() else {
            return;
        };
        match result {
            JobResult::Done => {
                // Every commit so far is in the main file now.
                self.changes = self.store.changes();
                // Serialized once the step has decided everything it will,
                // so the checkpoint is the core after this step, what
                // waits still waiting and these cursors still landing: the
                // inputs after it, the driver's Checkpointed first, replay
                // onto it.
                self.checkpoints.landing = Some(cursors);
                self.touched.serialized = true;
                let state = self.checkpoint();
                out.push(Output::Checkpoint { state });
            }
            result => {
                tracing::warn!(?result, "a checkpoint failed; the next is an interval on");
                self.checkpoints.committed.extend(cursors.into_keys());
                self.arm_checkpoint(out);
            }
        }
    }

    /// The driver's ring starts again at the checkpoint just serialized:
    /// the WAL job synced the main file, so the cursors it took are on the
    /// drive and the journal below them goes.
    pub(crate) fn checkpointed(&mut self) {
        let Some(cursors) = self.checkpoints.landing.take() else {
            return;
        };
        for (agent, cursor) in cursors {
            self.checkpoints.durable.insert(agent, cursor);
            self.reclaim(agent, cursor);
        }
    }

    /// Steps the inputs that waited for a WAL checkpoint, in the order they
    /// came, unless one is under way; one of them may start the next. Then
    /// starts the trust changes queued behind it: an input it does not hold,
    /// such as a link's control frame, may have queued one.
    pub(crate) fn release_held(&mut self, out: &mut Vec<Output>) {
        while !self.checkpoints.holding()
            && let Some(input) = self.checkpoints.held.pop_front()
        {
            self.dispatch(input, out);
        }
        self.next_trust_change(out);
    }

    /// Notes a commit of the agent's journal, whose cursor the next
    /// checkpoint takes.
    pub(crate) fn committed(&mut self, agent: AgentId) {
        self.checkpoints.committed.insert(agent);
    }

    /// Any commit the step made, of whatever row, makes the next checkpoint
    /// due an interval on, if it is not already: with SQLite's own
    /// checkpoints off, only a checkpoint moves the WAL into the main file.
    /// A commit while the WAL is being moved, but in the step that began
    /// moving it, is one the checkpoint does not account for.
    pub(crate) fn arm_on_commit(&mut self, out: &mut Vec<Output>) {
        let changes = self.store.changes();
        if changes != self.changes {
            if self.checkpoints.holding() && !self.touched.hold_began {
                self.touched.committed_while_held = true;
            }
            self.changes = changes;
            self.arm_checkpoint(out);
        }
    }

    /// The agent's cursor went back to `end`: a checkpoint never vouches
    /// for more of its journal than is now there.
    pub(crate) fn rewound(&mut self, agent: AgentId, end: u64) {
        if let Some(cursor) = self.checkpoints.durable.get_mut(&agent) {
            *cursor = (*cursor).min(end);
        }
    }

    fn arm_checkpoint(&mut self, out: &mut Vec<Output>) {
        if self.checkpoints.armed {
            return;
        }
        self.checkpoints.armed = true;
        out.push(Output::ArmTimer {
            id: TimerId::Checkpoint,
            at_ms: self.timers.now_ms + self.settings.checkpoint_interval_ms,
        });
    }

    /// Deletes the agent's journal segments that lie wholly below `below`,
    /// a cursor on the drive, keeping the newest [`KEPT_SEGMENTS`] of them
    /// for dumps.
    fn reclaim(&mut self, agent: AgentId, below: u64) {
        self.touched.reclaimed.push((agent, below));
        let dir = self.journal_dir(agent);
        let starts = match journal::reclaimable(&dir, below, KEPT_SEGMENTS) {
            Ok(starts) => starts,
            Err(error) => {
                tracing::warn!(%error, "listing journal segments failed");
                return;
            }
        };
        for start in starts {
            if let Err(error) = std::fs::remove_file(journal::segment_path(&dir, start)) {
                tracing::warn!(%error, start, "deleting an ingested journal segment failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use store::Sqlite;

    use super::*;
    use crate::payload::ClientRequest;
    use crate::{CallId, Create, Input, StartOptions};

    const OWN: [u8; 16] = *b"own-host-0000000";

    fn create(agent: AgentId) -> Input {
        Input::ClientCall {
            call: CallId(u64::from(agent.0[0])),
            request: ClientRequest::CreateAgent(Box::new(Create {
                agent,
                cwd: "/src".into(),
                given: None,
                branches: BTreeSet::new(),
                new_worktree: false,
                recent: None,
                spec: wire::AgentSpec {
                    agent_id: agent.0.to_vec(),
                    kind: wire::Kind::Codex as i32,
                    ..wire::AgentSpec::default()
                },
            })),
        }
    }

    fn step(daemon: &mut Daemon, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        daemon.step(input, &mut out);
        out
    }

    fn armed(out: &[Output]) -> bool {
        out.iter().any(|output| {
            matches!(
                output,
                Output::ArmTimer {
                    id: TimerId::Checkpoint,
                    ..
                }
            )
        })
    }

    /// A row the registry writes, not an ingest, makes a checkpoint due.
    #[test]
    fn any_commit_makes_the_next_checkpoint_due() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let mut daemon = Daemon::new(
            Sqlite::open_in_memory(OWN).expect("a store opens"),
            StartOptions {
                agents: dir.path().to_path_buf(),
                ..StartOptions::default()
            },
        );
        step(&mut daemon, Input::Swept);
        let made = step(&mut daemon, create(AgentId([1; 16])));
        assert!(
            made.iter()
                .any(|output| matches!(output, Output::SpawnAgent { .. }))
        );
        assert!(armed(&made), "{made:?}");
        let quiet = step(
            &mut daemon,
            Input::Tick {
                now_ms: 5,
                link_ms: 5,
            },
        );
        assert!(!armed(&quiet), "a step that wrote nothing arms nothing");
    }

    /// The WAL job a step asked for, if it asked for one.
    fn wal_job(out: &[Output]) -> Option<JobId> {
        out.iter().find_map(|output| match output {
            Output::Job {
                id,
                op: Job::CheckpointWal,
            } => Some(*id),
            _ => None,
        })
    }

    /// The state a step handed the driver, if it serialized the core.
    fn serialized(out: &[Output]) -> Option<&[u8]> {
        out.iter().find_map(|output| match output {
            Output::Checkpoint { state } => Some(state.as_slice()),
            _ => None,
        })
    }

    fn spawned(out: &[Output]) -> bool {
        out.iter()
            .any(|output| matches!(output, Output::SpawnAgent { .. }))
    }

    /// A write that arrives while the WAL job runs waits out the job, so
    /// the main file holds exactly what was committed before it, and lands
    /// in the step after the one that serialized the core; the checkpoint
    /// restored over that main file steps it the same way.
    #[test]
    fn a_write_during_the_wal_job_waits_for_its_end_and_lands_after_it() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("store.sqlite");
        let store = Sqlite::open(&path, OWN).expect("a store opens");
        store
            .leave_checkpoints_to_owner()
            .expect("checkpoints are ours");
        let mut daemon = Daemon::new(
            store,
            StartOptions {
                agents: dir.path().join("agents"),
                ..StartOptions::default()
            },
        );
        step(&mut daemon, Input::Swept);
        let (before, during) = (AgentId([1; 16]), AgentId([2; 16]));
        step(&mut daemon, create(before));
        let started = step(
            &mut daemon,
            Input::Timer {
                id: TimerId::Checkpoint,
            },
        );
        let wal = wal_job(&started).expect("the timer starts the WAL job");
        assert!(daemon.checkpoints().holding());

        let held = step(&mut daemon, create(during));
        assert!(!spawned(&held), "the create waits: {held:?}");
        assert_eq!(daemon.checkpoints().held(), 1);
        let held_again = step(&mut daemon, create(during));
        assert!(!spawned(&held_again));
        assert_eq!(daemon.checkpoints().held(), 1, "the same news waits once");
        let read = step(
            &mut daemon,
            Input::ClientCall {
                call: CallId(9),
                request: ClientRequest::SubscribeSession {
                    agent_id: before.0.to_vec(),
                    tail: 8,
                },
            },
        );
        assert!(
            read.iter().any(|output| matches!(
                output,
                Output::Answer {
                    call: CallId(9),
                    ..
                }
            )),
            "a read steps on: {read:?}"
        );

        // The job, on a connection of its own.
        Sqlite::checkpoint_wal(&path).expect("the WAL moves");
        let done = step(
            &mut daemon,
            Input::JobDone {
                job: wal,
                result: JobResult::Done,
            },
        );
        let Some(state) = serialized(&done) else {
            panic!("the WAL in the main file, the core is serialized: {done:?}");
        };
        let state = state.to_vec();
        assert!(!daemon.checkpoints().holding());
        assert!(
            daemon.checkpoints().taking(),
            "its cursors are still landing"
        );
        assert!(!spawned(&done), "what waited waits for the next step");

        // The main file alone, as the checkpoint is paired with it.
        let copy = dir.path().join("copy.sqlite");
        std::fs::copy(&path, &copy).expect("the main file copies");
        let main = Sqlite::open(&copy, OWN).expect("the main file opens");
        let held = |store: &Sqlite, agent: AgentId| {
            store
                .agent(&AgentKey::new(OWN, agent.0))
                .expect("a row reads")
                .is_some()
        };
        assert!(held(&main, before), "committed before the WAL job");
        assert!(!held(&main, during), "it waited");
        assert!(!held(daemon.store(), during), "nor committed yet");

        // The checkpoint restored over that main file is the core as of
        // the step that serialized it, and the inputs after it replay onto
        // it: the driver's Checkpointed comes first and lets what waited
        // through.
        let mut restored =
            Daemon::restore(&state, main, dir.path().join("agents")).expect("it restores");
        assert_eq!(restored.checkpoint(), daemon.checkpoint());
        assert!(spawned(&step(&mut daemon, Input::Checkpointed)), "it lands");
        assert!(spawned(&step(&mut restored, Input::Checkpointed)));
        assert!(held(daemon.store(), during));
        assert!(held(restored.store(), during));
        assert_eq!(restored.checkpoint(), daemon.checkpoint());
        assert!(!daemon.checkpoints().taking());
    }

    /// A WAL job that could not move the whole WAL lets go of what waited
    /// at once, and the next checkpoint is tried an interval on.
    #[test]
    fn a_busy_wal_job_lets_go_of_what_waited_and_is_tried_again_later() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let mut daemon = Daemon::new(
            Sqlite::open_in_memory(OWN).expect("a store opens"),
            StartOptions {
                agents: dir.path().to_path_buf(),
                ..StartOptions::default()
            },
        );
        step(&mut daemon, Input::Swept);
        let started = step(
            &mut daemon,
            Input::Timer {
                id: TimerId::Checkpoint,
            },
        );
        let wal = wal_job(&started).expect("the timer starts the WAL job");
        assert!(!spawned(&step(&mut daemon, create(AgentId([1; 16])))));
        let busy = step(
            &mut daemon,
            Input::JobDone {
                job: wal,
                result: JobResult::Failed("the WAL was busy".into()),
            },
        );
        assert!(spawned(&busy), "what waited lands at once: {busy:?}");
        assert!(armed(&busy), "the next is an interval on: {busy:?}");
        assert_eq!(serialized(&busy), None, "nothing is serialized");
        assert!(!daemon.checkpoints().taking());
        assert_eq!(daemon.checkpoints().held(), 0);
    }
}
