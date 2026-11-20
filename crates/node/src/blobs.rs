//! Blobs: content-addressed bytes whose only home is their agent's
//! directory, `agents/<id>/blobs/<sha256>` for an own agent and
//! `replicas/<host>/agents/<id>/blobs/<sha256>` for a peer's. There is no
//! table: name, mime and size ride on the attachment that references a
//! blob, and a blob lives and dies with its directory.
//!
//! PutBlob writes a person's attachment into the agent's directory before
//! the input that references it is handed over. The agent process writes
//! its own: images the model read, and its catalogue, whose hash its
//! snapshot names. GetBlob reads a file for a client, and for a peer's
//! agent keeps the origin's exact bytes, so a catalogue with fields this
//! daemon does not know keeps its hash. Diff compares the agent's working directory through git-facts
//! and, only when asked for the patch, writes it as a blob of the agent, so
//! it lives as long as that agent whether or not a review is ever attached.

use std::io;
use std::path::{Path, PathBuf};

/// The mime type of a patch written by Diff.
pub use daemon_core::blobs::PATCH_MIME;
use daemon_core::payload::{BlobRefusal, CallError, ClientRequest, GitRefusal, Response, Status};
use git_facts::{Change, Comparison, GitError};
use sha2::{Digest as _, Sha256};
use store::StoreError;
use uuid::Uuid;
use wire::{
    BlobRef, DiffBase, DiffFile, DiffFileChange, ErrorCode, GetBlobRequest, GetBlobResponse,
    GetDiffRequest, PutBlobRequest, diff_base,
};

use crate::HostId;
use crate::api::Owner;
use crate::driver::{Ask, Delivered};
use crate::install::REPLICAS;
use crate::reach::{ANSWER_PATIENCE, Unreached};
use crate::runtime::ProfileRuntime;

#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error("no agent with that id")]
    NoAgent,
    #[error("a blob is written only into an agent this host runs")]
    NotOwn,
    #[error("a blob hash is 32 bytes, not {0}")]
    BadHash(usize),
    #[error(
        "{name} is {size} bytes, over the {} MB limit for an attachment",
        wire::ATTACHMENT_SIZE_LIMIT / (1024 * 1024)
    )]
    TooLarge { name: String, size: usize },
    #[error("the agent holds no blob {0}")]
    NoBlob(String),
    #[error("a diff needs a base")]
    NoBase,
    /// Writing failed, most often because the disk is full. Nothing is
    /// left behind.
    #[error("writing the blob: {0}")]
    Write(io::Error),
    #[error("reading the blob: {0}")]
    Read(io::Error),
    #[error(transparent)]
    Git(#[from] git_facts::GitError),
    #[error("the bytes a peer sent are not the blob {0}")]
    Mismatch(String),
    /// A file of another host's agent this node has not fetched yet, and
    /// that host could not be asked for it: nothing left this host.
    #[error("{}", .0.message)]
    Unreached(wire::Error),
    /// The agent's host was asked for the file and refused, or stopped
    /// answering: its own words, or the uncertainty.
    #[error("{}", .0.message)]
    Owner(wire::Error),
    #[error("the store: {0}")]
    Store(#[from] StoreError),
    #[error("the daemon core: {0}")]
    Core(String),
}

impl BlobError {
    pub fn to_wire(&self) -> wire::Error {
        let code = match self {
            Self::NoAgent | Self::NoBlob(_) => ErrorCode::NotFound,
            Self::NotOwn | Self::Git(_) => ErrorCode::FailedPrecondition,
            Self::BadHash(_) | Self::NoBase => ErrorCode::InvalidArgument,
            Self::TooLarge { .. } => ErrorCode::ResourceExhausted,
            Self::Write(error) if is_full(error) => ErrorCode::ResourceExhausted,
            Self::Write(_) | Self::Read(_) | Self::Mismatch(_) | Self::Store(_) | Self::Core(_) => {
                ErrorCode::Internal
            }
            Self::Unreached(error) | Self::Owner(error) => return error.clone(),
        };
        wire::Error {
            code: code as i32,
            message: self.to_string(),
            details: Vec::new(),
        }
    }
}

/// Refuses an attachment over [`wire::ATTACHMENT_SIZE_LIMIT`], naming the
/// limit. Clients check first; this is the daemon's own word on it, before
/// the bytes go anywhere.
pub(crate) fn within_limit(request: &PutBlobRequest) -> Result<(), BlobError> {
    if request.bytes.len() > wire::ATTACHMENT_SIZE_LIMIT {
        return Err(BlobError::TooLarge {
            name: if request.name.is_empty() {
                "the attachment".to_owned()
            } else {
                request.name.clone()
            },
            size: request.bytes.len(),
        });
    }
    Ok(())
}

/// Why `name`, the host holding a file, could not be asked for it, as the
/// wire says it: the reason a client tells apart, in words a person reads.
fn unreached(name: &str, why: Unreached) -> wire::Error {
    match why {
        Unreached::Unreachable => wire::Error::because(
            wire::ErrorReason::HostUnreachable,
            format!("{name} holds the file and cannot be reached"),
        ),
        Unreached::Lapsed => wire::Error::because(
            wire::ErrorReason::PaymentRequired,
            format!(
                "{name} holds the file and can be reached only through the relay, and this \
                 account's subscription has lapsed to the free plan, which carries no calls \
                 between hosts; renew it to reach {name}"
            ),
        ),
        Unreached::Revoked => wire::Error::new(
            ErrorCode::Unauthenticated,
            format!("{name} holds the file and no longer trusts this host"),
        ),
    }
}

fn is_full(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded
    )
}

