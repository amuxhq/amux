//! Every transcript row and hook payload in every terminal Claude recording
//! decodes with nothing left unknown. A failure here is provider drift, or a
//! type that does not match what Claude writes.

use claude_protocol::{hooks, transcript};
use serde_json::Value;

use crate::fixtures;

/// A row the recorder's transcript tailer adds when it has read the file
/// to its end; amux's, not Claude's.
const TAILER_MARKER: &str = "amux.transcript_ready";

/// The terminal corpus: amux's live captures.
const ROOTS: [&str; 1] = ["live/pty"];

enum Line {
    Row(String),
    Hook(String),
}

/// Each recorded row and payload with where it came from.
fn lines() -> Vec<(String, Line)> {
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
        recordings.len() >= 29,
        "found only {} recordings",
        recordings.len()
    );
    let mut lines = Vec::new();
    for path in recordings {
        let name = path
            .strip_prefix(fixtures())
            .map(|relative| format!("fixtures/{}", relative.display()))
            .expect("a recording under fixtures");
        let text = std::fs::read_to_string(&path).expect("recording");
        for (number, record) in text.lines().enumerate() {
            let place = format!("{name}:{}", number + 1);
            let record: Value = serde_json::from_str(record).expect("record is JSON");
            let line = record["line"].as_str().expect("line").to_owned();
            let transport = record["transport_id"].as_str().expect("transport_id");
            if transport.starts_with("hook") {
                lines.push((place, Line::Hook(line)));
            } else if transport.starts_with("transcript") {
                let wrapped: Value = serde_json::from_str(&line).expect("transcript line is JSON");
                let row = &wrapped["row"];
                if row["type"] == TAILER_MARKER {
                    continue;
                }
                lines.push((place, Line::Row(row.to_string())));
            }
        }
    }
    let rows = lines
        .iter()
        .filter(|(_, line)| matches!(line, Line::Row(_)))
        .count();
    assert!(rows >= 900, "found only {rows} transcript rows");
    assert!(
        lines.len() - rows >= 140,
        "found only {} hook payloads",
        lines.len() - rows
    );
    lines
}

#[test]
fn every_recorded_row_and_payload_is_known() {
    let mut failures = Vec::new();
    let all = lines();
    for (place, line) in &all {
        let drift = match line {
            Line::Row(row) => transcript::strict(row.as_bytes()).err(),
            Line::Hook(payload) => hooks::strict(payload.as_bytes()).err(),
        };
        if let Some(drift) = drift {
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
fn an_invented_row_or_payload_is_unknown_and_fails_strict() {
    let row = br#"{"type":"teleport","destination":"mars","sessionId":"s"}"#;
    match transcript::decode(row).expect("an object decodes") {
        transcript::Row::Unknown(unknown) => {
            assert_eq!(unknown.field("destination").unwrap(), "mars")
        }
        other => panic!("decoded as {other:?}"),
    }
    let drift = transcript::strict(row).expect_err("strict refuses it");
    assert!(drift.to_string().contains("teleport"), "{drift}");

    let payload =
        br#"{"hook_event_name":"Teleport","session_id":"s","transcript_path":"/t","cwd":"/"}"#;
    let decoded = hooks::decode(payload).expect("an object decodes");
    assert_eq!(decoded.name(), "Teleport");
    assert!(decoded.common().is_none());
    let drift = hooks::strict(payload).expect_err("strict refuses it");
    assert!(drift.to_string().contains("Teleport"), "{drift}");
}

/// A prompt queued with a pasted image is written as content blocks, text
/// then image, where a plain prompt is a string. The recorded queued
/// prompt, given an image the way Claude writes one.
#[test]
fn a_queued_prompt_with_an_image_reads_as_blocks() {
    let recorded = lines()
        .into_iter()
        .find_map(|(_, line)| match line {
            Line::Row(row) if row.contains("queued_command") && row.contains("BANANA") => Some(row),
            _ => None,
        })
        .expect("steer_queued's queued prompt");
    let mut row: Value = serde_json::from_str(&recorded).unwrap();
    row["attachment"]["prompt"] = serde_json::json!([
        {"type": "text", "text": "[Image #1]\n\nAlso append the word BANANA."},
        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="}},
    ]);
    row["attachment"]["imagePasteIds"] = serde_json::json!([1]);
    let line = row.to_string();
    let decoded = transcript::strict(line.as_bytes()).expect("known");
    let transcript::Row::Attachment(attachment) = &decoded else {
        panic!("decoded as {decoded:?}");
    };
    let transcript::Attachment::QueuedCommand(queued) = &attachment.attachment else {
        panic!("decoded as {attachment:?}");
    };
    let claude_protocol::stream::MessageContent::Blocks(blocks) = &queued.prompt else {
        panic!("prompt read as {:?}", queued.prompt);
    };
    assert_eq!(
        blocks.iter().map(|block| block.kind()).collect::<Vec<_>>(),
        ["text", "image"]
    );
}
