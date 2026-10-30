//! Product analytics across hosts: a pairing is counted on both sides, each
//! in its own role, and a prompt to a paired host's agent is counted once,
//! on the host where the person sent it, naming the host it went to.

use analytics::{CheckCause, Event, Kind, Method, On, PairingFailure, Recording, Role};
use node::harness::LiveRoute;
use provider_fakes::script::Step;
use support::*;
use testnet::{AgentDecl, Net, NetOptions, Topology, until};
use tonic::Request;
use wire::session_service_server::SessionService as _;
use wire::{ClaudeSdkInput, PromptInput, SendInputRequest, begin_pair_request, claude_sdk_input};

use crate::support;

const SCOPE: &str = "analytics-test";

/// What `host` counted, without steps that ran over their budget: a step's
/// budget is wall-clock time, so a loaded machine counts some and an idle
/// one none.
fn of(recording: &Recording, host: uuid::Uuid) -> Vec<Event> {
    recording
        .events()
        .into_iter()
        .filter(|(by, _)| *by == host)
        .map(|(_, event)| event)
        .filter(|event| {
            !matches!(
                event,
                Event::CheckFailed {
                    cause: CheckCause::OverBudget,
                    ..
                }
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn pairing_counts_on_both_sides_and_a_prompt_counts_where_it_was_sent() {
    let recording = Recording::new();
    let net = Net::start_with(
        Topology::new()
            .host_decl(lan_host("desk", SCOPE))
            .host_decl(lan_host("laptop", SCOPE))
            .agent(AgentDecl::new("worker", "desk").steps(vec![
                Step::Text {
                    chunks: vec!["Done.".into()],
                },
                Step::TurnEnd,
            ])),
        NetOptions {
            analytics: Some(node::Telemetry::Record(recording.clone())),
            ..NetOptions::default()
        },
    )
    .await
    .unwrap();
    let (desk, laptop) = (host_id(&net, "desk"), host_id(&net, "laptop"));

    pair(&net, "laptop", "desk").await;
    until_via(&net, "laptop", "desk", LiveRoute::Direct).await;
    // A second window, joined with the wrong PIN.
    let started = start_pairing(&net, "desk", pin_window()).await.unwrap();
    let wrong = if pin_of(&started) == "000000" {
        "111111"
    } else {
        "000000"
    };
    begin_pair(
        &net,
        "laptop",
        Some(desk),
        begin_pair_request::Secret::Pin(wrong.into()),
        started.addrs,
    )
    .await
    .unwrap_err();

    // The person at the laptop prompts the desk's agent, once the laptop
    // knows whose agent it is.
    let worker = net.agent("worker").unwrap().id;
    let prompt = || SendInputRequest {
        agent_id: worker.as_bytes().to_vec(),
        input: Some(wire::Input::new(
            uuid::Uuid::new_v4().as_bytes().to_vec(),
            ClaudeSdkInput {
                of: Some(claude_sdk_input::Of::Prompt(PromptInput {
                    text: "and the tests?".into(),
                    attachments: Vec::new(),
                })),
            },
        )),
    };
    let laptop_client = net.client("laptop").unwrap();
    until("the laptop's prompt to reach the desk's agent", || async {
        let verdict = laptop_client
            .send_input(Request::new(prompt()))
            .await
            .map_err(|status| status.message().to_owned())?
            .into_inner();
        match verdict.of {
            Some(wire::send_input_response::Of::Accepted(_)) => Ok(()),
            other => Err(format!("{other:?}")),
        }
    })
    .await
    .unwrap();

    let pin = Method::Pin;
    let laptop_events = of(&recording, laptop);
    println!("laptop: {laptop_events:?}");
    assert_eq!(
        laptop_events
            .iter()
            .filter(|event| !matches!(event, Event::PromptSent { .. }))
            .cloned()
            .collect::<Vec<_>>(),
        [
            Event::Installed,
            Event::PairingStarted {
                role: Role::Joiner,
                method: pin
            },
            Event::PairingSucceeded {
                role: Role::Joiner,
                method: pin,
                remote_host: desk
            },
            Event::PairingStarted {
                role: Role::Joiner,
                method: pin
            },
            Event::PairingFailed {
                role: Role::Joiner,
                method: pin,
                reason: PairingFailure::WrongSecret
            },
        ]
    );
    let prompts: Vec<_> = laptop_events
        .iter()
        .filter_map(|event| match event {
            Event::PromptSent { kind, on, .. } => Some((*kind, *on)),
            _ => None,
        })
        .collect();
    assert_eq!(prompts, [(Kind::ClaudeSdk, On::PairedHost(desk))]);

    let desk_events = of(&recording, desk);
    println!("desk: {desk_events:?}");
    assert_eq!(
        desk_events,
        [
            Event::Installed,
            Event::PairingStarted {
                role: Role::Offerer,
                method: pin
            },
            Event::PairingSucceeded {
                role: Role::Offerer,
                method: pin,
                remote_host: laptop
            },
            Event::PairingStarted {
                role: Role::Offerer,
                method: pin
            },
            Event::PairingFailed {
                role: Role::Offerer,
                method: pin,
                reason: PairingFailure::WrongSecret
            },
        ],
        "the forwarded prompt is the laptop's, not the desk's"
    );
}
