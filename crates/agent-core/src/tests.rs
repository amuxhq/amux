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

fn journaled_keys(out: &[Output]) -> Vec<String> {
    out.iter()
        .filter_map(|output| match output {
            Output::AppendJournal(step) => Some(step),
            _ => None,
        })
        .flat_map(|step| step.items.iter().map(|item| item.key.clone()))
        .collect()
}

#[test]
fn the_interpreter_is_woken_when_it_asked_and_never_again_for_a_time_it_heard() {
    let mut run = Run::start(&spec(false, None));
    run.on(Input::DaemonDials { connection: DAEMON });
    let out = run.fact(json!({"type": "wake", "at": 5_000}));
    assert!(out.contains(&Output::ArmTimer {
        id: TimerId::Wake,
        at_ms: 5_000,
    }));
    // The timer's fire is the tick it asked for, and nothing is armed
    // after it.
    let out = run.at(5_000, Input::Timer(TimerId::Wake));
    assert_eq!(journaled_keys(&out), vec!["woke:5000".to_owned()]);
    let arms_wake = |out: &[Output]| {
        out.iter().any(|output| {
            matches!(
                output,
                Output::ArmTimer {
                    id: TimerId::Wake,
                    ..
                }
            )
        })
    };
    assert!(!arms_wake(&out));
    // A time the interpreter's last tick already passed is never armed,
    // which would fire at once and without end; the next event's tick
    // reaches it.
    let out = run.fact(json!({"type": "wake", "at": 4_000}));
    assert!(!arms_wake(&out));
    let out = run.fact(json!({"type": "model", "model": "m"}));
    assert_eq!(journaled_keys(&out), vec![format!("woke:{}", run.now_ms)]);
}

#[test]
fn a_draining_agent_is_not_woken() {
    let mut run = Run::start(&spec(false, None));
    run.on(Input::DaemonDials { connection: DAEMON });
    run.fact(json!({"type": "wake", "at": 50_000}));
    let out = run.frame(ctl_frame::Of::Stop(wire::Stop { kill: false }));
    assert!(out.contains(&Output::CancelTimer { id: TimerId::Wake }));
}

/// The core around headless Claude's own interpreter, for rules that hang
/// on what Claude's stream says rather than on the stand-in's.
mod headless_claude {
    use interpret::claude_sdk::ClaudeSdk;

    use super::*;

    struct Claude {
        agent: Agent<ClaudeSdk>,
        now_ms: u64,
    }

    impl Claude {
        /// A one-shot headless Claude started on its spawn prompt, with its
        /// daemon connected.
        fn one_shot(prompt: &str) -> Self {
            let mut out = Vec::new();
            let agent = Agent::<ClaudeSdk>::start(&spec(true, Some(prompt)), None, 1_000, &mut out);
            let mut claude = Self {
                agent,
                now_ms: 1_000,
            };
            claude.on(Input::DaemonDials { connection: DAEMON });
            claude
        }

        fn on(&mut self, input: Input) -> Vec<Output> {
            self.now_ms += 1;
            let mut out = Vec::new();
            self.agent.step(
                Input::Tick {
                    now_ms: self.now_ms,
                },
                &mut out,
            );
            self.agent.step(input, &mut out);
            out
        }

        /// One line of Claude's stream-json output.
        fn says(&mut self, line: serde_json::Value) -> Vec<Output> {
            self.on(Input::Provider(Fact {
                channel: Channel::Stream,
                payload: serde_json::to_vec(&line).expect("json"),
            }))
        }

        /// A main-thread assistant message.
        fn assistant(&mut self, uuid: &str, content: serde_json::Value) -> Vec<Output> {
            self.says(json!({
                "type": "assistant", "uuid": uuid, "session_id": "s", "parent_tool_use_id": null,
                "message": {
                    "id": format!("msg-{uuid}"), "type": "message", "role": "assistant",
                    "model": "claude-test", "content": content,
                    "usage": {"input_tokens": 10, "output_tokens": 5},
                },
            }))
        }

        /// The result that ends the main thread's turn.
        fn result(&mut self, uuid: &str, text: &str) -> Vec<Output> {
            self.says(json!({
                "type": "result", "subtype": "success", "uuid": uuid, "session_id": "s",
                "is_error": false, "duration_ms": 10, "num_turns": 1, "result": text,
            }))
        }
    }

