//! The harness's own conformance: topologies validate, real agents run on
//! each fake provider, the driven clock is the one every daemon's policy
//! timers sleep on, each fault verb installs its fault, observations fail
//! when their predicate is stuck, the block invariant catches a broken
//! replica, and the door serves a topology to a real client.

#![cfg(unix)]

mod stories;

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use prost::Message as _;
use provider_fakes::script::Step;
use serde_json::{Value, json};
use store::{Absorb, AgentRow};
use stories::{gate, says, text};
use testnet::door::{CAPABILITIES, Control, ErrorKind, Readiness, Reply};
use testnet::observe::{self, Mark, marks};
use testnet::{
    AgentDecl, FakeKind, Faults, HostDecl, JournalCut, Net, NetError, PATIENCE, Stuck, Topology,
    TopologyError, UdpGate, holds_for, until, until_within,
};
use wire::{ClaudeSdkItem, SessionEvent, claude_sdk_item, session_event};

/// Whether a claude_sdk agent's turn-end row has been seen: the last row
/// its turn writes, so its journal is settled once this shows.
fn turn_ended(events: &[SessionEvent]) -> bool {
    events.iter().any(|event| match &event.of {
        Some(session_event::Of::Item(item)) if item.kind == wire::Kind::ClaudeSdk as i32 => {
            matches!(
                ClaudeSdkItem::decode(item.body.as_slice()).map(|item| item.kind),
                Ok(Some(claude_sdk_item::Kind::Turn(_)))
            )
        }
        _ => false,
    })
}

