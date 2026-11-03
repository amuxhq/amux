//! Dumps: one bundle of this host's records, whatever asked for it.
//!
//! A record is what a stage concluded, never what anybody wrote: the
//! daemon core's inputs as their structure alone, each own agent's record
//! (its facts ring run again through its interpreter when the dump is
//! made), and the record of each chat the asking client shows. Beside them
//! go our own logs for the minute before. Every bundle carries the same
//! parts; what asked for it, a check or a person, is only its manifest's
//! `trigger`.
//!
//! A bundle is a directory under the installation's `reports/`, assembled
//! beside it under a dotted name and renamed into place once complete:
//!
//! ```text
//! reports/dump-<unix ms>-<id>/
//!   manifest.json            what asked, when, by which daemon, what each
//!                            record holds, and every part that could not
//!                            be gathered
//!   daemon.log               the daemon's own log and its .1 for the
//!                            minute before the dump, at INFO and above,
//!                            redacted as text
//!   supervisor.log           the same of the supervisor's, when there is one
//!   core/                    the daemon core's record, as its structure
//!                            alone (below):
//!     checkpoint             its newest checkpoint
//!     inputs                 every input stepped since, and what each
//!                            ingest read of a journal, one JSON line each
//!                            (daemon_core::Recorded)
//!     store.sqlite           the store as of that checkpoint
//!     journals/<agent_id>/<segment>
//!                            each own agent's journal from the segment
//!                            holding the checkpoint's cursor on, each frame
//!                            written to its original length, so every
//!                            offset holds
//!   agents/<agent_id>/       each own agent:
//!     record                 its record: each event and what its
//!                            interpreter concluded of it, by kind, size
//!                            and alias
//!     agent.log              its own log for the minute before, redacted
//!                            as text; never the provider's
//!   client/sessions/<agent_id>/
//!                            each chat the asking client shows, as its
//!                            record (`ui_state::record`):
//!     checkpoint             the session's state when its trace began, a
//!                            wire.ClientSession
//!     events                 every event its driver saw since, through the
//!                            one that broke it when one did, a
//!                            wire.ClientSessionEvents
//! ```
//!
//! The daemon core reads no content: it acts on ids, kinds, order, sizes
//! and time. So its record is its part as [`Structure`] leaves it: every
//! text and body zero bytes of its length, every identifier it compares an
//! alias, every id, count, revision, timing and its randomness as
//! recorded. It still restores and replays as the original does.
//!
//! The whole bundle is held to [`REPORT_LIMIT`] once compressed. The logs
//! always go and come first; the records fill what is left, the one of
//! what asked first (the core's for a daemon's check, the agent's for an
//! agent's, the chats' for a person or a transcript's check), then the
//! chats on screen, the core's and the other agents', the most recently
//! busy first. An agent's record is built to the room left when it is
//! asked for, its oldest events left out to fit; a record that still
//! does not fit is left out, its size named under `over_limit` and
//! counted in the manifest's `left_out`. The core's is measured whole,
//! its checkpoint, inputs, store copy and journals together.
//!
//! Everything protobuf is encoded without a length prefix except journal
//! segments, which keep the journal's own framing. [`pack`] turns a bundle
//! into one `wire.DumpPart` whose files carry these relative paths, which is
//! how a bundle is sent.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use core_ring::DumpKind;
use daemon_core::Recorded;
use prost::Message as _;
use store::AgentRow;
use tokio::sync::oneshot;
use uuid::Uuid;
use wire::dump::ClientSessionHistory;
use wire::{AgentDumpRequest, CtlFrame, DumpFile, DumpPart, DumpRequest, Step, ctl_frame};

use crate::driver::{AGENT_DUMP, AGENT_VIOLATION_DUMP, Captured};
use crate::runtime::ProfileRuntime;

