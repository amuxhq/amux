//! A terminal recording keeps the rows the scenario produced and none of the
//! recording machine owner's own context.

use replay_support::{IoDirection, IoEvent};
use serde_json::{Value, json};

fn transcript(row: Value) -> IoEvent {
    IoEvent {
        us: 0,
        direction: IoDirection::Read,
        line: json!({"path": "<TRANSCRIPT_1>", "row": row}).to_string(),
        transport_id: Some("transcript".to_owned()),
        session_id: None,
    }
}

fn rows(events: &[IoEvent]) -> Vec<Value> {
    events
        .iter()
        .map(|event| serde_json::from_str::<Value>(&event.line).unwrap()["row"].take())
        .collect()
}

/// Claude 2.1.312 copies each API request into the transcript: the system
/// prompt, the owner's context and the conversation so far. None of it is
/// the scenario's, and the conversation is already in the other rows.
#[test]
fn the_api_request_rows_are_left_out() {
    let mut events = vec![
        transcript(
            json!({"type": "user", "uuid": "u1", "message": {"role": "user", "content": "hi"}}),
        ),
        transcript(json!({
            "type": "api-request", "sessionId": "s", "id": "r1", "version": "2.1.312",
            "params": "{\"model\":\"claude-haiku-4-5\"}", "requestDigest": "d",
        })),
        transcript(json!({
            "type": "api-request-blob", "sessionId": "s", "hash": "h",
            "message": "{\"role\":\"system\",\"content\":[{\"type\":\"text\",\"text\":\"# Environment\"}]}",
        })),
        transcript(json!({
            "type": "api-request-shape", "sessionId": "s", "shapeHash": "h", "version": "2.1.312",
            "shape": "{\"system\":[{\"type\":\"text\",\"text\":\"You are Claude Code\"}]}",
        })),
        transcript(json!({"type": "assistant", "uuid": "a1", "requestRef": "r1"})),
    ];
    claude_specs::scrub::transcript(&mut events).unwrap();
    let kinds: Vec<_> = rows(&events)
        .iter()
        .map(|row| row["type"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(kinds, ["user", "assistant"]);
}

/// What the owner has installed (skills, agents, deferred tools) is listed
/// to the model in attachments; the attachment stays, its list does not.
#[test]
fn what_the_owner_installed_is_scrubbed() {
    let installed = [
        json!({"type": "skill_listing", "content": "- handoff\n- autopilot", "skillCount": 2}),
        json!({"type": "agent_listing_delta", "addedLines": ["- claude: Catch-all"]}),
        json!({"type": "deferred_tools_delta", "addedLines": ["ArtifactData"]}),
        json!({"type": "deferred_tools_record", "tools": ["ArtifactData"]}),
        json!({"type": "instructions", "content": "# Working with me"}),
    ];
    let mut events: Vec<_> = installed
        .iter()
        .map(|attachment| {
            transcript(json!({
                "type": "attachment", "uuid": "x", "attachment": attachment,
                "rendered": [{"type": "text", "content": "the same list"}],
            }))
        })
        .collect();
    claude_specs::scrub::transcript(&mut events).unwrap();
    for (row, original) in rows(&events).iter().zip(&installed) {
        assert_eq!(
            row["attachment"],
            json!({"type": original["type"], "scrubbed": true})
        );
        assert_eq!(row["rendered"], json!([{"type": "text", "content": ""}]));
    }
}

/// The recording machine's environment (its folders, the guidance Claude
/// gives about them) and its attribution text are the owner's, not the
/// scenario's; both reach the model as system reminders.
#[test]
fn the_environment_and_attribution_are_scrubbed() {
    let owners = [
        json!({"type": "environment", "snapshot": {"workingDirectory": "<MACHINE_PATH>", "shell": "zsh"}}),
        json!({
            "type": "remote_session_change",
            "commit": "Co-Authored-By: Claude Haiku 4.5 <noreply@anthropic.com>",
            "pr": "Generated with [Claude Code](https://claude.com/claude-code)",
        }),
    ];
    let mut events: Vec<_> = owners
        .iter()
        .map(|attachment| {
            transcript(json!({
                "type": "attachment", "uuid": "x", "attachment": attachment,
                "rendered": [{"content": "<system-reminder>\n# Environment\n</system-reminder>"}],
            }))
        })
        .collect();
    claude_specs::scrub::transcript(&mut events).unwrap();
    for (row, original) in rows(&events).iter().zip(&owners) {
        assert_eq!(
            row["attachment"],
            json!({"type": original["type"], "scrubbed": true})
        );
        assert_eq!(row["rendered"], json!([{"content": ""}]));
    }
}
