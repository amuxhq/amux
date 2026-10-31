//! The link socket explored over the real socket core at the bounds `just
//! test` runs.

use models::{Bounds, SocketModel, check};

fn holds(model: &SocketModel) {
    let threads = models::threads();
    let verdict = check(model, Bounds::exhaustive().threads(threads));
    println!("{verdict}");
    verdict.assert_holds();
}

#[test]
fn every_link_socket_promise_holds_across_a_relaunch_at_ci_bounds() {
    holds(&SocketModel::ci());
}

#[test]
fn every_link_socket_promise_holds_when_the_local_network_goes_at_ci_bounds() {
    holds(&SocketModel::lan_down());
}

#[test]
fn every_link_socket_promise_holds_when_a_datagram_is_lost_at_ci_bounds() {
    holds(&SocketModel::loss());
}

#[test]
fn every_link_socket_promise_holds_when_a_datagram_comes_twice_at_ci_bounds() {
    holds(&SocketModel::duplicate());
}

#[test]
fn every_link_socket_promise_holds_as_the_relay_link_goes_at_ci_bounds() {
    holds(&SocketModel::relay_down());
}

#[test]
fn every_link_socket_promise_holds_as_time_passes_at_ci_bounds() {
    holds(&SocketModel::time());
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
