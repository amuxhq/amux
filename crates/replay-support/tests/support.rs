//! The bundles the suites replay, written at test time by the build under
//! test, so a bundle always reads under the layout its reader expects. A
//! healthy bundle is a real daemon's `amux dump`; the two that break a
//! property are that bundle with the break planted, as a build with the bug
//! would have written it.

use std::path::Path;

use daemon_core::payload::ClientRequest;
use daemon_core::{CallId, Checkpoint, Daemon, Input, Recorded, StartOptions};
use prost::Message as _;
use provider_fakes::script::Step;
use replay_support::bundle::Bundle;
use store::{AgentKey, AgentRow, Sqlite};
use testnet::{AgentDecl, Net, Topology};
use ui_state::{Connection, Msg, SessionState};
use wire::dump::{ClientSessionEvents, ClientTraced, client_traced};
use wire::{SessionEvent, session_event};

/// A bundle in a directory of its own, removed with it.
pub struct Dumped {
    dir: tempfile::TempDir,
}

impl Dumped {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The one agent's id, as the bundle names it.
    pub fn agent(&self) -> String {
        let bundle = Bundle::open(self.path()).expect("the bundle opens");
        let [agent] = &bundle.agents[..] else {
            panic!("{} agents in the bundle", bundle.agents.len());
        };
        agent.id.clone()
    }

    pub fn manifest(&self) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(self.path().join("manifest.json")).unwrap()).unwrap()
    }

    fn set_manifest(&self, manifest: &serde_json::Value) {
        std::fs::write(
            self.path().join("manifest.json"),
            serde_json::to_vec_pretty(manifest).unwrap(),
        )
        .unwrap();
    }
}

/// `amux dump` on a net of one host and one headless Claude agent on the
/// fake provider, after two turns: thinking and an answer, then another,
/// each streamed in two chunks. No client is attached, as from the command line.
pub fn healthy() -> Dumped {
    let text = |chunks: &[&str]| Step::Text {
        chunks: chunks.iter().map(|&chunk| chunk.to_owned()).collect(),
    };
    let dir = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let net = Net::start(
            Topology::new().host("desk").agent(
                AgentDecl::new("worker", "desk")
                    .steps(vec![
                        Step::Thinking {
                            text: "thinking it over".to_owned(),
                        },
                        text(&["first answer", " more"]),
                        Step::TurnEnd,
                        text(&["second answer", " more"]),
                        Step::TurnEnd,
                    ])
                    .prompt("first question"),
            ),
        )
        .await
        .unwrap();
        net.turn_ended("worker", 1).await.unwrap();
        net.send("worker", "second question").await.unwrap();
        net.turn_ended("worker", 2).await.unwrap();
        let written = net
            .runtime("desk")
            .unwrap()
            .dump(wire::DumpRequest {
                reason: "a healthy net".to_owned(),
                ..wire::DumpRequest::default()
            })
            .await
            .unwrap();
        copy_dir(&written, dir.path());
        net.shutdown().await.unwrap();
    });
    Dumped { dir }
}

