//! A seed is a run: the same seed makes the same schedule, another seed
//! another, a failure is reported by the seed that reproduces it on demand,
//! and a failing run shrinks to a shorter one that fails the same way.

mod stories;

use testnet::modes::{attempt, walk};
use testnet::sim::{Faults, SimOptions, same_failure, shrink};
use testnet::{SimNet, Topology, World as _};

/// The story every run here tells: the agent's first turn ends and its
/// replica on the laptop holds it.
async fn replicate_a_turn(net: SimNet) {
    let first = net.turn_ended("worker", 1).await.unwrap();
    net.fence("laptop", "worker", first).await.unwrap();
    net.shutdown().await.unwrap();
}

fn topology() -> Topology {
    stories::fence_topology()
}

/// Calls lose three messages in ten and the story never waits out a
/// backoff, so a seed that loses one of replication's messages leaves the
/// replica behind with nothing left to deliver.
const LOSSY: Faults = Faults {
    lose_per_mille: 300,
};

/// One run under `seed`: its trace, printed under `title`.
async fn traced(title: &str, seed: u64) -> Vec<String> {
    let net = SimNet::start(topology(), seed).unwrap();
    let first = net.turn_ended("worker", 1).await.unwrap();
    net.fence("laptop", "worker", first).await.unwrap();
    let trace = net.trace();
    println!("== {title}: seed {seed}, {} deliveries", trace.len());
    for line in &trace {
        println!("  {line}");
    }
    net.shutdown().await.unwrap();
    trace
}

#[tokio::test]
async fn one_seed_replays_its_schedule_and_another_seed_makes_another() {
    let first = traced("run 1", 7).await;
    let again = traced("run 2, the same seed", 7).await;
    let other = traced("run 3, another seed", 8).await;
    assert_eq!(first, again, "one seed made two schedules");
    assert_ne!(first, other, "two seeds made one schedule");
    println!("runs 1 and 2 match line for line; run 3 differs");
}

#[tokio::test]
async fn a_failing_seed_reproduces_its_failure_on_demand_and_shrinks() {
    let lossy = |seed| async move {
        let net = SimNet::start_with(
            topology(),
            SimOptions {
                seed,
                faults: LOSSY,
            },
        )
        .unwrap();
        replicate_a_turn(net).await;
    };
    let failing = walk(0..32, lossy).await;
    let Some((reported, failure)) = failing.first().cloned() else {
        panic!("no seed of 32 lost one of replication's messages");
    };
    println!(
        "== injected fault: {} in a thousand messages lost; seeds failing of 32: {:?}",
        LOSSY.lose_per_mille,
        failing.iter().map(|(seed, _)| *seed).collect::<Vec<_>>()
    );
    println!("reported: seed {reported} failed: {failure}");

    // On demand: the reported seed alone fails again, the same way, after
    // the same schedule.
    let ring = |seed| async move {
        let net = SimNet::start_with(
            topology(),
            SimOptions {
                seed,
                faults: LOSSY,
            },
        )
        .unwrap();
        let reader = net.ring_reader();
        let failure = attempt(replicate_a_turn(net)).await.err();
        (failure, reader.ring())
    };
    let (again, first_ring) = ring(reported).await;
    let (third, second_ring) = ring(reported).await;
    assert_eq!(
        again.as_deref(),
        Some(failure.as_str()),
        "seed {reported} did not fail again"
    );
    assert_eq!(third, again, "seed {reported} failed two ways");
    assert_eq!(
        first_ring, second_ring,
        "seed {reported} made two schedules"
    );
    println!(
        "reproduced: seed {reported} fails again after the same {} deliveries",
        first_ring.len()
    );

    let shrunk = shrink(topology, LOSSY, reported, 0..32, replicate_a_turn)
        .await
        .expect("the reported seed fails");
    println!(
        "shrunk: seed {} and its first {} choices reproduce it, where the reported run made {}; \
         past them the net delivers in its own order and fails after {} deliveries: {}",
        shrunk.seed,
        shrunk.ring.len(),
        first_ring.len(),
        shrunk.deliveries,
        shrunk.failure
    );
    assert!(same_failure(&shrunk.failure, &failure), "{shrunk:?}");
    assert!(
        shrunk.ring.len() < first_ring.len(),
        "shrinking kept all {} choices",
        first_ring.len()
    );
    // The reproduction replays on demand.
    let net = SimNet::replay(topology(), shrunk.options(), shrunk.ring.clone()).unwrap();
    let replayed = attempt(replicate_a_turn(net)).await.err();
    assert!(
        replayed
            .as_deref()
            .is_some_and(|replayed| same_failure(replayed, &failure)),
        "the shrunk ring did not fail again: {replayed:?}"
    );
    println!("the shrunk ring fails again on replay");
}
