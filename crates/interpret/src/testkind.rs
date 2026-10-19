//! A minimal interpreter over an authored fact vocabulary, used to exercise
//! the shared core and the golden harness without any provider's format.
//!
//! Facts arrive on the stream channel as `{"type": …}`: `ready`, `user`
//! (a prompt's reflection, or `envelope` for an injected message's),
//! `text`/`text_done` (a streamed message), `tool`/`tool_result`,
//! `ask`/`ask_closed`, `turn_start`, `turn_end` and `model`. It writes
//! `user`, `answer` and, on a stop while a turn runs or an ask is open,
//! `interrupt`. Bodies are the SDK's messages under the kind tag `test`.

use std::collections::BTreeMap;

use prost::Message as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wire::{
    AgentSpec, Ask, ClaudeSdkItem, ClaudeSdkSnapshot, Envelope, Input, PermissionAsk, Refusal,
    Step, ToolCall, ToolState, Turn, TurnOutcome, claude_sdk_input, claude_sdk_item,
};

use crate::{
    Carrier, Channel, Checkpoint, Effect, Emit, Event, Fact, FixtureInput, Interpreter, ItemDraft,
    ItemView, RedactTarget, Shared, SnapshotView, Stepped, agent_message_body, agent_message_key,
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
}

impl<const FORGET: bool> State<FORGET> {
    fn body(&self) -> Vec<u8> {
        ClaudeSdkSnapshot {
            asks: self.shared.asks().open_asks().to_vec(),
            model: self.model.clone(),
            ..unknown::claude_sdk()
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

fn body(kind: claude_sdk_item::Kind) -> Vec<u8> {
    ClaudeSdkItem { kind: Some(kind) }.encode_to_vec()
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
                        body: body(claude_sdk_item::Kind::Prompt(wire::Prompt {})),
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
                            body: body(claude_sdk_item::Kind::Message(wire::Text {
                                complete: false,
                            })),
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
                        body: body(claude_sdk_item::Kind::Message(wire::Text {
                            complete: true,
                        })),
                        complete: true,
                        ..Default::default()
                    },
                );
                shared.note_message(&id);
            }
            "tool" => {
                let name = text_of(&value, "name");
                let server = text_of(&value, "server");
                let arguments = value.get("input").cloned().unwrap_or(Value::Null);
                let arguments = serde_json::to_vec(&arguments).expect("json");
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
                        body: body(claude_sdk_item::Kind::Tool(ToolCall {
                            name,
                            server,
                            input_json: arguments,
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
                        body: body(claude_sdk_item::Kind::Tool(ToolCall {
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
                            body: body(claude_sdk_item::Kind::Turn(Turn {
                                turn_id: turn.id,
                                outcome: outcome as i32,
                                started_at_ms: turn.started_at_ms,
                                cost_usd: None,
                            })),
                            complete: true,
                            ..Default::default()
                        },
                    );
                }
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
                body: body(claude_sdk_item::Kind::AgentMessage(agent_message_body(
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
                if let Some(ask) = shared.answer(emit, &id, &answer.ask_key) {
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

fn write(text: &str) -> Effect {
    Effect::ProviderWrite(
        serde_json::to_vec(&serde_json::json!({ "type": "user", "text": text })).expect("json"),
    )
}

impl<const FORGET: bool> Interpreter for TestKind<FORGET> {
    type State = State<FORGET>;
    const KIND: wire::Kind = wire::Kind::Unspecified;

    fn initial(spec: &AgentSpec, producer_version: &str) -> (Self::State, Step) {
        let mut state = State {
            shared: Shared::new(spec, Self::KIND, producer_version),
            model: None,
            tools: BTreeMap::new(),
            interrupted: false,
        };
        let step = state.shared.initial_step(Self::unknown_snapshot());
        (state, step)
    }

    fn reincarnate(
        mut state: Self::State,
        spec: &AgentSpec,
        producer_version: &str,
    ) -> (Self::State, Step) {
        state.shared.reincarnate(spec, producer_version);
        state.resume()
    }

    fn step(state: &mut Self::State, event: Event) -> Stepped {
        let mut emit = Emit::default();
        match event {
            Event::Tick { at_ms } => state.shared.tick(at_ms),
            Event::Fact(fact) if fact.channel == Channel::Stream => {
                Self::fact(state, &mut emit, fact)
            }
            Event::Fact(_) => {}
            Event::Input(input) => Self::input(state, &mut emit, input),
            Event::AgentMessage(envelope) => Self::agent_message(state, &mut emit, envelope),
            Event::Rename { .. } => {}
            Event::ProviderExit { code } => {
                // A session end closes every open ask, outcome unknown.
                state.shared.close_all_asks();
                state.shared.provider_exited();
                emit.end_incarnation(crate::provider_exit(code));
            }
            Event::Exiting(end) => {
                state.shared.close_all_asks();
                state.shared.provider_exited();
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
        state.shared.finish(emit, body)
    }

    fn redact(target: RedactTarget) -> RedactTarget {
        target
    }

    fn pending_messages(state: &Self::State) -> Vec<Vec<u8>> {
        state.shared.pending_messages().iter().cloned().collect()
    }

    fn typed_unrecorded(state: &Self::State) -> bool {
        state.shared.typed_unrecorded()
    }

    fn unknown_snapshot() -> Vec<u8> {
        unknown::claude_sdk().encode_to_vec()
    }

    fn describe_item(body: &[u8]) -> ItemView {
        let item = ClaudeSdkItem::decode(body).unwrap_or_default();
        let (arm, complete, text) = match item.kind {
            Some(claude_sdk_item::Kind::Prompt(_)) => ("prompt", true, String::new()),
            Some(claude_sdk_item::Kind::Message(text)) => ("message", text.complete, String::new()),
            Some(claude_sdk_item::Kind::Tool(tool)) => (
                "tool",
                true,
                format!(
                    "{} {}",
                    tool.name,
                    ToolState::try_from(tool.state).map_or("?", |state| state.as_str_name())
                ),
            ),
            Some(claude_sdk_item::Kind::Turn(turn)) => (
                "turn",
                true,
                format!("#{} started={}", turn.turn_id, turn.started_at_ms),
            ),
            Some(claude_sdk_item::Kind::AgentMessage(_)) => ("agent_message", true, String::new()),
            other => ("other", true, format!("{other:?}")),
        };
        ItemView {
            arm: arm.into(),
            complete,
            text,
        }
    }

    fn describe_snapshot(body: &[u8]) -> SnapshotView {
        let snapshot = ClaudeSdkSnapshot::decode(body).unwrap_or_default();
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

    fn stream(json: &str) -> Event {
        Event::Fact(Fact {
            channel: Channel::Stream,
            payload: json.as_bytes().to_vec(),
        })
    }

    fn started() -> State<false> {
        let (mut state, _) = TestKind::<false>::initial(&AgentSpec::default(), "1.0.0");
        TestKind::step(&mut state, stream(r#"{"type":"ready"}"#));
        state
    }

    #[test]
    fn a_stop_mid_turn_interrupts_the_provider_and_the_turn_ends_stopped() {
        let mut state = started();
        TestKind::step(&mut state, stream(r#"{"type":"turn_start"}"#));
        let stopped = TestKind::step(&mut state, Event::StopRequested);
        assert_eq!(
            stopped.effects,
            [Effect::ProviderWrite(br#"{"type":"interrupt"}"#.to_vec())]
        );

        let ended = TestKind::step(&mut state, stream(r#"{"type":"turn_end"}"#));
        assert_eq!(
            ended.step.turn_end.map(|end| end.outcome),
            Some(TurnOutcome::Stopped as i32)
        );
        let turns: Vec<i32> = ended
            .step
            .items
            .iter()
            .filter_map(|item| match ClaudeSdkItem::decode(item.body.as_slice()) {
                Ok(ClaudeSdkItem {
                    kind: Some(claude_sdk_item::Kind::Turn(turn)),
                }) => Some(turn.outcome),
                _ => None,
            })
            .collect();
        assert_eq!(turns, [TurnOutcome::Stopped as i32]);
    }

    /// An idle provider runs a handed agent message as a turn at once, so
    /// a stop that arrives before the provider says its turn started still
    /// finds one to cancel.
    #[test]
    fn a_stop_after_a_message_handed_to_an_idle_provider_interrupts_its_turn() {
        let mut state = started();
        let handed = TestKind::step(
            &mut state,
            Event::AgentMessage(Envelope {
                id: b"env-1".to_vec(),
                body: Some(wire::envelope::Body::Message(wire::MessageBody {
                    text: "look at the build".into(),
                    ..Default::default()
                })),
                ..Default::default()
            }),
        );
        assert!(
            handed
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::Inject { .. })),
            "{:?}",
            handed.effects
        );
        let stopped = TestKind::step(&mut state, Event::StopRequested);
        assert_eq!(
            stopped.effects,
            [Effect::ProviderWrite(br#"{"type":"interrupt"}"#.to_vec())]
        );

        TestKind::step(&mut state, stream(r#"{"type":"turn_start"}"#));
        let ended = TestKind::step(&mut state, stream(r#"{"type":"turn_end"}"#));
        assert_eq!(
            ended.step.turn_end.map(|end| (end.turn_id, end.outcome)),
            Some((1, TurnOutcome::Stopped as i32)),
            "the provider's turn start joins the turn the hand-off began"
        );
    }

    #[test]
    fn a_stop_with_no_turn_running_writes_nothing() {
        let mut state = started();
        let stopped = TestKind::step(&mut state, Event::StopRequested);
        assert_eq!(stopped.effects, []);
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

        fn item(key: &str, kind: claude_sdk_item::Kind) -> Item {
            Item {
                key: key.into(),
                kind: wire::Kind::Unspecified as i32,
                body: body(kind),
                ..Default::default()
            }
        }

        fn streaming(key: &str, complete: bool) -> Item {
            item(key, claude_sdk_item::Kind::Message(wire::Text { complete }))
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
            known.snapshot.as_mut().unwrap().body = ClaudeSdkSnapshot {
                model: Some("opus".into()),
                ..unknown::claude_sdk()
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
                    items: vec![item("k", claude_sdk_item::Kind::Prompt(wire::Prompt {}))],
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
                body: ClaudeSdkSnapshot {
                    asks: vec![Ask {
                        key: "ask".into(),
                        item_key: "tool-1".into(),
                        ..Default::default()
                    }],
                    ..unknown::claude_sdk()
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
