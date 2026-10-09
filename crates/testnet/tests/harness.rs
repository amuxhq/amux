//! The harness's own conformance: topologies validate, real agents run on
//! each fake provider, the driven clock is the one every daemon's policy
//! timers sleep on, each fault verb installs its fault, observations fail
//! when their predicate is stuck, the block invariant catches a broken
//! replica, and the door serves a topology to a real client.

#![cfg(unix)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use prost::Message as _;
use provider_fakes::script::Step;
use serde_json::{Value, json};
use store::{Absorb, AgentRow};
use testnet::door::{CAPABILITIES, Control, ErrorKind, Readiness, Reply};
use testnet::observe::{self, Mark, marks};
use testnet::{
    AgentDecl, FakeKind, JournalCut, Net, NetError, PATIENCE, Stuck, Topology, TopologyError,
    holds_for, until, until_within,
};
use wire::{ClaudeSdkItem, SessionEvent, claude_sdk_item, session_event};

fn text(text: &str) -> Step {
    Step::Text {
        chunks: vec![text.to_owned()],
    }
}

fn gate(name: &str) -> Step {
    Step::WaitFor { path: name.into() }
}

/// The texts of every item seen, in order.
fn texts(events: &[SessionEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.of {
            Some(session_event::Of::Item(item)) => Some(item.text.clone()),
            _ => None,
        })
        .collect()
}

