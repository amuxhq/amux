//! Frame shapes, to hold what a fake composes to what its provider sends.
//!
//! A frame's shape is the set of its field paths, each with its JSON type.
//! Frames are grouped by what they are (the discriminant: a message type,
//! subtype, request kind, content kind…). A composed frame conforms when
//! every path it has appears on some recorded frame of the same group, and
//! it carries every field the newest recorded provider version always
//! sends in that group, and the provider's protocol crate reads it with
//! nothing unknown. A fake that invents a field, drops one, or composes a
//! frame the provider never sends fails.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use crate::Kind;
use crate::playback::{self, Channel};

/// Subtrees whose contents depend on the data (a tool's input, a server's
/// schema, per-model tallies), compared by type only.
fn opaque(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::ClaudeSdk | Kind::ClaudePty => &[
            "message.content[].input",
            "message.content[].content",
            "tool_use_result",
            "toolUseResult",
            "modelUsage",
            "model_usage_since_last_result",
            "wire_tool_inputs",
            "wireToolInputs",
            "request.input",
            "request.requested_schema",
            "request.permission_suggestions",
            "response.response",
            "tool_input",
            "tool_response",
            "permission_suggestions",
            "event.content_block.input",
            "attachment",
            "snapshot",
        ],
        Kind::Codex => &[
            "params.item.commandActions",
            "params.item.changes",
            "params.item.arguments",
            "params.item.result",
            "params.item.content",
            "params.commandActions",
            "params.availableDecisions",
            "params.turn.items",
            "params.thread.turns",
            "result.thread.turns",
            "params.tokenUsage",
            "params.rateLimits",
            "params.requestedSchema",
            "params.permissions",
            "params._meta",
            "result.permissions",
        ],
    }
}

/// Every `path:type` in a value.
pub fn signature(kind: Kind, value: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    walk(opaque(kind), value, String::new(), &mut out);
    if kind == Kind::Codex && value.get("method").is_none() {
        // A response echoes the id the host chose, of whatever type.
        out.retain(|entry| !entry.starts_with("id:"));
        out.insert("id:echoed".into());
    }
    out
}

fn walk(opaque: &[&str], value: &Value, path: String, out: &mut BTreeSet<String>) {
    let kind = match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    if !path.is_empty() {
        out.insert(format!("{path}:{kind}"));
        if opaque.contains(&path.as_str()) {
            return;
        }
    }
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                walk(opaque, value, child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(opaque, item, format!("{path}[]"), out);
            }
        }
        _ => {}
    }
}

fn keys(signature: &BTreeSet<String>) -> BTreeSet<String> {
    signature
        .iter()
        .map(|entry| {
            entry
                .rsplit_once(':')
                .map_or(entry.as_str(), |(path, _)| path)
                .to_owned()
        })
        .collect()
}

/// Names what a frame is, following a stream: requests the other side sent
/// are remembered so a response is grouped by what it answers.
#[derive(Default)]
pub struct Classifier {
    requests: BTreeMap<String, String>,
}

impl Classifier {
    /// Note a frame the host sent.
    pub fn host(&mut self, kind: Kind, frame: &Value) {
        match kind {
            Kind::ClaudeSdk => {
                if frame["type"] == "control_request" {
                    self.requests.insert(
                        id(&frame["request_id"]),
                        text(&frame["request"]["subtype"]).to_owned(),
                    );
                }
            }
            Kind::Codex => {
                if frame.get("method").is_some() && frame.get("id").is_some() {
                    self.requests
                        .insert(id(&frame["id"]), text(&frame["method"]).to_owned());
                }
            }
            Kind::ClaudePty => {}
        }
    }

    /// The group of a frame the provider sent.
    pub fn provider(&mut self, kind: Kind, frame: &Value) -> String {
        match kind {
            Kind::ClaudeSdk => sdk_group(&self.requests, frame),
            Kind::Codex => codex_group(&self.requests, frame),
            Kind::ClaudePty => pty_group(frame),
        }
    }
}

