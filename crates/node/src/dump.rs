//! Dumps: one bundle with everything the host side knows about a profile's
//! agents, redacted before it is written.
//!
//! The daemon interprets nothing, with this one stated exception: only the
//! per-kind code in `interpret` can decode a body, so the dump path calls
//! its redactor on every record it writes. Each agent process redacts its
//! own part (the facts ring, its checkpoints and the specs) before it
//! leaves the process.
//!
//! A bundle is a directory under the installation's `reports/`, assembled
//! beside it under a dotted name and renamed into place once complete:
//!
//! ```text
//! reports/dump-<unix ms>-<id>/
//!   manifest.json            what was dumped, when, why, by which daemon, and
//!                            every file or part that could not be gathered
//!   daemon.log               the tail of the daemon's own log and its .1,
//!                            redacted as text
//!   supervisor.log           the same of the supervisor's, when there is one
//!   agents/<agent_id>/
//!     row.pb                 the inventory row, a wire.Agent
//!     store.pb               the store slice: one wire.Step holding the
//!                            snapshot and the newest rows, oldest first, with
//!                            their orders and revisions
//!     journal/<segment>      the journal from the segment holding the core's
//!                            checkpoint's cursor on, at least the last two
//!                            segments; each frame redacted and written to
//!                            its original length, so every offset holds,
//!                            and a torn tail kept as its length alone
//!                            (own agents)
//!     part/...               the agent process's part, as it sent it:
//!                            facts/<segment>, facts/<segment>.checkpoint,
//!                            spec.<n>, the tails of agent.log and
//!                            provider.log, and dump-errors when it had any. An
//!                            agent with no process has only its specs here,
//!                            redacted by the daemon
//!   client/sessions/<agent_id>/
//!                            each session the asking client had open, for a
//!                            replay of the client:
//!     checkpoint             the session's state when its trace began, a
//!                            wire.ClientSession, redacted
//!     events                 every event its driver saw since, through the
//!                            one that broke it when one did, a
//!                            wire.ClientSessionEvents, redacted
//!   core/                    the daemon core, for a replay of it:
//!     checkpoint             its newest checkpoint, redacted
//!     inputs                 every input stepped since, and what each
//!                            ingest read of a journal, one JSON line each
//!                            (daemon_core::Recorded), redacted
//!     store.sqlite           the store as of that checkpoint, redacted
//! ```
//!
//! A bundle holds only what this host has: an agent another host runs has
//! its row and store slice here and nothing more. Which of these parts a
//! bundle carries is its scope's ([`Gather`]): each trigger carries what
//! replays the stage it concerns.
//!
//! Everything protobuf is encoded without a length prefix except journal
//! segments, which keep the journal's own framing. [`pack`] turns a bundle
//! into one `wire.DumpPart` whose files carry these relative paths, which is
//! how a bundle is uploaded.

use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use core_ring::DumpKind;
use daemon_core::Recorded;
use interpret::RedactTarget;
use prost::Message as _;
use store::AgentRow;
use tokio::sync::oneshot;
use uuid::Uuid;
use wire::dump::ClientSessionHistory;
use wire::{
    AgentDumpRequest, CtlFrame, DumpFile, DumpPart, DumpRequest, DumpScope, Kind, Step, ctl_frame,
};

use crate::blobs::hex;
use crate::driver::{AGENT_DUMP, AGENT_VIOLATION_DUMP, Captured};
use crate::runtime::{ProfileRuntime, to_wire};