fn answered_with(answer: impl std::fmt::Debug) -> BlobError {
    BlobError::Core(format!("a blob call was answered with {answer:?}"))
}

impl ProfileRuntime {
    /// Writes a person's attachment into the named own agent's directory
    /// and returns its reference. The same bytes twice are one file.
    pub async fn put_blob(&self, mut request: PutBlobRequest) -> Result<BlobRef, BlobError> {
        within_limit(&request)?;
        let bytes = std::mem::take(&mut request.bytes);
        let hash = Sha256::digest(&bytes).to_vec();
        match self
            .blob_call(ClientRequest::PutBlob {
                request,
                hash,
                bytes,
            })
            .await?
        {
            Some(Response::Written(blob)) => Ok(blob),
            other => Err(answered_with(other)),
        }
    }

    pub async fn get_blob(&self, request: GetBlobRequest) -> Result<GetBlobResponse, BlobError> {
        match self.blob_call(ClientRequest::GetBlob(request)).await? {
            Some(Response::Bytes(bytes)) => Ok(GetBlobResponse { bytes }),
            other => Err(answered_with(other)),
        }
    }

    /// A file a conversation this node holds references: read from the
    /// agent's directory here, own or copy. A file of another host's agent
    /// that this node has not fetched yet is the copy filling itself: this
    /// node asks that host's peer service for it as itself, so the reader's
    /// call is never passed on, checks the bytes against the hash they are
    /// named by, and keeps them for the next reader.
    pub(crate) async fn read_through_blob(
        &self,
        agent_id: &[u8],
        hash: &[u8],
    ) -> Result<Vec<u8>, BlobError> {
        let request = GetBlobRequest {
            agent_id: agent_id.to_vec(),
            hash: hash.to_vec(),
        };
        match self.get_blob(request).await {
            Err(BlobError::NoBlob(_)) => {}
            read => return read.map(|blob| blob.bytes),
        }
        let host = match self.owner(agent_id).await? {
            Owner::Peer(host) => host,
            Owner::Here | Owner::Unknown => return Err(BlobError::NoBlob(hex(hash))),
        };
        let bytes = self.fetch_blob(host, agent_id, hash).await?;
        self.keep_replica_blob(agent_id, hash, &bytes).await?;
        Ok(bytes)
    }

