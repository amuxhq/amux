//! A stream made up from an agent's record stands for the stream the
//! record was made from. Every fixture's record, turned into a stream and
//! recorded again, is the record it was, alias for alias, but for what the
//! interpreter derives from words the record does not keep ([`derived`]).
//! And a record a report carried reproduces the check its stream broke, at
//! the step that broke it, from the record alone.

use std::path::{Path, PathBuf};

use interpret::claude_pty::ClaudePty;
use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::Codex;
use interpret::ring::Entry;
use interpret::skeleton::Skeleton;
use interpret::{Channel, Event, Fact, Interpreter, checks};
use replay_support::synthetic::{self, Synthetic};
use serde_json::{Map, Value, json};
use wire::AgentSpec;

const RECORDED: [u8; 16] = [7; 16];
const AGAIN: [u8; 16] = [9; 16];

fn fixtures(kind: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../interpret/fixtures")
        .join(kind);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
}

fn ring(events: &[Event]) -> Vec<u8> {
    let mut out = Vec::new();
    for event in events {
        out.extend(serde_json::to_vec(&Entry::of(event)).unwrap());
        out.push(b'\n');
    }
    out
}

/// The record of `events` run from a fresh interpreter under `spec`.
fn record<I: Interpreter>(spec: &AgentSpec, salt: [u8; 16], events: &[Event]) -> Value {
    let (state, _) = I::initial(spec);
    interpret::record::record::<I>(
        spec,
        salt,
        Value::Null,
        [(interpret::encode_checkpoint(&state), ring(events))],
    )
}

/// `record` with every alias's hash replaced by the one its made-up value
/// has under the salt `AGAIN`, so it reads as the made-up stream's record
/// would if the stream were the one it stands for.
fn translated(record: &Value, synthetic: &Synthetic) -> Value {
    let again = Skeleton { salt: AGAIN };
    let text = |text: &str| -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(at) = rest.find(['-', ':', '@', '%']) {
            out.push_str(&rest[..=at]);
            rest = &rest[at + 1..];
            let hex = rest
                .bytes()
                .take_while(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
                .count();
            if hex == 12 {
                match synthetic.values.get(&rest[..12]) {
                    Some(value) => out.push_str(&again.hash(value)),
                    None => out.push_str(&format!("<unbound {}>", &rest[..12])),
                }
                rest = &rest[12..];
            }
        }
        out.push_str(rest);
        out
    };
    fn walk(value: &Value, text: &dyn Fn(&str) -> String) -> Value {
        match value {
            Value::String(value) => Value::String(text(value)),
            Value::Array(items) => {
                Value::Array(items.iter().map(|item| walk(item, text)).collect())
            }
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(name, value)| (text(name), walk(value, text)))
                    .collect::<Map<_, _>>(),
            ),
            other => other.clone(),
        }
    }
    walk(record, &text)
}

/// Where two records differ, by path.
fn differences(path: &str, left: &Value, right: &Value, out: &mut Vec<String>) {
    match (left, right) {
        (Value::Object(left), Value::Object(right)) => {
            for (name, value) in left {
                match right.get(name) {
                    Some(other) => differences(&format!("{path}.{name}"), value, other, out),
                    None => out.push(format!("{path}.{name}: only recorded: {value}")),
                }
            }
            for (name, value) in right {
                if !left.contains_key(name) {
                    out.push(format!("{path}.{name}: only made up: {value}"));
                }
            }
        }
        // The items one step writes at once come in their keys' order,
        // which aliasing changes.
        (Value::Array(left), Value::Array(right))
            if path.ends_with(".step.items") && left.len() == right.len() =>
        {
            let sorted = |list: &[Value]| {
                let mut list = list.to_vec();
                list.sort_by_key(|item| item["key"].to_string());
                list
            };
            for (index, (left, right)) in sorted(left).iter().zip(&sorted(right)).enumerate() {
                differences(&format!("{path}[{index}]"), left, right, out);
            }
        }
        // A tool's keys are listed in their own order, which aliasing
        // changes.
        (Value::Array(left), Value::Array(right)) if path.ends_with(".keys") => {
            let sorted = |list: &[Value]| {
                let mut list = list.to_vec();
                list.sort_by_key(Value::to_string);
                list
            };
            if sorted(left) != sorted(right) {
                out.push(format!("{path}: recorded {left:?} made up {right:?}"));
            }
        }
        (Value::Array(left), Value::Array(right)) if left.len() == right.len() => {
            for (index, (left, right)) in left.iter().zip(right).enumerate() {
                differences(&format!("{path}[{index}]"), left, right, out);
            }
        }
        (left, right) if left != right => {
            out.push(format!("{path}: recorded {left} made up {right}"));
        }
        _ => {}
    }
}

