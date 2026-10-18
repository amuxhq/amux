//! The unsent drafts of this machine's clients, one per agent, kept so a
//! draft outlives the client that was typing it (a terminal client closing
//! or detaching). Drafts are this machine's and never reach the store,
//! other hosts or other machines.
//!
//! The core holds them and answers for them; the file
//! ([`StartOptions::drafts`](crate::StartOptions::drafts)) is written whole
//! by one [`Job::Fsync`] at a time after each change, so a later change
//! never lands under an earlier one. The driver reads the file at start
//! and hands it over as [`Input::Drafts`](crate::Input::Drafts).

use std::collections::BTreeMap;

use prost::Message as _;
use serde::{Deserialize, Serialize};

use crate::Daemon;
use crate::ids::{CallId, JobId};
use crate::io::{Job, Output};
use crate::payload::{JobResult, Response, Status};
use crate::serve::answered;

/// Each agent's draft, encoded, by agent id, as the file and
/// [`Input::Drafts`](crate::Input::Drafts) carry them.
pub type DraftList = Vec<(Vec<u8>, Vec<u8>)>;

/// Each agent's draft, encoded, by agent id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Drafts {
    by_agent: BTreeMap<Vec<u8>, Vec<u8>>,
    /// The write of the file under way.
    writing: Option<JobId>,
    /// Changed since that write began: written again when it is done.
    dirty: bool,
}

/// The drafts as the file holds them.
pub fn encode(drafts: &DraftList) -> Vec<u8> {
    postcard::to_allocvec(drafts).expect("drafts serialize")
}

/// The drafts a file holds; a file that does not decode holds none, since
/// losing a draft must never stop the profile.
pub fn decode(bytes: &[u8]) -> DraftList {
    postcard::from_bytes(bytes).unwrap_or_else(|error| {
        tracing::warn!(%error, "cannot read the drafts; starting empty");
        Vec::new()
    })
}

impl Drafts {
    pub(crate) fn jobs(&self, out: &mut Vec<JobId>) {
        out.extend(self.writing);
    }

    pub(crate) fn canonical(&self, ids: &crate::fingerprint::Ids) -> Drafts {
        Drafts {
            writing: self.writing.map(|job| ids.jobs.get(job)),
            ..self.clone()
        }
    }

    /// Whether a draft of the agent is kept.
    pub fn holds(&self, agent_id: &[u8]) -> bool {
        self.by_agent.contains_key(agent_id)
    }
}

impl Daemon {
    /// The drafts file as the driver read it at start.
    pub(crate) fn drafts_read(&mut self, drafts: DraftList) {
        self.drafts.by_agent = drafts.into_iter().collect();
    }

    /// The agent's draft, empty if none is kept.
    pub(crate) fn get_draft(&self, call: CallId, agent_id: &[u8], out: &mut Vec<Output>) {
        let draft = self
            .drafts
            .by_agent
            .get(agent_id)
            .and_then(|bytes| wire::ComposerDraft::decode(bytes.as_slice()).ok())
            .unwrap_or_default();
        answered(call, Response::Draft(draft), out);
    }

    /// Keeps `draft` for the agent; an empty one forgets it. Answered at
    /// once: a save that fails to reach the file is logged, and the client
    /// still holds the words it showed.
    pub(crate) fn save_draft(
        &mut self,
        call: CallId,
        agent_id: Vec<u8>,
        draft: &wire::ComposerDraft,
        out: &mut Vec<Output>,
    ) {
        if draft.text.trim().is_empty() && draft.attachments.is_empty() {
            self.forget_draft(&agent_id, out);
        } else {
            self.drafts.by_agent.insert(agent_id, draft.encode_to_vec());
            self.write_drafts(out);
        }
        out.push(Output::EndCall {
            call,
            status: Status::Ok,
        });
    }

    /// Forgets the agent's draft: the agent is gone.
    pub(crate) fn forget_draft(&mut self, agent_id: &[u8], out: &mut Vec<Output>) {
        if self.drafts.by_agent.remove(agent_id).is_some() {
            self.write_drafts(out);
        }
    }

    fn write_drafts(&mut self, out: &mut Vec<Output>) {
        if self.drafts.writing.is_some() {
            self.drafts.dirty = true;
            return;
        }
        let job = self.pending.mint();
        self.drafts.writing = Some(job);
        self.drafts.dirty = false;
        let drafts: DraftList = self
            .drafts
            .by_agent
            .iter()
            .map(|(agent, draft)| (agent.clone(), draft.clone()))
            .collect();
        out.push(Output::Job {
            id: job,
            op: Job::Fsync {
                path: self.options.drafts.clone(),
                bytes: encode(&drafts),
            },
        });
    }

    /// A write of the drafts file finished; returns whether `job` was one.
    pub(crate) fn drafts_written(
        &mut self,
        job: JobId,
        result: &JobResult,
        out: &mut Vec<Output>,
    ) -> bool {
        if self.drafts.writing != Some(job) {
            return false;
        }
        self.drafts.writing = None;
        if *result != JobResult::Done {
            tracing::warn!(?result, "cannot save the drafts");
        }
        if self.drafts.dirty {
            self.write_drafts(out);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use store::Sqlite;

    use super::*;
    use crate::payload::ClientRequest;
    use crate::{Input, StartOptions, StepBudget};

    fn daemon() -> Daemon {
        let store = Sqlite::open_in_memory(vec![1; 16]).expect("a store opens");
        Daemon::new(
            store,
            StartOptions {
                drafts: "/profile/drafts".into(),
                budget: StepBudget::UNLIMITED,
                ..StartOptions::default()
            },
        )
    }

    fn save(daemon: &mut Daemon, text: &str) -> Vec<Output> {
        let mut out = Vec::new();
        daemon.step(
            Input::ClientCall {
                call: CallId(1),
                request: ClientRequest::SaveDraft {
                    agent_id: vec![2; 16],
                    draft: wire::ComposerDraft {
                        text: text.into(),
                        ..Default::default()
                    },
                },
            },
            &mut out,
        );
        out
    }

    fn writes(out: &[Output]) -> Vec<(JobId, DraftList)> {
        out.iter()
            .filter_map(|output| match output {
                Output::Job {
                    id,
                    op: Job::Fsync { bytes, .. },
                } => Some((*id, decode(bytes))),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn one_write_of_the_file_at_a_time_and_the_last_change_is_written_last() {
        let mut daemon = daemon();
        let first = writes(&save(&mut daemon, "half a thought"));
        assert_eq!(first.len(), 1);
        // Changed twice while the first write is under way: nothing more
        // is written until it is done, and then the newest drafts.
        assert!(writes(&save(&mut daemon, "a whole")).is_empty());
        assert!(writes(&save(&mut daemon, "a whole thought")).is_empty());
        let mut out = Vec::new();
        daemon.step(
            Input::JobDone {
                job: first[0].0,
                result: JobResult::Done,
            },
            &mut out,
        );
        let again = writes(&out);
        let [(_, drafts)] = again.as_slice() else {
            panic!("one write: {out:?}");
        };
        let draft = wire::ComposerDraft::decode(drafts[0].1.as_slice()).expect("a draft");
        assert_eq!(draft.text, "a whole thought");

        let mut out = Vec::new();
        daemon.step(
            Input::ClientCall {
                call: CallId(2),
                request: ClientRequest::GetDraft {
                    agent_id: vec![2; 16],
                },
            },
            &mut out,
        );
        assert!(matches!(
            out.as_slice(),
            [Output::Answer { response: Response::Draft(draft), .. }, Output::EndCall { .. }]
                if draft.text == "a whole thought"
        ));
    }
}
