//! The link socket explored over the real socket core at the bounds `just
//! test` runs.

use models::{Bounds, SocketModel, check};

/// How many states each exhaustive run may explore: its count at these
/// bounds on 2026-10-29 with two percent over, as the other models allow
/// for counts that move with the threads (this one's did not, on 4 and 12).
/// A run past it says the bounds have outgrown their place; a run that
/// hangs is the test harness's bound to catch.
const RELAUNCH_STATES: u64 = 3_602_400;
const LAN_DOWN_STATES: u64 = 52;
const LOSS_STATES: u64 = 253_300;
const DUPLICATE_STATES: u64 = 106_900;
const RELAY_DOWN_STATES: u64 = 39_200;
const TIME_STATES: u64 = 89_400;

fn holds_within_budget(model: &SocketModel, budget: u64) {
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
        "the link socket model explored {} states, past its {budget}",
        measured.states
    );
}

#[test]
fn every_link_socket_promise_holds_across_a_relaunch_at_ci_bounds() {
    holds_within_budget(&SocketModel::ci(), RELAUNCH_STATES);
}

#[test]
fn every_link_socket_promise_holds_when_the_local_network_goes_at_ci_bounds() {
    holds_within_budget(&SocketModel::lan_down(), LAN_DOWN_STATES);
}

#[test]
fn every_link_socket_promise_holds_when_a_datagram_is_lost_at_ci_bounds() {
    holds_within_budget(&SocketModel::loss(), LOSS_STATES);
}

#[test]
fn every_link_socket_promise_holds_when_a_datagram_comes_twice_at_ci_bounds() {
    holds_within_budget(&SocketModel::duplicate(), DUPLICATE_STATES);
}

#[test]
fn every_link_socket_promise_holds_as_the_relay_link_goes_at_ci_bounds() {
    holds_within_budget(&SocketModel::relay_down(), RELAY_DOWN_STATES);
}

#[test]
fn every_link_socket_promise_holds_as_time_passes_at_ci_bounds() {
    holds_within_budget(&SocketModel::time(), TIME_STATES);
}

/// The nightly bounds, run by hand or by the nightly recipe.
#[test]
#[ignore = "nightly bounds: minutes on every core"]
fn every_link_socket_promise_holds_at_nightly_bounds() {
    let verdict = check(
        &SocketModel::nightly(),
        Bounds::exhaustive().threads(models::threads()),
    );
    println!("{verdict}");
    verdict.assert_holds();
}