/// Whether a difference is in what the interpreter derives from words the
/// record does not keep, so a made-up stream cannot match it:
///
/// - the size of an item, a snapshot or a write the interpreter makes from
///   a text it reads into (a shell command it unwraps from the user's
///   shell, a diff whose lines it counts, a preview it clips at a word, a
///   model's display name, a plan's text);
/// - the shape of an answer the interpreter writes back, keyed by the
///   question as the provider asked it: a question id kept as a field name
///   there and aliased where it was asked, and an answer joined from
///   several choices.
///
/// Every event, every item's key, arm and op, every snapshot's phase,
/// queue and asks, every turn end, effect and check is still the record's;
/// the items one step writes at once are compared in any order, as the
/// interpreter writes them in their keys' order.
fn derived(difference: &str) -> bool {
    let size = difference.contains(".bytes:")
        && (difference.contains(".step.") || difference.contains(".effects["));
    let answer = difference.contains(".effects[") && difference.contains("].shape.");
    size || answer
}

/// The fixtures whose title comes out of the words of a provider's answer.
/// A record keeps no words, so its replay has no title to take.
const TITLED_FROM_WORDS: &[&str] = &["claude_sdk/session_title.json", "codex/thread_title.json"];

struct Outcome {
    fixtures: usize,
    exact: usize,
    /// Fixtures that differ only in what is derived from words.
    derived: Vec<String>,
    /// Every other difference, by fixture.
    failures: Vec<String>,
}

/// Every fixture of `kind`, recorded, made up and recorded again.
fn round_trip<I: Interpreter>(kind: &str) -> Outcome {
    let mut outcome = Outcome {
        fixtures: 0,
        exact: 0,
        derived: Vec::new(),
        failures: Vec::new(),
    };
    for fixture in fixtures(kind) {
        let name = format!("{kind}/{}", fixture.file_name().unwrap().to_string_lossy());
        let script = interpret::fixture_script::<I>(&fixture).unwrap();
        let events: Vec<Event> = script.events.into_iter().flat_map(|(_, e)| e).collect();
        let recorded = record::<I>(&script.spec, RECORDED, &events);
        let synthetic = synthetic::synthesize::<I>(&recorded);
        let again = record::<I>(&synthetic.spec, AGAIN, &synthetic.events);
        let mut found = Vec::new();
        differences("", &translated(&recorded, &synthetic), &again, &mut found);
        outcome.fixtures += 1;
        // Only a recorded fixture's frames are the provider's own whole;
        // an authored one's are as short as its test needs.
        if name.contains("/recorded_") {
            for note in &synthetic.notes {
                // A frame the provider's types refuse as recorded, they
                // refuse made up too.
                let recorded = note
                    .strip_prefix("event ")
                    .and_then(|note| note.split_once(':'))
                    .and_then(|(index, _)| events.get(index.parse::<usize>().ok()?));
                let drifted = matches!(recorded, Some(Event::Fact(fact))
                    if serde_json::from_slice(&fact.payload).ok().and_then(|frame| {
                        synthetic::drift(I::KIND, fact.channel, &frame)
                    }).is_some());
                if !drifted {
                    outcome.failures.push(format!("{name}: {note}"));
                }
            }
        }
        let other: Vec<&String> = found.iter().filter(|line| !derived(line)).collect();
        if TITLED_FROM_WORDS.contains(&name.as_str()) {
            outcome.derived.push(name);
        } else if found.is_empty() {
            outcome.exact += 1;
        } else if other.is_empty() {
            outcome.derived.push(name);
        } else {
            outcome.failures.push(format!(
                "{name}: {} differences: {:#?}",
                other.len(),
                &other[..other.len().min(10)]
            ));
        }
    }
    eprintln!(
        "{kind}: {} fixtures, {} exactly, {} but for derived sizes and answers: {:?}",
        outcome.fixtures,
        outcome.exact,
        outcome.derived.len(),
        outcome.derived
    );
    outcome
}

