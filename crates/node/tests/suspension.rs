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
        id: b"wake-up".to_vec(),
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
            Some(wire::bootstrap::Of::AgentMessage(message)) if message.id == b"wake-up"
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
