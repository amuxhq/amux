use interpret::testkind::TestKind;
use interpret::{Channel, Fact, FixtureInput};
use serde_json::json;
use wire::{
    AgentRef, AgentSpec, Bootstrap, CtlFrame, PromptInput, PromptSubmission, bootstrap, ctl_frame,
};

use super::*;

type Kind = TestKind;

const DAEMON: ConnId = ConnId(1);
const GRACE_MS: u64 = 60_000;
const DRAIN_MS: u64 = 30_000;

/// An agent under test: the core, the clock it was last told, and every
/// output it has asked for.
struct Run {
    agent: Agent<Kind>,
    now_ms: u64,
    out: Vec<Output>,
}

fn spec(one_shot: bool, prompt: Option<&str>) -> AgentSpec {
    AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 1,
        parent: one_shot.then(AgentRef::default),
        config: Some(wire::EffectiveConfig {
            grace_ms: GRACE_MS as u32,
            drain_ms: DRAIN_MS as u32,
            // Outputs here are a function of the inputs alone; the budget
            // has a test of its own.
            step_budget_micros: StepBudget::UNLIMITED.micros,
            ..Default::default()
        }),
        bootstrap: prompt.map(|text| Bootstrap {
            of: Some(bootstrap::Of::Prompt(PromptSubmission {
                input_id: b"first".to_vec(),
                prompt: Some(PromptInput {
                    text: text.to_owned(),
                    ..Default::default()
                }),
            })),
        }),
        ..Default::default()
    }
}

impl Run {
    fn start(spec: &AgentSpec) -> Self {
        Self::resume(spec, None)
    }

    fn resume(spec: &AgentSpec, resumed: Option<Resumed<<Kind as Interpreter>::State>>) -> Self {
        let mut out = Vec::new();
        let agent = Agent::<Kind>::start(spec, resumed, "0.0.0", 1_000, &mut out);
        Self {
            agent,
            now_ms: 1_000,
            out,
        }
    }

    /// The clock moves to `now_ms`, then `input` happens; the outputs it
    /// asked for.
    fn at(&mut self, now_ms: u64, input: Input) -> Vec<Output> {
        self.now_ms = now_ms;
        let mut out = Vec::new();
        self.agent.step(Input::Tick { now_ms }, &mut out);
        self.agent.step(input, &mut out);
        self.out.extend(out.iter().cloned());
        out
    }

    fn on(&mut self, input: Input) -> Vec<Output> {
        self.at(self.now_ms + 1, input)
    }

    fn fact(&mut self, fact: serde_json::Value) -> Vec<Output> {
        self.on(Input::Provider(Fact {
            channel: Channel::Stream,
            payload: serde_json::to_vec(&fact).expect("json"),
        }))
    }

    fn frame(&mut self, of: ctl_frame::Of) -> Vec<Output> {
        self.on(Input::Control {
            connection: DAEMON,
            frame: CtlFrame { of: Some(of) },
        })
    }

    fn prompt(&mut self, id: &str, text: &str) -> Vec<Output> {
        let input = interpret::claude_sdk_input(
            id.as_bytes().to_vec(),
            &FixtureInput::Prompt {
                text: text.to_owned(),
            },
        )
        .expect("the test kind takes prompts");
        self.frame(ctl_frame::Of::Input(input))
    }

    /// The provider records the prompt it was handed and runs a turn on
    /// it.
    fn reflect(&mut self, key: &str) -> Vec<Output> {
        self.fact(json!({"type": "user", "id": key, "text": key}))
    }

    fn turn_end(&mut self) -> Vec<Output> {
        self.fact(json!({"type": "turn_end"}))
    }
}

fn exit_of(out: &[Output]) -> Option<&ExitCause> {
    out.iter().find_map(|output| match output {
        Output::Exit(cause) => Some(cause),
        _ => None,
    })
}

fn closes(out: &[Output]) -> bool {
    out.contains(&Output::CloseProvider)
}

fn provider_writes(out: &[Output]) -> Vec<String> {
    out.iter()
        .filter_map(|output| match output {
            Output::WriteProvider(Effect::ProviderWrite(bytes)) => {
                Some(String::from_utf8_lossy(bytes).into_owned())
            }
            _ => None,
        })
        .collect()
}

fn verdicts(out: &[Output]) -> Vec<(Vec<u8>, wire::SendInputResponse)> {
    out.iter()
        .filter_map(|output| match output {
            Output::WriteControl {
                frame:
                    CtlFrame {
                        of: Some(ctl_frame::Of::Reply(verdict)),
                    },
                ..
            } => Some((
                verdict.id.clone(),
                verdict.verdict.clone().unwrap_or_default(),
            )),
            _ => None,
        })
        .collect()
}