fn id(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

fn content_kind(message: &Value) -> String {
    match &message["content"] {
        Value::String(_) => "string".into(),
        Value::Array(blocks) => blocks
            .first()
            .map(|block| text(&block["type"]).to_owned())
            .unwrap_or_else(|| "empty".into()),
        _ => "none".into(),
    }
}

fn sdk_group(requests: &BTreeMap<String, String>, frame: &Value) -> String {
    let kind = text(&frame["type"]);
    match kind {
        "system" | "result" => format!("{kind}/{}", text(&frame["subtype"])),
        "control_request" => format!("control_request/{}", text(&frame["request"]["subtype"])),
        "control_response" => {
            let answered = requests
                .get(&id(&frame["response"]["request_id"]))
                .map_or("?", String::as_str);
            format!(
                "control_response/{answered}/{}",
                text(&frame["response"]["subtype"])
            )
        }
        "assistant" => format!("assistant/{}", content_kind(&frame["message"])),
        "user" => format!(
            "user/{}{}",
            content_kind(&frame["message"]),
            if frame.get("isReplay").is_some() {
                "/replay"
            } else {
                ""
            }
        ),
        "stream_event" => format!("stream_event/{}", text(&frame["event"]["type"])),
        other => other.to_owned(),
    }
}

fn codex_group(requests: &BTreeMap<String, String>, frame: &Value) -> String {
    if let Some(method) = frame.get("method").and_then(Value::as_str) {
        let item = frame["params"]["item"]["type"].as_str();
        return match item {
            Some(item) => format!("{method}/{item}"),
            None => method.to_owned(),
        };
    }
    let answered = requests.get(&id(&frame["id"])).map_or("?", String::as_str);
    if frame.get("error").is_some() {
        format!("error/{answered}")
    } else {
        format!("result/{answered}")
    }
}

fn pty_group(frame: &Value) -> String {
    if let Some(event) = frame.get("hook_event_name").and_then(Value::as_str) {
        return format!("hook/{event}");
    }
    let kind = text(&frame["type"]);
    match kind {
        "assistant" | "user" => format!("row/{kind}/{}", content_kind(&frame["message"])),
        "system" => format!("row/system/{}", text(&frame["subtype"])),
        "attachment" => format!("row/attachment/{}", text(&frame["attachment"]["type"])),
        "queue-operation" => format!("row/queue-operation/{}", text(&frame["operation"])),
        other => format!("row/{other}"),
    }
}

/// What a provider's recordings show of each group.
#[derive(Default)]
pub struct Corpus {
    groups: BTreeMap<String, Group>,
}

#[derive(Default)]
struct Group {
    allowed: BTreeSet<String>,
    newest: Option<(Vec<u64>, BTreeSet<String>)>,
}

impl Corpus {
    /// Every recording under `roots`: each directory holding an `io.jsonl`.
    pub fn load(kind: Kind, roots: &[&Path]) -> Result<Self, String> {
        let mut corpus = Corpus::default();
        for root in roots {
            let mut dirs: Vec<_> = std::fs::read_dir(root)
                .map_err(|error| format!("{}: {error}", root.display()))?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|dir| dir.join("io.jsonl").exists())
                .collect();
            dirs.sort();
            for dir in dirs {
                corpus.add(kind, &dir)?;
            }
        }
        Ok(corpus)
    }

    fn add(&mut self, kind: Kind, dir: &Path) -> Result<(), String> {
        let manifest: Value = std::fs::read(dir.join("manifest.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let version: Vec<u64> = text(&manifest["recorded"]["version"])
            .split('.')
            .filter_map(|part| part.parse().ok())
            .collect();
        for process in playback::load(dir).map_err(|error| error.to_string())? {
            let mut classifier = Classifier::default();
            for event in &process.events {
                let Ok(frame) = serde_json::from_slice::<Value>(&event.bytes) else {
                    continue;
                };
                match event.channel {
                    Channel::Input => classifier.host(kind, &frame),
                    Channel::Exit => {}
                    Channel::Output | Channel::Transcript | Channel::Hook => {
                        let group = classifier.provider(kind, &frame);
                        let signature = signature(kind, &frame);
                        let entry = self.groups.entry(group).or_default();
                        entry.allowed.extend(signature.iter().cloned());
                        // What sits inside an array is only there when the
                        // array has members.
                        let keys: BTreeSet<String> = keys(&signature)
                            .into_iter()
                            .filter(|key| !key.contains("[]"))
                            .collect();
                        match &mut entry.newest {
                            Some((newest, required)) if *newest == version => {
                                required.retain(|key| keys.contains(key));
                            }
                            Some((newest, _)) if *newest > version => {}
                            _ => entry.newest = Some((version.clone(), keys)),
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Whether a composed frame of `group` has a recorded shape.
    pub fn check(&self, kind: Kind, group: &str, frame: &Value) -> Result<(), String> {
        typed(kind, frame).map_err(|drift| format!("{group}: {drift}: {frame}"))?;
        let Some(recorded) = self.groups.get(group) else {
            return Err(format!("the corpus never shows a {group} frame: {frame}"));
        };
        let signature = signature(kind, frame);
        let invented: Vec<_> = signature.difference(&recorded.allowed).collect();
        let composed = keys(&signature);
        let dropped: Vec<_> = recorded
            .newest
            .as_ref()
            .map(|(_, required)| required.difference(&composed).collect())
            .unwrap_or_default();
        if invented.is_empty() && dropped.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "{group}: fields no recording has {invented:?}; fields the newest recordings always send {dropped:?}"
            ))
        }
    }
}

/// Whether the provider's protocol types read a composed frame with nothing
/// left unknown, as they read what the provider itself sends.
pub fn typed(kind: Kind, frame: &Value) -> Result<(), String> {
    let line = serde_json::to_vec(frame).map_err(|error| error.to_string())?;
    match kind {
        Kind::Codex => codex_protocol::strict(&line)
            .map(drop)
            .map_err(|drift| drift.to_string()),
        Kind::ClaudeSdk => claude_protocol::stream::strict(&line)
            .map(drop)
            .map_err(|drift| drift.to_string()),
        Kind::ClaudePty if frame.get("hook_event_name").is_some() => {
            claude_protocol::hooks::strict(&line)
                .map(drop)
                .map_err(|drift| drift.to_string())
        }
        Kind::ClaudePty => claude_protocol::transcript::strict(&line)
            .map(drop)
            .map_err(|drift| drift.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn completed(status: &str) -> Value {
        json!({
            "method": "turn/completed",
            "params": {
                "threadId": "t",
                "turn": {"id": "u", "items": [], "status": status, "error": null}
            }
        })
    }

    /// A status Codex never sends has the same shape as one it does; only
    /// the protocol types tell them apart.
    #[test]
    fn a_spelling_codex_never_sends_fails_though_its_shape_matches() {
        assert_eq!(typed(Kind::Codex, &completed("completed")), Ok(()));
        assert_eq!(
            signature(Kind::Codex, &completed("completed")),
            signature(Kind::Codex, &completed("paused"))
        );
        assert!(typed(Kind::Codex, &completed("paused")).is_err());
    }

    #[test]
    fn a_frame_of_each_claude_channel_is_read_by_its_own_types() {
        let result = json!({"type": "result", "subtype": "invented"});
        assert!(typed(Kind::ClaudeSdk, &result).is_err());
        let hook = json!({"hook_event_name": "InventedEvent", "session_id": "s"});
        assert!(typed(Kind::ClaudePty, &hook).is_err());
        let row = json!({"type": "invented-row"});
        assert!(typed(Kind::ClaudePty, &row).is_err());
    }
}
