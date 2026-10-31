//! Dump redaction per kind. Each kind runs a short session with secrets
//! planted in a tool's input and output, the prompts, an answer, an
//! environment-bearing payload and the session id. Every item body,
//! snapshot body, facts-ring entry (fact or input) and the checkpoint it
//! produces is redacted; afterwards no planted secret is present, the
//! target still decodes as what it was, and redacting again changes
//! nothing. Run with `--nocapture` to read the transcript: every target
//! that carried a planted secret, what was planted in it, and what it reads
//! as after redaction.

use std::collections::BTreeSet;

use interpret::claude_pty::ClaudePty;
use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::Codex;
use interpret::{
    Channel, Checkpoint, Event, Fact, FixtureInput, Interpreter, RedactTarget, decode_checkpoint,
    encode_checkpoint, from_hex, redact,
};
use prost::Message as _;
use serde_json::{Value, json};
use wire::{AgentSpec, Input};

const SESSION: &str = "5e55a1d0-7a11-4b0c-9d1e-2f3a4b5c6d7e";
const ANTHROPIC_KEY: &str = "sk-ant-api03-PLANTEDanthropic0001";
const GITHUB_TOKEN: &str = "ghp_PLANTEDgithub0002abcdef";
const BEARER: &str = "PLANTEDbearer0003";
const ENV_VALUE: &str = "PLANTEDenv0004";
const ASSIGNED: &str = "PLANTEDassign0005";
const JSON_KEY: &str = "PLANTEDjsonkey0006";
const FORM_SECRET: &str = "PLANTEDform0007";
const EMAIL: &str = "planted.person@example.com";

const PLANTED: &[(&str, &str)] = &[
    ("session id", SESSION),
    ("anthropic key", ANTHROPIC_KEY),
    ("github token", GITHUB_TOKEN),
    ("bearer token", BEARER),
    ("env map value", ENV_VALUE),
    ("env assignment", ASSIGNED),
    ("api_key field", JSON_KEY),
    ("form answer", FORM_SECRET),
    ("email", EMAIL),
];

fn command() -> String {
    format!(
        "curl -H 'Authorization: Bearer {BEARER}' https://api.example.com/v1 && DB_PASSWORD={ASSIGNED} ./migrate"
    )
}

fn output() -> String {
    format!(
        "migrated\nusing {ANTHROPIC_KEY}\n{{\"api_key\": \"{JSON_KEY}\", \"rows\": 3}}\nnotified {EMAIL}\n"
    )
}

fn prompt() -> String {
    format!("Deploy with {GITHUB_TOKEN} and tell {EMAIL} when it is done")
}

fn env() -> Value {
    json!({ "AWS_SECRET_ACCESS_KEY": ENV_VALUE, "HOME": "/Users/planted", "LANG": "en_US.UTF-8" })
}

fn hook(value: Value) -> Event {
    fact(Channel::Hook, value)
}

fn fact(channel: Channel, value: Value) -> Event {
    Event::Fact(Fact {
        channel,
        payload: serde_json::to_vec(&value).unwrap(),
    })
}

fn prompt_input<I: Interpreter>(id: &str) -> Event {
    let input = FixtureInput::Prompt { text: prompt() };
    Event::Input(I::fixture_input(id.as_bytes().to_vec(), &input).expect("a prompt input"))
}

fn answer_input<I: Interpreter>() -> Input {
    let answer = FixtureInput::Answer {
        ask: "ask-1".into(),
        answer: json!({ "form": { "action": "accept", "content": { "password": FORM_SECRET, "note": prompt() } } }),
    };
    I::fixture_input(b"answer-1".to_vec(), &answer)
        .or_else(|| {
            let answer = FixtureInput::Answer {
                ask: "ask-1".into(),
                answer: json!({ "selected": [format!("the password is {FORM_SECRET}")] }),
            };
            I::fixture_input(b"answer-1".to_vec(), &answer)
        })
        .expect("an answer input")
}

