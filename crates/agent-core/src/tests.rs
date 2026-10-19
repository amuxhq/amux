use interpret::testkind::TestKind;
use interpret::{Channel, Fact, FixtureInput};
use serde_json::json;
use wire::{
    AgentRef, AgentSpec, Bootstrap, CtlFrame, Envelope, PromptInput, PromptSubmission, bootstrap,
    ctl_frame,
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

    /// The journal steps written so far, in order.
    fn journal(&self) -> Vec<&Step> {
        self.out
            .iter()
            .filter_map(|output| match output {
                Output::AppendJournal(step) => Some(step),
                _ => None,
            })
            .collect()
    }

    /// The facts-ring lines written so far, in order.
    fn recorded(&self) -> Vec<Entry> {
        self.out
            .iter()
            .filter_map(|output| match output {
                Output::Record(entry) => Some(entry.clone()),
                _ => None,
            })
            .collect()
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

fn turn_ends(out: &[Output]) -> Vec<bool> {
    out.iter()
        .filter_map(|output| match output {
            Output::AppendJournal(step) => step.turn_end.as_ref().map(|end| end.last),
            _ => None,
        })
        .collect()
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

#[test]
fn a_start_journals_the_first_step_and_arms_the_grace() {
    let run = Run::start(&spec(false, None));
    assert_eq!(run.journal().len(), 1, "the initial snapshot");
    assert!(run.out.contains(&Output::ArmTimer {
        id: TimerId::Grace,
        at_ms: 1_000 + GRACE_MS,
    }));
    assert!(run.out.contains(&Output::Job {
        id: JobId(1),
        op: AgentJob::ReadGit,
    }));
}

#[test]
fn a_tick_asks_for_nothing() {
    let mut run = Run::start(&spec(false, None));
    let mut out = Vec::new();
    run.agent.step(Input::Tick { now_ms: 5_000 }, &mut out);
    assert_eq!(out, Vec::new());
}

#[test]
fn a_one_shot_agent_exits_finished_at_its_turn_end_and_not_before() {
    let mut run = one_shot_working();
    let out = run.fact(json!({"type": "text", "id": "m", "delta": "done"}));
    assert_eq!(exit_of(&out), None);
    assert!(!closes(&out), "nothing exits while the turn runs");

    let out = run.turn_end();
    assert_eq!(turn_ends(&out), vec![true], "the turn end is marked last");
    assert!(closes(&out), "the provider is asked to finish");
    let last = run.journal().last().copied().cloned().expect("a step");
    assert_eq!(
        last.incarnation_end,
        Some(ExitCause::Finished.end()),
        "the exit cause is the journal's last step"
    );
    assert_eq!(exit_of(&out), None, "the process waits for its provider");

    assert_eq!(provider_exits(&mut run), Some(ExitCause::Finished));
    assert_eq!(run.agent.exited(), Some(&ExitCause::Finished));
}

#[test]
fn a_one_shot_agent_runs_its_queued_follow_up_before_it_exits() {
    let mut run = one_shot_working();
    let out = run.prompt("p2", "and then this");
    assert_eq!(
        verdicts(&out),
        vec![(b"p2".to_vec(), reply::accepted(true))],
        "queued behind the running turn"
    );

    let out = run.turn_end();
    assert_eq!(turn_ends(&out), vec![false], "work is still queued");
    assert!(!closes(&out));
    assert_eq!(
        provider_writes(&out),
        vec![r#"{"text":"and then this","type":"user"}"#],
        "the follow-up is handed over at the turn end"
    );

    run.reflect("p2");
    let out = run.turn_end();
    assert_eq!(turn_ends(&out), vec![true]);
    assert!(closes(&out));
    assert_eq!(provider_exits(&mut run), Some(ExitCause::Finished));
}

#[test]
fn an_agent_without_a_parent_stays_after_its_turn() {
    let mut run = Run::start(&spec(false, None));
    run.on(Input::DaemonDials { connection: DAEMON });
    run.fact(json!({"type": "ready"}));
    run.prompt("p1", "hello");
    run.reflect("p1");
    let out = run.turn_end();
    assert_eq!(turn_ends(&out), vec![false]);
    assert!(!closes(&out));
    assert_eq!(run.agent.exited(), None);
}

#[test]
fn a_lost_daemon_drains_the_running_turn_and_exits_at_its_end() {
    let mut run = Run::start(&spec(false, None));
    run.on(Input::DaemonDials { connection: DAEMON });
    run.fact(json!({"type": "ready"}));
    run.prompt("p1", "hello");
    run.reflect("p1");

    let gone = run.on(Input::DaemonGone { connection: DAEMON });
    let deadline = run.now_ms + GRACE_MS;
    assert!(gone.contains(&Output::ArmTimer {
        id: TimerId::Grace,
        at_ms: deadline,
    }));
    let early = run.at(deadline - 1, Input::Timer(TimerId::Grace));
    assert!(
        !early.contains(&Output::Record(Entry::DaemonLost)),
        "a timer before its deadline changes nothing"
    );

    let out = run.at(deadline, Input::Timer(TimerId::Grace));
    assert!(out.contains(&Output::Record(Entry::DaemonLost)));
    assert!(!closes(&out), "the running turn finishes first");
    assert!(
        run.fact(json!({"type": "text", "id": "m", "delta": "still"}))
            .iter()
            .all(|output| output != &Output::CloseProvider)
    );

    let out = run.turn_end();
    assert_eq!(turn_ends(&out), vec![true]);
    assert!(closes(&out));
    assert_eq!(provider_exits(&mut run), Some(ExitCause::DaemonLost));
}

#[test]
fn a_drain_leaves_queued_input_for_the_next_incarnation() {
    let mut run = Run::start(&spec(false, None));
    run.on(Input::DaemonDials { connection: DAEMON });
    run.fact(json!({"type": "ready"}));
    run.prompt("p1", "hello");
    run.reflect("p1");
    run.prompt("p2", "and then this");
    run.on(Input::DaemonGone { connection: DAEMON });
    run.at(run.now_ms + GRACE_MS, Input::Timer(TimerId::Grace));

    let out = run.turn_end();
    assert!(closes(&out), "the drain ends at the turn end");
    assert_eq!(
        provider_writes(&out),
        Vec::<String>::new(),
        "nothing more goes to a provider being closed"
    );
    let queued: Vec<Vec<u8>> = run
        .journal()
        .iter()
        .rev()
        .find_map(|step| step.snapshot.as_ref())
        .expect("a snapshot")
        .queue
        .iter()
        .map(|queued| queued.input_id.clone())
        .collect();
    assert_eq!(
        queued,
        vec![b"p2".to_vec()],
        "the follow-up waits in the queue"
    );
    assert_eq!(provider_exits(&mut run), Some(ExitCause::DaemonLost));
}

#[test]
fn a_daemon_back_within_the_grace_keeps_the_agent() {
    let mut run = Run::start(&spec(false, None));
    run.on(Input::DaemonDials { connection: DAEMON });
    run.on(Input::DaemonGone { connection: DAEMON });
    let back = run.on(Input::DaemonDials {
        connection: ConnId(2),
    });
    assert!(back.contains(&Output::CancelTimer { id: TimerId::Grace }));
    let out = run.at(run.now_ms + GRACE_MS, Input::Timer(TimerId::Grace));
    assert!(!out.contains(&Output::Record(Entry::DaemonLost)));
    assert_eq!(run.agent.exited(), None);
}

#[test]
fn an_ask_left_open_after_the_daemon_is_lost_exits_orphaned_at_the_drain_deadline() {
    let mut run = Run::start(&spec(false, None));
    run.on(Input::DaemonDials { connection: DAEMON });
    run.fact(json!({"type": "ready"}));
    run.prompt("p1", "hello");
    run.reflect("p1");
    run.fact(json!({"type": "ask", "id": "a1", "tool": "t1", "tool_name": "Bash"}));
    run.on(Input::DaemonGone { connection: DAEMON });

    let lost = run.now_ms + GRACE_MS;
    let out = run.at(lost, Input::Timer(TimerId::Grace));
    let drain = lost + DRAIN_MS;
    assert!(out.contains(&Output::ArmTimer {
        id: TimerId::Drain,
        at_ms: drain,
    }));
    assert!(!closes(&out), "the ask waits its deadline");

    assert!(!closes(&run.at(drain - 1, Input::Timer(TimerId::Drain))));
    let out = run.at(drain, Input::Timer(TimerId::Drain));
    assert!(closes(&out));
    assert_eq!(provider_exits(&mut run), Some(ExitCause::Orphaned));
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
fn a_kill_ends_the_process_at_once_and_writes_nothing() {
    let mut run = Run::start(&spec(false, None));
    run.on(Input::DaemonDials { connection: DAEMON });
    run.fact(json!({"type": "ready"}));
    run.prompt("p1", "hello");
    let out = run.frame(ctl_frame::Of::Stop(wire::Stop { kill: true }));
    assert!(out.contains(&Output::KillProvider));
    assert_eq!(exit_of(&out), Some(&ExitCause::Killed));
    assert!(
        !out.iter()
            .any(|output| matches!(output, Output::AppendJournal(_))),
        "a kill writes nothing"
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
fn a_message_after_the_exit_decision_is_refused_and_resumes_the_next_incarnation() {
    let first = spec(true, Some("do the thing"));
    let mut run = Run::start(&first);
    let checkpoint = run.agent.checkpoint();
    run.on(Input::DaemonDials { connection: DAEMON });
    run.fact(json!({"type": "ready"}));
    run.reflect("p1");
    run.turn_end();
    assert!(closes(&run.out), "the exit is decided");

    let message = Envelope {
        id: b"m1".to_vec(),
        body: Some(wire::envelope::Body::Message(wire::MessageBody {
            text: "one more thing".into(),
            ..Default::default()
        })),
        ..Default::default()
    };
    let out = run.frame(ctl_frame::Of::AgentMessage(message.clone()));
    assert_eq!(
        verdicts(&out),
        vec![(b"m1".to_vec(), reply::rejected(wire::Refusal::Exiting))],
        "the refusal is what tells the daemon to resume the agent with it"
    );
    assert_eq!(provider_exits(&mut run), Some(ExitCause::Finished));

    // The daemon resumes the agent with the message as its bootstrap; the
    // next incarnation continues from what the facts ring holds.
    let next = AgentSpec {
        incarnation: 2,
        bootstrap: Some(Bootstrap {
            of: Some(bootstrap::Of::AgentMessage(message)),
        }),
        ..first
    };
    let resumed = Resumed {
        state: checkpoint.interpreter::<Kind>().expect("it decodes"),
        inputs: run.recorded(),
    };
    let mut resumed = Run::resume(&next, Some(resumed));
    assert_eq!(resumed.agent.incarnation(), 2);
    assert!(
        resumed
            .journal()
            .iter()
            .all(|step| step.incarnation_end.is_none()),
        "the last incarnation wrote its own end"
    );
    resumed.on(Input::DaemonDials { connection: DAEMON });
    let out = resumed.fact(json!({"type": "ready"}));
    assert_eq!(
        provider_writes(&out),
        vec![r#"{"text":"one more thing","type":"user"}"#],
        "the message runs as the next incarnation's first turn"
    );
}

#[test]
fn an_incarnation_that_ended_without_its_boundary_gets_one_when_the_next_starts() {
    let first = spec(false, None);
    let mut run = Run::start(&first);
    let checkpoint = run.agent.checkpoint();
    run.on(Input::DaemonDials { connection: DAEMON });
    run.fact(json!({"type": "ready"}));
    // The process is killed here: no boundary.
    let resumed = Resumed {
        state: checkpoint.interpreter::<Kind>().expect("it decodes"),
        inputs: run.recorded(),
    };
    let next = Run::resume(
        &AgentSpec {
            incarnation: 2,
            ..first
        },
        Some(resumed),
    );
    let journal = next.journal();
    assert_eq!(journal.len(), 2, "the missing boundary, then the resume");
    assert_eq!(
        journal[0].incarnation_end, None,
        "the row already says how the last incarnation ended"
    );
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
fn a_checkpoint_restores_the_interpreter_it_was_taken_from() {
    let run = Run::start(&spec(false, None));
    let checkpoint = run.agent.checkpoint();
    let state = checkpoint.interpreter::<Kind>().expect("it decodes");
    assert_eq!(interpret::encode_checkpoint(&state), checkpoint.state);
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