    /// Asks `host`'s peer service for a file of one of its agents. The call
    /// carries no caller: it is this daemon asking for its own copy.
    async fn fetch_blob(
        &self,
        host: HostId,
        agent_id: &[u8],
        hash: &[u8],
    ) -> Result<Vec<u8>, BlobError> {
        let channel = self
            .reach(host)
            .await
            .map_err(|why| BlobError::Unreached(unreached(&self.host_name(host), why)))?;
        let mut owner = wire::amux::staging::peer_service_client(channel);
        let asked = owner.get_blob(wire::amux::staging::PeerServiceGetBlobRequest {
            agent_id: agent_id.to_vec(),
            hash: hash.to_vec(),
        });
        let stopped = || {
            wire::Error::new(
                ErrorCode::Aborted,
                format!(
                    "{} stopped answering while sending a file",
                    self.host_name(host)
                ),
            )
        };
        let answered = match tokio::time::timeout(ANSWER_PATIENCE, asked).await {
            Ok(answered) => answered,
            Err(_) => return Err(BlobError::Owner(stopped())),
        };
        answered
            .map(|answer| answer.into_inner().bytes)
            .map_err(|status| {
                BlobError::Owner(match crate::net_error::from_status(&status) {
                    Some(error) => error,
                    None => match status.code() {
                        tonic::Code::Unimplemented => {
                            wire::Error::new(ErrorCode::Unimplemented, wire::HOST_NEEDS_UPDATE)
                        }
                        tonic::Code::Unavailable
                        | tonic::Code::Unknown
                        | tonic::Code::Cancelled
                        | tonic::Code::DeadlineExceeded => stopped(),
                        // A status code is the wire's error code.
                        code => wire::Error {
                            code: code as i32,
                            message: status.message().to_owned(),
                            details: Vec::new(),
                        },
                    },
                })
            })
    }

    /// Keeps the bytes of a replica's blob its origin sent, once they are
    /// shown to be the blob.
    pub async fn keep_replica_blob(
        &self,
        agent_id: &[u8],
        hash: &[u8],
        bytes: &[u8],
    ) -> Result<(), BlobError> {
        if Sha256::digest(bytes)[..] != *hash {
            return Err(BlobError::Mismatch(hex(hash)));
        }
        let request = ClientRequest::KeepBlob {
            agent_id: agent_id.to_vec(),
            hash: hash.to_vec(),
            bytes: bytes.to_vec(),
        };
        self.blob_call(request).await.map(|_| ())
    }

    /// A blob file was read: a replica's counts as used now.
    pub(crate) fn read_blob(&self, path: &Path) {
        if path.starts_with(self.dir().join(REPLICAS))
            && let Some(blobs) = self.replica_blobs.lock().unwrap().as_mut()
        {
            blobs.touch(path, self.clock_now());
        }
    }

    /// A blob file was written: a replica's is indexed for the sweep.
    pub(crate) fn kept_blob(&self, path: PathBuf, size: u64) {
        if path.starts_with(self.dir().join(REPLICAS))
            && let Some(blobs) = self.replica_blobs.lock().unwrap().as_mut()
        {
            blobs.insert(path, size, self.clock_now());
        }
    }

