//! The golden harness: recorded or authored facts in, an emission golden,
//! explicit outcomes and the interpreter invariants out.
//!
//! A fixture is a JSON file:
//!
//! ```json
//! {
//!   "about": "what this fixture shows",
//!   "spec": { "agent_id": "a1", "created_at_ms": 1000, "initial_prompt": "fix it",
//!             "provider_args": ["--effort", "low"] },
//!   "recording": { "path": "corpus.jsonl", "format": "transcript" },
//!   "events": [
//!     { "tick": 1500 },
//!     { "fact": { "channel": "stream", "json": { "type": "ready" } } },
//!     { "input": { "id": "p1", "prompt": { "text": "hello" } },
//!       "expect": { "reply": "accepted", "phase": "WORKING" } },
//!     { "agent_message": { "envelope": "e1", "from": "reviewer", "text": "look" } },
//!     { "rename": { "name": "loud-heron" } },
//!     { "git": { "branch": "fix", "base_branch": "main",
//!                "uncommitted": { "files": 1, "added": 2, "removed": 0 } } },
//!     { "checkpoint": {} }
//!   ],
//!   "end": { "queue_may_remain": false }
//! }
//! ```
//!
//! Recording events, when present, run before the authored ones; authored
//! events that must come first (what the agent process knew before the
//! provider wrote anything, inputs already accepted) go in `"prelude"`, the
//! same shape as `"events"`. The golden is the rendered emission beside the
//! fixture, `<name>.golden`; it is only written when
//! `INTERPRET_UPDATE_GOLDENS=1`, never by an ordinary run. A recorded event
//! that emitted nothing is left out of the golden; its frame number shows
//! the gap.
//!
//! Besides the golden and the expectations, every run checks the invariants
//! ([`check_invariants`]) and the checkpoint property: resuming from a
//! checkpoint taken after any prefix of the events and running the rest
//! yields the same items and final snapshot as the uninterrupted run, and a
//! resume re-emits only known keys at their current content.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use prost::Message as _;
use serde::Deserialize;
use serde_json::Value;
use wire::{
    AgentSpec, ClaudeAnswer, CodexAnswer, Envelope, EnvelopeKind, Input, Item, Phase, PromptInput,
    SendInputResponse, SendQueuedNow, Sender, Snapshot, Step, StopMode, WithdrawQueued,
    claude_answer, claude_pty_input, claude_sdk_input, codex_input, send_input_response, sender,
};

use crate::serde_pb::{from_hex, to_hex};
use crate::{
    Channel, Checkpoint, Effect, Event, Fact, Interpreter, decode_checkpoint, encode_checkpoint,
};

/// The label prefix of an event read from a recording.
const RECORDED: &str = "recorded ";

/// Set to 1 to write goldens instead of comparing against them.
pub const UPDATE_GOLDENS_ENV: &str = "INTERPRET_UPDATE_GOLDENS";

/// What a fixture allows to be left over when its events run out.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndRules {
    /// The fixture ends with prompts still queued on purpose.
    #[serde(default)]
    pub queue_may_remain: bool,
    /// The fixture ends mid-stream on purpose (a recording cut short).
    #[serde(default)]
    pub streams_may_remain: bool,
}