fn claude_pty_session() -> Vec<Event> {
    let base = json!({ "session_id": SESSION, "transcript_path": format!("/Users/planted/.claude/projects/p/{SESSION}.jsonl"), "cwd": "/Users/planted/work" });
    let with = |extra: Value| {
        let mut value = base.clone();
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        hook(value)
    };
    vec![
        with(
            json!({ "hook_event_name": "SessionStart", "source": "startup", "model": "claude-opus-5-5", "env": env() }),
        ),
        prompt_input::<ClaudePty>("p1"),
        with(json!({ "hook_event_name": "UserPromptSubmit", "prompt": prompt() })),
        fact(
            Channel::Transcript,
            json!({ "type": "user", "uuid": "u1", "timestamp": "2027-01-15T08:00:00.010Z", "sessionId": SESSION, "origin": { "kind": "human" }, "promptSource": "typed", "message": { "role": "user", "content": prompt() } }),
        ),
        with(
            json!({ "hook_event_name": "PreToolUse", "tool_use_id": "t1", "tool_name": "Bash", "tool_input": { "command": command() } }),
        ),
        with(
            json!({ "hook_event_name": "PostToolUse", "tool_use_id": "t1", "tool_name": "Bash", "tool_input": { "command": command() }, "tool_response": { "stdout": output(), "stderr": "", "interrupted": false } }),
        ),
        with(
            json!({ "hook_event_name": "PreToolUse", "tool_use_id": "t2", "tool_name": "Bash", "tool_input": { "command": command() } }),
        ),
        fact(
            Channel::Transcript,
            json!({ "type": "assistant", "uuid": "a2", "timestamp": "2027-01-15T08:00:01.000Z", "sessionId": SESSION, "message": { "id": "m2", "role": "assistant", "model": "claude-opus-5-5", "content": [{ "type": "tool_use", "id": "t2", "name": "Bash", "input": { "command": command() } }] } }),
        ),
        with(
            json!({ "hook_event_name": "Elicitation", "amux_reply": 1, "mcp_server_name": "tracker", "mode": "form", "message": output(), "requested_schema": { "type": "object", "properties": { "repo": { "type": "string", "default": command() } } } }),
        ),
        prompt_input::<ClaudePty>("p2"),
    ]
}

fn claude_sdk_session() -> Vec<Event> {
    let stream = |value: Value| fact(Channel::Stream, value);
    vec![
        stream(
            json!({ "type": "system", "subtype": "init", "uuid": "init-1", "session_id": SESSION, "cwd": "/Users/planted/work", "claude_code_version": "2.1.283", "model": "claude-opus-5-5", "permissionMode": "default" }),
        ),
        stream(
            json!({ "type": "control_request", "request_id": "hook-1", "request": { "subtype": "hook_callback", "callback_id": "cb-1", "input": { "hook_event_name": "SessionStart", "session_id": SESSION, "env": env() } } }),
        ),
        prompt_input::<ClaudeSdk>("p1"),
        stream(
            json!({ "type": "assistant", "uuid": "m1", "session_id": SESSION, "message": { "id": "m1", "role": "assistant", "model": "claude-opus-5-5", "content": [{ "type": "tool_use", "id": "t1", "name": "Bash", "input": { "command": command() } }] } }),
        ),
        stream(
            json!({ "type": "user", "uuid": "u1", "session_id": SESSION, "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": output() }] } }),
        ),
        stream(
            json!({ "type": "assistant", "uuid": "m2", "session_id": SESSION, "message": { "id": "m2", "role": "assistant", "model": "claude-opus-5-5", "content": [{ "type": "tool_use", "id": "t2", "name": "Bash", "input": { "command": command() } }] } }),
        ),
        prompt_input::<ClaudeSdk>("p2"),
    ]
}

fn codex_session() -> Vec<Event> {
    let rpc = |value: Value| fact(Channel::Rpc, value);
    let item = |status: &str, extra: Value| {
        let mut item = json!({ "type": "commandExecution", "id": "cmd-1", "command": command(), "cwd": "/Users/planted/work", "status": status, "env": env() });
        item.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        item
    };
    vec![
        rpc(
            json!({ "id": 2, "result": { "thread": { "id": SESSION, "cliVersion": "0.157.0", "path": format!("/Users/planted/.codex/sessions/rollout-{SESSION}.jsonl"), "turns": [] }, "model": "gpt-5.6-luna", "approvalPolicy": "on-request", "sandbox": { "type": "workspaceWrite" } } }),
        ),
        prompt_input::<Codex>("p1"),
        rpc(
            json!({ "method": "turn/started", "params": { "threadId": SESSION, "turn": { "id": "turn-1", "status": "inProgress", "items": [] } } }),
        ),
        rpc(
            json!({ "method": "item/started", "params": { "threadId": SESSION, "turnId": "turn-1", "item": item("inProgress", json!({ "exitCode": null })), "startedAtMs": 1_800_000_001_000_i64 } }),
        ),
        rpc(
            json!({ "method": "item/completed", "params": { "threadId": SESSION, "turnId": "turn-1", "item": item("completed", json!({ "exitCode": 0, "aggregatedOutput": output(), "durationMs": 900 })), "completedAtMs": 1_800_000_002_000_i64 } }),
        ),
        prompt_input::<Codex>("p2"),
    ]
}

