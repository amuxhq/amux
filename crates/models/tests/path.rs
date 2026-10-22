//! The path machine explored over the real path core at the bounds `just
//! test` runs, and walked by seed.

use models::{Bounds, PathModel, check, walk};

/// How long the exhaustive run may take under `just test`. No CI runner
/// figure yet, so it is sized by the README's rule from the four-thread Mac
/// time: 21.2 s, times 4.7, doubled and rounded up to a whole minute. A run
/// past it says the bounds have outgrown their place.
const BUDGET_SECONDS: f64 = 240.0;

#[test]
fn every_path_machine_promise_holds_at_ci_bounds() {
    let threads = models::threads();
    let verdict = check(&PathModel::ci(), Bounds::exhaustive().threads(threads));
    println!("{verdict}");
    let measured = verdict.measured;
    println!(
        "measured on {threads} threads: states={} seconds={:.1} states_per_second={:.0}",
        measured.states, measured.seconds, measured.states_per_second
    );
    verdict.assert_holds();
    assert!(
        measured.seconds <= BUDGET_SECONDS,
        "the path model took {:.1}s, past its {BUDGET_SECONDS}s budget",
        measured.seconds
    );
}

#[test]
fn one_seed_walks_one_path_machine_schedule() {
    let model = PathModel::ci();
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
    let verdict = check(
        &PathModel::nightly(),
        Bounds::exhaustive().threads(models::threads()),
    );
    println!("{verdict}");
    verdict.assert_holds();
}