    /// Headless Claude starts the subagents its Agent tool asks for in the
    /// background: the call's result only says the subagent was launched,
    /// and the parent's turn ends while the subagent still works. A
    /// one-shot agent that left then would kill the subagent and report
    /// "launched" as its answer. It stays until the subagent's
    /// notification has started Claude's follow-up turn and that turn has
    /// ended, and only then finishes.
    #[test]
    fn a_one_shot_headless_claude_stays_while_a_background_subagent_it_started_runs() {
        let mut claude = Claude::one_shot("count the files with a subagent");
        claude.says(json!({"type": "system", "subtype": "init", "uuid": "i1", "session_id": "s", "model": "claude-test"}));
        claude.assistant(
            "a1",
            json!([{"type": "tool_use", "id": "toolu_agent", "name": "Agent",
                "input": {"description": "count", "prompt": "Count the files.", "subagent_type": "general-purpose"}}]),
        );
        claude.says(json!({
            "type": "system", "subtype": "task_started", "uuid": "t1", "session_id": "s",
            "task_id": "a1task", "task_type": "local_agent", "tool_use_id": "toolu_agent",
            "description": "count", "is_backgrounded": true,
        }));
        claude.says(json!({
            "type": "user", "uuid": "u1", "session_id": "s", "parent_tool_use_id": null,
            "message": {"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "toolu_agent", "is_error": false,
                "content": [{"type": "text", "text": "Async agent launched successfully.\nagentId: a1task\nThe agent is working in the background. You will be notified automatically when it completes."}],
            }]},
        }));
        claude.assistant(
            "a2",
            json!([{"type": "text", "text": "The subagent is running in the background."}]),
        );
        let out = claude.result("r1", "The subagent is running in the background.");
        assert_eq!(
            marked_last(&out),
            Some(false),
            "the turn that launched a running subagent is not the agent's last"
        );
        assert!(
            !closes(&out),
            "the agent left while its subagent was running"
        );
        assert_eq!(exit_of(&out), None);

        // The subagent works on, then finishes.
        let out = claude.says(json!({
            "type": "system", "subtype": "task_progress", "uuid": "t2", "session_id": "s",
            "task_id": "a1task", "tool_use_id": "toolu_agent", "description": "count",
            "last_tool_name": "Bash", "usage": {"total_tokens": 100, "tool_uses": 1, "duration_ms": 5},
        }));
        assert!(!closes(&out));
        let out = claude.says(json!({
            "type": "system", "subtype": "task_notification", "uuid": "t3", "session_id": "s",
            "task_id": "a1task", "tool_use_id": "toolu_agent", "status": "completed",
            "output_file": "/tmp/claude/tasks/a1task.output", "summary": "There are 3 files.",
        }));
        assert!(
            !closes(&out),
            "the notification starts Claude's follow-up turn"
        );

        // Claude's follow-up turn on the notification is the last.
        claude.says(json!({"type": "system", "subtype": "init", "uuid": "i2", "session_id": "s", "model": "claude-test"}));
        claude.assistant(
            "a3",
            json!([{"type": "text", "text": "Done: there are 3 files."}]),
        );
        let out = claude.result("r2", "Done: there are 3 files.");
        assert_eq!(marked_last(&out), Some(true));
        assert!(closes(&out));
        let out = claude.on(Input::ProviderExited { code: Some(0) });
        assert_eq!(exit_of(&out), Some(&ExitCause::Finished));
    }
}

/// The one-shot rule over provider subagents, from the parts the agent
/// journals alone: the test kind, where any fact may also carry `starts`
/// or `ends` naming a subagent, which adds that subagent's start or end to
/// the fact's step, as a provider's interpreter does when its provider
/// starts or ends one.
mod hosting {
    use interpret::shape::{InputShape, Structure};
    use interpret::{ItemView, SnapshotView, Stepped};

    use super::*;

    struct Hosting;

    impl Interpreter for Hosting {
        type State = <Kind as Interpreter>::State;
        const KIND: wire::Kind = Kind::KIND;

        fn initial(spec: &AgentSpec) -> (Self::State, Step) {
            Kind::initial(spec)
        }

