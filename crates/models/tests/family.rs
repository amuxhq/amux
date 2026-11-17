//! The family rule and the deliveries outbox explored over two hosts' real
//! daemon cores at the bounds `just test` runs.

use models::{Bounds, FamilyDeliveries, check};

#[test]
fn every_family_promise_holds_at_ci_bounds() {
    let threads = models::threads();
    let verdict = check(
        &FamilyDeliveries::ci(),
        Bounds::exhaustive().threads(threads),
    );
    println!("{verdict}");
    verdict.assert_holds();
}

/// With the suspension experiment on: the parent suspends, and its machine
/// restarts with it idle or in its turn.
#[test]
fn every_family_promise_holds_over_suspensions_and_restarts_at_ci_bounds() {
    let verdict = check(
        &FamilyDeliveries::suspending(),
        Bounds::exhaustive().threads(models::threads()),
    );
    println!("{verdict}");
    verdict.assert_holds();
}

/// The nightly bounds, run by hand or by the nightly recipe.
#[test]
#[ignore = "nightly bounds: minutes on every core"]
fn every_family_promise_holds_at_nightly_bounds() {
    let verdict = check(
        &FamilyDeliveries::nightly(),
        Bounds::exhaustive().threads(models::threads()),
    );
    println!("{verdict}");
    verdict.assert_holds();
}
