//! Presence and routing explored over the real daemon core at the bounds
//! `just test` runs, and walked by seed.

use daemon_core::{HostId, Route};
use models::presence::{Action, Concluded, State, Via};
use models::{Bounds, Machine, PresenceRouting, check, walk};

/// How long the exhaustive run may take under `just test`, sized to CI's
/// four-core runner. No runner has timed these bounds, so the budget is the
/// four-thread Mac time (50.6 s, 900,130 states) times 4.7, the slowest
/// runner-to-Mac ratio, doubled and rounded up to a whole minute. With two
/// failures and the plan heard at once the bounds explored 1,184,108 states,
/// about 330 s projected on the runner, and twice that passed the 600 s
/// cap, so the second failure moved to the nightly bounds. A run past it
/// says the bounds have outgrown their place.
const BUDGET_SECONDS: f64 = 480.0;

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
        measured.seconds <= BUDGET_SECONDS,
        "the presence model took {:.1}s, past its {BUDGET_SECONDS}s budget",
        measured.seconds
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

/// The CI bounds keep the relay refusing the host's packets for the plan
/// both while an attempt through it is in flight and while the route through
/// it carries the host's traffic, the plan having lapsed before this host
/// heard.
#[test]
fn the_relay_refuses_packets_at_ci_bounds() {
    let model = PresenceRouting::ci();
    let relayed = Route::Via(HostId([9; 16]));
    let refused = Action::ForwardRefused(Via::Relay, true);

    let claimed = take(&model, &model.init(), Action::ClaimUp(Via::Relay));
    let trying = take(&model, &claimed, Action::TimerFired);
    assert!(trying.attempting(relayed), "{trying:?}");
    take(&model, &trying, refused.clone());

    let bought = take(&model, &model.init(), Action::PlanChanged);
    let paid = take(&model, &bought, Action::PlanHeard);
    let claimed = take(&model, &paid, Action::ClaimUp(Via::Relay));
    let trying = take(&model, &claimed, Action::TimerFired);
    let carried = take(
        &model,
        &trying,
        Action::AttemptConcluded(relayed, Concluded::Ok),
    );
    assert_eq!(carried.active(), Some(relayed), "{carried:?}");
    let lapsed = take(&model, &carried, Action::PlanChanged);
    take(&model, &lapsed, refused);
}
