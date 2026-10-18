//! Replication explored over two real daemon cores at the bounds `just test`
//! runs, and walked by seed.

use models::{Bounds, Outcome, Replication, check, walk};

/// How long the exhaustive run may take under `just test`, sized to CI's
/// four-core runner: three times the run's time on four of the enrolled
/// Mac's threads (66 s) fits inside it. A run past it says the bounds have
/// outgrown their place.
const BUDGET_SECONDS: f64 = 240.0;

#[test]
fn every_replication_promise_holds_at_ci_bounds() {
    let threads = models::threads();
    let verdict = check(&Replication::ci(), Bounds::exhaustive().threads(threads));
    println!("{verdict}");
    let measured = verdict.measured;
    println!(
        "measured on {threads} threads: states={} seconds={:.1} states_per_second={:.0}",
        measured.states, measured.seconds, measured.states_per_second
    );
    verdict.assert_holds();
    assert!(
        measured.seconds <= BUDGET_SECONDS,
        "the replication model took {:.1}s, past its {BUDGET_SECONDS}s budget",
        measured.seconds
    );
}

#[test]
fn a_wrong_promise_fails_with_a_counterexample() {
    let model = Replication {
        wrong: true,
        ..Replication::ci()
    };
    let verdict = check(&model, Bounds::depth(6));
    println!("{verdict}");
    let Some(Outcome::Fails(path)) = verdict.outcome("the block is never non-empty") else {
        panic!("the wrong promise should fail\n{verdict}");
    };
    assert!(!path.actions.is_empty());
}

#[test]
fn one_seed_walks_one_replication_schedule() {
    let model = Replication::ci();
    let bounds = Bounds::depth(60);
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
