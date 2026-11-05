//! First-run onboarding as the desktop clients run it: which step opens,
//! which comes next, and how many are left to finish, all from the steps
//! the daemon serves. Each client draws the steps in its own words; the
//! order and what counts as left are decided here once.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use wire::{OnboardingStepId, StepState};

/// One step of onboarding, in the order clients show them.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SetupStep {
    /// Connect a phone: the pairing window.
    PairPhone,
    /// Keep the machine reachable: start at login, keep awake.
    KeepReachable,
    /// Reach it from anywhere: sign in to an account.
    SignIn,
    /// Pick the folders new agents are offered.
    Repositories,
    /// Usage counts and automatic reports.
    Improve,
    /// Start an agent from the phone.
    FirstAgent,
}

impl SetupStep {
    /// The step a wire id names; none for an id this build does not know.
    pub fn of(id: OnboardingStepId) -> Option<SetupStep> {
        Some(match id {
            OnboardingStepId::PairPhone => SetupStep::PairPhone,
            OnboardingStepId::KeepReachable => SetupStep::KeepReachable,
            OnboardingStepId::SignIn => SetupStep::SignIn,
            OnboardingStepId::Repositories => SetupStep::Repositories,
            OnboardingStepId::Improve => SetupStep::Improve,
            OnboardingStepId::FirstAgent => SetupStep::FirstAgent,
            OnboardingStepId::Unspecified => return None,
        })
    }

    /// The step's id on the wire, for an answer.
    pub fn id(self) -> OnboardingStepId {
        match self {
            SetupStep::PairPhone => OnboardingStepId::PairPhone,
            SetupStep::KeepReachable => OnboardingStepId::KeepReachable,
            SetupStep::SignIn => OnboardingStepId::SignIn,
            SetupStep::Repositories => OnboardingStepId::Repositories,
            SetupStep::Improve => OnboardingStepId::Improve,
            SetupStep::FirstAgent => OnboardingStepId::FirstAgent,
        }
    }
}

/// What a desktop client does with onboarding as it stands.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OnboardingView {
    /// Onboarding opens full-screen before home, at this step: the first
    /// one still needed. None once every step is done or skipped.
    pub opens_at: Option<SetupStep>,
    /// How many steps home offers to finish: those still needed, and those
    /// skipped unless the person dismissed finishing them.
    pub to_finish: u32,
    /// The analytics notice has not been shown by any client yet: the
    /// first screen this client shows carries it.
    pub notice: bool,
}

fn steps(onboarding: &wire::Onboarding) -> impl Iterator<Item = (SetupStep, StepState)> + '_ {
    onboarding
        .steps
        .iter()
        .filter_map(|step| Some((SetupStep::of(step.id())?, step.state())))
}

/// What onboarding calls for now.
pub fn onboarding_view(onboarding: &wire::Onboarding) -> OnboardingView {
    let count = |wanted: StepState| {
        steps(onboarding)
            .filter(|(_, state)| *state == wanted)
            .count()
    };
    let opens_at = steps(onboarding)
        .find(|(_, state)| *state == StepState::Needed)
        .map(|(step, _)| step);
    let skipped = if onboarding.dismissed {
        0
    } else {
        count(StepState::Skipped)
    };
    let left = count(StepState::Needed) + skipped;
    OnboardingView {
        opens_at,
        to_finish: u32::try_from(left).unwrap_or(u32::MAX),
        notice: !onboarding.notice_shown,
    }
}

/// The step a run of onboarding shows after `after` (from the start when
/// none): the next one still needed and, when the person came back to
/// finish, the next one skipped too. None ends the run.
pub fn next_step(
    onboarding: &wire::Onboarding,
    after: Option<SetupStep>,
    finishing: bool,
) -> Option<SetupStep> {
    steps(onboarding)
        .filter(|(step, _)| after.is_none_or(|after| *step > after))
        .find(|(_, state)| {
            *state == StepState::Needed || (finishing && *state == StepState::Skipped)
        })
        .map(|(step, _)| step)
}

/// Whether `step` is still to do: needed, or skipped.
pub fn step_open(onboarding: &wire::Onboarding, step: SetupStep) -> bool {
    steps(onboarding)
        .any(|(at, state)| at == step && matches!(state, StepState::Needed | StepState::Skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn onboarding(states: [StepState; 6], dismissed: bool) -> wire::Onboarding {
        let ids = [
            OnboardingStepId::PairPhone,
            OnboardingStepId::KeepReachable,
            OnboardingStepId::SignIn,
            OnboardingStepId::Repositories,
            OnboardingStepId::Improve,
            OnboardingStepId::FirstAgent,
        ];
        wire::Onboarding {
            profile_id: Vec::new(),
            steps: ids
                .iter()
                .zip(states)
                .map(|(id, state)| wire::OnboardingStep {
                    id: *id as i32,
                    state: state as i32,
                })
                .collect(),
            notice_shown: true,
            dismissed,
        }
    }

    use StepState::{Done, Needed, NotNeeded, Skipped};

    #[test]
    fn it_opens_at_the_first_needed_step_and_walks_the_rest() {
        let now = onboarding([Done, NotNeeded, Needed, Skipped, Needed, Needed], false);
        let view = onboarding_view(&now);
        assert_eq!(view.opens_at, Some(SetupStep::SignIn));
        assert_eq!(view.to_finish, 4);
        assert_eq!(
            next_step(&now, Some(SetupStep::SignIn), false),
            Some(SetupStep::Improve),
            "a step skipped before is not shown again on a resume"
        );
        assert_eq!(
            next_step(&now, Some(SetupStep::SignIn), true),
            Some(SetupStep::Repositories),
            "finishing walks the skipped steps too"
        );
        assert_eq!(next_step(&now, Some(SetupStep::FirstAgent), true), None);
    }

    #[test]
    fn skipped_through_it_offers_to_finish_until_dismissed() {
        let skipped = onboarding([Skipped, NotNeeded, Done, Skipped, Done, Skipped], false);
        let view = onboarding_view(&skipped);
        assert_eq!(view.opens_at, None);
        assert_eq!(view.to_finish, 3);
        assert_eq!(next_step(&skipped, None, true), Some(SetupStep::PairPhone));
        let dismissed = onboarding([Skipped, NotNeeded, Done, Skipped, Done, Skipped], true);
        assert_eq!(onboarding_view(&dismissed).to_finish, 0);
        let done = onboarding([Done, NotNeeded, Done, Done, Done, Done], false);
        assert_eq!(onboarding_view(&done), OnboardingView::default());
    }
}
