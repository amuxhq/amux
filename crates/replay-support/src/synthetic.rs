//! A provider stream made up from an agent's record, so a report that
//! carries no words can still be replayed through the interpreter that
//! wrote it.
//!
//! The record keeps every event's skeleton (`interpret::skeleton`): its
//! field names, the provider's fixed vocabulary, and every other value as
//! an alias with its size. The stream puts a made-up value of that size
//! wherever an alias stood, the same value for every alias of one hash, so
//! ids that matched still match and texts that were equal stay equal. A
//! value the interpreter itself chose (a request id, a user message's
//! uuid, an item key) is learned from the interpreter as the stream is
//! built: each event is stepped as soon as it is made, and an alias the
//! record shows in what the step concluded is bound to what the
//! interpreter concluded now, so a later reply that echoes it echoes the
//! interpreter's own value.
//!
//! What it cannot stand for is said in [`Synthetic::notes`]: a ring
//! segment that started from a state a fresh interpreter does not have
//! (an older checkpoint's open items), and a frame the provider's own
//! types would not read.

use std::collections::{HashMap, HashSet};

use interpret::checks::Violation;
use interpret::claude_pty::ClaudePty;
use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::Codex;
use interpret::ring::Entry;
use interpret::skeleton::{self, Field, Leaf, input_message, leaf};
use interpret::{Channel, Effect, Event, Fact, Interpreter, Stepped};
use prost::Message as _;
use prost_types::field_descriptor_proto::Type;
use serde_json::Value;
use wire::{AgentSpec, Envelope, Git, IncarnationEnd, Input};

/// A stream made up from a record.
#[derive(Clone, Debug)]
pub struct Synthetic {
    pub spec: AgentSpec,
    /// Every event, in the record's order.
    pub events: Vec<Event>,
    /// The value each alias's hash stands for in this stream.
    pub values: HashMap<String, Vec<u8>>,
    /// What the stream could not stand for.
    pub notes: Vec<String>,
}

impl Synthetic {
    /// The stream as a facts-ring segment: one line per event, as the
    /// agent process writes them.
    pub fn ring(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for event in &self.events {
            out.extend(serde_json::to_vec(&Entry::of(event)).expect("an entry serializes"));
            out.push(b'\n');
        }
        out
    }
}

/// A made-up stream's run through the real interpreter.
#[derive(Clone, Debug)]
pub struct Reproduction {
    pub kind: wire::Kind,
    pub synthetic: Synthetic,
    /// What each event's step concluded, in order.
    pub steps: Vec<Stepped>,
    /// Each check a step broke, by the event's index.
    pub broken: Vec<(usize, Violation)>,
}

/// Makes up the stream of `record`, an agent's record as the agent wrote
/// it, and runs it through the interpreter of the record's kind.
pub fn reproduce(record: &Value) -> Result<Reproduction, String> {
    let kind = record["settings"]["kind"].as_str().unwrap_or_default();
    match wire::Kind::from_str_name(kind) {
        Some(wire::Kind::ClaudeSdk) => Ok(reproduce_as::<ClaudeSdk>(record)),
        Some(wire::Kind::ClaudePty) => Ok(reproduce_as::<ClaudePty>(record)),
        Some(wire::Kind::Codex) => Ok(reproduce_as::<Codex>(record)),
        _ => Err(format!("a record of kind {kind:?} has no interpreter")),
    }
}

/// [`reproduce`] for interpreter `I`.
pub fn reproduce_as<I: Interpreter>(record: &Value) -> Reproduction {
    let synthetic = synthesize::<I>(record);
    let steps = run::<I>(&synthetic);
    let broken = steps
        .iter()
        .enumerate()
        .filter_map(|(index, stepped)| Some((index, stepped.broken.clone()?)))
        .collect();
    Reproduction {
        kind: I::KIND,
        synthetic,
        steps,
        broken,
    }
}

