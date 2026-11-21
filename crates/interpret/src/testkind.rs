//! A minimal interpreter over an authored fact vocabulary, used to exercise
//! the shared core and the golden harness without any provider's format.
//!
//! Facts arrive on the stream channel as `{"type": …}`: `ready`, `user`
//! (a prompt's reflection, or `envelope` for an injected message's),
//! `text`/`text_done` (a streamed message), `tool`/`tool_result`,
//! `ask`/`ask_closed`, `turn_start`, `turn_end` and `model`; for a
//! provider subagent it runs, `subagent_start`, `subagent_text` and
//! `subagent_end`, each naming the subagent by the provider's id for it
//! (`agent`); and what the provider holds: `job_started`/`job_ended` (a
//! background job by `id`, whose end `reported` in a turn of its own when
//! it says so) and `wakeup`/`wakeup_fired`/`wakeup_cancelled` (a scheduled
//! wake-up by `id`, `recurring` or not, whose firing starts a turn). It
//! writes `user`, `answer` and, on a stop while a turn runs or an ask is
//! open, `interrupt`; an answer allowing with a scope grants for the
//! session. Bodies are the SDK's messages under the kind tag `test`.
//!
//! It housekeeps while idle as the providers' interpreters compact: after
//! `housekeeping` (with `after`, in milliseconds), each idle stretch that
//! lasts that long past its turn's end starts housekeeping once, writing
//! `housekeep`, and the provider's `housekept` ends it.
//!
//! It also runs provider subagents, each named by the fact's `child`:
//! `sub_start` (with the `call` that started it, its `title`, and `by`, the
//! subagent that made the call when it is nested), `sub_text`/
//! `sub_text_done`, `sub_ask` and `sub_end`. A person's answer to a
//! subagent's ask writes `answer` with its `child`.

use std::collections::{BTreeMap, BTreeSet};

use prost::Message as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wire::{
    AgentSpec, Ask, ClaudeItem, ClaudeSnapshot, Envelope, Input, NativeStart, PermissionAsk,
    Refusal, Snapshot, Step, ToolCall, ToolState, Turn, TurnOutcome, claude_item, claude_sdk_input,
};

use crate::{
    Carrier, Channel, Checkpoint, Effect, Emit, Event, Fact, FixtureInput, Interpreter, ItemDraft,
    ItemView, Shared, SnapshotView, Stepped, Subagent, agent_message_body, agent_message_key,
    claude_sdk_input, human, is_status_tool, status_working_on, unknown,
};

/// With `FORGET` set, a resume loses the model: the checkpoint property
/// must catch it.
pub struct TestKind<const FORGET: bool = false>;

#[derive(Serialize, Deserialize)]
pub struct State<const FORGET: bool> {
    shared: Shared<Ask>,
    model: Option<String>,
    tools: BTreeMap<String, String>,
    /// A stop interrupted the turn running: its end reads as cut short.
    interrupted: bool,
    /// The background jobs running, by id.
    #[serde(default)]
    jobs: BTreeSet<String>,
    /// When a `wake` fact asked to hear the clock; the tick that reaches
    /// it writes a `woke` item.
    #[serde(default)]
    wake: Option<i64>,
    /// How long an idle stretch lasts before housekeeping starts in it, once
    /// a `housekeeping` fact asked for it.
    #[serde(default)]
    housekeep_after: Option<i64>,
    /// This agent's id, which each subagent's id is derived from.
    #[serde(default)]
    host: Vec<u8>,
    /// The subagents' parts of the step under way, added to the step it
    /// returns; always empty between steps.
    #[serde(skip)]
    parts: Vec<wire::AgentStep>,
}

impl<const FORGET: bool> State<FORGET> {
    /// When this stretch's housekeeping starts, if it is still to come.
    fn housekeeping_at(&self) -> Option<i64> {
        let after = self.housekeep_after?;
        let since = self.shared.idle_since_ms()?;
        self.shared.idle_wake(since, after)
    }

    fn body(&self) -> Vec<u8> {
        ClaudeSnapshot {
            asks: self.shared.asks().open_asks().to_vec(),
            model: self.model.clone(),
            ..unknown::claude()
        }
        .encode_to_vec()
    }
}

impl<const FORGET: bool> Checkpoint for State<FORGET> {
    fn resume(mut self) -> (Self, Step) {
        if FORGET {
            self.model = None;
        }
        let body = self.body();
        let step = self.shared.resume_step(body);
        (self, step)
    }
}

/// A provider subagent's agent id: the same for one provider id under one
/// host, whenever it is asked for.
fn subagent_id(host: &[u8], provider_id: &str) -> Vec<u8> {
    use sha2::Digest as _;
    let digest = sha2::Sha256::new()
        .chain_update(host)
        .chain_update(provider_id.as_bytes())
        .finalize();
    digest[..16].to_vec()
}

/// The step's part for the subagent the provider calls `provider_id`.
fn part<'a>(
    parts: &'a mut Vec<wire::AgentStep>,
    host: &[u8],
    provider_id: &str,
) -> &'a mut wire::AgentStep {
    let agent_id = subagent_id(host, provider_id);
    match parts.iter().position(|part| part.agent_id == agent_id) {
        Some(at) => &mut parts[at],
        None => {
            parts.push(wire::AgentStep {
                agent_id,
                ..wire::AgentStep::default()
            });
            parts.last_mut().expect("just pushed")
        }
    }
}

fn body(kind: claude_item::Kind) -> Vec<u8> {
    ClaudeItem { kind: Some(kind) }.encode_to_vec()
}

