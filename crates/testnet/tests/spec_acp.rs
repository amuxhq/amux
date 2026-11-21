//! An ACP agent across hosts: a person on the laptop creates one on the
//! desk by its agent's id, and drives it from the laptop as they would a
//! Claude or Codex agent there. Its reply streams to the laptop, its
//! permission request is answered from the laptop, an interrupt from the
//! laptop cancels its turn, and a stop from the laptop ends it on the desk.
//! The desk runs the fake ACP agent, playing the desk's script. A host also
//! lists the ACP agents it offers, for a paired host's client to read.

#![cfg(unix)]

use std::collections::HashMap;
use std::path::PathBuf;

use prost::Message as _;
use provider_fakes::script::{AcpAgent, Call, Offer, Outcome, Script, Step};
use store::AgentRow;
use testnet::{FAKE_ACP_AGENT, HostDecl, Net, Observer, PATIENCE, Topology, until};
use tonic::Request;
use uuid::Uuid;
use wire::agent_service_server::AgentService as _;
use wire::session_service_server::SessionService as _;
use wire::{AcpInput, Lifecycle, Phase, acp_input};

const REPLY: &str = "Hello from the fake agent.";
const PASSED: &str = "The suite passed.";
const STARTED: &str = "Starting the long job.";
const NEVER: &str = "This is never said.";

fn text(chunks: &[&str]) -> Step {
    Step::Text {
        chunks: chunks.iter().map(|chunk| (*chunk).to_owned()).collect(),
    }
}

/// Three turns: a reply streamed a word at a time; a command that asks
/// permission first, then a word on how it went; and a long job that
/// waits on a gate nobody opens, so only an interrupt ends it.
fn desk_script() -> Script {
    Script {
        chunk_ms: 60,
        steps: vec![
            text(&["Hello ", "from ", "the ", "fake ", "agent."]),
            Step::TurnEnd,
            Step::Call(Call {
                kind: "execute".into(),
                title: "Run the test suite".into(),
                name: None,
                input: Some(serde_json::json!({ "command": "cargo test" })),
                locations: Vec::new(),
                diff: None,
                ask: Some(Offer::Once),
                outcome: Outcome {
                    output: "214 passed".into(),
                    ..Outcome::default()
                },
            }),
            text(&[PASSED]),
            Step::TurnEnd,
            text(&[STARTED]),
            Step::WaitFor {
                path: "never".into(),
            },
            text(&[NEVER]),
            Step::TurnEnd,
        ],
        acp: AcpAgent {
            // Nothing of a cancelled turn streams after the cancel, so the
            // words after the gate show only if the turn went on.
            cancel_chunks: 0,
            ..AcpAgent::default()
        },
        ..Script::default()
    }
}

fn input(of: acp_input::Of) -> wire::Input {
    wire::Input::new(
        Uuid::new_v4().as_bytes().to_vec(),
        AcpInput { of: Some(of) },
    )
}

fn prompt(text: &str) -> wire::Input {
    input(acp_input::Of::Prompt(wire::PromptInput {
        text: text.into(),
        attachments: Vec::new(),
    }))
}

