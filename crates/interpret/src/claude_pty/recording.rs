//! Recorded Claude terminal sessions as interpreter events.
//!
//! Two formats:
//!
//! - `claude_pty_io`: a live provider recording (`io.jsonl`). Each
//!   line is `{dir, line, transport_id, us}`; hook lines carry the payload,
//!   transcript lines `{path, row}`, and PTY bytes are skipped. A
//!   `subagent` line is what Claude wrote for one of its subagents: a row
//!   of its transcript (`{path, row}`), read as the agent process would
//!   deliver it, or its whole `meta.json` (`{path, meta}`), read as the
//!   agent process reports it. The agent process reports a file's meta
//!   before its first row, so a meta recorded after rows of its file is
//!   delivered ahead of them, and a file reads as drained once its last
//!   recorded row is delivered: an end that waits for the agent process to
//!   read the file to its end resolves there, as it would live. Time is
//!   microseconds since the recording
//!   began, placed on the wall clock the transcript's own timestamps use.
//! - `transcript_rows`: one transcript row per line, hook payloads mixed in
//!   (they carry `hook_event_name`); time comes from row timestamps.
//!
//! Every fact is preceded by a tick when the clock moved.

use claude_protocol::hooks::Payload;
use claude_protocol::transcript::Row;
use serde::Deserialize;
use serde_json::Value;

use crate::claude_common::timestamp_ms;
use crate::{Channel, Event, Fact};

/// Written into transcripts by an earlier tailer; not Claude's.
const TAILER_MARKER: &str = "amux.transcript_ready";

/// One line of a live recording: when, on which transport, and
/// what.
#[derive(Deserialize)]
struct Record {
    #[serde(default)]
    us: i64,
    #[serde(default)]
    transport_id: String,
    #[serde(default)]
    line: Option<String>,
}

/// What the recording host read from the transcript: the file and the row.
#[derive(Deserialize)]
struct TranscriptLine {
    row: Value,
}

/// What the recording host read for a subagent: its file, and a row of it
/// or its `meta.json`.
#[derive(Deserialize)]
struct SubagentLine {
    path: String,
    #[serde(default)]
    row: Option<Value>,
    #[serde(default)]
    meta: Option<Value>,
}

/// The subagent a recorded path is for: `agent-<id>.jsonl` or
/// `agent-<id>.meta.json`.
fn subagent_of(path: &str) -> Option<&str> {
    let name = path.rsplit('/').next()?;
    let name = name.strip_prefix("agent-")?;
    name.strip_suffix(".meta.json")
        .or_else(|| name.strip_suffix(".jsonl"))
}

