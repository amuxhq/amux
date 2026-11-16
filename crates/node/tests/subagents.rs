//! Provider subagents through a real daemon: the agents Claude's Agent tool
//! and Codex's child threads start inside one of this host's agents, its
//! host, which journals their parts of each step. They have no process, so
//! a person's input to one goes to its host's, and the start's sweep has
//! no directory to look for.

use std::sync::Arc;

use node::{Daemon, ProfileRuntime};
use store::{AgentKey, AgentRow};
use support::synthetic::*;
use support::*;
use uuid::Uuid;
use wire::{
    AgentStep, ClaudeAnswer, ClaudeSdkInput, Input, Lifecycle, NativeStart, PromptInput,
    QuestionAnswer, SendInputRequest, SendInputResponse, Step, claude_answer, claude_sdk_input,
    send_input_response,
};

use crate::support;

const SEGMENTS: u64 = 1 << 20;

async fn start(install: &Install, boot: &str) -> (Daemon, Arc<ProfileRuntime>) {
    let daemon = install.start(boot, quiet_launch()).await;
    let runtime = runtime(&daemon, install);
    (daemon, runtime)
}

/// The host's step that starts the subagent `sub`.
fn starts(host: &SyntheticAgent, sub: Uuid) -> Step {
    Step {
        agents: vec![AgentStep {
            agent_id: sub.as_bytes().to_vec(),
            start: Some(NativeStart {
                provider_child_id: "task-1".into(),
                call_agent_id: host.id.as_bytes().to_vec(),
                call_key: "agent-call".into(),
                title: "Read the logs".into(),
            }),
            ..AgentStep::default()
        }],
        ..Step::default()
    }
}

fn input(id: &[u8], of: claude_sdk_input::Of) -> Input {
    Input::new(id.to_vec(), ClaudeSdkInput { of: Some(of) })
}

