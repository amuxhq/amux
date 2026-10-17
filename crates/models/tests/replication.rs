//! Replication explored over two real stores at the bounds `just test` runs.

use models::{Bounds, Outcome, Replication, check};

/// Every core explores at once: the verdict and the state count are the
/// same on any number of threads, only which of several shortest
/// counterexamples is printed may differ.
fn threads() -> usize {
    std::thread::available_parallelism().map_or(1, |threads| threads.get())
}

#[test]
fn every_replication_promise_holds_at_ci_bounds() {
    let verdict = check(&Replication::ci(), Bounds::exhaustive().threads(threads()));
    println!("{verdict}");
    verdict.assert_holds();
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
