//! Every recorded ACP line, in both directions, decodes with nothing left
//! unknown and encodes back to the same JSON, and every answer reads as the
//! answer to the request it answers. A failure here is an agent changing
//! what it writes, or a type that does not match what it writes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use acp_protocol::{AgentMessage, AgentResponse, ClientMessage, RequestId};
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

/// One recording's lines: where each came from, whether the agent wrote
/// it, and the line.
type Recording = Vec<(String, bool, String)>;

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
            let record: Value = serde_json::from_str(record).expect("record is JSON");
            let line = record["line"].as_str().expect("line").to_owned();
            let from_agent = match record["dir"].as_str() {
                Some("stdout") => true,
                Some("stdin") => false,
                other => panic!("{name}:{}: direction {other:?}", number + 1),
            };
            lines.push((format!("{name}:{}", number + 1), from_agent, line));
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
    for (place, from_agent, line) in &all {
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
    for (place, from_agent, line) in lines() {
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

/// Every agent answer to one of amux's requests in `recording`, read as
/// the answer to that request: a failure per answer that does not.
fn misread_responses(recording: &Recording) -> Vec<String> {
    let mut failures = Vec::new();
    let mut asked: HashMap<RequestId, String> = HashMap::new();
    for (place, from_agent, line) in recording {
        if !from_agent {
            if let Ok(ClientMessage::Request { id, request, .. }) =
                acp_protocol::decode_client(line.as_bytes())
            {
                asked.insert(id, request.method().to_owned());
            }
            continue;
        }
        let Ok(AgentMessage::Response { id, result, .. }) = acp_protocol::decode(line.as_bytes())
        else {
            continue;
        };
        let Ok(result) = result else { continue };
        let Some(method) = asked.get(&id) else {
            failures.push(format!("{place}: answers {id}, which amux never asked"));
            continue;
        };
        match AgentResponse::strict_read(method, &result) {
            None => failures.push(format!("{place}: {method} has no answer type")),
            Some(Err(error)) => failures.push(format!("{place}: {method}: {error}")),
            Some(Ok(answer)) => {
                let again = serde_json::to_value(&answer).expect("answers serialize");
                if again != result {
                    let mut differences = Vec::new();
                    differ("", &result, &again, &mut differences);
                    failures.push(format!("{place}: {method} {}", differences.join(", ")));
                }
            }
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
