//! Opening a dump bundle: the records it carries, each as a stage replays
//! it. The layout is the daemon's (see `node::dump`): `manifest.json`; the
//! daemon core's record under `core/`, its `checkpoint`, the `inputs`
//! since, the `store.sqlite` as of that checkpoint and each own agent's
//! journal from its cursor there under `journals/<id>/`; each own agent's
//! `agents/<id>/record`; and the record of each chat the asking client
//! showed, under `client/sessions/<id>/`: its `checkpoint` and the
//! `events` since, which the manifest names.
//!
//! An agent's record is read as it was written, each event's skeleton
//! and the interpreter's conclusions by kind, size and alias; the agent
//! stage turns it back into a stream for its interpreter
//! ([`crate::synthetic`]).
//!
//! A bundle opens from its directory, from a person's report holding it,
//! or from the `dump.pb.zst` a report was sent with.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use daemon_core::AgentId;
use prost::Message as _;
use wire::Step;
use wire::dump::{ClientSession, ClientSessionEvents, ClientTraced};

/// The core's record in a bundle, and its files.
pub const CORE: &str = "core";
pub const CORE_CHECKPOINT: &str = "checkpoint";
pub const CORE_INPUTS: &str = "inputs";
pub const CORE_STORE: &str = "store.sqlite";
pub const CORE_JOURNALS: &str = "journals";
/// Where each agent's record is.
pub const AGENTS: &str = "agents";
pub const AGENT_RECORD: &str = "record";

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("{path}: {error}")]
    Read { path: PathBuf, error: io::Error },
    #[error("{path}: not a {what}: {error}")]
    Decode {
        path: PathBuf,
        what: &'static str,
        error: String,
    },
}

/// One own agent's record, as its process built it when the dump was
/// made.
#[derive(Clone, Debug)]
pub struct AgentRecord {
    /// The directory name: the agent id.
    pub id: String,
    pub record: serde_json::Value,
}

/// A journal's segment files: each one's start offset and its bytes.
pub type Segments = Vec<(u64, Vec<u8>)>;

/// The daemon core's record: its newest checkpoint, every line its driver
/// recorded since (each input, and how far each ingest read a journal),
/// the store as of that checkpoint and the journals those reads reach.
#[derive(Clone, Debug)]
pub struct CoreDump {
    pub checkpoint: Vec<u8>,
    pub inputs: Vec<daemon_core::Recorded>,
    pub store: PathBuf,
    /// Each own agent's journal segments, at their offsets.
    pub journals: BTreeMap<AgentId, Segments>,
}

/// One chat the asking client showed, as the manifest names it.
#[derive(Clone, Debug)]
pub struct ClientDump {
    /// The agent id, as the bundle writes ids.
    pub agent: String,
    /// The session, or why its files cannot be read.
    pub history: Result<ClientHistory, String>,
}

/// A client session's state when its trace began, and every event its
/// driver saw since.
#[derive(Clone, Debug)]
pub struct ClientHistory {
    pub start: ClientSession,
    pub events: Vec<ClientTraced>,
}

#[derive(Clone, Debug)]
pub struct Bundle {
    /// Where the bundle's files are: the directory named, the `dump/` of a
    /// person's report, or where a sent archive was unpacked.
    pub dir: PathBuf,
    /// An unpacked archive's directory, removed with the last clone.
    _unpacked: Option<Arc<tempfile::TempDir>>,
    /// Each own agent's record; an agent whose record could not be had is
    /// named in the manifest's errors instead.
    pub agents: Vec<AgentRecord>,
    /// None when the core's record could not be had or was past the
    /// limit, as the manifest says.
    pub core: Option<CoreDump>,
    /// Empty when no client showed a chat: a dump from the command line.
    pub clients: Vec<ClientDump>,
}

pub(crate) fn read(path: &Path) -> Result<Vec<u8>, BundleError> {
    std::fs::read(path).map_err(|error| BundleError::Read {
        path: path.to_owned(),
        error,
    })
}

