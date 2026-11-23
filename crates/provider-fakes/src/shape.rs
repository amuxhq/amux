//! Frame shapes, to hold what a fake composes to what its provider sends.
//!
//! A frame's shape is the set of its field paths, each with its JSON type.
//! Frames are grouped by what they are (the discriminant: a message type,
//! subtype, request kind, content kind…). A composed frame conforms when
//! every path it has appears on some recorded frame of the same group, and
//! it carries every field the newest recordings of that group always send,
//! and the provider's protocol crate reads it with nothing unknown. A fake
//! that invents a field, drops one, or composes a frame the provider never
//! sends fails.
//!
//! "Always" needs more than one session to mean anything: every frame of a
//! group in one recording shares that session's circumstances (a rate-limit
//! warning's threshold, a permission prompt's suggestions), so its frames
//! agree on fields that are only situational. What a group always sends is
//! what every frame of it carries across its newest [`SAMPLES`] recordings
//! (all of them, where fewer show it), taking whole provider versions newest
//! first: a newer version shown by fewer recordings than that is read
//! together with the versions before it. A field such a version added is
//! required once enough of its recordings show it; one it stopped sending
//! is never required.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use crate::Kind;
use crate::playback::{self, Channel};

/// Subtrees whose contents depend on the data (a tool's input, a server's
/// schema, per-model tallies, a turn's items), compared by type only.
fn opaque(kind: Kind) -> [&'static [&'static str]; 2] {
    match kind {
        Kind::ClaudeSdk | Kind::ClaudePty => [claude_protocol::DATA_SUBTREES, &[]],
        Kind::Codex => [codex_protocol::DATA_SUBTREES, codex_protocol::ITEM_LISTS],
        Kind::Acp => [acp_protocol::DATA_SUBTREES, acp_protocol::ITEM_LISTS],
    }
}

/// Maps keyed by the data (a form's fields by their names), whose keys are
/// compared as one key and whose values by their shape.
fn keyed_by_data(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Acp => acp_protocol::DATA_MAPS,
        // Their shapes were recorded before maps were told apart, and
        // their maps' keys are few and fixed enough to be recorded.
        Kind::ClaudeSdk | Kind::ClaudePty | Kind::Codex => &[],
    }
}

/// Whether `path` is in `list`: a path, or `*.<name>`, any field called
/// `name` at any depth.
fn listed(list: &[&str], path: &str) -> bool {
    list.iter().any(|entry| match entry.strip_prefix("*.") {
        Some(name) => path == name || path.ends_with(&format!(".{name}")),
        None => *entry == path,
    })
}

/// Every `path:type` in a value.
pub fn signature(kind: Kind, value: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let lists = Lists {
        opaque: opaque(kind),
        maps: keyed_by_data(kind),
    };
    walk(&lists, value, String::new(), &mut out);
    if matches!(kind, Kind::Codex | Kind::Acp) && value.get("method").is_none() {
        // A response echoes the id the host chose, of whatever type.
        out.retain(|entry| !entry.starts_with("id:"));
        out.insert("id:echoed".into());
    }
    out
}

struct Lists {
    opaque: [&'static [&'static str]; 2],
    maps: &'static [&'static str],
}

