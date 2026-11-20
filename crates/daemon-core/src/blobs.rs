//! Blob files: an attachment's bytes, named by their SHA-256, in the
//! directory of the agent they belong to.
//!
//! A blob is written only into an agent this host runs, or, fetched from
//! its origin, under the replica of a paired host's agent. The bytes go to
//! the drive as a [`Job::WriteBlob`] and the call is answered when the job
//! is done; a read leaves as [`Output::SendFile`], which the driver answers
//! from the file without the bytes passing through the core. The hash is
//! the caller's: hashing an attachment's bytes would take a step longer
//! than its budget. A write failing on a full drive is told apart, which
//! the driver says by answering the job with a `ResourceExhausted`
//! refusal.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use store::AgentRow;
use wire::{BlobRef, ErrorCode, GetBlobRequest, PutBlobRequest};

use crate::Daemon;
use crate::ids::{AgentId, CallId, JobId};
use crate::ingest::agent_dir;
use crate::io::{Job, Output};
use crate::payload::{BlobRefusal, CallError, JobResult, Response, Status};
use crate::serve::answered;

/// An agent directory's folder of blob files.
pub const BLOBS: &str = "blobs";

/// The blob writes under way, by their job.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Blobs {
    writing: BTreeMap<JobId, Writing>,
}

impl Blobs {
    pub(crate) fn jobs(&self, out: &mut Vec<JobId>) {
        out.extend(self.writing.keys().copied());
    }

    pub(crate) fn canonical(&self, ids: &crate::fingerprint::Ids) -> Blobs {
        Blobs {
            writing: self
                .writing
                .iter()
                .map(|(job, writing)| (ids.jobs.get(*job), writing.clone()))
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Writing {
    /// A client's upload, answered with its reference once written.
    Put {
        call: CallId,
        #[serde(with = "crate::codec::message")]
        blob: BlobRef,
    },
    /// A replica's blob the origin sent, kept for the next reader.
    Keep { call: CallId },
}

/// A blob's name on the drive: its hash in lower-case hex.
pub fn hex(hash: &[u8]) -> String {
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl Daemon {
    /// The directory holding the agent's files: its own directory, or the
    /// replica's under its host.
    pub(crate) fn files_of(&self, row: &AgentRow) -> PathBuf {
        let mut agent = AgentId(row.agent.agent.as_slice().try_into().unwrap_or_default());
        if row.agent.host == self.store.own_host() {
            // A provider subagent has no directory: everything it attaches
            // or uploads lives in the directory of the agent that runs it,
            // whose process wrote it.
            if row.native.is_some()
                && let Some(host) = row
                    .parent
                    .as_ref()
                    .and_then(|parent| AgentId::from_slice(&parent.agent))
            {
                agent = host;
            }
            agent_dir(&self.options.agents, agent)
        } else {
            let host: [u8; 16] = row.agent.host.as_slice().try_into().unwrap_or_default();
            agent_dir(
                &agent_dir(&self.options.replicas, AgentId(host)).join("agents"),
                agent,
            )
        }
    }

    fn blob_owner(&self, agent_id: &[u8]) -> Result<AgentRow, CallError> {
        self.store.agent_by_id(agent_id)?.ok_or(CallError::NoAgent)
    }

    /// A client uploads a blob into one of this host's agents; the bytes
    /// come beside the request, which carries none.
    pub(crate) fn put_blob(
        &mut self,
        call: CallId,
        request: PutBlobRequest,
        hash: Vec<u8>,
        bytes: Vec<u8>,
        out: &mut Vec<Output>,
    ) -> Result<(), CallError> {
        let row = self.blob_owner(&request.agent_id)?;
        if row.agent.host != self.store.own_host() {
            return Err(CallError::Blob(BlobRefusal::NotOwn));
        }
        let blob = BlobRef {
            size: bytes.len() as u64,
            hash: hash.clone(),
            name: request.name,
            mime: request.mime,
        };
        let dir = self.files_of(&row).join(BLOBS);
        self.write_blob(Writing::Put { call, blob }, dir, hash, bytes, out);
        Ok(())
    }

    /// Keeps a replica's blob the origin sent; an own agent's is already
    /// here.
    pub(crate) fn keep_blob(
        &mut self,
        call: CallId,
        agent_id: &[u8],
        hash: Vec<u8>,
        bytes: Vec<u8>,
        out: &mut Vec<Output>,
    ) -> Result<(), CallError> {
        let row = self.blob_owner(agent_id)?;
        if row.agent.host == self.store.own_host() {
            out.push(Output::EndCall {
                call,
                status: Status::Ok,
            });
            return Ok(());
        }
        let dir = self.files_of(&row).join(BLOBS);
        self.write_blob(Writing::Keep { call }, dir, hash, bytes, out);
        Ok(())
    }

    fn write_blob(
        &mut self,
        writing: Writing,
        dir: PathBuf,
        hash: Vec<u8>,
        bytes: Vec<u8>,
        out: &mut Vec<Output>,
    ) {
        let job = self.pending.mint();
        self.blobs.writing.insert(job, writing);
        out.push(Output::Job {
            id: job,
            op: Job::WriteBlob { dir, hash, bytes },
        });
    }

    /// A client reads one of an agent's blobs, own or replica.
    pub(crate) fn get_blob(
        &mut self,
        call: CallId,
        request: &GetBlobRequest,
        out: &mut Vec<Output>,
    ) -> Result<(), CallError> {
        if request.hash.len() != 32 {
            return Err(CallError::Blob(BlobRefusal::BadHash(request.hash.len())));
        }
        let row = self.blob_owner(&request.agent_id)?;
        out.push(Output::SendFile {
            call,
            path: self.files_of(&row).join(BLOBS).join(hex(&request.hash)),
            range: 0..u64::MAX,
        });
        Ok(())
    }

    /// A blob write finished; returns whether `job` was one.
    pub(crate) fn blob_done(
        &mut self,
        job: JobId,
        result: &JobResult,
        out: &mut Vec<Output>,
    ) -> bool {
        let Some(writing) = self.blobs.writing.remove(&job) else {
            return false;
        };
        let call = match &writing {
            Writing::Put { call, .. } | Writing::Keep { call } => *call,
        };
        match (writing, result) {
            (Writing::Put { blob, .. }, JobResult::Done) => {
                answered(call, Response::Written(blob), out)
            }
            (Writing::Keep { .. }, JobResult::Done) => out.push(Output::EndCall {
                call,
                status: Status::Ok,
            }),
            (_, failed) => {
                let (full, words) = match failed {
                    JobResult::Refused(error) => (
                        error.code == ErrorCode::ResourceExhausted as i32,
                        error.message.clone(),
                    ),
                    JobResult::Failed(words) => (false, words.clone()),
                    other => (false, format!("{other:?}")),
                };
                out.push(Output::EndCall {
                    call,
                    status: Status::Failed(CallError::Blob(BlobRefusal::Write { full, words })),
                });
            }
        }
        true
    }
}
