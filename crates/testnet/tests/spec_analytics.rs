//! Product analytics across hosts: a pairing is counted on both sides, each
//! in its own role, and a prompt to a paired host's agent is counted once,
//! on the agent's host, naming the host the person sent it from. A page
//! review sent from the browser through its bridge, addressed to the host
//! that owns the agent, is counted there the same way, and the browser is
//! counted opening on the host it is attached to.

use std::process::Stdio;

use analytics::{
    By, CheckCause, Client, Event, Kind, Method, Origin, PairingFailure, Recording, Role,
};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use node::harness::LiveRoute;
use provider_fakes::script::Step;
use serde_json::{Value, json};
use support::*;
use testnet::{AgentDecl, Net, NetOptions, Topology, until};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
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

/// `amux internal browser` for one host, as a browser starts it, driven
/// as the extension drives it.
struct Bridge {
    input: tokio::process::ChildStdin,
    output: tokio::process::ChildStdout,
    next: u64,
    _child: tokio::process::Child,
}

impl Bridge {
    fn start(net: &Net, host: &str) -> Bridge {
        let mut child = tokio::process::Command::new(testnet::Binaries::built().amux())
            .arg("--config")
            .arg(&net.host(host).unwrap().config)
            .args(["internal", "browser", "chrome-extension://test/"])
            .env_remove("AMUX_LOG")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("the bridge starts");
        Bridge {
            input: child.stdin.take().unwrap(),
            output: child.stdout.take().unwrap(),
            next: 1,
            _child: child,
        }
    }

    async fn send(&mut self, frame: &Value) {
        let bytes = serde_json::to_vec(frame).unwrap();
        self.input
            .write_all(&(bytes.len() as u32).to_ne_bytes())
            .await
            .unwrap();
        self.input.write_all(&bytes).await.unwrap();
        self.input.flush().await.unwrap();
    }

    async fn read(&mut self) -> Value {
        let read = async {
            let mut length = [0u8; 4];
            self.output.read_exact(&mut length).await.unwrap();
            let mut frame = vec![0; u32::from_ne_bytes(length) as usize];
            self.output.read_exact(&mut frame).await.unwrap();
            serde_json::from_slice(&frame).unwrap()
        };
        tokio::time::timeout(testnet::REAL_PATIENCE, read)
            .await
            .expect("the bridge answers")
    }

    /// Opens a call and answers its id.
    async fn open(
        &mut self,
        target: &str,
        authority: Option<&str>,
        method: &str,
        request: &impl prost::Message,
    ) -> u64 {
        let id = self.next;
        self.next += 1;
        let mut call = json!({
            "target": target,
            "method": method,
            "body": STANDARD.encode(request.encode_to_vec()),
        });
        if let Some(authority) = authority {
            call["authority"] = json!(authority);
        }
        self.send(&json!({ "id": id, "call": call })).await;
        id
    }

