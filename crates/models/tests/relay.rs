//! The relay's forwarding explored over the real relay core at the bounds
//! `just test` runs, as the cloud relay and as a daemon forwarding between
//! its paired hosts, and walked by seed.

use models::{Bounds, RelayModel, check, walk};

/// How long each exhaustive run may take under `just test`. No CI runner
/// figure yet, so it is sized by the README's rule from the slower of the
/// two four-thread Mac times: 4.0 s, times 4.7, doubled and rounded up to a
/// whole minute. A run past it says the bounds have outgrown their place.
const BUDGET_SECONDS: f64 = 60.0;

fn holds_within_budget(model: &RelayModel) {
    let threads = models::threads();
    let verdict = check(model, Bounds::exhaustive().threads(threads));
    println!("{verdict}");
    let measured = verdict.measured;
    println!(
        "measured on {threads} threads: states={} seconds={:.1} states_per_second={:.0}",
        measured.states, measured.seconds, measured.states_per_second
    );
    verdict.assert_holds();
    assert!(
        measured.seconds <= BUDGET_SECONDS,
        "the relay model took {:.1}s, past its {BUDGET_SECONDS}s budget",
        measured.seconds
    );
}

#[test]
fn every_relay_forwarding_promise_holds_at_ci_bounds() {
    holds_within_budget(&RelayModel::ci());
}

#[test]
fn every_relay_forwarding_promise_holds_for_a_daemon_forwarding_at_ci_bounds() {
    holds_within_budget(&RelayModel::daemon());
}

#[test]
fn one_seed_walks_one_relay_schedule() {
    let model = RelayModel::ci();
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
fn every_relay_forwarding_promise_holds_at_nightly_bounds() {
    let verdict = check(
        &RelayModel::nightly(),
        Bounds::exhaustive().threads(models::threads()),
    );
    println!("{verdict}");
    verdict.assert_holds();
}
