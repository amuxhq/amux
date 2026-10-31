//! The harness's own stories that hold in every world: each declares its
//! topology and runs against [`World`], so the real net and the simulated
//! one are held to the same verbs, fences and observations.

#![allow(dead_code)]

use std::time::{Duration, Instant};

use provider_fakes::script::Step;
use testnet::{AgentDecl, NetError, PATIENCE, Stuck, Topology, World};
use wire::{SessionEvent, session_event};

pub fn text(text: &str) -> Step {
    Step::Text {
        chunks: vec![text.to_owned()],
    }
}

pub fn gate(name: &str) -> Step {
    Step::WaitFor { path: name.into() }
}

/// The texts of every item seen, in order.
pub fn texts(events: &[SessionEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.of {
            Some(session_event::Of::Item(item)) => Some(item.text.clone()),
            _ => None,
        })
        .collect()
}

pub fn says(events: &[SessionEvent], wanted: &str) -> bool {
    texts(events).iter().any(|text| text.contains(wanted))
}

pub fn stuck_topology() -> Topology {
    Topology::new().host("desk").agent(
        AgentDecl::new("worker", "desk")
            .steps(vec![text("started"), gate("never"), Step::TurnEnd])
            .prompt("go"),
    )
}

pub async fn a_stuck_observation_fails_at_its_deadline_and_a_closed_one_at_once(
    mut net: impl World,
) {
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

pub fn fence_topology() -> Topology {
    Topology::new()
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
        )
}

/// Everything `host` holds of the agent, in order.
async fn held(net: &impl World, host: &str, agent: &str) -> Vec<wire::Item> {
    let key = net.agent(agent).unwrap().key();
    net.read_store(host, move |store| store.cut(&key, u32::MAX).unwrap().held)
        .await
        .unwrap()
}

pub async fn a_fence_holds_a_host_to_what_its_own_cursors_report(net: impl World) {
    // The first turn ends on the origin; fenced to the row that ended it,
    // the laptop holds everything before it.
    let first = net.turn_ended("worker", 1).await.unwrap();
    net.fence("laptop", "worker", first).await.unwrap();
    let held_first = held(&net, "laptop", "worker").await;
    assert!(held_first.iter().any(|item| item.text.contains("one")));
    assert_eq!(
        held_first
            .iter()
            .filter(|item| item.key.starts_with("turn:"))
            .count(),
        1
    );
    assert!(
        !held_first.iter().any(|item| item.text.contains("two")),
        "the second turn waits on its gate"
    );
    net.current("laptop", "worker").await.unwrap();

    // A sent input settles as the row that reflects it, and the second
    // turn's end comes after it.
    let input = b"second-go".to_vec();
    net.input("worker", testnet::prompt(&input, "again"))
        .await
        .unwrap();
    let settled = net.input_settled("worker", &input).await.unwrap();
    assert!(settled > first);
    net.open_gate("second").unwrap();
    let second = net.turn_ended("worker", 2).await.unwrap();
    assert!(second > settled);
    net.fence("laptop", "worker", second).await.unwrap();
    let held_second = held(&net, "laptop", "worker").await;
    assert!(held_second.iter().any(|item| item.text.contains("two")));
    net.shutdown().await.unwrap();
}

pub fn severed_topology() -> Topology {
    Topology::new().host("a").host("b").host("c").link("a", "b")
}

pub async fn a_severed_link_goes_down_and_a_restored_one_comes_back(mut net: impl World) {
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
    net.holds_for(
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

pub fn killed_topology() -> Topology {
    Topology::new()
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
        )
}

pub async fn a_killed_daemon_leaves_its_agents_running_and_its_restart_reads_what_they_wrote(
    mut net: impl World,
) {
    let generation = net.generation("desk").unwrap();
    let mut observer = net.observe("desk", "worker", 50).await.unwrap();
    observer
        .observe_until(|events| says(events, "before"), PATIENCE)
        .await
        .unwrap();

    net.kill_daemon("desk").await.unwrap();
    assert!(!net.is_up("desk"));
    assert!(matches!(
        net.read_store("desk", |_| ()).await,
        Err(NetError::Down(_))
    ));
    net.wait_link("desk", "laptop", false).await.unwrap();
    // With no daemon, the agent goes on and writes its journal.
    let written = net.journal_end("worker").unwrap();
    net.open_gate("release").unwrap();
    net.until("the agent to write on", || {
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
    let key = net.agent("worker").unwrap().key();
    let row = net
        .read_store("desk", move |store| store.agent(&key).unwrap())
        .await
        .unwrap()
        .expect("the agent's row");
    assert_eq!(row.lifecycle, wire::Lifecycle::Live as i32);
    assert_eq!(row.incarnation, 1, "the same process, never restarted");
    net.wait_link("desk", "laptop", true).await.unwrap();

    // A clean stop and start also keeps the generation.
    net.stop_daemon("desk").await.unwrap();
    net.restart_daemon("desk").await.unwrap();
    assert_eq!(net.generation("desk").unwrap(), generation);
    net.shutdown().await.unwrap();
}
