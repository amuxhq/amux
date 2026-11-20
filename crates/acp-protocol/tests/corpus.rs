//! Every recorded ACP line, in both directions, decodes with nothing left
//! unknown and encodes back to the same JSON, and every answer reads as the
//! answer to the request it answers. A failure here is an agent changing
//! what it writes, or a type that does not match what it writes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use acp_protocol::{AgentMessage, AgentResponse, ClientMessage, ClientResponse, RequestId};
use serde_json::Value;

/// The agents whose recordings are held to this crate.
const AGENTS: &[&str] = &["antigravity", "opencode", "grok"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every io.jsonl recorded from an ACP agent: the live lane's captures.
fn recordings() -> Vec<PathBuf> {
    let live = root().join("crates/interpret/fixtures/acp/live");
    let Ok(entries) = std::fs::read_dir(&live) else {
        return Vec::new();
    };
    let mut entries: Vec<_> = entries
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.join("io.jsonl").is_file())
        .collect();
    entries.sort();
    let mut found = Vec::new();
    for recording in entries {
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(recording.join("manifest.json")).expect("manifest.json"),
        )
        .expect("manifest is JSON");
        if AGENTS
            .iter()
            .any(|agent| manifest["recorded"]["provider"] == *agent)
        {
            found.push(recording.join("io.jsonl"));
        }
    }
    found
}

/// One recorded line: where in the corpus it is, the agent process that
/// read or wrote it (a recording may span a restart), whether the agent
/// wrote it, and the line.
struct Line {
    place: String,
    process: String,
    from_agent: bool,
    line: String,
}

type Recording = Vec<Line>;

