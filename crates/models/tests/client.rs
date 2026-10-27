//! One open chat's session state, explored over the real `ui_state` at the
//! bounds `just test` runs, and walked by seed.

use models::{Bounds, ClientModel, check, walk};

/// How many states the exhaustive run may explore: its count at these
/// bounds (1,217,819 on 2026-10-23) with two percent over, since the count
/// moves by a few dozen states with the threads. A run past it says the
/// bounds have outgrown their place; a run that hangs is the test
/// harness's bound to catch.
const BUDGET_STATES: u64 = 1_242_200;

#[test]
fn every_client_promise_holds_at_ci_bounds() {
    let threads = models::threads();
    let verdict = check(&ClientModel::ci(), Bounds::exhaustive().threads(threads));
    println!("{verdict}");
    let measured = verdict.measured;
    println!(
        "measured on {threads} threads: states={} seconds={:.1} states_per_second={:.0}",
        measured.states, measured.seconds, measured.states_per_second
    );
    verdict.assert_holds();
    assert!(
        measured.states <= BUDGET_STATES,
        "the client model explored {} states, past its {BUDGET_STATES}",
        measured.states
    );
}

#[test]
fn one_seed_walks_one_client_schedule() {
    let model = ClientModel::ci();
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
fn every_client_promise_holds_at_nightly_bounds() {
    let verdict = check(
        &ClientModel::nightly(),
        Bounds::exhaustive().threads(models::threads()),
    );
    println!("{verdict}");
    verdict.assert_holds();
}
