//! The family rule and the deliveries outbox explored over two hosts' real
//! daemon cores at the bounds `just test` runs, and walked by seed.

use models::{Bounds, FamilyDeliveries, check, walk};

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

#[test]
fn one_seed_walks_one_family_schedule() {
    let model = FamilyDeliveries::ci();
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
fn every_family_promise_holds_at_nightly_bounds() {
    let verdict = check(
        &FamilyDeliveries::nightly(),
        Bounds::exhaustive().threads(models::threads()),
    );
    println!("{verdict}");
    verdict.assert_holds();
}