    /// A call answered by one message, or how it ended.
    async fn unary<T: prost::Message + Default>(
        &mut self,
        target: &str,
        authority: Option<&str>,
        method: &str,
        request: impl prost::Message,
    ) -> Result<T, Value> {
        let id = self.open(target, authority, method, &request).await;
        let mut answer = None;
        loop {
            let frame = self.read().await;
            assert_eq!(frame["id"], json!(id), "{frame}");
            if let Some(end) = frame.get("end") {
                return match end["code"].as_i64() {
                    Some(0) => Ok(answer.expect("an answer")),
                    _ => Err(end.clone()),
                };
            }
            assert!(frame.get("more").is_none(), "a small answer in one part");
            let bytes = STANDARD.decode(frame["message"].as_str().unwrap()).unwrap();
            answer = Some(T::decode(bytes.as_slice()).unwrap());
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_page_review_from_the_browser_is_counted_on_the_agents_host() {
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
    let worker = net.agent("worker").unwrap().id.as_bytes().to_vec();

    let mut bridge = Bridge::start(&net, "laptop");
    let hello = bridge.read().await;
    assert_eq!(hello["hello"]["framing"], json!(2), "{hello}");
    let profiles: wire::ListProfilesResponse = bridge
        .unary(
            "installation",
            None,
            "/amux.v1.InstallationService/ListProfiles",
            wire::ListProfilesRequest {},
        )
        .await
        .unwrap();
    let profile: String = profiles.profiles[0]
        .id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    // The extension opens the laptop's fleet, which counts the browser
    // opening there.
    let fleet = bridge
        .open(
            &profile,
            None,
            "/amux.v1.InventoryService/SubscribeInventory",
            &wire::SubscribeInventoryRequest::default(),
        )
        .await;
    let first = bridge.read().await;
    assert_eq!(first["id"], json!(fleet), "{first}");
    bridge.send(&json!({"id": fleet, "cancel": true})).await;
    loop {
        let frame = bridge.read().await;
        if frame["id"] == json!(fleet) && frame.get("end").is_some() {
            break;
        }
    }

    let review = || {
        wire::Input::new(
            uuid::Uuid::new_v4().as_bytes().to_vec(),
            ClaudeSdkInput {
                of: Some(claude_sdk_input::Of::Prompt(PromptInput {
                    text: "Fix this \u{FFFC}".into(),
                    attachments: vec![wire::Attachment {
                        of: Some(wire::attachment::Of::Page(wire::PageReview {
                            url: "http://localhost:5173/".into(),
                            title: "Home".into(),
                            comments: vec![wire::PageComment {
                                text: "Bigger".into(),
                                element: Some(wire::PageElement {
                                    selector: "h1".into(),
                                    ..Default::default()
                                }),
                                quote: "Welcome".into(),
                                image: None,
                            }],
                        })),
                        ..Default::default()
                    }],
                })),
            },
        )
    };
    // Unaddressed, the laptop's node answers, and it owns no such agent.
    let unaddressed = bridge
        .unary::<wire::SendInputResponse>(
            &profile,
            None,
            "/amux.v1.AgentService/SendInput",
            wire::SendInputRequest {
                agent_id: worker.clone(),
                input: Some(review()),
            },
        )
        .await
        .expect_err("the laptop owns no such agent");
    assert_eq!(
        unaddressed["code"],
        json!(tonic::Code::NotFound as i32),
        "{unaddressed}"
    );
    // Addressed to the desk, the laptop carries it there.
    let to_desk = wire::authority(&desk);
    let verdict: wire::SendInputResponse = bridge
        .unary(
            &profile,
            Some(&to_desk),
            "/amux.v1.AgentService/SendInput",
            wire::SendInputRequest {
                agent_id: worker.clone(),
                input: Some(review()),
            },
        )
        .await
        .unwrap();
    assert!(
        matches!(verdict.of, Some(wire::send_input_response::Of::Accepted(_))),
        "the desk's agent took the review: {verdict:?}"
    );

    let counted = |events: Vec<Event>| -> Vec<Event> {
        events
            .into_iter()
            .filter(|event| {
                matches!(
                    event,
                    Event::ClientOpened { .. }
                        | Event::PromptSent { .. }
                        | Event::PageReviewSent { .. }
                )
            })
            .collect()
    };
    until("the desk to count the review", || async {
        let desk_events = counted(of(&recording, desk));
        (desk_events.len() >= 2)
            .then_some(())
            .ok_or_else(|| format!("{desk_events:?}"))
    })
    .await
    .unwrap();
    let desk_events = counted(of(&recording, desk));
    println!("desk: {desk_events:?}");
    let [
        Event::PromptSent {
            kind: Kind::ClaudeSdk,
            from: Origin::PairedHost(from),
            ..
        },
        Event::PageReviewSent {
            comments: 1,
            quotes: 1,
            regions: 0,
        },
    ] = desk_events.as_slice()
    else {
        panic!("the desk counts the prompt and its page review: {desk_events:?}");
    };
    assert_eq!(*from, laptop, "sent from the laptop");
    let laptop_events = counted(of(&recording, laptop));
    println!("laptop: {laptop_events:?}");
    assert_eq!(
        laptop_events,
        [Event::ClientOpened {
            client: Client::Browser
        }],
        "the laptop counts the browser opening, and carried the review uncounted"
    );

    drop(bridge);
    net.shutdown().await.unwrap();
}
