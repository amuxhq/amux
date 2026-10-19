//! What only a simulated net promises: one seed is one schedule, time
//! moves only when the story moves it, and a lost message breaks its call
//! while the replica still catches up. The stories walk the seeds
//! `testnet::modes::seeds` names, a few by default and thousands at night;
//! the harness's stories that hold on either net are in `harness_stories`.

mod stories;

use std::cell::Cell;
use std::time::Duration;

use provider_fakes::script::Step;
use testnet::modes::{seeds, walk_seeds};
use testnet::sim::Faults;
use testnet::{AgentDecl, NetError, SimNet, SimOptions, Stuck, Topology, World as _};
use wire::{ExitCause, Lifecycle};

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
    walk_seeds(|seed| async move {
        let mut net = SimNet::start(one_turn(), seed).unwrap();
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
    })
    .await;
}

#[tokio::test]
async fn a_lost_message_breaks_its_call_and_the_replica_still_catches_up() {
    let lost = &Cell::new(0);
    walk_seeds(|seed| async move {
        // Losing three messages in twenty breaks most streams at least once
        // and still lets one through within the backoffs waited out.
        let options = SimOptions {
            seed,
            faults: Faults {
                lose_per_mille: 150,
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
        lost.set(lost.get() + losses);
        net.shutdown().await.unwrap();
    })
    .await;
    println!("{} messages lost across the seeds", lost.get());
    // One seed may lose nothing; several at three in twenty per message
    // do. At three in ten a whole catch-up so rarely got through that some
    // seeds used up the fifty backoffs the story waits out.
    if seeds().count() > 1 {
        assert!(lost.get() > 0, "the faults lost something under some seed");
    }
}

/// The steps of a journal fixture, as its file holds them.
fn fixture(name: &str) -> Vec<wire::Step> {
    let path = format!(
        "{}/fixtures/journals/{name}.frames",
        env!("CARGO_MANIFEST_DIR")
    );
    let bytes = std::fs::read(&path).unwrap();
    let mut steps = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let journal::Frame::Whole(step, length) = journal::decode_frame(&bytes[at..]) else {
            panic!("{path}: byte {at} is not a whole frame");
        };
        steps.push(*step);
        at += length;
    }
    steps
}

#[tokio::test]
async fn an_agent_playing_a_recorded_claude_journal_reaches_the_far_host_turn_by_turn() {
    const JOURNAL: &str = "claude_sdk-multi_turn";
    let steps = fixture(JOURNAL);
    let turns = steps.iter().filter(|step| step.turn_end.is_some()).count();
    let mut keys: Vec<String> = steps
        .iter()
        .flat_map(|step| step.items.iter().map(|item| item.key.clone()))
        .collect();
    keys.sort();
    keys.dedup();
    assert!(turns > 0 && !keys.is_empty(), "{JOURNAL} ends a turn");
    println!(
        "{JOURNAL}: {} steps, {turns} turns, items {keys:?}",
        steps.len()
    );
    walk_seeds(|seed| {
        let keys = keys.clone();
        async move {
            let topology = Topology::new()
                .host("desk")
                .host("laptop")
                .link("desk", "laptop")
                .agent(AgentDecl::new("player", "desk").journal(JOURNAL));
            let net = SimNet::start(topology, seed).unwrap();
            let last = net.turn_ended("player", turns).await.unwrap();
            net.fence("laptop", "player", last).await.unwrap();
            let key = net.agent("player").unwrap().key();
            let mut held: Vec<String> = net
                .read_store("laptop", move |store| store.rows(&key).unwrap())
                .await
                .unwrap()
                .into_iter()
                .map(|item| item.key)
                .collect();
            held.sort();
            assert_eq!(
                held, keys,
                "seed {seed}: the far host holds every item the journal wrote"
            );
            net.shutdown().await.unwrap();
        }
    })
    .await;
}
