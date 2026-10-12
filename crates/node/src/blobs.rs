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

use git_facts::{Change, Comparison};
use sha2::{Digest as _, Sha256};
use store::{AgentRow, StoreError};
use uuid::Uuid;
use wire::{
    BlobRef, DiffBase, DiffFile, DiffFileChange, DiffRequest, ErrorCode, GetBlobRequest,
    GetBlobResponse, PutBlobRequest, diff_base,
};

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
}

impl BlobError {
    pub fn to_wire(&self) -> wire::Error {
        let code = match self {
            Self::NoAgent | Self::NoBlob(_) => ErrorCode::NotFound,
            Self::NotOwn | Self::Git(_) => ErrorCode::FailedPrecondition,
            Self::BadHash(_) | Self::NoBase => ErrorCode::InvalidArgument,
            Self::TooLarge { .. } => ErrorCode::ResourceExhausted,
            Self::Write(error) if is_full(error) => ErrorCode::ResourceExhausted,
            Self::Write(_) | Self::Read(_) | Self::Mismatch(_) | Self::Store(_) => {
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

impl ProfileRuntime {
    /// Writes a person's attachment into the named own agent's directory
    /// and returns its reference. The same bytes twice are one file.
    pub async fn put_blob(&self, request: PutBlobRequest) -> Result<BlobRef, BlobError> {
        within_limit(&request)?;
        let row = self.blob_owner(&request.agent_id).await?;
        if row.agent.host != self.host().as_bytes() {
            return Err(BlobError::NotOwn);
        }
        let id = Uuid::from_slice(&row.agent.agent).map_err(|_| BlobError::NoAgent)?;
        let hash = Sha256::digest(&request.bytes).to_vec();
        let dir = self.agent_dir(id).join(agent_dir::BLOBS);
        let bytes = request.bytes;
        let size = bytes.len() as u64;
        let written = hash.clone();
        tokio::task::spawn_blocking(move || write_blob(&dir, &written, &bytes))
            .await
            .map_err(|error| BlobError::Write(io::Error::other(error)))?
            .map_err(BlobError::Write)?;
        Ok(BlobRef {
            size,
            hash,
            name: request.name,
            mime: request.mime,
        })
    }

    /// Reads a blob of an own agent or of a replica this runtime holds. A
    /// replica file not fetched yet is not found here.
    pub async fn get_blob(&self, request: GetBlobRequest) -> Result<GetBlobResponse, BlobError> {
        if request.hash.len() != 32 {
            return Err(BlobError::BadHash(request.hash.len()));
        }
        let row = self.blob_owner(&request.agent_id).await?;
        let path = self.blob_path(&row, &request.hash);
        if row.agent.host != self.host().as_bytes()
            && let Some(blobs) = self.replica_blobs.lock().unwrap().as_mut()
        {
            blobs.touch(&path, self.clock_now());
        }
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(BlobError::NoBlob(hex(&request.hash)));
            }
            Err(error) => return Err(BlobError::Read(error)),
        };
        Ok(GetBlobResponse { bytes })
    }

    /// Keeps bytes of a peer's agent, read from its origin, under that
    /// replica's directory, once they prove to be what `hash` names.
    pub async fn keep_replica_blob(
        &self,
        agent_id: &[u8],
        hash: &[u8],
        bytes: &[u8],
    ) -> Result<(), BlobError> {
        if Sha256::digest(bytes)[..] != *hash {
            return Err(BlobError::Mismatch(hex(hash)));
        }
        let row = self.blob_owner(agent_id).await?;
        if row.agent.host == self.host().as_bytes() {
            return Ok(());
        }
        let path = self.blob_path(&row, hash);
        let dir = path
            .parent()
            .expect("a blob path has a directory")
            .to_owned();
        let (written, owned) = (hash.to_vec(), bytes.to_vec());
        tokio::task::spawn_blocking(move || write_blob(&dir, &written, &owned))
            .await
            .map_err(|error| BlobError::Write(io::Error::other(error)))?
            .map_err(BlobError::Write)?;
        if let Some(blobs) = self.replica_blobs.lock().unwrap().as_mut() {
            blobs.insert(path, bytes.len() as u64, self.clock_now());
        }
        Ok(())
    }

    /// Compares the agent's working directory with `base`: HEAD, or where
    /// the branch left the named base branch, to the working tree with
    /// untracked files. Lists the changed files; only `with_patch` builds
    /// the patch and writes it as the agent's blob. An exited agent's
    /// folder is compared as it is now.
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
