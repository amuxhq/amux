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

/// A walk is cheap, so one walks a world with everything in it, past what
/// the exhaustive worlds can hold.
#[test]
fn a_walk_through_a_world_with_everything_breaks_no_path_promise() {
    let model = PathModel {
        forwarding_drops: 1,
        outages: 1,
        arrivals: 1,
        second_forwarder: 1,
        framed: true,
        ..PathModel::ci_answers()
    };
    let walked = walk(&model, Bounds::depth(40), 7);
    println!("{walked}");
    assert!(walked.failure.is_none(), "{walked}");
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
