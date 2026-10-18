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
use crate::payload::{BlobRefusal, CallError, JobResult, PeerReply, Response, Status};
use crate::serve::answered;

/// An agent directory's folder of blob files.
pub const BLOBS: &str = "blobs";

/// The blob writes under way, by their job.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Blobs {
    writing: BTreeMap<JobId, Writing>,
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
        let agent = AgentId(row.agent.agent.as_slice().try_into().unwrap_or_default());
        if row.agent.host == self.store.own_host() {
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

    /// A client uploads a blob into one of this host's agents.
    pub(crate) fn put_blob(
        &mut self,
        call: CallId,
        request: PutBlobRequest,
        hash: Vec<u8>,
        out: &mut Vec<Output>,
    ) -> Result<(), CallError> {
        let row = self.blob_owner(&request.agent_id)?;
        if row.agent.host != self.store.own_host() {
            return Err(CallError::Blob(BlobRefusal::NotOwn));
        }
        let blob = BlobRef {
            size: request.bytes.len() as u64,
            hash: hash.clone(),
            name: request.name,
            mime: request.mime,
        };
        let dir = self.files_of(&row).join(BLOBS);
        self.write_blob(Writing::Put { call, blob }, dir, hash, request.bytes, out);
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
                answered(call, Response::Peer(Box::new(PeerReply::Blob(blob))), out)
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

#[cfg(test)]
mod tests {
    use store::{AgentKey, AgentRow, Sqlite};

    use super::*;
    use crate::payload::ClientRequest;
    use crate::{Input, StartOptions, StepBudget};

    const OWN: &[u8] = &[1; 16];
    const PEER: &[u8] = &[2; 16];

    fn daemon() -> Daemon {
        let mut store = Sqlite::open_in_memory(OWN.to_vec()).expect("a store opens");
        for (host, agent) in [(OWN, [3; 16]), (PEER, [4; 16])] {
            let key = AgentKey::new(host.to_vec(), agent.to_vec());
            store
                .put_agent(&AgentRow::new(key, 1, "/src"))
                .expect("the store takes the agent");
        }
        Daemon::new(
            store,
            StartOptions {
                agents: "/profile/agents".into(),
                replicas: "/profile/replicas".into(),
                budget: StepBudget::UNLIMITED,
                ..StartOptions::default()
            },
        )
    }

    fn put(agent: [u8; 16]) -> Input {
        Input::ClientCall {
            call: CallId(1),
            request: ClientRequest::PutBlob {
                request: PutBlobRequest {
                    agent_id: agent.to_vec(),
                    name: "notes.txt".into(),
                    mime: "text/plain".into(),
                    bytes: b"notes".to_vec(),
                },
                hash: vec![9; 32],
            },
        }
    }

    #[test]
    fn an_upload_is_answered_only_once_its_write_is_done() {
        let mut daemon = daemon();
        let mut out = Vec::new();
        daemon.step(put([3; 16]), &mut out);
        let [
            Output::Job {
                id,
                op: Job::WriteBlob { dir, hash, bytes },
            },
        ] = out.as_slice()
        else {
            panic!("one write and nothing else: {out:?}");
        };
        assert_eq!(
            dir,
            &PathBuf::from("/profile/agents/03030303-0303-0303-0303-030303030303/blobs")
        );
        assert_eq!(
            (hash.as_slice(), bytes.as_slice()),
            (&[9; 32][..], &b"notes"[..])
        );
        let job = *id;
        out.clear();
        daemon.step(
            Input::JobDone {
                job,
                result: JobResult::Done,
            },
            &mut out,
        );
        let [
            Output::Answer {
                response: Response::Peer(reply),
                ..
            },
            Output::EndCall {
                status: Status::Ok, ..
            },
        ] = out.as_slice()
        else {
            panic!("the reference and the end: {out:?}");
        };
        let PeerReply::Blob(blob) = reply.as_ref() else {
            panic!("a blob reference: {reply:?}");
        };
        assert_eq!((blob.size, blob.name.as_str()), (5, "notes.txt"));
    }

    #[test]
    fn a_replica_takes_no_upload_and_its_reads_come_from_its_hosts_folder() {
        let mut daemon = daemon();
        let mut out = Vec::new();
        daemon.step(put([4; 16]), &mut out);
        assert_eq!(
            out,
            vec![Output::EndCall {
                call: CallId(1),
                status: Status::Failed(CallError::Blob(BlobRefusal::NotOwn)),
            }]
        );
        out.clear();
        let read = GetBlobRequest {
            agent_id: vec![4; 16],
            hash: vec![10; 32],
        };
        daemon.step(
            Input::ClientCall {
                call: CallId(2),
                request: ClientRequest::GetBlob(read),
            },
            &mut out,
        );
        let [Output::SendFile { call, path, .. }] = out.as_slice() else {
            panic!("one file sent: {out:?}");
        };
        assert_eq!(*call, CallId(2));
        assert_eq!(
            path,
            &PathBuf::from(format!(
                "/profile/replicas/02020202-0202-0202-0202-020202020202/agents/\
                 04040404-0404-0404-0404-040404040404/blobs/{}",
                hex(&[10; 32])
            ))
        );
    }
}