    /// Hands a blob call to the core and waits for its end.
    async fn blob_call(&self, request: ClientRequest) -> Result<Option<Response>, BlobError> {
        let mut answers = self.driver().call(Ask::Ready(request), 1).await;
        let mut answer = None;
        loop {
            match answers.next().await {
                Some(Delivered::Answer(response)) => answer = Some(response),
                Some(Delivered::End(Status::Ok)) => return Ok(answer),
                Some(Delivered::End(Status::Failed(error))) => {
                    return Err(match error {
                        CallError::NoAgent => BlobError::NoAgent,
                        CallError::NoBlob(hash) => BlobError::NoBlob(hash),
                        CallError::Blob(refusal) => match refusal {
                            BlobRefusal::NotOwn => BlobError::NotOwn,
                            BlobRefusal::BadHash(len) => BlobError::BadHash(len),
                            BlobRefusal::TooLarge { name, size } => BlobError::TooLarge {
                                name,
                                size: usize::try_from(size).unwrap_or(usize::MAX),
                            },
                            BlobRefusal::Write { full, words } => BlobError::Write(if full {
                                io::Error::new(io::ErrorKind::StorageFull, words)
                            } else {
                                io::Error::other(words)
                            }),
                            BlobRefusal::Read(words) => BlobError::Read(io::Error::other(words)),
                            BlobRefusal::NoBase => BlobError::NoBase,
                            BlobRefusal::Git(refusal) => BlobError::Git(match refusal {
                                GitRefusal::NotARepository => GitError::NotARepository,
                                GitRefusal::NoForkPoint(base) => GitError::NoForkPoint(base),
                                GitRefusal::Failed(words) => GitError::Failed {
                                    command: "diff".to_owned(),
                                    stderr: words,
                                },
                            }),
                        },
                        CallError::Store(words) => BlobError::Core(words),
                        other => BlobError::Core(format!("{other:?}")),
                    });
                }
                None => return Err(BlobError::Core("the profile is no longer running".into())),
            }
        }
    }

    pub async fn get_diff(&self, request: GetDiffRequest) -> Result<wire::Diff, BlobError> {
        match self.blob_call(ClientRequest::GetDiff(request)).await? {
            Some(Response::Diff(diff)) => Ok(*diff),
            other => Err(answered_with(other)),
        }
    }
}

/// Writes `<dir>/<hex>` by temp-and-rename, so a reader sees the whole file
/// or none; the name is the content's hash, so two writers are safe. A
/// failed write removes its temporary file.
pub(crate) fn write_blob(dir: &Path, hash: &[u8], bytes: &[u8]) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let name = hex(hash);
    let path = dir.join(&name);
    if std::fs::metadata(&path).is_ok_and(|meta| meta.is_file()) {
        return Ok(());
    }
    let temp = dir.join(format!(".{name}.{}", Uuid::new_v4().simple()));
    let written = std::fs::write(&temp, bytes).and_then(|()| std::fs::rename(&temp, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn diff_file(file: git_facts::FileChange) -> DiffFile {
    let change = match file.change {
        Change::Created => DiffFileChange::Created,
        Change::Deleted => DiffFileChange::Deleted,
        Change::Changed => DiffFileChange::Changed,
    };
    DiffFile {
        path: file.path,
        added: file.added,
        removed: file.removed,
        change: change as i32,
        binary: file.binary,
    }
}

/// Compares a working tree for the core's diff: the diff without its patch,
/// and the patch with its hash.
pub(crate) async fn compare(
    cwd: &Path,
    base: diff_base::Base,
    with_patch: bool,
) -> daemon_core::payload::JobResult {
    use daemon_core::payload::{JobResult, Patch};
    let comparison = match &base {
        diff_base::Base::Head(_) => Comparison::Uncommitted,
        diff_base::Base::Branch(branch) => Comparison::OnBranch {
            base: branch.clone(),
        },
    };
    match git_facts::compare(cwd, &comparison, with_patch).await {
        Ok(compared) => JobResult::Compared {
            diff: wire::Diff {
                patch: None,
                base: Some(DiffBase { base: Some(base) }),
                head: compared.head.unwrap_or_default(),
                merge_base: compared.merge_base,
                files: compared.files.into_iter().map(diff_file).collect(),
                // A count that fails leaves it unknown, which a delete's
                // confirmation says as a branch kept only if it has any.
                own_commits: git_facts::own_commits(cwd).await.ok().flatten(),
            },
            patch: compared.patch.map(|bytes| Patch {
                hash: Sha256::digest(&bytes).to_vec(),
                bytes,
            }),
        },
        Err(error) => JobResult::CompareFailed(match error {
            GitError::NotARepository => GitRefusal::NotARepository,
            GitError::NoForkPoint(base) => GitRefusal::NoForkPoint(base),
            other => GitRefusal::Failed(other.to_string()),
        }),
    }
}
