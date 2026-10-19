//! The composed net explored over two hosts' real daemon cores and one
//! agent's real agent core, at the bounds `just test` runs, and walked by
//! seed.

use models::{Bounds, ComposedNet, check, walk};

/// How long the three exhaustive runs together may take under `just
/// test`, sized to CI's four-core runner by the crate's rule until the
/// runner measures it: their four-thread Mac time (42 s) times 4.2,
/// doubled, rounded up to a whole minute. A run past it says the bounds
/// have outgrown their place.
const BUDGET_SECONDS: f64 = 360.0;

#[test]
fn every_composed_promise_holds_at_ci_bounds() {
    let threads = models::threads();
    let mut seconds = 0.0;
    for (name, model) in ComposedNet::ci() {
        let verdict = check(&model, Bounds::exhaustive().threads(threads));
        println!("{name}:\n{verdict}");
        let measured = verdict.measured;
        println!(
            "{name}, measured on {threads} threads: states={} seconds={:.1} states_per_second={:.0}",
            measured.states, measured.seconds, measured.states_per_second
        );
        verdict.assert_holds();
        seconds += measured.seconds;
    }
    assert!(
        seconds <= BUDGET_SECONDS,
        "the composed net took {seconds:.1}s, past its {BUDGET_SECONDS}s budget"
    );
}

#[test]
fn one_seed_walks_one_composed_schedule() {
    let model = ComposedNet::reset_while_flipping();
    let bounds = Bounds::depth(200);
    let first = walk(&model, bounds, 7);
    let again = walk(&model, bounds, 7);
    println!("{first}");
    assert_eq!(
        first.actions, again.actions,
        "one seed walked two schedules"
    );
    assert!(first.failure.is_none(), "{first}");
    let other = walk(&model, bounds, 8);
    assert_ne!(
        first.actions, other.actions,
        "two seeds walked one schedule"
    );
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
