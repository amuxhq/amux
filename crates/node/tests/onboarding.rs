//! First-run onboarding as the front door serves it: the steps derived
//! from what the daemon knows, the answers a client gives kept across a
//! restart, their usage counted, and the projects worth suggesting.

use std::sync::Arc;

use analytics::{Client, Event, Recording, Step, StepOutcome};
use futures_util::StreamExt as _;
use support::synthetic::*;
use support::*;
use tonic::Request;
use wire::answer_onboarding_request::Answer;
use wire::installation_service_server::InstallationService as _;
use wire::{AnswerOnboardingRequest, OnboardingStepId as Id, StepState, Surface};

use crate::support;

fn answer(answer: Answer) -> Request<AnswerOnboardingRequest> {
    Request::new(AnswerOnboardingRequest {
        profile_id: Vec::new(),
        answer: Some(answer),
        surface: Surface::Terminal as i32,
    })
}

fn states(onboarding: &wire::Onboarding) -> Vec<(Id, StepState)> {
    onboarding
        .steps
        .iter()
        .map(|step| (step.id(), step.state()))
        .collect()
}

async fn start(install: &Install, boot: &str, recording: &Arc<Recording>) -> node::Daemon {
    let mut options = install.options(boot, quiet_launch());
    options.analytics = node::Telemetry::Record(recording.clone());
    node::start(options, None).await.unwrap()
}

/// The steps start needed, follow each answer on the stream, refuse a
/// confirmation of a step only the daemon can see done, and are kept
/// across a restart; each answer is counted with the client that gave it.
#[tokio::test(flavor = "multi_thread")]
async fn answers_move_the_steps_and_outlive_a_restart() {
    use StepState::{Needed, NotNeeded, Skipped};
    let install = Install::new();
    let recording = Recording::new();
    let daemon = start(&install, "boot-1", &recording).await;
    let door = daemon.front_door();
    let main = runtime(&daemon, &install).profile();

    let mut steps = door
        .subscribe_onboarding(Request::new(wire::OnboardingRequest::default()))
        .await
        .unwrap()
        .into_inner();
    let first = steps.next().await.expect("the steps now").unwrap();
    assert_eq!(first.profile_id, main.as_bytes(), "the oldest profile's");
    assert!(!first.notice_shown);
    assert_eq!(
        states(&first),
        [
            (Id::PairPhone, Needed),
            (Id::KeepReachable, NotNeeded),
            (Id::SignIn, Needed),
            (Id::Projects, Needed),
            (Id::Improve, Needed),
            (Id::FirstAgent, Needed),
        ],
        "nothing to keep reachable without a supervisor"
    );

    door.answer_onboarding(answer(Answer::Shown(Id::PairPhone as i32)))
        .await
        .unwrap();
    let skipped = door
        .answer_onboarding(answer(Answer::Skip(Id::PairPhone as i32)))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(skipped.steps[0].state(), Skipped);
    assert!(!skipped.dismissed, "skipping is not dismissing");
    let streamed = steps.next().await.expect("the change").unwrap();
    assert_eq!(streamed, skipped);

    let refused = door
        .answer_onboarding(answer(Answer::Done(Id::SignIn as i32)))
        .await
        .unwrap_err();
    assert_eq!(refused.code(), tonic::Code::InvalidArgument, "{refused:?}");

    let done = door
        .answer_onboarding(answer(Answer::Done(Id::Projects as i32)))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(done.steps[3].state(), StepState::Done);
    let noticed = door
        .answer_onboarding(answer(Answer::NoticeShown(wire::Empty {})))
        .await
        .unwrap()
        .into_inner();
    assert!(noticed.notice_shown);
    assert!(node::notice_shown_in(&install.data_dir));
    let dismissed = door
        .answer_onboarding(answer(Answer::Dismiss(wire::Empty {})))
        .await
        .unwrap()
        .into_inner();
    assert!(
        dismissed.steps.iter().all(|step| step.state() != Needed),
        "{dismissed:?}"
    );
    assert!(
        dismissed.dismissed,
        "the skipped steps stay skipped, and finishing them is no longer offered"
    );
    door.answer_onboarding(answer(Answer::SignInOfferShown(
        wire::OfferRole::Receiver as i32,
    )))
    .await
    .unwrap();
    assert_eq!(
        recording.named("sign_in_offer"),
        [Event::SignInOffer {
            role: analytics::OfferRole::Receiver,
            outcome: analytics::OfferOutcome::Shown,
        }]
    );

    let step = |step, outcome| Event::OnboardingStep {
        step,
        outcome,
        surface: Client::Terminal,
    };
    assert_eq!(
        recording.named("onboarding_step"),
        [
            step(Step::PairPhone, StepOutcome::Shown),
            step(Step::PairPhone, StepOutcome::Skipped),
            step(Step::Projects, StepOutcome::Done),
            step(Step::SignIn, StepOutcome::Skipped),
            step(Step::Improve, StepOutcome::Skipped),
            step(Step::FirstAgent, StepOutcome::Skipped),
        ]
    );
    assert_eq!(
        recording.named("projects_chosen"),
        [Event::ProjectsChosen {
            kept: 0,
            suggested: 0
        }]
    );

    drop(steps);
    daemon.shutdown().await.unwrap();
    let daemon = start(&install, "boot-2", &recording).await;
    let again = daemon
        .front_door()
        .subscribe_onboarding(Request::new(wire::OnboardingRequest::default()))
        .await
        .unwrap()
        .into_inner()
        .next()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(states(&again), states(&dismissed));
    assert!(again.notice_shown && again.dismissed);
    daemon.shutdown().await.unwrap();
}

/// The projects suggested are the repositories Claude Code ran in lately,
/// read from the home the daemon was given.
#[tokio::test(flavor = "multi_thread")]
async fn suggestions_come_from_the_session_logs_in_the_home() {
    let install = Install::new();
    let home = install.path("home");
    // Under the home, which is under the system's temporary directory.
    let repo = home.join("code/app");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let repo = repo.canonicalize().unwrap();
    let log = home.join(".claude/projects/-app/session.jsonl");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, format!("{}\n", serde_json::json!({"cwd": repo}))).unwrap();

    let mut options = install.options("boot-1", quiet_launch());
    options.settings.home = Some(home);
    let daemon = node::start(options, None).await.unwrap();
    let suggested = daemon
        .front_door()
        .suggest_projects(Request::new(wire::Empty {}))
        .await
        .unwrap()
        .into_inner()
        .suggestions;
    assert_eq!(suggested.len(), 1, "{suggested:?}");
    assert_eq!(suggested[0].path, repo.to_str().unwrap());
    assert_eq!(suggested[0].name, "app");
    daemon.shutdown().await.unwrap();
}
