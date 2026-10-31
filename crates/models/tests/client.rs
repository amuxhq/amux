//! One open chat's session state, explored over the real `ui_state` at the
//! bounds `just test` runs.

use models::{Bounds, ClientModel, check};

#[test]
fn every_client_promise_holds_at_ci_bounds() {
    let threads = models::threads();
    let verdict = check(&ClientModel::ci(), Bounds::exhaustive().threads(threads));
    println!("{verdict}");
    verdict.assert_holds();
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
