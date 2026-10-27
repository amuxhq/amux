//! Product analytics at the daemon's choke points: what a start says about
//! the run before it, a person's inputs and a client opening, and the daily
//! check-in, each recorded into a test's sink instead of being sent.

mod support;

use std::sync::Arc;

use analytics::{Ask, Client, Event, Kind, On, Recording};
use node::{ClientApi, Daemon, Generation, ProfileRuntime, StartOptions, Telemetry};
use support::synthetic::*;
use support::*;
use tonic::Request;
use wire::agent_service_server::AgentService as _;
use wire::session_service_server::SessionService as _;
use wire::{
    ClaudeAnswer, ClaudeSdkInput, Input, Interrupt, Kind as WireKind, PromptInput, QuestionAnswer,
    SendInputRequest, claude_answer, claude_sdk_input,
};

const SEGMENTS: u64 = 1 << 20;
const T0: i64 = 1_000_000_000;

fn recording_options(install: &Install, boot: &str, recording: &Arc<Recording>) -> StartOptions {
    StartOptions {
        analytics: Telemetry::Record(recording.clone()),
        ..install.options(boot, quiet_launch())
    }
}

async fn start(install: &Install, boot: &str, recording: &Arc<Recording>) -> Daemon {
    node::start(recording_options(install, boot, recording), None)
        .await
        .expect("the daemon starts")
}

fn sdk(of: claude_sdk_input::Of) -> Input {
    Input::new(
        uuid::Uuid::new_v4().as_bytes().to_vec(),
        ClaudeSdkInput { of: Some(of) },
    )
}

fn send(agent: &SyntheticAgent, input: Input) -> Request<SendInputRequest> {
    Request::new(SendInputRequest {
        agent_id: agent.id.as_bytes().to_vec(),
        input: Some(input),
    })
}

/// Rewrites the generation file as a run of another build would have left
/// it.
fn previous_run(install: &Install, version: &str, clean: bool) {
    let path = install.data_dir.join(node::GENERATION);
    let mut last = Generation::read(&install.data_dir).unwrap().unwrap();
    last.version = version.to_owned();
    last.clean = clean;
    std::fs::write(path, serde_json::to_vec(&last).unwrap()).unwrap();
}

