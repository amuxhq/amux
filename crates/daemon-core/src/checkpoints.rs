//! Checkpoints: when the core takes one, and the journal it lets go of once
//! one is on the drive.
//!
//! A checkpoint is the core's state serialized at one step, written beside
//! the store's main file by a job. The step that takes it first runs a
//! passive WAL checkpoint on the core's own connection, so the main file
//! holds exactly what was committed up to that step; with SQLite's own
//! checkpoints off, every commit after it stays in the WAL until the next
//! checkpoint's step, so the main file and the state are the daemon as of
//! one step whatever the core commits while the job runs. Store commits
//! are not synced one by one, so a journal segment may go only once a
//! checkpoint has put a cursor past it on the drive.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use store::AgentKey;

use crate::Daemon;
use crate::ids::{AgentId, JobId, TimerId};
use crate::ingest::KEPT_SEGMENTS;
use crate::io::{Job, Output};
use crate::payload::JobResult;

/// What the core remembers about its checkpoints.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
}

/// A checkpoint job in flight and the cursors it puts on the drive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Taking {
    job: JobId,
    cursors: BTreeMap<AgentId, u64>,
}

impl Checkpoints {
    pub(crate) fn jobs(&self, out: &mut Vec<JobId>) {
        out.extend(self.taking.as_ref().map(|taking| taking.job));
    }

    pub(crate) fn canonical(&self, ids: &crate::fingerprint::Ids) -> Checkpoints {
        let mut checkpoints = self.clone();
        if let Some(taking) = checkpoints.taking.as_mut() {
            taking.job = ids.jobs.get(taking.job);
        }
        checkpoints
    }

    /// The cursor below which the agent's journal may be reclaimed: its
    /// cursor at the newest checkpoint on the drive.
    pub fn durable_cursor(&self, agent: AgentId) -> Option<u64> {
        self.durable.get(&agent).copied()
    }

    /// Whether a checkpoint is being taken.
    pub fn taking(&self) -> bool {
        self.taking.is_some()
    }
}

impl Daemon {
    /// Takes a checkpoint: puts what is committed into the store's main
    /// file, then serializes the core as of this step and asks for it to be
    /// written beside it. One being taken already makes this one due an
    /// interval on.
    pub(crate) fn take_checkpoint(&mut self, out: &mut Vec<Output>) {
        if std::mem::take(&mut self.checkpoints.armed) {
            out.push(Output::CancelTimer {
                id: TimerId::Checkpoint,
            });
        }
        if self.checkpoints.taking.is_some() {
            self.arm_checkpoint(out);
            return;
        }
        if let Err(error) = self.store.checkpoint_wal() {
            tracing::warn!(%error, "a checkpoint of the WAL failed; the next is an interval on");
            self.arm_checkpoint(out);
            return;
        }
        // Every commit so far is in the main file now.
        self.changes = self.store.changes();
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
        // Serialized once the step has decided everything it will, so the
        // checkpoint is the core after this step: the inputs after it,
        // its own job's end among them, replay onto it.
        let job = self.pending.mint();
        self.checkpoints.taking = Some(Taking { job, cursors });
        let state = self.checkpoint().to_bytes();
        out.push(Output::Job {
            id: job,
            op: Job::Checkpoint { state },
        });
    }

    /// Whether `job` is the checkpoint being taken.
    pub(crate) fn is_checkpoint(&self, job: JobId) -> bool {
        self.checkpoints
            .taking
            .as_ref()
            .is_some_and(|taking| taking.job == job)
    }

    /// The checkpoint job ended. Done, its cursors are on the drive and the
    /// journal below them goes; failed, the next is tried an interval on.
    pub(crate) fn checkpoint_done(&mut self, result: JobResult, out: &mut Vec<Output>) {
        let Some(Taking { cursors, .. }) = self.checkpoints.taking.take() else {
            return;
        };
        match result {
            JobResult::Done => {
                for (agent, cursor) in cursors {
                    self.checkpoints.durable.insert(agent, cursor);
                    self.reclaim(agent, cursor);
                }
            }
            _ => {
                tracing::warn!(?result, "a checkpoint failed; the next is an interval on");
                self.checkpoints.committed.extend(cursors.into_keys());
                self.arm_checkpoint(out);
            }
        }
    }

    /// Notes a commit of the agent's journal, whose cursor the next
    /// checkpoint takes.
    pub(crate) fn committed(&mut self, agent: AgentId) {
        self.checkpoints.committed.insert(agent);
    }

    /// Any commit the step made, of whatever row, makes the next checkpoint
    /// due an interval on, if it is not already: with SQLite's own
    /// checkpoints off, only a checkpoint moves the WAL into the main file.
    pub(crate) fn arm_on_commit(&mut self, out: &mut Vec<Output>) {
        let changes = self.store.changes();
        if changes != self.changes {
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
        let quiet = step(&mut daemon, Input::Tick { now_ms: 5 });
        assert!(!armed(&quiet), "a step that wrote nothing arms nothing");
    }

    /// The store's main file is taken up to the checkpoint's step, and a
    /// row written while the job runs stays out of it, so the checkpoint
    /// and the main file are the daemon as of that step.
    #[test]
    fn a_row_written_while_a_checkpoint_is_taken_stays_out_of_the_main_file() {
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
        let taken = step(
            &mut daemon,
            Input::Timer {
                id: TimerId::Checkpoint,
            },
        );
        let Some(Output::Job {
            id: job,
            op: Job::Checkpoint { state },
        }) = taken
            .iter()
            .find(|output| matches!(output, Output::Job { .. }))
            .cloned()
        else {
            panic!("the timer takes a checkpoint: {taken:?}");
        };
        step(&mut daemon, create(during));
        assert!(daemon.checkpoints().taking());

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
        assert!(held(&main, before), "written before the checkpoint's step");
        assert!(!held(&main, during), "written while the job ran");
        assert!(held(daemon.store(), during), "committed all the same");

        // The checkpoint restored over that main file is the core as of
        // the step that took it, and the inputs after it replay onto it.
        let mut restored = Daemon::restore(
            crate::Checkpoint::from_bytes(&state).expect("it reads"),
            main,
        )
        .expect("it restores");
        step(&mut restored, create(during));
        assert!(held(restored.store(), during));
        step(
            &mut daemon,
            Input::JobDone {
                job,
                result: JobResult::Done,
            },
        );
        assert!(!daemon.checkpoints().taking());
    }
}