fn walk(lists: &Lists, value: &Value, path: String, out: &mut BTreeSet<String>) {
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
        if lists.opaque.iter().any(|list| listed(list, &path)) {
            return;
        }
    }
    match value {
        Value::Object(fields) => {
            let map = !path.is_empty() && listed(lists.maps, &path);
            for (key, value) in fields {
                let key = if map { "*" } else { key.as_str() };
                let child = if path.is_empty() {
                    key.to_owned()
                } else {
                    format!("{path}.{key}")
                };
                walk(lists, value, child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(lists, item, format!("{path}[]"), out);
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
            Kind::Codex | Kind::Acp => {
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
            Kind::Acp => acp_group(&self.requests, frame),
        }
    }
}

/// An ACP frame's group: a session update by its kind, a form by its
/// mode, an answer by the method it answers. An error answer's shape is
/// JSON-RPC's whatever it answers, so errors are one group.
fn acp_group(requests: &BTreeMap<String, String>, frame: &Value) -> String {
    if let Some(method) = frame.get("method").and_then(Value::as_str) {
        return match method {
            "session/update" => format!(
                "session/update/{}",
                text(&frame["params"]["update"]["sessionUpdate"])
            ),
            "elicitation/create" => format!("{method}/{}", text(&frame["params"]["mode"])),
            other => other.to_owned(),
        };
    }
    if frame.get("error").is_some() {
        return "error".into();
    }
    let answered = requests.get(&id(&frame["id"])).map_or("?", String::as_str);
    format!("result/{answered}")
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
    let group = session_sdk_group(requests, frame);
    // What headless Claude forwards of a subagent's names it: shaped as the
    // session's own frames are, apart from them.
    if frame["agent_id"].is_string() || frame["request"]["agent_id"].is_string() {
        format!("subagent/{group}")
    } else {
        group
    }
}

fn session_sdk_group(requests: &BTreeMap<String, String>, frame: &Value) -> String {
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

/// The group of what terminal Claude wrote for a subagent, from the
/// recorder's line: its rows grouped as the session's own are, apart from
/// them, and its `meta.json`.
pub fn subagent_group(line: &Value) -> Option<String> {
    if line.get("meta").is_some() {
        return Some("subagent/meta".into());
    }
    Some(format!("subagent/{}", pty_group(line.get("row")?)))
}

fn pty_group(frame: &Value) -> String {
    if let Some(event) = frame.get("hook_event_name").and_then(Value::as_str) {
        // A hook a subagent raised names it, and the main thread's never do.
        let within = !text(&frame["agent_id"]).is_empty();
        return format!("hook/{event}{}", if within { "/subagent" } else { "" });
    }
    let kind = text(&frame["type"]);
    match kind {
        "assistant" | "user" => format!("row/{kind}/{}", content_kind(&frame["message"])),
        "system" => format!("row/system/{}", text(&frame["subtype"])),
        // A queued prompt and a queued task notification are written apart.
        "attachment" if frame["attachment"]["type"] == "queued_command" => format!(
            "row/attachment/queued_command/{}",
            text(&frame["attachment"]["commandMode"])
        ),
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

/// How many recordings a group's newest versions must add up to before
/// what their frames share is taken as what the provider always sends.
pub const SAMPLES: usize = 3;

#[derive(Default)]
struct Group {
    allowed: BTreeSet<String>,
    /// By the provider that recorded it (one corpus may hold several
    /// agents of one protocol, each with versions of its own), then per
    /// version, oldest first.
    versions: BTreeMap<String, BTreeMap<Vec<u64>, Sample>>,
    /// What the newest recordings always send, once the corpus is loaded.
    required: BTreeSet<String>,
}

/// What one provider version's recordings show of a group.
struct Sample {
    /// How many recordings show the group.
    recordings: usize,
    /// The fields every one of their frames of the group carries.
    always: BTreeSet<String>,
}

impl Group {
    /// The fields every provider's newest recordings always send.
    fn required(&self) -> BTreeSet<String> {
        let mut providers = self.versions.values().map(required_of);
        let Some(first) = providers.next() else {
            return BTreeSet::new();
        };
        providers.fold(first, |all, required| &all & &required)
    }
}

/// What one provider's newest recordings always send: its newest version's,
/// read with the versions before it until they add up to [`SAMPLES`]
/// recordings.
fn required_of(versions: &BTreeMap<Vec<u64>, Sample>) -> BTreeSet<String> {
    let mut recordings = 0;
    let mut required: Option<BTreeSet<String>> = None;
    for sample in versions.values().rev() {
        required = Some(match required {
            None => sample.always.clone(),
            Some(required) => &required & &sample.always,
        });
        recordings += sample.recordings;
        if recordings >= SAMPLES {
            break;
        }
    }
    required.unwrap_or_default()
}

/// Capabilities ACP lets an agent leave out, and that an agent lacking
/// them does leave out. Every recorded agent offers them, so the corpus
/// alone would require them; the fake leaves them out to stand for an
/// agent that cannot reopen a session, or list its sessions.
const ACP_CAPABILITIES: &[&str] = &[
    "result.agentCapabilities.sessionCapabilities.list",
    "result.agentCapabilities.sessionCapabilities.resume",
];

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
        for group in corpus.groups.values_mut() {
            group.required = group.required();
        }
        Ok(corpus)
    }

    fn add(&mut self, kind: Kind, dir: &Path) -> Result<(), String> {
        let manifest: Value = std::fs::read(dir.join("manifest.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let provider = text(&manifest["recorded"]["provider"]).to_owned();
        let version: Vec<u64> = text(&manifest["recorded"]["version"])
            .split('.')
            .filter_map(|part| part.parse().ok())
            .collect();
        let mut shown = BTreeSet::new();
        for process in playback::load(dir).map_err(|error| error.to_string())? {
            let mut classifier = Classifier::default();
            for event in &process.events {
                let Ok(mut frame) = serde_json::from_slice::<Value>(&event.bytes) else {
                    continue;
                };
                match event.channel {
                    Channel::Input => classifier.host(kind, &frame),
                    Channel::Exit => {}
                    Channel::Output | Channel::Transcript | Channel::Hook | Channel::Subagent => {
                        let group = if event.channel == Channel::Subagent {
                            let Some(group) = subagent_group(&frame) else {
                                continue;
                            };
                            let key = if frame.get("meta").is_some() {
                                "meta"
                            } else {
                                "row"
                            };
                            frame = frame[key].take();
                            group
                        } else {
                            classifier.provider(kind, &frame)
                        };
                        let signature = signature(kind, &frame);
                        let entry = self.groups.entry(group.clone()).or_default();
                        entry.allowed.extend(signature.iter().cloned());
                        // What sits inside an array or a map keyed by the
                        // data is only there when it has members.
                        let keys: BTreeSet<String> = keys(&signature)
                            .into_iter()
                            .filter(|key| {
                                !key.contains("[]") && !key.split('.').any(|part| part == "*")
                            })
                            .collect();
                        let first = shown.insert(group);
                        let versions = entry.versions.entry(provider.clone()).or_default();
                        match versions.get_mut(&version) {
                            Some(sample) => {
                                sample.always.retain(|key| keys.contains(key));
                                sample.recordings += usize::from(first);
                            }
                            None => {
                                let sample = Sample {
                                    recordings: 1,
                                    always: keys,
                                };
                                versions.insert(version.clone(), sample);
                            }
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
        if kind == Kind::Acp
            && let Some(method) = group.strip_prefix("result/")
        {
            answer(method, frame).map_err(|drift| format!("{group}: {drift}: {frame}"))?;
        }
        let Some(recorded) = self.groups.get(group) else {
            return Err(format!("the corpus never shows a {group} frame: {frame}"));
        };
        let signature = signature(kind, frame);
        let invented: Vec<_> = signature.difference(&recorded.allowed).collect();
        let composed = keys(&signature);
        let mut required = recorded.required.clone();
        if kind == Kind::Acp {
            required.retain(|key| !ACP_CAPABILITIES.contains(&key.as_str()));
        }
        let dropped: Vec<_> = required.difference(&composed).collect();
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
        Kind::Acp => acp_protocol::strict(&line)
            .map(drop)
            .map_err(|drift| drift.to_string()),
    }
}

/// Whether ACP's types read an agent's answer to a `method` request with
/// nothing left unknown. An answer's line does not say what it answers,
/// so [`typed`] reads only its envelope.
fn answer(method: &str, frame: &Value) -> Result<(), String> {
    match acp_protocol::AgentResponse::strict_read(method, &frame["result"]) {
        Some(read) => read.map(drop).map_err(|error| error.to_string()),
        None => Err(format!("no ACP request {method} has an answer")),
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

    fn acp_corpus() -> Corpus {
        let root = source_tree::crate_dir!().join("../interpret/fixtures/acp/live");
        Corpus::load(Kind::Acp, &[&root]).unwrap()
    }

    fn chunk(update: Value) -> Value {
        json!({"jsonrpc": "2.0", "method": "session/update",
               "params": {"sessionId": "s", "update": update}})
    }

    /// An ACP agent's frames are held to the recorded agents': a field
    /// none sent, a value ACP does not have, and an answer that does not
    /// read as the answer to its request all fail.
    #[test]
    fn an_acp_frame_no_agent_sends_fails() {
        let corpus = acp_corpus();
        let said = chunk(json!({"sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": "hi"}}));
        let group = "session/update/agent_message_chunk";
        assert_eq!(corpus.check(Kind::Acp, group, &said), Ok(()));
        let mut invented = said.clone();
        invented["params"]["update"]["mood"] = json!("happy");
        assert!(corpus.check(Kind::Acp, group, &invented).is_err());
        let answer =
            |reason: &str| json!({"id": 3, "jsonrpc": "2.0", "result": {"stopReason": reason}});
        let group = "result/session/prompt";
        assert_eq!(corpus.check(Kind::Acp, group, &answer("end_turn")), Ok(()));
        assert!(corpus.check(Kind::Acp, group, &answer("paused")).is_err());
        let wrong = json!({"id": 3, "jsonrpc": "2.0", "result": {"sessionId": 7}});
        assert!(
            corpus
                .check(Kind::Acp, "result/session/new", &wrong)
                .is_err()
        );
    }

    /// A form's fields are the data's: any name passes, shaped as a
    /// recorded field is.
    #[test]
    fn an_acp_forms_field_names_are_its_own() {
        let form = |field: Value| {
            json!({"id": "1", "jsonrpc": "2.0", "method": "elicitation/create", "params": {
                "message": "?", "mode": "form", "sessionId": "s", "toolCallId": "c",
                "requestedSchema": {"type": "object", "properties": {"colour": field}}
            }})
        };
        let corpus = acp_corpus();
        let group = "elicitation/create/form";
        let field = json!({"type": "string", "title": "Colour"});
        assert_eq!(corpus.check(Kind::Acp, group, &form(field)), Ok(()));
        let field = json!({"type": "string", "colourful": true});
        assert!(corpus.check(Kind::Acp, group, &form(field)).is_err());
    }

    fn group(versions: &[(u64, usize, &[&str])]) -> Group {
        let versions = versions
            .iter()
            .map(|(minor, recordings, always)| {
                let always = always.iter().map(|key| (*key).to_owned()).collect();
                let sample = Sample {
                    recordings: *recordings,
                    always,
                };
                (vec![0, *minor, 0], sample)
            })
            .collect();
        Group {
            versions: BTreeMap::from([("provider".to_owned(), versions)]),
            ..Group::default()
        }
    }

    fn names(keys: &[&str]) -> BTreeSet<String> {
        keys.iter().map(|key| (*key).to_owned()).collect()
    }

    /// One session of a new version requires nothing it alone shows: its
    /// situational fields would read as always sent.
    #[test]
    fn a_version_too_few_recordings_show_is_read_with_the_one_before() {
        let one = group(&[(1, 8, &["type", "kept"]), (2, 1, &["type", "warning"])]);
        assert_eq!(one.required(), names(&["type"]));
        let enough = group(&[(1, 8, &["type", "kept"]), (2, SAMPLES, &["type", "added"])]);
        assert_eq!(enough.required(), names(&["type", "added"]));
        let few = group(&[(1, 1, &["type", "kept"])]);
        assert_eq!(few.required(), names(&["type", "kept"]));
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
