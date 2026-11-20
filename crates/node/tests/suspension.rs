//! Suspension on the agent's own host, against a real daemon with
//! synthetic agents: an end the agent's journal gives no cause for is
//! recorded only after a grace, and none at all once the daemon is on its
//! way out; and a suspended row is handled with the experiment switched
//! off as with it on.

use std::time::Duration;

use node::Launch;
use support::synthetic::*;
use support::*;
use wire::{Envelope, ExitCause, Lifecycle};

use crate::support;

const SEGMENTS: u64 = 1 << 20;

/// What daemons here start with: nothing spawned, the experiment on.
fn suspending() -> Launch {
    let mut launch = quiet_launch();
    launch.experiments = settings::experiments::Enabled::of(&[&settings::experiments::SUSPENSION]);
    launch
}

/// The agent's row, read beside the daemon.
fn row(install: &Install, agent: &SyntheticAgent) -> store::AgentRow {
    install
        .store()
        .agent(&agent.key(install))
        .unwrap()
        .expect("the agent's row")
}

async fn until_connected(agent: &SyntheticAgent) {
    until("the daemon to connect", || async {
        (agent.hellos() > 0)
            .then_some(())
            .ok_or_else(|| "no Hello".to_owned())
    })
    .await
    .unwrap();
}

/// An agent process that dies without its journal saying why is marked
/// exited only once the grace is up: a daemon about to be signalled itself
/// at a shutdown never gets there first. Its child is stopped only then.
#[tokio::test(flavor = "multi_thread")]
async fn an_end_without_a_cause_is_recorded_after_a_grace() {
    let install = Install::new();
    let mut lead = SyntheticAgent::new(&install, "lead", SEGMENTS);
    lead.register(&install);
    lead.go_live();
    let daemon = install.start("boot-1", suspending()).await;
    let runtime = runtime(&daemon, &install);
    until_connected(&lead).await;

    lead.die().await;
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    assert_eq!(
        row(&install, &lead).lifecycle,
        Lifecycle::Live as i32,
        "within the grace"
    );
    until("the end to be recorded", || async {
        let row = row(&install, &lead);
        (row.lifecycle == Lifecycle::Exited as i32)
            .then_some(())
            .ok_or_else(|| format!("{row:?}"))
    })
    .await
    .unwrap();
    assert_eq!(
        row(&install, &lead).exit_cause,
        ExitCause::EndedUnexpectedly as i32
    );
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// Once the daemon has been asked to stop it records no exit: an agent
/// that dies then is decided by the next start's sweep, which on the same
/// boot marks it exited as ended while away.
#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_on_its_way_out_records_no_exit_and_the_next_start_decides() {
    let install = Install::new();
    let mut lead = SyntheticAgent::new(&install, "lead", SEGMENTS);
    lead.register(&install);
    lead.go_live();
    let daemon = install.start("boot-1", suspending()).await;
    let runtime = runtime(&daemon, &install);
    until_connected(&lead).await;

    runtime.stopping().await;
    lead.die().await;
    tokio::time::sleep(Duration::from_millis(
        daemon_core::CAUSELESS_GRACE_MS + 1_000,
    ))
    .await;
    assert_eq!(row(&install, &lead).lifecycle, Lifecycle::Live as i32);
    drop(runtime);
    daemon.shutdown().await.unwrap();

    let daemon = install.start("boot-1", suspending()).await;
    let row = row(&install, &lead);
    assert_eq!(
        (row.lifecycle, row.exit_cause),
        (Lifecycle::Exited as i32, ExitCause::EndedWhileAway as i32)
    );
    daemon.shutdown().await.unwrap();
}