fn verdict(response: &SendInputResponse) -> String {
    match &response.of {
        Some(send_input_response::Of::Accepted(_)) => "accepted".into(),
        Some(send_input_response::Of::Rejected(rejected)) => {
            format!("rejected {:?}", rejected.refusal())
        }
        None => "none".into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_or_a_job_stop_for_a_subagent_goes_to_its_hosts_process_and_nothing_else_does() {
    let install = Install::new();
    let mut host = SyntheticAgent::new(&install, "host", SEGMENTS);
    host.register(&install);
    host.go_live();
    let (daemon, runtime) = start(&install, "boot-1").await;
    let sub = Uuid::new_v4();
    host.append(&starts(&host, sub));
    runtime.ingest(host.id).await.unwrap();
    let send = |id: &[u8], of| SendInputRequest {
        agent_id: sub.as_bytes().to_vec(),
        input: Some(input(id, of)),
    };

    let answer = claude_sdk_input::Of::Answer(ClaudeAnswer {
        ask_key: "ask-1".into(),
        of: Some(claude_answer::Of::Question(QuestionAnswer::default())),
    });
    let verdict_of = runtime.send_input(&send(b"i1", answer)).await.unwrap();
    assert_eq!(verdict(&verdict_of), "accepted", "the host's verdict");
    let stop = claude_sdk_input::Of::StopJob(wire::StopJob {
        key: "task-1".into(),
    });
    runtime.send_input(&send(b"i2", stop)).await.unwrap();
    assert_eq!(
        host.child_inputs(),
        vec![
            (sub.as_bytes().to_vec(), b"i1".to_vec()),
            (sub.as_bytes().to_vec(), b"i2".to_vec())
        ],
        "each went to the host for the subagent"
    );

    // A prompt, or anything else, is refused before it reaches anyone.
    let prompt = claude_sdk_input::Of::Prompt(PromptInput {
        text: "hello".into(),
        attachments: Vec::new(),
    });
    let refused = runtime.send_input(&send(b"i3", prompt)).await.unwrap();
    assert_eq!(verdict(&refused), "rejected Unsupported");
    let interrupt = claude_sdk_input::Of::Interrupt(wire::Interrupt {});
    let refused = runtime.send_input(&send(b"i4", interrupt)).await.unwrap();
    assert_eq!(verdict(&refused), "rejected Unsupported");
    assert_eq!(
        host.inputs().len(),
        2,
        "only the answer and the stop reached the host"
    );

    // Once it has ended, as any agent that has.
    host.append(&Step {
        agents: vec![AgentStep {
            agent_id: sub.as_bytes().to_vec(),
            end: Some(wire::IncarnationEnd {
                cause: wire::ExitCause::Finished as i32,
                detail: None,
            }),
            ..AgentStep::default()
        }],
        ..Step::default()
    });
    runtime.ingest(host.id).await.unwrap();
    let late = claude_sdk_input::Of::StopJob(wire::StopJob {
        key: "task-1".into(),
    });
    let refused = runtime.send_input(&send(b"i5", late)).await.unwrap();
    assert_eq!(verdict(&refused), "rejected Exited");
    daemon.shutdown().await.unwrap();
}

/// A subagent's row as its host's ingest would have left it, with no
/// directory: subagents have none.
fn subagent_row(install: &Install, host: &SyntheticAgent, live: bool) -> AgentRow {
    let key = AgentKey::new(
        support::synthetic::host(install).as_bytes().to_vec(),
        Uuid::new_v4().as_bytes().to_vec(),
    );
    let mut row = AgentRow::new(key, KIND, "/src");
    row.name = format!(
        "sub-{}",
        &Uuid::from_slice(&row.agent.agent)
            .unwrap()
            .simple()
            .to_string()[..8]
    );
    row.parent = Some(host.key(install));
    row.owes_report = false;
    row.native = Some(wire::NativeChild {
        provider_child_id: "task".into(),
    });
    if !live {
        row.lifecycle = Lifecycle::Exited as i32;
        row.exit_cause = wire::ExitCause::Finished as i32;
    }
    row
}

#[tokio::test(flavor = "multi_thread")]
async fn the_starts_sweep_leaves_subagents_to_their_hosts() {
    let install = Install::new();
    let mut running = SyntheticAgent::new(&install, "running", SEGMENTS);
    running.register(&install);
    running.go_live();
    let gone = SyntheticAgent::new(&install, "gone", SEGMENTS);
    gone.register(&install);
    let under_running = subagent_row(&install, &running, true);
    let under_gone = subagent_row(&install, &gone, true);
    let ended = subagent_row(&install, &running, false);
    for row in [&under_running, &under_gone, &ended] {
        install.store().put_agent(row).unwrap();
    }
    let (daemon, runtime) = start(&install, "boot-1").await;
    let read = |row: &AgentRow| {
        let key = row.agent.clone();
        let runtime = runtime.clone();
        async move {
            runtime
                .read_store(move |store| store.agent(&key).unwrap().unwrap())
                .await
                .unwrap()
        }
    };

    // No directory is no failure: its host's process still runs it.
    let row = read(&under_running).await;
    assert_eq!(row.lifecycle, Lifecycle::Live as i32);
    // Its host ended while no daemon was there, and it ended with it.
    let row = read(&under_gone).await;
    assert_eq!(
        (row.lifecycle, row.exit_cause),
        (
            Lifecycle::Exited as i32,
            wire::ExitCause::EndedWhileAway as i32
        )
    );
    let row = read(&ended).await;
    assert_eq!(row.exit_cause, wire::ExitCause::Finished as i32);
    let markers = runtime
        .read_store(|store| store.markers().clone())
        .await
        .unwrap();
    assert_eq!(
        markers.get(&ended.agent),
        Some(&store::Marker::CaughtUp),
        "an ended subagent reads as current"
    );
    daemon.shutdown().await.unwrap();
}
