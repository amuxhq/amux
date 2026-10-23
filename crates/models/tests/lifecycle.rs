//! The agent lifecycle explored over the real agent core at the bounds
//! `just test` runs, and walked by seed.

use models::{AgentLifecycle, Bounds, check, walk};

/// How many states each exhaustive run may explore: its count at these
/// bounds on 2026-10-23 with two percent over, since the count moves by a
/// few dozen states with the threads. A run past it says the bounds have
/// outgrown their place; a run that hangs is the test harness's bound to
/// catch.
const ONE_SHOT_CHILD_STATES: u64 = 1_322_700;
const PARENTLESS_STATES: u64 = 72_700;

fn holds_within_budget(name: &str, model: &AgentLifecycle, budget: u64) {
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
        measured.states <= budget,
        "the {name} lifecycle explored {} states, past its {budget}",
        measured.states
    );
}

#[test]
fn every_lifecycle_promise_holds_for_a_one_shot_child_at_ci_bounds() {
    holds_within_budget(
        "one-shot child",
        &AgentLifecycle::ci(),
        ONE_SHOT_CHILD_STATES,
    );
}

#[test]
fn every_lifecycle_promise_holds_for_an_agent_without_a_parent_at_ci_bounds() {
    holds_within_budget(
        "parentless agent",
        &AgentLifecycle::parentless(),
        PARENTLESS_STATES,
    );
}

#[test]
fn one_seed_walks_one_lifecycle_schedule() {
    let model = AgentLifecycle::ci();
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

/// The nightly bounds, run by hand or by the nightly recipe: twelve million
/// states for the one-shot child, under a minute and a half on the enrolled
/// Mac's cores.
#[test]
#[ignore = "nightly bounds: minutes on every core"]
fn every_lifecycle_promise_holds_at_nightly_bounds() {
    for one_shot in [true, false] {
        let verdict = check(
            &AgentLifecycle::nightly(one_shot),
            Bounds::exhaustive().threads(models::threads()),
        );
        println!("one_shot={one_shot}: {verdict}");
        verdict.assert_holds();
    }
}
