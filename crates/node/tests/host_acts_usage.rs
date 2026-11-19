//! What the host that does an act counts of it: a new agent, a person's
//! prompt and answer, each once, here, saying where it came from; and the
//! onboarding step a person's first agent from a paired device finishes.

use analytics::{By, Event, Kind, Origin, Recording};
use futures_util::StreamExt as _;
use tonic::Request;
use wire::amux::staging;
use wire::installation_service_server::InstallationService as _;

use crate::host_acts::{EVERYONE, Setup, Who, create, peer, prompt};

/// Every act lands on the host that does it, and only there is it
/// counted, under this host's setting: a new agent whoever asked, saying
/// whether a person or an agent did; a person's prompts, a first one
/// included, and none of an agent's. Each says where it came from: here,
/// or the paired host the call was carried from.
#[tokio::test(flavor = "multi_thread")]
async fn usage_is_counted_by_the_host_that_acts_saying_where_it_came_from() {
    let mut setup = Setup::new();
    let mut workers = Vec::new();
    for who in EVERYONE {
        let parent = setup.parent_of(who);
        workers.push(setup.agent(&format!("{who:?}").to_lowercase(), parent, true));
    }
    let recording = Recording::new();
    let sink = recording.clone();
    let host = setup
        .start_with(move |options| options.analytics = node::Telemetry::Record(sink))
        .await;
    let work = host.install.work.clone();

    let mut expected = Vec::new();
    for (who, worker) in EVERYONE.into_iter().zip(workers) {
        let from = match who {
            Who::PersonHere | Who::AgentHere => Origin::Here,
            Who::PersonThere | Who::AgentThere => Origin::PairedHost(peer()),
        };
        let tag = format!("{who:?}").to_lowercase();
        host.agents(who)
            .create_agent(create(&work, &format!("{tag}-new"), Some("start here")))
            .await
            .unwrap_or_else(|status| panic!("{who:?}: {status:?}"));
        let by = if who.is_agent() {
            By::Agent
        } else {
            By::Person
        };
        expected.push(Event::AgentCreated {
            kind: Kind::ClaudeSdk,
            from,
            by,
        });
        if !who.is_agent() {
            expected.push(Event::PromptSent {
                kind: Kind::ClaudeSdk,
                from,
                queued: false,
            });
        }

        let verdict = host
            .agents(who)
            .send_input(staging::SendInputRequest {
                agent_id: worker.as_bytes().to_vec(),
                input: Some(prompt("a prompt the analytics never carries")),
            })
            .await
            .unwrap_or_else(|status| panic!("{who:?}: {status:?}"));
        assert!(
            matches!(
                verdict.into_inner().of,
                Some(wire::send_input_response::Of::Accepted(_))
            ),
            "{who:?}"
        );
        if !who.is_agent() {
            expected.push(Event::PromptSent {
                kind: Kind::ClaudeSdk,
                from,
                queued: true,
            });
        }

        let answer = wire::Input::new(
            uuid::Uuid::new_v4().as_bytes().to_vec(),
            wire::ClaudeSdkInput {
                of: Some(wire::claude_sdk_input::Of::Answer(wire::ClaudeAnswer {
                    ask_key: "ask-1".into(),
                    of: Some(wire::claude_answer::Of::Question(
                        wire::QuestionAnswer::default(),
                    )),
                })),
            },
        );
        host.agents(who)
            .send_input(staging::SendInputRequest {
                agent_id: worker.as_bytes().to_vec(),
                input: Some(answer),
            })
            .await
            .unwrap_or_else(|status| panic!("{who:?}: {status:?}"));
        if !who.is_agent() {
            expected.push(Event::AskAnswered {
                kind: Kind::ClaudeSdk,
                from,
                ask: analytics::Ask::Question,
            });
        }
    }

    let counted: Vec<_> = recording
        .events()
        .into_iter()
        .filter(|(_, event)| {
            matches!(
                event,
                Event::AgentCreated { .. } | Event::PromptSent { .. } | Event::AskAnswered { .. }
            )
        })
        .collect();
    let here = host.runtime.host();
    assert_eq!(
        counted,
        expected
            .into_iter()
            .map(|event| (here, event))
            .collect::<Vec<_>>()
    );
    host.stop().await;
}

/// The onboarding's first-agent step is done on this host once a person
/// starts an agent here from a paired device, the phone or another
/// computer. An agent's create, from anywhere, and a person's at this
/// machine leave it as it was.
#[tokio::test(flavor = "multi_thread")]
async fn a_persons_first_agent_from_a_paired_device_finishes_onboarding() {
    let setup = Setup::new();
    let host = setup.start().await;
    let work = host.install.work.clone();
    let first_agent = || async {
        let mut steps = host
            .daemon
            .front_door()
            .subscribe_onboarding(Request::new(wire::SubscribeOnboardingRequest::default()))
            .await
            .unwrap()
            .into_inner();
        let onboarding = steps.next().await.unwrap().unwrap().onboarding.unwrap();
        onboarding
            .steps
            .iter()
            .find(|step| step.id() == wire::OnboardingStepId::FirstAgent)
            .unwrap()
            .state()
    };

    assert_eq!(first_agent().await, wire::StepState::Needed);
    // The person at the paired host last, since the step stays done.
    for who in [
        Who::PersonHere,
        Who::AgentHere,
        Who::AgentThere,
        Who::PersonThere,
    ] {
        host.agents(who)
            .create_agent(create(&work, &format!("{who:?}").to_lowercase(), None))
            .await
            .unwrap_or_else(|status| panic!("{who:?}: {status:?}"));
        let expected = if who == Who::PersonThere {
            wire::StepState::Done
        } else {
            wire::StepState::Needed
        };
        assert_eq!(first_agent().await, expected, "after {who:?}'s create");
    }
    host.stop().await;
}
