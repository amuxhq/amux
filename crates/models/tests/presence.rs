//! Presence and routing explored over the real daemon core at the bounds
//! `just test` runs, and walked by seed.

use daemon_core::Presence;
use models::presence::{Action, Answered, State, Via};
use models::{Bounds, Machine, PresenceRouting, check, walk};

/// How many states the exhaustive run may explore: its count at these
/// bounds (167,950 on 2026-10-30) with two percent over, since the count
/// can move with the threads. A run past it says the bounds have outgrown
/// their place; a run that hangs is the test harness's bound to catch.
const BUDGET_STATES: u64 = 171_400;

#[test]
fn every_presence_promise_holds_at_ci_bounds() {
    let threads = models::threads();
    let verdict = check(
        &PresenceRouting::ci(),
        Bounds::exhaustive().threads(threads),
    );
    println!("{verdict}");
    let measured = verdict.measured;
    println!(
        "measured on {threads} threads: states={} seconds={:.1} states_per_second={:.0}",
        measured.states, measured.seconds, measured.states_per_second
    );
    verdict.assert_holds();
    assert!(
        measured.states <= BUDGET_STATES,
        "the presence model explored {} states, past its {BUDGET_STATES}",
        measured.states
    );
}

#[test]
fn one_seed_walks_one_presence_schedule() {
    let model = PresenceRouting::ci();
    let bounds = Bounds::depth(40);
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
fn every_presence_promise_holds_at_nightly_bounds() {
    let verdict = check(
        &PresenceRouting::nightly(),
        Bounds::exhaustive().threads(models::threads()),
    );
    println!("{verdict}");
    verdict.assert_holds();
}

/// Takes `action` from `state`, which must enable it.
fn take(model: &PresenceRouting, state: &State, action: Action) -> State {
    let mut enabled = Vec::new();
    model.actions(state, &mut enabled);
    assert!(
        enabled.contains(&action),
        "{action:?} is not enabled in {state:?}: {enabled:?}"
    );
    model.next(state, &action).expect("an enabled action steps")
}

/// At the CI bounds the relay refuses the launch's dial through it for the
/// plan, which reads the host away; once the host has left its address
/// and the plan is bought and heard of, the core dials again at once and
/// the relay carries it, which reads the host online over a link of its
/// own.
#[test]
fn the_relay_refuses_then_carries_a_dial_at_ci_bounds() {
    let model = PresenceRouting::ci();
    let launched = model.init();
    assert!(launched.dialling(), "{launched:?}");
    // Arrived at the relay, the target is left to dial this host; once the
    // wait is over, this one dials it there.
    let claimed = take(&model, &launched, Action::ClaimUp(Via::Relay));
    assert!(!claimed.dialling(), "{claimed:?}");
    let waited = take(&model, &claimed, Action::Retry);
    assert!(waited.dialling(), "{waited:?}");
    let refused = take(&model, &waited, Action::Refused(Via::Relay));
    assert_eq!(refused.presence(), Presence::Away, "{refused:?}");

    let left = take(&model, &refused, Action::Leave);
    let lapsed = take(&model, &left, Action::Deadline);
    assert!(!lapsed.dialling(), "{lapsed:?}");
    let bought = take(&model, &lapsed, Action::PlanChanged);
    let heard = take(&model, &bought, Action::PlanHeard);
    assert!(heard.dialling(), "{heard:?}");
    let carried = take(
        &model,
        &heard,
        Action::Answer(Answered::Through(Via::Relay)),
    );
    assert_eq!(carried.presence(), Presence::Online, "{carried:?}");
    assert!(
        carried.active().is_some_and(|route| route.is_direct()),
        "{carried:?}"
    );
    assert!(!carried.dialling(), "{carried:?}");
}
