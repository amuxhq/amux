//! The supervisor's updates explored over the real supervisor core at the
//! bounds `just test` runs, and walked by seed.

use models::{Bounds, SupervisorUpdate, check, walk};

fn holds(name: &str, model: &SupervisorUpdate) {
    let threads = models::threads();
    let verdict = check(model, Bounds::exhaustive().threads(threads));
    println!("{name}: {verdict}");
    verdict.assert_holds();
}

#[test]
fn every_update_promise_holds_where_a_running_binary_can_be_replaced() {
    holds("replace while running", &SupervisorUpdate::ci());
}

#[test]
fn every_update_promise_holds_where_the_daemon_stops_before_the_swap() {
    holds("stop before the swap", &SupervisorUpdate::windows());
}

#[test]
fn one_seed_walks_one_update_schedule() {
    let model = SupervisorUpdate::ci();
    let bounds = Bounds::depth(30);
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
fn every_update_promise_holds_at_nightly_bounds() {
    for replace_while_running in [true, false] {
        let verdict = check(
            &SupervisorUpdate::nightly(replace_while_running),
            Bounds::exhaustive().threads(models::threads()),
        );
        println!("replace_while_running={replace_while_running}: {verdict}");
        verdict.assert_holds();
    }
}
