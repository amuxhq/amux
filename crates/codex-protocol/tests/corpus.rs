//! Every recorded Codex line, in both directions, decodes with nothing left
//! unknown and encodes back to the same JSON. A failure here is provider
//! drift, or a type that does not match what Codex writes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use codex_protocol::server::{
    AccountReadResponse, BackgroundTerminalsResponse, InitializeResponse, ModelListResponse,
    SkillsListResponse, ThreadReadResponse, ThreadResponse, TurnStartResponse, TurnSteerResponse,
};
use codex_protocol::{ClientMessage, RequestId, ServerMessage};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every io.jsonl recorded from Codex: the spec corpus and the live
/// captures beside it.
fn recordings() -> Vec<PathBuf> {
    let mut found = Vec::new();
    for dir in [
        "crates/codex-specs/fixtures/runtime",
        "crates/codex-specs/fixtures/live",
    ] {
        let mut entries: Vec<_> = std::fs::read_dir(root().join(dir))
            .unwrap_or_else(|error| panic!("{dir}: {error}"))
            .map(|entry| entry.expect("entry").path())
            .filter(|path| path.join("io.jsonl").is_file())
            .collect();
        entries.sort();
        for recording in entries {
            let manifest: Value = serde_json::from_slice(
                &std::fs::read(recording.join("manifest.json")).expect("manifest.json"),
            )
            .expect("manifest is JSON");
            if manifest["recorded"]["provider"] == "codex" {
                found.push(recording.join("io.jsonl"));
            }
        }
    }
    assert!(
        found.len() >= 29,
        "found only {} Codex recordings",
        found.len()
    );
    found
}