/// The planted secrets present in `bytes`, looking inside the hex a
/// checkpoint writes its protobuf values as.
fn planted_in(bytes: &[u8]) -> BTreeSet<&'static str> {
    let mut found = PLANTED
        .iter()
        .filter(|(_, secret)| contains(bytes, secret.as_bytes()))
        .map(|(name, _)| *name)
        .collect::<BTreeSet<_>>();
    if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
        hex_strings(&value, &mut |decoded| found.extend(planted_in(decoded)));
    }
    found
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn hex_strings(value: &Value, found: &mut impl FnMut(&[u8])) {
    match value {
        Value::String(text) => {
            if let Ok(bytes) = from_hex(text) {
                found(&bytes);
            }
        }
        Value::Array(values) => values.iter().for_each(|value| hex_strings(value, found)),
        Value::Object(map) => map.values().for_each(|value| hex_strings(value, found)),
        _ => {}
    }
}

/// Every path through a JSON value, so a redacted fact can be compared
/// with the original by shape.
fn shape(value: &Value, path: String, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                shape(value, format!("{path}.{key}"), out);
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                shape(value, format!("{path}[{index}]"), out);
            }
        }
        _ => {
            out.insert(path);
        }
    }
}

fn json_shape(bytes: &[u8]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    shape(
        &serde_json::from_slice(bytes).expect("JSON"),
        String::new(),
        &mut out,
    );
    out
}

fn target_bytes(target: &RedactTarget) -> &[u8] {
    match target {
        RedactTarget::Input(bytes)
        | RedactTarget::AgentMessage(bytes)
        | RedactTarget::Checkpoint(bytes)
        | RedactTarget::Spec(bytes)
        | RedactTarget::Step(bytes)
        | RedactTarget::Agent(bytes)
        | RedactTarget::ClientSession(bytes) => bytes,
        RedactTarget::Fact(fact) => &fact.payload,
    }
}

/// Encoded protobuf as its readable runs of text.
fn readable(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .map(|c| if c.is_control() { '·' } else { c })
        .collect()
}

struct Transcript {
    text: String,
    planted: BTreeSet<&'static str>,
    /// Entries already written; a snapshot repeated unchanged is written
    /// once.
    written: BTreeSet<String>,
}

impl Transcript {
    /// Redacts one target, checks it, and records it when something was
    /// planted in it. `same` compares before and after by structure and
    /// says what it compared.
    fn check<I: Interpreter>(
        &mut self,
        label: &str,
        before: RedactTarget,
        same: impl Fn(&[u8], &[u8]) -> Result<String, String>,
        show: impl Fn(&[u8]) -> String,
    ) {
        let kind = I::KIND;
        let after = redact(kind, before.clone());
        let again = redact(kind, after.clone());
        let (before_bytes, after_bytes) = (target_bytes(&before), target_bytes(&after));
        let planted = planted_in(before_bytes);
        let left = planted_in(after_bytes);
        assert!(
            left.is_empty(),
            "{:?} {label}: {left:?} survived redaction:\n{}",
            I::KIND,
            show(after_bytes)
        );
        let kept = same(before_bytes, after_bytes)
            .unwrap_or_else(|why| panic!("{:?} {label}: structure lost: {why}", I::KIND));
        assert_eq!(
            again,
            after,
            "{:?} {label}: redaction is not idempotent",
            I::KIND
        );
        if planted.is_empty() {
            return;
        }
        self.planted.extend(planted.iter().copied());
        let names = planted.iter().copied().collect::<Vec<_>>().join(", ");
        let entry = format!(
            "{label}\n  planted: {names}\n  after:   none present; {kept}\n  reads:   {}\n",
            show(after_bytes)
        );
        if self.written.insert(entry.clone()) {
            self.text.push_str(&entry);
        }
    }
}

