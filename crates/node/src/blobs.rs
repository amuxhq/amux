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

use daemon_core::payload::{BlobRefusal, CallError, ClientRequest, PeerReply, Response, Status};
use git_facts::{Change, Comparison};
use sha2::{Digest as _, Sha256};
use store::{AgentRow, StoreError};
use uuid::Uuid;
use wire::{
    BlobRef, DiffBase, DiffFile, DiffFileChange, DiffRequest, ErrorCode, GetBlobRequest,
    GetBlobResponse, PutBlobRequest, diff_base,
};

use crate::driver::{Ask, Delivered};
use crate::install::{AGENTS, REPLICAS};
use crate::runtime::ProfileRuntime;

/// The mime type of a patch written by Diff.
pub const PATCH_MIME: &str = "text/x-diff";

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
    pub async fn put_blob(&self, request: PutBlobRequest) -> Result<BlobRef, BlobError> {
        within_limit(&request)?;
        let hash = Sha256::digest(&request.bytes).to_vec();
        match self
            .blob_call(ClientRequest::PutBlob { request, hash })
            .await?
        {
            Some(Response::Peer(reply)) => match *reply {
                PeerReply::Blob(blob) => Ok(blob),
                other => Err(answered_with(other)),
            },
            other => Err(answered_with(other)),
        }
    }

    pub async fn get_blob(&self, request: GetBlobRequest) -> Result<GetBlobResponse, BlobError> {
        match self.blob_call(ClientRequest::GetBlob(request)).await? {
            Some(Response::Peer(reply)) => match *reply {
                PeerReply::GetBlob(blob) => Ok(blob),
                other => Err(answered_with(other)),
            },
            other => Err(answered_with(other)),
        }
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
                        },
                        CallError::Store(words) => BlobError::Core(words),
                        other => BlobError::Core(format!("{other:?}")),
                    });
                }
                None => return Err(BlobError::Core("the profile is no longer running".into())),
            }
        }
    }

    pub async fn diff(&self, request: DiffRequest) -> Result<wire::Diff, BlobError> {
        let row = self.blob_owner(&request.agent_id).await?;
        if row.agent.host != self.host().as_bytes() {
            return Err(BlobError::NotOwn);
        }
        let base = request.base.and_then(|base| base.base);
        let (comparison, name) = match &base {
            Some(diff_base::Base::Head(_)) => {
                (Comparison::Uncommitted, "working-tree.diff".to_owned())
            }
            Some(diff_base::Base::Branch(branch)) => (
                Comparison::OnBranch {
                    base: branch.clone(),
                },
                format!("{branch}.diff"),
            ),
            None => return Err(BlobError::NoBase),
        };
        let compared =
            git_facts::compare(Path::new(&row.cwd), &comparison, request.with_patch).await?;
        let patch = match compared.patch {
            Some(bytes) => Some(
                self.put_blob(PutBlobRequest {
                    agent_id: request.agent_id,
                    name,
                    mime: PATCH_MIME.to_owned(),
                    bytes,
                })
                .await?,
            ),
            None => None,
        };
        Ok(wire::Diff {
            patch,
            base: Some(DiffBase { base }),
            head: compared.head.unwrap_or_default(),
            merge_base: compared.merge_base,
            files: compared.files.into_iter().map(diff_file).collect(),
        })
    }

    /// The row of the agent a blob call names, own or replica.
    pub(crate) async fn blob_owner(&self, agent_id: &[u8]) -> Result<AgentRow, BlobError> {
        self.store
            .lock()
            .await
            .agent_by_id(agent_id)?
            .ok_or(BlobError::NoAgent)
    }

    pub(crate) fn blob_path(&self, row: &AgentRow, hash: &[u8]) -> PathBuf {
        self.files_of(row).join(agent_dir::BLOBS).join(hex(hash))
    }

    /// Where an agent's files are on this host: its own directory, or the
    /// replica's.
    pub(crate) fn files_of(&self, row: &AgentRow) -> PathBuf {
        let agent = Uuid::from_slice(&row.agent.agent).unwrap_or_default();
        if row.agent.host == self.host().as_bytes() {
            self.agent_dir(agent)
        } else {
            let host = Uuid::from_slice(&row.agent.host).unwrap_or_default();
            self.dir()
                .join(REPLICAS)
                .join(host.to_string())
                .join(AGENTS)
                .join(agent.to_string())
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
