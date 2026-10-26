//! The relay's forwarding explored over the real relay core at the bounds
//! `just test` runs, as the cloud relay and as a daemon forwarding between
//! its paired hosts, and walked by seed.

use models::{Bounds, RelayModel, check, walk};

/// How many states each exhaustive run may explore: its count at these
/// bounds on 2026-10-26 with two percent over, since the count can move
/// with the threads. A run past it says the bounds have outgrown their
/// place; a run that hangs is the test harness's bound to catch.
const CLOUD_RELAY_STATES: u64 = 708_400;
const DAEMON_FORWARDING_STATES: u64 = 733_500;

fn holds_within_budget(model: &RelayModel, budget: u64) {
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
        measured.states <= budget,
        "the relay model explored {} states, past its {budget}",
        measured.states
    );
}

#[test]
fn every_relay_forwarding_promise_holds_at_ci_bounds() {
    holds_within_budget(&RelayModel::ci(), CLOUD_RELAY_STATES);
}

#[test]
fn every_relay_forwarding_promise_holds_for_a_daemon_forwarding_at_ci_bounds() {
    holds_within_budget(&RelayModel::daemon(), DAEMON_FORWARDING_STATES);
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
    for model in [RelayModel::nightly(), RelayModel::daemon_nightly()] {
        let verdict = check(&model, Bounds::exhaustive().threads(models::threads()));
        println!("{verdict}");
        verdict.assert_holds();
    }
}
