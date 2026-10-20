//! The supervisor's updates explored over the real supervisor core at the
//! bounds `just test` runs, and walked by seed.

use models::{Bounds, SupervisorUpdate, check, walk};

/// How long each exhaustive run may take under `just test`, sized to CI's
/// four-core runner: twice the slowest of the binary's measured times there
/// (27 s for both runs; the README lists them all), rounded up to a whole
/// minute. A run past it says the bounds have outgrown their place.
const BUDGET_SECONDS: f64 = 60.0;

fn holds_within_budget(name: &str, model: &SupervisorUpdate) {
    let threads = models::threads();
    let verdict = check(model, Bounds::exhaustive().threads(threads));
    println!("{name}: {verdict}");
    let measured = verdict.measured;
    println!(
        "{name} measured on {threads} threads: states={} seconds={:.1} states_per_second={:.0}",
        measured.states, measured.seconds, measured.states_per_second
    );
    verdict.assert_holds();
    assert!(
        measured.seconds <= BUDGET_SECONDS,
        "the {name} supervisor took {:.1}s, past its {BUDGET_SECONDS}s budget",
        measured.seconds
    );
}

#[test]
fn every_update_promise_holds_where_a_running_binary_can_be_replaced() {
    holds_within_budget("replace while running", &SupervisorUpdate::ci());
}

#[test]
fn every_update_promise_holds_where_the_daemon_stops_before_the_swap() {
    holds_within_budget("stop before the swap", &SupervisorUpdate::windows());
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