/// Steps every event of `synthetic` through a fresh interpreter `I`.
pub fn run<I: Interpreter>(synthetic: &Synthetic) -> Vec<Stepped> {
    let (mut state, _) = I::initial(&synthetic.spec);
    synthetic
        .events
        .iter()
        .map(|event| I::step(&mut state, event.clone()))
        .collect()
}

/// Makes up the stream of `record` for interpreter `I`.
pub fn synthesize<I: Interpreter>(record: &Value) -> Synthetic {
    let mut values = Values::default();
    values.named(record);
    let mut notes = Vec::new();
    let spec = values.spec::<I>(&record["settings"]);
    let (mut state, _) = I::initial(&spec);
    let fresh = I::structure(&state);
    let mut events = Vec::new();
    for (index, segment) in array(&record["segments"]).iter().enumerate() {
        let checkpoint = &segment["checkpoint"];
        if checkpoint.is_null() {
            notes.push(format!("segment {index}: its checkpoint was unreadable"));
            continue;
        }
        if index == 0 && !starts_fresh(checkpoint, &fresh) {
            notes.push(format!(
                "segment {index}: it started from a state a fresh interpreter does not have: {checkpoint}"
            ));
        }
        if let Some(unreadable) = segment["unreadable"].as_u64() {
            notes.push(format!(
                "segment {index}: {unreadable} lines were unreadable"
            ));
        }
        for fed in array(&segment["events"]) {
            let Some(event) = values.event::<I>(&fed["event"], &mut notes) else {
                notes.push(format!(
                    "event {}: one the stream cannot make: {}",
                    events.len(),
                    fed["event"]
                ));
                continue;
            };
            let stepped = I::step(&mut state, event.clone());
            values.learn::<I>(fed, &stepped, &state);
            events.push(event);
            values.events = events.len();
        }
    }
    Synthetic {
        spec,
        events,
        values: values.bound,
        notes,
    }
}

