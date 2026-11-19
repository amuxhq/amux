//! Product analytics across hosts: a pairing is counted on both sides, each
//! in its own role, and a prompt to a paired host's agent is counted once,
//! on the agent's host, naming the host the person sent it from.

use analytics::{By, CheckCause, Event, Kind, Method, Origin, PairingFailure, Recording, Role};
use node::harness::LiveRoute;
use provider_fakes::script::Step;
use support::*;
use testnet::{AgentDecl, Net, NetOptions, Topology, until};
use wire::{ClaudeSdkInput, PromptInput, begin_pair_request, claude_sdk_input};

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
async fn pairing_counts_on_both_sides_and_a_prompt_counts_on_the_agents_host() {
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
    // can reach the desk.
    let prompt = || {
        wire::Input::new(
            uuid::Uuid::new_v4().as_bytes().to_vec(),
            ClaudeSdkInput {
                of: Some(claude_sdk_input::Of::Prompt(PromptInput {
                    text: "and the tests?".into(),
                    attachments: Vec::new(),
                })),
            },
        )
    };
    let worker = net
        .person("laptop")
        .await
        .unwrap()
        .agent(net.agent("worker").unwrap());
    until("the laptop's prompt to reach the desk's agent", || async {
        let verdict = worker
            .send_input(prompt())
            .await
            .map_err(|error| error.to_string())?;
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
        laptop_events,
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
        ],
        "the laptop carried the prompt and counted none of it"
    );

    let desk_events = of(&recording, desk);
    println!("desk: {desk_events:?}");
    let prompts: Vec<_> = desk_events
        .iter()
        .filter_map(|event| match event {
            Event::PromptSent { kind, from, .. } => Some((*kind, *from)),
            _ => None,
        })
        .collect();
    assert_eq!(prompts, [(Kind::ClaudeSdk, Origin::PairedHost(laptop))]);
    assert_eq!(
        desk_events
            .iter()
            .filter(|event| !matches!(event, Event::PromptSent { .. }))
            .cloned()
            .collect::<Vec<_>>(),
        [
            Event::Installed,
            // The net makes its declared agent as a person on the desk.
            Event::AgentCreated {
                kind: Kind::ClaudeSdk,
                from: Origin::Here,
                by: By::Person,
            },
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
        ]
    );

    net.shutdown().await.unwrap();
}