/// The rows of each agent a dump carries: the newest, which is where a
/// problem being reported lives.
pub const DUMP_ROWS: u32 = 1_000;
/// The journal segments a dump carries at least: the daemon keeps the last
/// two ingested ones for exactly this.
pub const DUMP_SEGMENTS: usize = 2;
/// Where the core's part of a bundle lives, and its files.
pub const CORE: &str = "core";
pub const CORE_CHECKPOINT: &str = "checkpoint";
pub const CORE_INPUTS: &str = "inputs";
pub const CORE_STORE: &str = "store.sqlite";
/// Where a client's sessions live in a bundle, one directory per agent, and
/// each one's files.
pub const CLIENT_SESSIONS: &str = "client/sessions";
pub const CLIENT_CHECKPOINT: &str = "checkpoint";
pub const CLIENT_EVENTS: &str = "events";
/// How often, and how far apart, a dump asks for the core again while its
/// store's main file is being checkpointed.
const CAPTURE_TRIES: u32 = 30;
const CAPTURE_APART: Duration = Duration::from_millis(100);
/// How much of each of the installation's logs a dump carries, from its
/// end: a few thousand lines, which reach back past the minute or so the
/// core's inputs cover without dwarfing the parts that replay.
pub const DUMP_LOG_BYTES: u64 = 1 << 20;
/// How long an agent process has to send its part.
const PART_PATIENCE: Duration = Duration::from_secs(10);

pub const MANIFEST: &str = "manifest.json";
pub const DAEMON_LOG: &str = "daemon.log";
pub const SUPERVISOR_LOG: &str = "supervisor.log";

/// What a bundle of one scope gathers: what replays the stage its trigger
/// concerns, and nothing it does not need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Gather {
    /// The core's checkpoint, the inputs since and the store copy.
    core: bool,
    slices: Slices,
    journals: Journals,
    /// The agent process's part: its facts ring, checkpoints, specs and
    /// log tails.
    parts: bool,
    /// The daemon's and the supervisor's log tails.
    logs: bool,
}

/// Whose store slice a bundle carries. Every covered agent has its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slices {
    All,
    /// Only agents a client session in the request is for.
    Sessions,
    None,
}

/// How much of each own agent's journal a bundle carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Journals {
    None,
    /// From the segment holding the core checkpoint's cursor: what the
    /// core's replay reads.
    FromCursor,
    /// That, and at least the last [`DUMP_SEGMENTS`]: what the facts ring
    /// is compared with.
    Tail,
}