fn names(recording: &Recording) -> Vec<&'static str> {
    recording
        .events()
        .iter()
        .map(|(_, event)| event.name())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_reports_a_first_start_a_crash_an_update_and_a_rollback() {
    let install = Install::new();
    let current: semver::Version = node::version().parse().unwrap();

    let recording = Recording::new();
    let daemon = start(&install, "boot-1", &recording).await;
    let host = runtime(&daemon, &install).host();
    assert_eq!(names(&recording), ["installed"]);
    assert_eq!(
        recording.events()[0].0,
        host,
        "the profile's host records it"
    );
    daemon.shutdown().await.unwrap();

    // The run before was an older build that died on this same boot.
    previous_run(&install, "0.0.1", false);
    let recording = Recording::new();
    let daemon = start(&install, "boot-1", &recording).await;
    let old: semver::Version = "0.0.1".parse().unwrap();
    assert_eq!(
        recording
            .events()
            .into_iter()
            .map(|(_, event)| event)
            .collect::<Vec<_>>(),
        [
            Event::DaemonCrashed {
                version: Some(old.clone())
            },
            Event::Updated {
                from: old,
                to: current.clone()
            },
        ]
    );
    daemon.shutdown().await.unwrap();

    // A newer build ran and stopped cleanly, and this older one was put
    // back; a reboot in between is no crash.
    previous_run(&install, "99.0.0", false);
    let recording = Recording::new();
    let daemon = start(&install, "boot-2", &recording).await;
    assert_eq!(
        recording
            .events()
            .into_iter()
            .map(|(_, event)| event)
            .collect::<Vec<_>>(),
        [Event::UpdateRolledBack {
            from: "99.0.0".parse().unwrap(),
            to: current,
        }]
    );
    daemon.shutdown().await.unwrap();

    // The same build, shut down cleanly: nothing to say.
    let recording = Recording::new();
    let daemon = start(&install, "boot-2", &recording).await;
    assert!(names(&recording).is_empty(), "{:?}", names(&recording));
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_persons_prompts_and_answers_count_and_keys_do_not() {
    let install = Install::new();
    let mut worker = SyntheticAgent::new(&install, "worker", SEGMENTS);
    worker.register_offline(&install);
    worker.go_live();
    let recording = Recording::new();
    let daemon = start(&install, "boot-1", &recording).await;
    let runtime: Arc<ProfileRuntime> = runtime(&daemon, &install);
    let person = ClientApi::new(&runtime, None).opened_as(Client::Terminal);

    // The terminal lists the agents when it starts; listing again within
    // the hour is the same visit.
    for _ in 0..2 {
        drop(
            person
                .subscribe_inventory(Request::new(wire::SubscribeInventoryRequest {}))
                .await
                .unwrap(),
        );
    }

    let prompt = sdk(claude_sdk_input::Of::Prompt(PromptInput {
        text: "a prompt the analytics never carries".into(),
        attachments: Vec::new(),
    }));
    person
        .send_input(send(&worker, prompt.clone()))
        .await
        .unwrap();
    let answer = sdk(claude_sdk_input::Of::Answer(ClaudeAnswer {
        ask_key: "ask-1".into(),
        of: Some(claude_answer::Of::Question(QuestionAnswer::default())),
    }));
    person.send_input(send(&worker, answer)).await.unwrap();
    let interrupt = sdk(claude_sdk_input::Of::Interrupt(Interrupt {}));
    person.send_input(send(&worker, interrupt)).await.unwrap();

    // The same service without the terminal's mark, as the phone calls it
    // in process, counts no client of its own.
    let plain = ClientApi::new(&runtime, None);
    drop(
        plain
            .subscribe_inventory(Request::new(wire::SubscribeInventoryRequest {}))
            .await
            .unwrap(),
    );

    let host = runtime.host();
    let events: Vec<_> = recording
        .events()
        .into_iter()
        .filter(|(_, event)| event.name() != "installed")
        .collect();
    assert_eq!(
        events,
        [
            (
                host,
                Event::ClientOpened {
                    client: Client::Terminal
                }
            ),
            (
                host,
                Event::PromptSent {
                    kind: Kind::ClaudeSdk,
                    on: On::ThisHost,
                    queued: true
                }
            ),
            (
                host,
                Event::AskAnswered {
                    kind: Kind::ClaudeSdk,
                    on: On::ThisHost,
                    ask: Ask::Question
                }
            ),
        ]
    );
    assert_eq!(worker.inputs().len(), 3, "every input reached the agent");
    daemon.shutdown().await.unwrap();
}

/// The analytics read a copy of an input's body. A body this daemon cannot
/// read, from a newer client or for a kind it does not know, counts as
/// nothing and still reaches the agent as sent: only the agent decides it.
#[tokio::test(flavor = "multi_thread")]
async fn undecodable_input_is_still_delivered() {
    let install = Install::new();
    let mut worker = SyntheticAgent::new(&install, "worker", SEGMENTS);
    worker.register_offline(&install);
    worker.go_live();
    let recording = Recording::new();
    let daemon = start(&install, "boot-1", &recording).await;
    let runtime: Arc<ProfileRuntime> = runtime(&daemon, &install);
    let person = ClientApi::new(&runtime, None);

    let unreadable = [
        // Not a protobuf message: a field key with no value after it.
        Input {
            input_id: b"torn".to_vec(),
            kind: WireKind::ClaudeSdk as i32,
            body: vec![0x0a],
        },
        // A kind this daemon has no name for.
        Input {
            input_id: b"newer-kind".to_vec(),
            kind: 99,
            body: vec![0x0a, 0x00],
        },
    ];
    for input in &unreadable {
        person
            .send_input(send(&worker, input.clone()))
            .await
            .expect("an input the daemon cannot read is still relayed");
    }

    let handed = worker
        .inputs()
        .into_iter()
        .filter_map(|handed| match handed {
            Handed::Input(input) => Some(input),
            Handed::Message(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(handed, unreadable, "each reached the agent byte for byte");
    let events: Vec<_> = recording
        .events()
        .into_iter()
        .filter(|(_, event)| event.name() != "installed")
        .collect();
    assert!(events.is_empty(), "nothing was counted: {events:?}");
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_profile_checks_in_a_minute_after_start_and_daily_after_that() {
    let install = Install::new();
    let mut worker = SyntheticAgent::new(&install, "worker", SEGMENTS);
    worker.register_offline(&install);
    worker.go_live();
    let clock = agent_dir::ManualClock::new(T0);
    let recording = Recording::new();
    let daemon = node::start(
        StartOptions {
            clock: Arc::new(clock.clone()),
            ..recording_options(&install, "boot-1", &recording)
        },
        None,
    )
    .await
    .unwrap();
    let runtime = runtime(&daemon, &install);

    // Two turns end before the first check-in.
    worker.append(&turn_end(1, "a"));
    worker.append(&turn_end(2, "b"));
    worker.nudge().await;
    until("both turns to commit", || async {
        let frames = runtime.ingested_frames();
        (frames >= 2)
            .then_some(())
            .ok_or_else(|| format!("{frames} frames"))
    })
    .await
    .unwrap();

    let first = T0 + 60_000;
    clock.armed(first).await;
    clock.set(first);
    until("the first check-in", || async {
        (!recording.named("checked_in").is_empty())
            .then_some(())
            .ok_or("none yet")
    })
    .await
    .unwrap();
    let Event::CheckedIn(check_in) = recording.named("checked_in").remove(0) else {
        unreachable!()
    };
    assert_eq!(check_in.agents_running.get(Kind::ClaudeSdk), 1);
    assert_eq!(
        check_in.agents_created_24h.get(Kind::ClaudeSdk),
        0,
        "the worker was created long before"
    );
    assert_eq!(check_in.turns_24h.get(Kind::ClaudeSdk), 2);
    assert_eq!(check_in.paired_hosts, 0);
    assert!(!check_in.signed_in);
    // The event is recorded before the time is written down.
    let marked = install.profile_dir().join("checked_in");
    until("the check-in to be written down", || async {
        let read = std::fs::read_to_string(&marked).unwrap_or_default();
        (read == first.to_string())
            .then_some(())
            .ok_or(format!("{read:?}"))
    })
    .await
    .unwrap();

    let second = first + 24 * 60 * 60 * 1000;
    clock.armed(second).await;
    clock.set(second);
    until("the second check-in", || async {
        (recording.named("checked_in").len() == 2)
            .then_some(())
            .ok_or("one so far")
    })
    .await
    .unwrap();
    let Event::CheckedIn(check_in) = recording.named("checked_in").remove(1) else {
        unreachable!()
    };
    assert_eq!(
        check_in.turns_24h.get(Kind::ClaudeSdk),
        0,
        "the turns were reported once"
    );
    daemon.shutdown().await.unwrap();
}
