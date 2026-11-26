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
//! daemon does not know keeps its hash.

use std::io;
use std::path::{Path, PathBuf};

use daemon_core::payload::{BlobRefusal, CallError, ClientRequest, Response, Status};
use sha2::{Digest as _, Sha256};
use store::StoreError;
use wire::{BlobRef, ErrorCode, GetBlobRequest, GetBlobResponse, PutBlobRequest};

use crate::api::Owner;
use crate::driver::{Ask, Delivered};
use crate::install::REPLICAS;
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
    /// Writing failed, most often because the disk is full. Nothing is
    /// left behind.
    #[error("writing the blob: {0}")]
    Write(io::Error),
    #[error("reading the blob: {0}")]
    Read(io::Error),
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
            Self::NotOwn => ErrorCode::FailedPrecondition,
            Self::BadHash(_) => ErrorCode::InvalidArgument,
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
        let bytes = self.peer_blob(host, agent_id, hash).await?;
        self.keep_replica_blob(agent_id, hash, &bytes).await?;
        Ok(bytes)
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
}

/// Writes `<dir>/<hex>` all at once ([`os::fs::replace_atomically`]), so a
/// reader sees the whole file or none; the name is the content's hash, so
/// two writers are safe. A failed write leaves no temporary file.
pub(crate) fn write_blob(dir: &Path, hash: &[u8], bytes: &[u8]) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(hex(hash));
    if std::fs::metadata(&path).is_ok_and(|meta| meta.is_file()) {
        return Ok(());
    }
    os::fs::replace_atomically(&path, bytes)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}