/// Switching the experiment off stops new suspensions only: a row left
/// suspended is still left alone by the sweep, resumed by a message as its
/// first input, and marked exited as stopped when the person closes it.
#[tokio::test(flavor = "multi_thread")]
async fn with_the_experiment_off_a_suspended_row_is_still_woken_and_closed() {
    let install = Install::new();
    let napper = SyntheticAgent::new(&install, "napper", SEGMENTS);
    napper.write_spec();
    let dozer = SyntheticAgent::new(&install, "dozer", SEGMENTS);
    for agent in [&napper, &dozer] {
        let mut row = agent.row(&install);
        row.exit_cause = ExitCause::Suspended as i32;
        install.store().put_agent(&row).unwrap();
    }
    let daemon = install
        .start(
            "boot-1",
            install.launch(
                "resumed",
                vec![
                    support::text("awake"),
                    provider_fakes::script::Step::TurnEnd,
                ],
            ),
        )
        .await;
    let runtime = runtime(&daemon, &install);
    for agent in [&napper, &dozer] {
        let row = row(&install, agent);
        assert_eq!(
            (row.lifecycle, row.exit_cause),
            (Lifecycle::Live as i32, ExitCause::Suspended as i32),
            "the sweep leaves {} suspended",
            agent.name
        );
    }

    let envelope = Envelope {
        id: b"wake-up-message!".to_vec(),
        to: Some(wire::AgentRef {
            host_id: host(&install).as_bytes().to_vec(),
            agent_id: napper.id.as_bytes().to_vec(),
        }),
        body: Some(wire::envelope::Body::Message(wire::MessageBody {
            text: "time to go on".into(),
            ..Default::default()
        })),
        ..Envelope::default()
    };
    runtime
        .send_message(envelope, None)
        .await
        .expect("the message resumes the suspended agent");
    assert_eq!(runtime.agent(napper.id).await.unwrap().incarnation, 2);
    let spec = std::fs::read(napper.dir.join("spec.2")).unwrap();
    let spec = <wire::AgentSpec as prost::Message>::decode(spec.as_slice()).unwrap();
    assert!(
        matches!(
            spec.bootstrap.and_then(|bootstrap| bootstrap.of),
            Some(wire::bootstrap::Of::AgentMessage(message)) if message.id == b"wake-up-message!"
        ),
        "the message is the first input"
    );

    let closed = runtime
        .file(dozer.id, daemon_core::Filing::Closed)
        .await
        .unwrap();
    assert_eq!(
        (closed.lifecycle, closed.exit_cause),
        (Lifecycle::Exited as i32, ExitCause::Stopped as i32)
    );
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

fn prompt(id: &[u8], text: &str) -> wire::Input {
    wire::Input::new(
        id.to_vec(),
        wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
                text: text.to_owned(),
                attachments: Vec::new(),
            })),
        },
    )
}

/// What a person typing to the agent sends, as `amux send`, a chat's
/// composer and a call carried from a far host all do.
fn typed(agent: &SyntheticAgent, id: &[u8], text: &str) -> wire::SendInputRequest {
    wire::SendInputRequest {
        agent_id: agent.id.as_bytes().to_vec(),
        input: Some(prompt(id, text)),
    }
}