/// Whether a record's checkpoint shows the state a fresh interpreter
/// starts in, by what is open and pending.
fn starts_fresh(checkpoint: &Value, fresh: &interpret::shape::Structure) -> bool {
    let len = |field: &str| array(&checkpoint[field]).len();
    len("open_items") == fresh.open_items.len()
        && len("queue") == fresh.queue.len()
        && len("asks") == fresh.asks.len()
        && len("submitted") == fresh.submitted.len()
        && len("pending_messages") == fresh.pending_messages.len()
        && checkpoint["turn"].as_u64() == fresh.turn
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

/// The values aliases stand for: bound when the interpreter chose them,
/// made up when the stream first needs them.
#[derive(Default)]
struct Values {
    bound: HashMap<String, Vec<u8>>,
    taken: HashSet<Vec<u8>>,
    /// How many events have been made.
    events: usize,
}

impl Values {
    /// Binds every value the record kept with its alias's hash, wherever
    /// it is, so an alias of the same value met earlier stands for it.
    fn named(&mut self, value: &Value) {
        match value {
            Value::String(text) => {
                if let Some(Leaf::Named { hash, value }) = leaf(text) {
                    self.bind(hash, value.as_bytes());
                }
            }
            Value::Array(items) => items.iter().for_each(|item| self.named(item)),
            Value::Object(fields) => fields.values().for_each(|value| self.named(value)),
            _ => {}
        }
    }

    fn bind(&mut self, hash: &str, value: &[u8]) {
        if hash.is_empty() || self.bound.contains_key(hash) {
            return;
        }
        self.bound.insert(hash.to_owned(), value.to_vec());
        self.taken.insert(value.to_vec());
    }

    /// The value `hash` stands for, made up as `bytes` bytes with `lines`
    /// line breaks when none is bound yet.
    fn value(&mut self, hash: &str, prefix: &str, bytes: usize, lines: usize) -> Vec<u8> {
        if let Some(value) = self.bound.get(hash) {
            return value.clone();
        }
        let mut attempt = 0;
        let value = loop {
            let value = made_up(hash, attempt, prefix, bytes, lines);
            if !self.taken.contains(&value) || attempt > 64 {
                break value;
            }
            attempt += 1;
        };
        self.bind(hash, &value);
        value
    }

    fn text(&mut self, text: &str) -> Vec<u8> {
        match leaf(text) {
            Some(Leaf::Kept(kept) | Leaf::Named { value: kept, .. }) => kept.as_bytes().to_vec(),
            Some(Leaf::Number { characters }) => number(characters).into_bytes(),
            Some(Leaf::Alias {
                prefix,
                hash,
                bytes,
                lines,
            }) => {
                let mut out = prefix.as_bytes().to_vec();
                out.extend(self.value(hash, prefix, bytes, lines));
                out
            }
            None => text.as_bytes().to_vec(),
        }
    }

    /// The JSON a skeleton stands for.
    fn json(&mut self, skeleton: &Value) -> Value {
        match skeleton {
            Value::String(text) => match leaf(text) {
                Some(Leaf::Number { characters }) => {
                    serde_json::from_str(&number(characters)).unwrap_or(Value::Null)
                }
                _ => Value::String(String::from_utf8_lossy(&self.text(text)).into_owned()),
            },
            Value::Array(items) => Value::Array(items.iter().map(|item| self.json(item)).collect()),
            Value::Object(fields) if fields.contains_key("=text") => {
                let mut text = Vec::new();
                for piece in array(&fields["=text"]) {
                    text.extend(self.text(piece.as_str().unwrap_or_default()));
                }
                Value::String(String::from_utf8_lossy(&text).into_owned())
            }
            Value::Object(fields) if fields.contains_key("=json") => {
                let size = fields["bytes"].as_u64().unwrap_or_default() as usize;
                let mut text =
                    serde_json::to_string(&self.json(&fields["=json"])).unwrap_or_default();
                if text.len() < size {
                    text.extend(std::iter::repeat_n(' ', size - text.len()));
                }
                Value::String(text)
            }
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(name, value)| {
                        let name = if name.starts_with('@') {
                            String::from_utf8_lossy(&self.text(name)).into_owned()
                        } else {
                            name.clone()
                        };
                        (name, self.json(value))
                    })
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// The encoding of the protobuf message `message` a skeleton stands
    /// for.
    fn proto(&mut self, message: &str, skeleton: &Value) -> Vec<u8> {
        let Some(fields) = skeleton::schema().get(message) else {
            return Vec::new();
        };
        let mut present: Vec<(&Field, &Value)> = skeleton
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(name, value)| {
                let field = fields.values().find(|field| field.name == *name)?;
                Some((field, value))
            })
            .collect();
        present.sort_by_key(|(field, _)| field.number);
        let mut out = Vec::new();
        for (field, value) in present {
            let values = if field.repeated {
                array(value)
            } else {
                std::slice::from_ref(value)
            };
            if field.repeated && packed(field.ty) {
                let mut run = Vec::new();
                for value in values {
                    scalar(field.ty, value, &mut run);
                }
                tag(field.number, 2, &mut out);
                prost::encoding::encode_varint(run.len() as u64, &mut out);
                out.extend(run);
                continue;
            }
            for value in values {
                match field.ty {
                    Type::String | Type::Bytes | Type::Message => {
                        let bytes = match (field.ty, value) {
                            (Type::Message, _) => self.proto(&field.type_name, value),
                            (_, Value::Object(json)) => {
                                let size = json
                                    .get("bytes")
                                    .and_then(Value::as_u64)
                                    .unwrap_or_default()
                                    as usize;
                                let json = json.get("=json").map(|json| self.json(json));
                                let mut bytes = serde_json::to_vec(&json.unwrap_or(Value::Null))
                                    .unwrap_or_default();
                                if bytes.len() < size {
                                    bytes.resize(size, b' ');
                                }
                                bytes
                            }
                            (_, value) => self.text(value.as_str().unwrap_or_default()),
                        };
                        tag(field.number, 2, &mut out);
                        prost::encoding::encode_varint(bytes.len() as u64, &mut out);
                        out.extend(bytes);
                    }
                    ty => {
                        tag(field.number, wire_type(ty), &mut out);
                        scalar(ty, value, &mut out);
                    }
                }
            }
        }
        out
    }

    /// The spec a record's settings stand for.
    fn spec<I: Interpreter>(&mut self, settings: &Value) -> AgentSpec {
        let bytes = self.proto("amux.v1.AgentSpec", &settings["spec"]);
        let mut spec = AgentSpec::decode(bytes.as_slice()).unwrap_or_default();
        spec.kind = I::KIND as i32;
        // The provider's file name is kept beside the spec, its folders
        // made up to the command's length.
        if let Some(provider) = settings["provider"].as_str() {
            let len = spec.provider_command.len();
            spec.provider_command = if len > provider.len() + 1 {
                format!("/{}/{provider}", "p".repeat(len - provider.len() - 2))
            } else {
                format!("/{provider}")
            };
        }
        spec
    }

    /// The event a record's entry stands for.
    fn event<I: Interpreter>(
        &mut self,
        recorded: &Value,
        notes: &mut Vec<String>,
    ) -> Option<Event> {
        let event =
            match recorded["event"].as_str()? {
                "fact" => {
                    let channel: Channel =
                        serde_json::from_value(recorded["channel"].clone()).ok()?;
                    let shape = &recorded["shape"];
                    let mut payload = match shape {
                        Value::String(text) => self.text(text),
                        shape => {
                            let frame = self.json(shape);
                            if let Some(drift) = drift(I::KIND, channel, &frame) {
                                notes.push(format!(
                                "event {}: a made-up frame the provider's types refuse: {drift}",
                                self.events
                            ));
                            }
                            serde_json::to_vec(&frame).ok()?
                        }
                    };
                    let bytes = recorded["bytes"].as_u64().unwrap_or_default() as usize;
                    // Every reader takes trailing whitespace, so the made-up
                    // fact is the recorded one's size.
                    if payload.len() < bytes {
                        payload.resize(bytes, b' ');
                    }
                    Event::Fact(Fact { channel, payload })
                }
                "input" => {
                    let size = recorded["id_bytes"].as_u64().unwrap_or(16) as usize;
                    let input_id = self.id(recorded["input_id"].as_str().unwrap_or_default(), size);
                    let body = self.proto(input_message(I::KIND), &recorded["shape"]);
                    Event::Input(Input {
                        input_id,
                        kind: I::KIND as i32,
                        body,
                    })
                }
                "agent_message" => {
                    let bytes = self.proto("amux.v1.Envelope", &recorded["shape"]);
                    Event::AgentMessage(Envelope::decode(bytes.as_slice()).ok()?)
                }
                "rename" => Event::Rename {
                    name: String::from_utf8_lossy(
                        &self.text(recorded["shape"].as_str().unwrap_or("=")),
                    )
                    .into_owned(),
                },
                "tick" => Event::Tick {
                    at_ms: recorded["at_ms"].as_i64().unwrap_or_default(),
                },
                "provider_exit" => Event::ProviderExit {
                    code: recorded["code"].as_i64().map(|code| code as i32),
                },
                "daemon_lost" => Event::DaemonLost,
                "stop" => Event::StopRequested,
                "exiting" => Event::Exiting(IncarnationEnd {
                    cause: recorded["cause"].as_i64().unwrap_or_default() as i32,
                    detail: recorded["detail"]
                        .as_str()
                        .map(|detail| String::from_utf8_lossy(&self.text(detail)).into_owned()),
                }),
                "git" => Event::Git(recorded["repository"].as_bool().unwrap_or_default().then(
                    || {
                        let bytes = self.proto("amux.v1.Git", &recorded["shape"]);
                        Git::decode(bytes.as_slice()).unwrap_or_default()
                    },
                )),
                _ => return None,
            };
        Some(event)
    }

    /// The bytes a record's id alias (`id-<hash>`) stands for, made up as
    /// `size` bytes when none is bound.
    fn id(&mut self, alias: &str, size: usize) -> Vec<u8> {
        match alias.strip_prefix("id-") {
            Some(hash) => self.value(hash, "", size, 0),
            None => Vec::new(),
        }
    }

    /// Binds every alias the record shows in what a step concluded to what
    /// the interpreter concluded now.
    fn learn<I: Interpreter>(&mut self, fed: &Value, stepped: &Stepped, state: &I::State) {
        let step = &fed["step"];
        for (recorded, item) in array(&step["items"]).iter().zip(&stepped.step.items) {
            self.bind_alias(&recorded["key"], item.key.as_bytes());
            self.bind_alias(&recorded["input_id"], &item.input_id);
        }
        for (recorded, append) in array(&step["appends"]).iter().zip(&stepped.step.appends) {
            self.bind_alias(&recorded[0], append.key.as_bytes());
        }
        if let Some(snapshot) = &stepped.step.snapshot {
            let recorded = &step["snapshot"];
            for (alias, queued) in array(&recorded["queue"]).iter().zip(&snapshot.queue) {
                self.bind_alias(alias, &queued.input_id);
            }
            let asks = I::describe_snapshot(&snapshot.body).asks;
            for (alias, (ask, item)) in array(&recorded["asks"]).iter().zip(&asks) {
                self.bind_alias(&alias[0], ask.as_bytes());
                self.bind_alias(&alias[1], item.as_bytes());
            }
        }
        for (recorded, effect) in array(&fed["effects"]).iter().zip(&stepped.effects) {
            match effect {
                Effect::Reply { input_id, .. } => self.bind_alias(&recorded["id"], input_id),
                Effect::Inject { id, .. } => self.bind_alias(&recorded["id"], id),
                Effect::UserMessage { uuid, .. } => {
                    self.bind_alias(&recorded["id"], uuid.as_bytes())
                }
                Effect::ProviderSession { id } => self.bind_alias(&recorded["id"], id.as_bytes()),
                Effect::ProviderWrite(bytes) | Effect::CodexTurnInput { request: bytes, .. } => {
                    if let Ok(actual) = serde_json::from_slice::<Value>(bytes) {
                        self.bind_shape(&recorded["shape"], &actual);
                    }
                }
                _ => {}
            }
        }
        for (alias, id) in array(&fed["pending_messages"])
            .iter()
            .zip(I::pending_messages(state))
        {
            self.bind_alias(alias, &id);
        }
    }

    /// Binds a record's alias of an id or key (`id-<hash>`, `key-<hash>`,
    /// `turn:<hash>`) to the value it stood for.
    fn bind_alias(&mut self, alias: &Value, actual: &[u8]) {
        let Some(alias) = alias.as_str() else {
            return;
        };
        if let Some(hash) = ["id-", "key-", "name-"]
            .iter()
            .find_map(|prefix| alias.strip_prefix(prefix))
        {
            self.bind(hash, actual);
        } else if let Some((prefix, hash)) = alias.split_once(':')
            && let Some(rest) = actual
                .strip_prefix(prefix.as_bytes())
                .and_then(|rest| rest.strip_prefix(b":"))
        {
            self.bind(hash, rest);
        }
    }

    /// Binds every alias in a skeleton to the value at its place in
    /// `actual`.
    fn bind_shape(&mut self, skeleton: &Value, actual: &Value) {
        match (skeleton, actual) {
            (Value::String(text), Value::String(actual)) => {
                if let Some(Leaf::Alias { prefix, hash, .. }) = leaf(text)
                    && let Some(rest) = actual.strip_prefix(prefix)
                {
                    self.bind(hash, rest.as_bytes());
                }
            }
            (Value::Object(fields), Value::String(actual)) if fields.contains_key("=json") => {
                if let Ok(actual) = serde_json::from_str::<Value>(actual) {
                    self.bind_shape(&fields["=json"], &actual);
                }
            }
            (Value::Array(items), Value::Array(actual)) => {
                for (item, actual) in items.iter().zip(actual) {
                    self.bind_shape(item, actual);
                }
            }
            (Value::Object(fields), Value::Object(actual)) => {
                for (name, value) in fields {
                    if let Some(actual) = actual.get(name) {
                        self.bind_shape(value, actual);
                    }
                }
            }
            _ => {}
        }
    }
}

/// A made-up value: `bytes` bytes of letters and digits drawn from `hash`,
/// with `lines` line breaks spread through it. A tool server's tool name
/// keeps the `__` between its server and its tool.
fn made_up(hash: &str, attempt: u32, prefix: &str, bytes: usize, lines: usize) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let mut seed = hash.bytes().fold(
        u64::from(attempt).wrapping_add(0x9e37_79b9_7f4a_7c15),
        |seed, byte| (seed ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3),
    );
    let mut out: Vec<u8> = (0..bytes)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            ALPHABET[(seed % ALPHABET.len() as u64) as usize]
        })
        .collect();
    // A digit first, so a made-up value never reads as a schema's name
    // where the one it stands for did not.
    if let Some(first) = out.first_mut() {
        *first = b"23456789"[(seed % 8) as usize];
    }
    if prefix == "mcp__" && bytes >= 4 {
        let middle = (bytes - 2) / 2;
        out[middle] = b'_';
        out[middle + 1] = b'_';
    }
    for line in 1..=lines.min(bytes) {
        let at = (line * bytes / (lines + 1)).min(bytes - 1);
        out[at] = b'\n';
    }
    out
}

