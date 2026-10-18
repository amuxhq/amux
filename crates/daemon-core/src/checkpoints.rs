//! Checkpoints: when the core takes one, what it holds while one is taken,
//! and the journal it lets go of once one is on the drive.
//!
//! A checkpoint is the core's state serialized at one step, written beside
//! the store's main file after a passive WAL checkpoint run at that step,
//! so the two together are the daemon as of the step. Commits are held
//! from that step until the job is done, which keeps the main file from
//! running ahead of the state it is paired with. Store commits are not
//! synced one by one, so a journal segment may go only once a checkpoint
//! has put a cursor past it on the drive.

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
    /// The checkpoint being taken, which commits wait for.
    taking: Option<Taking>,
    /// Each agent's cursor as of the newest checkpoint on the drive: the
    /// store's main file holds at least this much of its journal, so the
    /// segments below it may go.
    durable: BTreeMap<AgentId, u64>,
    /// Agents committed since their cursors were last taken into a
    /// checkpoint.
    committed: BTreeSet<AgentId>,
    /// Agents whose ingest waits for the checkpoint being taken.
    held: BTreeSet<AgentId>,
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

    /// Whether a checkpoint is being taken, so commits wait.
    pub fn taking(&self) -> bool {
        self.taking.is_some()
    }
}

impl Daemon {
    /// Takes a checkpoint, unless one is being taken: serializes the core
    /// as of this step and asks for it to be written beside a passive WAL
    /// checkpoint. Ingest holds its commits until the job is done.
    pub(crate) fn take_checkpoint(&mut self, out: &mut Vec<Output>) {
        if self.checkpoints.taking.is_some() {
            return;
        }
        if std::mem::take(&mut self.checkpoints.armed) {
            out.push(Output::CancelTimer {
                id: TimerId::Checkpoint,
            });
        }
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
    /// Either way the held commits go ahead.
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
        for agent in std::mem::take(&mut self.checkpoints.held) {
            self.ingest(agent, out);
        }
    }

    /// Holds the agent's ingest while a checkpoint is taken: true when it
    /// waits, and is read once the checkpoint is done.
    pub(crate) fn hold_ingest(&mut self, agent: AgentId) -> bool {
        if self.checkpoints.taking.is_none() {
            return false;
        }
        self.checkpoints.held.insert(agent);
        true
    }

    /// Notes a commit of the agent's journal: the next checkpoint is due
    /// an interval on, if it is not already.
    pub(crate) fn committed(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        self.checkpoints.committed.insert(agent);
        self.arm_checkpoint(out);
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