/// A person's input to a suspended agent wakes it with that input as its
/// next incarnation's first, and is answered as taken; the same input to
/// an agent the person stopped is refused as exited and starts nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_persons_input_wakes_a_suspended_agent_and_not_a_stopped_one() {
    let install = Install::new();
    let napper = SyntheticAgent::new(&install, "napper", SEGMENTS);
    napper.write_spec();
    let mut row = napper.row(&install);
    row.exit_cause = ExitCause::Suspended as i32;
    install.store().put_agent(&row).unwrap();
    let stopped = SyntheticAgent::new(&install, "stopped", SEGMENTS);
    stopped.write_spec();
    let mut row = stopped.row(&install);
    row.lifecycle = Lifecycle::Exited as i32;
    row.exit_cause = ExitCause::Stopped as i32;
    install.store().put_agent(&row).unwrap();
    let daemon = install
        .start(
            "boot-1",
            install.launch(
                "resumed",
                vec![
                    support::text("awake"),
                    provider_fakes::script::Step::TurnEnd,
                ],
            ),
        )
        .await;
    let runtime = runtime(&daemon, &install);

    let verdict = runtime
        .send_input(&typed(&napper, b"typed-1", "time to go on"))
        .await
        .expect("the input is answered");
    assert!(
        matches!(verdict.of, Some(wire::send_input_response::Of::Accepted(_))),
        "the suspended agent took it: {verdict:?}"
    );
    let woken = runtime.agent(napper.id).await.unwrap();
    assert_eq!(
        (woken.lifecycle(), woken.incarnation),
        (Lifecycle::Live, 2),
        "{woken:?}"
    );
    let spec = std::fs::read(napper.dir.join("spec.2")).unwrap();
    let spec = <wire::AgentSpec as prost::Message>::decode(spec.as_slice()).unwrap();
    assert!(
        matches!(
            spec.bootstrap.and_then(|bootstrap| bootstrap.of),
            Some(wire::bootstrap::Of::Prompt(prompt))
                if prompt.input_id == b"typed-1"
                    && prompt.prompt.as_ref().is_some_and(|prompt| prompt.text == "time to go on")
        ),
        "the input is the first"
    );

    let verdict = runtime
        .send_input(&typed(&stopped, b"typed-2", "and you"))
        .await
        .expect("the input is answered");
    assert!(
        matches!(
            verdict.of,
            Some(wire::send_input_response::Of::Rejected(ref rejected))
                if rejected.refusal() == wire::Refusal::Exited
        ),
        "{verdict:?}"
    );
    assert_eq!(runtime.agent(stopped.id).await.unwrap().incarnation, 1);
    assert!(!stopped.dir.join("spec.2").exists());
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// A person's input that reaches the agent as it suspends, refused as
/// exiting, waits for the exit and then wakes it with that input as its
/// next incarnation's first: answered as taken, not "not sent".
#[tokio::test(flavor = "multi_thread")]
async fn a_persons_input_met_by_a_suspension_wakes_the_agent_with_it() {
    let install = Install::new();
    let mut napper = SyntheticAgent::new(&install, "napper", SEGMENTS);
    napper.register(&install);
    napper.write_spec();
    napper.go_live();
    napper.answer_with(Answer::Reject(wire::Refusal::Exiting));
    let mut launch = install.launch(
        "resumed",
        vec![
            support::text("awake"),
            provider_fakes::script::Step::TurnEnd,
        ],
    );
    launch.experiments = settings::experiments::Enabled::of(&[&settings::experiments::SUSPENSION]);
    let daemon = install.start("boot-1", launch).await;
    let runtime = runtime(&daemon, &install);
    until_connected(&napper).await;

    let sent = tokio::spawn({
        let runtime = runtime.clone();
        let request = typed(&napper, b"typed-1", "time to go on");
        async move { runtime.send_input(&request).await }
    });
    // A window: an input waiting for the exit leaves no mark to wait on.
    holds_for("the input to wait", Duration::from_millis(300), || async {
        !sent.is_finished()
    })
    .await
    .expect("the input waits for the exit instead of being refused");
    napper.exit(ExitCause::Suspended, None).await;
    let verdict = sent.await.unwrap().expect("the input is answered");
    assert!(
        matches!(verdict.of, Some(wire::send_input_response::Of::Accepted(_))),
        "the suspending agent took it: {verdict:?}"
    );
    assert_eq!(runtime.agent(napper.id).await.unwrap().incarnation, 2);
    let spec = std::fs::read(napper.dir.join("spec.2")).unwrap();
    let spec = <wire::AgentSpec as prost::Message>::decode(spec.as_slice()).unwrap();
    assert!(
        matches!(
            spec.bootstrap.and_then(|bootstrap| bootstrap.of),
            Some(wire::bootstrap::Of::Prompt(prompt)) if prompt.input_id == b"typed-1"
        ),
        "the input is the first"
    );
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// A person's input typed once the row reads suspended, while the process
/// that suspended is still connected, is held for the next incarnation:
/// nothing is handed to the closing process, which would exit without
/// answering and leave the person not knowing whether it arrived.
#[tokio::test(flavor = "multi_thread")]
async fn a_persons_input_to_a_row_just_suspended_waits_for_the_process_to_go() {
    let install = Install::new();
    let mut napper = SyntheticAgent::new(&install, "napper", SEGMENTS);
    napper.register(&install);
    napper.write_spec();
    napper.go_live();
    // A closing process reads what it is sent and answers none of it.
    napper.freeze();
    let mut launch = install.launch(
        "resumed",
        vec![
            support::text("awake"),
            provider_fakes::script::Step::TurnEnd,
        ],
    );
    launch.experiments = settings::experiments::Enabled::of(&[&settings::experiments::SUSPENSION]);
    let daemon = install.start("boot-1", launch).await;
    let runtime = runtime(&daemon, &install);
    until_connected(&napper).await;

    napper.append(&wire::Step {
        incarnation_end: Some(wire::IncarnationEnd {
            cause: ExitCause::Suspended as i32,
            detail: None,
        }),
        ..wire::Step::default()
    });
    napper.nudge().await;
    until("the row to read suspended", || async {
        let row = row(&install, &napper);
        (row.exit_cause == ExitCause::Suspended as i32)
            .then_some(())
            .ok_or_else(|| format!("{row:?}"))
    })
    .await
    .unwrap();

    let sent = tokio::spawn({
        let runtime = runtime.clone();
        let request = typed(&napper, b"typed-1", "time to go on");
        async move { runtime.send_input(&request).await }
    });
    // A window: an input waiting for the exit leaves no mark to wait on.
    holds_for("the input to wait", Duration::from_millis(300), || async {
        !sent.is_finished()
    })
    .await
    .expect("the input waits for the exit");
    assert!(
        napper.inputs().is_empty(),
        "nothing is handed to the closing process: {:?}",
        napper.inputs()
    );
    napper.die().await;
    let verdict = sent.await.unwrap().expect("the input is answered");
    assert!(
        matches!(verdict.of, Some(wire::send_input_response::Of::Accepted(_))),
        "the next incarnation took it: {verdict:?}"
    );
    assert_eq!(runtime.agent(napper.id).await.unwrap().incarnation, 2);
    let spec = std::fs::read(napper.dir.join("spec.2")).unwrap();
    let spec = <wire::AgentSpec as prost::Message>::decode(spec.as_slice()).unwrap();
    assert!(
        matches!(
            spec.bootstrap.and_then(|bootstrap| bootstrap.of),
            Some(wire::bootstrap::Of::Prompt(prompt)) if prompt.input_id == b"typed-1"
        ),
        "the input is the first"
    );
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// A client acting on a row it holds asks for a wake, as `amux attach` does
/// for an agent its row says is suspended: the agent is resumed only if it
/// still is, and one stopped since is refused, never started again.
#[tokio::test(flavor = "multi_thread")]
async fn a_resume_asked_only_if_suspended_starts_no_stopped_agent() {
    let install = Install::new();
    let napper = SyntheticAgent::new(&install, "napper", SEGMENTS);
    napper.write_spec();
    let mut row = napper.row(&install);
    row.exit_cause = ExitCause::Suspended as i32;
    install.store().put_agent(&row).unwrap();
    let stopped = SyntheticAgent::new(&install, "stopped", SEGMENTS);
    stopped.write_spec();
    let mut row = stopped.row(&install);
    row.lifecycle = Lifecycle::Exited as i32;
    row.exit_cause = ExitCause::Stopped as i32;
    install.store().put_agent(&row).unwrap();
    let daemon = install
        .start(
            "boot-1",
            install.launch(
                "resumed",
                vec![
                    support::text("awake"),
                    provider_fakes::script::Step::TurnEnd,
                ],
            ),
        )
        .await;
    let runtime = runtime(&daemon, &install);
    let client = wire::amux::staging::agent_service_client(served_to(&runtime, None).await);
    let wake = |agent: &SyntheticAgent| {
        tonic::Request::new(wire::amux::staging::ResumeAgentRequest {
            agent_id: agent.id.as_bytes().to_vec(),
            initial_prompt: None,
            if_suspended: true,
        })
    };

    let refused = client
        .clone()
        .resume_agent(wake(&stopped))
        .await
        .expect_err("a stopped agent is not woken");
    assert_eq!(
        refused.code(),
        tonic::Code::FailedPrecondition,
        "{refused:?}"
    );
    assert_eq!(runtime.agent(stopped.id).await.unwrap().incarnation, 1);
    assert!(!stopped.dir.join("spec.2").exists());

    let woken = client
        .clone()
        .resume_agent(wake(&napper))
        .await
        .expect("a suspended agent is woken")
        .into_inner()
        .agent
        .unwrap();
    assert_eq!((woken.lifecycle(), woken.incarnation), (Lifecycle::Live, 2));
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}
