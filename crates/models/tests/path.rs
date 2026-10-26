//! The path machine explored over the real path core at the bounds `just
//! test` runs, and walked by seed.

use models::{Bounds, PathModel, check, walk};

/// Each CI world and how many states its exhaustive run may explore: its
/// count at these bounds on 2026-10-26 (12,929,251, 11,118,688 and
/// 12,701,530) with two percent over, since the count can move with the
/// threads. A run past it says the bounds have outgrown their place; a run
/// that hangs is the test harness's bound to catch.
fn ci_worlds() -> [(&'static str, PathModel, u64); 3] {
    [
        ("the addresses and the network", PathModel::ci(), 13_187_900),
        (
            "the peer's own packets",
            PathModel::ci_arrivals(),
            11_341_100,
        ),
        (
            "answers, refusals and forgotten addresses",
            PathModel::ci_answers(),
            12_955_600,
        ),
    ]
}

/// The worlds run one after another: each holds about 5 GB of states and
/// every core, so side by side they would crowd the runner past its
/// memory.
#[test]
fn every_path_machine_promise_holds_at_ci_bounds() {
    let threads = models::threads();
    for (world, model, budget) in ci_worlds() {
        let verdict = check(&model, Bounds::exhaustive().threads(threads));
        println!("{world}: {verdict}");
        let measured = verdict.measured;
        println!(
            "{world}: measured on {threads} threads: states={} seconds={:.1} states_per_second={:.0}",
            measured.states, measured.seconds, measured.states_per_second
        );
        verdict.assert_holds();
        assert!(
            measured.states <= budget,
            "the path model's world of {world} explored {} states, past its {budget}",
            measured.states
        );
    }
}

#[test]
fn one_seed_walks_one_path_machine_schedule() {
    // A walk is cheap, so it walks a world with everything in it.
    let model = PathModel {
        forwarding_drops: 1,
        outages: 1,
        arrivals: 1,
        second_forwarder: 1,
        framed: true,
        ..PathModel::ci_answers()
    };
    let bounds = Bounds::depth(40);
    let first = walk(&model, bounds, 7);
    let again = walk(&model, bounds, 7);
    println!("{first}");
    assert_eq!(
        first.actions, again.actions,
        "one seed walked two schedules"
    );
    assert_eq!(first.failure, again.failure);
    assert!(first.failure.is_none(), "{first}");
    let other = walk(&model, bounds, 8);
    assert_ne!(
        first.actions, other.actions,
        "two seeds walked one schedule"
    );
}

/// The nightly bounds, run by hand or by the nightly recipe.
#[test]
#[ignore = "nightly bounds: minutes on every core"]
fn every_path_machine_promise_holds_at_nightly_bounds() {
    for model in [
        PathModel::nightly(),
        PathModel::nightly_arrivals(),
        PathModel::nightly_answers(),
    ] {
        let verdict = check(&model, Bounds::exhaustive().threads(models::threads()));
        println!("{verdict}");
        verdict.assert_holds();
    }
}
