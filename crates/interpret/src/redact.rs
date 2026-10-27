//! Dump redaction, per kind: item and snapshot bodies, facts-ring entries
//! and checkpoints lose their secrets and keep their structure.
//!
//! Only an interpreter can decode its bodies, so the redactor lives here.
//! Protobuf values are walked by their descriptor: every string field is
//! redacted as text (or as JSON when it holds a JSON object or array), every
//! `*_json` bytes field as JSON, the per-kind `body` bytes as the kind's
//! body or input message, and ids and hashes are kept. A field the descriptor does
//! not name is dropped. Facts are JSON and are walked as JSON; a checkpoint
//! is decoded as the kind's state and written again with every protobuf
//! value inside it walked the same way.
//!
//! On top of the shared capture rules (secret-named keys and assignments,
//! known token prefixes, machine paths, emails, this machine's own names),
//! the redactor blanks the values of environment maps and replaces
//! provider session and thread ids, wherever they appear in the target,
//! with a placeholder derived from the id so one session stays recognisable
//! across a dump.

use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

use prost::{Message, Name};
use prost_types::field_descriptor_proto::Type;
use prost_types::{DescriptorProto, FileDescriptorSet};
use redaction::{Redaction, RedactionSummary, SECRET_PLACEHOLDER};
use serde_json::Value;
use wire::Kind;

use crate::claude_pty::ClaudePty;
use crate::claude_sdk::ClaudeSdk;
use crate::codex::Codex;
use crate::{
    Checkpoint, Interpreter, RedactTarget, decode_checkpoint, encode_checkpoint, serde_pb,
};

/// What a session or thread id becomes: this prefix, eight hex digits of a
/// hash of the id, and `>`.
pub const SESSION_PLACEHOLDER_PREFIX: &str = "<SESSION-";

/// The rules every target is redacted under: this machine's home, user
/// and host names on top of the shared ones, read once.
fn rules() -> &'static Redaction {
    static RULES: OnceLock<Redaction> = OnceLock::new();
    RULES.get_or_init(Redaction::for_this_machine)
}

/// Redacts one target of one kind. An unknown kind cannot be decoded, so
/// its target comes back empty.
pub fn redact(kind: Kind, target: RedactTarget) -> RedactTarget {
    match kind {
        Kind::ClaudePty => ClaudePty::redact(target),
        Kind::ClaudeSdk => ClaudeSdk::redact(target),
        Kind::Codex => Codex::redact(target),
        Kind::Unspecified => match target {
            RedactTarget::ItemBody(_) => RedactTarget::ItemBody(Vec::new()),
            RedactTarget::SnapshotBody(_) => RedactTarget::SnapshotBody(Vec::new()),
            RedactTarget::Input(_) => RedactTarget::Input(Vec::new()),
            RedactTarget::AgentMessage(envelope) => {
                ClaudeSdk::redact(RedactTarget::AgentMessage(envelope))
            }
            RedactTarget::Fact(fact) => RedactTarget::Fact(crate::Fact {
                channel: fact.channel,
                payload: Vec::new(),
            }),
            RedactTarget::Checkpoint(_) => RedactTarget::Checkpoint(Vec::new()),
            RedactTarget::Spec(_) => RedactTarget::Spec(Vec::new()),
            RedactTarget::Step(_) => RedactTarget::Step(Vec::new()),
            // A row has no body, so any kind's redactor reads it.
            RedactTarget::Agent(agent) => ClaudeSdk::redact(RedactTarget::Agent(agent)),
            // Walked with every body, which no kind is there to decode,
            // emptied.
            RedactTarget::ClientSession(history) => RedactTarget::ClientSession(redact_message(
                &wire::dump::ClientSessionHistory::full_name(),
                &history,
            )),
        },
    }
}

/// Redacts free text no kind can decode, such as the daemon's log: the
/// shared capture rules, credential prefixes and secret assignments.
pub fn redact_text(text: &str) -> String {
    Scrubber::default().twice(|s| s.text(text))
}

/// Redacts an encoded protocol message of the named type (`amux.v1.Item`)
/// that belongs to no one kind, such as one the daemon's core was fed:
/// walked by descriptor as every target is, with per-kind bodies, which no
/// kind is there to decode, emptied.
pub fn redact_message(name: &str, bytes: &[u8]) -> Vec<u8> {
    let name = format!(".{name}");
    Scrubber::default().twice(|s| s.message(&name, bytes))
}