/// Sends `input` to `agent` from `host`'s client and expects it accepted.
async fn send(net: &Net, host: &str, agent: Uuid, input: wire::Input) {
    let verdict = net
        .client(host)
        .unwrap()
        .send_input(Request::new(wire::SendInputRequest {
            agent_id: agent.as_bytes().to_vec(),
            input: Some(input),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(
        matches!(verdict.of, Some(wire::send_input_response::Of::Accepted(_))),
        "{verdict:?}"
    );
}

/// `agent`'s row as `host` holds it: the desk's own, the laptop's replica.
async fn row(net: &Net, host: &str, key: &store::AgentKey) -> Option<AgentRow> {
    let key = key.clone();
    net.runtime(host)
        .unwrap()
        .read_store(move |store| store.agent(&key).unwrap())
        .await
        .unwrap()
}

/// Waits until `host`'s row for the agent satisfies `holds`, and returns it.
async fn until_row(
    net: &Net,
    host: &str,
    key: &store::AgentKey,
    what: &str,
    holds: impl Fn(&AgentRow) -> bool,
) -> AgentRow {
    let holds = &holds;
    until(&format!("{what} at {host}"), || async move {
        match row(net, host, key).await {
            Some(row) if holds(&row) => Ok(row),
            Some(row) => Err(format!(
                "phase {:?}, lifecycle {:?}",
                Phase::try_from(row.phase),
                Lifecycle::try_from(row.lifecycle)
            )),
            None => Err("no row".to_owned()),
        }
    })
    .await
    .unwrap()
}

fn phase(row: &AgentRow) -> Phase {
    Phase::try_from(row.phase).unwrap_or(Phase::Unspecified)
}

/// The chat as the session's events build it: each item's text by its
/// key, with what each item's appends added, and how many appends each
/// took.
fn chat(events: &[wire::SessionEvent]) -> (HashMap<String, String>, HashMap<String, usize>) {
    let mut texts = HashMap::new();
    let mut appends = HashMap::new();
    for event in events {
        match &event.of {
            Some(wire::session_event::Of::Item(item)) => {
                texts.insert(item.key.clone(), item.text.clone());
            }
            Some(wire::session_event::Of::Append(append)) => {
                texts
                    .entry(append.key.clone())
                    .or_insert_with(String::new)
                    .push_str(&append.text);
                *appends.entry(append.key.clone()).or_insert(0) += 1;
            }
            _ => {}
        }
    }
    (texts, appends)
}

fn says(events: &[wire::SessionEvent], text: &str) -> bool {
    chat(events).0.values().any(|said| said == text)
}

/// What the desk's fake agent read from its host, a JSON-RPC message a
/// line.
fn read_by_agent(net: &Net) -> Vec<serde_json::Value> {
    let log: PathBuf = net.root().join("scripts").join("desk.host.input");
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

async fn until_read(net: &Net, what: &str, found: impl Fn(&serde_json::Value) -> bool) {
    let found = &found;
    until(what, || async move {
        let read = read_by_agent(net);
        read.iter()
            .any(found)
            .then_some(())
            .ok_or_else(|| format!("the agent read {} lines", read.len()))
    })
    .await
    .unwrap();
}

async fn observe_until(
    observer: &mut Observer,
    what: &str,
    holds: impl Fn(&[wire::SessionEvent]) -> bool,
) {
    if let Err(stuck) = observer.observe_until(holds, PATIENCE).await {
        panic!("waiting for {what}: {stuck}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_acp_agent_on_one_host_is_driven_from_another() {
    let net = Net::start(
        Topology::new()
            .host_decl(HostDecl {
                name: "desk".into(),
                script: Some(desk_script()),
                ..HostDecl::default()
            })
            .host("laptop")
            .link("desk", "laptop"),
    )
    .await
    .unwrap();
    let desk = net.host("desk").unwrap().host_id;

    // Created from the laptop, on the desk, by the agent's id.
    let created = net
        .client("laptop")
        .unwrap()
        .create_agent(Request::new(wire::CreateAgentRequest {
            host: Some(wire::HostRef::named("desk")),
            name: Some("helper".into()),
            cwd: net
                .host("desk")
                .unwrap()
                .work
                .to_string_lossy()
                .into_owned(),
            agent: Some(wire::create_agent_request::Agent::Acp(
                wire::AcpCreateConfig {
                    agent: FAKE_ACP_AGENT.into(),
                    ..wire::AcpCreateConfig::default()
                },
            )),
            ..wire::CreateAgentRequest::default()
        }))
        .await
        .expect("the desk starts the laptop's ACP agent")
        .into_inner()
        .agent
        .unwrap();
    assert_eq!(created.host_id, desk.as_bytes(), "made on the desk");
    assert_eq!(created.kind(), wire::Kind::Acp);
    let id = Uuid::from_slice(&created.agent_id).unwrap();
    let key = store::AgentKey::new(desk.as_bytes().to_vec(), id.as_bytes().to_vec());
    let own = until_row(&net, "desk", &key, "the agent ready", |row| {
        phase(row) == Phase::Idle
    })
    .await;
    let snapshot = wire::AcpSnapshot::decode(own.snapshot.unwrap().body.as_slice()).unwrap();
    assert_eq!(snapshot.agent.as_deref(), Some(FAKE_ACP_AGENT));
    assert_eq!(snapshot.reopen(), wire::AcpReopen::Resume);
    until_row(&net, "laptop", &key, "the agent ready", |row| {
        phase(row) == Phase::Idle
    })
    .await;
    println!("1. created from the laptop on the desk by id {FAKE_ACP_AGENT:?}; ready on both");

    // A prompt from the laptop, its reply streamed to the laptop.
    let mut laptop = net.observe_id("laptop", "helper", id, 50).await.unwrap();
    observe_until(
        &mut laptop,
        "the laptop's view to catch up",
        testnet::observe::caught_up,
    )
    .await;
    send(&net, "laptop", id, prompt("Say hello.")).await;
    observe_until(&mut laptop, "the streamed reply", |events| {
        says(events, REPLY)
    })
    .await;
    let (texts, appends) = chat(laptop.events());
    let reply = texts
        .iter()
        .find(|(_, said)| said.as_str() == REPLY)
        .map(|(key, _)| key.clone())
        .unwrap();
    let streamed = appends.get(&reply).copied().unwrap_or(0);
    assert!(
        streamed > 0,
        "the reply came whole, not streamed: {}",
        laptop.transcript()
    );
    until_row(&net, "laptop", &key, "the turn's end", |row| {
        phase(row) == Phase::Idle
    })
    .await;
    println!("2. the laptop's prompt is answered {REPLY:?}, streamed in {streamed} appends");

    // The permission request, read and answered on the laptop.
    send(&net, "laptop", id, prompt("Run the test suite.")).await;
    let asking = until_row(&net, "laptop", &key, "the permission request", |row| {
        phase(row) == Phase::NeedsYou
    })
    .await;
    let snapshot = wire::AcpSnapshot::decode(asking.snapshot.unwrap().body.as_slice()).unwrap();
    let ask = snapshot.asks.first().expect("an open ask").clone();
    let Some(wire::acp_ask::Body::Permission(permission)) = &ask.body else {
        panic!("a permission request: {ask:?}");
    };
    assert_eq!(permission.title, "Run the test suite");
    let allow = permission
        .options
        .iter()
        .find(|option| option.kind() == wire::AcpPermissionKind::AllowOnce)
        .expect("an allow-once option")
        .option_id
        .clone();
    send(
        &net,
        "laptop",
        id,
        input(acp_input::Of::Answer(wire::AcpAnswer {
            ask_key: ask.key.clone(),
            of: Some(wire::acp_answer::Of::Option(allow.clone())),
        })),
    )
    .await;
    observe_until(&mut laptop, "the word after the command", |events| {
        says(events, PASSED)
    })
    .await;
    until_read(&net, "the agent to read the laptop's answer", |line| {
        line["result"]["outcome"]["optionId"] == allow.as_str()
    })
    .await;
    until_row(&net, "laptop", &key, "the turn's end", |row| {
        phase(row) == Phase::Idle && row_asks(row) == 0
    })
    .await;
    println!(
        "3. the permission request for {:?} is answered {allow:?} from the laptop; the command ran",
        permission.title
    );

    // An interrupt from the laptop cancels the long job.
    send(&net, "laptop", id, prompt("Start the long job.")).await;
    observe_until(&mut laptop, "the long job to start", |events| {
        says(events, STARTED)
    })
    .await;
    until_row(&net, "laptop", &key, "the long job running", |row| {
        phase(row) == Phase::Working
    })
    .await;
    send(
        &net,
        "laptop",
        id,
        input(acp_input::Of::Interrupt(wire::Interrupt {})),
    )
    .await;
    until_read(&net, "the agent to be asked to cancel", |line| {
        line["method"] == "session/cancel"
    })
    .await;
    until_row(&net, "laptop", &key, "the cancelled turn's end", |row| {
        phase(row) == Phase::Idle
    })
    .await;
    assert!(
        !says(laptop.events(), NEVER),
        "the turn went on past the interrupt: {}",
        laptop.transcript()
    );
    println!("4. the laptop's interrupt cancels the long job; the desk's agent is idle again");

    // A stop from the laptop ends it on the desk.
    net.client("laptop")
        .unwrap()
        .file_agent(Request::new(wire::FileAgentRequest {
            agent_id: id.as_bytes().to_vec(),
            filing: wire::Filing::Closed as i32,
        }))
        .await
        .expect("the desk stops the agent");
    for host in ["desk", "laptop"] {
        until_row(&net, host, &key, "the agent stopped", |row| {
            Lifecycle::try_from(row.lifecycle) == Ok(Lifecycle::Exited)
        })
        .await;
    }
    let stopped = row(&net, "desk", &key).await.unwrap();
    println!(
        "5. the laptop's stop ends it on the desk: exited {:?}",
        wire::ExitCause::try_from(stopped.exit_cause)
    );

    drop(laptop);
    net.shutdown().await.unwrap();
}

/// The version the desk's fake agent reports at `initialize`.
const VERSION: &str = "4.5.6";

/// A host lists the ACP agents it offers in its own entry, without any
/// client asking: with what it is called, the kind it runs as, how to sign
/// in, its version, whether it is signed in and what its agents can do. A
/// paired host's client reads all of that from its inventory, and the
/// catalogue the entry names by hash from `GetCatalogue` on that host.
#[tokio::test(flavor = "multi_thread")]
async fn a_hosts_acp_agent_is_offered_with_its_descriptor_and_capabilities_to_another() {
    let script = Script {
        version: Some(VERSION.into()),
        acp: AcpAgent {
            name: "fake-acp".into(),
            image: true,
            resume: true,
            ..AcpAgent::default()
        },
        ..Script::default()
    };
    let net = Net::start(
        Topology::new()
            .host_decl(HostDecl {
                name: "desk".into(),
                script: Some(script),
                ..HostDecl::default()
            })
            .host("laptop")
            .link("desk", "laptop"),
    )
    .await
    .unwrap();
    let desk = net.host("desk").unwrap().host_id;

    // The laptop's inventory, until the desk's entry lists the agent as
    // asked: the desk asks it as it starts.
    let mut inventory = net.observe_inventory("laptop").await.unwrap();
    let listed = |events: &[wire::InventoryEvent]| {
        testnet::observe::inventory_hosts(events)
            .into_iter()
            .find(|host| host.host_id() == desk.as_bytes())
            .and_then(|host| host.provider(FAKE_ACP_AGENT).cloned())
            .filter(|offered| offered.catalogue.is_some())
    };
    inventory
        .observe_until(|events| listed(events).is_some(), PATIENCE)
        .await
        .unwrap_or_else(|stuck| panic!("the desk's entry listing {FAKE_ACP_AGENT}: {stuck}"));
    let entry = testnet::observe::inventory_hosts(inventory.events())
        .into_iter()
        .find(|host| host.host_id() == desk.as_bytes())
        .unwrap();
    let offered = listed(inventory.events()).unwrap();

    let descriptor = offered.descriptor.clone().expect("a descriptor");
    assert_eq!(
        descriptor,
        wire::ProviderDescriptor {
            name: "Fake ACP Agent".into(),
            kind: wire::Kind::Acp as i32,
            sign_in_hint: "Run `fake-acp login` in the terminal".into(),
            version: VERSION.into(),
        },
        "named as the desk's config names it, with the agent's own sign-in words and version"
    );
    assert_eq!(offered.signed_in(), Some(true));
    assert_eq!(
        offered.capabilities,
        Some(wire::AgentCapabilities::acp(true, true, false)),
        "resumable (it offers session/resume), takes images, and folds no prompt into a turn"
    );
    let capabilities = offered.capabilities.unwrap();
    assert!(!capabilities.stops_job && !capabilities.switches_back && !capabilities.attachable);
    // Claude and Codex are listed beside it, each by its own descriptor.
    let ids: Vec<&str> = entry
        .providers
        .iter()
        .map(|provider| provider.provider.as_str())
        .collect();
    assert_eq!(ids, ["claude", "codex", FAKE_ACP_AGENT]);
    for (id, name) in [("claude", "Claude"), ("codex", "Codex")] {
        let own = entry.provider(id).unwrap();
        assert_eq!(own.name(), name, "{id}");
        assert!(own.capabilities.is_some(), "{id} says what it can do");
    }
    println!(
        "1. the laptop reads the desk's {FAKE_ACP_AGENT}: {:?} {} as {:?}, signed in, {capabilities:?}",
        descriptor.name,
        descriptor.version,
        wire::Kind::try_from(descriptor.kind)
    );

    // The catalogue the entry names, asked of the desk from the laptop.
    let catalogue = net
        .client("laptop")
        .unwrap()
        .get_catalogue(Request::new(wire::GetCatalogueRequest {
            host_id: desk.as_bytes().to_vec(),
            provider: FAKE_ACP_AGENT.into(),
        }))
        .await
        .expect("the desk answers what its agent offers")
        .into_inner();
    assert_eq!(
        Some(&catalogue.hash),
        offered.catalogue.as_ref(),
        "the copy the entry names"
    );
    let decoded = wire::Catalogue::decode(catalogue.bytes.as_slice()).unwrap();
    println!(
        "2. GetCatalogue from the laptop answers the entry's copy: {} models, {} modes",
        decoded.models.len(),
        decoded.modes.len()
    );

    drop(inventory);
    net.shutdown().await.unwrap();
}

fn row_asks(row: &AgentRow) -> usize {
    row.snapshot
        .as_ref()
        .and_then(|snapshot| wire::AcpSnapshot::decode(snapshot.body.as_slice()).ok())
        .map_or(0, |snapshot| snapshot.asks.len())
}
