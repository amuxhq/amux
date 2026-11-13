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
        let agent = Agent::<Kind>::start(spec, resumed, 1_000, &mut out);
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

const CHILD: &[u8] = b"child";
/// The daemon that dials in once the first has gone.
const NEXT: ConnId = ConnId(2);

impl Run {
    /// What the process heard of the child its tool server asked for.
    fn hears(&mut self, heard: Spawn) -> Vec<Output> {
        self.on(Input::Spawn {
            child: CHILD.to_vec(),
            heard,
        })
    }

    /// The connected daemon's word of the live children: the child in its
    /// first incarnation, or none.
    fn children(&mut self, live: bool) -> Vec<Output> {
        let connection = self.agent.life.connection.expect("a daemon is connected");
        self.on(Input::Control {
            connection,
            frame: CtlFrame {
                of: Some(ctl_frame::Of::Children(wire::Children {
                    children: live
                        .then(|| wire::LiveChild {
                            agent_id: CHILD.to_vec(),
                            incarnation: 1,
                        })
                        .into_iter()
                        .collect(),
                })),
            },
        })
    }

    /// The daemon goes away and the next one dials in.
    fn next_daemon(&mut self) {
        self.on(Input::DaemonGone { connection: DAEMON });
        self.on(Input::DaemonDials { connection: NEXT });
    }

    /// The child's last report arrives and the provider runs it as a turn,
    /// which ends.
    fn child_reports_and_the_turn_ends(&mut self) -> Vec<Output> {
        self.frame(ctl_frame::Of::AgentMessage(wire::Envelope {
            id: b"report".to_vec(),
            from: Some(wire::Sender {
                value: Some(wire::sender::Value::Agent(wire::AgentSender {
                    agent: Some(AgentRef {
                        agent_id: CHILD.to_vec(),
                        ..Default::default()
                    }),
                    ..Default::default()
                })),
            }),
            body: Some(wire::envelope::Body::Report(wire::ChildReport {
                from_incarnation: 1,
                end: Some(wire::child_report::End::TurnEnded(wire::TurnEnded {
                    text: "done".into(),
                    last: true,
                    ..Default::default()
                })),
                ..Default::default()
            })),
            ..Default::default()
        }));
        self.fact(json!({"type": "user", "envelope": "report"}));
        self.fact(json!({"type": "turn_start"}));
        self.turn_end()
    }
}

/// Whether the turn end in `out` was marked the incarnation's last.
fn marked_last(out: &[Output]) -> Option<bool> {
    out.iter().find_map(|output| match output {
        Output::AppendJournal(step) => step.turn_end.as_ref().map(|end| end.last),
        _ => None,
    })
}

/// A one-shot agent's turn ends after it spawned a child and before its
/// daemon has said a word of that child: it stays, because it asked for the
/// child itself, and exits once the child's last report has had its turn.
#[test]
fn a_one_shot_agent_stays_for_a_child_it_spawned_before_its_daemon_names_it() {
    let mut run = one_shot_working();
    run.hears(Spawn::Requested);
    run.hears(Spawn::Created { incarnation: 1 });
    let out = run.turn_end();
    assert_eq!(marked_last(&out), Some(false));
    assert!(!closes(&out), "its child still works");

    let out = run.children(true);
    assert!(!closes(&out), "the daemon's word changes nothing");
    let out = run.child_reports_and_the_turn_ends();
    assert_eq!(marked_last(&out), Some(true));
    assert!(closes(&out));
    assert_eq!(provider_exits(&mut run), Some(ExitCause::Finished));
}

/// A spawn the daemon refused made no child: nothing is waited for.
#[test]
fn a_one_shot_agent_whose_spawn_was_refused_exits_at_its_turns_end() {
    let mut run = one_shot_working();
    run.hears(Spawn::Requested);
    run.hears(Spawn::Refused);
    let out = run.turn_end();
    assert_eq!(marked_last(&out), Some(true));
    assert!(closes(&out));
    assert_eq!(provider_exits(&mut run), Some(ExitCause::Finished));
}

