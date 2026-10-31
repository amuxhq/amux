//! The agent lifecycle explored over the real agent core at the bounds
//! `just test` runs.

use models::{AgentLifecycle, Bounds, check};

fn holds(name: &str, model: &AgentLifecycle) {
    let threads = models::threads();
    let verdict = check(model, Bounds::exhaustive().threads(threads));
    println!("{name}: {verdict}");
    verdict.assert_holds();
}

#[test]
fn every_lifecycle_promise_holds_for_a_one_shot_child_at_ci_bounds() {
    holds("one-shot child", &AgentLifecycle::ci());
}

#[test]
fn every_lifecycle_promise_holds_for_an_agent_without_a_parent_at_ci_bounds() {
    holds("parentless agent", &AgentLifecycle::parentless());
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
