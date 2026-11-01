//! The relay's forwarding explored over the real relay core at the bounds
//! `just test` runs, as the cloud relay and as a daemon forwarding between
//! its paired hosts.

use models::{Bounds, RelayModel, check};

fn holds(model: &RelayModel) {
    let threads = models::threads();
    let verdict = check(model, Bounds::exhaustive().threads(threads));
    println!("{verdict}");
    verdict.assert_holds();
}

#[test]
fn every_relay_forwarding_promise_holds_at_ci_bounds() {
    holds(&RelayModel::ci());
}

#[test]
fn every_relay_forwarding_promise_holds_across_a_purchase_at_ci_bounds() {
    holds(&RelayModel::purchase());
}

#[test]
fn every_relay_forwarding_promise_holds_for_a_daemon_forwarding_at_ci_bounds() {
    holds(&RelayModel::daemon());
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