#[test]
fn every_headless_claude_fixture_survives_its_record() {
    let outcome = round_trip::<ClaudeSdk>("claude_sdk");
    assert!(outcome.failures.is_empty(), "{:#?}", outcome.failures);
}

#[test]
fn every_terminal_claude_fixture_survives_its_record() {
    let outcome = round_trip::<ClaudePty>("claude_pty");
    assert!(outcome.failures.is_empty(), "{:#?}", outcome.failures);
}

#[test]
fn every_codex_fixture_survives_its_record() {
    let outcome = round_trip::<Codex>("codex");
    assert!(outcome.failures.is_empty(), "{:#?}", outcome.failures);
}

fn spec(kind: wire::Kind) -> AgentSpec {
    AgentSpec {
        agent_id: b"agent".to_vec(),
        kind: kind as i32,
        incarnation: 1,
        created_at_ms: 1_000,
        provider_command: "/usr/local/bin/claude".into(),
        ..AgentSpec::default()
    }
}

fn fact(channel: Channel, value: Value) -> Event {
    Event::Fact(Fact {
        channel,
        payload: serde_json::to_vec(&value).unwrap(),
    })
}

fn stream(event: Value) -> Event {
    fact(
        Channel::Stream,
        json!({"type": "stream_event", "session_id": "s", "event": event}),
    )
}

/// The check each step of `events` broke, by step.
fn broken<I: Interpreter>(spec: &AgentSpec, events: &[Event]) -> Vec<Option<&'static str>> {
    let (mut state, _) = I::initial(spec);
    events
        .iter()
        .map(|event| {
            I::step(&mut state, event.clone())
                .broken
                .map(|broken| broken.invariant.id)
        })
        .collect()
}

/// The steps of a record's made-up stream that broke a check, by index.
fn reproduced(record: &Value) -> Vec<(usize, &'static str)> {
    synthetic::reproduce(record)
        .unwrap()
        .broken
        .iter()
        .map(|(index, broken)| (*index, broken.invariant.id))
        .collect()
}

#[test]
fn a_tool_result_for_a_call_never_seen_reproduces_from_its_record() {
    let events = vec![
        fact(
            Channel::Stream,
            json!({"type": "system", "subtype": "init", "session_id": "s",
                "claude_code_version": "2.1.312", "model": "claude-opus-5-5"}),
        ),
        fact(
            Channel::Stream,
            json!({"type": "user", "session_id": "s", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_never_called", "content": "secret"}
            ]}}),
        ),
    ];
    let spec = spec(wire::Kind::ClaudeSdk);
    assert_eq!(
        broken::<ClaudeSdk>(&spec, &events),
        [None, Some(checks::TOOL_RESULT_WITHOUT_CALL.id)]
    );
    let record = record::<ClaudeSdk>(&spec, RECORDED, &events);
    assert!(!record.to_string().contains("secret"));
    assert_eq!(
        reproduced(&record),
        [(1, checks::TOOL_RESULT_WITHOUT_CALL.id)]
    );
}

#[test]
fn an_item_opened_again_reproduces_from_its_record() {
    let item = |method: &str, text: &str| {
        fact(
            Channel::Rpc,
            json!({"method": method, "params": {"threadId": "t", "turnId": "u", "item":
                {"type": "agentMessage", "id": "msg_1", "text": text}}}),
        )
    };
    let events = vec![
        item("item/started", ""),
        item("item/completed", "done"),
        item("item/started", ""),
    ];
    let spec = spec(wire::Kind::Codex);
    let record = record::<Codex>(&spec, RECORDED, &events);
    assert_eq!(reproduced(&record), [(2, checks::ITEM_BEFORE_OPEN.id)]);
}

