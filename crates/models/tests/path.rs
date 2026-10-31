//! The path machine explored over the real path core at the bounds `just
//! test` runs, and walked by seed.

use models::{Bounds, PathModel, check, walk};

/// Each CI world, by name.
fn ci_worlds() -> [(&'static str, PathModel); 3] {
    [
        ("the addresses and the network", PathModel::ci()),
        ("the peer's own packets", PathModel::ci_arrivals()),
        (
            "answers, refusals and forgotten addresses",
            PathModel::ci_answers(),
        ),
    ]
}

/// The worlds run one after another: each holds about 5 GB of states and
/// every core, so side by side they would crowd the runner past its
/// memory.
#[test]
fn every_path_machine_promise_holds_at_ci_bounds() {
    let threads = models::threads();
    for (world, model) in ci_worlds() {
        let verdict = check(&model, Bounds::exhaustive().threads(threads));
        println!("{world}: {verdict}");
        verdict.assert_holds();
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