/// The per-kind redactor: `Item` and `Snapshot` bodies of this kind, the
/// input body a person's input carries for it, and the state its checkpoint
/// holds.
pub(crate) fn redact_kind<ItemBody: Name, SnapshotBody: Name, InputBody: Name, S: Checkpoint>(
    target: RedactTarget,
) -> RedactTarget {
    let mut scrubber = Scrubber {
        item_body: type_name::<ItemBody>(),
        snapshot_body: type_name::<SnapshotBody>(),
        input_body: type_name::<InputBody>(),
        ..Scrubber::default()
    };
    match target {
        RedactTarget::ItemBody(body) => {
            let name = scrubber.item_body.clone();
            RedactTarget::ItemBody(scrubber.twice(|s| s.message(&name, &body)))
        }
        RedactTarget::SnapshotBody(body) => {
            let name = scrubber.snapshot_body.clone();
            RedactTarget::SnapshotBody(scrubber.twice(|s| s.message(&name, &body)))
        }
        RedactTarget::Input(input) => {
            let name = type_name::<wire::Input>();
            RedactTarget::Input(scrubber.twice(|s| s.message(&name, &input)))
        }
        RedactTarget::AgentMessage(envelope) => {
            let name = type_name::<wire::Envelope>();
            RedactTarget::AgentMessage(scrubber.twice(|s| s.message(&name, &envelope)))
        }
        RedactTarget::Fact(fact) => RedactTarget::Fact(crate::Fact {
            channel: fact.channel,
            payload: scrubber.twice(|s| s.json_bytes(&fact.payload)),
        }),
        RedactTarget::Checkpoint(bytes) => {
            RedactTarget::Checkpoint(scrubber.twice(|s| checkpoint::<S>(s, &bytes)))
        }
        RedactTarget::Spec(spec) => {
            let name = type_name::<wire::AgentSpec>();
            RedactTarget::Spec(scrubber.twice(|s| s.message(&name, &spec)))
        }
        RedactTarget::Step(step) => {
            let name = type_name::<wire::Step>();
            RedactTarget::Step(scrubber.twice(|s| s.message(&name, &step)))
        }
        RedactTarget::Agent(agent) => {
            let name = type_name::<wire::Agent>();
            RedactTarget::Agent(scrubber.twice(|s| s.message(&name, &agent)))
        }
        RedactTarget::ClientSession(history) => {
            let name = type_name::<wire::dump::ClientSessionHistory>();
            RedactTarget::ClientSession(scrubber.twice(|s| s.message(&name, &history)))
        }
    }
}

fn type_name<T: Name>() -> String {
    format!(".{}", T::full_name())
}

/// A checkpoint that decodes as the kind's state is written again with its
/// protobuf values walked by descriptor, then walked as JSON. One that does
/// not decode is walked as JSON with every hex string (an encoded message
/// or id it cannot read) emptied.
fn checkpoint<S: Checkpoint>(scrubber: &mut Scrubber, bytes: &[u8]) -> Vec<u8> {
    let json = match decode_checkpoint::<S>(bytes) {
        Ok(state) => serde_pb::scrubbing(scrubber, || encode_checkpoint(&state)),
        Err(_) => {
            let Ok(mut value) = serde_json::from_slice::<Value>(bytes) else {
                return Vec::new();
            };
            empty_hex(&mut value);
            serde_json::to_vec(&value).expect("JSON writes")
        }
    };
    let mut value: Value = serde_json::from_slice(&json).expect("a checkpoint is JSON");
    scrubber.json(&mut value);
    serde_json::to_vec(&value).expect("JSON writes")
}

fn empty_hex(value: &mut Value) {
    match value {
        Value::String(text) if !text.is_empty() && crate::from_hex(text).is_ok() => text.clear(),
        Value::Array(values) => values.iter_mut().for_each(empty_hex),
        Value::Object(map) => map.values_mut().for_each(empty_hex),
        _ => {}
    }
}

/// One redaction pass over one target. The first pass only learns the
/// session ids the target names, so the second can replace them wherever
/// they appear, including before the field that names them.
#[derive(Default)]
pub(crate) struct Scrubber {
    item_body: String,
    snapshot_body: String,
    input_body: String,
    sessions: BTreeSet<String>,
    learning: bool,
}

impl Scrubber {
    fn twice<T>(&mut self, mut pass: impl FnMut(&mut Self) -> T) -> T {
        self.learning = true;
        pass(self);
        self.learning = false;
        pass(self)
    }