/// A record that left out its oldest events to fit a report still
/// reproduces the check its newest broke, from its first kept event, and
/// says what it left out.
#[test]
fn a_record_cut_to_fit_still_reproduces_its_check() {
    let item = |method: &str, id: &str| {
        fact(
            Channel::Rpc,
            json!({"method": method, "params": {"threadId": "t", "turnId": "u", "item":
                {"type": "agentMessage", "id": id, "text": ""}}}),
        )
    };
    let mut events: Vec<Event> = (0..200)
        .flat_map(|n| {
            let id = format!("msg_{n}");
            [item("item/started", &id), item("item/completed", &id)]
        })
        .collect();
    events.extend([
        item("item/started", "last"),
        item("item/completed", "last"),
        item("item/started", "last"),
    ]);
    let spec = spec(wire::Kind::Codex);
    let (state, _) = Codex::initial(&spec);
    let whole = record::<Codex>(&spec, RECORDED, &events);
    let size = |record: &Value| serde_json::to_vec(record).unwrap().len();
    let limit = size(&whole) / 10;
    let record = interpret::record::within::<Codex>(
        &spec,
        RECORDED,
        Value::Null,
        [(interpret::encode_checkpoint(&state), ring(&events))],
        |record| size(record) <= limit,
    );
    let left_out = record["events_left_out"].as_u64().unwrap() as usize;
    assert!(left_out > 0 && size(&record) <= limit);
    let reproduction = synthetic::reproduce(&record).unwrap();
    let kept = events.len() - left_out;
    assert_eq!(reproduction.synthetic.events.len(), kept);
    assert_eq!(
        reproduction
            .broken
            .iter()
            .map(|(index, broken)| (*index, broken.invariant.id))
            .collect::<Vec<_>>(),
        [(kept - 1, checks::ITEM_BEFORE_OPEN.id)]
    );
    assert!(
        reproduction.synthetic.notes[0].contains(&format!("left out its {left_out} oldest events")),
        "{:?}",
        reproduction.synthetic.notes
    );
}

/// Headless Claude dying halfway through a reply, as a report's record
/// carries it: the record the interpreter wrote before it closed what was
/// streaming on the provider's exit.
const DIED_MID_MESSAGE: &str = include_str!("fixtures/claude_sdk_died_mid_message.json");

/// The stream the record above was made from.
fn died_mid_message() -> Vec<Event> {
    vec![
        fact(
            Channel::Stream,
            json!({"type": "system", "subtype": "init", "session_id": "s",
                "claude_code_version": "2.1.312", "model": "claude-opus-5-5",
                "permissionMode": "default", "cwd": "/home/someone/project"}),
        ),
        stream(json!({"type": "message_start", "message":
            {"id": "msg_1", "role": "assistant", "content": [], "usage": {"input_tokens": 12}}})),
        stream(json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}})),
        stream(json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "Half of a secret reply"}})),
        Event::ProviderExit { code: Some(1) },
    ]
}

#[test]
fn a_provider_dying_mid_message_runs_clean_from_the_record_that_broke() {
    let live = broken::<ClaudeSdk>(&spec(wire::Kind::ClaudeSdk), &died_mid_message());
    assert!(live.iter().all(Option::is_none), "{live:?}");
    let record: Value = serde_json::from_str(DIED_MID_MESSAGE).unwrap();
    assert!(!DIED_MID_MESSAGE.contains("secret"));
    assert!(!DIED_MID_MESSAGE.contains("someone"));
    // The record's last step broke the exit check, and the stream made up
    // from the record alone dies at the same step halfway through the
    // same reply; this interpreter closes the reply there.
    let events = &record["segments"][0]["events"];
    assert_eq!(events[4]["broken"], checks::EXIT_LEFT_OPEN.id);
    let reproduction = synthetic::reproduce(&record).unwrap();
    assert!(reproduction.broken.is_empty(), "{:?}", reproduction.broken);
    assert!(matches!(
        reproduction.synthetic.events[4],
        Event::ProviderExit { code: Some(1) }
    ));
    let streamed = &reproduction.steps[2].step.items;
    assert_eq!(streamed.len(), 1, "{streamed:?}");
    assert!(
        reproduction.steps[4].step.items.iter().any(
            |item| item.key == streamed[0].key && ClaudeSdk::describe_item(&item.body).complete
        ),
        "{:?}",
        reproduction.steps[4].step.items
    );
}
