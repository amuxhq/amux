//! The drivers against a real node in process, running a real agent on a
//! fake provider, through both clients: the in-process call the phone makes
//! and gRPC over the profile socket the terminal dials.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use client::{Client, GrpcClient, SystemClock};
use provider_fakes::script::Step;
use support::until_within;
use testnet::{AgentDecl, FakeKind, Net, Topology};
use ui_runtime::{Fleet, Session, Window};
use ui_state::{Connection, InputState};

use crate::support;

const PATIENCE: Duration = Duration::from_secs(20);
/// The window's cap: more rows than any case here delivers.
const CAP: u32 = 200;

fn text(text: &str) -> Step {
    Step::Text {
        chunks: vec![text.to_owned()],
    }
}

fn topology() -> Topology {
    let mut steps = Vec::new();
    for turn in ["one", "two", "three", "four"] {
        steps.push(text(&format!("turn {turn}")));
        steps.push(Step::TurnEnd);
    }
    steps.push(text("reply to the session"));
    steps.push(Step::TurnEnd);
    Topology::new().host("desk").agent(
        AgentDecl::new("worker", "desk")
            .kind(FakeKind::ClaudeSdk)
            .steps(steps)
            .prompt("go"),
    )
}

async fn grpc(net: &Net) -> GrpcClient {
    let profiles = net
        .front_door("desk")
        .await
        .unwrap()
        .list_profiles(wire::Empty {})
        .await
        .unwrap()
        .into_inner()
        .profiles;
    GrpcClient::connect(profiles[0].socket_path.as_ref())
        .await
        .expect("the profile socket answers")
}

fn says(session: &Session, wanted: &str) -> bool {
    session
        .state()
        .transcript()
        .iter()
        .any(|held| held.item.text.contains(wanted))
}

/// The trace one line per event, items by key and revision.
fn compact(trace: &ui_runtime::DriverTrace<ui_state::SessionState, ui_state::Msg>) -> String {
    use ui_runtime::TraceEvent;
    use ui_state::Msg;
    use wire::session_event::Of;
    let mut out = String::new();
    for traced in &trace.events {
        let line = match &traced.event {
            TraceEvent::Driver(event) => format!("driver  {event:?}"),
            TraceEvent::Msg(Msg::Event(event)) => match &event.of {
                Some(Of::Item(item)) => format!(
                    "item    {} order {} revision {} {:?}",
                    item.key, item.order, item.revision, item.text
                ),
                Some(Of::Snapshot(snapshot)) => format!(
                    "snapshot revision {} queue {}",
                    snapshot.revision,
                    snapshot.queue.len()
                ),
                Some(Of::Append(append)) => {
                    format!("append  {} -> revision {}", append.key, append.revision)
                }
                Some(other) => format!("marker  {other:?}"),
                None => "empty".into(),
            },
            TraceEvent::Msg(Msg::Send(input)) => {
                format!("send    {:?}", ui_state::InputWhat::of(input))
            }
            TraceEvent::Msg(Msg::Sent(_, outcome)) => format!("sent    {outcome:?}"),
            TraceEvent::Msg(other) => format!("msg     {other:?}"),
        };
        out.push_str(&format!("{:>3} {line}\n", traced.seq));
    }
    out
}

