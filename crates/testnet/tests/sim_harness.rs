//! The harness's stories in the simulated net, under a handful of seeds:
//! the same verbs, fences and observations the real net's conformance
//! holds, over every core in one process and a schedule each seed picks.
//! Beside them, what only a simulated net promises: one seed is one
//! schedule, time moves only when the story moves it, a lost message
//! breaks its call and the replica still catches up, and a broken
//! invariant fails the net with its seed.

mod stories;

use std::time::Duration;

use provider_fakes::script::Step;
use testnet::sim::Faults;
use testnet::{AgentDecl, NetError, SimNet, SimOptions, Stuck, Topology, World as _};
use wire::{ExitCause, Lifecycle};

/// Seeds every story runs under: enough to see several schedules, few
/// enough to stay a fast test.
const SEEDS: std::ops::Range<u64> = 0..6;

fn under_each_seed(topology: fn() -> Topology) -> impl Iterator<Item = (u64, SimNet)> {
    SEEDS.map(move |seed| {
        let net = SimNet::start(topology(), seed)
            .unwrap_or_else(|error| panic!("seed {seed}: the net starts: {error}"));
        (seed, net)
    })
}

#[tokio::test]
async fn a_stuck_observation_fails_at_its_deadline_and_a_closed_one_at_once() {
    for (seed, net) in under_each_seed(stories::stuck_topology) {
        println!("seed {seed}");
        stories::a_stuck_observation_fails_at_its_deadline_and_a_closed_one_at_once(net).await;
    }
}

#[tokio::test]
async fn a_fence_holds_a_host_to_what_its_own_cursors_report() {
    for (seed, net) in under_each_seed(stories::fence_topology) {
        println!("seed {seed}");
        stories::a_fence_holds_a_host_to_what_its_own_cursors_report(net).await;
    }
}

#[tokio::test]
async fn a_severed_link_goes_down_and_a_restored_one_comes_back() {
    for (seed, net) in under_each_seed(stories::severed_topology) {
        println!("seed {seed}");
        stories::a_severed_link_goes_down_and_a_restored_one_comes_back(net).await;
    }
}

#[tokio::test]
async fn a_killed_daemon_leaves_its_agents_running_and_its_restart_reads_what_they_wrote() {
    for (seed, net) in under_each_seed(stories::killed_topology) {
        println!("seed {seed}");
        stories::a_killed_daemon_leaves_its_agents_running_and_its_restart_reads_what_they_wrote(
            net,
        )
        .await;
    }
}

/// The schedule a story's first turn and its replication take under
/// `seed`.
async fn schedule(seed: u64) -> Vec<String> {
    let net = SimNet::start(stories::fence_topology(), seed).unwrap();
    let first = net.turn_ended("worker", 1).await.unwrap();
    net.fence("laptop", "worker", first).await.unwrap();
    net.trace()
}

#[tokio::test]
async fn one_seed_is_one_schedule_and_another_seed_another() {
    let once = schedule(7).await;
    assert!(
        once.len() > 20,
        "a story's schedule is many deliveries: {once:?}"
    );
    assert_eq!(once, schedule(7).await, "the same seed, the same schedule");
    assert_ne!(once, schedule(8).await, "another seed, another schedule");
}

fn one_turn() -> Topology {
    Topology::new().host("desk").agent(
        AgentDecl::new("worker", "desk")
            .steps(vec![stories::text("hi"), Step::TurnEnd])
            .prompt("go"),
    )
}

#[tokio::test]
async fn an_agent_outlives_its_daemon_for_its_grace_on_the_virtual_clock_and_no_longer() {
    for (seed, mut net) in SEEDS.map(|seed| (seed, SimNet::start(one_turn(), seed).unwrap())) {
        println!("seed {seed}");
        net.turn_ended("worker", 1).await.unwrap();
        net.kill_daemon("desk").await.unwrap();
        let written = net.journal_end("worker").unwrap();
        let wrote_on = || {
            let end = net.journal_end("worker").unwrap();
            async move {
                (end > written)
                    .then_some(())
                    .ok_or_else(|| format!("journal end {end}, was {written}"))
            }
        };
        // Nothing moves the clock but the story: with no daemon, the agent
        // waits out its grace, which never passes on its own.
        let waited = net
            .until("the agent to give up on its daemon", wrote_on)
            .await;
        assert!(
            matches!(waited, Err(NetError::Stuck(Stuck::Deadline { .. }))),
            "{waited:?}"
        );
        net.advance(Duration::from_secs(19)).unwrap();
        assert!(net.until("still within its grace", wrote_on).await.is_err());
        net.advance(Duration::from_secs(1)).unwrap();
        net.until("the agent to give up on its daemon", wrote_on)
            .await
            .unwrap();

        net.restart_daemon("desk").await.unwrap();
        let key = net.agent("worker").unwrap().key();
        let row = net
            .until("its exit to reach its row", || {
                let key = key.clone();
                let row = net.read_store("desk", move |store| store.agent(&key).unwrap());
                async move {
                    match row.await.unwrap() {
                        Some(row) if row.lifecycle == Lifecycle::Exited as i32 => Ok(row),
                        other => Err(format!("{other:?}")),
                    }
                }
            })
            .await
            .unwrap();
        assert_eq!(row.exit_cause, ExitCause::DaemonLost as i32, "{row:?}");
        net.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn a_lost_message_breaks_its_call_and_the_replica_still_catches_up() {
    let mut lost = 0;
    for seed in SEEDS {
        let options = SimOptions {
            seed,
            faults: Faults {
                lose_per_mille: 300,
            },
        };
        let net = SimNet::start_with(stories::fence_topology(), options).unwrap();
        let first = net.turn_ended("worker", 1).await.unwrap();
        // A broken stream is opened again after its backoff: the story
        // waits each one out until the replica holds the turn.
        let mut backoffs = 0;
        loop {
            match net.current("laptop", "worker").await {
                Ok(()) => break,
                Err(NetError::Stuck(Stuck::Deadline { .. })) if backoffs < 50 => {
                    backoffs += 1;
                    net.advance(Duration::from_secs(30)).unwrap();
                }
                Err(error) => panic!("seed {seed}: {error}"),
            }
        }
        net.fence("laptop", "worker", first).await.unwrap();
        let losses = net
            .trace()
            .iter()
            .filter(|line| line.contains("lost a message"))
            .count();
        println!("seed {seed}: {losses} messages lost, {backoffs} backoffs waited out");
        lost += losses;
        net.shutdown().await.unwrap();
    }
    assert!(lost > 0, "the faults lost something under some seed");
}
