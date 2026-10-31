//! One daemon under crash, explored over the real core and store at the
//! bounds `just test` runs.

use models::{Bounds, SingleDaemon, check};

/// The journal's bounds and the peer's, each explored in full.
#[test]
fn every_single_daemon_promise_holds_at_ci_bounds() {
    let threads = models::threads();
    for model in [SingleDaemon::ci(), SingleDaemon::peer()] {
        let verdict = check(&model, Bounds::exhaustive().threads(threads));
        println!("{verdict}");
        verdict.assert_holds();
    }
}

/// The nightly bounds, and the peer around the CI bounds' journal and
/// subscriber, run by hand or by the nightly recipe.
#[test]
#[ignore = "nightly bounds: minutes on every core"]
fn every_single_daemon_promise_holds_at_nightly_bounds() {
    for model in [SingleDaemon::nightly(), SingleDaemon::peer_nightly()] {
        let verdict = check(&model, Bounds::exhaustive().threads(models::threads()));
        println!("{verdict}");
        verdict.assert_holds();
    }
}