/// An authored input, in the vocabulary fixtures write. Each kind maps it
/// to its own input arm in [`Interpreter::fixture_input`].
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum FixtureInput {
    Prompt {
        text: String,
    },
    Withdraw {
        target: String,
    },
    /// Send a queued prompt into the running turn.
    SendNow {
        target: String,
    },
    Interrupt {},
    Clear {},
    /// `answer` is the kind's answer in JSON: for Claude `{"allow": {}}`,
    /// `{"allow": {"scope": 1}}`, `{"deny": {"note": "…", "stop": true}}`,
    /// `{"approve_plan": {}}`, `{"send_back": {"note": "…"}}`,
    /// `{"selected": [0, [1, 2], "typed", null], "notes": ["…", null]}`
    /// (one entry per question: an index, indices, a typed answer, or null
    /// to skip; a lone `"note"` goes on the last question), `{"reply":
    /// {"text": "…", "selected": [...]}}` to reply instead,
    /// `{"form": {"action": "accept", "content": {…}}}`,
    /// `{"link": {"action": "decline"}}`; for Codex
    /// `{"decision": "approve"}` answers an approval, and so does a
    /// question, form, link, `{"plan": {"choice": "start"}}` or
    /// `{"plan": {"choice": "keep_planning", "note": "…"}}`, or
    /// `{"grant": {"read": […], "write": […], "network": true,
    /// "for_session": true}}`.
    Answer {
        ask: String,
        #[serde(default)]
        answer: Value,
    },
    /// Terminal Claude's permission-mode cycle.
    CyclePermissionMode {},
    /// Change the model; null restores the launch model.
    Model {
        #[serde(default)]
        model: Option<String>,
    },
    /// A permission by the value the catalogue offers.
    Permission {
        value: String,
    },
    /// A mode by the value the catalogue offers.
    Mode {
        value: String,
    },
    Effort {
        #[serde(default)]
        effort: Option<String>,
    },
    /// An encoded Input, hex, for arms the vocabulary above does not name.
    Raw {
        hex: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    #[serde(default)]
    #[allow(dead_code)]
    about: String,
    #[serde(default)]
    spec: FixtureSpec,
    recording: Option<Recording>,
    #[serde(default)]
    prelude: Vec<Value>,
    #[serde(default)]
    events: Vec<Value>,
    #[serde(default)]
    end: EndRules,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureSpec {
    agent_id: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    created_at_ms: i64,
    #[serde(default)]
    parent: bool,
    initial_prompt: Option<String>,
    producer_version: Option<String>,
    /// The provider arguments the daemon resolved, as a recording's spawn
    /// passed them.
    #[serde(default)]
    provider_args: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recording {
    path: String,
    format: String,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Expect {
    /// Phase in the newest snapshot after this event.
    phase: Option<String>,
    /// accepted | queued | rejected:<REFUSAL> [detail], for this event's input.
    reply: Option<String>,
    /// Input ids queued in the newest snapshot, in order; a steered entry
    /// reads `<id>(steered)`.
    queue: Option<Vec<String>>,
    /// Open ask keys in the newest snapshot, in order.
    asks: Option<Vec<String>>,
    /// Envelope ids of agent messages still pending after this event.
    pending: Option<Vec<String>>,
    /// working_on in the newest snapshot; null for none.
    #[serde(default, deserialize_with = "present")]
    working_on: Option<Value>,
    /// false, or {"turn_id": n, "last_message_key": "k"}.
    turn_end: Option<Value>,
    /// Item keys this event emitted, in order.
    items: Option<Vec<String>>,
    /// Effect lines this event produced, rendered as in the golden.
    effects: Option<Vec<String>>,
    /// This event emitted nothing to the journal.
    silent: Option<bool>,
}

/// Keeps an explicit JSON null distinct from an absent key.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

#[allow(clippy::large_enum_variant)]
enum Action {
    Event(Event),
    Checkpoint,
}

struct Scripted {
    label: String,
    action: Action,
    expect: Option<Expect>,
}

struct Frame {
    label: String,
    step: Step,
    effects: Vec<Effect>,
    /// The pending agent-message set after this frame.
    pending: Vec<Vec<u8>>,
}

/// What [`run_golden`] found.
#[derive(Debug)]
pub struct GoldenReport {
    pub fixture: PathBuf,
    /// The emission as rendered now.
    pub rendered: String,
    /// The committed golden, if any.
    pub golden: Option<String>,
    /// The golden was rewritten because updates were requested.
    pub updated: bool,
    /// Broken invariants and checkpoint-property failures.
    pub violations: Vec<String>,
    /// Unmet expectations, fixture errors and golden mismatches.
    pub failures: Vec<String>,
}

impl GoldenReport {
    pub fn is_ok(&self) -> bool {
        self.violations.is_empty() && self.failures.is_empty()
    }

    /// Panics with everything wrong, for use in tests.
    #[track_caller]
    pub fn assert_ok(&self) {
        if self.is_ok() {
            return;
        }
        let mut message = format!("{} failed\n", self.fixture.display());
        for violation in &self.violations {
            let _ = writeln!(message, "  invariant: {violation}");
        }
        for failure in &self.failures {
            let _ = writeln!(message, "  {failure}");
        }
        panic!("{message}");
    }

    fn fail(fixture: &Path, failure: String) -> Self {
        Self {
            fixture: fixture.to_owned(),
            rendered: String::new(),
            golden: None,
            updated: false,
            violations: Vec::new(),
            failures: vec![failure],
        }
    }
}

/// One fixture event as the interpreter answered it: the input it carried,
/// if any, the step it emitted and the verdicts it replied.
#[derive(Clone, Debug)]
pub struct Replayed {
    pub label: String,
    pub input: Option<Input>,
    pub step: Step,
    pub replies: Vec<(Vec<u8>, SendInputResponse)>,
    /// The catalogue this frame wrote, with its hash, when it wrote one.
    pub catalogue: Option<(Vec<u8>, wire::Catalogue)>,
}

/// Runs one fixture through interpreter `I` and returns its emission, frame
/// by frame, the initial frame first: what the view goldens replay through
/// the session model.
pub fn replay<I: Interpreter>(fixture: &Path) -> Result<Vec<Replayed>, String> {
    let text =
        std::fs::read_to_string(fixture).map_err(|error| format!("read fixture: {error}"))?;
    let parsed: Fixture =
        serde_json::from_str(&text).map_err(|error| format!("parse fixture: {error}"))?;
    let script = script::<I>(fixture, &parsed)?;
    let spec = spec::<I>(&parsed.spec);
    let producer = parsed.spec.producer_version.as_deref().unwrap_or("test");
    let frames = execute::<I>(&spec, producer, &script, None)?;
    let mut inputs = script.iter().filter_map(|scripted| match &scripted.action {
        Action::Event(Event::Input(input)) => Some((scripted.label.as_str(), input.clone())),
        _ => None,
    });
    let mut next_input = inputs.next();
    Ok(frames
        .into_iter()
        .map(|frame| {
            let input = match &next_input {
                Some((label, _)) if *label == frame.label => next_input
                    .take()
                    .map(|(_, input)| input)
                    .inspect(|_| next_input = inputs.next()),
                _ => None,
            };
            let replies = frame
                .effects
                .iter()
                .filter_map(|effect| match effect {
                    Effect::Reply { input_id, verdict } => {
                        Some((input_id.clone(), verdict.clone()))
                    }
                    _ => None,
                })
                .collect();
            let named = named_catalogue(&frame.step);
            let catalogue = frame.effects.iter().find_map(|effect| match effect {
                Effect::WriteBlob { hash, bytes } if Some(hash.as_slice()) == named => Some((
                    hash.clone(),
                    wire::Catalogue::decode(bytes.as_slice()).unwrap_or_default(),
                )),
                _ => None,
            });
            Replayed {
                label: frame.label,
                input,
                step: frame.step,
                replies,
                catalogue,
            }
        })
        .collect())
}

/// A fixture as the events it feeds an interpreter, for tests that drive
/// one themselves.
pub struct FixtureScript {
    pub spec: AgentSpec,
    pub producer: String,
    /// Each event with its label, in order; `None` is a checkpoint the
    /// fixture takes there.
    pub events: Vec<(String, Option<Event>)>,
}

/// Reads one fixture for interpreter `I` without running it.
pub fn fixture_script<I: Interpreter>(fixture: &Path) -> Result<FixtureScript, String> {
    let text =
        std::fs::read_to_string(fixture).map_err(|error| format!("read fixture: {error}"))?;
    let parsed: Fixture =
        serde_json::from_str(&text).map_err(|error| format!("parse fixture: {error}"))?;
    let events = script::<I>(fixture, &parsed)?
        .into_iter()
        .map(|scripted| {
            let event = match scripted.action {
                Action::Event(event) => Some(event),
                Action::Checkpoint => None,
            };
            (scripted.label, event)
        })
        .collect();
    Ok(FixtureScript {
        spec: spec::<I>(&parsed.spec),
        producer: parsed
            .spec
            .producer_version
            .clone()
            .unwrap_or_else(|| "test".into()),
        events,
    })
}

/// Runs a fixture's events through interpreter `I` until one emits a step
/// `until` accepts, and returns the state at that point with the fixture's
/// spec; None when no step does. For tests that take a fixture to a moment
/// and drive the interpreter on from there.
pub fn run_until<I: Interpreter>(
    fixture: &Path,
    until: impl Fn(&Step) -> bool,
) -> Result<Option<(I::State, AgentSpec)>, String> {
    let script = fixture_script::<I>(fixture)?;
    let (mut state, _) = I::initial(&script.spec, &script.producer);
    for (_, event) in script.events {
        let Some(event) = event else {
            continue;
        };
        if until(&I::step(&mut state, event).step) {
            return Ok(Some((state, script.spec)));
        }
    }
    Ok(None)
}

/// Runs one fixture through interpreter `I` and checks its golden, its
/// expectations, the invariants and the checkpoint property.
pub fn run_golden<I: Interpreter>(fixture: &Path) -> GoldenReport {
    let text = match std::fs::read_to_string(fixture) {
        Ok(text) => text,
        Err(error) => return GoldenReport::fail(fixture, format!("read fixture: {error}")),
    };
    let parsed: Fixture = match serde_json::from_str(&text) {
        Ok(parsed) => parsed,
        Err(error) => return GoldenReport::fail(fixture, format!("parse fixture: {error}")),
    };
    let script = match script::<I>(fixture, &parsed) {
        Ok(script) => script,
        Err(error) => return GoldenReport::fail(fixture, error),
    };
    let spec = spec::<I>(&parsed.spec);
    let producer = parsed.spec.producer_version.as_deref().unwrap_or("test");

    let mut report = GoldenReport {
        fixture: fixture.to_owned(),
        rendered: String::new(),
        golden: None,
        updated: false,
        violations: Vec::new(),
        failures: Vec::new(),
    };

    let frames = match execute::<I>(&spec, producer, &script, None) {
        Ok(frames) => frames,
        Err(error) => return GoldenReport::fail(fixture, error),
    };
    report.rendered = render::<I>(&frames);

    let steps = frames
        .iter()
        .map(|frame| frame.step.clone())
        .collect::<Vec<_>>();
    report.violations = check_invariants::<I>(&steps, &parsed.end);
    report.violations.extend(check_resumes(&frames));
    report.failures = check_expectations::<I>(&frames, &script);

    // The checkpoint property, at every prefix.
    let whole = merge(&frames);
    for at in 0..=script.len() {
        match execute::<I>(&spec, producer, &script, Some(at)) {
            Ok(resumed) => {
                if let Some(problem) = check_resumes(&resumed).into_iter().next() {
                    report
                        .violations
                        .push(format!("checkpoint after event {at}: {problem}"));
                }
                if let Some(difference) = whole.difference(&merge(&resumed)) {
                    report.violations.push(format!(
                        "resuming from a checkpoint after event {at} diverges: {difference}"
                    ));
                }
            }
            Err(error) => report
                .violations
                .push(format!("checkpoint after event {at}: {error}")),
        }
    }

    let golden_path = fixture.with_extension("golden");
    if std::env::var(UPDATE_GOLDENS_ENV).is_ok_and(|value| value == "1") {
        match std::fs::write(&golden_path, &report.rendered) {
            Ok(()) => report.updated = true,
            Err(error) => report.failures.push(format!("write golden: {error}")),
        }
        report.golden = Some(report.rendered.clone());
    } else {
        match std::fs::read_to_string(&golden_path) {
            Ok(golden) => {
                if golden != report.rendered {
                    report.failures.push(format!(
                        "emission differs from {}:\n{}",
                        golden_path.display(),
                        first_difference(&golden, &report.rendered)
                    ));
                }
                report.golden = Some(golden);
            }
            Err(_) => report.failures.push(format!(
                "no golden at {}; review the emission and write it with {UPDATE_GOLDENS_ENV}=1",
                golden_path.display()
            )),
        }
    }
    report
}

fn spec<I: Interpreter>(spec: &FixtureSpec) -> AgentSpec {
    AgentSpec {
        agent_id: spec
            .agent_id
            .as_deref()
            .unwrap_or("agent")
            .as_bytes()
            .to_vec(),
        kind: I::KIND as i32,
        name: spec.name.clone(),
        created_at_ms: spec.created_at_ms,
        incarnation: 1,
        provider_args: spec.provider_args.clone(),
        parent: spec.parent.then(|| wire::AgentRef {
            host_id: b"parent-host".to_vec(),
            agent_id: b"parent".to_vec(),
        }),
        bootstrap: spec.initial_prompt.as_ref().map(|text| wire::Bootstrap {
            of: Some(wire::bootstrap::Of::Prompt(wire::PromptSubmission {
                input_id: b"initial".to_vec(),
                prompt: Some(prompt(text)),
            })),
        }),
        ..Default::default()
    }
}

fn script<I: Interpreter>(fixture: &Path, parsed: &Fixture) -> Result<Vec<Scripted>, String> {
    let mut script = Vec::new();
    for (index, value) in parsed.prelude.iter().enumerate() {
        script.push(scripted::<I>(value).map_err(|error| format!("prelude {index}: {error}"))?);
    }
    if let Some(recording) = &parsed.recording {
        let path = fixture
            .parent()
            .unwrap_or(Path::new("."))
            .join(&recording.path);
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("read recording {}: {error}", path.display()))?;
        for (index, event) in I::recording(&recording.format, &bytes)?
            .into_iter()
            .enumerate()
        {
            script.push(Scripted {
                label: format!("{RECORDED}{index}: {}", event_label(&event)),
                action: Action::Event(event),
                expect: None,
            });
        }
    }
    for (index, value) in parsed.events.iter().enumerate() {
        script.push(scripted::<I>(value).map_err(|error| format!("event {index}: {error}"))?);
    }
    Ok(script)
}

fn scripted<I: Interpreter>(value: &Value) -> Result<Scripted, String> {
    let Value::Object(object) = value else {
        return Err("an event is an object".into());
    };
    let mut action_keys = object
        .keys()
        .filter(|key| !matches!(key.as_str(), "expect" | "note"));
    let (Some(key), None) = (action_keys.next(), action_keys.next()) else {
        return Err(format!("an event has exactly one action: {value}"));
    };
    let body = &object[key];
    let expect = object
        .get("expect")
        .map(|expect| serde_json::from_value::<Expect>(expect.clone()))
        .transpose()
        .map_err(|error| format!("expect: {error}"))?;
    let label = format!("{key} {body}");
    let action = match key.as_str() {
        "tick" => Action::Event(Event::Tick {
            at_ms: body.as_i64().ok_or("tick takes milliseconds")?,
        }),
        "fact" => Action::Event(Event::Fact(fact(body)?)),
        "input" => Action::Event(Event::Input(input_of::<I>(body)?)),
        "agent_message" => Action::Event(Event::AgentMessage(agent_message(body)?)),
        "rename" => Action::Event(Event::Rename {
            name: body
                .get("name")
                .and_then(Value::as_str)
                .ok_or("rename takes a name")?
                .to_owned(),
        }),
        "provider_exit" => Action::Event(Event::ProviderExit {
            code: body
                .get("code")
                .and_then(Value::as_i64)
                .map(|code| code as i32),
        }),
        "daemon_lost" => Action::Event(Event::DaemonLost),
        "exiting" => {
            let word = body.as_str().ok_or("exiting takes a cause")?;
            let cause = wire::ExitCause::from_str_name(&format!(
                "EXIT_CAUSE_{}",
                word.to_ascii_uppercase().replace(' ', "_")
            ))
            .ok_or_else(|| format!("no exit cause is named {word}"))?;
            Action::Event(Event::Exiting(wire::IncarnationEnd {
                cause: cause as i32,
                detail: None,
            }))
        }
        "stop" => Action::Event(Event::StopRequested(
            match body.as_str().ok_or("stop takes a mode")? {
                "graceful" => StopMode::Graceful,
                "abort" => StopMode::Abort,
                "kill" => StopMode::Kill,
                other => return Err(format!("unknown stop mode {other:?}")),
            },
        )),
        "git" => Action::Event(Event::Git(git_of(body)?)),
        "checkpoint" => Action::Checkpoint,
        other => return Err(format!("unknown action {other:?}")),
    };
    Ok(Scripted {
        label,
        action,
        expect,
    })
}

/// A repository's facts as a fixture writes them; null outside one.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureGit {
    branch: Option<String>,
    base_branch: Option<String>,
    uncommitted: Option<FixtureTotals>,
    on_branch: Option<FixtureTotals>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureTotals {
    files: u32,
    added: u32,
    removed: u32,
}

fn git_of(body: &Value) -> Result<Option<wire::Git>, String> {
    let Some(git) = serde_json::from_value::<Option<FixtureGit>>(body.clone())
        .map_err(|error| format!("git: {error}"))?
    else {
        return Ok(None);
    };
    let totals = |totals: Option<FixtureTotals>| {
        totals.map(|totals| wire::ChangeTotals {
            files: totals.files,
            added: totals.added,
            removed: totals.removed,
        })
    };
    Ok(Some(wire::Git {
        branch: git.branch,
        base_branch: git.base_branch,
        uncommitted: totals(git.uncommitted),
        on_branch: totals(git.on_branch),
    }))
}

/// A hash's first four bytes, which tell catalogues apart in a golden.
fn short_hash(hash: &[u8]) -> String {
    crate::to_hex(&hash[..hash.len().min(4)])
}

/// A snapshot's repository facts in a golden.
fn git_label(git: &wire::Git) -> String {
    let totals = |totals: &Option<wire::ChangeTotals>| {
        totals.as_ref().map_or("-".to_owned(), |totals| {
            format!("{}f+{}-{}", totals.files, totals.added, totals.removed)
        })
    };
    format!(
        "git(branch={} base={} uncommitted={} on_branch={})",
        git.branch.as_deref().unwrap_or("-"),
        git.base_branch.as_deref().unwrap_or("-"),
        totals(&git.uncommitted),
        totals(&git.on_branch)
    )
}

fn fact(body: &Value) -> Result<Fact, String> {
    let channel =
        serde_json::from_value::<Channel>(body.get("channel").cloned().unwrap_or_default())
            .map_err(|error| format!("fact channel: {error}"))?;
    let payload = match (body.get("json"), body.get("text")) {
        (Some(json), None) => serde_json::to_vec(json).expect("json serializes"),
        (None, Some(Value::String(text))) => text.clone().into_bytes(),
        _ => return Err("a fact has either json or text".into()),
    };
    Ok(Fact { channel, payload })
}

fn input_of<I: Interpreter>(body: &Value) -> Result<Input, String> {
    let mut object = body.as_object().cloned().ok_or("an input is an object")?;
    let id = object
        .remove("id")
        .and_then(|id| id.as_str().map(str::to_owned))
        .ok_or("an input has a string id")?;
    let input: FixtureInput =
        serde_json::from_value(Value::Object(object)).map_err(|error| format!("input: {error}"))?;
    let input_id = id.into_bytes();
    match &input {
        FixtureInput::Raw { hex } => {
            let bytes = from_hex(hex)?;
            let mut decoded = Input::decode(bytes.as_slice()).map_err(|error| error.to_string())?;
            decoded.input_id = input_id;
            Ok(decoded)
        }
        other => I::fixture_input(input_id, other)
            .ok_or_else(|| format!("{:?} has no input for {other:?}", I::KIND)),
    }
}

/// An agent message as its daemon hands it over: the envelope with the
/// sender the daemon set.
fn agent_message(body: &Value) -> Result<Envelope, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Message {
        envelope: String,
        #[serde(default)]
        from: String,
        text: String,
    }
    let message: Message =
        serde_json::from_value(body.clone()).map_err(|error| format!("agent_message: {error}"))?;
    Ok(Envelope {
        id: message.envelope.into_bytes(),
        context: None,
        from: Some(Sender {
            value: Some(sender::Value::Agent(wire::AgentSender {
                agent_id: message.from.clone().into_bytes(),
                host_id: b"host".to_vec(),
                name: message.from,
            })),
        }),
        to: None,
        kind: EnvelopeKind::Message as i32,
        text: message.text,
        ended: None,
        incarnation: None,
    })
}

fn prompt(text: &str) -> PromptInput {
    PromptInput {
        text: text.to_owned(),
        attachments: Vec::new(),
    }
}

/// A queue entry as goldens and expectations name it: its input id, marked
/// when it was sent into the running turn and awaits its reflection.
fn queue_label(entry: &wire::QueuedInput) -> String {
    if entry.steer {
        format!("{}(steered)", display_id(&entry.input_id))
    } else {
        display_id(&entry.input_id)
    }
}

fn send_now(target: &str) -> SendQueuedNow {
    SendQueuedNow {
        queued_input_id: target.as_bytes().to_vec(),
    }
}

fn withdraw(target: &str) -> WithdrawQueued {
    WithdrawQueued {
        queued_input_id: target.as_bytes().to_vec(),
    }
}

fn claude_answer(ask: &str, answer: &Value) -> Option<ClaudeAnswer> {
    let text = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let of = if let Some(allow) = answer.get("allow") {
        claude_answer::Of::Permission(wire::PermissionAnswer {
            of: Some(wire::permission_answer::Of::Allow(wire::PermissionAllow {
                scope: allow
                    .get("scope")
                    .and_then(Value::as_u64)
                    .map(|scope| scope as u32),
            })),
        })
    } else if let Some(deny) = answer.get("deny") {
        claude_answer::Of::Permission(wire::PermissionAnswer {
            of: Some(wire::permission_answer::Of::Deny(wire::PermissionDeny {
                note: text(deny, "note"),
                stop: deny.get("stop").and_then(Value::as_bool).unwrap_or(false),
            })),
        })
    } else if let Some(approve) = answer.get("approve_plan") {
        let accepting_edits = approve
            .get("auto_accept_edits")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        claude_answer::Of::Plan(wire::PlanAnswer {
            choice: if accepting_edits {
                wire::PlanChoice::StartAcceptingEdits
            } else {
                wire::PlanChoice::Start
            } as i32,
            note: None,
        })
    } else if answer.get("selected").is_some() {
        claude_answer::Of::Question(question_answer(answer))
    } else if let Some(reply) = answer.get("reply") {
        claude_answer::Of::Reply(reply_instead(reply))
    } else if let Some(link) = answer.get("link") {
        claude_answer::Of::Link(wire::LinkAnswer {
            action: wire::FormAction::from_str_name(&format!(
                "FORM_ACTION_{}",
                text(link, "action").to_uppercase()
            ))? as i32,
        })
    } else if let Some(form) = answer.get("form") {
        claude_answer::Of::Form(wire::FormAnswer {
            action: wire::FormAction::from_str_name(&format!(
                "FORM_ACTION_{}",
                text(form, "action").to_uppercase()
            ))? as i32,
            content_json: form
                .get("content")
                .map(|content| content.to_string().into_bytes())
                .unwrap_or_default(),
        })
    } else {
        let send_back = answer.get("send_back")?;
        claude_answer::Of::Plan(wire::PlanAnswer {
            choice: wire::PlanChoice::KeepPlanning as i32,
            note: Some(text(send_back, "note")).filter(|note| !note.is_empty()),
        })
    };
    Some(ClaudeAnswer {
        ask_key: ask.to_owned(),
        of: Some(of),
    })
}

/// The shared fixture vocabulary as a Claude PTY input.
pub fn claude_pty_input(input_id: Vec<u8>, input: &FixtureInput) -> Option<Input> {
    use claude_pty_input::Of;
    let of = match input {
        FixtureInput::Prompt { text } => Of::Prompt(prompt(text)),
        FixtureInput::Withdraw { target } => Of::Withdraw(withdraw(target)),
        FixtureInput::SendNow { target } => Of::SendNow(send_now(target)),
        FixtureInput::Interrupt {} => Of::Interrupt(wire::Interrupt {}),
        FixtureInput::Clear {} => Of::Clear(wire::Clear {}),
        FixtureInput::Answer { ask, answer } => Of::Answer(claude_answer(ask, answer)?),
        FixtureInput::CyclePermissionMode {} => {
            Of::CyclePermissionMode(wire::CyclePermissionMode {})
        }
        FixtureInput::Raw { .. }
        | FixtureInput::Model { .. }
        | FixtureInput::Permission { .. }
        | FixtureInput::Mode { .. }
        | FixtureInput::Effort { .. } => return None,
    };
    Some(Input::new(input_id, wire::ClaudePtyInput { of: Some(of) }))
}

/// The shared fixture vocabulary as a Claude SDK input.
pub fn claude_sdk_input(input_id: Vec<u8>, input: &FixtureInput) -> Option<Input> {
    use claude_sdk_input::Of;
    let of = match input {
        FixtureInput::Prompt { text } => Of::Prompt(prompt(text)),
        FixtureInput::Withdraw { target } => Of::Withdraw(withdraw(target)),
        FixtureInput::SendNow { target } => Of::SendNow(send_now(target)),
        FixtureInput::Interrupt {} => Of::Interrupt(wire::Interrupt {}),
        FixtureInput::Clear {} => Of::Clear(wire::Clear {}),
        FixtureInput::Answer { ask, answer } => Of::Answer(claude_answer(ask, answer)?),
        FixtureInput::Model { model } => Of::Model(wire::SetModel {
            model: model.clone(),
        }),
        FixtureInput::Permission { value } => Of::Permission(wire::SetPermission {
            value: value.clone(),
        }),
        FixtureInput::Effort { effort } => Of::Effort(wire::SetEffort {
            effort: effort.clone(),
        }),
        FixtureInput::CyclePermissionMode {}
        | FixtureInput::Mode { .. }
        | FixtureInput::Raw { .. } => return None,
    };
    Some(Input::new(input_id, wire::ClaudeSdkInput { of: Some(of) }))
}

/// The shared fixture vocabulary as a Codex input.
pub fn codex_input(input_id: Vec<u8>, input: &FixtureInput) -> Option<Input> {
    use codex_input::Of;
    let of = match input {
        FixtureInput::Prompt { text } => Of::Prompt(prompt(text)),
        FixtureInput::Withdraw { target } => Of::Withdraw(withdraw(target)),
        FixtureInput::SendNow { target } => Of::SendNow(send_now(target)),
        FixtureInput::Interrupt {} => Of::Interrupt(wire::Interrupt {}),
        FixtureInput::Answer { ask, answer } => Of::Answer(codex_answer(ask, answer)?),
        FixtureInput::Model { model } => Of::Model(wire::SetModel {
            model: model.clone(),
        }),
        FixtureInput::Effort { effort } => Of::Effort(wire::SetEffort {
            effort: effort.clone(),
        }),
        FixtureInput::Permission { value } => Of::Permission(wire::SetPermission {
            value: value.clone(),
        }),
        FixtureInput::Mode { value } => Of::Mode(wire::SetMode {
            value: value.clone(),
        }),
        FixtureInput::Clear {}
        | FixtureInput::CyclePermissionMode {}
        | FixtureInput::Raw { .. } => return None,
    };
    Some(Input::new(input_id, wire::CodexInput { of: Some(of) }))
}

/// One response per question: an option index, a list of indices for a
/// multi-select, a string for a typed answer, or null for a skip. `notes`
/// holds each question's note (null for none); a lone `note` goes on the
/// last question.
fn question_responses(answer: &Value) -> Vec<wire::QuestionResponse> {
    let selected = answer
        .get("selected")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let notes = answer
        .get("notes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let last = selected.len().saturating_sub(1);
    selected
        .iter()
        .enumerate()
        .map(|(at, choice)| wire::QuestionResponse {
            selected: match choice {
                Value::Array(indices) => indices
                    .iter()
                    .filter_map(Value::as_u64)
                    .map(|index| index as u32)
                    .collect(),
                other => other
                    .as_u64()
                    .map(|index| vec![index as u32])
                    .unwrap_or_default(),
            },
            other: choice.as_str().map(str::to_owned),
            note: notes
                .get(at)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    (at == last)
                        .then(|| answer.get("note").and_then(Value::as_str))
                        .flatten()
                        .map(str::to_owned)
                }),
        })
        .collect()
}

fn question_answer(answer: &Value) -> wire::QuestionAnswer {
    wire::QuestionAnswer {
        answers: question_responses(answer),
    }
}

/// `{"reply": {"text": "…", "selected": [...]}}`: the person's words, with
/// the answers so far in the question vocabulary.
fn reply_instead(reply: &Value) -> wire::ReplyInstead {
    wire::ReplyInstead {
        text: reply
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        answers_so_far: question_responses(reply),
    }
}

fn codex_answer(ask: &str, answer: &Value) -> Option<CodexAnswer> {
    use wire::codex_answer::Of;
    let action = |value: &Value| {
        wire::FormAction::from_str_name(&format!(
            "FORM_ACTION_{}",
            value
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_uppercase()
        ))
        .map(|action| action as i32)
    };
    let of = if let Some(decision) = answer.get("decision").and_then(Value::as_str) {
        Of::Approve(
            wire::Decision::from_str_name(&format!("DECISION_{}", decision.to_uppercase()))? as i32,
        )
    } else if answer.get("selected").is_some() {
        Of::Question(question_answer(answer))
    } else if let Some(reply) = answer.get("reply") {
        Of::Reply(reply_instead(reply))
    } else if let Some(link) = answer.get("link") {
        Of::Link(wire::LinkAnswer {
            action: action(link)?,
        })
    } else if let Some(form) = answer.get("form") {
        Of::Form(wire::FormAnswer {
            action: action(form)?,
            content_json: form
                .get("content")
                .map(|content| content.to_string().into_bytes())
                .unwrap_or_default(),
        })
    } else if let Some(plan) = answer.get("plan") {
        let choice = plan
            .get("choice")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Of::Plan(wire::PlanAnswer {
            choice: wire::PlanChoice::from_str_name(&format!(
                "PLAN_CHOICE_{}",
                choice.to_uppercase()
            ))? as i32,
            note: plan.get("note").and_then(Value::as_str).map(str::to_owned),
        })
    } else {
        let grant = answer.get("grant")?;
        let paths = |key: &str| {
            grant
                .get(key)
                .and_then(Value::as_array)
                .map(|paths| {
                    paths
                        .iter()
                        .filter_map(|path| path.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        Of::Grant(wire::GrantAnswer {
            read: paths("read"),
            write: paths("write"),
            network: grant
                .get("network")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            for_session: grant
                .get("for_session")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    };
    Some(CodexAnswer {
        ask_key: ask.to_owned(),
        of: Some(of),
    })
}

/// Runs the script. With `checkpoint_at`, the state is written, read back
/// and resumed before event `checkpoint_at`, as a restart would.
fn execute<I: Interpreter>(
    spec: &AgentSpec,
    producer: &str,
    script: &[Scripted],
    checkpoint_at: Option<usize>,
) -> Result<Vec<Frame>, String> {
    let (mut state, first) = I::initial(spec, producer);
    let mut frames = vec![Frame {
        label: "initial".into(),
        step: first,
        effects: Vec::new(),
        pending: I::pending_messages(&state),
    }];
    let round_trip = |state: &I::State| -> Result<(I::State, Step), String> {
        let bytes = encode_checkpoint(state);
        let decoded: I::State = decode_checkpoint(&bytes)
            .map_err(|error| format!("checkpoint does not decode: {error}"))?;
        Ok(decoded.resume())
    };
    for (index, scripted) in script.iter().enumerate() {
        if checkpoint_at == Some(index) {
            let (resumed, step) = round_trip(&state)?;
            state = resumed;
            frames.push(Frame {
                label: "resume".into(),
                step,
                effects: Vec::new(),
                pending: I::pending_messages(&state),
            });
        }
        match &scripted.action {
            Action::Event(event) => {
                let stepped = I::step(&mut state, event.clone());
                frames.push(Frame {
                    label: scripted.label.clone(),
                    step: stepped.step,
                    effects: stepped.effects,
                    pending: I::pending_messages(&state),
                });
            }
            Action::Checkpoint => {
                let (resumed, step) = round_trip(&state)?;
                state = resumed;
                frames.push(Frame {
                    label: "resume".into(),
                    step,
                    effects: Vec::new(),
                    pending: I::pending_messages(&state),
                });
            }
        }
    }
    if checkpoint_at == Some(script.len()) {
        let (_, step) = round_trip(&state)?;
        frames.push(Frame {
            label: "resume".into(),
            step,
            effects: Vec::new(),
            pending: I::pending_messages(&state),
        });
    }
    Ok(frames)
}

/// The invariants every interpreter's journal obeys, over the steps of one
/// run in order, the first being the initial frame.
pub fn check_invariants<I: Interpreter>(steps: &[Step], end: &EndRules) -> Vec<String> {
    let mut violations = Vec::new();
    match steps.first() {
        None => violations.push("no initial frame".to_owned()),
        Some(first) => {
            match &first.snapshot {
                None => violations.push("the first frame is not a Snapshot".to_owned()),
                Some(snapshot) => {
                    if snapshot.phase != Phase::Starting as i32 {
                        violations.push(format!(
                            "the first Snapshot's phase is {}, not STARTING",
                            phase_name(snapshot.phase)
                        ));
                    }
                    if snapshot.body != I::unknown_snapshot() {
                        violations.push(
                            "the first Snapshot's body is not every field at its unknown"
                                .to_owned(),
                        );
                    }
                }
            }
            if !first.items.is_empty() || !first.appends.is_empty() {
                violations.push("the first frame carries items".to_owned());
            }
        }
    }

    let mut arms = BTreeMap::<String, String>::new();
    let mut open = BTreeSet::<String>::new();
    let mut last_snapshot: Option<&Snapshot> = None;
    for (index, step) in steps.iter().enumerate() {
        let mut in_step = BTreeSet::new();
        for item in &step.items {
            if !in_step.insert(item.key.as_str()) {
                violations.push(format!("frame {index}: key {} twice in one step", item.key));
            }
            let view = I::describe_item(&item.body);
            match arms.get(&item.key) {
                Some(arm) if *arm != view.arm => violations.push(format!(
                    "frame {index}: key {} changed from {arm} to {}",
                    item.key, view.arm
                )),
                Some(_) => {
                    if !view.complete && !open.contains(&item.key) {
                        violations.push(format!(
                            "frame {index}: key {} reopened after its final item",
                            item.key
                        ));
                    }
                }
                None => {
                    arms.insert(item.key.clone(), view.arm.clone());
                }
            }
            if view.complete {
                open.remove(&item.key);
            } else {
                open.insert(item.key.clone());
            }
        }
        for append in &step.appends {
            if !arms.contains_key(&append.key) {
                violations.push(format!(
                    "frame {index}: append to unknown key {}",
                    append.key
                ));
            } else if !open.contains(&append.key) {
                violations.push(format!(
                    "frame {index}: append to {} after its final item",
                    append.key
                ));
            }
        }
        if let Some(snapshot) = &step.snapshot {
            for (ask, item_key) in I::describe_snapshot(&snapshot.body).asks {
                if !item_key.is_empty() && !arms.contains_key(&item_key) {
                    violations.push(format!(
                        "frame {index}: open ask {ask} points at {item_key}, which has no item"
                    ));
                }
            }
            last_snapshot = Some(snapshot);
        }
    }

    if let Some(snapshot) = last_snapshot
        && !end.queue_may_remain
        && !snapshot.queue.is_empty()
    {
        violations.push(format!(
            "the queue did not drain: {}",
            snapshot
                .queue
                .iter()
                .map(|entry| display_id(&entry.input_id))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !end.streams_may_remain && !open.is_empty() {
        violations.push(format!(
            "streams never ended with a final item: {}",
            open.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    violations
}

/// A resume re-emits only keys readers already have, at the content they
/// already hold.
fn check_resumes(frames: &[Frame]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut view = View::default();
    for frame in frames {
        if frame.label == "resume" {
            for item in &frame.step.items {
                match view.items.get(&item.key) {
                    None => problems.push(format!("resume emitted unknown key {}", item.key)),
                    Some(held) if held != item => problems.push(format!(
                        "resume re-emitted {} with different content",
                        item.key
                    )),
                    Some(_) => {}
                }
            }
            if !frame.step.appends.is_empty() {
                problems.push("resume emitted appends".to_owned());
            }
            if frame.step.snapshot.is_none() {
                problems.push("resume emitted no snapshot".to_owned());
            }
        }
        view.apply(&frame.step);
    }
    problems
}

/// What a reader holding every committed record would have: items merged
/// by key with appends applied, and the newest snapshot.
#[derive(Default)]
struct View {
    items: BTreeMap<String, Item>,
    snapshot: Option<Snapshot>,
}

impl View {
    fn apply(&mut self, step: &Step) {
        for item in &step.items {
            self.items.insert(item.key.clone(), item.clone());
        }
        for append in &step.appends {
            if let Some(item) = self.items.get_mut(&append.key) {
                item.text.push_str(&append.text);
            }
        }
        if let Some(snapshot) = &step.snapshot {
            self.snapshot = Some(Snapshot {
                at_ms: 0,
                ..snapshot.clone()
            });
        }
    }

    fn difference(&self, other: &View) -> Option<String> {
        let keys = self
            .items
            .keys()
            .chain(other.items.keys())
            .collect::<BTreeSet<_>>();
        for key in keys {
            match (self.items.get(key), other.items.get(key)) {
                (Some(a), Some(b)) if a == b => {}
                (Some(_), Some(_)) => return Some(format!("item {key} differs")),
                (Some(_), None) => return Some(format!("item {key} missing")),
                (None, _) => return Some(format!("extra item {key}")),
            }
        }
        (self.snapshot != other.snapshot).then(|| "the final snapshot differs".to_owned())
    }
}

fn merge(frames: &[Frame]) -> View {
    let mut view = View::default();
    for frame in frames {
        view.apply(&frame.step);
    }
    view
}

fn check_expectations<I: Interpreter>(frames: &[Frame], script: &[Scripted]) -> Vec<String> {
    let mut failures = Vec::new();
    let mut snapshot: Option<Snapshot> = frames[0].step.snapshot.clone();
    // frames[0] is the initial frame; frames[n + 1] answers script[n].
    for (index, (scripted, frame)) in script.iter().zip(&frames[1..]).enumerate() {
        if let Some(latest) = &frame.step.snapshot {
            snapshot = Some(latest.clone());
        }
        let Some(expect) = &scripted.expect else {
            continue;
        };
        let mut fail = |what: &str, expected: String, got: String| {
            if expected != got {
                failures.push(format!(
                    "event {index} ({}): {what} expected {expected}, got {got}",
                    scripted.label
                ));
            }
        };
        let current = snapshot.clone().unwrap_or_default();
        if let Some(phase) = &expect.phase {
            fail("phase", phase.clone(), phase_name(current.phase).to_owned());
        }
        if let Some(reply) = &expect.reply {
            let replies = frame
                .effects
                .iter()
                .filter_map(|effect| match effect {
                    Effect::Reply { verdict, .. } => Some(verdict_name(verdict)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            fail(
                "reply",
                format!("[{reply}]"),
                format!("[{}]", replies.join(", ")),
            );
        }
        if let Some(queue) = &expect.queue {
            fail(
                "queue",
                format!("{queue:?}"),
                format!(
                    "{:?}",
                    current.queue.iter().map(queue_label).collect::<Vec<_>>()
                ),
            );
        }
        if let Some(asks) = &expect.asks {
            fail(
                "asks",
                format!("{asks:?}"),
                format!(
                    "{:?}",
                    I::describe_snapshot(&current.body)
                        .asks
                        .into_iter()
                        .map(|(key, _)| key)
                        .collect::<Vec<_>>()
                ),
            );
        }
        if let Some(pending) = &expect.pending {
            fail(
                "pending",
                format!("{pending:?}"),
                format!(
                    "{:?}",
                    frame
                        .pending
                        .iter()
                        .map(|id| display_id(id))
                        .collect::<Vec<_>>()
                ),
            );
        }
        if let Some(working_on) = &expect.working_on {
            fail(
                "working_on",
                working_on.to_string(),
                current
                    .working_on
                    .as_ref()
                    .map_or(Value::Null, |text| Value::String(text.clone()))
                    .to_string(),
            );
        }
        if let Some(turn_end) = &expect.turn_end {
            let got = frame.step.turn_end.as_ref().map_or(Value::Bool(false), |end| {
                serde_json::json!({ "turn_id": end.turn_id, "last_message_key": end.last_message_key })
            });
            fail("turn_end", turn_end.to_string(), got.to_string());
        }
        if let Some(items) = &expect.items {
            fail(
                "items",
                format!("{items:?}"),
                format!(
                    "{:?}",
                    frame
                        .step
                        .items
                        .iter()
                        .map(|item| &item.key)
                        .collect::<Vec<_>>()
                ),
            );
        }
        if let Some(effects) = &expect.effects {
            fail(
                "effects",
                format!("{effects:?}"),
                format!(
                    "{:?}",
                    frame
                        .effects
                        .iter()
                        .map(|effect| render_effect(effect, named_catalogue(&frame.step)))
                        .collect::<Vec<_>>()
                ),
            );
        }
        if let Some(silent) = expect.silent {
            let step = &frame.step;
            let is_silent = step.items.is_empty()
                && step.appends.is_empty()
                && step.snapshot.is_none()
                && step.turn_end.is_none();
            fail("silent", silent.to_string(), is_silent.to_string());
        }
    }
    failures
}

fn render<I: Interpreter>(frames: &[Frame]) -> String {
    let mut out = String::new();
    let mut pending: &[Vec<u8>] = &[];
    // Repository facts are printed when a snapshot changes them, not on
    // every snapshot that carries them on.
    let mut git: Option<&wire::Git> = None;
    let mut catalogue: Option<&Vec<u8>> = None;
    for (index, frame) in frames.iter().enumerate() {
        let step = &frame.step;
        let pending_changed = frame.pending != pending;
        pending = &frame.pending;
        let silent = frame.effects.is_empty()
            && step.items.is_empty()
            && step.appends.is_empty()
            && step.snapshot.is_none()
            && step.turn_end.is_none()
            && !pending_changed;
        if silent && frame.label.starts_with(RECORDED) {
            continue;
        }
        let _ = writeln!(out, "## {index} {}", frame.label);
        for effect in &frame.effects {
            let _ = writeln!(
                out,
                "effect {}",
                render_effect(effect, named_catalogue(step))
            );
        }
        for item in &frame.step.items {
            let view = I::describe_item(&item.body);
            let _ = writeln!(
                out,
                "item {} {} {}{} at={} text={}{}{}",
                item.key,
                view.arm,
                if view.complete { "final" } else { "open" },
                if item.input_id.is_empty() {
                    String::new()
                } else {
                    format!(" input={}", display_id(&item.input_id))
                },
                item.at_ms,
                Value::String(item.text.clone()),
                if item.attachments.is_empty() {
                    String::new()
                } else {
                    format!(" attachments={}", item.attachments.len())
                },
                if view.text.is_empty() {
                    String::new()
                } else {
                    format!(" {}", view.text)
                }
            );
        }
        for append in &frame.step.appends {
            let _ = writeln!(
                out,
                "append {} {}",
                append.key,
                Value::String(append.text.clone())
            );
        }
        if let Some(snapshot) = &frame.step.snapshot {
            if snapshot.git.as_ref() != git {
                git = snapshot.git.as_ref();
                let label = git.map_or("git none".to_owned(), git_label);
                let _ = writeln!(out, "snapshot {label}");
            }
            if snapshot.catalogue.as_ref() != catalogue {
                catalogue = snapshot.catalogue.as_ref();
                let label = catalogue.map_or("-".to_owned(), |hash| short_hash(hash));
                let _ = writeln!(out, "snapshot catalogue {label}");
            }
            let _ = writeln!(
                out,
                "snapshot {} queue=[{}] working_on={} at={} {}",
                phase_name(snapshot.phase),
                snapshot
                    .queue
                    .iter()
                    .map(queue_label)
                    .collect::<Vec<_>>()
                    .join(","),
                snapshot
                    .working_on
                    .as_ref()
                    .map_or("-".to_owned(), |text| Value::String(text.clone())
                        .to_string()),
                snapshot.at_ms,
                I::describe_snapshot(&snapshot.body).text
            );
        }
        if pending_changed {
            let _ = writeln!(
                out,
                "pending [{}]",
                frame
                    .pending
                    .iter()
                    .map(|id| display_id(id))
                    .collect::<Vec<_>>()
                    .join(",")
            );
        }
        if let Some(end) = &frame.step.turn_end {
            let _ = writeln!(
                out,
                "turn_end {} last={}",
                end.turn_id,
                if end.last_message_key.is_empty() {
                    "-"
                } else {
                    &end.last_message_key
                }
            );
        }
        if let Some(end) = &frame.step.incarnation_end {
            let _ = writeln!(out, "incarnation_end {}", incarnation_end(end));
        }
    }
    out
}

/// A cause by its name without the prefix, then its own words quoted.
fn incarnation_end(end: &wire::IncarnationEnd) -> String {
    format!(
        "{}{}",
        wire::ExitCause::try_from(end.cause).map_or("?", |cause| cause
            .as_str_name()
            .trim_start_matches("EXIT_CAUSE_")),
        end.detail
            .as_ref()
            .map(|detail| format!(" {detail:?}"))
            .unwrap_or_default()
    )
}

/// The catalogue hash a step's snapshot names: the blob it wrote under that
/// hash is its catalogue, rendered as one.
fn named_catalogue(step: &Step) -> Option<&[u8]> {
    step.snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.catalogue.as_deref())
}

fn render_effect(effect: &Effect, catalogue: Option<&[u8]>) -> String {
    match effect {
        Effect::ProviderWrite(bytes) => format!("write {}", display_bytes(bytes)),
        Effect::Reply { input_id, verdict } => {
            format!("reply {} {}", display_id(input_id), verdict_name(verdict))
        }
        Effect::Inject { envelope, via } => {
            format!("inject {} via {via:?}", display_id(&envelope.id))
        }
        Effect::CodexTurnInput {
            request,
            attachments,
        } => format!(
            "write {} attachments={}",
            display_bytes(request),
            attachments.len()
        ),
        Effect::WriteBlob { hash, bytes } if catalogue != Some(hash.as_slice()) => format!(
            "blob {} {} bytes",
            crate::to_hex(&hash[..hash.len().min(4)]),
            bytes.len()
        ),
        Effect::WriteBlob { hash, bytes } => {
            let catalogue = wire::Catalogue::decode(bytes.as_slice()).unwrap_or_default();
            format!(
                "catalogue {} models=[{}] commands={} permissions=[{}] modes=[{}]",
                short_hash(hash),
                crate::claude_common::describe_models(&catalogue.models),
                crate::claude_common::describe_commands(&catalogue.commands),
                catalogue
                    .permissions
                    .iter()
                    .map(|permission| permission.value.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                catalogue
                    .modes
                    .iter()
                    .map(|mode| mode.value.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        Effect::Exit { cause } => format!("exit {}", Value::String(cause.clone())),
        Effect::Terminal(input) => format!("terminal {input}"),
        Effect::FollowTranscript { path } => format!("follow {}", Value::String(path.clone())),
        Effect::UserMessage {
            uuid,
            text,
            attachments,
        } => format!(
            "user {uuid} {}{}",
            Value::String(text.clone()),
            if attachments.is_empty() {
                String::new()
            } else {
                format!(" attachments={}", attachments.len())
            }
        ),
    }
}

fn verdict_name(verdict: &SendInputResponse) -> String {
    match &verdict.of {
        Some(send_input_response::Of::Accepted(accepted)) if accepted.queued => "queued".into(),
        Some(send_input_response::Of::Accepted(_)) => "accepted".into(),
        Some(send_input_response::Of::Rejected(rejected)) => {
            let refusal = wire::Refusal::try_from(rejected.refusal).map_or("?", |refusal| {
                refusal.as_str_name().trim_start_matches("REFUSAL_")
            });
            match &rejected.detail {
                Some(detail) => format!("rejected:{refusal} {detail}"),
                None => format!("rejected:{refusal}"),
            }
        }
        None => "empty".into(),
    }
}

fn phase_name(phase: i32) -> &'static str {
    Phase::try_from(phase).map_or("INVALID", |phase| {
        phase.as_str_name().trim_start_matches("PHASE_")
    })
}

fn event_label(event: &Event) -> String {
    match event {
        Event::Fact(fact) => format!("fact {:?} {}", fact.channel, summary(&fact.payload)),
        Event::Input(input) => format!("input {}", display_id(&input.input_id)),
        Event::AgentMessage(envelope) => format!("agent_message {}", display_id(&envelope.id)),
        Event::Rename { name } => format!("rename {name:?}"),
        Event::Tick { at_ms } => format!("tick {at_ms}"),
        Event::ProviderExit { code } => format!("provider_exit {code:?}"),
        Event::DaemonLost => "daemon_lost".into(),
        Event::Exiting(exiting) => format!("exiting {}", incarnation_end(exiting)),
        Event::StopRequested(mode) => format!(
            "stop {}",
            mode.as_str_name().trim_start_matches("STOP_MODE_")
        ),
        Event::Git(None) => "git none".into(),
        Event::Git(Some(git)) => git_label(git),
    }
}

/// A recorded fact in a golden: the fields that say what it is, since
/// recorded payloads are long.
fn summary(payload: &[u8]) -> String {
    const NAMING: [&str; 6] = [
        "hook_event_name",
        "type",
        "subtype",
        "uuid",
        "tool_use_id",
        "method",
    ];
    if let Ok(Value::Object(object)) = serde_json::from_slice::<Value>(payload) {
        let named = NAMING
            .iter()
            .filter_map(|field| match object.get(*field) {
                Some(Value::String(text)) => Some(format!("{field}={text}")),
                _ => None,
            })
            .collect::<Vec<_>>();
        if !named.is_empty() {
            return named.join(" ");
        }
    }
    let text = display_bytes(payload);
    match text.char_indices().nth(80) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text,
    }
}

/// An id as fixtures write it: text when printable, else hex.
fn display_id(bytes: &[u8]) -> String {
    if !bytes.is_empty() && bytes.iter().all(|byte| (0x21..0x7f).contains(byte)) {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        format!("0x{}", to_hex(bytes))
    }
}

fn display_bytes(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => Value::String(text.to_owned()).to_string(),
        Err(_) => format!("0x{}", to_hex(bytes)),
    }
}

fn first_difference(golden: &str, rendered: &str) -> String {
    let golden_lines = golden.lines().collect::<Vec<_>>();
    let rendered_lines = rendered.lines().collect::<Vec<_>>();
    let at = golden_lines
        .iter()
        .zip(&rendered_lines)
        .position(|(a, b)| a != b)
        .unwrap_or(golden_lines.len().min(rendered_lines.len()));
    format!(
        "  line {}\n  golden:   {}\n  rendered: {}",
        at + 1,
        golden_lines.get(at).unwrap_or(&"<end>"),
        rendered_lines.get(at).unwrap_or(&"<end>")
    )
}