fn text_of(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

impl<const FORGET: bool> TestKind<FORGET> {
    fn fact(state: &mut State<FORGET>, emit: &mut Emit, fact: Fact) {
        let Ok(value) = serde_json::from_slice::<Value>(&fact.payload) else {
            return;
        };
        let shared = &mut state.shared;
        let id = text_of(&value, "id");
        match value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "ready" => shared.provider_started(),
            "model" => state.model = Some(text_of(&value, "model")),
            "wake" => state.wake = value.get("at").and_then(Value::as_i64),
            "turn_start" => {
                shared.turn_started();
            }
            "user" => {
                if let Some(envelope) = value.get("envelope").and_then(Value::as_str) {
                    shared.message_consumed(envelope.as_bytes());
                    return;
                }
                let input_id = shared.reflect_prompt().unwrap_or_default();
                shared.item(
                    emit,
                    ItemDraft {
                        key: id,
                        text: text_of(&value, "text"),
                        input_id,
                        body: body(claude_item::Kind::Prompt(wire::Prompt {})),
                        complete: true,
                        ..Default::default()
                    },
                );
            }
            "text" => {
                let delta = text_of(&value, "delta");
                if !shared.append(emit, &id, &delta) {
                    shared.item(
                        emit,
                        ItemDraft {
                            key: id,
                            text: delta,
                            body: body(claude_item::Kind::Message(wire::Text { complete: false })),
                            ..Default::default()
                        },
                    );
                }
            }
            "text_done" => {
                let text = shared
                    .open_item(&id)
                    .map(|item| item.text.clone())
                    .unwrap_or_default();
                shared.item(
                    emit,
                    ItemDraft {
                        key: id.clone(),
                        text,
                        body: body(claude_item::Kind::Message(wire::Text { complete: true })),
                        complete: true,
                        ..Default::default()
                    },
                );
                shared.note_message(&id);
                shared.reply_said(emit, &id);
            }
            "tool" => {
                let name = text_of(&value, "name");
                let server = text_of(&value, "server");
                let input = value.get("input").cloned().unwrap_or(Value::Null);
                let arguments = serde_json::to_vec(&input).expect("json");
                if is_status_tool(&server, &name)
                    && let Some(working_on) = status_working_on(&arguments)
                {
                    shared.set_working_on(working_on);
                }
                state.tools.insert(id.clone(), name.clone());
                shared.item(
                    emit,
                    ItemDraft {
                        key: id,
                        body: body(claude_item::Kind::Tool(ToolCall {
                            input: Some(crate::claude_common::tool_input(
                                &name, &server, &input, "",
                            )),
                            name,
                            server,
                            state: ToolState::Running as i32,
                            ..Default::default()
                        })),
                        complete: true,
                        ..Default::default()
                    },
                );
            }
            "tool_result" => {
                shared.close_asks_for_item(&id);
                let name = state.tools.get(&id).cloned().unwrap_or_default();
                shared.item(
                    emit,
                    ItemDraft {
                        key: id,
                        body: body(claude_item::Kind::Tool(ToolCall {
                            name,
                            state: ToolState::Succeeded as i32,
                            outcome_text: text_of(&value, "output"),
                            ..Default::default()
                        })),
                        complete: true,
                        ..Default::default()
                    },
                );
            }
            "ask" => shared.open_ask(Ask {
                key: id,
                item_key: text_of(&value, "tool"),
                body: Some(wire::ask::Body::Permission(PermissionAsk {
                    tool_name: text_of(&value, "tool_name"),
                    ..Default::default()
                })),
                opened_at_ms: shared.now_ms(),
            }),
            "ask_closed" => {
                shared.close_ask(&id);
            }
            "job_started" | "job_ended" => {
                if text_of(&value, "type") == "job_started" {
                    state.jobs.insert(id);
                } else if state.jobs.remove(&id)
                    && value.get("reported").and_then(Value::as_bool) == Some(true)
                {
                    shared.turn_owed();
                }
                shared.set_jobs(
                    state
                        .jobs
                        .iter()
                        .map(|id| wire::BackgroundJob {
                            key: id.clone(),
                            item_key: id.clone(),
                            ..Default::default()
                        })
                        .collect(),
                );
            }
            "wakeup" => shared.wakeup_scheduled(
                id,
                crate::Wakeup {
                    recurring: value.get("recurring").and_then(Value::as_bool) == Some(true),
                    at_ms: None,
                },
            ),
            "wakeup_fired" => {
                if shared
                    .wakeups()
                    .get(&id)
                    .is_some_and(|wakeup| !wakeup.recurring)
                {
                    shared.wakeup_gone(&id);
                }
                shared.turn_started();
            }
            "wakeup_cancelled" => shared.wakeup_gone(&id),
            "housekeeping" => state.housekeep_after = value.get("after").and_then(Value::as_i64),
            "housekept" => shared.housekeeping_ended(),
            "subagent_start" => {
                let start = wire::NativeStart {
                    provider_child_id: text_of(&value, "agent"),
                    call_agent_id: state.host.clone(),
                    call_key: text_of(&value, "call"),
                    title: text_of(&value, "title"),
                };
                let provider_id = start.provider_child_id.clone();
                part(&mut state.parts, &state.host, &provider_id).start = Some(start);
            }
            "subagent_text" => {
                let at_ms = shared.now_ms();
                let part = part(&mut state.parts, &state.host, &text_of(&value, "agent"));
                part.items.push(wire::Item {
                    key: id,
                    text: text_of(&value, "text"),
                    kind: Self::KIND as i32,
                    body: body(claude_item::Kind::Message(wire::Text { complete: true })),
                    at_ms,
                    ..wire::Item::default()
                });
            }
            "subagent_end" => {
                let cause = match value.get("cause").and_then(Value::as_str) {
                    Some("stopped") => wire::ExitCause::Stopped,
                    _ => wire::ExitCause::Finished,
                };
                part(&mut state.parts, &state.host, &text_of(&value, "agent")).end =
                    Some(wire::IncarnationEnd {
                        cause: cause as i32,
                        detail: None,
                    });
            }
            "turn_end" => {
                // Only a stop interrupts this kind's turns, so an
                // interrupted turn reads as stopped, as the step's TurnEnd
                // records it.
                let interrupted = std::mem::take(&mut state.interrupted);
                let (ended, outcome) = if interrupted {
                    (TurnOutcome::Interrupted, TurnOutcome::Stopped)
                } else {
                    (TurnOutcome::Completed, TurnOutcome::Completed)
                };
                if let Some(turn) = shared.turn_ended(emit, ended) {
                    shared.item(
                        emit,
                        ItemDraft {
                            key: format!("turn:{}", turn.id),
                            body: body(claude_item::Kind::Turn(Turn {
                                turn_id: turn.id,
                                outcome: outcome as i32,
                                started_at_ms: turn.started_at_ms,
                            })),
                            complete: true,
                            ..Default::default()
                        },
                    );
                }
            }
            "sub_start" => {
                // A nested subagent's call is in the chat of the subagent
                // that made it; a direct one's is in the agent's own.
                let call_agent_id = match value.get("by").and_then(Value::as_str) {
                    Some(by) => shared.subagent_id(by),
                    None => shared.agent_id().to_vec(),
                };
                let mut sub = shared.subagent(emit, &text_of(&value, "child"));
                sub.start(NativeStart {
                    call_agent_id,
                    call_key: text_of(&value, "call"),
                    title: text_of(&value, "title"),
                    ..Default::default()
                });
                sub_snapshot(&mut sub);
            }
            "sub_text" => {
                let mut sub = shared.subagent(emit, &text_of(&value, "child"));
                let delta = text_of(&value, "delta");
                if !sub.append(&id, &delta) {
                    sub.item(ItemDraft {
                        key: id,
                        text: delta,
                        body: body(claude_item::Kind::Message(wire::Text { complete: false })),
                        ..Default::default()
                    });
                }
            }
            "sub_text_done" => {
                let mut sub = shared.subagent(emit, &text_of(&value, "child"));
                let text = sub
                    .open_item(&id)
                    .map(|item| item.text.clone())
                    .unwrap_or_default();
                sub.item(ItemDraft {
                    key: id,
                    text,
                    body: body(claude_item::Kind::Message(wire::Text { complete: true })),
                    complete: true,
                    ..Default::default()
                });
            }
            "sub_ask" => {
                let now_ms = shared.now_ms();
                let mut sub = shared.subagent(emit, &text_of(&value, "child"));
                sub.open_ask(Ask {
                    key: id,
                    item_key: text_of(&value, "tool"),
                    body: Some(wire::ask::Body::Permission(PermissionAsk {
                        tool_name: text_of(&value, "tool_name"),
                        ..Default::default()
                    })),
                    opened_at_ms: now_ms,
                });
                sub_snapshot(&mut sub);
            }
            "sub_end" => {
                let mut sub = shared.subagent(emit, &text_of(&value, "child"));
                sub.close_all_asks();
                sub_snapshot(&mut sub);
                sub.end(wire::IncarnationEnd {
                    cause: wire::ExitCause::Finished as i32,
                    detail: None,
                });
            }
            _ => {}
        }
    }

    fn agent_message(state: &mut State<FORGET>, emit: &mut Emit, envelope: Envelope) {
        let shared = &mut state.shared;
        if shared.message_repeated(emit, &envelope.id) {
            return;
        }
        shared.message_accepted(&envelope.id);
        shared.message_handed();
        emit.effect(Effect::Inject {
            id: envelope.id.clone(),
            text: crate::shared::words(&envelope),
            attachments: crate::shared::attachments(&envelope).to_vec(),
            via: Carrier::Stdin,
        });
        shared.item(
            emit,
            ItemDraft {
                key: agent_message_key(&envelope.id),
                text: crate::shared::words(&envelope),
                input_id: envelope.id.clone(),
                body: body(claude_item::Kind::AgentMessage(agent_message_body(
                    &envelope,
                ))),
                complete: true,
                ..Default::default()
            },
        );
        shared.accept(emit, &envelope.id, true);
    }

    fn input(state: &mut State<FORGET>, emit: &mut Emit, input: Input) {
        let shared = &mut state.shared;
        let id = input.input_id.clone();
        let arm = match input.decode_body() {
            Some(wire::KindInput::ClaudeSdk(wire::ClaudeSdkInput { of: Some(arm) })) => arm,
            _ => return shared.reject(emit, &id, Refusal::Unsupported),
        };
        match arm {
            claude_sdk_input::Of::Prompt(prompt) => {
                if let Some(entry) = shared.admit_prompt(emit, &id, prompt, human()) {
                    emit.effect(write(&entry.text));
                }
            }
            claude_sdk_input::Of::Withdraw(withdraw) => {
                shared.withdraw(emit, &id, &withdraw.queued_input_id)
            }
            claude_sdk_input::Of::Interrupt(_) => {
                if shared.is_busy() {
                    emit.effect(Effect::ProviderWrite(br#"{"type":"interrupt"}"#.to_vec()));
                }
                shared.accept(emit, &id, false);
            }
            claude_sdk_input::Of::Answer(answer) => {
                let for_session = matches!(
                    &answer.of,
                    Some(wire::claude_answer::Of::Permission(
                        wire::PermissionAnswer {
                            of: Some(wire::permission_answer::Of::Allow(wire::PermissionAllow {
                                scope: Some(_),
                            })),
                        }
                    ))
                );
                if let Some(ask) = shared.answer(emit, &id, &answer.ask_key) {
                    if for_session {
                        shared.granted_for_session();
                    }
                    emit.effect(Effect::ProviderWrite(
                        format!(r#"{{"type":"answer","ask":"{}"}}"#, ask.key).into_bytes(),
                    ));
                    shared.accept(emit, &id, false);
                }
            }
            _ => shared.reject(emit, &id, Refusal::Unsupported),
        }
    }
}

impl<const FORGET: bool> TestKind<FORGET> {
    /// A person's answer to a running subagent's ask; anything else, or
    /// any input to a subagent not running, is refused.
    fn child_input(state: &mut State<FORGET>, emit: &mut Emit, agent_id: &[u8], input: Input) {
        let shared = &mut state.shared;
        let id = input.input_id.clone();
        let answer = match input.decode_body() {
            Some(wire::KindInput::ClaudeSdk(wire::ClaudeSdkInput {
                of: Some(claude_sdk_input::Of::Answer(answer)),
            })) => answer,
            _ => return shared.reject(emit, &id, Refusal::Unsupported),
        };
        let Some(child) = shared.subagent_child(agent_id).map(str::to_owned) else {
            return shared.reject(emit, &id, Refusal::Unsupported);
        };
        let mut sub = shared.subagent(emit, &child);
        if let Some(ask) = sub.answer(&id, &answer.ask_key) {
            sub.effect(Effect::ProviderWrite(
                serde_json::to_vec(&serde_json::json!({
                    "type": "answer", "child": child, "ask": ask.key,
                }))
                .expect("json"),
            ));
            sub.accept(&id);
            sub_snapshot(&mut sub);
        }
    }
}

/// A running subagent's snapshot: its phase and its open asks.
fn sub_snapshot(sub: &mut Subagent<'_, Ask>) {
    let body = ClaudeSnapshot {
        asks: sub.asks().open_asks().to_vec(),
        ..unknown::claude()
    }
    .encode_to_vec();
    sub.snapshot(Snapshot {
        phase: sub.phase() as i32,
        body,
        ..Default::default()
    });
}

fn write(text: &str) -> Effect {
    Effect::ProviderWrite(
        serde_json::to_vec(&serde_json::json!({ "type": "user", "text": text })).expect("json"),
    )
}

impl<const FORGET: bool> Interpreter for TestKind<FORGET> {
    type State = State<FORGET>;
    const KIND: wire::Kind = wire::Kind::Unspecified;

    fn initial(spec: &AgentSpec) -> (Self::State, Step) {
        let mut state = State {
            shared: Shared::new(spec, Self::KIND),
            model: None,
            tools: BTreeMap::new(),
            interrupted: false,
            jobs: BTreeSet::new(),
            wake: None,
            housekeep_after: None,
            host: spec.agent_id.clone(),
            parts: Vec::new(),
        };
        let step = state.shared.initial_step(Self::unknown_snapshot());
        (state, step)
    }

    fn reincarnate(mut state: Self::State, spec: &AgentSpec) -> (Self::State, Step) {
        state.shared.reincarnate(spec);
        state.jobs.clear();
        state.host = spec.agent_id.clone();
        state.resume()
    }

    fn step(state: &mut Self::State, event: Event) -> Stepped {
        let mut emit = Emit::default();
        match event {
            Event::Tick { at_ms } => {
                state.shared.tick(at_ms);
                if state.wake.is_some_and(|wake| wake <= at_ms) {
                    state.wake = None;
                    state.shared.item(
                        &mut emit,
                        ItemDraft {
                            key: format!("woke:{at_ms}"),
                            body: body(claude_item::Kind::Prompt(wire::Prompt {})),
                            complete: true,
                            ..Default::default()
                        },
                    );
                }
                if state.housekeeping_at().is_some_and(|at| at <= at_ms) {
                    state.shared.housekeeping_started();
                    emit.effect(Effect::ProviderWrite(br#"{"type":"housekeep"}"#.to_vec()));
                }
            }
            Event::Fact(fact) if fact.channel == Channel::Stream => {
                Self::fact(state, &mut emit, fact)
            }
            Event::Fact(_) => {}
            Event::Input(input) => Self::input(state, &mut emit, input),
            Event::ChildInput { agent_id, input } => {
                Self::child_input(state, &mut emit, &agent_id, input)
            }
            Event::AgentMessage(envelope) => Self::agent_message(state, &mut emit, envelope),
            Event::Rename { .. } => state.shared.renamed(),
            Event::ProviderExit { code } => {
                // A session end closes every open ask, outcome unknown.
                state.shared.close_all_asks();
                let end = crate::provider_exit(code);
                state.shared.provider_exited_as(&end);
                state.jobs.clear();
                emit.end_incarnation(end);
            }
            Event::Exiting(end) => {
                state.shared.close_all_asks();
                state.shared.provider_exited_as(&end);
                state.jobs.clear();
                emit.end_incarnation(end);
            }
            Event::DaemonLost => state.shared.leaving(),
            // A stop cancels the turn the provider's way, as every kind
            // does, and the agent drains.
            Event::StopRequested => {
                state.shared.stop_requested();
                if state.shared.is_busy() || !state.shared.asks().is_empty() {
                    state.interrupted = true;
                    emit.effect(Effect::ProviderWrite(br#"{"type":"interrupt"}"#.to_vec()));
                }
            }
            Event::Git(git) => state.shared.set_git(git),
        }
        if let Some(entry) = state.shared.next_queued() {
            emit.effect(write(&entry.text));
        }
        let body = state.body();
        let notice = state.shared.notice(crate::notice::claude_ask);
        let mut stepped = state.shared.finish(emit, body, notice);
        stepped.step.agents.append(&mut state.parts);
        stepped
    }

    fn pending_messages(state: &Self::State) -> Vec<Vec<u8>> {
        state.shared.pending_messages().iter().cloned().collect()
    }

    fn wake_at(state: &Self::State) -> Option<i64> {
        let housekeeping = state
            .housekeeping_at()
            .map(|at| at.max(state.shared.now_ms().saturating_add(1)));
        match (state.wake, housekeeping) {
            (Some(wake), Some(housekeeping)) => Some(wake.min(housekeeping)),
            (wake, housekeeping) => wake.or(housekeeping),
        }
    }

    fn housekeeping(state: &Self::State) -> crate::Housekeeping {
        if state.shared.is_housekeeping() {
            crate::Housekeeping::Running
        } else {
            state
                .housekeeping_at()
                .map_or(crate::Housekeeping::Settled, |at_ms| {
                    crate::Housekeeping::Due { at_ms }
                })
        }
    }

    fn typed_unrecorded(state: &Self::State) -> bool {
        state.shared.typed_unrecorded()
    }

    fn holds(state: &Self::State) -> crate::Holds {
        state.shared.holds()
    }

    fn fact_shape(fact: &crate::Fact) -> crate::shape::FactShape {
        crate::shape::fact(fact)
    }

    fn input_shape(input: &wire::Input) -> crate::shape::InputShape {
        crate::shape::claude_sdk_input(&input.body)
    }

    fn structure(state: &Self::State) -> crate::shape::Structure {
        state.shared.structure(|body| Self::item_shape(body).arm)
    }

    fn unknown_snapshot() -> Vec<u8> {
        unknown::claude().encode_to_vec()
    }

    fn describe_item(body: &[u8]) -> ItemView {
        let item = ClaudeItem::decode(body).unwrap_or_default();
        let (arm, complete, text) = match item.kind {
            Some(claude_item::Kind::Prompt(_)) => ("prompt", true, String::new()),
            Some(claude_item::Kind::Message(text)) => ("message", text.complete, String::new()),
            Some(claude_item::Kind::Tool(tool)) => (
                "tool",
                true,
                format!(
                    "{} {}",
                    tool.name,
                    ToolState::try_from(tool.state).map_or("?", |state| state.as_str_name())
                ),
            ),
            Some(claude_item::Kind::Turn(turn)) => (
                "turn",
                true,
                format!("#{} started={}", turn.turn_id, turn.started_at_ms),
            ),
            Some(claude_item::Kind::AgentMessage(_)) => ("agent_message", true, String::new()),
            other => ("other", true, format!("{other:?}")),
        };
        ItemView {
            arm: arm.into(),
            complete,
            text,
        }
    }

    fn describe_snapshot(body: &[u8]) -> SnapshotView {
        let snapshot = ClaudeSnapshot::decode(body).unwrap_or_default();
        SnapshotView {
            asks: snapshot
                .asks
                .iter()
                .map(|ask| (ask.key.clone(), ask.item_key.clone()))
                .collect(),
            text: format!(
                "asks=[{}] model={}",
                snapshot
                    .asks
                    .iter()
                    .map(|ask| ask.key.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                snapshot.model.as_deref().unwrap_or("?")
            ),
        }
    }

    fn fixture_input(input_id: Vec<u8>, input: &FixtureInput) -> Option<Input> {
        claude_sdk_input(input_id, input)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use wire::Phase;

    use super::*;
    use crate::{EndRules, check_invariants, run_golden};

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/core")
    }

    fn run(name: &str) {
        run_golden::<TestKind>(&fixtures().join(format!("{name}.json"))).assert_ok();
    }

    #[test]
    fn queue_submits_when_idle_and_drains_in_order() {
        run("queue");
    }

    #[test]
    fn asks_open_and_close_only_on_facts() {
        run("asks");
    }

    #[test]
    fn agent_messages_stay_pending_until_consumed() {
        run("agent-messages");
    }

    #[test]
    fn streams_end_full_and_resume_re_emits_open_items() {
        run("streaming");
    }

    #[test]
    fn a_spawn_prompt_seeds_the_queue_and_working_on() {
        run("spawn");
    }

    #[test]
    fn subagents_write_their_parts_into_the_hosts_steps() {
        run("subagents");
    }

    #[test]
    fn the_checkpoint_property_catches_state_a_resume_loses() {
        let dir = std::env::temp_dir().join(format!("interpret-forget-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fixture = dir.join("forget.json");
        std::fs::write(
            &fixture,
            r#"{"events": [{"fact": {"channel": "stream", "json": {"type": "ready"}}},
                           {"fact": {"channel": "stream", "json": {"type": "model", "model": "opus"}}},
                           {"tick": 5}]}"#,
        )
        .unwrap();
        let report = run_golden::<TestKind<true>>(&fixture);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            report
                .violations
                .iter()
                .any(|violation| violation.contains("diverges: the final snapshot differs")),
            "{:?}",
            report.violations
        );
    }

    #[test]
    fn a_golden_mismatch_fails_and_names_the_line() {
        let dir = std::env::temp_dir().join(format!("interpret-golden-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fixture = dir.join("tiny.json");
        std::fs::write(&fixture, r#"{"events": [{"tick": 5}]}"#).unwrap();
        std::fs::write(dir.join("tiny.golden"), "## 0 initial\nsomething else\n").unwrap();
        let report = run_golden::<TestKind>(&fixture);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(report.violations.is_empty(), "{:?}", report.violations);
        assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
        assert!(
            report.failures[0].contains("line 2"),
            "{}",
            report.failures[0]
        );
    }

    #[test]
    fn an_unmet_expectation_fails() {
        let dir = std::env::temp_dir().join(format!("interpret-expect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fixture = dir.join("expect.json");
        std::fs::write(
            &fixture,
            r#"{"events": [{"fact": {"channel": "stream", "json": {"type": "ready"}},
                            "expect": {"phase": "WORKING"}}]}"#,
        )
        .unwrap();
        let report = run_golden::<TestKind>(&fixture);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            report
                .failures
                .iter()
                .any(|failure| failure.contains("phase expected WORKING, got IDLE")),
            "{:?}",
            report.failures
        );
    }

    /// What the shared core does with what an interpreter writes for its
    /// provider subagents.
    mod subagents {
        use wire::{AgentStep, IncarnationEnd, Snapshot};

        use super::*;
        use crate::{Shared, checks, subagent_id};

        fn shared() -> Shared<Ask> {
            Shared::new(
                &AgentSpec {
                    agent_id: vec![1; 16],
                    ..Default::default()
                },
                wire::Kind::Unspecified,
            )
        }

        fn start(call: &str) -> NativeStart {
            NativeStart {
                call_agent_id: vec![1; 16],
                call_key: call.into(),
                title: "look".into(),
                ..Default::default()
            }
        }

        fn message(key: &str, complete: bool) -> ItemDraft {
            ItemDraft {
                key: key.into(),
                body: body(claude_item::Kind::Message(wire::Text { complete })),
                complete,
                ..Default::default()
            }
        }

        fn ids(step: &Step) -> Vec<Vec<u8>> {
            step.agents
                .iter()
                .map(|part| part.agent_id.clone())
                .collect()
        }

        #[test]
        fn parts_share_the_hosts_frame_one_per_subagent_in_first_touch_order() {
            let mut shared = shared();
            let mut emit = Emit::default();
            shared.item(&mut emit, message("call", true));
            shared.subagent(&mut emit, "b").start(start("call"));
            shared.subagent(&mut emit, "a").start(start("call"));
            shared.subagent(&mut emit, "b").item(message("m", false));
            assert!(shared.subagent(&mut emit, "b").append("m", "hi"));
            let stepped = shared.finish(emit, Vec::new(), None);
            assert_eq!(stepped.broken, None);
            let step = stepped.step;
            assert_eq!(step.items.len(), 1, "the host's own part is as ever");
            assert_eq!(step.items[0].key, "call");
            let host = [1u8; 16];
            assert_eq!(
                ids(&step),
                [subagent_id(&host, "b"), subagent_id(&host, "a")]
            );
            let b = &step.agents[0];
            assert_eq!(b.start.as_ref().unwrap().provider_child_id, "b");
            assert_eq!(b.items.len(), 1);
            assert_eq!(
                b.items[0].text, "hi",
                "an append in the item's own step joins it"
            );
            assert_eq!(b.items[0].kind, wire::Kind::Unspecified as i32);
        }

        #[test]
        fn a_part_outside_a_subagents_run_breaks_its_check() {
            let mut shared = shared();
            let mut emit = Emit::default();
            shared.subagent(&mut emit, "a").item(message("m", true));
            let stepped = shared.finish(emit, Vec::new(), None);
            assert_eq!(
                stepped.broken.map(|broken| broken.invariant.id),
                Some(checks::SUBAGENT_RUN.id)
            );
            // Nothing is kept for a subagent that never started.
            assert_eq!(shared.running_subagents().count(), 0);

            let mut emit = Emit::default();
            let mut sub = shared.subagent(&mut emit, "a");
            sub.start(start("call"));
            sub.item(message("n", false));
            sub.end(IncarnationEnd::default());
            let stepped = shared.finish(emit, Vec::new(), None);
            assert_eq!(
                stepped.broken.map(|broken| broken.detail),
                Some("subagent a ended with 1 items still streaming".into())
            );
        }

        #[test]
        fn an_end_then_a_new_start_in_one_step_are_two_parts_in_order() {
            let mut shared = shared();
            let mut emit = Emit::default();
            let mut sub = shared.subagent(&mut emit, "a");
            sub.start(start("call"));
            sub.end(IncarnationEnd::default());
            shared.subagent(&mut emit, "a").start(start("again"));
            let step = shared.finish(emit, Vec::new(), None).step;
            assert_eq!(step.agents.len(), 2);
            assert!(step.agents[0].end.is_some());
            assert_eq!(step.agents[1].start.as_ref().unwrap().call_key, "again");
            assert_eq!(step.agents[1].end, None);
        }

        #[test]
        fn the_incarnations_end_ends_every_subagent_still_running() {
            let mut shared = shared();
            let mut emit = Emit::default();
            shared.subagent(&mut emit, "a").start(start("call"));
            shared.subagent(&mut emit, "b").start(start("call"));
            shared.finish(emit, Vec::new(), None);
            let mut emit = Emit::default();
            let end = IncarnationEnd {
                cause: wire::ExitCause::Stopped as i32,
                detail: None,
            };
            emit.end_incarnation(end.clone());
            let step = shared.finish(emit, Vec::new(), None).step;
            assert_eq!(step.agents.len(), 2);
            assert!(step.agents.iter().all(|part| part.end == Some(end.clone())));
            assert_eq!(shared.running_subagents().count(), 0);
        }

        #[test]
        fn a_snapshot_is_carried_only_when_it_changed_and_a_resume_re_emits_it() {
            let mut shared = shared();
            let mut emit = Emit::default();
            let mut sub = shared.subagent(&mut emit, "a");
            sub.start(start("call"));
            sub.item(message("m", false));
            let snapshot = Snapshot {
                phase: sub.phase() as i32,
                ..Default::default()
            };
            sub.snapshot(snapshot.clone());
            shared.finish(emit, Vec::new(), None);
            shared.tick(5000);
            let mut emit = Emit::default();
            shared.subagent(&mut emit, "a").snapshot(snapshot);
            assert!(shared.finish(emit, Vec::new(), None).step.agents.is_empty());

            let resumed = shared.resume_step(Vec::new());
            assert_eq!(
                resumed.agents,
                [AgentStep {
                    agent_id: shared.subagent_id("a"),
                    items: vec![resumed.agents[0].items[0].clone()],
                    snapshot: resumed.agents[0].snapshot.clone(),
                    ..Default::default()
                }]
            );
            assert_eq!(resumed.agents[0].items[0].key, "m");
            assert!(resumed.agents[0].snapshot.is_some());
        }

        /// A checkpoint written before subagents were agents of their own
        /// has no record of them and reads as none; one with none writes
        /// none, so its bytes are as they were.
        #[test]
        fn a_checkpoint_without_subagents_reads_and_writes_as_before() {
            let shared = shared();
            let bytes = serde_json::to_value(&shared).unwrap();
            assert!(bytes.get("subagents").is_none(), "{bytes}");
            let read: Shared<Ask> = serde_json::from_value(bytes).unwrap();
            assert_eq!(read, shared);

            let mut running = shared.clone();
            let mut emit = Emit::default();
            running.subagent(&mut emit, "a").start(start("call"));
            running.finish(emit, Vec::new(), None);
            let again: Shared<Ask> =
                serde_json::from_value(serde_json::to_value(&running).unwrap()).unwrap();
            assert_eq!(again.running_subagents().collect::<Vec<_>>(), ["a"]);
            assert_eq!(again.subagent_child(&running.subagent_id("a")), Some("a"));
        }
    }

    mod invariants {
        use wire::{Append, Item, QueuedInput, Snapshot};

        use super::*;

        fn first() -> Step {
            Step {
                snapshot: Some(Snapshot {
                    kind: wire::Kind::Unspecified as i32,
                    body: TestKind::<false>::unknown_snapshot(),
                    phase: Phase::Starting as i32,
                    ..Default::default()
                }),
                ..Default::default()
            }
        }

        fn item(key: &str, kind: claude_item::Kind) -> Item {
            Item {
                key: key.into(),
                kind: wire::Kind::Unspecified as i32,
                body: body(kind),
                ..Default::default()
            }
        }

        fn streaming(key: &str, complete: bool) -> Item {
            item(key, claude_item::Kind::Message(wire::Text { complete }))
        }

        fn append(key: &str) -> Append {
            Append {
                key: key.into(),
                text: "x".into(),
                ..Default::default()
            }
        }

        fn check(steps: &[Step]) -> Vec<String> {
            check_invariants::<TestKind>(steps, &EndRules::default())
        }

        #[test]
        fn a_clean_run_has_no_violations() {
            let steps = [
                first(),
                Step {
                    items: vec![streaming("m", false)],
                    appends: vec![append("m")],
                    ..Default::default()
                },
                Step {
                    items: vec![streaming("m", true)],
                    ..Default::default()
                },
            ];
            assert_eq!(check(&steps), Vec::<String>::new());
        }

        #[test]
        fn the_first_frame_must_be_a_starting_snapshot_at_its_unknowns() {
            assert!(check(&[Step::default()])[0].contains("not a Snapshot"));
            let mut known = first();
            known.snapshot.as_mut().unwrap().body = ClaudeSnapshot {
                model: Some("opus".into()),
                ..unknown::claude()
            }
            .encode_to_vec();
            assert!(check(&[known])[0].contains("not every field at its unknown"));
            let mut idle = first();
            idle.snapshot.as_mut().unwrap().phase = Phase::Idle as i32;
            assert!(check(&[idle])[0].contains("not STARTING"));
        }

        #[test]
        fn keys_are_unique_in_a_step_and_never_change_arm() {
            let twice = Step {
                items: vec![streaming("k", true), streaming("k", true)],
                ..Default::default()
            };
            assert!(check(&[first(), twice])[0].contains("twice in one step"));
            let changed = [
                first(),
                Step {
                    items: vec![streaming("k", true)],
                    ..Default::default()
                },
                Step {
                    items: vec![item("k", claude_item::Kind::Prompt(wire::Prompt {}))],
                    ..Default::default()
                },
            ];
            assert!(check(&changed)[0].contains("changed from message to prompt"));
        }

        #[test]
        fn appends_go_to_any_open_item_and_never_after_its_final() {
            let after_final = [
                first(),
                Step {
                    items: vec![streaming("m", true)],
                    ..Default::default()
                },
                Step {
                    appends: vec![append("m")],
                    ..Default::default()
                },
            ];
            assert!(check(&after_final)[0].contains("after its final item"));
            let below_the_newest = [
                first(),
                Step {
                    items: vec![streaming("a", false), streaming("b", true)],
                    appends: vec![append("a")],
                    ..Default::default()
                },
                Step {
                    items: vec![streaming("a", true)],
                    ..Default::default()
                },
            ];
            assert_eq!(check(&below_the_newest), Vec::<String>::new());
            let unknown_key = [
                first(),
                Step {
                    appends: vec![append("ghost")],
                    ..Default::default()
                },
            ];
            assert!(check(&unknown_key)[0].contains("unknown key"));
        }

        #[test]
        fn every_open_ask_has_an_item() {
            let snapshot = Snapshot {
                body: ClaudeSnapshot {
                    asks: vec![Ask {
                        key: "ask".into(),
                        item_key: "tool-1".into(),
                        ..Default::default()
                    }],
                    ..unknown::claude()
                }
                .encode_to_vec(),
                phase: Phase::NeedsYou as i32,
                ..Default::default()
            };
            let steps = [
                first(),
                Step {
                    snapshot: Some(snapshot),
                    ..Default::default()
                },
            ];
            assert!(check(&steps)[0].contains("has no item"));
        }

        #[test]
        fn the_queue_drains_and_streams_end_unless_the_fixture_says_otherwise() {
            let left = [
                first(),
                Step {
                    items: vec![streaming("m", false)],
                    snapshot: Some(Snapshot {
                        queue: vec![QueuedInput {
                            input_id: b"p1".to_vec(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ];
            let violations = check(&left);
            assert!(
                violations[0].contains("did not drain: p1"),
                "{violations:?}"
            );
            assert!(violations[1].contains("never ended"), "{violations:?}");
            let allowed = EndRules {
                queue_may_remain: true,
                streams_may_remain: true,
            };
            assert!(check_invariants::<TestKind>(&left, &allowed).is_empty());
        }

        fn part(agent: u8, f: impl FnOnce(&mut wire::AgentStep)) -> wire::AgentStep {
            let mut part = wire::AgentStep {
                agent_id: vec![agent; 16],
                ..Default::default()
            };
            f(&mut part);
            part
        }

        fn started(part: &mut wire::AgentStep) {
            part.start = Some(wire::NativeStart {
                provider_child_id: "c".into(),
                ..Default::default()
            });
        }

        fn ended(part: &mut wire::AgentStep) {
            part.end = Some(wire::IncarnationEnd::default());
        }

        fn with_parts(parts: Vec<wire::AgentStep>) -> Step {
            Step {
                agents: parts,
                ..Default::default()
            }
        }

        #[test]
        fn a_subagents_keys_are_its_own() {
            let steps = [
                first(),
                Step {
                    items: vec![streaming("m", true)],
                    agents: vec![part(9, |part| {
                        started(part);
                        part.items = vec![streaming("m", false)];
                        part.appends = vec![append("m")];
                    })],
                    ..Default::default()
                },
                with_parts(vec![part(9, |part| {
                    part.items = vec![streaming("m", true)];
                    ended(part);
                })]),
            ];
            assert_eq!(check(&steps), Vec::<String>::new());
        }

        #[test]
        fn a_subagents_first_part_has_a_start() {
            let steps = [
                first(),
                with_parts(vec![part(9, |part| {
                    part.items = vec![streaming("m", true)]
                })]),
            ];
            let violations = check(&steps);
            assert!(
                violations[0].contains("subagent sub-09090909 has a part before its start"),
                "{violations:?}"
            );
        }

        #[test]
        fn nothing_follows_a_subagents_end_but_a_new_start() {
            let after = [
                first(),
                with_parts(vec![part(9, |part| {
                    started(part);
                    ended(part);
                })]),
                with_parts(vec![part(9, |part| {
                    part.items = vec![streaming("m", true)]
                })]),
            ];
            let violations = check(&after);
            assert!(violations[0].contains("after its end"), "{violations:?}");
            let continued = [
                first(),
                with_parts(vec![part(9, |part| {
                    started(part);
                    ended(part);
                })]),
                with_parts(vec![part(9, |part| {
                    started(part);
                    part.items = vec![streaming("m", true)];
                })]),
            ];
            assert_eq!(check(&continued), Vec::<String>::new());
        }

        #[test]
        fn a_subagent_ends_after_its_last_item_ends() {
            let steps = [
                first(),
                with_parts(vec![part(9, |part| {
                    started(part);
                    part.items = vec![streaming("m", false)];
                })]),
                with_parts(vec![part(9, ended)]),
            ];
            let violations = check(&steps);
            assert!(
                violations[0].contains("ended with streams that never ended: m"),
                "{violations:?}"
            );
            let running = [
                first(),
                with_parts(vec![part(9, |part| {
                    started(part);
                    part.items = vec![streaming("m", false)];
                })]),
            ];
            let violations = check(&running);
            assert!(
                violations[0].contains("subagent sub-09090909: streams never ended"),
                "{violations:?}"
            );
        }

        #[test]
        fn a_reopened_final_item_is_a_violation() {
            let steps = [
                first(),
                Step {
                    items: vec![streaming("m", true)],
                    ..Default::default()
                },
                Step {
                    items: vec![streaming("m", false)],
                    ..Default::default()
                },
                Step {
                    items: vec![streaming("m", true)],
                    ..Default::default()
                },
            ];
            assert!(check(&steps)[0].contains("reopened"));
        }
    }
}