    /// An encoded message of the named type (with its leading dot), walked
    /// field by field and written again in the same order.
    pub(crate) fn message(&mut self, name: &str, bytes: &[u8]) -> Vec<u8> {
        let Some(fields) = schema().get(name) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(bytes.len());
        let mut rest = bytes;
        while !rest.is_empty() {
            let before = rest;
            let Some(key) = read_varint(&mut rest) else {
                break;
            };
            let number = (key >> 3) as i32;
            let field = fields.get(&number);
            let fixed = match key & 7 {
                0 => {
                    if read_varint(&mut rest).is_none() {
                        break;
                    }
                    None
                }
                1 => Some(8),
                5 => Some(4),
                2 => {
                    let Some(len) = read_varint(&mut rest) else {
                        break;
                    };
                    let Some(payload) = take(&mut rest, len as usize) else {
                        break;
                    };
                    let Some(field) = field else {
                        continue;
                    };
                    let redacted = self.length_delimited(name, field, payload);
                    write_varint(&mut out, key);
                    write_varint(&mut out, redacted.len() as u64);
                    out.extend_from_slice(&redacted);
                    continue;
                }
                // Groups are not in this schema; stop rather than guess.
                _ => break,
            };
            if let Some(width) = fixed
                && take(&mut rest, width).is_none()
            {
                break;
            }
            if field.is_some() {
                out.extend_from_slice(&before[..before.len() - rest.len()]);
            }
        }
        out
    }

    /// An encoded item body of this kind.
    pub(crate) fn item_body(&mut self, bytes: &[u8]) -> Vec<u8> {
        let name = self.item_body.clone();
        self.message(&name, bytes)
    }

    fn length_delimited(&mut self, message: &str, field: &Field, payload: &[u8]) -> Vec<u8> {
        match field.ty {
            Type::String => {
                let text = String::from_utf8_lossy(payload);
                // A protobuf map<string, string> of environment variables:
                // the names stay, every value goes.
                if message.ends_with("EnvEntry") && field.name == "value" {
                    SECRET_PLACEHOLDER.as_bytes().to_vec()
                } else if is_session_key(&normalized(&field.name)) {
                    self.session(&text).into_bytes()
                } else {
                    self.string(&text).into_bytes()
                }
            }
            Type::Message => self.message(&field.type_name, payload),
            Type::Bytes => match (message, field.name.as_str()) {
                (".amux.v1.Item", "body") => {
                    let name = self.item_body.clone();
                    self.message(&name, payload)
                }
                (".amux.v1.Snapshot", "body") => {
                    let name = self.snapshot_body.clone();
                    self.message(&name, payload)
                }
                (".amux.v1.Input", "body") => {
                    let name = self.input_body.clone();
                    self.message(&name, payload)
                }
                (_, name) if name.ends_with("_json") => self.json_bytes(payload),
                (_, name) if is_id_field(name) => payload.to_vec(),
                _ => match std::str::from_utf8(payload) {
                    Ok(text) => self.string(text).into_bytes(),
                    Err(_) => payload.to_vec(),
                },
            },
            // Packed repeated numbers carry no text.
            _ => payload.to_vec(),
        }
    }

    /// JSON bytes walked as JSON; anything else as text.
    pub(crate) fn json_bytes(&mut self, bytes: &[u8]) -> Vec<u8> {
        match serde_json::from_slice::<Value>(bytes) {
            Ok(mut value) => {
                self.json(&mut value);
                serde_json::to_vec(&value).expect("JSON writes")
            }
            Err(_) => self.text(&String::from_utf8_lossy(bytes)).into_bytes(),
        }
    }

    pub(crate) fn json(&mut self, value: &mut Value) {
        self.json_keys(value);
        let mut summary = RedactionSummary::default();
        redaction::redact_value(value, rules(), &mut summary);
    }

    /// The rules the shared capture redactor does not know: environment
    /// maps, session ids by key, JSON inside strings, and the text rules.
    fn json_keys(&mut self, value: &mut Value) {
        match value {
            Value::Object(map) => {
                let entries = std::mem::take(map);
                for (key, mut value) in entries {
                    let norm = normalized(&key);
                    if is_env_key(&norm) {
                        blank_env(&mut value);
                    } else if is_session_key(&norm) && value.is_string() {
                        let text = value.as_str().unwrap_or_default().to_owned();
                        value = Value::String(self.session(&text));
                    } else {
                        if matches!(norm.as_str(), "thread" | "session")
                            && let Some(Value::String(id)) = value.get_mut("id")
                        {
                            *id = self.session(id);
                        }
                        self.json_keys(&mut value);
                    }
                    map.insert(self.text(&key), value);
                }
            }
            Value::Array(values) => values.iter_mut().for_each(|value| self.json_keys(value)),
            Value::String(text) => *text = self.string(text),
            _ => {}
        }
    }

