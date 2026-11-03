//! An agent's record: what a dump for one of the agent's own checks carries
//! of it, built only when the dump is made.
//!
//! The agent's facts ring already holds every event its interpreter was
//! fed since a checkpoint, with the interpreter's state at each segment's
//! start. [`record`] runs those events through the same interpreter again,
//! on the agent's own machine, and keeps what it concluded and never what
//! was said: each event's kind and size, a tool's name and the keys of its
//! input and result, the items it opened, updated and closed by key and
//! arm, the snapshot's phase and queue, turn ends, effects by kind and size
//! with the ids they answer, what its two queries answered after the step,
//! and the check the step broke. Ids, item keys and a tool server's names
//! become aliases, salted per record, so equal values stay equal within it
//! and none can be looked up. The checks run in the replay as they ran
//! live, since they judge the same steps.
//!
//! Nothing reads a record back but a person, so it is built as JSON
//! directly; a field that is empty or false is left out.

use std::collections::HashSet;

use serde_json::{Map, Value, json};
use wire::AgentSpec;

use crate::ring::Entry;
use crate::shape::{Name, Structure};
use crate::skeleton::{Skeleton, input_message, schema_name};
use crate::{Effect, Event, Interpreter, Stepped};

/// The size a report holds each record to once compressed (zstd, level
/// 3): a record past it is left out of the report.
pub const LIMIT: usize = 64 << 10;

/// The record of an agent under `spec` whose lifecycle stands as `core`,
/// from its facts ring's segments, oldest first, each the checkpoint at
/// its start and its bytes as written. `salt` keys the aliases.
///
/// A segment whose checkpoint this build cannot read runs no event, and a
/// line it cannot read is counted and skipped, as a resume skips it.
pub fn record<I: Interpreter>(
    spec: &AgentSpec,
    salt: [u8; 16],
    core: Value,
    segments: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>,
) -> Value {
    within::<I>(spec, salt, core, segments, |_| true)
}