impl Gather {
    fn of(scope: DumpScope) -> Self {
        match scope {
            DumpScope::Unspecified => Gather {
                core: true,
                slices: Slices::All,
                journals: Journals::Tail,
                parts: true,
                logs: true,
            },
            DumpScope::Core => Gather {
                core: true,
                slices: Slices::None,
                journals: Journals::FromCursor,
                parts: false,
                logs: true,
            },
            DumpScope::Agent => Gather {
                core: false,
                slices: Slices::All,
                journals: Journals::Tail,
                parts: true,
                logs: false,
            },
            DumpScope::Transcript => Gather {
                core: false,
                slices: Slices::All,
                journals: Journals::None,
                parts: false,
                logs: false,
            },
            DumpScope::Report => Gather {
                core: true,
                slices: Slices::Sessions,
                journals: Journals::FromCursor,
                parts: false,
                logs: true,
            },
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DumpError {
    #[error("no agent {0}")]
    NoAgent(String),
    #[error("writing the bundle: {0}")]
    Write(#[from] io::Error),
    #[error("the store: {0}")]
    Store(#[from] store::StoreError),
    #[error(transparent)]
    Stopped(#[from] crate::runtime::Stopped),
}

impl DumpError {
    pub fn to_wire(&self) -> wire::Error {
        let code = match self {
            Self::NoAgent(_) => wire::ErrorCode::NotFound,
            Self::Write(_) | Self::Store(_) => wire::ErrorCode::Internal,
            Self::Stopped(_) => wire::ErrorCode::Unavailable,
        };
        wire::Error {
            code: code as i32,
            message: self.to_string(),
            details: Vec::new(),
        }
    }
}

impl ProfileRuntime {
    /// Writes one bundle of the request's scope for its agents (every
    /// agent of the profile when it names none) and returns its directory.
    /// What cannot be gathered is named in the manifest, never included
    /// unredacted. An automatic one a check asked for is sent on when the
    /// installation sends automatic reports.
    pub async fn dump(self: &Arc<Self>, request: DumpRequest) -> Result<PathBuf, DumpError> {
        let rows = self.dump_rows(&request.agent_ids).await?;
        let bundle = self.write_dump(&request, rows).await?;
        if request.automatic {
            if request.scope() == DumpScope::Transcript {
                self.check_failed(crate::reports::Core::Transcript, &request.check, 0);
            }
            self.send_automatic(&bundle, &request);
        }
        Ok(bundle)
    }

    /// Writes an automatic bundle of `scope` for `agent_ids` (every agent
    /// when empty) off the caller, for `whose` asking: under the guard a
    /// shutdown waits on, so none starts once the profile stops waiting.
    pub(crate) fn dump_unasked(
        self: &Arc<Self>,
        scope: DumpScope,
        agent_ids: Vec<Vec<u8>>,
        reason: String,
        check: String,
        whose: &'static str,
    ) {
        let mut dumps = self.core_dumps.lock().unwrap();
        // A profile shutting down has stopped waiting for dumps: one
        // started now would hold the runtime past its shutdown.
        let Some(dumps) = dumps.as_mut() else {
            tracing::warn!(%reason, whose, "not dumped: the profile is stopping");
            return;
        };
        let weak = Arc::downgrade(self);
        let task = tokio::spawn(async move {
            let Some(runtime) = weak.upgrade() else {
                return;
            };
            let request = DumpRequest {
                agent_ids,
                reason,
                automatic: true,
                sessions: Vec::new(),
                scope: scope.into(),
                check,
            };
            match runtime.dump(request).await {
                Ok(bundle) => tracing::warn!(bundle = %bundle.display(), whose, "dumped"),
                Err(error) => tracing::error!(%error, whose, "dumping failed"),
            }
        });
        dumps.retain(|dump| !dump.is_finished());
        dumps.push(task);
    }

    /// An agent's core asked for a dump over its control connection: a
    /// bundle of that agent, whose facts ring and checkpoints replay as the
    /// interpreter stage and the daemon's parts as the core's, after this
    /// host's own limit per kind. The agent's lifecycle state is not a
    /// replay stage; the reason carries what its core said of it.
    pub(crate) fn agent_asked(self: &Arc<Self>, id: Uuid, asked: wire::DumpAsked) {
        // An agent from before checks had ids names its check by sentence.
        let check = if asked.invariant.is_empty() {
            String::new()
        } else if core_ring::Check::is_id(&asked.check) {
            asked.check.clone()
        } else {
            crate::reports::UNNAMED_AGENT_CHECK.to_owned()
        };
        let (kind, prefix) = if check.is_empty() {
            (DumpKind::OverBudget, AGENT_DUMP)
        } else {
            (
                DumpKind::Violation(check.clone().into()),
                AGENT_VIOLATION_DUMP,
            )
        };
        let now = self.clock_now().max(0) as u64;
        let Some(held_back) = self.agent_dumps.lock().unwrap().admit(kind, now) else {
            return;
        };
        // What this host held back across its agents, and what the agent's
        // own limit held back before asking.
        let core = crate::reports::Core::Agent;
        self.check_failed(core, &check, held_back + u64::from(asked.held_back));
        let reason =
            core_ring::with_suppressed(format!("{prefix}{id}: {}", asked.reason), held_back);
        self.dump_unasked(
            DumpScope::Agent,
            vec![id.as_bytes().to_vec()],
            reason,
            crate::reports::check_id(core, &check),
            "an agent's core",
        );
    }

    async fn write_dump(
        &self,
        request: &DumpRequest,
        rows: Vec<AgentRow>,
    ) -> Result<PathBuf, DumpError> {
        let gather = Gather::of(request.scope());
        let dump_id = Uuid::new_v4();
        let created_at_ms = self.clock_now();
        let name = format!(
            "dump-{created_at_ms}-{}",
            &dump_id.simple().to_string()[..8]
        );

        std::fs::create_dir_all(&self.reports)?;
        let partial = self.reports.join(format!(".{name}"));
        let _ = std::fs::remove_dir_all(&partial);
        std::fs::create_dir_all(&partial)?;
        let mut errors = Vec::new();
        let mut agents = Vec::new();
        // The core first: its store copy names the cursor each journal
        // must reach back to.
        let core = if gather.core {
            self.dump_core(&partial, &mut errors).await?
        } else {
            None
        };
        let cursors = core.as_ref().map(|core| &core.cursors);
        let sessions = client_sessions(&request.sessions, &rows, &partial, &mut errors)?;
        for row in &rows {
            // An agent the checkpoint's store does not hold was made since:
            // all of its journal came after.
            let cursor = cursors.map(|cursors| cursors.get(&row.agent.agent).copied().unwrap_or(0));
            let slice = match gather.slices {
                Slices::All => true,
                Slices::Sessions => sessions.iter().any(|session| {
                    session["agent_id"]
                        == Uuid::from_slice(&row.agent.agent)
                            .unwrap_or_default()
                            .to_string()
                }),
                Slices::None => false,
            };
            let (entry, mut failed) = self
                .dump_agent(row, dump_id.as_bytes(), &partial, gather, slice, cursor)
                .await?;
            agents.push(entry);
            errors.append(&mut failed);
        }
        if gather.logs {
            for (name, path) in [
                (DAEMON_LOG, &self.daemon_log),
                (SUPERVISOR_LOG, &self.supervisor_log),
            ] {
                let Some(path) = path else { continue };
                match dump_log(path, &partial.join(name)) {
                    Ok(Some(skipped)) => errors.push(format!(
                        "{name}: the first {skipped} bytes of the log were left out"
                    )),
                    Ok(None) => {}
                    Err(error) => errors.push(format!("{name}: {error}")),
                }
            }
        }
        let manifest = serde_json::json!({
            "dump_id": dump_id.to_string(),
            "created_at_ms": created_at_ms,
            "reason": interpret::redact_text(&request.reason),
            "automatic": request.automatic,
            "scope": scope_name(request.scope()),
            "check": (!request.check.is_empty()).then(|| request.check.clone()),
            "profile": self.profile().to_string(),
            "host": self.host().to_string(),
            "generation": self.generation(),
            "daemon_version": crate::version(),
            "agents": agents,
            "client": { "sessions": sessions },
            "core": core.map(|core| serde_json::json!({
                "schema": core.schema,
                "inputs": core.inputs,
                "reads": core.reads,
            })),
            "errors": errors,
        });
        std::fs::write(
            partial.join(MANIFEST),
            serde_json::to_vec_pretty(&manifest).expect("JSON writes"),
        )?;
        let bundle = self.reports.join(&name);
        std::fs::rename(&partial, &bundle)?;
        if let Err(error) = crate::install::prune_reports(&self.reports) {
            tracing::warn!(%error, "cannot prune the reports");
        }
        Ok(bundle)
    }

    /// The rows a dump covers, own and replica.
    async fn dump_rows(&self, ids: &[Vec<u8>]) -> Result<Vec<AgentRow>, DumpError> {
        let ids = ids.to_vec();
        self.read_store(move |store| {
            if ids.is_empty() {
                return Ok(store.agents()?);
            }
            ids.iter()
                .map(|id| {
                    store.agent_by_id(id)?.ok_or_else(|| {
                        DumpError::NoAgent(
                            Uuid::from_slice(id).map_or_else(|_| hex(id), |id| id.to_string()),
                        )
                    })
                })
                .collect()
        })
        .await?
    }

    /// The core's part of the bundle: its newest checkpoint, the ring's
    /// lines since and the store as of that checkpoint, each redacted.
    /// None, with the reason in `errors`, when it cannot be had.
    async fn dump_core(
        &self,
        bundle: &Path,
        errors: &mut Vec<String>,
    ) -> Result<Option<CoreDumped>, DumpError> {
        // The copy waits beside the live store, which holds the same rows,
        // until it is redacted: never unredacted under reports/.
        let scratch = self
            .dir()
            .join(format!(".dump-{}.sqlite", Uuid::new_v4().simple()));
        let mut tries = 0;
        let captured = loop {
            match self.driver().capture(scratch.clone()).await {
                Ok(captured) => break captured,
                Err(why) if tries + 1 < CAPTURE_TRIES => {
                    tracing::debug!(%why, "the core is not ready to be dumped; asking again");
                    tries += 1;
                    tokio::time::sleep(CAPTURE_APART).await;
                }
                Err(why) => {
                    errors.push(format!("{CORE}: {why}"));
                    let _ = std::fs::remove_file(&scratch);
                    return Ok(None);
                }
            }
        };
        let out = bundle.join(CORE);
        std::fs::create_dir_all(&out)?;
        let written = tokio::task::spawn_blocking(move || {
            let written = write_core(&captured, &scratch, &out);
            let _ = std::fs::remove_file(&scratch);
            written
        })
        .await
        .map_err(io::Error::other)?;
        match written {
            Ok(core) => Ok(Some(core)),
            Err(why) => {
                errors.push(format!("{CORE}: {why}"));
                Ok(None)
            }
        }
    }

    /// One agent's directory in the bundle, its journal reaching back to
    /// `cursor` when the core's part names one. Returns its manifest entry
    /// and what could not be gathered.
    async fn dump_agent(
        &self,
        row: &AgentRow,
        dump_id: &[u8],
        bundle: &Path,
        gather: Gather,
        slice: bool,
        cursor: Option<u64>,
    ) -> Result<(serde_json::Value, Vec<String>), DumpError> {
        let id = Uuid::from_slice(&row.agent.agent).unwrap_or_default();
        let kind = Kind::try_from(row.kind).unwrap_or(Kind::Unspecified);
        let own = row.agent.host == self.host().as_bytes();
        let out = bundle.join("agents").join(id.to_string());
        std::fs::create_dir_all(&out)?;
        let mut errors = Vec::new();
        let mut note = |what: &str, error: &dyn std::fmt::Display| {
            errors.push(format!("{id} {what}: {error}"));
        };

        std::fs::write(
            out.join("row.pb"),
            redacted(kind, RedactTarget::Agent(to_wire(row).encode_to_vec())),
        )?;
        let mut rows = None;
        if slice {
            let key = row.agent.clone();
            let cut = self
                .read_rows(move |store| store.cut(&key, DUMP_ROWS))
                .await?;
            let slice = Step {
                items: cut.held,
                snapshot: cut.snapshot,
                ..Step::default()
            };
            rows = Some(slice.items.len());
            std::fs::write(
                out.join("store.pb"),
                redacted(kind, RedactTarget::Step(slice.encode_to_vec())),
            )?;
        }

        let mut part_files = Vec::new();
        let dir = self.agent_dir(id);
        let at_least = match gather.journals {
            Journals::None => None,
            Journals::FromCursor => Some(0),
            Journals::Tail => Some(DUMP_SEGMENTS),
        };
        if own && let Some(at_least) = at_least {
            match journal_tail(&dir.join(agent_dir::JOURNAL), kind, cursor, at_least) {
                Ok(segments) => {
                    for (name, bytes) in segments {
                        write_under(&out.join("journal"), &name, &bytes)?;
                    }
                }
                Err(error) => note("journal", &error),
            }
        }
        if own && gather.parts {
            match self.agent_part(id, dump_id).await {
                Some(part) => {
                    for file in part.files {
                        match safe_relative(&file.name) {
                            Some(name) => {
                                write_under(&out.join("part"), &name, &file.contents)?;
                                part_files.push(file.name);
                            }
                            None => note("part", &format!("a file named {:?}", file.name)),
                        }
                    }
                }
                None => {
                    note(
                        "part",
                        &"no process answered, so its facts ring was not gathered",
                    );
                    match specs(&dir) {
                        Ok(specs) => {
                            for (name, bytes) in specs {
                                let bytes = redacted(kind, RedactTarget::Spec(bytes));
                                write_under(&out.join("part"), &name, &bytes)?;
                                part_files.push(name);
                            }
                        }
                        Err(error) => note("specs", &error),
                    }
                }
            }
        }
        let entry = serde_json::json!({
            "agent_id": id.to_string(),
            "host_id": Uuid::from_slice(&row.agent.host).unwrap_or_default().to_string(),
            "kind": row.kind,
            "own": own,
            "lifecycle": row.lifecycle,
            "incarnation": row.incarnation,
            "rows": rows,
            "part": part_files,
        });
        Ok((entry, errors))
    }

    /// Asks the agent's process for its part over its control connection.
    /// None when it has no connection or does not answer in time.
    async fn agent_part(&self, id: Uuid, dump_id: &[u8]) -> Option<DumpPart> {
        let handle = self.handle(id)?;
        let (tx, rx) = oneshot::channel();
        handle.expect_dump(dump_id.to_vec(), tx);
        let frame = CtlFrame {
            of: Some(ctl_frame::Of::DumpRequest(AgentDumpRequest {
                dump_id: dump_id.to_vec(),
            })),
        };
        let patience = crate::runtime::ms(self.launch().ctl_write_ms);
        let sent = matches!(
            handle.write_ctl(&frame, patience).await,
            crate::runtime::Sent::Written
        );
        let part = if sent {
            tokio::time::timeout(PART_PATIENCE, rx).await.ok()?.ok()
        } else {
            None
        };
        handle.forget_dump(dump_id);
        part
    }
}

/// The tail of the log at `path` and its `.1`, redacted, written to `to`.
/// Returns how many bytes of the log were left out, when it was cut.
fn dump_log(path: &Path, to: &Path) -> io::Result<Option<u64>> {
    let Some((tail, skipped)) = log_file::tail(path, DUMP_LOG_BYTES)? else {
        return Ok(None);
    };
    let text = interpret::redact_text(&String::from_utf8_lossy(&tail));
    std::fs::write(to, text)?;
    Ok((skipped > 0).then_some(skipped))
}

/// One target through the kind's redactor, as the bytes it came back as.
/// Writes each session the asking client handed in, redacted through its
/// agent's kind, and returns their manifest entries. A session of an agent
/// the dump does not cover is named in `errors` and left out.
fn client_sessions(
    sessions: &[Vec<u8>],
    rows: &[AgentRow],
    bundle: &Path,
    errors: &mut Vec<String>,
) -> io::Result<Vec<serde_json::Value>> {
    let mut entries = Vec::new();
    for encoded in sessions {
        let history = match ClientSessionHistory::decode(encoded.as_slice()) {
            Ok(history) => history,
            Err(error) => {
                errors.push(format!("a client session: {error}"));
                continue;
            }
        };
        let id = Uuid::from_slice(&history.agent_id).unwrap_or_default();
        let Some(row) = rows.iter().find(|row| row.agent.agent == history.agent_id) else {
            errors.push(format!(
                "{id} client session: not an agent this dump covers"
            ));
            continue;
        };
        let kind = Kind::try_from(row.kind).unwrap_or(Kind::Unspecified);
        let bytes = redacted(kind, RedactTarget::ClientSession(encoded.clone()));
        let history = match ClientSessionHistory::decode(bytes.as_slice()) {
            Ok(history) => history,
            Err(error) => {
                errors.push(format!("{id} client session: {error}"));
                continue;
            }
        };
        let events = history.events.unwrap_or_default();
        let dir = bundle.join(CLIENT_SESSIONS).join(id.to_string());
        let start = history.start.unwrap_or_default();
        write_under(&dir, CLIENT_CHECKPOINT, &start.encode_to_vec())?;
        write_under(&dir, CLIENT_EVENTS, &events.encode_to_vec())?;
        let at = |file: &str| format!("{CLIENT_SESSIONS}/{id}/{file}");
        entries.push(serde_json::json!({
            "agent_id": id.to_string(),
            "checkpoint": at(CLIENT_CHECKPOINT),
            "events": at(CLIENT_EVENTS),
            "traced": events.traced.len(),
        }));
    }
    Ok(entries)
}

/// A scope as the manifest names it.
fn scope_name(scope: DumpScope) -> &'static str {
    match scope {
        DumpScope::Unspecified => "host",
        DumpScope::Core => "core",
        DumpScope::Agent => "agent",
        DumpScope::Transcript => "transcript",
        DumpScope::Report => "report",
    }
}

fn redacted(kind: Kind, target: RedactTarget) -> Vec<u8> {
    match interpret::redact(kind, target) {
        RedactTarget::ItemBody(bytes)
        | RedactTarget::SnapshotBody(bytes)
        | RedactTarget::Input(bytes)
        | RedactTarget::AgentMessage(bytes)
        | RedactTarget::Checkpoint(bytes)
        | RedactTarget::Spec(bytes)
        | RedactTarget::Step(bytes)
        | RedactTarget::Agent(bytes)
        | RedactTarget::ClientSession(bytes) => bytes,
        RedactTarget::Fact(fact) => fact.payload,
    }
}

/// The journal segments from the one holding `cursor` on, and at least
/// the last `at_least` (the last [`DUMP_SEGMENTS`] when there is no
/// cursor), each whole frame redacted and written to its original length so
/// every offset in the journal holds. A torn tail, a write still under way,
/// keeps its length prefix and loses its bytes.
fn journal_tail(
    dir: &Path,
    kind: Kind,
    cursor: Option<u64>,
    at_least: usize,
) -> io::Result<Vec<(String, Vec<u8>)>> {
    let starts = journal::segments(dir)?;
    let at_least = if cursor.is_some() {
        at_least
    } else {
        DUMP_SEGMENTS
    };
    let last_few = starts.len().saturating_sub(at_least);
    let from = cursor
        .and_then(|cursor| starts.iter().rposition(|&start| start <= cursor))
        .map_or(last_few, |holding| holding.min(last_few));
    let mut out = Vec::new();
    for &start in &starts[from..] {
        let bytes = std::fs::read(journal::segment_path(dir, start))?;
        let mut framed = Vec::with_capacity(bytes.len());
        let mut at = 0;
        while let journal::Frame::Whole(step, len) = journal::decode_frame(&bytes[at..]) {
            framed.extend(fitted(kind, &step, len));
            at += len;
        }
        let tail = &bytes[at..];
        let prefix = tail
            .iter()
            .position(|byte| byte & 0x80 == 0)
            .map_or(tail.len(), |last| last + 1);
        framed.extend_from_slice(&tail[..prefix]);
        framed.resize(bytes.len(), 0);
        out.push((journal::segment_name(start), framed));
    }
    Ok(out)
}

/// A journal frame redacted, as a frame of `len` bytes: the redacted step,
/// else that step without its texts and bodies, else an empty step.
fn fitted(kind: Kind, step: &Step, len: usize) -> Vec<u8> {
    let redacted = redacted(kind, RedactTarget::Step(step.encode_to_vec()));
    let mut step = Step::decode(redacted.as_slice()).unwrap_or_default();
    if let Some(frame) = journal::encode_frame_exact(&step, len) {
        return frame;
    }
    for item in &mut step.items {
        item.text.clear();
        item.body.clear();
        item.attachments.clear();
    }
    for append in &mut step.appends {
        append.text.clear();
    }
    if let Some(snapshot) = &mut step.snapshot {
        snapshot.body.clear();
        snapshot.working_on = None;
        snapshot.git = None;
    }
    journal::encode_frame_exact(&step, len)
        .or_else(|| journal::encode_frame_exact(&Step::default(), len))
        .unwrap_or_else(|| vec![0; len])
}

/// What the core's part of a bundle holds, for the manifest.
struct CoreDumped {
    schema: u32,
    /// Inputs, and lines saying what an ingest read.
    inputs: usize,
    reads: usize,
    /// Each agent's ingest cursor as of the checkpoint, by agent id.
    cursors: BTreeMap<Vec<u8>, u64>,
}

/// Writes the core's part from what the core thread handed over: the
/// checkpoint and ring lines redacted as the core's values, the store copy
/// at `scratch` redacted in place and moved in.
fn write_core(captured: &Captured, scratch: &Path, out: &Path) -> Result<CoreDumped, String> {
    let checkpoint = daemon_core::Checkpoint::from_bytes(&captured.checkpoint)
        .and_then(|checkpoint| checkpoint.redacted(&Redact))
        .map_err(|error| format!("the checkpoint: {error}"))?;
    let cursors = store::redact::redact_file(scratch, &Redact)
        .map_err(|error| format!("redacting the store: {error}"))?;
    let mut lines = Vec::new();
    let (mut inputs, mut reads) = (0, 0);
    for line in captured.lines.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        // A line this build cannot read ends what it can replay, as it ends
        // a resume.
        let Ok(recorded) = serde_json::from_slice::<Recorded>(line) else {
            break;
        };
        match recorded {
            Recorded::Input(_) => inputs += 1,
            Recorded::Read { .. } => reads += 1,
        }
        lines.extend(serde_json::to_vec(&recorded.redacted(&Redact)).expect("a line writes"));
        lines.push(b'\n');
    }
    let write = |name: &str, bytes: &[u8]| {
        std::fs::write(out.join(name), bytes).map_err(|error| format!("writing {name}: {error}"))
    };
    write(CORE_CHECKPOINT, &checkpoint.to_bytes())?;
    write(CORE_INPUTS, &lines)?;
    std::fs::copy(scratch, out.join(CORE_STORE))
        .map_err(|error| format!("writing {CORE_STORE}: {error}"))?;
    Ok(CoreDumped {
        schema: checkpoint.schema,
        inputs,
        reads,
        cursors: cursors
            .into_iter()
            .map(|(key, cursor)| (key.agent, cursor))
            .collect(),
    })
}

/// The redactor the core's values and the store copy go through: protocol
/// messages by descriptor, free text by the shared rules, an agent's rows
/// and items by its kind's own redactor.
struct Redact;

impl store::redact::Redactor for Redact {
    fn text(&self, text: &str) -> String {
        interpret::redact_text(text)
    }

    fn message(&self, name: &str, bytes: &[u8]) -> Vec<u8> {
        interpret::redact_message(name, bytes)
    }

    fn agent(&self, kind: i32, agent: wire::Agent) -> wire::Agent {
        let kind = Kind::try_from(kind).unwrap_or(Kind::Unspecified);
        wire::Agent::decode(redacted(kind, RedactTarget::Agent(agent.encode_to_vec())).as_slice())
            .unwrap_or_default()
    }

    fn step(&self, kind: i32, step: Step) -> Step {
        let kind = Kind::try_from(kind).unwrap_or(Kind::Unspecified);
        Step::decode(redacted(kind, RedactTarget::Step(step.encode_to_vec())).as_slice())
            .unwrap_or_default()
    }
}

/// Every `spec.<n>` in an agent directory, by number.
fn specs(dir: &Path) -> io::Result<Vec<(String, Vec<u8>)>> {
    let mut specs = BTreeMap::new();
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if let Some(n) = name
            .strip_prefix("spec.")
            .and_then(|n| n.parse::<u32>().ok())
        {
            specs.insert(n, (name.clone(), std::fs::read(dir.join(&name))?));
        }
    }
    Ok(specs.into_values().collect())
}

/// A name an agent sent, if it stays inside the directory it is written
/// under.
fn safe_relative(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    let normal = path
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    (normal && !name.is_empty()).then(|| path.to_owned())
}

fn write_under(dir: &Path, name: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

/// A bundle as one message: every file under `bundle`, named by its path
/// relative to it with `/` separators, in path order.
pub fn pack(bundle: &Path) -> io::Result<DumpPart> {
    let mut files = Vec::new();
    let mut stack = vec![bundle.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path
                .strip_prefix(bundle)
                .map_err(io::Error::other)?
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            files.push(DumpFile {
                name,
                contents: std::fs::read(&path)?,
            });
        }
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    let dump_id = bundle
        .file_name()
        .map(|name| name.to_string_lossy().into_owned().into_bytes())
        .unwrap_or_default();
    Ok(DumpPart { dump_id, files })
}