/// The healthy bundle with the core's part a build would have written whose
/// core recorded the highest generation a stream opened under as more than
/// it was. Its checkpoint holds that record at 2 under a run of generation
/// 1, and the inputs since open a stream: the replay breaks "the generation
/// this host's streams open under only ever rises" at the Subscribe, the
/// second input.
pub fn core_violation() -> Dumped {
    let dumped = healthy();
    let id = |text: &str| uuid::Uuid::parse_str(text).unwrap().into_bytes();
    let agent = id(&dumped.agent());
    let host = id(dumped.manifest()["host"].as_str().unwrap());
    let core = dumped.path().join(replay_support::bundle::CORE);
    std::fs::remove_dir_all(&core).unwrap();
    std::fs::create_dir_all(&core).unwrap();
    let mut store = Sqlite::open(&core.join("store.sqlite"), host.to_vec()).unwrap();
    let row = AgentRow::new(
        AgentKey::new(host.to_vec(), agent.to_vec()),
        wire::Kind::ClaudeSdk,
        "/src/amux",
    );
    store.put_agent(&row).unwrap();
    let subscribe = |call| Input::ClientCall {
        call: CallId(call),
        request: ClientRequest::SubscribeSession {
            agent_id: agent.to_vec(),
            tail: 16,
        },
    };
    let run = |generation| StartOptions {
        generation,
        ..StartOptions::default()
    };
    // A stream opened under generation 2 leaves the record at 2. The run's
    // options are the last part of a checkpoint, so swapping their bytes
    // for generation 1's leaves the record where it was under a run that
    // serves generation 1.
    let mut daemon = Daemon::new(store, run(2));
    daemon.step(subscribe(1), &mut Vec::new());
    let mut state = daemon.checkpoint().state;
    drop(daemon);
    let options = postcard::to_allocvec(&run(2)).unwrap();
    assert!(state.ends_with(&options), "the run's options end the state");
    state.truncate(state.len() - options.len());
    state.extend(postcard::to_allocvec(&run(1)).unwrap());
    std::fs::write(core.join("checkpoint"), Checkpoint { state }.to_bytes()).unwrap();
    let inputs = [
        Input::Tick {
            now_ms: 1,
            link_ms: 1,
        },
        subscribe(2),
    ];
    let mut written = String::new();
    for input in inputs {
        written.push_str(&serde_json::to_string(&Recorded::Input(Box::new(input))).unwrap());
        written.push('\n');
    }
    std::fs::write(core.join("inputs"), written).unwrap();
    dumped
}

/// The healthy bundle with the session a client had open, as a session
/// driver that dropped the row at order 2 would have traced it: the
/// session as its trace began, then the connection, the stream's Opening,
/// and the store's opening cut without that row, through its CaughtUp. The
/// replay breaks the window's one block at the row at order 3, the fifth
/// message.
pub fn client_violation() -> Dumped {
    let dumped = healthy();
    let bundle = Bundle::open(dumped.path()).unwrap();
    let agent = &bundle.agents[0];
    let opening = agent
        .opening()
        .into_iter()
        .filter(|of| !matches!(of, session_event::Of::Item(item) if item.order == 2));
    let msgs = [
        Msg::Connection(Connection::Live),
        Msg::Event(SessionEvent {
            of: Some(session_event::Of::Opening(wire::Opening { generation: 1 })),
        }),
    ]
    .into_iter()
    .chain(opening.map(|of| Msg::Event(SessionEvent { of: Some(of) })));
    let traced = std::iter::once(client_traced::Event::Driver(
        "Subscribed { tail: 16 }".to_owned(),
    ))
    .chain(msgs.map(|msg| client_traced::Event::Msg(msg.to_wire())))
    .enumerate()
    .map(|(seq, event)| ClientTraced {
        seq: seq as u64 + 1,
        at_ms: 0,
        event: Some(event),
    })
    .collect::<Vec<_>>();

    let at = format!("client/sessions/{}", agent.id);
    std::fs::create_dir_all(dumped.path().join(&at)).unwrap();
    let checkpoint = format!("{at}/checkpoint");
    let events = format!("{at}/events");
    let start = SessionState::new(agent.row.clone(), 16).to_wire();
    std::fs::write(dumped.path().join(&checkpoint), start.encode_to_vec()).unwrap();
    let count = traced.len();
    std::fs::write(
        dumped.path().join(&events),
        ClientSessionEvents { traced }.encode_to_vec(),
    )
    .unwrap();
    let mut manifest = dumped.manifest();
    manifest["client"]["sessions"] = serde_json::json!([{
        "agent_id": agent.id,
        "checkpoint": checkpoint,
        "events": events,
        "traced": count,
    }]);
    dumped.set_manifest(&manifest);
    dumped
}

/// A redaction placeholder with its category set aside: the facts and the
/// journal are redacted apart, and name the fake provider's account email
/// `<REDACTED_IDENTIFIER>` and `<REDACTED_EMAIL>`.
pub fn placeholders_aside(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("<REDACTED_") {
        out.push_str(&rest[..at]);
        let end = rest[at..].find('>').map_or(rest.len(), |end| at + end + 1);
        out.push_str("<REDACTED>");
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
