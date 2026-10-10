//! Replaying a dump bundle's three pure stages, each from what the bundle
//! carries: the facts ring through the agent's interpreter from the
//! checkpoint at its oldest segment, the daemon's store slice through the
//! session model as a Subscribe opening would deliver it, and that state
//! through the views. A client bug reproduces from the second and third
//! stages; an interpreter bug from the first, compared with the journal the
//! bundle also carries.
//!
//! The layout is the daemon's (see `node::dump`): `manifest.json`, and per
//! agent `agents/<id>/row.pb`, `store.pb`, `journal/<segment>` and the
//! agent's own part under `part/`, with `facts/<segment>` and
//! `facts/<segment>.checkpoint`.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use interpret::Interpreter;
use interpret::claude_pty::ClaudePty;
use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::Codex;
use interpret::ring::{self, Entry};
use ui_state::{Msg, SessionState};
use ui_view::{ChatOptions, Row};
use wire::{Agent, CaughtUp, Item, Kind, SessionEvent, Step, session_event};

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
    #[error("agent {0} has no facts in the bundle, so there is nothing to replay")]
    NoFacts(String),
    #[error("agent {0}'s oldest facts segment has no checkpoint to start from")]
    NoCheckpoint(String),
    #[error("agent {agent}: facts line {line} is not an event this build can read")]
    Unreadable { agent: String, line: usize },
    #[error("agent {0} is of no kind this build interprets")]
    UnknownKind(String),
}

/// One segment of an agent's facts ring, as the agent's dump part carries
/// it: already redacted, line for line.
#[derive(Clone, Debug)]
pub struct FactsSegment {
    pub start: u64,
    /// The interpreter's state before the segment's first entry.
    pub checkpoint: Option<Vec<u8>>,
    pub entries: Vec<Entry>,
}

/// Everything a bundle holds for one agent.
#[derive(Clone, Debug)]
pub struct AgentDump {
    /// The directory name: the agent id.
    pub id: String,
    /// The inventory row.
    pub row: Agent,
    /// The store slice: the snapshot and the newest rows, oldest first.
    pub store: Step,
    /// Every whole frame of the journal segments carried, in order.
    pub journal: Vec<Step>,
    /// The facts ring's segments, oldest first; empty for an agent that had
    /// no process to ask.
    pub facts: Vec<FactsSegment>,
}

#[derive(Clone, Debug)]
pub struct Bundle {
    pub dir: PathBuf,
    pub agents: Vec<AgentDump>,
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
    let listing = match std::fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(BundleError::Read {
                path: dir.to_owned(),
                error,
            });
        }
    };
    for entry in listing {
        let entry = entry.map_err(|error| BundleError::Read {
            path: dir.to_owned(),
            error,
        })?;
        let name = entry.file_name();
        if let Some(start) = name.to_str().and_then(|name| name.parse::<u64>().ok()) {
            found.insert(start, entry.path());
        }
    }
    Ok(found.into_iter().collect())
}

fn journal_frames(path: &Path) -> Result<Vec<Step>, BundleError> {
    let bytes = read(path)?;
    let mut frames = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        match journal::decode_frame(&bytes[at..]) {
            journal::Frame::Whole(step, length) => {
                frames.push(*step);
                at += length;
            }
            // A dump carries whole frames only; anything else ends the read.
            journal::Frame::Incomplete | journal::Frame::Corrupt => break,
        }
    }
    Ok(frames)
}

impl Bundle {
    pub fn open(dir: &Path) -> Result<Bundle, BundleError> {
        let agents_dir = dir.join("agents");
        let mut ids: Vec<String> = std::fs::read_dir(&agents_dir)
            .map_err(|error| BundleError::Read {
                path: agents_dir.clone(),
                error,
            })?
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .collect();
        ids.sort();
        let mut agents = Vec::new();
        for id in ids {
            let at = agents_dir.join(&id);
            let mut journal = Vec::new();
            for (_, path) in segments(&at.join("journal"))? {
                journal.extend(journal_frames(&path)?);
            }
            let facts_dir = at.join("part").join("facts");
            let mut facts = Vec::new();
            for (start, path) in segments(&facts_dir)? {
                let checkpoint_path = facts_dir.join(format!(
                    "{}.checkpoint",
                    path.file_name().unwrap().to_string_lossy()
                ));
                let checkpoint = match std::fs::read(&checkpoint_path) {
                    Ok(bytes) => Some(bytes),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => {
                        return Err(BundleError::Read {
                            path: checkpoint_path,
                            error,
                        });
                    }
                };
                facts.push(FactsSegment {
                    start,
                    checkpoint,
                    entries: ring::entries(&read(&path)?),
                });
            }
            agents.push(AgentDump {
                row: decode(&at.join("row.pb"), "wire.Agent")?,
                store: decode(&at.join("store.pb"), "wire.Step")?,
                journal,
                facts,
                id,
            });
        }
        Ok(Bundle {
            dir: dir.to_owned(),
            agents,
        })
    }
}

