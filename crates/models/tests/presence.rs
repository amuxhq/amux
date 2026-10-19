//! Presence and routing explored over the real daemon core at the bounds
//! `just test` runs, and walked by seed.

use models::{Bounds, PresenceRouting, check, walk};

/// How long the exhaustive run may take under `just test`, sized to CI's
/// four-core runner: twice the run's measured time there (134 s), rounded up to
/// a whole minute. A run past it says the bounds have outgrown their place.
const BUDGET_SECONDS: f64 = 300.0;

#[test]
fn every_presence_promise_holds_at_ci_bounds() {
    let threads = models::threads();
    let verdict = check(
        &PresenceRouting::ci(),
        Bounds::exhaustive().threads(threads),
    );
    println!("{verdict}");
    let measured = verdict.measured;
    println!(
        "measured on {threads} threads: states={} seconds={:.1} states_per_second={:.0}",
        measured.states, measured.seconds, measured.states_per_second
    );
    verdict.assert_holds();
    assert!(
        measured.seconds <= BUDGET_SECONDS,
        "the presence model took {:.1}s, past its {BUDGET_SECONDS}s budget",
        measured.seconds
    );
}

#[test]
fn one_seed_walks_one_presence_schedule() {
    let model = PresenceRouting::ci();
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

/// The nightly bounds, run by hand or by the nightly recipe: about nine
/// million states, five minutes on the enrolled Mac's cores.
#[test]
#[ignore = "nightly bounds: minutes on every core"]
fn every_presence_promise_holds_at_nightly_bounds() {
    let verdict = check(
        &PresenceRouting::nightly(),
        Bounds::exhaustive().threads(models::threads()),
    );
    println!("{verdict}");
    verdict.assert_holds();
}