/// [`record`], with its oldest events left out until `fits` holds it, so
/// a long session's record still fits a report and keeps the step that
/// broke and the steps that led to it. The segment the record then starts
/// in starts from the interpreter's state at its first kept event, and
/// `events_left_out` counts the events left out. At least the newest
/// event is kept, fitting or not.
pub fn within<I: Interpreter>(
    spec: &AgentSpec,
    salt: [u8; 16],
    core: Value,
    segments: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>,
    fits: impl Fn(&Value) -> bool,
) -> Value {
    let segments: Vec<(Vec<u8>, Vec<u8>)> = segments.into_iter().collect();
    let mut run = Run {
        alias: Alias { salt },
        version: None,
        open: HashSet::new(),
    };
    let built: Vec<Value> = segments
        .iter()
        .map(|(checkpoint, bytes)| run.segment::<I>(checkpoint, bytes, 0))
        .collect();
    let whole = run.record(spec, core.clone(), built.clone(), 0);
    if fits(&whole) {
        return whole;
    }
    let counts: Vec<usize> = built.iter().map(|segment| array(segment).len()).collect();
    let total: usize = counts.iter().sum();
    if total <= 1 {
        return whole;
    }
    // Where the record would start with `left_out` events left out: the
    // segment, and how many of its own events go.
    let start = |left_out: usize| {
        let mut rest = left_out;
        for (index, count) in counts.iter().enumerate() {
            if rest < *count {
                return (index, rest);
            }
            rest -= count;
        }
        (
            counts.len().saturating_sub(1),
            counts.last().copied().unwrap_or(0),
        )
    };
    // The fewest left out that fits, judged with each segment's events
    // as built: a segment cut short starts from a different state, whose
    // few open keys hardly change the size.
    let estimate = |left_out: usize| {
        let (index, skip) = start(left_out);
        let mut kept: Vec<Value> = built[index..].to_vec();
        if let Some(Value::Array(events)) = kept[0].get_mut("events") {
            events.drain(..skip.min(events.len()));
        }
        run.record(spec, core.clone(), kept, left_out)
    };
    let last = total.saturating_sub(1);
    let (mut low, mut high) = (1, last.max(1));
    while low < high {
        let middle = low + (high - low) / 2;
        if fits(&estimate(middle)) {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    let mut left_out = low;
    loop {
        let (index, skip) = start(left_out);
        let mut kept: Vec<Value> = built[index..].to_vec();
        if skip > 0 {
            let version = run.version.clone();
            let (checkpoint, bytes) = &segments[index];
            kept[0] = run.segment::<I>(checkpoint, bytes, skip);
            run.version = version;
        }
        let record = run.record(spec, core.clone(), kept, left_out);
        if left_out >= last || fits(&record) {
            return record;
        }
        left_out = (left_out + (left_out / 64).max(1)).min(last);
    }
}

fn array(segment: &Value) -> &[Value] {
    segment["events"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// The replay in progress.
struct Run {
    alias: Alias,
    /// The provider's version, from the last fact that reported it.
    version: Option<String>,
    /// Item keys, aliased, emitted and not yet in their final state.
    open: HashSet<String>,
}

impl Run {
    /// The record of `segments`, with how many older events were left out.
    fn record(
        &self,
        spec: &AgentSpec,
        core: Value,
        segments: Vec<Value>,
        left_out: usize,
    ) -> Value {
        let config = spec.config.clone().unwrap_or_default();
        let flags: Vec<&str> = spec
            .provider_args
            .iter()
            .filter(|arg| arg.starts_with('-'))
            .map(|flag| flag.split('=').next().unwrap_or_default())
            .filter(|flag| schema_name(flag.trim_start_matches('-')))
            .collect();
        let mut record = json!({
            "settings": {
                "kind": spec.kind().as_str_name(),
                "provider": std::path::Path::new(&spec.provider_command)
                    .file_name()
                    .map(|name| name.to_string_lossy()),
                "version": self.version,
                "model": config.model,
                "permission": config.permission,
                "mode": config.mode,
                "effort": config.effort,
                "flags": flags,
                "spec": spec_shape(&self.alias.skeleton(), spec),
            },
            "core": core,
            "segments": segments,
        });
        if left_out > 0 {
            record["events_left_out"] = left_out.into();
        }
        record
    }

    /// One segment's record, its first `skip` events stepped and left
    /// out: the segment then starts from the state they leave.
    fn segment<I: Interpreter>(&mut self, checkpoint: &[u8], bytes: &[u8], skip: usize) -> Value {
        let Ok(mut state) = crate::decode_checkpoint::<I::State>(checkpoint) else {
            return json!({ "checkpoint": null, "events": [] });
        };
        let entries: Vec<Entry> = core_ring::inputs::<Entry>(bytes);
        let mut unreadable = 0;
        let mut entries = entries.into_iter();
        let mut skipped = 0;
        while skipped < skip {
            let Some(entry) = entries.next() else {
                break;
            };
            match entry.event() {
                Some(event) => {
                    if let Event::Fact(fact) = &event
                        && let Some(version) = I::fact_shape(fact).version
                    {
                        self.version = Some(version);
                    }
                    I::step(&mut state, event);
                    skipped += 1;
                }
                None => unreadable += 1,
            }
        }
        let alias = &self.alias;
        let Structure {
            open_items,
            queue,
            asks,
            submitted,
            pending_messages,
            turn,
            phase,
        } = I::structure(&state);
        let open_items: Vec<(String, String)> = open_items
            .into_iter()
            .map(|(key, arm)| (alias.key(&key), arm))
            .collect();
        let checkpoint = json!({
            "open_items": open_items,
            "queue": alias.ids(&queue),
            "asks": asks
                .iter()
                .map(|(ask, item)| (alias.key(ask), alias.key(item)))
                .collect::<Vec<_>>(),
            "submitted": alias.ids(&submitted),
            "pending_messages": alias.ids(&pending_messages),
            "typed_unrecorded": I::typed_unrecorded(&state),
            "turn": turn,
            "phase": phase,
        });
        self.open = open_items.into_iter().map(|(key, _)| key).collect();
        let mut events = Vec::new();
        for entry in entries {
            let Some(event) = entry.event() else {
                unreadable += 1;
                continue;
            };
            let mut fed = Map::new();
            fed.insert("event".into(), self.event::<I>(&event));
            let stepped = I::step(&mut state, event);
            self.concluded::<I>(&mut fed, &stepped, &state);
            events.push(Value::Object(fed));
        }
        let mut segment = json!({ "checkpoint": checkpoint, "events": events });
        if unreadable > 0 {
            segment["unreadable"] = unreadable.into();
        }
        segment
    }

    fn event<I: Interpreter>(&mut self, event: &Event) -> Value {
        let alias = &self.alias;
        let value = match event {
            Event::Fact(fact) => {
                let shape = I::fact_shape(fact);
                if shape.version.is_some() {
                    self.version = shape.version;
                }
                let tools: Vec<Value> = shape
                    .tools
                    .iter()
                    .map(|tool| {
                        sparse(json!({
                            "result": tool.result,
                            "name": tool.name.as_ref().map(|name| alias.name(name)),
                            "keys": tool.keys.iter().map(|key| alias.name(key)).collect::<Vec<_>>(),
                        }))
                    })
                    .collect();
                json!({
                    "event": "fact",
                    "channel": fact.channel,
                    "variant": shape.variant,
                    "bytes": fact.payload.len(),
                    "tools": tools,
                    "shape": alias.skeleton().fact(I::KIND, &fact.payload),
                })
            }
            Event::Input(input) => {
                let shape = I::input_shape(input);
                json!({
                    "event": "input",
                    "input_id": alias.id(&input.input_id),
                    "id_bytes": input.input_id.len(),
                    "arm": shape.arm,
                    "answer": shape.answer,
                    "ask_key": shape.ask_key.map(|key| alias.key(&key)),
                    "target": shape.target.map(|target| alias.id(&target)),
                    "bytes": input.body.len(),
                    "shape": alias.skeleton().proto(input_message(I::KIND), &input.body),
                })
            }
            Event::AgentMessage(envelope) => message(alias, envelope),
            Event::Rename { name } => json!({
                "event": "rename",
                "bytes": name.len(),
                "shape": alias.skeleton().text(name),
            }),
            Event::Tick { at_ms } => json!({ "event": "tick", "at_ms": at_ms }),
            Event::ProviderExit { code } => json!({ "event": "provider_exit", "code": code }),
            Event::DaemonLost => json!({ "event": "daemon_lost" }),
            Event::StopRequested => json!({ "event": "stop" }),
            Event::Exiting(end) => json!({
                "event": "exiting",
                "cause": end.cause,
                "detail": end.detail.as_deref().map(|detail| alias.skeleton().text(detail)),
            }),
            Event::Git(facts) => json!({
                "event": "git",
                "git": facts.as_ref().map(git),
                "repository": facts.is_some(),
                "shape": facts.as_ref().map(|facts| {
                    alias.skeleton().proto("amux.v1.Git", &prost::Message::encode_to_vec(facts))
                }),
            }),
        };
        sparse(value)
    }

    /// What the interpreter concluded of an event, into its entry.
    fn concluded<I: Interpreter>(
        &mut self,
        fed: &mut Map<String, Value>,
        stepped: &Stepped,
        state: &I::State,
    ) {
        let alias = &self.alias;
        let step = &stepped.step;
        let items: Vec<Value> = step
            .items
            .iter()
            .map(|item| {
                let key = alias.key(&item.key);
                let shape = I::item_shape(&item.body);
                let op = match (shape.complete, self.open.contains(&key)) {
                    (true, true) => "close",
                    (true, false) => "whole",
                    (false, true) => "update",
                    (false, false) => "open",
                };
                if shape.complete {
                    self.open.remove(&key);
                } else {
                    self.open.insert(key.clone());
                }
                sparse(json!({
                    "key": key,
                    "arm": shape.arm,
                    "op": op,
                    "input_id": alias.id(&item.input_id),
                    "bytes": item.text.len() + item.body.len(),
                }))
            })
            .collect();
        let appends: Vec<(String, usize)> = step
            .appends
            .iter()
            .map(|append| (alias.key(&append.key), append.text.len()))
            .collect();
        let snapshot = step.snapshot.as_ref().map(|snapshot| {
            let queue: Vec<String> = snapshot
                .queue
                .iter()
                .map(|queued| alias.id(&queued.input_id))
                .collect();
            let asks: Vec<(String, String)> = I::describe_snapshot(&snapshot.body)
                .asks
                .iter()
                .map(|(ask, item)| (alias.key(ask), alias.key(item)))
                .collect();
            json!({
                "phase": snapshot.phase,
                "queue": queue,
                "asks": asks,
                "working_on": snapshot.working_on.is_some(),
                "background_jobs": snapshot.background_jobs,
                "bytes": snapshot.body.len(),
            })
        });
        let concluded = json!({
            "step": {
                "items": items,
                "appends": appends,
                "snapshot": snapshot,
                // The turn's number, its outcome and whether it was the last.
                "turn_end": step.turn_end.as_ref().map(|end| (end.turn_id, end.outcome, end.last)),
                "incarnation_end": step.incarnation_end.as_ref().map(|end| end.cause),
            },
            "effects": stepped
                .effects
                .iter()
                .map(|effect| sparse(effect_record(alias, I::KIND, effect)))
                .collect::<Vec<_>>(),
            "pending_messages": alias.ids(&I::pending_messages(state)),
            "typed_unrecorded": I::typed_unrecorded(state),
            "broken": stepped.broken.as_ref().map(|broken| broken.invariant.id),
        });
        if let Value::Object(concluded) = sparse(concluded) {
            fed.extend(concluded);
        }
    }
}

/// The value with every null, false, empty string, list and object left
/// out, each nested object first. A skeleton (`shape`) is kept whole: its
/// empty and false values are the event's own.
fn sparse(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| match key.as_str() {
                    "shape" => (key, value),
                    _ => (key, sparse(value)),
                })
                .filter(|(key, value)| match value {
                    Value::Null => false,
                    _ if key == "shape" => true,
                    Value::Bool(false) => false,
                    Value::String(text) => !text.is_empty(),
                    Value::Array(list) => !list.is_empty(),
                    Value::Object(map) => !map.is_empty(),
                    Value::Bool(true) | Value::Number(_) => true,
                })
                .collect(),
        ),
        other => other,
    }
}

/// Turns ids, keys and names into aliases: a salted hash, so equal values
/// alias alike within one record and nothing outside it can look one up.
struct Alias {
    salt: [u8; 16],
}

impl Alias {
    fn hash(&self, bytes: &[u8]) -> String {
        self.skeleton().hash(bytes)
    }

    /// An id's alias; an empty id, which means none, stays empty.
    fn id(&self, bytes: &[u8]) -> String {
        if bytes.is_empty() {
            return String::new();
        }
        format!("id-{}", self.hash(bytes))
    }

    fn skeleton(&self) -> Skeleton {
        Skeleton { salt: self.salt }
    }

    fn ids(&self, ids: &[Vec<u8>]) -> Vec<String> {
        ids.iter().map(|id| self.id(id)).collect()
    }

    /// An item or ask key's alias, keeping an interpreter's own prefix
    /// (`turn:`, `ask:`), which names the key's kind.
    fn key(&self, key: &str) -> String {
        if key.is_empty() {
            return String::new();
        }
        match key.split_once(':') {
            Some((prefix, rest))
                if (1..=12).contains(&prefix.len())
                    && prefix.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') =>
            {
                format!("{prefix}:{}", self.hash(rest.as_bytes()))
            }
            _ => format!("key-{}", self.hash(key.as_bytes())),
        }
    }

    /// A provider's own name as it is when it reads as a schema's; a tool
    /// server's, or any that reads as data, aliased.
    ///
    /// A tool server's tool keeps the `mcp__` Claude names it with, and a
    /// Codex server's tool (`server/tool`) each part's alias, as an event's
    /// skeleton does.
    fn name(&self, name: &Name) -> String {
        let alias = |name: &str| format!("name-{}", self.hash(name.as_bytes()));
        match name {
            Name::Own(name) if schema_name(name) => name.clone(),
            Name::Server(name) if name.starts_with("mcp__") => {
                format!("mcp__{}", alias(&name["mcp__".len()..]))
            }
            Name::Server(name) => name.split('/').map(alias).collect::<Vec<_>>().join("/"),
            Name::Own(name) => alias(name),
        }
    }
}

/// The spec's skeleton, its launch arguments read in order.
fn spec_shape(skeleton: &Skeleton, spec: &AgentSpec) -> Value {
    let mut shape = skeleton.proto("amux.v1.AgentSpec", &prost::Message::encode_to_vec(spec));
    if !spec.provider_args.is_empty() {
        shape["provider_args"] = skeleton.launch_args(&spec.provider_args);
    }
    shape
}

/// The skeleton of what an interpreter wrote to its provider, when it is
/// JSON: the ids it chose, by alias, for a reply that echoes them.
fn json_shape(alias: &Alias, kind: wire::Kind, bytes: &[u8]) -> Value {
    serde_json::from_slice::<Value>(bytes)
        .map(|_| alias.skeleton().fact(kind, bytes))
        .unwrap_or(Value::Null)
}

/// The folder's git facts as counts: whether there is a branch, never its
/// name, and files, lines added and lines removed.
fn git(git: &wire::Git) -> Value {
    let totals = |totals: &Option<wire::ChangeTotals>| {
        totals
            .as_ref()
            .map(|totals| (totals.files, totals.added, totals.removed))
    };
    json!({
        "branch": git.branch.is_some(),
        "base_branch": git.base_branch.is_some(),
        "uncommitted": totals(&git.uncommitted),
        "on_branch": totals(&git.on_branch),
    })
}

/// An agent message by its ids, its size and a report's ends.
fn message(alias: &Alias, envelope: &wire::Envelope) -> Value {
    let (report, attachments) = match &envelope.body {
        Some(wire::envelope::Body::Report(report)) => {
            let report = match &report.end {
                Some(wire::child_report::End::TurnEnded(turn)) => {
                    json!({ "outcome": turn.outcome, "last": turn.last })
                }
                Some(wire::child_report::End::Exited(exited)) => {
                    json!({ "exited": exited.ended.as_ref().map_or(0, |ended| ended.cause) })
                }
                None => json!({}),
            };
            (Some(report), 0)
        }
        Some(wire::envelope::Body::Message(message)) => (None, message.attachments.len()),
        _ => (None, 0),
    };
    // The sending agent; none for a person.
    let from = match envelope.from.as_ref().and_then(|from| from.value.as_ref()) {
        Some(wire::sender::Value::Agent(wire::AgentSender {
            agent: Some(agent), ..
        })) => alias.id(&agent.agent_id),
        _ => String::new(),
    };
    json!({
        "event": "agent_message",
        "shape": alias.skeleton().proto("amux.v1.Envelope", &prost::Message::encode_to_vec(envelope)),
        "id": alias.id(&envelope.id),
        "from": from,
        "report": report,
        "attachments": attachments,
        "bytes": prost::Message::encoded_len(envelope),
    })
}

/// An effect by kind, with its size and the id it answers.
fn effect_record(alias: &Alias, kind: wire::Kind, effect: &Effect) -> Value {
    match effect {
        Effect::ProviderWrite(bytes) => json!({
            "effect": "provider_write",
            "bytes": bytes.len(),
            "shape": json_shape(alias, kind, bytes),
        }),
        Effect::Reply { input_id, verdict } => {
            let (accepted, refusal) = match &verdict.of {
                Some(wire::send_input_response::Of::Accepted(accepted)) => {
                    (Some(accepted.queued), None)
                }
                Some(wire::send_input_response::Of::Rejected(rejected)) => {
                    (None, Some(rejected.refusal))
                }
                None => (None, None),
            };
            json!({
                "effect": "reply",
                "id": alias.id(input_id),
                // Accepted, and whether queued; or the refusal.
                "queued": accepted,
                "refusal": refusal,
            })
        }
        Effect::Inject {
            id,
            text,
            attachments,
            ..
        } => json!({
            "effect": "inject",
            "id": alias.id(id),
            "bytes": text.len(),
            "attachments": attachments.len(),
        }),
        Effect::Exit { .. } => json!({ "effect": "exit" }),
        Effect::WriteBlob { bytes, .. } => json!({ "effect": "write_blob", "bytes": bytes.len() }),
        Effect::Terminal(_) => json!({ "effect": "terminal" }),
        Effect::HookAnswer { id, answer } => {
            json!({ "effect": "hook_answer", "hook": id, "bytes": answer.len() })
        }
        Effect::UserMessage {
            uuid,
            text,
            attachments,
        } => json!({
            "effect": "user_message",
            "id": alias.id(uuid.as_bytes()),
            "bytes": text.len(),
            "attachments": attachments.len(),
        }),
        Effect::FollowTranscript { .. } => json!({ "effect": "follow_transcript" }),
        Effect::ProviderSession { id } => {
            json!({ "effect": "provider_session", "id": alias.id(id.as_bytes()) })
        }
        Effect::CodexTurnInput {
            request,
            attachments,
        } => json!({
            "effect": "codex_turn_input",
            "bytes": request.len(),
            "shape": json_shape(alias, kind, request),
            "attachments": attachments.len(),
        }),
    }
}
