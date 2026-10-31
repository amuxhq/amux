//! One open chat's session state, explored over the real `ui_state` at the
//! bounds `just test` runs, and walked by seed.

use models::{Bounds, ClientModel, check, walk};

#[test]
fn every_client_promise_holds_at_ci_bounds() {
    let threads = models::threads();
    let verdict = check(&ClientModel::ci(), Bounds::exhaustive().threads(threads));
    println!("{verdict}");
    verdict.assert_holds();
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
