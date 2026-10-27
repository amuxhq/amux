//! One daemon under crash, explored over the real core and store at the
//! bounds `just test` runs, and walked by seed.

use models::{Bounds, SingleDaemon, check, walk};

/// How many states the two exhaustive runs together may explore: their
/// count at these bounds (106,592 and 28,248 on 2026-10-26) with two
/// percent over, since the count can move with the threads. A run past it
/// says the bounds have outgrown their place; a run that hangs is the test
/// harness's bound to catch.
const BUDGET_STATES: u64 = 137_600;

/// The journal's bounds and the peer's, each explored in full; the budget
/// is for both.
#[test]
fn every_single_daemon_promise_holds_at_ci_bounds() {
    let threads = models::threads();
    let mut states = 0;
    for model in [SingleDaemon::ci(), SingleDaemon::peer()] {
        let verdict = check(&model, Bounds::exhaustive().threads(threads));
        println!("{verdict}");
        let measured = verdict.measured;
        println!(
            "measured on {threads} threads: states={} seconds={:.1} states_per_second={:.0}",
            measured.states, measured.seconds, measured.states_per_second
        );
        verdict.assert_holds();
        states += measured.states;
    }
    assert!(
        states <= BUDGET_STATES,
        "the single-daemon model explored {states} states, past its {BUDGET_STATES}"
    );
}

#[test]
fn one_seed_walks_one_single_daemon_schedule() {
    let model = SingleDaemon::ci();
    let bounds = Bounds::depth(60);
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

/// The nightly bounds, and the peer around the CI bounds' journal and
/// subscriber, run by hand or by the nightly recipe.
#[test]
#[ignore = "nightly bounds: minutes on every core"]
fn every_single_daemon_promise_holds_at_nightly_bounds() {
    for model in [SingleDaemon::nightly(), SingleDaemon::peer_nightly()] {
        let verdict = check(&model, Bounds::exhaustive().threads(models::threads()));
        println!("{verdict}");
        verdict.assert_holds();
    }
}
