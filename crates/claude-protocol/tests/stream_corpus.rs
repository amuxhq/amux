//! Every line of every headless Claude recording, in both directions,
//! decodes with nothing left unknown and encodes back to the same JSON. A
//! failure here is provider drift, or a type that does not match what Claude
//! writes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use claude_protocol::stream::control::ControlOutcome;
use claude_protocol::stream::init::ContextUsage;
use claude_protocol::stream::{
    self, ControlResponse, ElicitationResult, HookOutput, InitializationResult, Input,
    InterruptResult, McpPermissionModeOverrideResult, McpSetServersResult, McpStatusResult, Output,
    PermissionResult, ReloadPluginsResult, ReloadSkillsResult, RewindFilesResult,
    SetPermissionModeResult, SettingsResult, UserDialogResult,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../claude-specs/fixtures")
}

/// The registered corpus and the live captures beside it.
const ROOTS: [&str; 2] = ["sdk", "live/sdk"];

/// One recording's lines: where each came from, whether Claude wrote it,
/// and the line.
type Recording = Vec<(String, bool, String)>;

/// Each recording's lines, in the order they were written.
fn recorded() -> Vec<Recording> {
    let mut recordings = Vec::new();
    for root in ROOTS {
        let mut found: Vec<_> = std::fs::read_dir(fixtures().join(root))
            .unwrap_or_else(|error| panic!("fixtures/{root}: {error}"))
            .map(|entry| entry.expect("entry").path().join("io.jsonl"))
            .filter(|path| path.is_file())
            .collect();
        found.sort();
        recordings.extend(found);
    }
    assert!(
        recordings.len() >= 36,
        "found only {} recordings",
        recordings.len()
    );
    let mut recorded = Vec::new();
    for path in recordings {
        let name = path
            .strip_prefix(fixtures())
            .map(|relative| format!("fixtures/{}", relative.display()))
            .expect("a recording under fixtures");
        let text = std::fs::read_to_string(&path).expect("recording");
        let mut lines = Vec::new();
        for (number, record) in text.lines().enumerate() {
            let record: Value = serde_json::from_str(record).expect("record is JSON");
            let line = record["line"].as_str().expect("line").to_owned();
            let from_claude = match record["dir"].as_str() {
                Some("stdout") => true,
                Some("stdin") => false,
                other => panic!("{name}:{}: direction {other:?}", number + 1),
            };
            lines.push((format!("{name}:{}", number + 1), from_claude, line));
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
    for (place, from_claude, line) in &all {
        let result = if *from_claude {
            stream::strict(line.as_bytes()).map(drop)
        } else {
            stream::strict_input(line.as_bytes()).map(drop)
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
    for (place, from_claude, line) in lines() {
        let encoded = if from_claude {
            stream::encode_output(&stream::decode(line.as_bytes()).expect("decodes"))
        } else {
            stream::encode(&stream::decode_input(line.as_bytes()).expect("decodes"))
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

/// Requests whose answer this crate keeps as plain JSON, with no result
/// type to hold it to.
const UNTYPED_ANSWERS: &[&str] = &[
    "mcp_message",
    "background_tasks",
    "stop_task",
    "apply_flag_settings",
    "mcp_reconnect",
    "mcp_toggle",
    "seed_read_state",
    "set_model",
];

/// Reads an answer strictly as `T` and encodes it back.
fn reread<T: DeserializeOwned + Serialize>(
    answer: &ControlResponse,
) -> Option<Result<Value, String>> {
    Some(
        answer
            .strict_result::<T>()?
            .map(|typed| serde_json::to_value(&typed).expect("results serialize"))
            .map_err(|drift| drift.to_string()),
    )
}

/// Reads an answer as the result type of `subtype`, the request it answers.
/// None when the answer carries no payload.
fn reread_as(subtype: &str, answer: &ControlResponse) -> Option<Result<Value, String>> {
    match subtype {
        // What Claude answers amux.
        "initialize" => reread::<InitializationResult>(answer),
        "interrupt" => reread::<InterruptResult>(answer),
        "set_permission_mode" => reread::<SetPermissionModeResult>(answer),
        "set_mcp_permission_mode_override" => reread::<McpPermissionModeOverrideResult>(answer),
        "mcp_set_servers" => reread::<McpSetServersResult>(answer),
        "get_context_usage" => reread::<ContextUsage>(answer),
        "get_settings" => reread::<SettingsResult>(answer),
        "mcp_status" => reread::<McpStatusResult>(answer),
        "reload_plugins" => reread::<ReloadPluginsResult>(answer),
        "reload_skills" => reread::<ReloadSkillsResult>(answer),
        "rewind_files" => reread::<RewindFilesResult>(answer),
        // What amux answers Claude.
        "can_use_tool" => reread::<PermissionResult>(answer),
        "hook_callback" => reread::<HookOutput>(answer),
        "elicitation" => reread::<ElicitationResult>(answer),
        "request_user_dialog" => reread::<UserDialogResult>(answer),
        subtype if UNTYPED_ANSWERS.contains(&subtype) => None,
        subtype => Some(Err(format!("{subtype} has no result type in this test"))),
    }
}

/// Every successful answer in `recording`, in either direction, read as
/// the result type of the request it answers: a failure per answer that
/// does not.
fn misread_answers(recording: &Recording) -> Vec<String> {
    let mut failures = Vec::new();
    // Requests by who asked and their id.
    let mut asked: HashMap<(bool, String), String> = HashMap::new();
    for (place, from_claude, line) in recording {
        let (request, answer) = if *from_claude {
            match stream::decode(line.as_bytes()).expect("decodes") {
                Output::ControlRequest(request) => (Some(request), None),
                Output::ControlResponse(answer) => (None, Some(answer)),
                _ => continue,
            }
        } else {
            match stream::decode_input(line.as_bytes()).expect("decodes") {
                Input::ControlRequest(request) => (Some(request), None),
                Input::ControlResponse(answer) => (None, Some(answer)),
                _ => continue,
            }
        };
        if let Some(request) = request {
            asked.insert(
                (*from_claude, request.request_id.clone()),
                request.request.kind().to_owned(),
            );
            continue;
        }
        let Some(answer) = answer else { continue };
        if answer.response.subtype != ControlOutcome::Success {
            continue;
        }
        // Started with --replay-user-messages, Claude echoes the host's
        // answer to its own request back on stdout.
        let id = answer.request_id().to_owned();
        let asked_of = asked
            .get(&(!from_claude, id.clone()))
            .or_else(|| asked.get(&(true, id)).filter(|_| *from_claude));
        let Some(subtype) = asked_of else {
            failures.push(format!(
                "{place}: answers {}, which nobody asked",
                answer.request_id()
            ));
            continue;
        };
        let written = answer.response.response.clone().unwrap_or(Value::Null);
        match reread_as(subtype, &answer) {
            None => {}
            Some(Ok(again)) if again == written => {}
            Some(Ok(again)) => {
                let mut differences = Vec::new();
                differ("", &written, &again, &mut differences);
                failures.push(format!("{place}: {subtype} {}", differences.join(", ")));
            }
            Some(Err(error)) => failures.push(format!("{place}: {subtype}: {error}")),
        }
    }
    failures
}

#[test]
fn every_recorded_answer_reads_as_its_requests_result_type() {
    let recorded = recorded();
    let failures: Vec<_> = recorded.iter().flat_map(misread_answers).collect();
    assert!(
        failures.is_empty(),
        "{} answers do not read as their request's result type:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn a_recorded_answer_missing_a_field_is_refused() {
    let needle = "\"still_queued\"";
    let mut recording = recorded()
        .into_iter()
        .find(|recording| recording.iter().any(|(_, _, line)| line.contains(needle)))
        .expect("a recording answers interrupt");
    let (place, _, line) = recording
        .iter_mut()
        .find(|(_, _, line)| line.contains(needle))
        .expect("the interrupt answer");
    let mut answer: Value = serde_json::from_str(line).expect("line is JSON");
    answer["response"]["response"]
        .as_object_mut()
        .expect("a payload")
        .remove("still_queued");
    *line = answer.to_string();
    let place = place.clone();

    let failures = misread_answers(&recording);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].starts_with(&format!("{place}: interrupt")),
        "{failures:?}"
    );
    assert!(failures[0].contains("still_queued"), "{failures:?}");
}

#[test]
fn a_recorded_answer_with_an_unknown_spelling_is_refused() {
    let needle = "\"behavior\":\"deny\"";
    let mut recording = recorded()
        .into_iter()
        .find(|recording| recording.iter().any(|(_, _, line)| line.contains(needle)))
        .expect("a recording refuses a tool");
    let (place, _, line) = recording
        .iter_mut()
        .find(|(_, _, line)| line.contains(needle))
        .expect("the refusal");
    let mut answer: Value = serde_json::from_str(line).expect("line is JSON");
    answer["response"]["response"]["behavior"] = Value::from("shrug");
    *line = answer.to_string();
    let place = place.clone();

    let failures = misread_answers(&recording);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].starts_with(&format!("{place}: can_use_tool")),
        "{failures:?}"
    );
    assert!(failures[0].contains("shrug"), "{failures:?}");
}

#[test]
fn an_invented_type_is_unknown_and_fails_strict() {
    let line = br#"{"type":"teleport","destination":"mars","session_id":"s","uuid":"u"}"#;
    match stream::decode(line).expect("an object decodes") {
        Output::Unknown(unknown) => {
            assert_eq!(unknown.kind.as_deref(), Some("teleport"));
            assert_eq!(unknown.raw.get().as_bytes(), line);
        }
        other => panic!("decoded as {other:?}"),
    }
    let drift = stream::strict(line).expect_err("strict refuses it");
    assert!(drift.to_string().contains("teleport"), "{drift}");

    let input = br#"{"type":"teleport","to":"mars"}"#;
    assert!(matches!(stream::decode_input(input), Ok(Input::Unknown(_))));
    assert!(stream::strict_input(input).is_err());
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