        fn step(state: &mut Self::State, event: Event) -> Stepped {
            let part = match &event {
                Event::Fact(fact) if fact.channel == Channel::Stream => {
                    serde_json::from_slice::<serde_json::Value>(&fact.payload).ok()
                }
                _ => None,
            };
            let mut stepped = Kind::step(state, event);
            let Some(fact) = part else {
                return stepped;
            };
            let named = |field: &str| fact[field].as_str().map(|id| id.as_bytes().to_vec());
            let (starts, ends) = (named("starts"), named("ends"));
            let mut parts: Vec<wire::AgentStep> = Vec::new();
            if let Some(id) = starts {
                parts.push(wire::AgentStep {
                    agent_id: id,
                    start: Some(wire::NativeStart::default()),
                    ..Default::default()
                });
            }
            if let Some(id) = ends {
                let end = Some(wire::IncarnationEnd {
                    cause: wire::ExitCause::Finished as i32,
                    detail: None,
                });
                match parts.iter_mut().find(|part| part.agent_id == id) {
                    Some(part) => part.end = end,
                    None => parts.push(wire::AgentStep {
                        agent_id: id,
                        end,
                        ..Default::default()
                    }),
                }
            }
            stepped.step.agents.extend(parts);
            stepped
        }

        fn reincarnate(state: Self::State, spec: &AgentSpec) -> (Self::State, Step) {
            Kind::reincarnate(state, spec)
        }

        fn unknown_snapshot() -> Vec<u8> {
            Kind::unknown_snapshot()
        }

        fn pending_messages(state: &Self::State) -> Vec<Vec<u8>> {
            Kind::pending_messages(state)
        }

        fn typed_unrecorded(state: &Self::State) -> bool {
            Kind::typed_unrecorded(state)
        }

        fn wake_at(state: &Self::State) -> Option<i64> {
            Kind::wake_at(state)
        }

        fn describe_item(body: &[u8]) -> ItemView {
            Kind::describe_item(body)
        }

        fn input_shape(input: &wire::Input) -> InputShape {
            Kind::input_shape(input)
        }

        fn structure(state: &Self::State) -> Structure {
            Kind::structure(state)
        }

        fn describe_snapshot(body: &[u8]) -> SnapshotView {
            Kind::describe_snapshot(body)
        }

        fn fixture_input(input_id: Vec<u8>, input: &FixtureInput) -> Option<wire::Input> {
            Kind::fixture_input(input_id, input)
        }
    }

    struct Host {
        agent: Agent<Hosting>,
        now_ms: u64,
    }

    impl Host {
        /// A one-shot agent started on its spawn prompt, its daemon
        /// connected and its provider running the prompt as a turn.
        fn one_shot_working() -> Self {
            let mut out = Vec::new();
            let agent =
                Agent::<Hosting>::start(&spec(true, Some("do the thing")), None, 1_000, &mut out);
            let mut host = Self {
                agent,
                now_ms: 1_000,
            };
            host.on(Input::DaemonDials { connection: DAEMON });
            host.fact(json!({"type": "ready"}));
            host.fact(json!({"type": "user", "id": "p1", "text": "p1"}));
            host
        }

        fn on(&mut self, input: Input) -> Vec<Output> {
            self.now_ms += 1;
            let mut out = Vec::new();
            self.agent.step(
                Input::Tick {
                    now_ms: self.now_ms,
                },
                &mut out,
            );
            self.agent.step(input, &mut out);
            out
        }

        fn fact(&mut self, fact: serde_json::Value) -> Vec<Output> {
            self.on(Input::Provider(Fact {
                channel: Channel::Stream,
                payload: serde_json::to_vec(&fact).expect("json"),
            }))
        }

        /// A step of the provider's that starts the subagent and nothing
        /// else of the host's.
        fn starts(&mut self, id: &str) -> Vec<Output> {
            self.fact(json!({"type": "progress", "starts": id}))
        }

        fn ends(&mut self, id: &str) -> Vec<Output> {
            self.fact(json!({"type": "progress", "ends": id}))
        }

        fn turn_end(&mut self) -> Vec<Output> {
            self.fact(json!({"type": "turn_end"}))
        }

        /// The provider runs a turn of its own, as Claude does on a
        /// subagent's task notification, and it ends.
        fn follow_up_turn(&mut self) -> Vec<Output> {
            self.fact(json!({"type": "turn_start"}));
            self.turn_end()
        }