/// Where the core's record lives in a bundle, and its files.
pub const CORE: &str = "core";
pub const CORE_CHECKPOINT: &str = "checkpoint";
pub const CORE_INPUTS: &str = "inputs";
pub const CORE_STORE: &str = "store.sqlite";
pub const CORE_JOURNALS: &str = "journals";
/// Where a client's session records live in a bundle, one directory per
/// agent, and each one's files.
pub const CLIENT_SESSIONS: &str = "client/sessions";
pub const CLIENT_CHECKPOINT: &str = "checkpoint";
pub const CLIENT_EVENTS: &str = "events";
/// How often, and how far apart, a dump asks for the core again while its
/// store's main file is being checkpointed.
const CAPTURE_TRIES: u32 = 30;
const CAPTURE_APART: Duration = Duration::from_millis(100);
/// The most of each of the installation's logs a dump reads back through
/// for its window: a log written fast enough to pass this in a minute
/// sends only its newest lines.
pub const DUMP_LOG_BYTES: u64 = 1 << 20;
/// The most a bundle may take, compressed: its logs and every record.
pub const REPORT_LIMIT: usize = interpret::record::LIMIT;
/// The room under which an agent whose record did not fit is left out
/// rather than asked for a shorter one: hardly an event would fit.
const SMALLEST_RECORD: usize = 4 << 10;
/// Where an agent's record lives in its bundle directory and in the part
/// its process sends.
pub const AGENT_RECORD: &str = "record";
/// How long an agent process has to send its record.
const PART_PATIENCE: Duration = Duration::from_secs(10);
/// What a bundle's manifest names as its trigger when a person asked.
pub const PERSON: &str = "person";

pub const MANIFEST: &str = "manifest.json";
pub const DAEMON_LOG: &str = "daemon.log";
pub const SUPERVISOR_LOG: &str = "supervisor.log";

#[derive(Debug, thiserror::Error)]
pub enum DumpError {
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

/// The record an asking agent's core sent with its ask, which stands in
/// for asking its process: the process may have ended since.
struct Asked {
    agent: Uuid,
    record: Vec<u8>,
}

impl ProfileRuntime {
    /// Writes the bundle and returns its directory. What cannot be gathered
    /// is named in the manifest. An automatic one a check asked for is sent
    /// on when the installation sends automatic reports.
    pub async fn dump(self: &Arc<Self>, request: DumpRequest) -> Result<PathBuf, DumpError> {
        self.dump_with(request, None).await
    }

    async fn dump_with(
        self: &Arc<Self>,
        request: DumpRequest,
        asked: Option<Asked>,
    ) -> Result<PathBuf, DumpError> {
        let bundle = self.write_dump(&request, asked).await?;
        if request.automatic {
            // A client's check has no analytics of its own to count it.
            let core = crate::reports::core_of_check(&request.check);
            if core == crate::reports::Core::Transcript {
                self.check_failed(core, &request.check, 0);
            }
            self.send_automatic(&bundle, &request);
        }
        Ok(bundle)
    }

    /// Writes an automatic bundle off the caller, for `whose` asking: under
    /// the guard a shutdown waits on, so none starts once the profile stops
    /// waiting.
    pub(crate) fn dump_unasked(
        self: &Arc<Self>,
        reason: String,
        check: String,
        whose: &'static str,
    ) {
        self.dump_unasked_with(reason, check, whose, None);
    }

