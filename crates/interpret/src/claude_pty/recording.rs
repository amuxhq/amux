//! Recorded Claude terminal sessions as interpreter events.
//!
//! Two formats:
//!
//! - `claude_pty_io`: a live provider recording (`io.jsonl`). Each
//!   line is `{dir, line, transport_id, us}`; hook lines carry the payload,
//!   transcript lines `{path, row}`, and PTY bytes are skipped. Time is
//!   microseconds since the recording began, placed on the wall clock the
//!   transcript's own timestamps use.
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
    let mut clock = Clock {
        events: Vec::new(),
        now: None,
    };
    for record in records {
        let at_ms = Some(origin + record.us / 1000);
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