/// A number written with `characters` characters.
fn number(characters: usize) -> String {
    format!("1{}", "0".repeat(characters.saturating_sub(1)))
}

fn tag(number: i32, wire: u32, out: &mut Vec<u8>) {
    prost::encoding::encode_varint(((number as u64) << 3) | u64::from(wire), out);
}

fn wire_type(ty: Type) -> u32 {
    match ty {
        Type::Double | Type::Fixed64 | Type::Sfixed64 => 1,
        Type::Float | Type::Fixed32 | Type::Sfixed32 => 5,
        Type::String | Type::Bytes | Type::Message | Type::Group => 2,
        _ => 0,
    }
}

fn packed(ty: Type) -> bool {
    !matches!(ty, Type::String | Type::Bytes | Type::Message | Type::Group)
}

/// A scalar as a skeleton keeps it: its raw bits, or a bool.
fn scalar(ty: Type, value: &Value, out: &mut Vec<u8>) {
    let raw = match value {
        Value::Bool(value) => u64::from(*value),
        value => value.as_u64().unwrap_or_default(),
    };
    match wire_type(ty) {
        1 => out.extend(raw.to_le_bytes()),
        5 => out.extend((raw as u32).to_le_bytes()),
        _ => prost::encoding::encode_varint(raw, out),
    }
}

/// How a frame differs from what the provider's own types read, if it
/// does. Of a made-up frame, that is a value the record could not keep
/// that the provider's types take only from a closed set, unless the frame
/// it stands for differed too.
pub fn drift(kind: wire::Kind, channel: Channel, frame: &Value) -> Option<String> {
    let kind = match (kind, channel) {
        (wire::Kind::ClaudeSdk, Channel::Stream) => provider_fakes::Kind::ClaudeSdk,
        (wire::Kind::ClaudePty, Channel::Transcript | Channel::Hook) => {
            provider_fakes::Kind::ClaudePty
        }
        (wire::Kind::Codex, Channel::Rpc) => provider_fakes::Kind::Codex,
        _ => return None,
    };
    provider_fakes::shape::typed(kind, frame)
        .err()
        .map(|drift| drift.to_string())
}