    fn dump_unasked_with(
        self: &Arc<Self>,
        reason: String,
        check: String,
        whose: &'static str,
        asked: Option<Asked>,
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
                reason,
                automatic: true,
                check,
                ..DumpRequest::default()
            };
            match runtime.dump_with(request, asked).await {
                Ok(bundle) => tracing::warn!(bundle = %bundle.display(), whose, "dumped"),
                Err(error) => tracing::error!(%error, whose, "dumping failed"),
            }
        });
        dumps.retain(|dump| !dump.is_finished());
        dumps.push(task);
    }

    /// An agent's core asked for a dump over its control connection, after
    /// this host's own limit per kind.
    pub(crate) fn agent_asked(self: &Arc<Self>, id: Uuid, asked: wire::DumpAsked) {
        let check = if asked.invariant.is_empty() {
            String::new()
        } else {
            asked.check.clone()
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
        self.dump_unasked_with(
            reason,
            crate::reports::check_id(core, &check),
            "an agent's core",
            (!asked.record.is_empty()).then_some(Asked {
                agent: id,
                record: asked.record,
            }),
        );
    }

    async fn write_dump(
        &self,
        request: &DumpRequest,
        asked: Option<Asked>,
    ) -> Result<PathBuf, DumpError> {
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
        let host = self.host().as_bytes().to_vec();
        let mut own: Vec<AgentRow> = self
            .read_store(|store| store.agents())
            .await??
            .into_iter()
            .filter(|row| row.agent.host == host)
            .collect();
        own.sort_by_key(|row| std::cmp::Reverse(row.phase_since_ms.unwrap_or(row.created_at_ms)));
        let logs = self.dump_logs(&own, &partial, &mut errors)?;
        let mut room = Room {
            left: REPORT_LIMIT.saturating_sub(logs),
            left_out: 0,
        };
        let core_first = !request.check.is_empty()
            && !matches!(
                crate::reports::core_of_check(&request.check),
                crate::reports::Core::Agent | crate::reports::Core::Transcript
            );
        let mut core = None;
        if core_first {
            core = self
                .dump_core(&partial, &own, &mut room, &mut errors)
                .await?;
        }
        let dump_id_bytes = dump_id.as_bytes().as_slice();
        let mut agents = Vec::new();
        let asked_id = asked.as_ref().map(|asked| asked.agent);
        if let Some(asked) = asked
            && let Some(row) = own
                .iter()
                .find(|row| row.agent.agent == asked.agent.as_bytes())
        {
            let entry = self
                .dump_agent(
                    row,
                    Some(asked.record),
                    dump_id_bytes,
                    &partial,
                    &mut room,
                    &mut errors,
                )
                .await?;
            agents.push(entry);
        }
        let sessions = client_sessions(&request.sessions, &partial, &mut room, &mut errors)?;
        if !core_first {
            core = self
                .dump_core(&partial, &own, &mut room, &mut errors)
                .await?;
        }
        let rest: Vec<&AgentRow> = own
            .iter()
            .filter(|row| asked_id.is_none_or(|id| row.agent.agent != id.as_bytes()))
            .collect();
        agents.extend(
            self.dump_agents(&rest, dump_id_bytes, &partial, &mut room, &mut errors)
                .await?,
        );
        let trigger = if request.check.is_empty() {
            PERSON.to_owned()
        } else {
            request.check.clone()
        };
        let manifest = serde_json::json!({
            "dump_id": dump_id.to_string(),
            "created_at_ms": created_at_ms,
            "trigger": trigger,
            "reason": interpret::redact_text(&request.reason),
            "profile": self.profile().to_string(),
            "host": self.host().to_string(),
            "generation": self.generation(),
            "daemon_version": crate::version(),
            "agents": agents,
            "client": { "sessions": sessions },
            "core": core.map(|core| serde_json::json!({
                "inputs": core.inputs,
                "reads": core.reads,
                "over_limit": (!core.kept).then(|| over_limit(core.bytes, core.room)),
            })),
            "limit": REPORT_LIMIT,
            "left_out": room.left_out,
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

    /// The daemon's and the supervisor's logs, and each own agent's, in
    /// `bundle`. Returns their size together, compressed.
    fn dump_logs(
        &self,
        own: &[AgentRow],
        bundle: &Path,
        errors: &mut Vec<String>,
    ) -> io::Result<usize> {
        let mut bytes = 0;
        for (name, path) in [
            (DAEMON_LOG, &self.daemon_log),
            (SUPERVISOR_LOG, &self.supervisor_log),
        ] {
            let Some(path) = path else { continue };
            match dump_log(path, &bundle.join(name), DUMP_LOG_BYTES) {
                Ok(written) => bytes += written,
                Err(error) => errors.push(format!("{name}: {error}")),
            }
        }
        for row in own {
            let id = Uuid::from_slice(&row.agent.agent).unwrap_or_default();
            let out = bundle.join("agents").join(id.to_string());
            std::fs::create_dir_all(&out)?;
            let log = self.agent_dir(id).join(agent_dir::AGENT_LOG);
            match dump_log(&log, &out.join(agent_dir::AGENT_LOG), log_file::AGENT_CAP) {
                Ok(written) => bytes += written,
                Err(error) => errors.push(format!("{id} agent.log: {error}")),
            }
        }
        Ok(bytes)
    }

    /// The core's record: its newest checkpoint, the ring's lines since,
    /// the store as of that checkpoint and each own agent's journal from
    /// its cursor there, each as its structure alone; only the counts of a
    /// window past the room left. None, with the reason in `errors`, when
    /// it cannot be had.
    async fn dump_core(
        &self,
        bundle: &Path,
        own: &[AgentRow],
        room: &mut Room,
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
        let journals: Vec<(Uuid, PathBuf)> = own
            .iter()
            .map(|row| {
                let id = Uuid::from_slice(&row.agent.agent).unwrap_or_default();
                (id, self.agent_dir(id).join(agent_dir::JOURNAL))
            })
            .collect();
        let limit = room.left;
        let written = tokio::task::spawn_blocking(move || {
            let written = write_core(&captured, &scratch, &out, &journals, limit);
            let _ = std::fs::remove_file(&scratch);
            written
        })
        .await
        .map_err(io::Error::other)?;
        match written {
            Ok(core) => {
                if !room.take(core.bytes) {
                    errors.push(format!(
                        "{CORE}: the record is {} bytes compressed, past the {} left of the \
                         report's limit: only its counts are here",
                        core.bytes, core.room
                    ));
                }
                Ok(Some(core))
            }
            Err(why) => {
                errors.push(format!("{CORE}: {why}"));
                Ok(None)
            }
        }
    }

    /// The record of each of `own`, asked of every process at once with
    /// the room left now and placed in order. Returns their manifest
    /// entries.
    async fn dump_agents(
        &self,
        own: &[&AgentRow],
        dump_id: &[u8],
        bundle: &Path,
        room: &mut Room,
        errors: &mut Vec<String>,
    ) -> Result<Vec<serde_json::Value>, DumpError> {
        // Each process runs its ring through its interpreter again, and one
        // that does not answer waits out its patience alone.
        let limit = room.left;
        let records = futures_util::future::join_all(own.iter().map(|row| {
            let id = Uuid::from_slice(&row.agent.agent).unwrap_or_default();
            self.agent_record(id, dump_id, limit)
        }))
        .await;
        let mut entries = Vec::new();
        for (row, record) in own.iter().zip(records) {
            entries.push(
                self.dump_agent(row, record, dump_id, bundle, room, errors)
                    .await?,
            );
        }
        Ok(entries)
    }

    /// Places one agent's `record` in the bundle when it fits the room
    /// left. One that does not is asked for again, cut to that room, while
    /// its process still answers. Returns the agent's manifest entry.
    async fn dump_agent(
        &self,
        row: &AgentRow,
        mut record: Option<Vec<u8>>,
        dump_id: &[u8],
        bundle: &Path,
        room: &mut Room,
        errors: &mut Vec<String>,
    ) -> Result<serde_json::Value, DumpError> {
        let id = Uuid::from_slice(&row.agent.agent).unwrap_or_default();
        let mut bytes = record.as_deref().map(compressed_len);
        if bytes.is_some_and(|bytes| bytes > room.left)
            && room.left >= SMALLEST_RECORD
            && let Some(shorter) = self.agent_record(id, dump_id, room.left).await
        {
            bytes = Some(compressed_len(&shorter));
            record = Some(shorter);
        }
        let mut over = None;
        let recorded = match (record, bytes) {
            (Some(record), Some(bytes)) => {
                let left = room.left;
                if room.take(bytes) {
                    let out = bundle.join("agents").join(id.to_string());
                    std::fs::create_dir_all(&out)?;
                    std::fs::write(out.join(AGENT_RECORD), record)?;
                    true
                } else {
                    errors.push(format!(
                        "{id} record: {bytes} bytes compressed, past the {left} left of the \
                         report's limit"
                    ));
                    over = Some(over_limit(bytes, left));
                    false
                }
            }
            _ => {
                errors.push(format!(
                    "{id} record: no process answered, and none was sent"
                ));
                false
            }
        };
        Ok(serde_json::json!({
            "agent_id": id.to_string(),
            "kind": row.kind,
            "lifecycle": row.lifecycle,
            "incarnation": row.incarnation,
            "record": recorded,
            "over_limit": over,
        }))
    }

    /// Asks the agent's process for its record, within `limit` bytes
    /// compressed, over its control connection. None when it has no
    /// connection or does not answer in time.
    async fn agent_record(&self, id: Uuid, dump_id: &[u8], limit: usize) -> Option<Vec<u8>> {
        let handle = self.handle(id)?;
        let (tx, rx) = oneshot::channel();
        handle.expect_dump(dump_id.to_vec(), tx);
        let frame = CtlFrame {
            of: Some(ctl_frame::Of::DumpRequest(AgentDumpRequest {
                dump_id: dump_id.to_vec(),
                limit: limit as u64,
            })),
        };
        let patience = crate::runtime::ms(self.launch().ctl_write_ms);
        let sent = matches!(
            handle.write_ctl(&frame, patience).await,
            crate::runtime::Sent::Written
        );
        let part = if sent {
            tokio::time::timeout(PART_PATIENCE, rx)
                .await
                .ok()
                .and_then(Result::ok)
        } else {
            None
        };
        handle.forget_dump(dump_id);
        part?
            .files
            .into_iter()
            .find(|file| file.name == AGENT_RECORD)
            .map(|file| file.contents)
    }
}

/// What is left of [`REPORT_LIMIT`] as a bundle's records go in, and how
/// many it left out.
struct Room {
    left: usize,
    left_out: usize,
}

impl Room {
    /// Takes a record of `bytes` compressed when it fits what is left, and
    /// otherwise counts it left out.
    fn take(&mut self, bytes: usize) -> bool {
        if bytes <= self.left {
            self.left -= bytes;
            true
        } else {
            self.left_out += 1;
            false
        }
    }
}

fn over_limit(bytes: usize, room: usize) -> serde_json::Value {
    serde_json::json!({ "bytes": bytes, "room": room })
}

/// `bytes` compressed as a bundle is, measured.
fn compressed_len(bytes: &[u8]) -> usize {
    zstd::bulk::compress(bytes, 3).map_or(usize::MAX, |compressed| compressed.len())
}

/// The log at `path` and its `.1` for the minute before now, at most
/// `max` bytes of it, redacted, written to `to`. The window is cut by the
/// wall clock the log is stamped with, whatever clock the core runs on.
/// Returns what was written's size compressed.
fn dump_log(path: &Path, to: &Path, max: u64) -> io::Result<usize> {
    let since = log_file::unix_ms_now() - log_file::DUMP_WINDOW_MS;
    let Some(window) = log_file::window(path, since, max)? else {
        return Ok(0);
    };
    let text = interpret::redact_text(&String::from_utf8_lossy(&window));
    std::fs::write(to, &text)?;
    Ok(compressed_len(text.as_bytes()))
}

/// Writes each session record the asking client handed in and returns
/// their manifest entries. Anything that is not a record, or one past the
/// room left, is named in `errors` and left out: a whole session holds
/// its chat's conversation.
fn client_sessions(
    sessions: &[Vec<u8>],
    bundle: &Path,
    room: &mut Room,
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
        if !history.structure_only {
            errors.push(format!(
                "{id} client session: a whole history, where a dump carries only records"
            ));
            continue;
        }
        let (bytes, left) = (compressed_len(encoded), room.left);
        if !room.take(bytes) {
            errors.push(format!(
                "{id} client session: {bytes} bytes compressed, past the {left} left of the \
                 report's limit"
            ));
            continue;
        }
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

/// The journal segments in `dir` from the one holding `cursor` on, each
/// whole frame as its structure and written to its original length so
/// every offset in the journal holds. A torn tail, a write still under way,
/// keeps its length prefix and loses its bytes.
fn journal_from(
    dir: &Path,
    structure: &Structure,
    cursor: u64,
) -> io::Result<Vec<(String, Vec<u8>)>> {
    let starts = match journal::segments(dir) {
        Ok(starts) => starts,
        // An agent whose process has not written yet.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let from = starts
        .iter()
        .rposition(|&start| start <= cursor)
        .unwrap_or(0);
    let mut out = Vec::new();
    for &start in starts.get(from..).unwrap_or_default() {
        let bytes = std::fs::read(journal::segment_path(dir, start))?;
        let mut framed = Vec::with_capacity(bytes.len());
        let mut at = 0;
        while let journal::Frame::Whole(step, len) = journal::decode_frame(&bytes[at..]) {
            framed.extend(fitted(structure, &step, len)?);
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

/// A journal frame as its structure, as a frame of `len` bytes. Every
/// alias is as long as what it stands for and every blank as long as its
/// text, so the step is never longer than it was and is padded to its
/// length; one that would not fit is an error rather than a frame that
/// says less than the live one did.
fn fitted(structure: &Structure, step: &Step, len: usize) -> io::Result<Vec<u8>> {
    let redacted = structure.message("amux.v1.Step", &step.encode_to_vec());
    let step = Step::decode(redacted.as_slice()).map_err(io::Error::other)?;
    journal::encode_frame_exact(&step, len)
        .ok_or_else(|| io::Error::other(format!("a frame of {len} bytes grew past its length")))
}

/// What the core's record in a bundle holds, for the manifest.
struct CoreDumped {
    /// Inputs, and lines saying what an ingest read.
    inputs: usize,
    reads: usize,
    /// The record's size compressed; for one past its room, at least as
    /// much as was measured before it was given up.
    bytes: usize,
    /// The room it was given, and whether it was kept in it.
    room: usize,
    kept: bool,
}

/// Writes the core's record from what the core thread handed over: the
/// checkpoint and ring lines as their structure, the store copy at
/// `scratch` redacted to its structure in place and moved in, and each of
/// `journals` from its cursor in that store. A record past `limit`
/// compressed, all of it together, is counted and nothing of it is left
/// written; the ring's lines alone are measured first, so a window that
/// cannot fit costs no store copy.
fn write_core(
    captured: &Captured,
    scratch: &Path,
    out: &Path,
    journals: &[(Uuid, PathBuf)],
    limit: usize,
) -> Result<CoreDumped, String> {
    // One for the record, so an identifier has one alias throughout it.
    let structure = Structure::new();
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
        lines.extend(serde_json::to_vec(&recorded.redacted(&structure)).expect("a line writes"));
        lines.push(b'\n');
    }
    let bytes = compressed_len(&lines);
    if bytes > limit {
        return Ok(CoreDumped {
            inputs,
            reads,
            bytes,
            room: limit,
            kept: false,
        });
    }
    let checkpoint = daemon_core::redact::checkpoint(&captured.checkpoint, &structure)
        .map_err(|error| format!("the checkpoint: {error}"))?;
    let cursors: BTreeMap<Vec<u8>, u64> = store::redact::redact_file(scratch, &structure)
        .map_err(|error| format!("redacting the store: {error}"))?
        .into_iter()
        .map(|(key, cursor)| (key.agent, cursor))
        .collect();
    std::fs::create_dir_all(out).map_err(|error| format!("making {CORE}: {error}"))?;
    let write = |name: &str, bytes: &[u8]| {
        std::fs::write(out.join(name), bytes).map_err(|error| format!("writing {name}: {error}"))
    };
    write(CORE_CHECKPOINT, &checkpoint)?;
    write(CORE_INPUTS, &lines)?;
    std::fs::copy(scratch, out.join(CORE_STORE))
        .map_err(|error| format!("writing {CORE_STORE}: {error}"))?;
    for (id, dir) in journals {
        // An agent the checkpoint's store does not hold was made since:
        // all of its journal came after.
        let cursor = cursors.get(id.as_bytes().as_slice()).copied().unwrap_or(0);
        let segments = journal_from(dir, &structure, cursor)
            .map_err(|error| format!("{id}'s journal: {error}"))?;
        let at = out.join(CORE_JOURNALS).join(id.to_string());
        for (name, bytes) in segments {
            write_under(&at, &name, &bytes)
                .map_err(|error| format!("writing {id}'s journal: {error}"))?;
        }
    }
    let bytes = compressed(out).map_err(|error| format!("measuring {CORE}: {error}"))?;
    let kept = bytes <= limit;
    if !kept {
        std::fs::remove_dir_all(out).map_err(|error| format!("removing {CORE}: {error}"))?;
    }
    Ok(CoreDumped {
        inputs,
        reads,
        bytes,
        room: limit,
        kept,
    })
}

/// Every file under `dir`, in path order, compressed together as a bundle
/// is sent: the size a record takes against [`REPORT_LIMIT`]. Streamed, so
/// a large store copy is never held whole.
fn compressed(dir: &Path) -> io::Result<usize> {
    struct Counted(usize);
    impl io::Write for Counted {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut files = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                stack.push(entry.path());
            } else {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    let mut encoder = zstd::stream::Encoder::new(Counted(0), 3)?;
    for file in files {
        io::copy(&mut std::fs::File::open(file)?, &mut encoder)?;
    }
    Ok(encoder.finish()?.0)
}

/// The redactor of what the daemon core is fed: every value treated by
/// the name of its field (`redaction::structure`), protocol messages
/// walked by descriptor, content bytes zero bytes of their length. A name
/// the core assigned itself is kept, as the core compares the names it
/// assigns with those it holds.
pub(crate) struct Structure(Mutex<redaction::structure::Aliases>);

impl Structure {
    fn new() -> Self {
        Structure(Mutex::new(redaction::structure::Aliases::new(
            daemon_core::names::is_assigned,
        )))
    }

    fn message(&self, name: &str, bytes: &[u8]) -> Vec<u8> {
        interpret::structure_message(name, bytes, &mut self.0.lock().unwrap())
    }
}

impl store::redact::Redactor for Structure {
    fn text(&self, holder: &str, field: &str, text: &str) -> String {
        self.0.lock().unwrap().text(holder, field, text)
    }

    fn message(&self, name: &str, bytes: &[u8]) -> Vec<u8> {
        Structure::message(self, name, bytes)
    }

    fn content(&self, bytes: &[u8]) -> Vec<u8> {
        vec![0; bytes.len()]
    }

    fn hash(&self, bytes: &[u8]) -> Vec<u8> {
        self.0.lock().unwrap().hash(bytes)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The room these give a core's record: small, so a few thousand rows
    /// pass it.
    const LIMIT: usize = 64 << 10;

    #[test]
    fn a_window_past_the_limit_is_counted_and_not_written() {
        // Randomness does not compress: enough of it passes the limit.
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut lines = Vec::new();
        for _ in 0..4_000 {
            let mut bytes = [0; 32];
            for byte in &mut bytes {
                seed = seed
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                *byte = (seed >> 56) as u8;
            }
            let input = Recorded::Input(Box::new(daemon_core::Input::Entropy(bytes)));
            lines.extend(serde_json::to_vec(&input).unwrap());
            lines.push(b'\n');
        }
        let captured = Captured {
            checkpoint: Vec::new(),
            lines,
        };
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join(CORE);
        let scratch = dir.path().join("store.sqlite");
        let core = write_core(&captured, &scratch, &out, &[], LIMIT)
            .expect("a window past the limit is counted");
        assert_eq!((core.inputs, core.reads), (4_000, 0));
        assert!(!core.kept && core.bytes > LIMIT, "{}", core.bytes);
        assert!(!out.exists(), "nothing of the window is written");
    }

    /// A journal frame through the core's structure keeps its length,
    /// however short what an alias stands for, and its attachment's hash
    /// is an alias, the same in every frame.
    #[test]
    fn a_frame_keeps_its_length_and_aliases_its_hashes() {
        let hash = vec![0xab; 32];
        let step = |key: &str| Step {
            items: vec![wire::Item {
                key: key.to_owned(),
                text: "a zebra".to_owned(),
                attachments: vec![wire::Attachment {
                    of: Some(wire::attachment::Of::Image(wire::BlobRef {
                        hash: hash.clone(),
                        name: "zebra.png".to_owned(),
                        mime: "image/png".to_owned(),
                        size: 3,
                    })),
                }],
                ..Default::default()
            }],
            snapshot: Some(wire::Snapshot {
                working_on: Some("a zebra".to_owned()),
                git: Some(wire::Git {
                    branch: Some("main".to_owned()),
                    base_branch: Some("m".to_owned()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let structure = Structure::new();
        let mut aliased = Vec::new();
        for key in ["first", "second"] {
            let live = journal::encode_frame(&step(key));
            let frame = fitted(&structure, &step(key), live.len()).expect("the frame fits");
            assert_eq!(frame.len(), live.len());
            let journal::Frame::Whole(recorded, _) = journal::decode_frame(&frame) else {
                panic!("the frame decodes");
            };
            let git = recorded.snapshot.as_ref().unwrap().git.clone().unwrap();
            assert_eq!(git.branch.as_ref().map(String::len), Some(4));
            assert_ne!(git.branch.as_deref(), Some("main"));
            assert_eq!(git.base_branch.as_ref().map(String::len), Some(1));
            let Some(wire::attachment::Of::Image(blob)) = &recorded.items[0].attachments[0].of
            else {
                panic!("the attachment is kept");
            };
            assert_eq!(blob.hash.len(), hash.len());
            assert_ne!(blob.hash, hash);
            aliased.push(blob.hash.clone());
        }
        assert_eq!(aliased[0], aliased[1], "one hash, one alias");
    }

    #[test]
    fn a_record_whose_store_passes_the_limit_is_counted_and_not_written() {
        // The window is empty, so the ring's lines fit; the store copy is
        // what passes the limit, its rows keyed by ids that do not compress.
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("store.sqlite");
        let host = vec![1; 16];
        let mut store = store::Sqlite::open(&scratch, host.clone()).unwrap();
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        for _ in 0..6_000 {
            let mut agent = vec![0; 16];
            for byte in &mut agent {
                seed = seed
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                *byte = (seed >> 56) as u8;
            }
            let row = AgentRow::new(
                store::AgentKey::new(host.clone(), agent),
                wire::Kind::ClaudeSdk,
                "/src/amux",
            );
            store.put_agent(&row).unwrap();
        }
        let daemon = daemon_core::Daemon::new(store, daemon_core::StartOptions::default());
        let captured = Captured {
            checkpoint: daemon.checkpoint(),
            lines: Vec::new(),
        };
        drop(daemon);
        let out = dir.path().join(CORE);
        let core =
            write_core(&captured, &scratch, &out, &[], LIMIT).expect("the record is counted");
        assert!(!core.kept && core.bytes > LIMIT, "{}", core.bytes);
        assert!(!out.exists(), "nothing of the record is written");
    }
}