pub(super) fn read(format: &str, bytes: &[u8]) -> Result<Vec<Event>, String> {
    let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
    let lines = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate();
    match format {
        "claude_pty_io" => {
            let records = lines
                .map(|(index, line)| {
                    serde_json::from_str::<Record>(line)
                        .map_err(|error| format!("line {}: {error}", index + 1))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(io(&records))
        }
        "transcript_rows" => {
            let rows = lines
                .map(|(index, line)| {
                    serde_json::from_str::<Value>(line)
                        .map_err(|error| format!("line {}: {error}", index + 1))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows_and_hooks(&rows))
        }
        other => Err(format!(
            "claude_pty reads claude_pty_io and transcript_rows, not {other:?}"
        )),
    }
}

/// A transcript row as written, decoded, unless it is the tailer's marker.
fn claude_row(row: &Value) -> Option<Row> {
    Row::deserialize(row)
        .ok()
        .filter(|row| row.kind() != TAILER_MARKER)
}

fn row_ms(row: &Row) -> Option<i64> {
    row.timestamp().and_then(timestamp_ms)
}

struct Clock {
    events: Vec<Event>,
    now: Option<i64>,
}

impl Clock {
    fn push(&mut self, at_ms: Option<i64>, channel: Channel, payload: Vec<u8>) {
        if let Some(at_ms) = at_ms
            && self.now.is_none_or(|now| at_ms > now)
        {
            self.now = Some(at_ms);
            self.events.push(Event::Tick { at_ms });
        }
        self.events.push(Event::Fact(Fact { channel, payload }));
    }
}

fn io(records: &[Record]) -> Vec<Event> {
    let row = |record: &Record| {
        if record.transport_id != "transcript" {
            return None;
        }
        serde_json::from_str::<TranscriptLine>(record.line.as_deref()?)
            .ok()
            .map(|line| line.row)
    };
    // Where the recording's zero sits on the wall clock: the first row
    // with a timestamp, less its offset.
    let origin = records
        .iter()
        .find_map(|record| {
            let row = Row::deserialize(row(record)?).ok()?;
            Some(row_ms(&row)? - record.us / 1000)
        })
        .unwrap_or(0);
    let subagent = |record: &Record| {
        if record.transport_id != "subagent" {
            return None;
        }
        let line = serde_json::from_str::<SubagentLine>(record.line.as_deref()?).ok()?;
        let agent_id = subagent_of(&line.path)?.to_owned();
        Some((agent_id, line))
    };
    let mut clock = Clock {
        events: Vec::new(),
        now: None,
    };
    // Each subagent's last recorded row, after which its file reads as
    // drained.
    let mut last_rows = std::collections::BTreeMap::new();
    for (index, record) in records.iter().enumerate() {
        if let Some((agent_id, line)) = subagent(record)
            && line.row.is_some()
        {
            last_rows.insert(agent_id, index);
        }
    }
    // The subagents whose meta went out, and the meta lines already sent
    // ahead of the rows they describe.
    let mut met = std::collections::BTreeSet::new();
    let mut sent_ahead = std::collections::BTreeSet::new();
    // A recording that spans a relaunch counts each process's time from its
    // own zero: time carries on from where the last process left it. The
    // transports' lines also land a little out of order, which is no
    // relaunch.
    const RELAUNCH_US: i64 = 1_000_000;
    let mut carried = 0;
    let mut last_us = 0;
    for (index, record) in records.iter().enumerate() {
        if record.us + RELAUNCH_US < last_us {
            carried += last_us - record.us;
        }
        last_us = last_us.max(record.us);
        let at_ms = Some(origin + (carried + record.us) / 1000);
        match record.transport_id.as_str() {
            "hook" => {
                let Some(payload) = &record.line else {
                    continue;
                };
                clock.push(at_ms, Channel::Hook, payload.as_bytes().to_vec());
            }
            "transcript" => {
                let Some(row) = row(record).filter(|row| claude_row(row).is_some()) else {
                    continue;
                };
                clock.push(at_ms, Channel::Transcript, row.to_string().into_bytes());
            }
            "subagent" => {
                let Some((agent_id, line)) = subagent(record) else {
                    continue;
                };
                if let Some(meta) = line.meta {
                    if sent_ahead.remove(&index) {
                        continue;
                    }
                    met.insert(agent_id.clone());
                    clock.push(
                        at_ms,
                        Channel::Agent,
                        super::subagent_meta_fact(&agent_id, meta),
                    );
                } else if let Some(row) = line.row {
                    if met.insert(agent_id.clone())
                        && let Some((ahead, meta)) = records
                            .iter()
                            .enumerate()
                            .skip(index + 1)
                            .find_map(|(at, later)| {
                                let (id, line) = subagent(later)?;
                                (id == agent_id).then_some((at, line.meta?))
                            })
                    {
                        sent_ahead.insert(ahead);
                        clock.push(
                            at_ms,
                            Channel::Agent,
                            super::subagent_meta_fact(&agent_id, meta),
                        );
                    }
                    let row = row.to_string().into_bytes();
                    if super::subagent_row_kept(&row) {
                        clock.push(at_ms, Channel::SubagentTranscript, row);
                    }
                    if last_rows.get(&agent_id) == Some(&index) {
                        clock.push(
                            at_ms,
                            Channel::Agent,
                            super::subagent_drained_fact(&agent_id),
                        );
                    }
                }
            }
            _ => {}
        }
    }
    clock.events
}

/// Rows with hook payloads mixed in, told apart by the hook's event name.
fn rows_and_hooks(lines: &[Value]) -> Vec<Event> {
    let mut clock = Clock {
        events: Vec::new(),
        now: None,
    };
    for line in lines {
        let payload = line.to_string().into_bytes();
        let hook = Payload::deserialize(line).is_ok_and(|hook| !hook.name().is_empty());
        if hook {
            clock.push(None, Channel::Hook, payload);
        } else if let Some(row) = claude_row(line) {
            clock.push(row_ms(&row), Channel::Transcript, payload);
        }
    }
    clock.events
}
