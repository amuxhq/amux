//! Replication explored over two real daemon cores at the bounds `just test`
//! runs.

use models::{Bounds, Replication, check};

#[test]
fn every_replication_promise_holds_at_ci_bounds() {
    let threads = models::threads();
    let verdict = check(&Replication::ci(), Bounds::exhaustive().threads(threads));
    println!("{verdict}");
    verdict.assert_holds();
}

/// The nightly bounds, run by hand or by the nightly recipe: minutes on the
/// enrolled Mac's cores.
#[test]
#[ignore = "nightly bounds: minutes on every core"]
fn every_replication_promise_holds_at_nightly_bounds() {
    let verdict = check(
        &Replication::nightly(),
        Bounds::exhaustive().threads(models::threads()),
    );
    println!("{verdict}");
    verdict.assert_holds();
}
