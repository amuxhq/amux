//! What a terminal recording keeps of the transcript Claude writes: the
//! rows the scenario produced, without the recording machine owner's own
//! context.

use std::io;

use replay_support::IoEvent;

/// Transcript attachments that carry the recording machine owner's own
/// context rather than anything the scenario did: their instruction files,
/// account email and organisation, the full system prompt, and what their
/// plugins and tool servers inject, and what they have installed (skills,
/// agents, deferred tools). Newer Claude Code versions write these into the
/// transcript. The row and its attachment type stay so the stream keeps its
/// shape; the payload and its rendered copy are replaced.
const PERSONAL_ATTACHMENTS: &[&str] = &[
    "instructions",
    "session_context",
    "credential_org",
    "prompt_snapshot",
    "mcp_instructions_delta",
    "hook_additional_context",
    "hook_success",
    "skill_listing",
    "agent_listing_delta",
    "deferred_tools_delta",
    "deferred_tools_record",
];

/// Rows that copy an API request whole (Claude 2.1.312 writes one per
/// request, its messages and system prompt as content-addressed blobs, and
/// the request's shape). They hold the system prompt and the owner's context,
/// the conversation they repeat is in the other rows, and amux reads none of
/// them, so a recording leaves them out.
fn api_request(row_type: &str) -> bool {
    row_type == "api-request" || row_type.starts_with("api-request-")
}

/// Scrubs the owner's context out of a terminal recording's transcript
/// lines.
pub fn transcript(events: &mut Vec<IoEvent>) -> io::Result<()> {
    let mut kept = Vec::with_capacity(events.len());
    for mut event in events.drain(..) {
        if event.transport_id.as_deref() != Some("transcript") {
            kept.push(event);
            continue;
        }
        let mut frame: serde_json::Value =
            serde_json::from_str(&event.line).map_err(io::Error::other)?;
        if frame
            .pointer("/row/type")
            .and_then(serde_json::Value::as_str)
            .is_some_and(api_request)
        {
            continue;
        }
        scrub_attachment(&mut event, &mut frame)?;
        kept.push(event);
    }
    *events = kept;
    Ok(())
}

/// Replaces a personal attachment's payload and its rendered copy.
fn scrub_attachment(event: &mut IoEvent, frame: &mut serde_json::Value) -> io::Result<()> {
    let Some(attachment) = frame.pointer_mut("/row/attachment") else {
        return Ok(());
    };
    let kind = attachment
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if !PERSONAL_ATTACHMENTS.contains(&kind.as_str()) {
        return Ok(());
    }
    *attachment = serde_json::json!({"type": kind, "scrubbed": true});
    // The rendered copy is what the model saw; it repeats the payload.
    if let Some(serde_json::Value::Array(rendered)) = frame.pointer_mut("/row/rendered") {
        for part in rendered {
            if let Some(content) = part.get_mut("content") {
                *content = serde_json::Value::String(String::new());
            }
        }
    }
    event.line = serde_json::to_string(frame).map_err(io::Error::other)?;
    Ok(())
}
