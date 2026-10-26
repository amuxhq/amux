//! Replication explored over two real daemon cores at the bounds `just test`
//! runs, and walked by seed.

use models::{Bounds, Outcome, Replication, check, walk};

/// How many states the exhaustive run may explore: its count at these
/// bounds (157,754 on 2026-10-26; 157,737 on four threads) with two percent
/// over, since the count moves with the threads. A run past it says the
/// bounds have outgrown their place; a run that hangs is the test harness's
/// bound to catch.
const BUDGET_STATES: u64 = 161_000;

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
        measured.states <= BUDGET_STATES,
        "the replication model explored {} states, past its {BUDGET_STATES}",
        measured.states
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