/// The daemon's frame can be written before it made the child, and read
/// before its answer: a child still being asked for is not dropped for
/// being absent from it.
#[test]
fn a_children_frame_before_the_answer_does_not_clear_a_child_asked_for() {
    let mut run = one_shot_working();
    run.hears(Spawn::Requested);
    run.children(false);
    let summary = run.agent.summary();
    assert_eq!(summary["spawns_unsettled"], 1, "{summary}");
    run.hears(Spawn::Created { incarnation: 1 });
    let out = run.turn_end();
    assert_eq!(marked_last(&out), Some(false));
    assert!(!closes(&out), "its child still works");
    assert_eq!(run.agent.exiting(), None);
}

/// The daemon went away before it answered the spawn. The agent stays past
/// its turn's end, and a frame names the child: it was made, and the agent
/// waits for its last report.
#[test]
fn an_unconfirmed_spawn_a_frame_names_is_waited_for() {
    let mut run = one_shot_working();
    run.hears(Spawn::Requested);
    run.hears(Spawn::Unconfirmed);
    let out = run.turn_end();
    assert_eq!(marked_last(&out), Some(false));
    assert!(!closes(&out), "nobody has said whether there is a child");

    let out = run.children(true);
    assert!(!closes(&out), "there is one: its last report is to come");
    assert_eq!(run.agent.summary()["spawns_unsettled"], 0);
    let out = run.child_reports_and_the_turn_ends();
    assert!(closes(&out));
    assert_eq!(provider_exits(&mut run), Some(ExitCause::Finished));
}

/// A frame that leaves the child out says it was never made only when it
/// comes from a daemon that connected after the one that went away: that
/// one may have written its frame before it made the child. No report is
/// coming to start another turn, so the agent, idle with nothing left to
/// wait for, finishes on the next daemon's word.
#[test]
fn an_unconfirmed_spawn_the_next_daemons_frame_leaves_out_ends_the_wait() {
    let mut run = one_shot_working();
    run.hears(Spawn::Requested);
    run.hears(Spawn::Unconfirmed);
    let out = run.turn_end();
    assert!(!closes(&out), "nobody has said whether there is a child");
    let out = run.children(false);
    assert!(!closes(&out), "written, perhaps, before the child was made");

    run.next_daemon();
    let out = run.children(false);
    assert!(closes(&out), "there is none, and nothing else to wait for");
    assert_eq!(dumps(&out), Vec::<&DumpReason>::new());
    assert_eq!(provider_exits(&mut run), Some(ExitCause::Finished));
}

/// Settled as never made while the turn still runs, the spawn leaves the
/// turn's end to decide, as ever.
#[test]
fn an_unconfirmed_spawn_settled_mid_turn_leaves_the_exit_to_the_turns_end() {
    let mut run = one_shot_working();
    run.hears(Spawn::Requested);
    run.hears(Spawn::Unconfirmed);
    run.next_daemon();
    let out = run.children(false);
    assert!(!closes(&out), "the turn still runs");
    assert_eq!(run.agent.summary()["spawns_unsettled"], 0);
    let out = run.turn_end();
    assert_eq!(marked_last(&out), Some(true));
    assert!(closes(&out));
}

/// A turn cut short can end while the spawn it made is still with the
/// daemon. The agent stays. A child made then is waited for; a refusal
/// leaves nothing to wait for and no turn to come, and the agent finishes.
#[test]
fn a_spawn_answered_after_its_turn_ended_is_waited_for_or_ends_the_wait() {
    let mut run = one_shot_working();
    run.hears(Spawn::Requested);
    let out = run.turn_end();
    assert_eq!(marked_last(&out), Some(false));
    assert!(!closes(&out), "the daemon has not answered yet");
    let out = run.hears(Spawn::Created { incarnation: 1 });
    assert!(!closes(&out), "its child works");
    assert_eq!(run.agent.exiting(), None);

    let mut run = one_shot_working();
    run.hears(Spawn::Requested);
    run.turn_end();
    let out = run.hears(Spawn::Refused);
    assert!(
        closes(&out),
        "there is no child, and nothing else to wait for"
    );
    assert_eq!(dumps(&out), Vec::<&DumpReason>::new());
    assert_eq!(provider_exits(&mut run), Some(ExitCause::Finished));
}