#[test]
fn a_topology_rejects_unknown_and_duplicate_names_before_anything_starts() {
    let two = || Topology::new().host("a").host("b");
    let cases: Vec<(Topology, &str)> = vec![
        (Topology::new(), "at least one host"),
        (two().host("a"), "declared twice"),
        (Topology::new().host("a b"), "non-empty word"),
        (two().link("a", "c"), "unknown host \"c\""),
        (two().link("a", "a"), "linked to itself"),
        (two().link("a", "b").link("b", "a"), "linked twice"),
        (two().agent(AgentDecl::new("x", "c")), "unknown host \"c\""),
        (
            two()
                .agent(AgentDecl::new("x", "a"))
                .agent(AgentDecl::new("x", "b")),
            "agent \"x\" is declared twice",
        ),
        (
            two()
                .agent(AgentDecl::new("child", "a").parent("parent"))
                .agent(AgentDecl::new("parent", "b")),
            "not declared before it",
        ),
        (
            two().agent(AgentDecl::new("x", "a").branch("feature")),
            "names a branch",
        ),
        (
            two().agent(AgentDecl::new("x", "a").cwd("/tmp").branch("feature")),
            "names a branch",
        ),
        (
            Topology::new().host_decl(HostDecl {
                name: "a".to_owned(),
                process: true,
                router: Some(testnet::RouterKind::Strict),
                ..HostDecl::default()
            }),
            "a router stands only in front of an in-process host",
        ),
    ];
    for (topology, expected) in cases {
        let error = topology.validate().expect_err(expected).to_string();
        assert!(
            error.contains(expected),
            "{error:?} should say {expected:?}"
        );
    }
    two()
        .link("a", "b")
        .agent(AgentDecl::new("parent", "a"))
        .agent(AgentDecl::new("child", "b").parent("parent"))
        .validate()
        .unwrap();

    // The JSON loader reads script files beside the topology and holds the
    // same rules.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("hello.json"),
        r#"{"steps": [{"text": {"chunks": ["hello"]}}, "turn_end"]}"#,
    )
    .unwrap();
    let path = dir.path().join("topology.json");
    std::fs::write(
        &path,
        r#"{"hosts": [{"name": "a"}],
            "agents": [{"name": "x", "host": "a", "kind": "codex", "script_file": "hello.json"}]}"#,
    )
    .unwrap();
    let loaded = Topology::load(&path).unwrap();
    assert_eq!(loaded.agents[0].kind, FakeKind::Codex);
    assert_eq!(
        loaded.agents[0].script.as_ref().unwrap().steps,
        vec![text("hello"), Step::TurnEnd]
    );
    std::fs::write(&path, r#"{"hosts": [{"name": "a", "router": "strict"}]}"#).unwrap();
    assert_eq!(
        Topology::load(&path).unwrap().hosts[0].router,
        Some(testnet::RouterKind::Strict)
    );
    std::fs::write(&path, r#"{"hosts": [{"name": "a", "colour": "red"}]}"#).unwrap();
    assert!(matches!(
        Topology::load(&path),
        Err(TopologyError::Parse { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn the_polling_waiters_fail_on_a_false_a_broken_or_a_hung_check() {
    let deadline = Duration::from_secs(1);
    let never = until_within("never", deadline, || async { Err::<(), _>("still no") }).await;
    match &never {
        Err(Stuck::Deadline { seen, .. }) => assert_eq!(seen, "still no"),
        other => panic!("{other:?}"),
    }
    let hung = until_within("hung", deadline, std::future::pending::<Result<(), String>>).await;
    assert!(matches!(hung, Err(Stuck::Hung { .. })), "{hung:?}");
    until_within("at once", deadline, || async { Ok::<_, String>(()) })
        .await
        .unwrap();

    let broke = holds_for("broken", deadline, || async { false }).await;
    assert!(matches!(broke, Err(Stuck::Broke { .. })), "{broke:?}");
    // The old stability waiter passed here: its check timed out and it
    // took the silence for the condition holding.
    let silent = holds_for("silent", deadline, std::future::pending::<bool>).await;
    assert!(matches!(silent, Err(Stuck::Hung { .. })), "{silent:?}");
    holds_for("steady", deadline, || async { true })
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_agent_runs_on_each_fake_and_its_stream_opens_with_a_snapshot() {
    let mut topology = Topology::new().host("desk");
    for (name, kind) in [
        ("pty", FakeKind::ClaudePty),
        ("sdk", FakeKind::ClaudeSdk),
        ("codex", FakeKind::Codex),
    ] {
        topology = topology.agent(
            AgentDecl::new(name, "desk")
                .kind(kind)
                .steps(vec![text(&format!("hello from {name}")), Step::TurnEnd])
                .prompt("say hello")
                .cwd(name)
                .branch(&format!("{name}-work")),
        );
    }
    let net = Net::start(topology).await.unwrap();
    for name in ["pty", "sdk", "codex"] {
        let mut observer = net.observe("desk", name, 50).await.unwrap();
        let wanted = format!("hello from {name}");
        let events = observer
            .observe_until(|events| says(events, &wanted), PATIENCE)
            .await
            .unwrap();
        assert!(
            matches!(marks(&events[..2])[..], [Mark::Opening(_), Mark::Snapshot]),
            "{name}: a stream opens with its generation and the snapshot"
        );
        println!("{name}:\n{}", observer.transcript());
        // Its folder is a repository on the branch it declares, which its
        // row reports.
        let branch = format!("{name}-work");
        observer
            .observe_until(
                |events| {
                    events.iter().any(|event| match &event.of {
                        Some(session_event::Of::Snapshot(snapshot)) => snapshot
                            .git
                            .as_ref()
                            .is_some_and(|git| git.branch.as_deref() == Some(branch.as_str())),
                        _ => false,
                    })
                },
                PATIENCE,
            )
            .await
            .unwrap();
    }
    net.shutdown().await.unwrap();
}

/// Waits until both daemons' retention sweeps sleep until `at`.
async fn sweeps_asleep(clock: &testnet::DrivenClock, at: i64) {
    testnet::until(&format!("both retention sweeps asleep until {at}"), || {
        let sleeping = clock.sleeping();
        async move {
            let asleep = sleeping.iter().filter(|until| **until == at).count();
            (asleep == 2)
                .then_some(())
                .ok_or_else(|| format!("{asleep} asleep until {at}; sleeping: {sleeping:?}"))
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn every_daemon_sleeps_its_policy_timers_on_the_driven_clock() {
    let net = Net::start(Topology::new().host("a").host("b"))
        .await
        .unwrap();
    let clock = net.clock().expect("a driven net").clone();
    let start = net.now_ms();
    let interval = node::Launch::default().retention_interval_ms;
    // Each daemon's retention sweep ran at start and sleeps one interval on
    // the driven clock; wall time passing wakes neither.
    sweeps_asleep(&clock, start + interval).await;
    // A window: nothing gates the wall, so the only proof policy time
    // ignores it is that it stays put while the wall moves.
    holds_for(
        "policy time to ignore the wall",
        Duration::from_millis(200),
        || async { net.now_ms() == start },
    )
    .await
    .unwrap();

    net.advance(Duration::from_millis(interval as u64)).unwrap();
    sweeps_asleep(&clock, start + 2 * interval).await;
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rewound_host_loses_what_the_drive_never_got_under_a_new_generation() {
    let topology = Topology::new().host("desk").agent(
        AgentDecl::new("worker", "desk")
            .steps(vec![text("one"), Step::TurnEnd, text("two"), Step::TurnEnd])
            .prompt("first"),
    );
    let mut net = Net::start(topology).await.unwrap();
    let generation = net.generation("desk").unwrap();
    let key = net.agent("worker").unwrap().key();
    let mut observer = net.observe("desk", "worker", 50).await.unwrap();
    observer
        .observe_until(|events| says(events, "one"), PATIENCE)
        .await
        .unwrap();
    // Everything the agent wrote so far is committed, then reaches the
    // drive.
    testnet::until("the journal read to its end", || async {
        let end = net.journal_end("worker").unwrap();
        let key = key.clone();
        let cursor = net
            .runtime("desk")
            .unwrap()
            .read_store(move |store| store.cursor(&key).unwrap())
            .await
            .unwrap();
        (cursor == end)
            .then_some(())
            .ok_or_else(|| format!("cursor {cursor} of {end}"))
    })
    .await
    .unwrap();
    let durable = net.journal_end("worker").unwrap();
    assert!(matches!(
        net.rewind_host("desk", &[]).await,
        Err(NetError::NoCheckpoint(_))
    ));
    net.checkpoint_host("desk").await.unwrap();

    net.send("worker", "second").await.unwrap();
    observer
        .observe_until(|events| says(events, "two"), PATIENCE)
        .await
        .unwrap();

    net.rewind_host(
        "desk",
        &[JournalCut {
            agent: "worker".to_owned(),
            byte: durable,
        }],
    )
    .await
    .unwrap();
    assert_eq!(
        net.generation("desk").unwrap(),
        generation + 1,
        "an unclean start under a new boot is a new generation"
    );
    assert_eq!(net.journal_end("worker").unwrap(), durable);
    let mut after = net.observe("desk", "worker", 50).await.unwrap();
    let events = after
        .observe_until(observe::caught_up, PATIENCE)
        .await
        .unwrap();
    assert!(says(events, "one"), "{}", after.transcript());
    assert!(!says(events, "two"), "{}", after.transcript());
    let agent = net
        .runtime("desk")
        .unwrap()
        .agent(net.agent("worker").unwrap().id)
        .await
        .unwrap();
    assert_eq!(
        agent.lifecycle,
        wire::Lifecycle::Exited as i32,
        "the power took the agent too"
    );
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_block_invariant_holds_at_the_origin_and_catches_a_replica_with_a_hole() {
    // The attic is linked to nothing, so no source writes its replica and
    // the hole written there by hand stays.
    let topology = Topology::new()
        .host("desk")
        .host("laptop")
        .host("attic")
        .link("desk", "laptop")
        .agent(
            AgentDecl::new("worker", "desk")
                .steps(vec![text("one"), text("two"), text("three"), Step::TurnEnd])
                .prompt("count"),
        );
    let net = Net::start(topology).await.unwrap();
    // The check wants a settled block: wait for the turn's end, the last
    // row the turn writes, not its last text.
    let mut observer = net.observe("desk", "worker", 50).await.unwrap();
    observer.observe_until(turn_ended, PATIENCE).await.unwrap();
    net.assert_block_invariant("desk", "worker").await.unwrap();
    net.assert_block_invariant("attic", "worker").await.unwrap();
    // The laptop follows the desk: once settled, its block is the desk's.
    let mut replica = net.observe("laptop", "worker", 50).await.unwrap();
    replica
        .observe_until(
            |events| turn_ended(events) && observe::caught_up(events),
            PATIENCE,
        )
        .await
        .unwrap();
    net.assert_block_invariant("laptop", "worker")
        .await
        .unwrap();

    // A replica in the attic that skipped a row: the check names the hole.
    let worker = net.agent("worker").unwrap().clone();
    let key = worker.key();
    let (origin, snapshot) = {
        let runtime = net.runtime("desk").unwrap();
        let key = key.clone();
        let cut = runtime
            .read_store(move |store| store.cut(&key, 50).unwrap())
            .await
            .unwrap();
        (cut.held, cut.snapshot.unwrap_or_default())
    };
    assert!(origin.len() >= 3, "{origin:#?}");
    let mut holed = origin.clone();
    holed.remove(1);
    {
        let runtime = net.runtime("attic").unwrap();
        runtime
            .write_store_beneath(move |store| {
                store
                    .put_agent(&AgentRow::new(key.clone(), wire::Kind::ClaudeSdk, "/"))
                    .unwrap();
                store
                    .absorb(
                        &key,
                        Absorb::Reset {
                            tail: holed,
                            snapshot,
                            generation: 1,
                        },
                    )
                    .unwrap();
            })
            .await
            .unwrap();
    }
    let broken = net
        .assert_block_invariant("attic", "worker")
        .await
        .expect_err("a hole breaks the block");
    let NetError::Block(violation) = &broken else {
        panic!("a block violation, not {broken:?}");
    };
    assert!(violation.problem.contains("hole"), "{violation}");
    println!("{violation}");
    net.shutdown().await.unwrap();
}

/// A connection to the door that sends one request per line and reads one
/// reply per line.
struct Door {
    stream: BufReader<TcpStream>,
}

impl Door {
    fn connect(addr: SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).expect("the door answers");
        stream.set_read_timeout(Some(PATIENCE)).unwrap();
        Self {
            stream: BufReader::new(stream),
        }
    }

    fn send_raw(&mut self, line: &str) {
        let stream = self.stream.get_mut();
        stream.write_all(line.as_bytes()).unwrap();
        stream.write_all(b"\n").unwrap();
    }

    fn reply(&mut self) -> Reply {
        let mut line = String::new();
        self.stream.read_line(&mut line).expect("a reply line");
        serde_json::from_str(&line).unwrap_or_else(|error| panic!("{line:?}: {error}"))
    }

    fn call(&mut self, request: Value) -> Reply {
        self.send_raw(&request.to_string());
        self.reply()
    }

    fn ok(&mut self, request: Value) -> Value {
        match self.call(request.clone()) {
            Reply::Ok(value) => value,
            Reply::Error { kind, message } => panic!("{request}: {kind:?}: {message}"),
        }
    }
}

/// The served process, killed if the test fails before it shuts down.
struct Served(std::process::Child);

impl Drop for Served {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn error_kind(reply: Reply) -> ErrorKind {
    match reply {
        Reply::Error { kind, .. } => kind,
        Reply::Ok(value) => panic!("an error, not {value}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_door_serves_a_topology_to_a_real_client_once_it_publishes_readiness() {
    let dir = tempfile::tempdir().unwrap();
    let topology = dir.path().join("two-hosts.json");
    std::fs::write(
        &topology,
        json!({
            "hosts": [{"name": "desk"}, {"name": "laptop"}],
            "links": [{"a": "desk", "b": "laptop"}],
            "agents": [
                {"name": "writer", "host": "desk", "prompt": "write",
                 "script": {"steps": [{"text": {"chunks": ["a draft"]}}, "turn_end"]}},
                {"name": "reviewer", "host": "laptop", "kind": "codex"}
            ]
        })
        .to_string(),
    )
    .unwrap();
    let mut served = Served(
        Command::new(env!("CARGO_BIN_EXE_testnet"))
            .arg("serve")
            .arg(&topology)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("testnet serve starts"),
    );
    // Readiness is one line, published once the net is ready: nothing is
    // assumed by sleeping.
    let mut stdout = BufReader::new(served.0.stdout.take().unwrap());
    let readiness: Readiness = tokio::task::spawn_blocking(move || {
        let mut line = String::new();
        stdout.read_line(&mut line).expect("a readiness line");
        serde_json::from_str(&line).unwrap_or_else(|error| panic!("{line:?}: {error}"))
    })
    .await
    .unwrap();
    assert_eq!(
        readiness
            .hosts
            .iter()
            .map(|host| host.name.as_str())
            .collect::<Vec<_>>(),
        ["desk", "laptop"]
    );
    assert_eq!(readiness.agents.len(), 2);

    // The real client: the amux binary, pointed at the served host's
    // installation config, lists its agent.
    let desk = &readiness.hosts[0];
    let listed = tokio::process::Command::new(testnet::Binaries::built().amux())
        .arg("--config")
        .arg(&desk.config)
        .arg("ls")
        .env_remove("AMUX_LOG")
        .output()
        .await
        .unwrap();
    let listing = String::from_utf8_lossy(&listed.stdout);
    assert!(
        listed.status.success() && listing.contains("writer"),
        "amux ls: {listing}\n{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    println!("amux ls against the served desk:\n{listing}");

    let control = readiness.control;
    tokio::task::spawn_blocking(move || {
        let mut door = Door::connect(control);
        // Dispatch and results.
        // The laptop's fleet is its own agent and, once its inventory of
        // the desk has caught up, the desk's.
        let deadline = Instant::now() + PATIENCE;
        loop {
            let inventory = door.ok(json!({"Inventory": {"host": "laptop"}}));
            let mut names: Vec<&str> = inventory["agents"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|agent| agent["name"].as_str())
                .collect();
            names.sort_unstable();
            assert!(names.contains(&"reviewer"), "{names:?}");
            if names == ["reviewer", "writer"] {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the desk's agent never listed: {names:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        // Beside the agents, the hosts they run on, with the trust a
        // client filters them by.
        let inventory = door.ok(json!({"Inventory": {"host": "laptop"}}));
        assert!(
            inventory["hosts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|host| host["name"] == "desk" && host["trust"] == "TRUSTED"),
            "{inventory}"
        );
        assert_eq!(
            door.ok(json!({"Link": {"a": "desk", "b": "laptop"}}))["up"],
            true
        );
        door.ok(json!({"Sever": {"a": "desk", "b": "laptop"}}));
        let deadline = Instant::now() + PATIENCE;
        while door.ok(json!({"Link": {"a": "desk", "b": "laptop"}}))["up"] == true {
            assert!(Instant::now() < deadline, "the severed link stays up");
            std::thread::sleep(Duration::from_millis(50));
        }
        door.ok(json!({"Restore": {"a": "desk", "b": "laptop"}}));

        // Validation and error propagation, typed.
        assert_eq!(
            error_kind(door.call(json!({"Sever": {"a": "desk"}}))),
            ErrorKind::Invalid
        );
        door.send_raw("not json");
        assert_eq!(error_kind(door.reply()), ErrorKind::Invalid);
        assert_eq!(
            error_kind(door.call(json!({"KillDaemon": {"host": "nowhere"}}))),
            ErrorKind::Invalid
        );
        assert_eq!(
            error_kind(door.call(json!({"Advance": {"ms": 1000}}))),
            ErrorKind::Refused,
            "a served net runs on wall time"
        );
        assert_eq!(
            error_kind(door.call(json!({"Rewind": {"host": "desk"}}))),
            ErrorKind::Refused,
            "no checkpoint yet"
        );

        // Ordering: requests written together are answered in order.
        door.send_raw(
            &[
                json!({"Link": {"a": "desk", "b": "nowhere"}}),
                json!({"OpenGate": {"name": "g"}}),
                json!({"Block": {"host": "laptop", "agent": "writer"}}),
            ]
            .map(|request| request.to_string())
            .join("\n"),
        );
        assert_eq!(error_kind(door.reply()), ErrorKind::Invalid);
        assert!(matches!(door.reply(), Reply::Ok(value) if value["installed"] == "gate g open"));
        assert_eq!(door.reply(), Reply::Ok(json!("holds")));

        // Teardown: Shutdown answers, the process ends, the socket closes.
        assert_eq!(door.ok(json!("Shutdown")), json!("shut down"));
    })
    .await
    .unwrap();
    let exited = until("testnet serve to exit after Shutdown", || {
        std::future::ready(
            served
                .0
                .try_wait()
                .unwrap()
                .ok_or_else(|| "still running".to_owned()),
        )
    })
    .await
    .unwrap();
    assert!(exited.success(), "{exited:?}");
    assert!(
        TcpStream::connect(control).is_err(),
        "the control socket closed"
    );
}

#[test]
fn every_door_verb_maps_to_one_capability_and_the_crate_docs_list_the_same_map() {
    let samples = [
        Control::Sever {
            a: "a".into(),
            b: "b".into(),
        },
        Control::Restore {
            a: "a".into(),
            b: "b".into(),
        },
        Control::Link {
            a: "a".into(),
            b: "b".into(),
        },
        Control::Trust {
            a: "a".into(),
            b: "b".into(),
        },
        Control::Untrust {
            a: "a".into(),
            b: "b".into(),
        },
        Control::RefuseCalls {
            a: "a".into(),
            b: "b".into(),
            refuse: true,
        },
        Control::SetTier {
            account: "a".into(),
            tier: testnet::TierDecl::Free,
        },
        Control::SignIn {
            host: "a".into(),
            account: "b".into(),
        },
        Control::KillDaemon { host: "a".into() },
        Control::StopDaemon { host: "a".into() },
        Control::RestartDaemon { host: "a".into() },
        Control::Checkpoint { host: "a".into() },
        Control::Rewind {
            host: "a".into(),
            cuts: vec![JournalCut {
                agent: "x".into(),
                byte: 7,
            }],
        },
        Control::Advance { ms: 1 },
        Control::Spawn {
            agent: Box::new(AgentDecl::new("x", "a")),
        },
        Control::Resume {
            agent: "x".into(),
            text: None,
        },
        Control::Send {
            agent: "x".into(),
            text: "hi".into(),
        },
        Control::Stop { agent: "x".into() },
        Control::Freeze { agent: "x".into() },
        Control::Thaw { agent: "x".into() },
        Control::OpenGate { name: "g".into() },
        Control::LanGate { host: "a".into() },
        Control::LanFaults {
            host: "a".into(),
            delay_ms: 100,
            loss_percent: 0,
            jitter_ms: 0,
            reorder_percent: 0,
            rate_bytes_per_sec: 0,
        },
        Control::RelayGate {},
        Control::RelayFaults {
            delay_ms: 100,
            loss_percent: 0,
            jitter_ms: 0,
            reorder_percent: 0,
            rate_bytes_per_sec: 0,
        },
        Control::NetFaults {
            target: "a".into(),
            delay_ms: 10,
            loss_percent: 1,
            jitter_ms: 5,
            reorder_percent: 2,
            rate_bytes_per_sec: 1000,
        },
        Control::BlockUdp {
            host: "a".into(),
            blocked: true,
        },
        Control::SetRouter {
            host: "a".into(),
            kind: testnet::RouterKind::Strict,
        },
        Control::ChangeNetwork { host: "a".into() },
        Control::BindIpv6Only { host: "a".into() },
        Control::CarryingPath {
            host: "a".into(),
            peer: "b".into(),
        },
        Control::LinkLog { host: "a".into() },
        Control::Flap {
            a: "a".into(),
            b: "b".into(),
            down_ms: 1,
            up_ms: 1,
        },
        Control::FlapStop {},
        Control::PairGate {
            from: "a".into(),
            to: "b".into(),
        },
        Control::AddHost {
            host: Box::new(testnet::HostDecl {
                name: "c".into(),
                ..testnet::HostDecl::default()
            }),
        },
        Control::RemoveHost { host: "c".into() },
        Control::AddLink {
            a: "a".into(),
            b: "b".into(),
        },
        Control::RemoveLink {
            a: "a".into(),
            b: "b".into(),
        },
        Control::AddAccount {
            account: testnet::AccountDecl {
                name: "x".into(),
                tier: testnet::TierDecl::Pro,
            },
        },
        Control::Settle {
            agent: "x".into(),
            input: None,
            timeout_ms: Some(5),
        },
        Control::Release {
            version: "9.9.9".into(),
            channel: None,
            rollout: Some(10),
            broken: false,
        },
        Control::Status {},
        Control::Inventory { host: "a".into() },
        Control::Block {
            host: "a".into(),
            agent: "x".into(),
        },
        Control::Chat {
            host: "a".into(),
            agent: "x".into(),
        },
        Control::ProviderInput { agent: "x".into() },
        Control::Shutdown,
    ];
    let mut verbs: Vec<&str> = samples.iter().map(Control::verb).collect();
    verbs.sort_unstable();
    verbs.dedup();
    assert_eq!(verbs.len(), samples.len(), "one sample per verb");
    let mut mapped: Vec<&str> = CAPABILITIES.iter().map(|(verb, _)| *verb).collect();
    mapped.sort_unstable();
    assert_eq!(mapped, verbs, "every verb has exactly one capability");
    for sample in &samples {
        let text = serde_json::to_string(sample).unwrap();
        assert!(text.contains(sample.verb()), "{text}");
        assert_eq!(&serde_json::from_str::<Control>(&text).unwrap(), sample);
    }

    let docs = include_str!("../src/lib.rs");
    let table: Vec<(String, String)> = docs
        .lines()
        .skip_while(|line| !line.contains("door-capabilities:start"))
        .take_while(|line| !line.contains("door-capabilities:end"))
        .filter_map(|line| {
            let cells: Vec<&str> = line
                .trim_start_matches("//!")
                .split('|')
                .map(str::trim)
                .collect();
            (cells.len() == 4 && cells[1].starts_with('`')).then(|| {
                (
                    cells[1].trim_matches('`').to_owned(),
                    cells[2].trim_matches('`').to_owned(),
                )
            })
        })
        .collect();
    let expected: Vec<(String, String)> = CAPABILITIES
        .iter()
        .map(|(verb, capability)| ((*verb).to_owned(), (*capability).to_owned()))
        .collect();
    assert_eq!(
        table, expected,
        "the crate docs' table is the capability map"
    );
}

fn wall() -> testnet::NetOptions {
    testnet::NetOptions {
        clock: testnet::ClockMode::Wall,
        ..testnet::NetOptions::default()
    }
}

fn lan_host(name: &str) -> HostDecl {
    HostDecl {
        name: name.to_owned(),
        lan: true,
        ..HostDecl::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_net_grows_hosts_links_and_accounts_and_sheds_them_while_it_runs() {
    let mut net = Net::start_with(Topology::new(), testnet::NetOptions::default())
        .await
        .unwrap();
    assert!(net.host_names().is_empty(), "a net may start with no host");
    net.add_host(lan_host("desk")).await.unwrap();
    net.add_host(lan_host("laptop")).await.unwrap();
    assert!(matches!(
        net.add_host(lan_host("desk")).await,
        Err(NetError::Topology(TopologyError::DuplicateHost(..)))
    ));
    net.add_link("desk", "laptop").await.unwrap();
    assert!(net.link_up("desk", "laptop").await);
    net.spawn(
        AgentDecl::new("worker", "desk")
            .steps(vec![text("grown"), Step::TurnEnd])
            .prompt("go"),
    )
    .await
    .unwrap();
    net.settle("worker").await.unwrap();
    let mut laptop = net.observe_inventory("laptop").await.unwrap();
    laptop
        .observe_until(
            |events| {
                observe::inventory_agents(events)
                    .iter()
                    .any(|agent| agent.name == "worker")
            },
            PATIENCE,
        )
        .await
        .unwrap();

    net.remove_link("desk", "laptop").await.unwrap();
    net.wait_link("desk", "laptop", false).await.unwrap();
    assert!(net.topology().links.is_empty());

    net.add_account(testnet::AccountDecl {
        name: "ada".to_owned(),
        tier: testnet::TierDecl::Pro,
    })
    .await
    .unwrap();
    assert!(net.relay().is_ok(), "the first account starts the relay");
    net.add_host(HostDecl {
        account: Some("ada".to_owned()),
        ..lan_host("phone-less")
    })
    .await
    .unwrap();

    let removed = net.host("laptop").unwrap().clone();
    net.remove_host("laptop").await.unwrap();
    assert_eq!(net.host_names(), vec!["desk", "phone-less"]);
    assert!(matches!(net.host("laptop"), Err(NetError::NoHost(..))));
    // A host added under a removed one's name is a new machine, not the
    // removed one coming back with its profile and agents.
    net.add_host(lan_host("laptop")).await.unwrap();
    let added = net.host("laptop").unwrap();
    assert_ne!(added.dir, removed.dir);
    assert_ne!(added.host_id, removed.host_id);
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_gate_reorders_jitters_and_caps_the_rate_of_what_it_passes() {
    async fn pass(faults: Faults, count: usize, size: usize) -> (Vec<u8>, Duration) {
        let sink = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let gate = UdpGate::bind(Some(sink.local_addr().unwrap())).unwrap();
        gate.set_faults(faults);
        let sender = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let started = Instant::now();
        for n in 0..count {
            let mut datagram = vec![0_u8; size.max(1)];
            datagram[0] = n as u8;
            sender.send_to(&datagram, gate.addr()).await.unwrap();
        }
        let mut order = Vec::new();
        let mut buffer = vec![0_u8; 65_536];
        while order.len() < count {
            let (_, _) = tokio::time::timeout(Duration::from_secs(10), sink.recv_from(&mut buffer))
                .await
                .expect("every datagram arrives")
                .unwrap();
            order.push(buffer[0]);
        }
        (order, started.elapsed())
    }
    let in_order: Vec<u8> = (0..40).collect();

    let (order, _) = pass(
        Faults {
            delay: Duration::from_millis(5),
            jitter: Duration::from_millis(20),
            ..Faults::default()
        },
        40,
        1,
    )
    .await;
    assert_eq!(order, in_order, "jitter alone keeps the wire's order");

    let (order, _) = pass(
        Faults {
            delay: Duration::from_millis(20),
            reorder_percent: 50,
            ..Faults::default()
        },
        40,
        1,
    )
    .await;
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, in_order, "reordering loses nothing");
    assert_ne!(order, in_order, "half held back: some are overtaken");

    // Ten kilobytes at ten a second take a second, within the second's
    // worth the gate queues before it drops.
    let (_, took) = pass(
        Faults {
            rate_bytes_per_sec: 10_000,
            ..Faults::default()
        },
        10,
        1_000,
    )
    .await;
    assert!(
        took >= Duration::from_millis(800),
        "the rate cap spaces them out: {took:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_process_host_pairs_for_real_through_gates_that_cut_it_and_survives_a_crash() {
    assert!(matches!(
        Net::start(Topology::new().host_decl(HostDecl {
            process: true,
            ..lan_host("box")
        }))
        .await,
        Err(NetError::Process { .. })
    ));

    let mut net = Net::start_with(
        Topology::new(),
        testnet::NetOptions {
            gated_lan: true,
            ..wall()
        },
    )
    .await
    .unwrap();
    net.add_host(HostDecl {
        process: true,
        discovery: true,
        ..lan_host("box")
    })
    .await
    .unwrap();
    net.add_host(HostDecl {
        discovery: true,
        ..lan_host("desk")
    })
    .await
    .unwrap();
    assert!(net.host("box").unwrap().process);
    net.add_link("desk", "box").await.unwrap();
    assert!(net.link_up("desk", "box").await);

    net.spawn(
        AgentDecl::new("worker", "box")
            .steps(vec![
                text("from a process"),
                Step::TurnEnd,
                text("while cut"),
                Step::TurnEnd,
                text("after the crash"),
                Step::TurnEnd,
            ])
            .prompt("go"),
    )
    .await
    .unwrap();
    net.turn_ended("worker", 1).await.unwrap();
    let mut replica = net.observe("desk", "worker", 50).await.unwrap();
    replica
        .observe_until(|events| says(events, "from a process"), PATIENCE)
        .await
        .unwrap();

    // The pair's gate cuts a link the hosts made by pairing, silently, as
    // a dead network does: nothing crosses until it is restored, and then
    // what was held back arrives.
    net.sever_link("desk", "box").unwrap();
    net.send("worker", "again").await.unwrap();
    net.turn_ended("worker", 2).await.unwrap();
    assert!(
        replica
            .observe_until(|events| says(events, "while cut"), Duration::from_secs(2))
            .await
            .is_err(),
        "nothing crosses a cut link"
    );
    net.restore_link("desk", "box").await.unwrap();
    replica
        .observe_until(|events| says(events, "while cut"), PATIENCE)
        .await
        .unwrap();

    // A crash kills the daemon process; its agent runs on in its grace and
    // the next daemon adopts it.
    net.kill_daemon("box").await.unwrap();
    assert!(!net.is_up("box"));
    net.restart_daemon("box").await.unwrap();
    net.send("worker", "once more").await.unwrap();
    net.turn_ended("worker", 3).await.unwrap();

    assert!(
        net.checkpoint_host("box").await.is_err(),
        "reaching into a process's store is refused"
    );
    net.stop_daemon("box").await.unwrap();
    assert!(!net.is_up("box"));

    // Removed, it is gone for good: no daemon left running, and no longer
    // advertised to browsers.
    net.restart_daemon("box").await.unwrap();
    let removed = net.host("box").unwrap().clone();
    assert!(net.advertised().contains(&removed.host_id));
    net.remove_host("box").await.unwrap();
    assert!(
        !net.advertised().contains(&removed.host_id),
        "a removed host is not advertised"
    );
    let left = Command::new("pgrep")
        .arg("-f")
        .arg(&removed.dir)
        .output()
        .unwrap();
    assert!(
        left.stdout.is_empty(),
        "nothing of the removed host runs on: {}",
        String::from_utf8_lossy(&left.stdout)
    );
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_supervised_host_takes_a_published_release_and_rolls_back_a_broken_one() {
    let mut net = Net::start_with(Topology::new(), wall()).await.unwrap();
    net.add_host(HostDecl {
        supervised: true,
        ..lan_host("box")
    })
    .await
    .unwrap();
    let install = net.host("box").unwrap().dir.join("install/amux");
    let supervisor_log = net.host("box").unwrap().data_dir.join("supervisor.log");
    let version = || {
        Command::new(&install)
            .arg("--version")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .unwrap_or_default()
    };
    assert!(
        version().ends_with(testnet::SUPERVISED_VERSION),
        "{}",
        version()
    );

    net.publish_release("0.0.2", "stable", None, false)
        .await
        .unwrap();
    until("the supervisor to run 0.0.2", || {
        let log = std::fs::read_to_string(&supervisor_log).unwrap_or_default();
        let now = version();
        async move {
            (log.contains("supervising version=\"0.0.2\"") && now.ends_with("0.0.2"))
                .then_some(())
                .ok_or(now)
        }
    })
    .await
    .unwrap();

    net.publish_release("0.0.3", "stable", None, true)
        .await
        .unwrap();
    until("the supervisor to reject 0.0.3 and put 0.0.2 back", || {
        let log = std::fs::read_to_string(&supervisor_log).unwrap_or_default();
        let now = version();
        async move {
            (log.contains("rejected=0.0.3")
                && log.contains("put prev back")
                && now.ends_with("0.0.2"))
            .then_some(())
            .ok_or(now)
        }
    })
    .await
    .unwrap();

    // A crash under a supervisor is followed by a start it makes itself.
    net.kill_daemon("box").await.unwrap();
    net.spawn(
        AgentDecl::new("worker", "box")
            .steps(vec![text("after the crash"), Step::TurnEnd])
            .prompt("go"),
    )
    .await
    .unwrap();
    net.turn_ended("worker", 1).await.unwrap();
    // Removing it takes the supervisor with the daemon.
    let dir = net.host("box").unwrap().dir.clone();
    net.remove_host("box").await.unwrap();
    let left = Command::new("pgrep").arg("-f").arg(&dir).output().unwrap();
    assert!(
        left.stdout.is_empty(),
        "nothing of the removed host runs on: {}",
        String::from_utf8_lossy(&left.stdout)
    );
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_a_host_leaves_a_host_whose_name_it_starts_alone() {
    let mut net = Net::start_with(Topology::new(), testnet::NetOptions::default())
        .await
        .unwrap();
    net.add_host(lan_host("desk")).await.unwrap();
    net.add_host(lan_host("desk2")).await.unwrap();
    net.spawn(
        AgentDecl::new("held", "desk2")
            .steps(vec![gate("go"), text("still here"), Step::TurnEnd])
            .prompt("wait"),
    )
    .await
    .unwrap();
    net.remove_host("desk").await.unwrap();
    net.open_gate("go").unwrap();
    net.turn_ended("held", 1).await.unwrap();
    net.shutdown().await.unwrap();
}

/// The plan the net's account service gives `account` when the phone
/// reads it: the `access` of its answer to `/api/graphql`.
fn plan(net: &Net, account: &str) -> Value {
    let url = net.relay().unwrap().url().to_owned();
    let addr = url.trim_start_matches("http://");
    let body = r#"{"query":"{ me { access { pro until grant { __typename } } } }"}"#;
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "POST /api/graphql HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer access-{account}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut answer = String::new();
    std::io::Read::read_to_string(&mut stream, &mut answer).unwrap();
    let (_, json) = answer.split_once("\r\n\r\n").expect("an HTTP answer");
    serde_json::from_str::<Value>(json).unwrap()["data"]["me"]["access"].take()
}

/// A changed plan reaches a host running as its own process as at once as
/// one in the net's process, and the plan read a phone makes says the same:
/// given access while Pro, access that ended when it went Free.
#[tokio::test(flavor = "multi_thread")]
async fn a_changed_tier_reaches_a_process_host_and_the_plan_read_at_once() {
    let mut net = Net::start_with(Topology::new().relay(&["ada"]), wall())
        .await
        .unwrap();
    net.add_host(HostDecl {
        process: true,
        account: Some("ada".to_owned()),
        ..HostDecl {
            name: "box".to_owned(),
            ..HostDecl::default()
        }
    })
    .await
    .unwrap();
    let tier = async |net: &Net| {
        net.front_door("box")
            .await
            .unwrap()
            .list_profiles(wire::Empty {})
            .await
            .unwrap()
            .into_inner()
            .profiles[0]
            .relay_connected()
            .map(wire::RelayConnected::tier)
    };
    assert_eq!(tier(&net).await, Some(wire::Tier::Pro));
    assert_eq!(plan(&net, "ada")["pro"], true);

    let said = net.set_tier("ada", testnet::TierDecl::Free).await.unwrap();
    assert!(said.installed.contains("box learned it"), "{said:?}");
    assert_eq!(
        tier(&net).await,
        Some(wire::Tier::Free),
        "box knows at once"
    );
    let lapsed = plan(&net, "ada");
    assert_eq!(lapsed["pro"], false, "{lapsed}");
    assert!(lapsed["until"].is_string(), "the access ended: {lapsed}");
    assert_eq!(
        net.topology().relay.as_ref().unwrap().accounts[0].tier,
        testnet::TierDecl::Free,
        "the net's own account of it, which `ctl status` prints, follows"
    );

    net.set_tier("ada", testnet::TierDecl::Pro).await.unwrap();
    assert_eq!(tier(&net).await, Some(wire::Tier::Pro));
    assert_eq!(plan(&net, "ada")["until"], Value::Null);
    net.shutdown().await.unwrap();
}

/// Two in-process hosts with no LAN between them, for links a test makes
/// by hand.
fn bare_host(name: &str) -> HostDecl {
    HostDecl {
        name: name.to_owned(),
        ..HostDecl::default()
    }
}

/// Waits until `near` reaches `far` by `via`, as `near` sees it.
async fn until_route(net: &Net, near: &str, far: &str, via: node::harness::LiveRoute) {
    let edge = net.edge(near).unwrap();
    let far_id = net.host(far).unwrap().host_id;
    until(&format!("{near} to reach {far} {via:?}"), || {
        let edge = edge.clone();
        async move {
            let seen = edge.via(far_id).await;
            (seen == via).then_some(()).ok_or(format!("{seen:?}"))
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_ssh_stand_in_carries_a_link_into_the_far_hosts_link_socket() {
    let mut net = Net::start_with(Topology::new(), testnet::NetOptions::default())
        .await
        .unwrap();
    net.add_host(bare_host("desk")).await.unwrap();
    net.add_host(bare_host("laptop")).await.unwrap();
    let desk = net.host("desk").unwrap().clone();
    let target = net.ssh_target("desk").unwrap();

    // The desk trusts the laptop, so its link socket admits the laptop's
    // pinned handshake; the laptop trusts the desk as an SSH pairing does,
    // with the target and profile it reaches the desk by.
    net.trust("desk", "laptop").await.unwrap();
    let desk_edge = net.edge("desk").unwrap();
    net.edge("laptop")
        .unwrap()
        .trust_ssh_peer(wire::TrustSshPeerRequest {
            peer: Some(wire::PairingIdentity {
                host_id: desk.host_id.as_bytes().to_vec(),
                pubkey: desk_edge.public_key().to_vec(),
                name: "desk".to_owned(),
                expires_at_ms: 0,
            }),
            ssh_target: Some(wire::SshTarget {
                target: target.clone(),
                profile_id: desk.profile.as_bytes().to_vec(),
            }),
        })
        .await
        .unwrap();
    until_route(&net, "laptop", "desk", node::harness::LiveRoute::Ssh).await;
    until("the laptop's link log to name SSH", || async {
        match net.carrying_path("laptop", "desk") {
            Ok(Some(node::link_log::PathKind::Ssh)) => Ok(()),
            other => Err(format!("{other:?}")),
        }
    })
    .await
    .unwrap();
    println!("the laptop reached the desk over the stand-in at {target}");
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_link_behind_a_router_survives_its_host_changing_network() {
    let mut net = Net::start_with(Topology::new(), testnet::NetOptions::default())
        .await
        .unwrap();
    net.add_host(lan_host("desk")).await.unwrap();
    net.add_host(lan_host("laptop")).await.unwrap();
    net.set_router("laptop", testnet::RouterKind::Ordinary)
        .unwrap();
    net.trust("desk", "laptop").await.unwrap();
    net.trust("laptop", "desk").await.unwrap();
    let desk = net.host("desk").unwrap().host_id;
    let laptop = net.edge("laptop").unwrap();
    assert!(
        laptop.lan_addr().is_some(),
        "the laptop listens behind its router"
    );
    // From behind an ordinary router the laptop's dial opens the mapping
    // the desk's answers come back through.
    laptop.dial(desk, net.edge("desk").unwrap().lan_addr().unwrap());
    until_route(&net, "laptop", "desk", node::harness::LiveRoute::Direct).await;
    let ups = |net: &Net| {
        net.link_log("laptop")
            .unwrap()
            .iter()
            .filter(|event| event.event == node::link_log::LinkTransition::Up)
            .count()
    };
    let before = ups(&net);

    // A new network is a new router with new public ports: the connection
    // migrates, and the link neither drops nor comes up again.
    println!("{}", net.change_network("laptop").unwrap().installed);
    let mut peer = net
        .edge("laptop")
        .unwrap()
        .peer(desk)
        .await
        .expect("a channel to the desk after the move");
    let mut inventory = peer
        .subscribe_inventory(wire::Empty {})
        .await
        .expect("the desk answers across the moved link")
        .into_inner();
    inventory
        .message()
        .await
        .expect("the desk's inventory streams across the moved link")
        .expect("a first event");
    assert_eq!(
        laptop.via(desk).await,
        node::harness::LiveRoute::Direct,
        "the link stays direct"
    );
    assert_eq!(ups(&net), before, "the link did not come up again");
    assert!(matches!(
        net.change_network("nobody"),
        Err(NetError::NoHost(_))
    ));
    net.shutdown().await.unwrap();
}