fn same_json(before: &[u8], after: &[u8]) -> Result<String, String> {
    let (before, after) = (json_shape(before), json_shape(after));
    if before == after {
        Ok(format!("same JSON shape ({} leaves)", after.len()))
    } else {
        Err(format!("JSON shape {before:?} became {after:?}"))
    }
}

fn run<I: Interpreter>(events: Vec<Event>) -> (String, BTreeSet<&'static str>) {
    let spec = AgentSpec {
        agent_id: vec![7; 16],
        kind: I::KIND as i32,
        created_at_ms: 1_800_000_000_000,
        ..AgentSpec::default()
    };
    let (mut state, first) = I::initial(&spec);
    let mut steps = vec![first];
    let mut inputs = vec![answer_input::<I>()];
    // An agent message is a ring entry of its own, with a person's words.
    let mut messages = vec![wire::Envelope {
        id: b"message-1".to_vec(),
        body: Some(wire::envelope::Body::Message(wire::MessageBody {
            text: prompt(),
            ..Default::default()
        })),
        ..Default::default()
    }];
    let mut facts = Vec::new();
    for (index, event) in events.into_iter().enumerate() {
        match &event {
            Event::Fact(fact) => facts.push(fact.clone()),
            Event::Input(input) => inputs.push(input.clone()),
            Event::AgentMessage(envelope) => messages.push(envelope.clone()),
            _ => {}
        }
        let tick = I::step(
            &mut state,
            Event::Tick {
                at_ms: 1_800_000_000_000 + index as i64 * 1000,
            },
        );
        steps.push(tick.step);
        steps.push(I::step(&mut state, event).step);
    }

    let mut transcript = Transcript {
        text: String::new(),
        planted: BTreeSet::new(),
        written: BTreeSet::new(),
    };
    for fact in facts {
        let value: Value = serde_json::from_slice(&fact.payload).unwrap();
        let what = ["hook_event_name", "method", "type"]
            .iter()
            .find_map(|key| value.get(key).and_then(Value::as_str))
            .unwrap_or("response")
            .to_owned();
        transcript.check::<I>(
            &format!("fact {:?} {what}", fact.channel),
            RedactTarget::Fact(fact),
            same_json,
            |bytes| String::from_utf8_lossy(bytes).into_owned(),
        );
    }
    for input in inputs {
        transcript.check::<I>(
            &format!("input {}", String::from_utf8_lossy(&input.input_id)),
            RedactTarget::Input(input.encode_to_vec()),
            |before, after| {
                let (before, after) = (Input::decode(before).unwrap(), Input::decode(after));
                let after = after.map_err(|error| error.to_string())?;
                // The kind and the arm inside its body, as Debug names them.
                let arm = |input: &Input| {
                    let body = format!("{:?}", input.decode_body()?);
                    let mut words = body.split(['(', ' ']);
                    let kind = words.next()?.to_owned();
                    let arm = words.skip_while(|word| *word != "Some").nth(1)?.to_owned();
                    Some((kind, arm))
                };
                if arm(&before).is_some()
                    && arm(&before) == arm(&after)
                    && before.input_id == after.input_id
                {
                    Ok(format!(
                        "decodes as an Input on the same arm ({})",
                        arm(&after).map(|(kind, _)| kind).unwrap_or_default()
                    ))
                } else {
                    Err(format!("{before:?} became {after:?}"))
                }
            },
            readable,
        );
    }
    for message in messages {
        transcript.check::<I>(
            &format!("agent message {}", String::from_utf8_lossy(&message.id)),
            RedactTarget::AgentMessage(message.encode_to_vec()),
            |before, after| {
                let before = wire::Envelope::decode(before).unwrap();
                let after = wire::Envelope::decode(after).map_err(|error| error.to_string())?;
                if before.id == after.id {
                    Ok("decodes as an envelope with the same id".to_owned())
                } else {
                    Err(format!("{before:?} became {after:?}"))
                }
            },
            readable,
        );
    }
    // The daemon's side of a dump: each journal frame, and the store's
    // rows for the agent as one step.
    let slice = wire::Step {
        items: steps.iter().flat_map(|step| step.items.clone()).collect(),
        snapshot: steps.iter().rev().find_map(|step| step.snapshot.clone()),
        ..wire::Step::default()
    };
    for (what, step) in steps
        .iter()
        .enumerate()
        .map(|(index, step)| (format!("journal frame {index}"), step))
        .chain([("store slice".to_owned(), &slice)])
    {
        transcript.check::<I>(
            &what,
            RedactTarget::Step(step.encode_to_vec()),
            |before, after| {
                let before = wire::Step::decode(before).unwrap();
                let after = wire::Step::decode(after).map_err(|error| error.to_string())?;
                // Each body decodes as it did: an item on the same arm and
                // as complete, the snapshot with the same open asks.
                let arms = |step: &wire::Step| {
                    step.items
                        .iter()
                        .map(|item| {
                            let view = I::describe_item(&item.body);
                            (view.arm, view.complete)
                        })
                        .collect::<Vec<_>>()
                };
                let asks = |step: &wire::Step| {
                    step.snapshot
                        .as_ref()
                        .map(|snapshot| I::describe_snapshot(&snapshot.body).asks)
                };
                if arms(&before) == arms(&after)
                    && asks(&before) == asks(&after)
                    && before.turn_end.is_some() == after.turn_end.is_some()
                {
                    Ok(format!(
                        "decodes as a step with the same {} items",
                        after.items.len()
                    ))
                } else {
                    Err(format!("{before:?} became {after:?}"))
                }
            },
            readable,
        );
    }
    let row = wire::Agent {
        agent_id: vec![7; 16],
        name: format!("deploy with {GITHUB_TOKEN}"),
        cwd: "/Users/planted/work".to_owned(),
        working_on: Some(format!("mailing {EMAIL}")),
        exit_cause: wire::ExitCause::CouldNotStart as i32,
        exit_detail: Some(format!("export AWS_SECRET_ACCESS_KEY={ASSIGNED}")),
        kind: wire::Kind::ClaudeSdk as i32,
        ..wire::Agent::default()
    };
    transcript.check::<I>(
        "inventory row",
        RedactTarget::Agent(row.encode_to_vec()),
        |before, after| {
            let before = wire::Agent::decode(before).unwrap();
            let after = wire::Agent::decode(after).map_err(|error| error.to_string())?;
            if before.agent_id == after.agent_id && before.kind == after.kind {
                Ok("decodes as the same agent".to_owned())
            } else {
                Err(format!("{before:?} became {after:?}"))
            }
        },
        readable,
    );
    // A client's session as a dump carries it: the same rows, held in its
    // window and arriving as messages since.
    let history = wire::dump::ClientSessionHistory {
        agent_id: row.agent_id.clone(),
        start: Some(wire::dump::ClientSession {
            agent: Some(row.clone()),
            snapshot: slice.snapshot.clone(),
            transcript: Some(wire::dump::ClientTranscript {
                items: slice.items.clone(),
                exhausted: true,
            }),
            ..wire::dump::ClientSession::default()
        }),
        events: Some(wire::dump::ClientSessionEvents {
            traced: slice
                .items
                .iter()
                .enumerate()
                .map(|(seq, item)| wire::dump::ClientTraced {
                    seq: seq as u64,
                    at_ms: 0,
                    event: Some(wire::dump::client_traced::Event::Msg(
                        wire::dump::ClientSessionMsg {
                            of: Some(wire::dump::client_session_msg::Of::Event(
                                wire::SessionEvent {
                                    of: Some(wire::session_event::Of::Item(item.clone())),
                                },
                            )),
                        },
                    )),
                })
                .collect(),
        }),
    };
    transcript.check::<I>(
        "client session",
        RedactTarget::ClientSession(history.encode_to_vec()),
        |before, after| {
            let before = wire::dump::ClientSessionHistory::decode(before).unwrap();
            let after = wire::dump::ClientSessionHistory::decode(after)
                .map_err(|error| error.to_string())?;
            let arms = |history: &wire::dump::ClientSessionHistory| {
                let held = history
                    .start
                    .iter()
                    .flat_map(|start| start.transcript.iter())
                    .flat_map(|transcript| transcript.items.iter());
                let arrived = history
                    .events
                    .iter()
                    .flat_map(|events| events.traced.iter())
                    .filter_map(|traced| match &traced.event {
                        Some(wire::dump::client_traced::Event::Msg(
                            wire::dump::ClientSessionMsg {
                                of:
                                    Some(wire::dump::client_session_msg::Of::Event(
                                        wire::SessionEvent {
                                            of: Some(wire::session_event::Of::Item(item)),
                                        },
                                    )),
                            },
                        )) => Some(item),
                        _ => None,
                    });
                held.chain(arrived)
                    .map(|item| (item.key.clone(), I::describe_item(&item.body).arm))
                    .collect::<Vec<_>>()
            };
            if arms(&before) == arms(&after) && before.agent_id == after.agent_id {
                Ok(format!(
                    "decodes as the same session with the same {} rows",
                    arms(&after).len()
                ))
            } else {
                Err(format!("{before:?} became {after:?}"))
            }
        },
        readable,
    );
    let launched = AgentSpec {
        provider_args: vec![
            "--settings".into(),
            format!("{{\"env\": {{\"ANTHROPIC_API_KEY\": \"{ANTHROPIC_KEY}\"}}}}"),
        ],
        provider_env: [("GITHUB_TOKEN", GITHUB_TOKEN), ("LANG", "en_US.UTF-8")]
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect(),
        bootstrap: Some(wire::Bootstrap {
            of: Some(wire::bootstrap::Of::Prompt(wire::PromptSubmission {
                input_id: b"initial".to_vec(),
                prompt: Some(wire::PromptInput {
                    text: prompt(),
                    ..Default::default()
                }),
            })),
        }),
        ..spec
    };
    transcript.check::<I>(
        "spec",
        RedactTarget::Spec(launched.encode_to_vec()),
        |before, after| {
            let (before, after) = (AgentSpec::decode(before).unwrap(), AgentSpec::decode(after));
            let after = after.map_err(|error| error.to_string())?;
            let names =
                |spec: &AgentSpec| spec.provider_env.keys().cloned().collect::<BTreeSet<_>>();
            if names(&before) == names(&after)
                && before.bootstrap.is_some() == after.bootstrap.is_some()
            {
                Ok("decodes as a spec with the same environment names".to_owned())
            } else {
                Err(format!("{before:?} became {after:?}"))
            }
        },
        |bytes| {
            let spec = AgentSpec::decode(bytes).unwrap();
            let env = spec
                .provider_env
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>();
            format!(
                "args={:?} env={env:?} prompt={:?}",
                spec.provider_args,
                spec.bootstrap
                    .map(|bootstrap| readable(&bootstrap.encode_to_vec())),
            )
        },
    );
    let checkpoint = encode_checkpoint(&state);
    transcript.check::<I>(
        "checkpoint",
        RedactTarget::Checkpoint(checkpoint),
        |before, after| {
            let before = decode_checkpoint::<I::State>(before).unwrap();
            let after = decode_checkpoint::<I::State>(after).map_err(|error| error.to_string())?;
            let keys = |step: &wire::Step| {
                step.items
                    .iter()
                    .map(|item| item.key.clone())
                    .collect::<Vec<_>>()
            };
            let (before, after) = (before.resume().1, after.resume().1);
            if keys(&before) == keys(&after)
                && before.snapshot.is_some() == after.snapshot.is_some()
            {
                Ok(format!(
                    "decodes as the state and resumes with the same {} open items",
                    after.items.len()
                ))
            } else {
                Err(format!(
                    "resume {:?} became {:?}",
                    keys(&before),
                    keys(&after)
                ))
            }
        },
        |bytes| String::from_utf8_lossy(bytes).into_owned(),
    );
    (transcript.text, transcript.planted)
}

fn report<I: Interpreter>(events: Vec<Event>) {
    let (text, planted) = run::<I>(events);
    println!("== {:?}\n{text}", I::KIND);
    let missing = PLANTED
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !planted.contains(name))
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "{:?}: the session never carried {missing:?}, so their redaction went untested",
        I::KIND
    );
}

#[test]
fn claude_pty_redaction() {
    report::<ClaudePty>(claude_pty_session());
}

#[test]
fn claude_sdk_redaction() {
    report::<ClaudeSdk>(claude_sdk_session());
}

#[test]
fn codex_redaction() {
    report::<Codex>(codex_session());
}

/// A kind the redactor does not know cannot be decoded, so nothing of it
/// leaves.
#[test]
fn unknown_kind_redacts_to_nothing() {
    let payload = format!("{{\"secret\": \"{ANTHROPIC_KEY}\"}}").into_bytes();
    let after = redact(
        wire::Kind::Unspecified,
        RedactTarget::Fact(Fact {
            channel: Channel::Hook,
            payload,
        }),
    );
    assert_eq!(
        after,
        RedactTarget::Fact(Fact {
            channel: Channel::Hook,
            payload: Vec::new(),
        })
    );
}