        fn exits(&mut self) -> Option<ExitCause> {
            exit_of(&self.on(Input::ProviderExited { code: Some(0) })).cloned()
        }
    }

    /// A turn that ends while a subagent it started runs is not the last:
    /// the agent stays. The subagent's end alone, with the agent idle, does
    /// not end it either, since the provider hands the answer back as a
    /// turn of its own; that turn's end is the last.
    #[test]
    fn a_one_shot_host_stays_while_a_subagent_runs_and_ends_after_the_turn_on_its_answer() {
        let mut host = Host::one_shot_working();
        host.starts("sub-a");
        let out = host.turn_end();
        assert_eq!(marked_last(&out), Some(false), "{out:?}");
        assert!(!closes(&out), "it left while its subagent ran");
        assert_eq!(host.agent.summary()["subagents_live"], 1);

        let out = host.ends("sub-a");
        assert!(
            !closes(&out),
            "the turn on the subagent's answer is to come"
        );
        assert_eq!(exit_of(&out), None);
        assert_eq!(host.agent.summary()["subagents_live"], 0);

        let out = host.follow_up_turn();
        assert_eq!(marked_last(&out), Some(true));
        assert!(closes(&out));
        assert_eq!(host.exits(), Some(ExitCause::Finished));
    }

    /// With two subagents, each notification turn reports up, and only the
    /// one after the last subagent ended is the last.
    #[test]
    fn only_the_turn_after_the_last_subagent_ends_is_a_one_shot_hosts_last() {
        let mut host = Host::one_shot_working();
        host.starts("sub-a");
        host.starts("sub-b");
        assert_eq!(marked_last(&host.turn_end()), Some(false));

        host.ends("sub-a");
        let out = host.follow_up_turn();
        assert_eq!(marked_last(&out), Some(false), "sub-b still runs");
        assert!(!closes(&out));

        host.ends("sub-b");
        let out = host.follow_up_turn();
        assert_eq!(marked_last(&out), Some(true));
        assert!(closes(&out));
    }

    /// A subagent that ends in the step that ends the turn, as a foreground
    /// subagent's end comes within its parent's turn, holds nothing; nor
    /// does one that starts and ends within one step.
    #[test]
    fn a_subagent_ended_in_the_turns_own_step_holds_nothing() {
        let mut host = Host::one_shot_working();
        host.fact(json!({"type": "progress", "starts": "sub-a", "ends": "sub-a"}));
        assert_eq!(host.agent.summary()["subagents_live"], 0);
        host.starts("sub-b");
        let out = host.fact(json!({"type": "turn_end", "ends": "sub-b"}));
        assert_eq!(marked_last(&out), Some(true), "{out:?}");
        assert!(closes(&out));
    }

    /// A subagent that starts in the step that ends the turn holds the
    /// agent, as Claude's background subagent launching just before the
    /// parent's result does.
    #[test]
    fn a_subagent_started_in_the_turns_own_step_holds_the_host() {
        let mut host = Host::one_shot_working();
        let out = host.fact(json!({"type": "turn_end", "starts": "sub-a"}));
        assert_eq!(marked_last(&out), Some(false), "{out:?}");
        assert!(!closes(&out));
    }

    /// A parent's model continuing a subagent that ended makes it live
    /// again, and an end for a subagent this incarnation never started (the
    /// provider reporting one of its last process stopped) changes nothing.
    #[test]
    fn a_continued_subagent_holds_the_host_again_and_an_unknown_end_changes_nothing() {
        let mut host = Host::one_shot_working();
        host.starts("sub-a");
        host.ends("sub-a");
        host.ends("from-before");
        host.starts("sub-a");
        let out = host.turn_end();
        assert_eq!(
            marked_last(&out),
            Some(false),
            "the continued subagent runs"
        );
        host.ends("sub-a");
        assert_eq!(marked_last(&host.follow_up_turn()), Some(true));
    }

    /// The live set is part of the core's value, so a restored core judges
    /// the next turn end as the original does.
    #[test]
    fn a_restored_host_still_waits_for_its_subagent() {
        let mut host = Host::one_shot_working();
        host.starts("sub-a");
        let value = host.agent.value();
        host.agent = Agent::<Hosting>::from_value(&value).expect("the value restores");
        assert_eq!(marked_last(&host.turn_end()), Some(false));
    }
}