/// One recording's lines: where each came from, which way it went, and
/// the line.
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
            let from_server = match record["dir"].as_str() {
                Some("stdout") => true,
                Some("stdin") => false,
                other => panic!("{name}:{}: direction {other:?}", number + 1),
            };
            lines.push((format!("{name}:{}", number + 1), from_server, line));
        }
        recorded.push(lines);
    }
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
    for (place, from_server, line) in &all {
        let result = if *from_server {
            codex_protocol::strict(line.as_bytes()).map(drop)
        } else {
            codex_protocol::strict_client(line.as_bytes()).map(drop)
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
    for (place, from_server, line) in lines() {
        let encoded = if from_server {
            codex_protocol::encode_server(
                &codex_protocol::decode(line.as_bytes()).expect("decodes"),
            )
        } else {
            codex_protocol::encode(
                &codex_protocol::decode_client(line.as_bytes()).expect("decodes"),
            )
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

/// Methods whose answer Codex writes as an empty object, so there is no
/// response type to hold it to.
const ANSWERED_EMPTY: &[&str] = &[
    "thread/name/set",
    "thread/compact/start",
    "thread/inject_items",
    "turn/interrupt",
];

/// Reads `result` strictly as `T` and encodes it back.
fn reread<T: DeserializeOwned + Serialize>(result: &Value) -> Result<Value, String> {
    let typed: T = codex_protocol::strict_result(result).map_err(|error| error.to_string())?;
    Ok(serde_json::to_value(&typed).expect("responses serialize"))
}

/// Reads a result as the response type of `method`, the request it answers.
fn reread_as(method: &str, result: &Value) -> Result<Value, String> {
    match method {
        "initialize" => reread::<InitializeResponse>(result),
        "thread/start" | "thread/resume" => reread::<ThreadResponse>(result),
        "turn/start" => reread::<TurnStartResponse>(result),
        "turn/steer" => reread::<TurnSteerResponse>(result),
        "model/list" => reread::<ModelListResponse>(result),
        "skills/list" => reread::<SkillsListResponse>(result),
        "thread/read" => reread::<ThreadReadResponse>(result),
        "account/read" => reread::<AccountReadResponse>(result),
        "thread/backgroundTerminals/list" => reread::<BackgroundTerminalsResponse>(result),
        method if ANSWERED_EMPTY.contains(&method) => match result {
            Value::Object(object) if object.is_empty() => Ok(result.clone()),
            _ => Err(format!("{method} answered with more than an empty object")),
        },
        method => Err(format!("{method} has no response type in this test")),
    }
}

/// Every server answer to one of amux's requests in `recording`, read as
/// that request's response type: a failure per answer that does not.
fn misread_responses(recording: &Recording) -> Vec<String> {
    let mut failures = Vec::new();
    let mut asked: HashMap<RequestId, String> = HashMap::new();
    for (place, from_server, line) in recording {
        if !from_server {
            if let Ok(ClientMessage::Request { id, request, .. }) =
                codex_protocol::decode_client(line.as_bytes())
            {
                asked.insert(id, request.method().to_owned());
            }
            continue;
        }
        let Ok(ServerMessage::Response { id, result, .. }) =
            codex_protocol::decode(line.as_bytes())
        else {
            continue;
        };
        let Ok(result) = result else { continue };
        let Some(method) = asked.get(&id) else {
            failures.push(format!("{place}: answers {id}, which amux never asked"));
            continue;
        };
        match reread_as(method, &result) {
            Ok(again) if again == result => {}
            Ok(again) => {
                let mut differences = Vec::new();
                differ("", &result, &again, &mut differences);
                failures.push(format!("{place}: {method} {}", differences.join(", ")));
            }
            Err(error) => failures.push(format!("{place}: {method}: {error}")),
        }
    }
    failures
}

#[test]
fn every_recorded_response_reads_as_its_requests_response_type() {
    let recorded = recorded();
    let failures: Vec<_> = recorded.iter().flat_map(misread_responses).collect();
    assert!(
        failures.is_empty(),
        "{} responses do not read as their request's response type:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn a_recorded_response_missing_a_field_is_refused() {
    let mut recording = recorded()
        .into_iter()
        .find(|recording| {
            recording
                .iter()
                .any(|(_, _, line)| line.contains("\"displayName\""))
        })
        .expect("a recording answers model/list");
    let (place, _, line) = recording
        .iter_mut()
        .find(|(_, from_server, line)| *from_server && line.contains("\"displayName\""))
        .expect("the model/list answer");
    let mut answer: Value = serde_json::from_str(line).expect("line is JSON");
    let model = answer["result"]["data"][0]
        .as_object_mut()
        .expect("a model");
    model
        .remove("isDefault")
        .expect("the model says whether it is the default");
    *line = answer.to_string();
    let place = place.clone();

    let failures = misread_responses(&recording);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].starts_with(&format!("{place}: model/list")),
        "{failures:?}"
    );
    assert!(failures[0].contains("isDefault"), "{failures:?}");
}

#[test]
fn a_recorded_response_with_an_unknown_spelling_is_refused() {
    let mut recording = recorded()
        .into_iter()
        .find(|recording| {
            recording
                .iter()
                .any(|(_, _, line)| line.contains("\"displayName\""))
        })
        .expect("a recording answers model/list");
    let (place, _, line) = recording
        .iter_mut()
        .find(|(_, from_server, line)| *from_server && line.contains("\"displayName\""))
        .expect("the model/list answer");
    let mut answer: Value = serde_json::from_str(line).expect("line is JSON");
    answer["result"]["data"][0]["defaultReasoningEffort"] = Value::from("ludicrous");
    *line = answer.to_string();
    let place = place.clone();

    let failures = misread_responses(&recording);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].starts_with(&format!("{place}: model/list")),
        "{failures:?}"
    );
    assert!(failures[0].contains("ludicrous"), "{failures:?}");
}

#[test]
fn an_invented_method_is_unknown_and_fails_strict() {
    let line =
        br#"{"method":"thread/teleported","params":{"threadId":"t","to":"mars"},"emittedAtMs":1}"#;
    match codex_protocol::decode(line).expect("an object decodes") {
        codex_protocol::ServerMessage::Unknown(unknown) => {
            assert_eq!(unknown.method.as_deref(), Some("thread/teleported"));
            assert_eq!(unknown.raw.get().as_bytes(), line);
        }
        other => panic!("decoded as {other:?}"),
    }
    let drift = codex_protocol::strict(line).expect_err("strict refuses it");
    assert!(drift.to_string().contains("thread/teleported"), "{drift}");
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
