//! Presence and routing explored over the real daemon core at the bounds
//! `just test` runs.

use models::{Bounds, PresenceRouting, check};

#[test]
fn every_presence_promise_holds_at_ci_bounds() {
    let threads = models::threads();
    let verdict = check(
        &PresenceRouting::ci(),
        Bounds::exhaustive().threads(threads),
    );
    println!("{verdict}");
    verdict.assert_holds();
}

/// The nightly bounds, run by hand or by the nightly recipe.
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
