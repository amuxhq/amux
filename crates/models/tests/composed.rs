//! The composed net explored over two hosts' real daemon cores and one
//! agent's real agent core, at the bounds `just test` runs.

use models::{Bounds, ComposedNet, check};

#[test]
fn every_composed_promise_holds_at_ci_bounds() {
    let threads = models::threads();
    for (name, model) in ComposedNet::ci() {
        let verdict = check(&model, Bounds::exhaustive().threads(threads));
        println!("{name}:\n{verdict}");
        verdict.assert_holds();
    }
}

/// The nightly bounds, run by hand or by the nightly recipe: minutes on the
/// enrolled Mac's cores.
#[test]
#[ignore = "nightly bounds: minutes on every core"]
fn every_composed_promise_holds_at_nightly_bounds() {
    for (name, model) in ComposedNet::nightly() {
        let verdict = check(&model, Bounds::exhaustive().threads(models::threads()));
        println!("{name}:\n{verdict}");
        verdict.assert_holds();
    }
}