pub(crate) fn decode<M: prost::Message + Default>(
    path: &Path,
    what: &'static str,
) -> Result<M, BundleError> {
    M::decode(read(path)?.as_slice()).map_err(|error| BundleError::Decode {
        path: path.to_owned(),
        what,
        error: error.to_string(),
    })
}

/// Segment files in a directory, by the offset their name spells.
pub(crate) fn segments(dir: &Path) -> Result<Vec<(u64, PathBuf)>, BundleError> {
    let mut found = BTreeMap::new();
    for (name, path) in listing(dir)? {
        if let Ok(start) = name.parse::<u64>() {
            found.insert(start, path);
        }
    }
    Ok(found.into_iter().collect())
}

/// A directory's entries by name, none when it is not there.
pub(crate) fn listing(dir: &Path) -> Result<Vec<(String, PathBuf)>, BundleError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(BundleError::Read {
                path: dir.to_owned(),
                error,
            });
        }
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| BundleError::Read {
            path: dir.to_owned(),
            error,
        })?;
        if let Ok(name) = entry.file_name().into_string() {
            names.push((name, entry.path()));
        }
    }
    names.sort();
    Ok(names)
}

/// Every whole frame of a journal segment, in order.
pub(crate) fn journal_frames(bytes: &[u8]) -> Vec<Step> {
    let mut frames = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        match journal::decode_frame(&bytes[at..]) {
            journal::Frame::Whole(step, length) => {
                frames.push(*step);
                at += length;
            }
            // A torn tail keeps only its length: it ends the read.
            journal::Frame::Incomplete | journal::Frame::Corrupt => break,
        }
    }
    frames
}

/// Where a bundle's files are, given what was named: a dump's directory
/// as written, a person's report (its `dump/`, or the `dump.pb.zst` it was
/// sent with), or a `dump.pb.zst` downloaded on its own, unpacked into a
/// directory that lasts as long as the returned guard.
pub fn locate(named: &Path) -> Result<(PathBuf, Option<Arc<tempfile::TempDir>>), BundleError> {
    let archive = if named.is_file() {
        named.to_owned()
    } else if named.join(DUMP_DIR).is_dir() {
        return Ok((named.join(DUMP_DIR), None));
    } else if named.join(ARCHIVE).is_file() {
        named.join(ARCHIVE)
    } else {
        return Ok((named.to_owned(), None));
    };
    let unreadable = |error: String| BundleError::Decode {
        path: archive.clone(),
        what: "a packed dump",
        error,
    };
    let packed = unpacked_at_most(&read(&archive)?, UNPACKED_LIMIT).map_err(unreadable)?;
    let part =
        wire::DumpPart::decode(packed.as_slice()).map_err(|error| unreadable(error.to_string()))?;
    let unpacked = tempfile::tempdir().map_err(|error| BundleError::Read {
        path: archive.clone(),
        error,
    })?;
    for file in part.files {
        let Some(path) = under(unpacked.path(), &file.name) else {
            return Err(unreadable(format!("a file named {:?}", file.name)));
        };
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, &file.contents));
        written.map_err(|error| BundleError::Read { path, error })?;
    }
    Ok((unpacked.path().to_owned(), Some(Arc::new(unpacked))))
}

/// A person's report keeps its dump here, and sends it packed as this.
const DUMP_DIR: &str = "dump";
const ARCHIVE: &str = "dump.pb.zst";
/// The most a packed dump unpacks to. A report is at most 8 MiB packed,
/// and its records, their text zero bytes of its length, pack many times
/// smaller than they unpack; past this only an archive that is not a
/// report reaches.
pub const UNPACKED_LIMIT: u64 = 1 << 30;

/// `packed` decompressed, or why not: an archive that unpacks past `limit`
/// is refused before it is held whole.
pub fn unpacked_at_most(packed: &[u8], limit: u64) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let decoder = zstd::stream::Decoder::new(packed).map_err(|error| error.to_string())?;
    let mut unpacked = Vec::new();
    decoder
        .take(limit + 1)
        .read_to_end(&mut unpacked)
        .map_err(|error| error.to_string())?;
    if unpacked.len() as u64 > limit {
        return Err(format!("it unpacks past {limit} bytes"));
    }
    Ok(unpacked)
}