fn dumps(out: &[Output]) -> Vec<&DumpReason> {
    out.iter()
        .filter_map(|output| match output {
            Output::Dump { reason } => Some(reason),
            _ => None,
        })
        .collect()
}

/// The provider's stop deadline lets it finish; its exit ends the process
/// with the cause the lifecycle decided.
fn provider_exits(run: &mut Run) -> Option<ExitCause> {
    exit_of(&run.on(Input::ProviderExited { code: Some(0) })).cloned()
}

/// A one-shot agent started on its spawn prompt, with its daemon connected
/// and its provider running that prompt as a turn.
fn one_shot_working() -> Run {
    let mut run = Run::start(&spec(true, Some("do the thing")));
    run.on(Input::DaemonDials { connection: DAEMON });
    let out = run.fact(json!({"type": "ready"}));
    assert_eq!(
        provider_writes(&out),
        vec![r#"{"text":"do the thing","type":"user"}"#]
    );
    run.reflect("p1");
    run
}

/// A step is timed against the spec's budget: any step takes longer than
/// nothing, given a clock fine enough, so under a budget of one
/// microsecond (zero is the agent's own) a step is soon measured over it.
#[test]
fn a_step_over_its_budget_asks_for_a_dump() {
    let mut over = spec(false, None);
    over.config.as_mut().unwrap().step_budget_micros = 1;
    let mut run = Run::start(&over);
    run.on(Input::DaemonDials { connection: DAEMON });
    let mut out = Vec::new();
    while out.is_empty() {
        out = dumps(&run.fact(json!({"type": "ready"})))
            .into_iter()
            .cloned()
            .collect();
    }
    assert!(
        out.iter().all(|reason| matches!(
            reason,
            DumpReason::OverBudget {
                micros,
                budget: StepBudget { micros: 1 }
            } if *micros > 1
        )),
        "{out:?}"
    );
}

/// A spec that names no budget runs under the agent's own.
#[test]
fn a_spec_without_a_budget_runs_under_the_agents_own() {
    assert_eq!(Params::of(&AgentSpec::default()).budget, StepBudget::AGENT);
}

#[test]
fn a_stop_with_no_turn_running_exits_at_once() {
    let mut run = Run::start(&spec(false, None));
    run.on(Input::DaemonDials { connection: DAEMON });
    run.fact(json!({"type": "ready"}));
    let out = run.frame(ctl_frame::Of::Stop(wire::Stop { kill: false }));
    assert!(out.contains(&Output::Record(Entry::Stop)));
    assert!(closes(&out));
    assert_eq!(provider_exits(&mut run), Some(ExitCause::Stopped));
}

/// An idle provider runs a handed agent message as a turn at once, so a
/// stop that arrives before the provider says the turn started cancels
/// that turn, and the agent exits only as the turn ends.
#[test]
fn a_stop_right_after_an_injected_message_exits_stopped_at_the_turns_end() {
    let mut run = Run::start(&spec(false, None));
    run.on(Input::DaemonDials { connection: DAEMON });
    run.fact(json!({"type": "ready"}));
    run.frame(ctl_frame::Of::AgentMessage(wire::Envelope {
        id: b"env-1".to_vec(),
        body: Some(wire::envelope::Body::Message(wire::MessageBody {
            text: "look at the build".into(),
            ..Default::default()
        })),
        ..Default::default()
    }));
    let out = run.frame(ctl_frame::Of::Stop(wire::Stop { kill: false }));
    assert!(out.contains(&Output::Record(Entry::Stop)));
    assert_eq!(provider_writes(&out), vec![r#"{"type":"interrupt"}"#]);
    assert!(!closes(&out), "the message's turn is still running");
    assert_eq!(run.agent.exited(), None);

    run.fact(json!({"type": "turn_start"}));
    let out = run.turn_end();
    let outcomes: Vec<i32> = out
        .iter()
        .filter_map(|output| match output {
            Output::AppendJournal(step) => step.turn_end.as_ref().map(|end| end.outcome),
            _ => None,
        })
        .collect();
    assert_eq!(outcomes, vec![wire::TurnOutcome::Stopped as i32]);
    assert!(closes(&out));
    assert_eq!(provider_exits(&mut run), Some(ExitCause::Stopped));
}

#[test]
fn a_draining_agent_refuses_new_input() {
    let mut run = Run::start(&spec(false, None));
    run.on(Input::DaemonDials { connection: DAEMON });
    run.fact(json!({"type": "ready"}));
    run.prompt("p1", "hello");
    run.reflect("p1");
    run.frame(ctl_frame::Of::Stop(wire::Stop { kill: false }));
    let out = run.prompt("p2", "more");
    assert_eq!(
        verdicts(&out),
        vec![(b"p2".to_vec(), reply::rejected(wire::Refusal::Draining))]
    );
}

#[test]
fn a_provider_that_will_not_finish_is_killed_at_its_deadline() {
    let mut run = one_shot_working();
    run.turn_end();
    let deadline = run.now_ms + PROVIDER_STOP_MS;
    assert!(run.out.contains(&Output::ArmTimer {
        id: TimerId::ProviderStop,
        at_ms: deadline,
    }));
    let out = run.at(deadline, Input::Timer(TimerId::ProviderStop));
    assert!(out.contains(&Output::KillProvider));
    assert_eq!(exit_of(&out), Some(&ExitCause::Finished));
}

#[test]
fn a_failed_write_ends_the_incarnation_and_a_second_kills_it() {
    let mut run = Run::start(&spec(false, None));
    run.on(Input::DaemonDials { connection: DAEMON });
    let out = run.on(Input::WriteFailed {
        why: "disk full".into(),
    });
    assert!(closes(&out), "the final boundary is tried");
    let out = run.on(Input::WriteFailed {
        why: "disk full".into(),
    });
    assert!(out.contains(&Output::KillProvider));
    assert_eq!(
        exit_of(&out),
        Some(&ExitCause::WriteFailed("disk full".into()))
    );
}

#[test]
fn frames_from_a_replaced_connection_are_ignored() {
    let mut run = Run::start(&spec(false, None));
    run.on(Input::DaemonDials { connection: DAEMON });
    run.on(Input::DaemonDials {
        connection: ConnId(2),
    });
    let out = run.frame(ctl_frame::Of::Stop(wire::Stop { kill: true }));
    assert_eq!(exit_of(&out), None);
    run.on(Input::DaemonGone { connection: DAEMON });
    assert_eq!(run.agent.connection, Some(ConnId(2)));
}

#[test]
fn a_drain_deadline_armed_while_running_is_a_violation() {
    let mut run = Run::start(&spec(false, None));
    run.agent.ask = Some(OpenAsk { until: 0 });
    let mut out = Vec::new();
    run.agent
        .step(Input::DaemonDials { connection: DAEMON }, &mut out);
    let dumps = dumps(&out);
    assert_eq!(dumps.len(), 1, "{out:?}");
    match dumps[0] {
        DumpReason::Violation(violation) => assert_eq!(
            violation.invariant,
            "a drain deadline waits only on an ask left open while draining for a lost daemon"
        ),
        other => panic!("not a violation: {other:?}"),
    }
}

#[test]
fn an_input_taken_while_still_queued_is_a_violation() {
    let mut run = Run::start(&spec(false, None));
    run.on(Input::DaemonDials { connection: DAEMON });
    run.fact(json!({"type": "ready"}));
    run.prompt("p1", "hello");
    // The journal's queue still names the prompt the provider records next.
    run.agent.pending.queued.insert(b"p1".to_vec());
    let out = run.reflect("p1");
    assert!(
        matches!(dumps(&out).as_slice(), [DumpReason::Violation(violation)]
            if violation.invariant == "the pending set never holds an input the journal recorded as taken"),
        "{out:?}"
    );
}

#[test]
fn a_lifecycle_exit_decided_mid_turn_is_a_violation() {
    let mut run = Run::start(&spec(false, None));
    run.agent.touched.decided = Some(Decided {
        cause: ExitCause::DaemonLost,
        turn_ended: false,
        turn_running: true,
        deadline_passed: false,
    });
    assert_eq!(
        run.agent.check_step_invariants().map_err(|v| v.invariant),
        Err("an agent exits only at a turn end or a drain deadline")
    );
}

#[test]
fn an_agent_restored_from_its_value_steps_as_the_original_does() {
    let mut run = one_shot_working();
    let value = run.agent.value();
    let mut restored = Agent::<Kind>::from_value(&value).expect("the value restores");
    assert_eq!(restored.value(), value);

    let input = Input::Provider(Fact {
        channel: Channel::Stream,
        payload: serde_json::to_vec(&json!({"type": "turn_end"})).expect("json"),
    });
    let mut original = Vec::new();
    run.agent.step(input.clone(), &mut original);
    let mut again = Vec::new();
    restored.step(input, &mut again);
    assert_eq!(again, original);
    assert_eq!(restored.value(), run.agent.value());
}

#[test]
fn a_checkpoint_under_another_layout_is_refused() {
    let mut checkpoint = Run::start(&spec(false, None)).agent.checkpoint();
    checkpoint.schema += 1;
    assert_eq!(
        checkpoint.interpreter::<Kind>().err(),
        Some(RestoreError::Schema {
            found: CHECKPOINT_SCHEMA + 1,
            expected: CHECKPOINT_SCHEMA,
        })
    );
}
