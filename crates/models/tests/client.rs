//! One open chat's session state, explored over the real `ui_state` at the
//! bounds `just test` runs, and walked by seed.

use models::{Bounds, ClientModel, check, walk};

/// How long the exhaustive run may take under `just test`, sized to CI's
/// four-core runner: not measured there yet, so twice the enrolled Mac's
/// four-thread time times 4.2, rounded up to a whole minute. A run past it
/// says the bounds have outgrown their place.
const BUDGET_SECONDS: f64 = 600.0;

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
        measured.seconds <= BUDGET_SECONDS,
        "the client model took {:.1}s, past its {BUDGET_SECONDS}s budget",
        measured.seconds
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