/// The fleet lists the agent, a session opens on it, a prompt lands and
/// is answered, and history pages in from below.
async fn drive(client: Arc<dyn Client>, net: &Net) {
    let fleet = Fleet::connect(client, SystemClock).await.unwrap();
    assert!(fleet.state().caught_up());
    let worker = net.agent("worker").unwrap().id;
    let agent = fleet
        .state()
        .find(worker.as_bytes())
        .map(ui_state::agent_key)
        .expect("the fleet lists the worker");

    // The first turn is written before the session sends anything.
    let session = fleet
        .open(&agent, Window { tail: 2, cap: CAP })
        .await
        .unwrap();
    until_within(PATIENCE, session.changed(), || {
        (session.state().caught_up() && says(&session, "turn one")).then_some(())
    })
    .await;
    for turn in ["two", "three", "four"] {
        let sent = session
            .send_prompt(&format!("next {turn}"), Vec::new())
            .await;
        until_within(PATIENCE, session.changed(), || {
            (session.state().input_state(&sent.id) == Some(InputState::Settled)).then_some(())
        })
        .await;
        until_within(PATIENCE, session.changed(), || {
            says(&session, &format!("turn {turn}")).then_some(())
        })
        .await;
    }
    let sent = session.send_prompt("and now?", Vec::new()).await;
    until_within(PATIENCE, session.changed(), || {
        (says(&session, "reply to the session")
            && session.state().input_state(&sent.id) == Some(InputState::Settled))
        .then_some(())
    })
    .await;

    // The window started two rows from the head; page it down to order one.
    let mut pages = 0;
    while session.state().transcript().has_older() {
        session
            .page_older(3)
            .await
            .expect("an own agent's history pages");
        pages += 1;
        assert!(pages < 50, "paging never reached the first row");
    }
    assert_eq!(session.state().oldest_order(), Some(1));
    assert!(says(&session, "turn one"), "paged in from below");
    // Events still arrive (the idle snapshot trails the turn's last row),
    // so the trace and the state it must replay to are read together.
    let state = session.state();
    assert_eq!(state.trace().replay(), *state);
    drop(state);
    // A real driver's trace, paging and sends and all, replays alike from
    // its record.
    replay_support::record::replays_alike(&session.history())
        .expect("the record replays alike")
        .expect("the window holds");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_and_the_fleet_run_over_the_profile_socket_against_a_real_node() {
    let net = Net::start(topology()).await.unwrap();
    let client: Arc<dyn Client> = Arc::new(grpc(&net).await);
    drive(client, &net).await;
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_over_the_socket_reconnects_across_a_daemon_restart() {
    let mut net = Net::start(
        Topology::new().host("desk").agent(
            AgentDecl::new("worker", "desk")
                .kind(FakeKind::ClaudeSdk)
                .steps(vec![
                    text("before"),
                    Step::TurnEnd,
                    text("after the restart"),
                    Step::TurnEnd,
                ])
                .prompt("go"),
        ),
    )
    .await
    .unwrap();
    let client: Arc<dyn Client> = Arc::new(grpc(&net).await);
    let fleet = Fleet::connect(client, SystemClock).await.unwrap();
    let worker = net.agent("worker").unwrap().id;
    let agent = fleet
        .state()
        .find(worker.as_bytes())
        .map(ui_state::agent_key)
        .unwrap();
    let session = fleet
        .open(&agent, Window { tail: 40, cap: CAP })
        .await
        .unwrap();
    until_within(PATIENCE, session.changed(), || {
        (session.state().caught_up() && says(&session, "before")).then_some(())
    })
    .await;

    net.kill_daemon("desk").await.unwrap();
    until_within(PATIENCE, session.changed(), || {
        (session.state().connection() == Connection::Reconnecting).then_some(())
    })
    .await;
    until_within(PATIENCE, fleet.changed(), || {
        (fleet.state().connection() == Connection::Reconnecting).then_some(())
    })
    .await;
    net.restart_daemon("desk").await.unwrap();
    until_within(PATIENCE, session.changed(), || {
        let state = session.state();
        (state.connection() == Connection::Live && state.caught_up()).then_some(())
    })
    .await;
    until_within(PATIENCE, fleet.changed(), || {
        (fleet.state().caught_up() && fleet.state().find(worker.as_bytes()).is_some()).then_some(())
    })
    .await;
    let sent = session.send_prompt("still there?", Vec::new()).await;
    until_within(PATIENCE, session.changed(), || {
        (says(&session, "after the restart")
            && session.state().input_state(&sent.id) == Some(InputState::Settled))
        .then_some(())
    })
    .await;
    println!("{}", compact(&session.state().trace()));
    fleet.close(&agent);
    drop(session);
    drop(fleet);
    net.shutdown().await.unwrap();
}

/// The text of the draft desk's daemon keeps for `agent`, once it reads
/// `wanted`.
async fn kept_reads(client: &dyn Client, agent: &[u8], wanted: &str) {
    let mut last = String::new();
    let reached = tokio::time::timeout(PATIENCE, async {
        loop {
            last = client
                .get_draft(wire::GetDraftRequest {
                    agent_id: agent.to_vec(),
                })
                .await
                .unwrap()
                .text;
            if last == wanted {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        reached.is_ok(),
        "the kept draft never read {wanted:?} (last {last:?})"
    );
}

/// A prompt sent while its agent's host can't be reached never left: it is
/// refused at once, never uncertain, and nothing sends it again when the
/// host is back. Its words are the composer's again, and the draft this
/// machine keeps keeps them; sent again by the person, it goes once.
#[tokio::test(flavor = "multi_thread")]
async fn a_prompt_for_an_unreachable_host_is_refused_and_goes_only_when_sent_again() {
    let mut net = Net::start(
        Topology::new()
            .host("desk")
            .host("laptop")
            .link("desk", "laptop")
            .agent(
                AgentDecl::new("far", "laptop")
                    .kind(FakeKind::ClaudeSdk)
                    .steps(vec![
                        text("Ready."),
                        Step::TurnEnd,
                        text("Tests pass."),
                        Step::TurnEnd,
                    ])
                    .prompt("go"),
            ),
    )
    .await
    .unwrap();
    net.settle("far").await.unwrap();
    let client: Arc<dyn Client> = Arc::new(client::InProcess::new(net.client("desk").unwrap()));
    let fleet = Fleet::connect(client.clone(), SystemClock).await.unwrap();
    let far = net.agent("far").unwrap().id;
    let agent = until_within(PATIENCE, fleet.changed(), || {
        fleet.state().find(far.as_bytes()).map(ui_state::agent_key)
    })
    .await;
    let session = fleet
        .open(&agent, Window { tail: 20, cap: CAP })
        .await
        .unwrap();
    until_within(PATIENCE, session.changed(), || {
        (session.state().caught_up() && says(&session, "Ready.")).then_some(())
    })
    .await;

    net.sever_link("desk", "laptop").unwrap();
    until_within(PATIENCE, session.changed(), || {
        let state = session.state();
        state
            .host()
            .is_some_and(|host| host.online().is_none())
            .then_some(())
    })
    .await;
    let sent = session.send_prompt("Run the tests.", Vec::new()).await;
    let refused = Some(InputState::Rejected(ui_state::Refusal::of(
        wire::Refusal::HostUnreachable,
    )));
    assert_eq!(session.state().input_state(&sent.id), refused);
    // The composer takes the words back, and the kept draft is its words.
    fleet.keep_draft(
        &agent,
        wire::Draft {
            text: "Run the tests.".into(),
            attachments: Vec::new(),
        },
    );
    kept_reads(&*client, far.as_bytes(), "Run the tests.").await;

    net.restore_link("desk", "laptop").await.unwrap();
    until_within(PATIENCE, session.changed(), || {
        let state = session.state();
        (state.caught_up() && state.host().is_some_and(|host| host.online().is_some()))
            .then_some(())
    })
    .await;
    assert_eq!(
        session.state().input_state(&sent.id),
        refused,
        "nothing sent the refused prompt again"
    );
    assert!(!says(&session, "Run the tests."));
    fleet.keep_draft(&agent, wire::Draft::default());
    session.send_prompt("Run the tests.", Vec::new()).await;
    until_within(PATIENCE, session.changed(), || {
        says(&session, "Tests pass.").then_some(())
    })
    .await;
    let prompts = session
        .state()
        .transcript()
        .iter()
        .filter(|held| held.item.text.contains("Run the tests."))
        .count();
    assert_eq!(prompts, 1, "the prompt reached the agent once");
    kept_reads(&*client, far.as_bytes(), "").await;
    fleet.close(&agent);
    drop(session);
    drop(fleet);
    net.shutdown().await.unwrap();
}

/// A report from the terminal while a chat is on screen, whatever asked
/// for it: the fleet hands the daemon the chat's record, which the bundle
/// carries beside the core's and the agent's, and nothing anybody wrote
/// (the agent's name, folder and branch, its prompts, a tool's file name,
/// input and output, and the replies) reaches any of its files.
#[tokio::test(flavor = "multi_thread")]
async fn no_report_carries_a_word_of_the_chat_on_screen() {
    const MARKER: &str = "walrus-umbrella-marker";
    let net = Net::start(
        Topology::new().host("desk").agent(
            AgentDecl::new(&format!("worker-{MARKER}"), "desk")
                .kind(FakeKind::ClaudeSdk)
                .cwd(&format!("work-{MARKER}"))
                .branch(&format!("{MARKER}-branch"))
                .steps(vec![
                    text("ready"),
                    Step::TurnEnd,
                    Step::Tool(provider_fakes::script::Tool {
                        name: Some("Read".into()),
                        class: provider_fakes::script::ToolClass::Exploration,
                        input: Some(serde_json::json!({
                            "file_path": format!("notes/{MARKER}.txt"),
                        })),
                        outcome: provider_fakes::script::Outcome {
                            output: format!("the file says {MARKER}"),
                            ..Default::default()
                        },
                        wait_for: None,
                    }),
                    text(&format!("the {MARKER} answered")),
                    Step::TurnEnd,
                ])
                .prompt(&format!("go, {MARKER}")),
        ),
    )
    .await
    .unwrap();
    let client: Arc<dyn Client> = Arc::new(grpc(&net).await);
    let fleet = Fleet::connect(client.clone(), SystemClock).await.unwrap();
    let worker = net.agent(&format!("worker-{MARKER}")).unwrap().id;
    let agent = fleet
        .state()
        .find(worker.as_bytes())
        .map(ui_state::agent_key)
        .unwrap();
    let session = fleet
        .open(&agent, Window { tail: 40, cap: CAP })
        .await
        .unwrap();
    until_within(PATIENCE, session.changed(), || {
        (session.state().caught_up() && says(&session, "ready")).then_some(())
    })
    .await;
    session
        .send_prompt(&format!("tell the {MARKER}"), Vec::new())
        .await;
    until_within(PATIENCE, session.changed(), || {
        says(&session, &format!("the {MARKER} answered")).then_some(())
    })
    .await;

    let forms = [
        MARKER.as_bytes().to_vec(),
        MARKER.to_uppercase().into_bytes(),
        MARKER
            .bytes()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
            .into_bytes(),
    ];
    for (automatic, check) in [
        (false, ""),
        (true, "daemon.test"),
        (true, "agent.test"),
        (true, ui_runtime::TRANSCRIPT_CHECK),
    ] {
        let records = fleet.on_screen_records();
        assert_eq!(records.len(), 1, "the one chat on screen");
        let bundle = client
            .dump(wire::DumpRequest {
                reason: format!("asked by {check}"),
                automatic,
                check: check.to_owned(),
                sessions: records,
            })
            .await
            .unwrap()
            .report_path;
        let bundle = std::path::PathBuf::from(bundle);
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(bundle.join("manifest.json")).unwrap()).unwrap();
        let sessions = manifest["client"]["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1, "{manifest}");
        assert!(
            bundle
                .join(sessions[0]["events"].as_str().unwrap())
                .is_file()
        );
        assert!(bundle.join(format!("agents/{worker}/record")).is_file());
        assert!(bundle.join("core/checkpoint").is_file());
        let mut dirs = vec![bundle.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                let bytes = std::fs::read(&path).unwrap();
                for form in &forms {
                    assert!(
                        !bytes.windows(form.len()).any(|window| window == form),
                        "{check:?}: {} holds what was written",
                        path.display()
                    );
                }
            }
        }
    }

    // Off screen, the fleet's own session is no part of a report.
    fleet.close(&agent);
    assert!(fleet.on_screen_records().is_empty());
    drop(session);
    drop(fleet);
    net.shutdown().await.unwrap();
}