/// `relative` under `dir`, when it is a relative path that stays there:
/// a name a bundle or its manifest supplies never reaches outside it.
pub fn under(dir: &Path, relative: &str) -> Option<PathBuf> {
    let path = Path::new(relative);
    (!relative.is_empty()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_))))
    .then(|| dir.join(path))
}

impl Bundle {
    /// The bundle `named` holds; see [`locate`].
    pub fn open(named: &Path) -> Result<Bundle, BundleError> {
        let (dir, unpacked) = locate(named)?;
        let dir = dir.as_path();
        let mut agents = Vec::new();
        for (id, at) in listing(&dir.join(AGENTS))? {
            let path = at.join(AGENT_RECORD);
            if !path.is_file() {
                continue;
            }
            let record =
                serde_json::from_slice(&read(&path)?).map_err(|error| BundleError::Decode {
                    path: path.clone(),
                    what: "JSON record",
                    error: error.to_string(),
                })?;
            agents.push(AgentRecord { id, record });
        }
        let core_dir = dir.join(CORE);
        let core = if core_dir.join(CORE_CHECKPOINT).is_file() {
            let mut journals = BTreeMap::new();
            for (name, at) in listing(&core_dir.join(CORE_JOURNALS))? {
                let Some(agent) = id(&name).map(AgentId) else {
                    continue;
                };
                let mut held = Vec::new();
                for (start, path) in segments(&at)? {
                    held.push((start, read(&path)?));
                }
                journals.insert(agent, held);
            }
            Some(CoreDump {
                checkpoint: read(&core_dir.join(CORE_CHECKPOINT))?,
                inputs: core_ring::inputs(&read(&core_dir.join(CORE_INPUTS))?),
                store: core_dir.join(CORE_STORE),
                journals,
            })
        } else {
            None
        };
        Ok(Bundle {
            dir: dir.to_owned(),
            _unpacked: unpacked,
            agents,
            core,
            clients: clients(dir)?,
        })
    }
}

/// The client sessions the manifest names, each read or the reason it
/// cannot be.
fn clients(dir: &Path) -> Result<Vec<ClientDump>, BundleError> {
    let path = dir.join("manifest.json");
    let manifest: serde_json::Value = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| BundleError::Decode {
            path: path.clone(),
            what: "manifest",
            error: error.to_string(),
        })?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(BundleError::Read { path, error }),
    };
    let named = manifest["client"]["sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    Ok(named
        .iter()
        .map(|entry| {
            let field = |name: &str| entry[name].as_str().unwrap_or_default().to_owned();
            ClientDump {
                agent: field("agent_id"),
                history: client_history(dir, &field("checkpoint"), &field("events")),
            }
        })
        .collect())
}

fn client_history(dir: &Path, checkpoint: &str, events: &str) -> Result<ClientHistory, String> {
    if checkpoint.is_empty() || events.is_empty() {
        return Err("the manifest names no checkpoint or no events".to_owned());
    }
    let within = |named: &str| {
        under(dir, named).ok_or_else(|| format!("the manifest names {named:?}, outside the bundle"))
    };
    let start: ClientSession =
        decode(&within(checkpoint)?, "wire.ClientSession").map_err(|error| error.to_string())?;
    let events: ClientSessionEvents =
        decode(&within(events)?, "wire.ClientSessionEvents").map_err(|error| error.to_string())?;
    Ok(ClientHistory {
        start,
        events: events.traced,
    })
}

/// An id from the hyphenated form a bundle writes ids in.
pub(crate) fn id(name: &str) -> Option<[u8; 16]> {
    let hex: String = name.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.is_ascii() {
        return None;
    }
    let mut id = [0; 16];
    for (at, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[at * 2..at * 2 + 2], 16).ok()?;
    }
    Some(id)
}