/// An agent without a parent keeps what it spawned too, and never exits
/// for it.
#[test]
fn an_agent_without_a_parent_never_exits_over_its_spawns() {
    let mut run = Run::start(&spec(false, Some("do the thing")));
    run.on(Input::DaemonDials { connection: DAEMON });
    run.fact(json!({"type": "ready"}));
    run.reflect("p1");
    run.hears(Spawn::Requested);
    run.hears(Spawn::Unconfirmed);
    assert!(!closes(&run.turn_end()));
    run.next_daemon();
    assert!(!closes(&run.children(false)));
    assert_eq!(run.agent.exiting(), None);
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
    assert_eq!(run.agent.life.connection, Some(ConnId(2)));
}

#[test]
fn a_drain_deadline_armed_while_running_is_a_violation() {
    let mut run = Run::start(&spec(false, None));
    run.agent.life.ask = Some(OpenAsk { until: 0 });
    let mut out = Vec::new();
    run.agent
        .step(Input::DaemonDials { connection: DAEMON }, &mut out);
    let dumps = dumps(&out);
    assert_eq!(dumps.len(), 1, "{out:?}");
    match dumps[0] {
        DumpReason::Violation(violation) => {
            assert_eq!(violation.invariant, crate::DEADLINE_WITHOUT_ORPHAN)
        }
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
    run.agent.life.pending.queued.insert(b"p1".to_vec());
    let out = run.reflect("p1");
    assert!(
        matches!(dumps(&out).as_slice(), [DumpReason::Violation(violation)]
            if violation.invariant == crate::TAKEN_STILL_PENDING),
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
        Err(crate::EXIT_OUTSIDE_TURN_END)
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

/// An exit's words can be the provider's, and `amux ls` prints them as they
/// are, so they carry no terminal codes.
#[test]
fn an_exits_words_carry_no_terminal_codes() {
    let end =
        ExitCause::Interpreter("refused \u{1b}]52;c;eA==\u{7}the thread\u{1b}[2J".into()).end();
    assert_eq!(end.detail.as_deref(), Some("refused the thread"));
    let end = ExitCause::Unstarted("no \u{9b}31mclaude".into()).end();
    assert_eq!(end.detail.as_deref(), Some("the provider: no claude"));
}

/// A step that breaks one of the interpreter's checks asks for a dump as
/// one that breaks the core's own does.
#[test]
fn a_step_that_breaks_an_interpreters_check_asks_for_a_dump() {
    let spec = spec(false, None);
    let mut run = Run::start(&spec);
    run.on(Input::DaemonDials { connection: DAEMON });
    run.fact(json!({"type": "ready"}));
    run.fact(json!({"type": "text", "id": "m1", "delta": "half a"}));
    let out = run.on(Input::ProviderExited { code: Some(1) });
    assert!(
        matches!(dumps(&out).as_slice(), [DumpReason::Violation(violation)]
            if violation.invariant == interpret::checks::EXIT_LEFT_OPEN),
        "{out:?}"
    );
}

/// The lifecycle a dump carries names its states and counts its sets,
/// and holds no id.
#[test]
fn the_lifecycles_summary_holds_states_and_counts() {
    let spec = spec(false, None);
    let mut run = Run::start(&spec);
    run.on(Input::DaemonDials { connection: DAEMON });
    run.fact(json!({"type": "ready"}));
    run.prompt("p1", "hello");
    let summary = run.agent.summary();
    assert_eq!(summary["mode"], "running", "{summary}");
    assert_eq!(summary["connected"], true, "{summary}");
    let text = summary.to_string();
    assert!(!text.contains("p1") && !text.contains("hello"), "{text}");
}