    /// A string value: JSON inside it is walked as JSON, the rest as text.
    fn string(&mut self, text: &str) -> String {
        let trimmed = text.trim_start();
        if (trimmed.starts_with('{') || trimmed.starts_with('['))
            && let Ok(mut value) = serde_json::from_str::<Value>(text)
        {
            self.json(&mut value);
            return value.to_string();
        }
        self.text(text)
    }

    fn text(&mut self, text: &str) -> String {
        let mut out = text.to_owned();
        for id in &self.sessions {
            if out.contains(id.as_str()) {
                out = out.replace(id.as_str(), &session_placeholder(id));
            }
        }
        let mut summary = RedactionSummary::default();
        redaction::redact_text(&out, rules(), &mut summary)
    }

    fn session(&mut self, id: &str) -> String {
        if id.starts_with(SESSION_PLACEHOLDER_PREFIX) || id.len() < 8 {
            return self.text(id);
        }
        if self.learning {
            self.sessions.insert(id.to_owned());
        }
        session_placeholder(id)
    }
}

fn session_placeholder(id: &str) -> String {
    // FNV-1a: stable across builds, so one session reads the same in every
    // part of a dump.
    let hash = id.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    format!("{SESSION_PLACEHOLDER_PREFIX}{hash:08x}>")
}

fn normalized(key: &str) -> String {
    key.chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_session_key(norm: &str) -> bool {
    norm.ends_with("sessionid")
        || norm.ends_with("threadid")
        || matches!(
            norm,
            "session" | "providersession" | "conversationid" | "rolloutid"
        )
}

fn is_env_key(norm: &str) -> bool {
    matches!(
        norm,
        "env" | "envs" | "environment" | "envvars" | "environmentvariables"
    )
}

fn is_id_field(name: &str) -> bool {
    name == "id" || name == "agent" || name == "hash" || name.ends_with("_id")
}

/// An environment map keeps its names; every value is blanked. A list of
/// `NAME=value` strings keeps each name.
fn blank_env(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for value in map.values_mut() {
                if !value.is_null() {
                    *value = Value::String(SECRET_PLACEHOLDER.to_owned());
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                match value {
                    Value::String(text) => {
                        if let Some((name, _)) = text.split_once('=') {
                            *text = format!("{name}={SECRET_PLACEHOLDER}");
                        } else {
                            *text = SECRET_PLACEHOLDER.to_owned();
                        }
                    }
                    other => blank_env(other),
                }
            }
        }
        Value::Null => {}
        other => *other = Value::String(SECRET_PLACEHOLDER.to_owned()),
    }
}

fn read_varint(bytes: &mut &[u8]) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = bytes.split_first()?;
        *bytes = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn take<'a>(bytes: &mut &'a [u8], len: usize) -> Option<&'a [u8]> {
    if bytes.len() < len {
        return None;
    }
    let (head, rest) = bytes.split_at(len);
    *bytes = rest;
    Some(head)
}

struct Field {
    name: String,
    ty: Type,
    type_name: String,
}

/// Every message in the wire schema by its dotted full name, with its
/// fields by number.
fn schema() -> &'static HashMap<String, HashMap<i32, Field>> {
    static SCHEMA: OnceLock<HashMap<String, HashMap<i32, Field>>> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let set = FileDescriptorSet::decode(wire::DESCRIPTOR_SET).expect("descriptor set decodes");
        let mut schema = HashMap::new();
        for file in &set.file {
            let package = file.package.clone().unwrap_or_default();
            for message in &file.message_type {
                add_message(&mut schema, &format!(".{package}"), message);
            }
        }
        schema
    })
}

fn add_message(
    schema: &mut HashMap<String, HashMap<i32, Field>>,
    scope: &str,
    message: &DescriptorProto,
) {
    let name = format!("{scope}.{}", message.name());
    let fields = message
        .field
        .iter()
        .map(|field| {
            (
                field.number(),
                Field {
                    name: field.name().to_owned(),
                    ty: field.r#type(),
                    type_name: field.type_name().to_owned(),
                },
            )
        })
        .collect();
    for nested in &message.nested_type {
        add_message(schema, &name, nested);
    }
    schema.insert(name, fields);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_placeholders_are_stable_and_idempotent() {
        let mut scrubber = Scrubber::default();
        let once = scrubber.session("661bdeed-c3d2-4270-9da6-0aa62531459e");
        assert_eq!(once, "<SESSION-bed1e833>");
        assert_eq!(scrubber.session(&once), once);
    }
}