fn says(events: &[SessionEvent], wanted: &str) -> bool {
    texts(events).iter().any(|text| text.contains(wanted))
}

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
async fn a_stuck_observation_fails_at_its_deadline_and_a_closed_one_at_once() {
    let topology = Topology::new().host("desk").agent(
        AgentDecl::new("worker", "desk")
            .steps(vec![text("started"), gate("never"), Step::TurnEnd])
            .prompt("go"),
    );
    let mut net = Net::start(topology).await.unwrap();
    let mut observer = net.observe("desk", "worker", 50).await.unwrap();
    observer
        .observe_until(|events| says(events, "started"), PATIENCE)
        .await
        .unwrap();

    let stuck = observer
        .observe_until(
            |events| says(events, "finished"),
            Duration::from_millis(300),
        )
        .await
        .expect_err("nothing says finished");
    let Stuck::Deadline { seen, .. } = &stuck else {
        panic!("a deadline, not {stuck:?}");
    };
    assert!(
        seen.contains("started"),
        "the failure shows what it saw: {seen}"
    );

    // The daemon going away ends the stream: the observer says so at once
    // instead of waiting out a long deadline.
    net.kill_daemon("desk").await.unwrap();
    let began = Instant::now();
    let closed = observer
        .observe_until(|events| says(events, "finished"), PATIENCE)
        .await
        .expect_err("a closed stream never finishes");
    assert!(matches!(closed, Stuck::Closed { .. }), "{closed:?}");
    assert!(began.elapsed() < Duration::from_secs(5));
    net.restart_daemon("desk").await.unwrap();
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fence_holds_a_host_to_what_its_own_cursors_report() {
    let topology = Topology::new()
        .host("desk")
        .host("laptop")
        .link("desk", "laptop")
        .agent(
            AgentDecl::new("worker", "desk")
                .steps(vec![
                    text("one"),
                    Step::TurnEnd,
                    gate("second"),
                    text("two"),
                    Step::TurnEnd,
                ])
                .prompt("go"),
        );
    let net = Net::start(topology).await.unwrap();
    let key = net.agent("worker").unwrap().key();

    // The first turn ends on the origin; fenced to the row that ended it,
    // the laptop holds everything before it.
    let first = net.turn_ended("worker", 1).await.unwrap();
    net.fence("laptop", "worker", first).await.unwrap();
    let held = {
        let runtime = net.runtime("laptop").unwrap();
        let store = runtime.store().await;
        store.cut(&key, u32::MAX).unwrap().held
    };
    assert!(held.iter().any(|item| item.text.contains("one")));
    assert_eq!(
        held.iter()
            .filter(|item| item.key.starts_with("turn:"))
            .count(),
        1
    );
    assert!(
        !held.iter().any(|item| item.text.contains("two")),
        "the second turn waits on its gate"
    );
    net.current("laptop", "worker").await.unwrap();

    // A sent input settles as the row that reflects it, and the second
    // turn's end comes after it.
    let input = b"second-go".to_vec();
    net.input(
        "worker",
        testnet::prompt(net.agent("worker").unwrap().kind, &input, "again"),
    )
    .await
    .unwrap();
    let settled = net.input_settled("worker", &input).await.unwrap();
    assert!(settled > first);
    net.open_gate("second").unwrap();
    let second = net.turn_ended("worker", 2).await.unwrap();
    assert!(second > settled);
    net.fence("laptop", "worker", second).await.unwrap();
    let held = {
        let runtime = net.runtime("laptop").unwrap();
        let store = runtime.store().await;
        store.cut(&key, u32::MAX).unwrap().held
    };
    assert!(held.iter().any(|item| item.text.contains("two")));
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_severed_link_goes_down_and_a_restored_one_comes_back() {
    let mut net = Net::start(Topology::new().host("a").host("b").host("c").link("a", "b"))
        .await
        .unwrap();
    assert!(
        net.link_up("a", "b").await,
        "declared links are up at start"
    );

    let ack = net.sever_link("b", "a").unwrap();
    assert_eq!(ack.installed, "link b - a severed");
    net.wait_link("a", "b", false).await.unwrap();
    net.restore_link("a", "b").await.unwrap();
    net.wait_link("a", "b", true).await.unwrap();

    // Restored at once, before either end has seen the cut: the new link
    // comes back and stays, rather than being refused as a second dial
    // while the severed one still looks up.
    net.sever_link("a", "b").unwrap();
    net.restore_link("a", "b").await.unwrap();
    net.wait_link("a", "b", true).await.unwrap();
    // A window: nothing the test controls gates a link that must not
    // drop again.
    holds_for(
        "the restored link to stay up",
        Duration::from_millis(500),
        || net.link_up("a", "b"),
    )
    .await
    .unwrap();

    assert!(matches!(
        net.sever_link("a", "c"),
        Err(NetError::NoLink(..))
    ));
    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_killed_daemon_leaves_its_agents_running_and_its_restart_reads_what_they_wrote() {
    let topology = Topology::new()
        .host("desk")
        .host("laptop")
        .link("desk", "laptop")
        .agent(
            AgentDecl::new("worker", "desk")
                .steps(vec![
                    text("before"),
                    gate("release"),
                    text("after"),
                    Step::TurnEnd,
                ])
                .prompt("go"),
        );
    let mut net = Net::start(topology).await.unwrap();
    let generation = net.generation("desk").unwrap();
    let mut observer = net.observe("desk", "worker", 50).await.unwrap();
    observer
        .observe_until(|events| says(events, "before"), PATIENCE)
        .await
        .unwrap();

    net.kill_daemon("desk").await.unwrap();
    assert!(!net.is_up("desk"));
    assert!(matches!(net.runtime("desk"), Err(NetError::Down(_))));
    net.wait_link("desk", "laptop", false).await.unwrap();
    // With no daemon, the agent goes on and writes its journal.
    let written = net.journal_end("worker").unwrap();
    net.open_gate("release").unwrap();
    testnet::until("the agent to write on", || {
        let end = net.journal_end("worker").unwrap();
        async move {
            (end > written)
                .then_some(())
                .ok_or_else(|| format!("journal end {end}, was {written}"))
        }
    })
    .await
    .unwrap();

    net.restart_daemon("desk").await.unwrap();
    assert_eq!(
        net.generation("desk").unwrap(),
        generation,
        "a crash under the same boot keeps the generation"
    );
    let mut observer = net.observe("desk", "worker", 50).await.unwrap();
    observer
        .observe_until(|events| says(events, "after"), PATIENCE)
        .await
        .unwrap();
    let agent = net
        .runtime("desk")
        .unwrap()
        .agent(net.agent("worker").unwrap().id)
        .await
        .unwrap();
    assert_eq!(agent.lifecycle, wire::Lifecycle::Live as i32);
    assert_eq!(agent.incarnation, 1, "the same process, never restarted");
    net.wait_link("desk", "laptop", true).await.unwrap();

    // A clean stop and start also keeps the generation.
    net.stop_daemon("desk").await.unwrap();
    net.restart_daemon("desk").await.unwrap();
    assert_eq!(net.generation("desk").unwrap(), generation);
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
        let cursor = net
            .runtime("desk")
            .unwrap()
            .store()
            .await
            .cursor(&key)
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
        let store = runtime.store().await;
        let cut = store.cut(&key, 50).unwrap();
        (cut.held, cut.snapshot.unwrap_or_default())
    };
    assert!(origin.len() >= 3, "{origin:#?}");
    let mut holed = origin.clone();
    holed.remove(1);
    {
        let runtime = net.runtime("attic").unwrap();
        let mut store = runtime.store().await;
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
        },
        Control::RelayGate {},
        Control::RelayFaults {
            delay_ms: 100,
            loss_percent: 0,
        },
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
