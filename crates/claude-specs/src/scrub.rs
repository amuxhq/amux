//! What a terminal recording keeps of the transcript Claude writes: the
//! rows the scenario produced, without the recording machine owner's own
//! context.

use std::io;

use replay_support::IoEvent;

/// Transcript attachments that carry the recording machine owner's own
/// context rather than anything the scenario did: their instruction files,
/// account email and organisation, the full system prompt, and what their
/// plugins and tool servers inject. Newer Claude Code versions write these into
/// the transcript. The row and its attachment type stay so the stream keeps its
/// shape; the payload and its rendered copy are replaced.
const PERSONAL_ATTACHMENTS: &[&str] = &[
    "instructions",
    "session_context",
    "credential_org",
    "prompt_snapshot",
    "mcp_instructions_delta",
    "hook_additional_context",
    "hook_success",
];

/// Scrubs the owner's context out of a terminal recording's transcript
/// lines.
pub fn transcript(events: &mut Vec<IoEvent>) -> io::Result<()> {
    for event in events.iter_mut() {
        if event.transport_id.as_deref() != Some("transcript") {
            continue;
        }
        let mut frame: serde_json::Value =
            serde_json::from_str(&event.line).map_err(io::Error::other)?;
        let Some(attachment) = frame.pointer_mut("/row/attachment") else {
            continue;
        };
        let kind = attachment
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if PERSONAL_ATTACHMENTS.contains(&kind.as_str()) {
            *attachment = serde_json::json!({"type": kind, "scrubbed": true});
            // The rendered copy is what the model saw; it repeats the payload.
            if let Some(serde_json::Value::Array(rendered)) = frame.pointer_mut("/row/rendered") {
                for part in rendered {
                    if let Some(content) = part.get_mut("content") {
                        *content = serde_json::Value::String(String::new());
                    }
                }
            }
            event.line = serde_json::to_string(&frame).map_err(io::Error::other)?;
        }
    }
    Ok(())
}