impl AgentDump {
    /// Stage one: the facts ring through the agent's interpreter, from the
    /// checkpoint at the oldest segment kept. Returns every non-empty step
    /// in order, as the agent would have journaled them.
    pub fn replay_facts(&self) -> Result<Vec<Step>, BundleError> {
        match self.row.kind() {
            Kind::ClaudePty => self.replay::<ClaudePty>(),
            Kind::ClaudeSdk => self.replay::<ClaudeSdk>(),
            Kind::Codex => self.replay::<Codex>(),
            Kind::Unspecified => Err(BundleError::UnknownKind(self.id.clone())),
        }
    }

    fn replay<I: Interpreter>(&self) -> Result<Vec<Step>, BundleError> {
        let oldest = self
            .facts
            .first()
            .ok_or_else(|| BundleError::NoFacts(self.id.clone()))?;
        let checkpoint = oldest
            .checkpoint
            .as_ref()
            .ok_or_else(|| BundleError::NoCheckpoint(self.id.clone()))?;
        let mut state: I::State =
            interpret::decode_checkpoint(checkpoint).map_err(|error| BundleError::Decode {
                path: PathBuf::from(format!("{}.checkpoint", oldest.start)),
                what: "checkpoint",
                error: error.to_string(),
            })?;
        let mut steps = Vec::new();
        let mut line = 0;
        for segment in &self.facts {
            for entry in &segment.entries {
                line += 1;
                let event = entry.clone().event().ok_or(BundleError::Unreadable {
                    agent: self.id.clone(),
                    line,
                })?;
                let stepped = I::step(&mut state, event);
                if stepped.step != Step::default() {
                    steps.push(stepped.step);
                }
            }
        }
        Ok(steps)
    }

    /// Stage two: the store slice through the session model, delivered as
    /// a Subscribe opening delivers it: the snapshot, the held rows, then
    /// CaughtUp.
    pub fn session(&self) -> SessionState {
        let mut state = SessionState::new(self.row.clone(), usize::MAX);
        let event = |of| Msg::Event(SessionEvent { of: Some(of) });
        let mut revision = 0;
        if let Some(snapshot) = &self.store.snapshot {
            revision = snapshot.revision;
            state.update(event(session_event::Of::Snapshot(snapshot.clone())));
        }
        for item in &self.store.items {
            revision = revision.max(item.revision);
            state.update(event(session_event::Of::Item(item.clone())));
        }
        state.update(event(session_event::Of::CaughtUp(CaughtUp { revision })));
        state
    }

    /// Stage three: the rows the views draw for everything the state holds.
    pub fn rows(state: &SessionState) -> Vec<Row> {
        let transcript = state.transcript();
        match (transcript.oldest_held(), transcript.head()) {
            (Some(oldest), Some(head)) => {
                ui_view::chat_rows(state, oldest..=head, &ChatOptions::default())
            }
            _ => Vec::new(),
        }
    }
}

/// Each key's final text across a sequence of steps: full items replace
/// what came before, appends extend it. How two sequences are compared
/// when their revisions differ.
pub fn final_texts<'a>(steps: impl IntoIterator<Item = &'a Step>) -> BTreeMap<String, String> {
    let mut texts: BTreeMap<String, String> = BTreeMap::new();
    for step in steps {
        for Item { key, text, .. } in &step.items {
            texts.insert(key.clone(), text.clone());
        }
        for append in &step.appends {
            texts
                .entry(append.key.clone())
                .or_default()
                .push_str(&append.text);
        }
    }
    texts
}