/// Each recording's lines, in the order they were written.
fn recorded() -> Vec<Recording> {
    let mut recorded = Vec::new();
    for path in recordings() {
        let name = path
            .strip_prefix(root())
            .unwrap_or(&path)
            .display()
            .to_string();
        let text = std::fs::read_to_string(&path).expect("recording");
        let mut lines = Vec::new();
        for (number, record) in text.lines().enumerate() {
            let place = format!("{name}:{}", number + 1);
            let record: Value = serde_json::from_str(record).expect("record is JSON");
            let line = record["line"].as_str().expect("line").to_owned();
            let from_agent = match record["dir"].as_str() {
                Some("stdout") => true,
                Some("stdin") => false,
                other => panic!("{place}: direction {other:?}"),
            };
            let process = match &record["process"] {
                Value::String(process) => process.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            lines.push(Line {
                place,
                process,
                from_agent,
                line,
            });
        }
        recorded.push(lines);
    }
    eprintln!(
        "checked {} ACP recordings, {} lines",
        recorded.len(),
        recorded.iter().map(Vec::len).sum::<usize>()
    );
    recorded
}

/// Every recorded line.
fn lines() -> Recording {
    recorded().into_iter().flatten().collect()
}

#[test]
fn every_recorded_line_is_known() {
    let mut failures = Vec::new();
    let all = lines();
    for Line {
        place,
        from_agent,
        line,
        ..
    } in &all
    {
        let result = if *from_agent {
            acp_protocol::strict(line.as_bytes()).map(drop)
        } else {
            acp_protocol::strict_client(line.as_bytes()).map(drop)
        };
        if let Err(drift) = result {
            failures.push(format!("{place}: {drift}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} lines are not known:\n{}",
        failures.len(),
        all.len(),
        failures.join("\n")
    );
}

#[test]
fn every_recorded_line_encodes_back_to_the_same_json() {
    let mut failures = Vec::new();
    for Line {
        place,
        from_agent,
        line,
        ..
    } in lines()
    {
        let encoded = if from_agent {
            acp_protocol::encode_server(&acp_protocol::decode(line.as_bytes()).expect("decodes"))
        } else {
            acp_protocol::encode(&acp_protocol::decode_client(line.as_bytes()).expect("decodes"))
        };
        let written: Value = serde_json::from_str(&line).expect("line is JSON");
        let again: Value = serde_json::from_slice(&encoded).expect("encoded is JSON");
        if written != again {
            let mut differences = Vec::new();
            differ("", &written, &again, &mut differences);
            failures.push(format!("{place}: {}", differences.join(", ")));
        }
    }
    assert!(
        failures.is_empty(),
        "{} lines changed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The method a line asks, with the id it is to be answered by: a
/// request of either side, a vendor's own included.
fn asked(line: &Line) -> Option<(RequestId, String)> {
    let bytes = line.line.as_bytes();
    if line.from_agent {
        match acp_protocol::decode(bytes).ok()? {
            AgentMessage::Request { id, request, .. } => Some((id, request.method().to_owned())),
            AgentMessage::Extension { extension, .. } => Some((extension.id?, extension.method)),
            _ => None,
        }
    } else {
        match acp_protocol::decode_client(bytes).ok()? {
            ClientMessage::Request { id, request, .. } => Some((id, request.method().to_owned())),
            ClientMessage::Extension { extension, .. } => Some((extension.id?, extension.method)),
            _ => None,
        }
    }
}

/// The result a line answers with, and its id; None for a line that is no
/// answer. An error answer has no result to read.
fn answered(line: &Line) -> Option<(RequestId, Option<Value>)> {
    let bytes = line.line.as_bytes();
    if line.from_agent {
        match acp_protocol::decode(bytes).ok()? {
            AgentMessage::Response { id, result, .. } => Some((id, result.ok())),
            _ => None,
        }
    } else {
        match acp_protocol::decode_client(bytes).ok()? {
            ClientMessage::Response { id, response, .. } => {
                let result = response
                    .ok()
                    .map(|response| serde_json::to_value(response).expect("answers serialize"));
                Some((id, result))
            }
            _ => None,
        }
    }
}

/// Reads `result` strictly as the answer to `method`, and writes it back.
fn reread(from_agent: bool, method: &str, result: &Value) -> Result<Value, String> {
    let answer = if from_agent {
        AgentResponse::strict_read(method, result)
            .map(|answer| answer.map(|answer| serde_json::to_value(answer).unwrap()))
    } else {
        ClientResponse::strict_read(method, result)
            .map(|answer| answer.map(|answer| serde_json::to_value(answer).unwrap()))
    };
    match answer {
        None => Err(format!("{method} has no answer type")),
        Some(Err(error)) => Err(format!("{method}: {error}")),
        Some(Ok(again)) => Ok(again),
    }
}

/// Every answer in `recording`, from either side, read as the answer to
/// the request it answers: the one the other side sent with its id, in the
/// same agent process. A failure per answer that does not read, or answers
/// nothing.
fn misread_responses(recording: &Recording) -> Vec<String> {
    let mut failures = Vec::new();
    // Requests not yet answered, by process, the side that asked, and id.
    let mut open: HashMap<(String, bool, RequestId), String> = HashMap::new();
    for line in recording {
        if let Some((id, method)) = asked(line) {
            open.insert((line.process.clone(), line.from_agent, id), method);
            continue;
        }
        let Some((id, result)) = answered(line) else {
            continue;
        };
        let place = &line.place;
        let asker = if line.from_agent { "amux" } else { "the agent" };
        let Some(method) = open.remove(&(line.process.clone(), !line.from_agent, id.clone()))
        else {
            failures.push(format!(
                "{place}: answers {id}, which {asker} never asked in process {:?}",
                line.process
            ));
            continue;
        };
        let Some(result) = result else { continue };
        match reread(line.from_agent, &method, &result) {
            Ok(again) if again == result => {}
            Ok(again) => {
                let mut differences = Vec::new();
                differ("", &result, &again, &mut differences);
                failures.push(format!("{place}: {method} {}", differences.join(", ")));
            }
            Err(error) => failures.push(format!("{place}: {error}")),
        }
    }
    failures
}

#[test]
fn every_recorded_response_reads_as_its_requests_answer() {
    let failures: Vec<_> = recorded().iter().flat_map(misread_responses).collect();
    assert!(
        failures.is_empty(),
        "{} answers do not read as their request's answer:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Where two JSON values differ, as paths with what each side holds.
fn differ(path: &str, written: &Value, encoded: &Value, out: &mut Vec<String>) {
    match (written, encoded) {
        (Value::Object(a), Value::Object(b)) => {
            for key in a.keys().chain(b.keys().filter(|key| !a.contains_key(*key))) {
                let inner = format!("{path}.{key}");
                match (a.get(key), b.get(key)) {
                    (Some(x), Some(y)) => differ(&inner, x, y, out),
                    (x, y) => out.push(format!("{inner} written {x:?} encoded {y:?}")),
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (index, (x, y)) in a.iter().zip(b).enumerate() {
                differ(&format!("{path}[{index}]"), x, y, out);
            }
        }
        (a, b) if a != b => out.push(format!("{path} written {a} encoded {b}")),
        _ => {}
    }
}
