//! Families across hosts: an agent on one host spawns a child on another
//! by the host's name, the parent's host forwards the create there, and
//! every host's fleet groups the family from the replica rows it holds.
//! Stop and send are checked against the parent edge wherever the child
//! lives; a cascade delete reaches every child through its own host, and
//! deletes nothing while part of the family is out of reach; and the
//! deliveries outbox carries a
//! child's finished and failed messages over the peer link, exactly once,
//! to the incarnation that was waiting for them.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use node::SourceVerdict;
use prost::Message as _;
use provider_fakes::script::{Ask, Question, Script, Step};
use store::{AgentKey, AgentRow};
use testnet::{AgentDecl, Net, NetError, PATIENCE, Topology, holds_for, observe, until};
use tonic::{Code, Request};
use ui_state::{Attention, FleetMsg, FleetState};
use uuid::Uuid;
use wire::client_service_server::ClientService as _;
use wire::{
    AgentRef, AgentSender, AmbiguousHostName, ClaudeSdkInput, CreateAgentRequest, Envelope,
    EnvelopeKind, InventoryEvent, Kind, Lifecycle, Phase, Sender, claude_sdk_input,
    inventory_event, sender,
};

fn text(text: &str) -> Step {
    Step::Text {
        chunks: vec![text.to_owned()],
    }
}

fn says(label: &str) -> Vec<Step> {
    vec![text(label), Step::TurnEnd]
}

/// The lead: one turn that plans, then idle, taking what its children
/// send it as turns of their own.
fn lead() -> AgentDecl {
    AgentDecl::new("lead", "desk")
        .prompt("Plan the release survey.")
        .steps(says("I will have a helper on the server run the suite."))
}

fn child(name: &str, host: &str, steps: Vec<Step>) -> AgentDecl {
    AgentDecl::new(name, host)
        .prompt("Run the test suite and report.")
        .steps(steps)
}

/// A turn that waits on gate `name` in the net's gate directory.
fn waits(name: &str) -> Step {
    Step::WaitFor { path: name.into() }
}

fn question() -> Step {
    Step::Ask(Ask::Question {
        questions: vec![Question {
            question: "Which suite should I run?".into(),
            header: "Suite".into(),
            options: vec!["unit".into(), "full".into()],
            multi_select: false,
            other: false,
            secret: false,
        }],
    })
}

/// A message as an agent's tool server submits it.
fn message(id: &[u8], to: &AgentRef, text: &str) -> wire::SendMessageRequest {
    wire::SendMessageRequest {
        id: id.to_vec(),
        to: Some(to.clone()),
        text: text.into(),
        context: None,
        attachments: Vec::new(),
    }
}

/// A message as a peer daemon forwards it, its sender stamped.
fn envelope(id: &[u8], to: &AgentRef, text: &str) -> Envelope {
    Envelope {
        id: id.to_vec(),
        to: Some(to.clone()),
        text: text.into(),
        ..Envelope::default()
    }
}

fn interrupt() -> wire::Input {
    wire::Input::new(
        Uuid::new_v4().as_bytes().to_vec(),
        ClaudeSdkInput {
            of: Some(claude_sdk_input::Of::Interrupt(wire::Interrupt {})),
        },
    )
}

/// `agent`'s row as `host` holds it: its own row there, or a replica.
async fn row(net: &Net, host: &str, agent: &str) -> Option<AgentRow> {
    let key = net.agent(agent).unwrap().key();
    net.runtime(host)
        .unwrap()
        .store()
        .await
        .agent(&key)
        .unwrap()
}

/// What `agent`'s own host holds for it that came in as an input other
/// than its first prompt: the agent messages it accepted.
async fn received(net: &Net, agent: &str) -> Vec<wire::Item> {
    items(net, agent)
        .await
        .into_iter()
        .filter(|item| !item.input_id.is_empty() && item.input_id != b"testnet-first")
        .collect()
}

/// Everything `agent`'s own host holds for it, oldest first.
async fn items(net: &Net, agent: &str) -> Vec<wire::Item> {
    let at = net.agent(agent).unwrap().clone();
    net.runtime(&at.host)
        .unwrap()
        .store()
        .await
        .page(&at.key(), None, 1_000)
        .unwrap()
        .items
}

async fn with_input(net: &Net, agent: &str, input_id: &[u8]) -> usize {
    received(net, agent)
        .await
        .iter()
        .filter(|item| item.input_id == input_id)
        .count()
}

/// How many lines `agent`'s provider has read that carry `text`, once
/// everything its process had accepted when this was called has reached
/// the provider: its host has ingested the journal as far as it went then,
/// the agent is idle with nothing queued, and a turn has ended since the
/// newest accepted agent message carrying `text`. A prompt is queued until
/// it is delivered, but an agent message is not: the host hands it to the
/// provider as it accepts it, and the row reads idle with nothing queued
/// until the provider takes it and runs the turn it starts. Every kind
/// writes a `turn:` item when a turn ends, and a delivered message is read
/// before the turn it starts or folds into can end, so a duplicate
/// accepted by then is counted.
async fn taken(net: &Net, agent: &str, text: &str) -> usize {
    let end = net.journal_end(agent).unwrap();
    until(
        &format!("{agent} idle past {end}, its messages taken"),
        || async move {
            let Some(row) = row(net, &net.agent(agent).unwrap().host, agent).await else {
                return Err("no row".to_owned());
            };
            let Some(snapshot) = row.snapshot.as_ref() else {
                return Err("no snapshot yet".to_owned());
            };
            let queued = snapshot.queue.len();
            let settled =
                row.ingest_cursor >= end && row.phase == Phase::Idle as i32 && queued == 0;
            if !settled {
                return Err(format!(
                    "cursor {} of {end}, phase {}, {queued} queued",
                    row.ingest_cursor, row.phase
                ));
            }
            let items = items(net, agent).await;
            let Some(message) = items
                .iter()
                .filter(|item| !item.input_id.is_empty() && item.input_id != b"testnet-first")
                .filter(|item| item.text.contains(text))
                .map(|item| item.order)
                .max()
            else {
                return Ok(());
            };
            items
                .iter()
                .any(|item| item.key.starts_with("turn:") && item.order > message)
                .then_some(())
                .ok_or_else(|| format!("no turn ended after the message at order {message}"))
        },
    )
    .await
    .unwrap();
    net.provider_input(agent)
        .unwrap()
        .iter()
        .filter(|line| line.contains(text))
        .count()
}

async fn outbox(net: &Net, host: &str) -> usize {
    net.runtime(host)
        .unwrap()
        .store()
        .await
        .deliveries()
        .unwrap()
        .len()
}

async fn lifecycle(net: &Net, host: &str, agent: &str) -> Option<Lifecycle> {
    row(net, host, agent)
        .await
        .and_then(|row| Lifecycle::try_from(row.lifecycle).ok())
}

/// The key of the ask a headless Claude `agent` holds open, from the
/// snapshot its own host holds.
async fn open_ask(net: &Net, agent: &str) -> String {
    let at = net.agent(agent).unwrap().clone();
    let cut = net
        .runtime(&at.host)
        .unwrap()
        .store()
        .await
        .cut(&at.key(), 0)
        .unwrap();
    let snapshot = cut.snapshot.expect("a snapshot");
    let body = wire::ClaudeSdkSnapshot::decode(snapshot.body.as_slice()).unwrap();
    body.asks.first().expect("an open ask").key.clone()
}

fn fleet(events: &[InventoryEvent]) -> FleetState {
    let mut fleet = FleetState::new();
    for event in events {
        fleet.update(FleetMsg::Event(Box::new(event.clone())));
    }
    fleet
}

fn short(net: &Net, host_id: &[u8]) -> String {
    net.host_names()
        .into_iter()
        .find(|name| net.host(name).unwrap().host_id.as_bytes() == host_id)
        .unwrap_or_else(|| observe::short(host_id))
}

fn wire_error(status: &tonic::Status) -> wire::Error {
    wire::Error::decode(status.details()).expect("a wire error in the status")
}

fn three_hosts() -> Topology {
    Topology::new()
        .host("desk")
        .host("server")
        .host("phone")
        .link("desk", "server")
        .link("desk", "phone")
        .link("server", "phone")
}

/// A spawn names its host as the person does: resolved among this host
/// and the trusted ones, exactly and then ignoring case. No match is
/// NOT_FOUND naming the hosts there are; two are ambiguous, with both as
/// candidates. A match elsewhere forwards the create to that host with the
/// parent as host and id, and a directory there, or none, which starts the
/// child in its host's home directory; the parent's host and a third host
/// list the child under its own host in the parent's family.
#[tokio::test(flavor = "multi_thread")]
async fn a_spawn_by_host_name_resolves_among_trusted_hosts_and_is_forwarded() {
    let mut net = Net::start(
        Topology::new()
            .host("desk")
            .host("box")
            .host("BOX")
            .link("desk", "box")
            .link("desk", "BOX")
            .link("box", "BOX")
            .agent(lead()),
    )
    .await
    .unwrap();
    let tools = net.tools("lead").unwrap();
    let request = |host: &str| CreateAgentRequest {
        host: Some(wire::HostRef::named(host)),
        agent: wire::create_agent_request::Agent::defaults(Kind::ClaudeSdk),
        ..CreateAgentRequest::default()
    };

    let refused = tools
        .create_agent(Request::new(request("attic")))
        .await
        .unwrap_err();
    assert_eq!(refused.code(), Code::NotFound);
    let error = wire_error(&refused);
    for name in ["desk", "box", "BOX"] {
        assert!(error.message.contains(name), "{}", error.message);
    }
    println!("spawn on \"attic\": NOT_FOUND ({})", error.message);

    let refused = tools
        .create_agent(Request::new(request("Box")))
        .await
        .unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition);
    let error = wire_error(&refused);
    let detail = error
        .details
        .iter()
        .find(|detail| detail.r#type == "amux.v1.AmbiguousHostName")
        .expect("the candidates ride the error");
    let ambiguous = AmbiguousHostName::decode(detail.value.as_slice()).unwrap();
    let mut candidates: Vec<&str> = ambiguous
        .candidates
        .iter()
        .map(|host| host.name.as_str())
        .collect();
    candidates.sort_unstable();
    assert_eq!(candidates, ["BOX", "box"]);
    println!("spawn on \"Box\": ambiguous between {candidates:?}");

    let spawned = net
        .spawn_child("lead", child("helper", "box", says("Suite green.")))
        .await
        .unwrap();
    let box_id = net.host("box").unwrap().host_id;
    assert_eq!(spawned.host_id, box_id.as_bytes());
    let parent = net.agent("lead").unwrap().parent();
    assert_eq!(
        spawned.parent.as_ref(),
        Some(&parent),
        "parent is host and id"
    );
    let own = row(&net, "box", "helper")
        .await
        .expect("the child lives on box");
    assert_eq!(
        own.cwd,
        net.host("box").unwrap().work.to_string_lossy(),
        "a directory on the child's host"
    );
    println!(
        "spawn on \"box\": forwarded; helper lives on box with parent lead@desk in {}",
        own.cwd
    );

    // The fleet groups the family from replica rows of both hosts, on the
    // parent's host and on a host that is neither.
    let helper = net.agent("helper").unwrap().id;
    let lead_id = net.agent("lead").unwrap().id;
    for host in ["desk", "BOX"] {
        let mut inventory = net.observe_inventory(host).await.unwrap();
        let events = inventory
            .observe_until(
                |events| {
                    let fleet = fleet(events);
                    fleet
                        .find(helper.as_bytes())
                        .is_some_and(|child| fleet.parent(&ui_state::agent_key(child)).is_some())
                },
                PATIENCE,
            )
            .await
            .unwrap();
        let fleet = fleet(events);
        let lead = fleet.find(lead_id.as_bytes()).unwrap();
        let family: Vec<String> = fleet
            .family(&ui_state::agent_key(lead))
            .iter()
            .map(|agent| format!("{}@{}", agent.name.clone(), short(&net, &agent.host_id)))
            .collect();
        assert_eq!(family, ["lead@desk", "helper@box"]);
        println!("{host}'s fleet: family {family:?}");
    }

    // The spawn tool names no directory: the child starts where box's
    // agents work (helper's folder), not in the home directory.
    let homed = net
        .spawn_child("lead", child("wanderer", "box", says("Here.")).cwd(""))
        .await
        .unwrap();
    let home = std::env::var("HOME").unwrap();
    assert_eq!(homed.cwd, own.cwd);
    assert_eq!(row(&net, "box", "wanderer").await.unwrap().cwd, own.cwd);
    println!(
        "spawn on \"box\" naming no directory: wanderer starts in {}",
        own.cwd
    );

    // Naming the parent's own host, by any of its names, is a spawn here:
    // with no directory the child starts where its parent works.
    let lead_cwd = row(&net, "desk", "lead").await.unwrap().cwd;
    assert_ne!(
        lead_cwd, home,
        "the parent works outside the home directory"
    );
    let local = net
        .spawn_child(
            "lead",
            child("neighbour", "desk", says("Here too.")).cwd(""),
        )
        .await
        .unwrap();
    assert_eq!(local.host_id, net.host("desk").unwrap().host_id.as_bytes());
    assert_eq!(local.cwd, lead_cwd);
    let named = tools
        .create_agent(Request::new(request("DESK")))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(named.host_id, net.host("desk").unwrap().host_id.as_bytes());
    assert_eq!(named.cwd, lead_cwd);
    println!("spawn on \"desk\" or \"DESK\" naming no directory: the child starts in {lead_cwd}");

    net.shutdown().await.unwrap();
}

/// Stop is an interrupt only a parent may send its own child, and a send
/// to an exited child resumes it only from its parent; both hold with the
/// child on another host, checked where the caller is known and carried
/// out where the child lives.
#[tokio::test(flavor = "multi_thread")]
async fn stop_and_send_are_lineage_checked_across_hosts() {
    let mut net = Net::start(
        three_hosts()
            .agent(lead())
            .agent(AgentDecl::new("other", "desk").steps(says("idle"))),
    )
    .await
    .unwrap();
    net.spawn_child(
        "lead",
        child("helper", "server", vec![waits("never"), Step::TurnEnd]),
    )
    .await
    .unwrap();
    let helper = net.agent("helper").unwrap().clone();
    // Stop cancels a running turn. Until Claude takes input the creation
    // prompt only waits in the queue, and an interrupt then cancels
    // nothing; the prompt's turn would run and wait on its gate forever.
    until("desk to see helper's turn running", || {
        let net = &net;
        async move {
            let phase = row(net, "desk", "helper").await.map(|row| row.phase);
            (phase == Some(Phase::Working as i32))
                .then_some(())
                .ok_or_else(|| format!("phase {phase:?}"))
        }
    })
    .await
    .unwrap();

    let stop = |caller: &str| {
        let tools = net.tools(caller).unwrap();
        let request = wire::SendInputRequest {
            agent_id: helper.id.as_bytes().to_vec(),
            input: Some(interrupt()),
        };
        async move { tools.send_input(Request::new(request)).await }
    };
    let refused = stop("other").await.unwrap_err();
    assert_eq!(refused.code(), Code::PermissionDenied);
    println!("other stops helper: PERMISSION_DENIED (not its child)");
    let verdict = stop("lead").await.unwrap().into_inner();
    assert!(
        matches!(verdict.of, Some(wire::send_input_response::Of::Accepted(_))),
        "{verdict:?}"
    );
    println!("lead stops helper: accepted on server");
    until("the interrupted one-shot child to exit", || {
        let net = &net;
        async move {
            let lifecycle = lifecycle(net, "server", "helper").await;
            (lifecycle == Some(Lifecycle::Exited))
                .then_some(())
                .ok_or_else(|| format!("lifecycle {lifecycle:?}"))
        }
    })
    .await
    .unwrap();

    let to = helper.parent();
    let refused = net
        .tools("other")
        .unwrap()
        .send_message(Request::new(message(b"from-other", &to, "Still there?")))
        .await
        .unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition);
    assert!(
        refused.message().contains("has stopped"),
        "{}",
        refused.message()
    );
    println!("other sends to exited helper: {}", refused.message());

    net.next_start(
        "helper",
        Script {
            steps: says("Picked it back up."),
            ..Script::default()
        },
    )
    .unwrap();
    net.tools("lead")
        .unwrap()
        .send_message(Request::new(message(b"from-lead", &to, "Run it again.")))
        .await
        .expect("the parent's send resumes its child");
    let resumed = row(&net, "server", "helper").await.unwrap();
    assert_eq!(resumed.incarnation, 2);
    assert_eq!(with_input(&net, "helper", b"from-lead").await, 1);
    assert_eq!(with_input(&net, "helper", b"from-other").await, 0);
    println!("lead sends to exited helper: resumed as incarnation 2 with the message");
    net.shutdown().await.unwrap();
}

/// Deleting a parent whose family reaches a host that cannot be reached
/// fails at once, naming that host, and deletes nothing: it said "Deleted
/// lead" and left the far children running with no parent. That holds when
/// the parent's host still lists the far child, and when it no longer does
/// (the hosts unlinked), since the parent's host remembers the children it
/// started elsewhere. Once the host is back, the delete takes the whole
/// family through each child's own host.
#[tokio::test(flavor = "multi_thread")]
async fn a_delete_whose_family_reaches_an_unreachable_host_deletes_nothing() {
    let mut net = Net::start(
        Topology::new()
            .host("desk")
            .host("server")
            .host("laptop")
            .link("desk", "server")
            .link("desk", "laptop")
            .agent(lead()),
    )
    .await
    .unwrap();
    net.spawn_child(
        "lead",
        child("helper", "server", vec![waits("never"), Step::TurnEnd]),
    )
    .await
    .unwrap();
    net.spawn_child(
        "lead",
        child("scout", "laptop", vec![waits("never"), Step::TurnEnd]),
    )
    .await
    .unwrap();
    let helper = net.agent("helper").unwrap().clone();
    until("desk to hold both children", || {
        let net = &net;
        async move {
            let held = (
                row(net, "desk", "helper").await.is_some(),
                row(net, "desk", "scout").await.is_some(),
            );
            (held == (true, true))
                .then_some(())
                .ok_or_else(|| format!("helper, scout held: {held:?}"))
        }
    })
    .await
    .unwrap();

    let refused = |error: NetError| match error {
        NetError::Refused(status) => *status,
        other => panic!("the delete failed otherwise: {other}"),
    };
    // Severed: desk still lists scout.
    net.sever_link("desk", "laptop").unwrap();
    net.wait_link("desk", "laptop", false).await.unwrap();
    let status = refused(net.delete_family("lead").await.unwrap_err());
    println!("delete lead, laptop severed: {}", status.message());
    assert!(
        status.message().contains("laptop"),
        "the refusal does not name laptop: {}",
        status.message()
    );
    assert!(row(&net, "desk", "lead").await.is_some(), "lead is kept");
    assert!(
        net.runtime("server")
            .unwrap()
            .store()
            .await
            .agent(&helper.key())
            .unwrap()
            .is_some(),
        "helper is kept"
    );
    assert_eq!(
        lifecycle(&net, "laptop", "scout").await,
        Some(Lifecycle::Live),
        "scout runs on"
    );

    // Unlinked: desk no longer lists scout, and still knows of it.
    net.restore_link("desk", "laptop").await.unwrap();
    net.wait_link("desk", "laptop", true).await.unwrap();
    net.remove_link("desk", "laptop").await.unwrap();
    until("desk to forget scout", || {
        let net = &net;
        async move {
            row(net, "desk", "scout")
                .await
                .is_none()
                .then_some(())
                .ok_or_else(|| "desk still lists scout".to_owned())
        }
    })
    .await
    .unwrap();
    let status = refused(net.delete_family("lead").await.unwrap_err());
    println!("delete lead, laptop unlinked: {}", status.message());
    assert!(
        status.message().contains("laptop"),
        "the refusal does not name laptop: {}",
        status.message()
    );
    assert!(
        row(&net, "desk", "lead").await.is_some(),
        "lead is deleted with scout out of reach"
    );
    assert_eq!(
        lifecycle(&net, "laptop", "scout").await,
        Some(Lifecycle::Live),
        "scout runs on"
    );
    net.shutdown().await.unwrap();
}

/// A parent's host keeps knowing the far child it started after a stop: the
/// stop reached the child while its host's listing held it, and forgot it
/// was forwarded, so once the hosts unlinked a delete of the parent saw no
/// child, said it deleted the family, and left the child behind.
#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_parent_keeps_its_far_child_for_the_delete() {
    let mut net = Net::start(
        Topology::new()
            .host("desk")
            .host("laptop")
            .link("desk", "laptop")
            .agent(lead()),
    )
    .await
    .unwrap();
    net.spawn_child(
        "lead",
        child("scout", "laptop", vec![waits("never"), Step::TurnEnd]),
    )
    .await
    .unwrap();
    until("desk to hold scout", || {
        let net = &net;
        async move {
            row(net, "desk", "scout")
                .await
                .is_some()
                .then_some(())
                .ok_or_else(|| "desk does not list scout".to_owned())
        }
    })
    .await
    .unwrap();
    net.stop("lead").await.unwrap();
    until("scout to stop with its parent", || {
        let net = &net;
        async move {
            let at = lifecycle(net, "laptop", "scout").await;
            (at == Some(Lifecycle::Exited))
                .then_some(())
                .ok_or_else(|| format!("scout is {at:?}"))
        }
    })
    .await
    .unwrap();
    net.remove_link("desk", "laptop").await.unwrap();
    until("desk to forget scout", || {
        let net = &net;
        async move {
            row(net, "desk", "scout")
                .await
                .is_none()
                .then_some(())
                .ok_or_else(|| "desk still lists scout".to_owned())
        }
    })
    .await
    .unwrap();
    let status = match net.delete_family("lead").await {
        Err(NetError::Refused(status)) => *status,
        other => panic!("the delete with scout out of reach did {other:?}"),
    };
    println!("delete stopped lead, laptop unlinked: {}", status.message());
    assert!(
        status.message().contains("laptop"),
        "the refusal does not name laptop: {}",
        status.message()
    );
    assert!(row(&net, "desk", "lead").await.is_some(), "lead is kept");
    assert!(
        lifecycle(&net, "laptop", "scout").await.is_some(),
        "scout is kept"
    );
    net.shutdown().await.unwrap();
}

/// A child's finished message waits in its host's outbox while its parent's
/// host is away, and arrives once when it returns. A row for a parent
/// incarnation that has since ended is dropped instead; and the parent's
/// own daemon drops a message for any incarnation but the current one.
#[tokio::test(flavor = "multi_thread")]
async fn an_away_parents_rows_wait_and_a_stale_incarnation_is_dropped() {
    let mut net = Net::start(
        Topology::new()
            .host("desk")
            .host("server")
            .link("desk", "server")
            .agent(lead()),
    )
    .await
    .unwrap();
    net.spawn_child(
        "lead",
        child(
            "helper",
            "server",
            vec![waits("finish"), text("Suite green."), Step::TurnEnd],
        ),
    )
    .await
    .unwrap();
    net.sever_link("desk", "server").unwrap();
    net.wait_link("desk", "server", false).await.unwrap();
    net.open_gate("finish").unwrap();
    until("helper's finished row", || {
        let net = &net;
        async move {
            let rows = outbox(net, "server").await;
            (rows == 1)
                .then_some(())
                .ok_or_else(|| format!("{rows} outbox rows"))
        }
    })
    .await
    .unwrap();
    net.advance(Duration::from_secs(120)).unwrap();
    // A window: the retry runs on the policy clock and finds no parent
    // row to hand to; nothing the test controls marks that attempt.
    holds_for(
        "the row to wait for its parent",
        Duration::from_secs(1),
        || {
            let net = &net;
            async move { outbox(net, "server").await == 1 }
        },
    )
    .await
    .unwrap();
    assert!(received(&net, "lead").await.is_empty());
    println!("desk away: helper's finished message waits on server");
    net.restore_link("desk", "server").await.unwrap();
    until("the row to be delivered", || {
        let net = &net;
        async move {
            let rows = outbox(net, "server").await;
            (rows == 0)
                .then_some(())
                .ok_or_else(|| format!("{rows} outbox rows"))
        }
    })
    .await
    .unwrap();
    let got = received(&net, "lead").await;
    assert_eq!(got.len(), 1, "one finished message: {got:?}");
    println!("desk back: lead received {:?}", got[0].text);

    // A second child finishes while the parent's host is away, and the
    // parent is resumed there meanwhile: the row is for an incarnation
    // that is not waiting for anything.
    net.spawn_child(
        "lead",
        child(
            "second",
            "server",
            vec![waits("second"), text("Done too."), Step::TurnEnd],
        ),
    )
    .await
    .unwrap();
    net.sever_link("desk", "server").unwrap();
    net.wait_link("desk", "server", false).await.unwrap();
    net.open_gate("second").unwrap();
    until("second's finished row", || {
        let net = &net;
        async move {
            let rows = outbox(net, "server").await;
            (rows == 1)
                .then_some(())
                .ok_or_else(|| format!("{rows} outbox rows"))
        }
    })
    .await
    .unwrap();
    let lead = net.agent("lead").unwrap().id;
    net.runtime("desk").unwrap().stop(lead).await.unwrap();
    net.resume("lead", None).await.unwrap();
    net.restore_link("desk", "server").await.unwrap();
    until("the stale row to be dropped", || {
        let net = &net;
        async move {
            let rows = outbox(net, "server").await;
            (rows == 0)
                .then_some(())
                .ok_or_else(|| format!("{rows} outbox rows"))
        }
    })
    .await
    .unwrap();
    assert_eq!(
        received(&net, "lead").await.len(),
        1,
        "the resumed lead receives nothing"
    );
    println!("lead resumed while desk was away: second's row dropped as stale");

    // The parent's daemon makes the same check itself.
    let server = net.host("server").unwrap().host_id;
    let desk = net.host("desk").unwrap().host_id;
    let helper = net.agent("helper").unwrap().clone();
    let from = Sender {
        value: Some(sender::Value::Agent(AgentSender {
            agent_id: helper.id.as_bytes().to_vec(),
            host_id: server.as_bytes().to_vec(),
            name: "helper".into(),
        })),
    };
    let current = row(&net, "desk", "lead").await.unwrap().incarnation;
    let stale = Envelope {
        from: Some(from),
        kind: EnvelopeKind::Finished as i32,
        incarnation: Some(current - 1),
        ..envelope(b"stale", &net.agent("lead").unwrap().parent(), "late news")
    };
    let mut peer = net.edge("server").unwrap().peer(desk).await.unwrap();
    let refused = peer.send_message(stale).await.unwrap_err();
    assert_eq!(refused.code(), Code::NotFound);
    assert_eq!(with_input(&net, "lead", b"stale").await, 0);
    println!(
        "a message for lead's incarnation {} reaching desk: dropped ({})",
        current - 1,
        refused.message()
    );
    net.shutdown().await.unwrap();
}

/// The row's incarnation rides the wire. A parent resumed where the
/// child's host has not seen it yet still shows the incarnation the row
/// is for there, so the child's host sends it; the parent's host, which
/// knows better, answers NOT_FOUND, and the row is dropped unsent.
///
/// Stopping the parent stops the child too, mid-turn: its turn is
/// cancelled and it exits, or its deadline kills it first. Either way the
/// child's last word is a row for the parent's stopped incarnation,
/// Finished or Failed, and that row is the one refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_row_for_a_parent_resumed_out_of_sight_is_refused_where_the_parent_lives() {
    let mut net = Net::start(
        Topology::new()
            .host("desk")
            .host("server")
            .link("desk", "server")
            .agent(lead()),
    )
    .await
    .unwrap();
    net.spawn_child(
        "lead",
        child(
            "helper",
            "server",
            vec![waits("finish"), text("Suite green."), Step::TurnEnd],
        ),
    )
    .await
    .unwrap();
    until("server to hold lead's replica", || {
        let net = &net;
        async move { row(net, "server", "lead").await.map(|_| ()).ok_or("no row") }
    })
    .await
    .unwrap();

    // From here server's follower holds every change to lead it hears of.
    let lead = net.agent("lead").unwrap().id;
    let (release, released) = tokio::sync::watch::channel(false);
    let (heard, mut hearing) = tokio::sync::mpsc::unbounded_channel();
    net.runtime("server")
        .unwrap()
        .set_inventory_hook(Some(Arc::new(move |_, event: &InventoryEvent| {
            let about_lead = matches!(
                &event.of,
                Some(inventory_event::Of::Agent(agent)) if agent.agent_id == lead.as_bytes()
            );
            if !about_lead {
                return SourceVerdict::Keep;
            }
            let _ = heard.send(());
            let mut released = released.clone();
            SourceVerdict::Hold(Box::pin(async move {
                let _ = released.wait_for(|released| *released).await;
            }))
        })));
    net.runtime("desk").unwrap().stop(lead).await.unwrap();
    net.resume("lead", None).await.unwrap();
    hearing.recv().await.expect("server hears lead stop");
    let seen = row(&net, "server", "lead").await.unwrap();
    assert_eq!(
        (seen.incarnation, seen.lifecycle),
        (1, Lifecycle::Live as i32)
    );
    assert_eq!(row(&net, "desk", "lead").await.unwrap().incarnation, 2);
    println!("lead resumed on desk as incarnation 2; server still shows incarnation 1, live");

    net.open_gate("finish").unwrap();
    let helper = net.agent("helper").unwrap().key();
    until(
        "helper, stopped with lead, to have exited and been ingested",
        || {
            let net = &net;
            let helper = helper.clone();
            async move {
                let end = net
                    .journal_end("helper")
                    .map_err(|error| error.to_string())?;
                let store = net.runtime("server").unwrap();
                let store = store.store().await;
                let row = store.agent(&helper).unwrap().unwrap();
                (row.lifecycle == Lifecycle::Exited as i32 && row.ingest_cursor >= end)
                    .then_some(())
                    .ok_or_else(|| {
                        format!(
                            "lifecycle {:?}, cursor {} of {end}",
                            Lifecycle::try_from(row.lifecycle),
                            row.ingest_cursor
                        )
                    })
            }
        },
    )
    .await
    .unwrap();
    // The cascade may end helper while lead is between its stop and its
    // resume; desk then turns the row away as for a stopped parent and it
    // waits for the outbox's next pass. Server cannot hear the resume that
    // would wake it (its follower is held), so the pass is run here.
    until("the row to be settled", || {
        let net = &net;
        async move {
            net.runtime("server").unwrap().drain_deliveries().await;
            let rows = outbox(net, "server").await;
            (rows == 0)
                .then_some(())
                .ok_or_else(|| format!("{rows} outbox rows"))
        }
    })
    .await
    .unwrap();
    assert!(received(&net, "lead").await.is_empty());
    assert_eq!(taken(&net, "lead", "Suite green.").await, 0);
    println!(
        "helper stops with lead; server sends its row for incarnation 1; desk refuses it and it is dropped"
    );
    release.send_replace(true);
    net.shutdown().await.unwrap();
}

/// The same message sent twice at once, across hosts, reaches its
/// recipient once: the recipient's lane takes one at a time and the second
/// finds the first's item.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_duplicate_sends_across_hosts_yield_one_item() {
    let net = Net::start(
        three_hosts()
            .agent(lead())
            .agent(AgentDecl::new("reviewer", "server").steps(says("Reviewing."))),
    )
    .await
    .unwrap();
    until("desk to hold reviewer's replica", || {
        let net = &net;
        async move {
            row(net, "desk", "reviewer")
                .await
                .map(|_| ())
                .ok_or("no row")
        }
    })
    .await
    .unwrap();
    let to = net.agent("reviewer").unwrap().parent();
    let message = message(b"twice", &to, "Please review the survey.");
    let tools = net.tools("lead").unwrap();
    let (first, second) = tokio::join!(
        tools.send_message(Request::new(message.clone())),
        tools.send_message(Request::new(message)),
    );
    first.expect("the first send");
    second.expect("the duplicate is answered as sent");
    assert_eq!(with_input(&net, "reviewer", b"twice").await, 1);
    assert_eq!(
        taken(&net, "reviewer", "Please review the survey.").await,
        1,
        "reviewer's provider was sent the message once"
    );
    println!("two concurrent sends of one envelope to reviewer on server: one item, one turn");
    net.shutdown().await.unwrap();
}

/// The accepted story, end to end: lead on desk spawns helper on server
/// by name; helper's question is flagged on the family from the phone and
/// answered in helper's own chat; helper's one finished message reaches
/// lead once although the link drops while desk is taking it; lead's send
/// resumes the exited helper; deleting lead removes helper from server.
#[tokio::test(flavor = "multi_thread")]
async fn cross_host_family_journey() {
    let mut net = Net::start(three_hosts().agent(lead())).await.unwrap();
    let mut story = Vec::new();
    let mut say = |line: String| {
        println!("{line}");
        story.push(line);
    };

    // Spawn by name.
    let spawned = net
        .spawn_child(
            "lead",
            child(
                "helper",
                "server",
                vec![
                    question(),
                    text("Ran the unit suite: 214 passed."),
                    Step::TurnEnd,
                ],
            ),
        )
        .await
        .unwrap();
    let helper = net.agent("helper").unwrap().clone();
    let lead = net.agent("lead").unwrap().clone();
    let parent = spawned.parent.clone().unwrap();
    assert_eq!(parent, lead.parent());
    say(format!(
        "1. lead@desk spawns helper on \"server\": forwarded; helper@{}, parent lead@{}",
        short(&net, &spawned.host_id),
        short(&net, &parent.host_id)
    ));

    // The ask, flagged on the family from the phone.
    let mut phone = net.observe_inventory("phone").await.unwrap();
    let events = phone
        .observe_until(
            |events| {
                let fleet = fleet(events);
                let Some(lead) = fleet.find(lead.id.as_bytes()) else {
                    return false;
                };
                fleet.family_attention(&ui_state::agent_key(lead)) == Some(Attention::NeedsYou)
            },
            PATIENCE,
        )
        .await
        .unwrap();
    let seen = fleet(events);
    let child_card = seen.find(helper.id.as_bytes()).unwrap();
    assert_eq!(child_card.phase(), Phase::NeedsYou);
    assert_eq!(child_card.host_id, helper.host_id.as_bytes());
    let family: Vec<String> = seen
        .family(&ui_state::agent_key(seen.find(lead.id.as_bytes()).unwrap()))
        .iter()
        .map(|agent| {
            format!(
                "{}@{} {:?}",
                agent.name.clone(),
                short(&net, &agent.host_id),
                agent.phase()
            )
        })
        .collect();
    say(format!(
        "2. phone's fleet: family {family:?}, flagged {:?} on the family",
        Attention::NeedsYou
    ));

    // Answered in the child's own chat, from the phone. The lead is busy
    // when the finished message comes: its process is frozen.
    net.freeze("lead").unwrap();
    let ask = open_ask(&net, "helper").await;
    let answer = interpret::claude_sdk_input(
        Uuid::new_v4().as_bytes().to_vec(),
        &interpret::FixtureInput::Answer {
            ask,
            answer: serde_json::json!({ "selected": [0] }),
        },
    )
    .unwrap();
    let verdict = net
        .client("phone")
        .unwrap()
        .send_input(Request::new(wire::SendInputRequest {
            agent_id: helper.id.as_bytes().to_vec(),
            input: Some(answer),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(
        matches!(verdict.of, Some(wire::send_input_response::Of::Accepted(_))),
        "{verdict:?}"
    );
    say("3. phone answers \"unit\" in helper's chat: forwarded to server, accepted".into());

    // One finished message under a link fault.
    until("helper's finished row", || {
        let net = &net;
        async move {
            let rows = outbox(net, "server").await;
            (rows == 1)
                .then_some(())
                .ok_or_else(|| format!("{rows} outbox rows"))
        }
    })
    .await
    .unwrap();
    // Server's hand-off reaches desk, which holds it for the frozen lead's
    // verdict.
    let lead_id = net.agent("lead").unwrap().id;
    until("server's hand-off to reach desk", || {
        let desk = net.runtime("desk").unwrap();
        async move {
            desk.in_hand(lead_id)
                .then_some(())
                .ok_or("nothing in hand for lead at desk")
        }
    })
    .await
    .unwrap();
    // Both of server's links, or desk would reach it through the phone.
    net.sever_link("desk", "server").unwrap();
    net.sever_link("server", "phone").unwrap();
    net.wait_link("desk", "server", false).await.unwrap();
    net.thaw("lead").unwrap();
    until("lead to take the first hand-off", || {
        let net = &net;
        async move {
            let got = received(net, "lead").await;
            (got.len() == 1)
                .then_some(())
                .ok_or_else(|| format!("lead received {got:?}"))
        }
    })
    .await
    .unwrap();
    assert_eq!(
        outbox(&net, "server").await,
        1,
        "server never heard the answer"
    );
    say("4. helper finishes; server hands the message to desk; server's links drop before desk answers; lead has it, server still holds the row".into());
    net.restore_link("desk", "server").await.unwrap();
    net.restore_link("server", "phone").await.unwrap();
    until("server's retry to settle the row", || {
        let net = &net;
        async move {
            let rows = outbox(net, "server").await;
            (rows == 0)
                .then_some(())
                .ok_or_else(|| format!("{rows} outbox rows"))
        }
    })
    .await
    .unwrap();
    let got = received(&net, "lead").await;
    assert_eq!(got.len(), 1, "one finished message: {got:?}");
    assert_eq!(
        taken(&net, "lead", "Ran the unit suite: 214 passed.").await,
        1,
        "desk found the envelope id already accepted and relayed nothing"
    );
    say(format!(
        "5. link back: server retries, desk finds the envelope id already accepted; lead holds one finished message: {:?}",
        got[0].text
    ));

    // Resume by send.
    until("the one-shot helper to exit", || {
        let net = &net;
        async move {
            let lifecycle = lifecycle(net, "server", "helper").await;
            (lifecycle == Some(Lifecycle::Exited))
                .then_some(())
                .ok_or_else(|| format!("lifecycle {lifecycle:?}"))
        }
    })
    .await
    .unwrap();
    net.next_start(
        "helper",
        Script {
            steps: says("Added the flaky test to the report."),
            ..Script::default()
        },
    )
    .unwrap();
    net.tools("lead")
        .unwrap()
        .send_message(Request::new(message(
            b"follow-up",
            &helper.parent(),
            "Add the flaky test to the report.",
        )))
        .await
        .expect("lead's send resumes helper");
    assert_eq!(row(&net, "server", "helper").await.unwrap().incarnation, 2);
    until("helper's second finished message", || {
        let net = &net;
        async move {
            let got = received(net, "lead").await;
            (got.len() == 2)
                .then_some(())
                .ok_or_else(|| format!("lead received {got:?}"))
        }
    })
    .await
    .unwrap();
    say("6. helper exits; lead sends a follow-up: server resumes helper (incarnation 2) with it; lead hears finished again".into());

    // Cascade delete.
    let answer = net.delete_family("lead").await.unwrap();
    assert_eq!(answer.removed_children.len(), 1);
    assert!(
        net.runtime("server")
            .unwrap()
            .store()
            .await
            .agent(&AgentKey::new(
                helper.host_id.as_bytes().to_vec(),
                helper.id.as_bytes().to_vec()
            ))
            .unwrap()
            .is_none()
    );
    phone
        .observe_until(
            |events| {
                let fleet = fleet(events);
                fleet.find(lead.id.as_bytes()).is_none()
                    && fleet.find(helper.id.as_bytes()).is_none()
            },
            PATIENCE,
        )
        .await
        .unwrap();
    say("7. delete lead on desk: helper deleted on server; the phone's fleet lists neither".into());
    assert_eq!(story.len(), 7);
    net.shutdown().await.unwrap();
}

/// A headless Claude parent is told who a report comes from and what it
/// is, as a Codex parent is (`[agent <name> finished its turn]`). The
/// envelope carries the sender and the kind; the line Claude reads on its
/// stdin must carry them too, or the model takes the child's last words
/// for its person's and cannot tell its child has finished.
#[tokio::test(flavor = "multi_thread")]
async fn a_headless_claude_parent_reads_which_child_finished() {
    let mut net = Net::start(
        Topology::new()
            .host("desk")
            .host("server")
            .link("desk", "server")
            .agent(lead()),
    )
    .await
    .unwrap();
    net.settle("lead").await.unwrap();
    net.spawn_child("lead", child("helper", "server", says("Suite green.")))
        .await
        .unwrap();
    until("helper's finished message to reach lead", || {
        let net = &net;
        async move {
            let got = received(net, "lead").await;
            (got.len() == 1)
                .then_some(())
                .ok_or_else(|| format!("lead received {got:?}"))
        }
    })
    .await
    .unwrap();
    assert_eq!(taken(&net, "lead", "Suite green.").await, 1);
    let lines: Vec<String> = net
        .provider_input("lead")
        .unwrap()
        .into_iter()
        .filter(|line| line.contains("Suite green."))
        .collect();
    println!("what lead's Claude read: {lines:?}");
    assert!(
        lines
            .iter()
            .any(|line| line.contains("helper") && line.contains("finished")),
        "the report reached Claude without its sender or kind: {lines:?}"
    );
    net.shutdown().await.unwrap();
}

/// Stopping a parent stops its whole family, as deleting it does: every
/// descendant, on its own host or another, is asked to stop, and a child
/// on a host the stop cannot reach is left as it is. Each child here is
/// mid-turn when its parent stops; the stop cancels that turn and ends the
/// incarnation as stopped, where a child left running would end it as
/// finished by the one-shot rule.
#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_parent_stops_every_descendant_on_every_host() {
    let mut net = Net::start(three_hosts().agent(lead())).await.unwrap();
    net.settle("lead").await.unwrap();
    let busy = || vec![waits("release"), text("Survey part done."), Step::TurnEnd];
    net.spawn_child("lead", child("notes", "desk", busy()))
        .await
        .unwrap();
    net.spawn_child("lead", child("helper", "server", busy()))
        .await
        .unwrap();
    net.spawn_child("helper", child("scout", "phone", busy()))
        .await
        .unwrap();
    let family = [("notes", "desk"), ("helper", "server"), ("scout", "phone")];
    until("every descendant to be live on its host", || {
        let net = &net;
        async move {
            let mut held = Vec::new();
            for (name, host) in family {
                held.push((name, lifecycle(net, host, name).await));
            }
            held.iter()
                .all(|(_, lifecycle)| *lifecycle == Some(Lifecycle::Live))
                .then_some(())
                .ok_or_else(|| format!("{held:?}"))
        }
    })
    .await
    .unwrap();

    net.stop("lead").await.unwrap();
    until("lead to exit", || {
        let net = &net;
        async move {
            let now = lifecycle(net, "desk", "lead").await;
            (now == Some(Lifecycle::Exited))
                .then_some(())
                .ok_or_else(|| format!("lead {now:?}"))
        }
    })
    .await
    .unwrap();
    println!("lead stopped; releasing its descendants' turns");

    net.open_gate("release").unwrap();
    let mut causes = Vec::new();
    for (name, host) in family {
        until(&format!("{name} to exit on {host}"), || {
            let net = &net;
            async move {
                let now = lifecycle(net, host, name).await;
                (now == Some(Lifecycle::Exited))
                    .then_some(())
                    .ok_or_else(|| format!("{name} {now:?}"))
            }
        })
        .await
        .unwrap();
        let row = row(&net, host, name).await.unwrap();
        causes.push((name, wire::ExitCause::try_from(row.exit_cause).ok()));
    }
    println!("how each descendant ended: {causes:?}");
    assert!(
        causes
            .iter()
            .all(|(_, cause)| *cause == Some(wire::ExitCause::Stopped)),
        "stopping lead stopped none of its descendants; they ran on to the end of their turns: {causes:?}"
    );
    net.shutdown().await.unwrap();
}

fn tool(name: &str, input: serde_json::Value, wait_for: Option<&str>) -> Step {
    Step::Tool(provider_fakes::script::Tool {
        name: Some(name.to_owned()),
        class: provider_fakes::script::ToolClass::Consequential,
        input: Some(input),
        outcome: Default::default(),
        wait_for: wait_for.map(Into::into),
    })
}

async fn until_exited(net: &Net, host: &str, agent: &str) {
    until(&format!("{agent} to exit on {host}"), || async move {
        let now = lifecycle(net, host, agent).await;
        (now == Some(Lifecycle::Exited))
            .then_some(())
            .ok_or_else(|| format!("{agent} {now:?}"))
    })
    .await
    .unwrap();
}

async fn until_live(net: &Net, host: &str, agent: &str) {
    until(&format!("{agent} to be live on {host}"), || async move {
        let now = lifecycle(net, host, agent).await;
        (now == Some(Lifecycle::Live))
            .then_some(())
            .ok_or_else(|| format!("{agent} {now:?}"))
    })
    .await
    .unwrap();
}

/// Names are unique on a host, not across hosts: the desk and the server
/// may each run a worker. A bare name that matches agents on two hosts is
/// picked with `name@host`, and the refusal for the bare name offers
/// exactly those forms. QA: the refusal offered ids, which every command
/// and tool then refused, so a lead could reach neither of its workers.
#[tokio::test(flavor = "multi_thread")]
async fn a_name_on_two_hosts_resolves_as_name_at_host_and_the_refusal_offers_those_forms() {
    let net = Net::start(
        Topology::new()
            .host("desk")
            .host("server")
            .link("desk", "server")
            .agent(
                AgentDecl::new("worker", "desk")
                    .prompt("Wait.")
                    .steps(says("Ready.")),
            )
            .agent(
                AgentDecl::new("other", "server")
                    .prompt("Wait.")
                    .steps(says("Ready.")),
            ),
    )
    .await
    .unwrap();
    net.settle("worker").await.unwrap();
    net.settle("other").await.unwrap();
    let other = net.agent("other").unwrap().id;
    net.runtime("server")
        .unwrap()
        .rename(other, "worker")
        .await
        .unwrap();
    until("the desk to hold the server's agent as worker", || {
        let net = &net;
        async move {
            let name = row(net, "desk", "other").await.map(|row| row.name);
            (name.as_deref() == Some("worker"))
                .then_some(())
                .ok_or_else(|| format!("{name:?}"))
        }
    })
    .await
    .unwrap();

    let desk = net.runtime("desk").unwrap();
    let picked = desk.resolve_agent("worker@server").await;
    let bare = desk.resolve_agent("worker").await;
    drop(desk);
    net.shutdown().await.unwrap();
    println!("worker@server: {picked:?}");
    println!("worker: {bare:?}");
    let refusal = bare
        .expect_err("worker names an agent on each host")
        .to_string();
    assert!(
        refusal.contains("worker@desk") && refusal.contains("worker@server"),
        "the refusal does not offer the name@host forms that pick each agent: {refusal}"
    );
    let picked = picked.unwrap_or_else(|error| panic!("worker@server picks nothing: {error:?}"));
    assert_eq!(picked.agent_id, other.as_bytes().to_vec());
}

/// A one-shot sub-agent exits only once its turn has ended, its queue is
/// empty and no child it spawned is still running. Here the middle agent
/// ends its first turn while its scout still works; it stays, the scout's
/// report starts its next turn, and it reports up to the lead. QA: the
/// middle agent exited at once, the scout's report had nowhere to go, and
/// the lead waited forever.
#[tokio::test(flavor = "multi_thread")]
async fn a_sub_agent_waits_for_its_running_child_and_reports_its_result_up() {
    let mut net = Net::start(three_hosts().agent(lead())).await.unwrap();
    net.settle("lead").await.unwrap();
    net.spawn_child(
        "lead",
        child(
            "mid",
            "server",
            vec![
                waits("mid-go"),
                text("Started the scout."),
                Step::TurnEnd,
                text("The scout reported; all done."),
                Step::TurnEnd,
            ],
        ),
    )
    .await
    .unwrap();
    net.spawn_child(
        "mid",
        child(
            "scout",
            "phone",
            vec![waits("scout-go"), text("Scout done."), Step::TurnEnd],
        ),
    )
    .await
    .unwrap();
    until_live(&net, "phone", "scout").await;
    net.open_gate("mid-go").unwrap();
    net.turn_ended("mid", 1).await.unwrap();
    net.open_gate("scout-go").unwrap();
    let reached = until("the lead to hear mid report the scout's result", || {
        let net = &net;
        async move {
            let got = received(net, "lead").await;
            got.iter()
                .any(|item| item.text.contains("all done"))
                .then_some(())
                .ok_or_else(|| {
                    format!(
                        "lead received {:?}",
                        got.iter().map(|item| &item.text).collect::<Vec<_>>()
                    )
                })
        }
    })
    .await;
    let mid = row(&net, "server", "mid").await.unwrap();
    println!(
        "mid ended {:?}: {:?}",
        Lifecycle::try_from(mid.lifecycle),
        wire::ExitCause::try_from(mid.exit_cause)
    );
    net.shutdown().await.unwrap();
    reached.expect(
        "mid exited while its scout still ran, and the scout's result never reached the lead",
    );
}

/// A one-shot sub-agent waits for its child's last report, not its first.
/// Its child reports mid-way (it started a scout of its own) and again when
/// the scout's result is in; the sub-agent takes the mid-way report as a
/// turn of its own and stays, and reports up only once the child's last
/// report has come. Every hop crosses hosts.
#[tokio::test(flavor = "multi_thread")]
async fn a_sub_agent_waits_for_its_childs_last_report_not_its_mid_way_one() {
    let mut net = Net::start(three_hosts().agent(lead())).await.unwrap();
    net.settle("lead").await.unwrap();
    net.spawn_child(
        "lead",
        child(
            "planner",
            "server",
            vec![
                waits("planner-go"),
                text("Started mid."),
                Step::TurnEnd,
                text("Mid is still on it."),
                Step::TurnEnd,
                text("Mid reported the scout's result; planner done."),
                Step::TurnEnd,
            ],
        ),
    )
    .await
    .unwrap();
    net.spawn_child(
        "planner",
        child(
            "mid",
            "phone",
            vec![
                waits("mid-go"),
                text("Started the scout."),
                Step::TurnEnd,
                text("The scout reported; mid done."),
                Step::TurnEnd,
            ],
        ),
    )
    .await
    .unwrap();
    net.spawn_child(
        "mid",
        child(
            "scout",
            "desk",
            vec![waits("scout-go"), text("Scout done."), Step::TurnEnd],
        ),
    )
    .await
    .unwrap();
    until_live(&net, "desk", "scout").await;
    net.open_gate("planner-go").unwrap();
    net.turn_ended("planner", 1).await.unwrap();
    net.open_gate("mid-go").unwrap();
    net.turn_ended("planner", 2).await.unwrap();
    let planner = row(&net, "server", "planner").await.unwrap();
    assert_eq!(
        Lifecycle::try_from(planner.lifecycle),
        Ok(Lifecycle::Live),
        "the planner left on mid's mid-way report"
    );
    net.open_gate("scout-go").unwrap();
    let reached = until("the lead to hear the planner report mid's result", || {
        let net = &net;
        async move {
            let got = received(net, "lead").await;
            got.iter()
                .any(|item| item.text.contains("planner done"))
                .then_some(())
                .ok_or_else(|| {
                    format!(
                        "lead received {:?}",
                        got.iter().map(|item| &item.text).collect::<Vec<_>>()
                    )
                })
        }
    })
    .await;
    net.shutdown().await.unwrap();
    reached.expect("the planner never reported mid's last result to the lead");
}

/// Every child incarnation ends with a last report. A sub-agent stopped
/// while it waits idle for its own child has no turn to cut short, yet its
/// parent still hears that it stopped: a parent waiting on it would
/// otherwise wait for good.
#[tokio::test(flavor = "multi_thread")]
async fn a_sub_agent_stopped_while_it_waits_tells_its_parent_it_stopped() {
    let mut net = Net::start(three_hosts().agent(lead())).await.unwrap();
    net.settle("lead").await.unwrap();
    net.spawn_child(
        "lead",
        child(
            "mid",
            "server",
            vec![waits("mid-go"), text("Started the scout."), Step::TurnEnd],
        ),
    )
    .await
    .unwrap();
    net.spawn_child(
        "mid",
        child(
            "scout",
            "phone",
            vec![waits("never"), text("Scout done."), Step::TurnEnd],
        ),
    )
    .await
    .unwrap();
    until_live(&net, "phone", "scout").await;
    net.open_gate("mid-go").unwrap();
    net.turn_ended("mid", 1).await.unwrap();
    net.stop("mid").await.unwrap();
    let reached = until("the lead to hear that mid stopped", || {
        let net = &net;
        async move {
            let got = received(net, "lead").await;
            let texts: Vec<&String> = got.iter().map(|item| &item.text).collect();
            (texts.len() >= 2 && texts.iter().any(|text| text.contains("stopped")))
                .then_some(())
                .ok_or_else(|| format!("lead received {texts:?}"))
        }
    })
    .await;
    net.shutdown().await.unwrap();
    reached.expect("the lead never heard that mid stopped");
}

/// A child's report says how its turn ended. A child deleted in the
/// middle of its turn tells its parent it was stopped before it finished,
/// never that it finished. QA: deleted and denied children reported
/// "finished" with no words, and parents told the person the work was
/// done.
#[tokio::test(flavor = "multi_thread")]
async fn a_child_deleted_mid_turn_tells_its_parent_it_was_stopped() {
    let mut net = Net::start(
        Topology::new()
            .host("desk")
            .host("server")
            .link("desk", "server")
            .agent(lead()),
    )
    .await
    .unwrap();
    net.settle("lead").await.unwrap();
    net.spawn_child(
        "lead",
        child(
            "helper",
            "server",
            vec![waits("never"), text("Suite green."), Step::TurnEnd],
        ),
    )
    .await
    .unwrap();
    until("helper to be working", || {
        let net = &net;
        async move {
            let phase = row(net, "server", "helper").await.map(|row| row.phase);
            (phase == Some(Phase::Working as i32))
                .then_some(())
                .ok_or_else(|| format!("{phase:?}"))
        }
    })
    .await
    .unwrap();
    net.delete("helper").await.unwrap();
    until("helper's report to reach lead", || {
        let net = &net;
        async move {
            let got = received(net, "lead").await;
            (!got.is_empty())
                .then_some(())
                .ok_or_else(|| "nothing yet".to_owned())
        }
    })
    .await
    .unwrap();
    taken(&net, "lead", "").await;
    let lines: Vec<String> = net
        .provider_input("lead")
        .unwrap()
        .into_iter()
        .filter(|line| line.contains("helper"))
        .collect();
    net.shutdown().await.unwrap();
    println!("what lead's Claude read of helper: {lines:?}");
    assert!(
        lines.iter().any(|line| line.contains("stopped")),
        "the deleted child's report does not say it was stopped: {lines:?}"
    );
}

/// The fleet's agent row counts what a stop would cut: live descendants
/// on every host the agent's host knows, and background jobs. Both
/// clients' stop and delete confirms name them ("lead is working and has
/// 1 background job and 2 sub-agents. Stop anyway?"); the phone, which
/// sees only its paired host's agents, cannot count them itself. QA: a
/// delete removed a child on another host and nothing said so.
#[tokio::test(flavor = "multi_thread")]
async fn the_agent_row_counts_live_descendants_and_background_jobs() {
    let mut net = Net::start(
        three_hosts().agent(
            AgentDecl::new("lead", "desk")
                .prompt("Plan the release survey.")
                .steps(vec![
                    tool(
                        "Bash",
                        serde_json::json!({"command": "make survey", "run_in_background": true}),
                        Some("build"),
                    ),
                    text("A build runs in the background."),
                    Step::TurnEnd,
                ]),
        ),
    )
    .await
    .unwrap();
    net.settle("lead").await.unwrap();
    let busy = || vec![waits("release"), text("Done."), Step::TurnEnd];
    net.spawn_child("lead", child("notes", "desk", busy()))
        .await
        .unwrap();
    net.spawn_child("lead", child("helper", "server", busy()))
        .await
        .unwrap();
    until_live(&net, "desk", "notes").await;
    until_live(&net, "server", "helper").await;
    until("lead's background job to be listed", || {
        let net = &net;
        async move {
            let at = net.agent("lead").unwrap().clone();
            let cut = net
                .runtime("desk")
                .unwrap()
                .store()
                .await
                .cut(&at.key(), 0)
                .unwrap();
            let jobs = cut
                .snapshot
                .map(|snapshot| {
                    wire::ClaudeSdkSnapshot::decode(snapshot.body.as_slice())
                        .unwrap()
                        .background_jobs
                        .map(|jobs| jobs.jobs.len())
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            (jobs == 1)
                .then_some(())
                .ok_or_else(|| format!("{jobs} jobs"))
        }
    })
    .await
    .unwrap();
    let lead = net
        .runtime("desk")
        .unwrap()
        .resolve_agent("lead")
        .await
        .unwrap();
    net.shutdown().await.unwrap();
    let shown = format!("{lead:?}");
    println!("lead's row: {shown}");
    assert!(
        shown.contains("descendants: 2"),
        "lead's row does not count its two live sub-agents: {shown}"
    );
    assert!(
        shown.contains("background_jobs: 1"),
        "lead's row does not count its background job: {shown}"
    );
}

/// An agents' `stop` tool stops: the child ends as stopped, as a person's
/// stop ends it, and the stop cascades to the child's own descendants on
/// every host. QA and the decision: the tool interrupted the child's turn,
/// which left its own children running.
#[tokio::test(flavor = "multi_thread")]
async fn an_agents_stop_tool_stops_its_child_and_the_childs_descendants() {
    let mut net = Net::start(
        three_hosts().agent(
            AgentDecl::new("lead", "desk")
                .prompt("Plan the release survey.")
                .steps(vec![
                    text("I will have a helper run the suite."),
                    Step::TurnEnd,
                    tool(
                        "mcp__amux__stop",
                        serde_json::json!({"name": "helper"}),
                        None,
                    ),
                    text("Stopped the helper."),
                    Step::TurnEnd,
                ]),
        ),
    )
    .await
    .unwrap();
    net.settle("lead").await.unwrap();
    let busy = || vec![waits("release"), text("Done."), Step::TurnEnd];
    net.spawn_child("lead", child("helper", "server", busy()))
        .await
        .unwrap();
    net.spawn_child("helper", child("scout", "phone", busy()))
        .await
        .unwrap();
    until_live(&net, "server", "helper").await;
    until_live(&net, "phone", "scout").await;
    net.send("lead", "Stop the helper.").await.unwrap();
    net.turn_ended("lead", 2).await.unwrap();
    until_exited(&net, "server", "helper").await;
    // The cascade starts each descendant's stop without waiting for it.
    until_exited(&net, "phone", "scout").await;
    let helper = row(&net, "server", "helper").await.unwrap();
    let scout = row(&net, "phone", "scout").await.unwrap();
    net.shutdown().await.unwrap();
    let ended = [
        (
            "helper",
            Lifecycle::try_from(helper.lifecycle).ok(),
            wire::ExitCause::try_from(helper.exit_cause).ok(),
        ),
        (
            "scout",
            Lifecycle::try_from(scout.lifecycle).ok(),
            wire::ExitCause::try_from(scout.exit_cause).ok(),
        ),
    ];
    println!("after lead's stop tool: {ended:?}");
    assert!(
        ended.iter().all(
            |(_, lifecycle, cause)| *lifecycle == Some(Lifecycle::Exited)
                && *cause == Some(wire::ExitCause::Stopped)
        ),
        "the stop tool did not stop the child and its descendants: {ended:?}"
    );
}

/// A host that runs no agents (the phone announces no kinds) is never
/// asked to start one: a spawn naming it is refused in plain words.
/// QA: the terminal offered the phone as a host and starting there failed
/// with a socket-path error from the phone's sandbox.
#[tokio::test(flavor = "multi_thread")]
async fn a_spawn_on_a_host_that_runs_no_agents_is_refused_in_plain_words() {
    let mut net = Net::start_with(
        three_hosts().agent(lead()),
        testnet::NetOptions {
            edge: Some(Arc::new(|host, edge| {
                if host == "phone" {
                    edge.kinds = Vec::new();
                }
            })),
            ..testnet::NetOptions::default()
        },
    )
    .await
    .unwrap();
    net.settle("lead").await.unwrap();
    let spawned = net
        .spawn_child("lead", child("helper", "phone", says("Ready.")))
        .await;
    net.shutdown().await.unwrap();
    println!("a spawn on the phone: {spawned:?}");
    let refusal = spawned
        .expect_err("the phone runs no agents, so a spawn there is refused")
        .to_string();
    assert!(
        refusal.contains("phone") && refusal.contains("run"),
        "the refusal does not say the phone runs no agents: {refusal}"
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A file one agent passes to another arrives as a file, wherever the
/// recipient runs: the send tool moves the attachment out of the text,
/// the recipient's host fetches its bytes from the sender's host, and the
/// recipient reads it from a path on its own host, exactly as a person's
/// attachment. QA: the element travelled as text naming the sender host's
/// path, which an agent on another machine cannot open.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_one_agent_sends_another_arrives_on_the_recipients_own_host() {
    let mut net = Net::start(
        Topology::new()
            .host("desk")
            .host("server")
            .link("desk", "server")
            .agent(
                AgentDecl::new("scribe", "desk")
                    .prompt("Wait.")
                    .steps(says("Ready.")),
            ),
    )
    .await
    .unwrap();
    net.settle("scribe").await.unwrap();
    let bytes = b"release notes: ship on friday\n".to_vec();
    let put = |agent: Uuid| wire::PutBlobRequest {
        agent_id: agent.as_bytes().to_vec(),
        name: "notes.txt".into(),
        mime: "text/plain".into(),
        bytes: bytes.clone(),
    };
    let blob = net
        .client("desk")
        .unwrap()
        .put_blob(Request::new(put(net.agent("scribe").unwrap().id)))
        .await
        .unwrap()
        .into_inner();
    let hash = hex(&blob.hash);
    let sender_path = net.agent_dir("scribe").unwrap().join("blobs").join(&hash);
    let element = format!(
        "<amux-attachment kind=\"file\" hash=\"sha256:{hash}\" name=\"notes.txt\" mime=\"text/plain\" size=\"{}\" path=\"{}\"/>",
        bytes.len(),
        sender_path.display()
    );
    net.spawn(
        AgentDecl::new("lead", "desk")
            .prompt("Plan the release survey.")
            .steps(vec![
                text("I will have a helper read the notes."),
                Step::TurnEnd,
                tool(
                    "mcp__amux__send",
                    serde_json::json!({"to": "helper", "text": format!("Please read {element}")}),
                    None,
                ),
                text("Sent the notes."),
                Step::TurnEnd,
            ]),
    )
    .await
    .unwrap();
    net.settle("lead").await.unwrap();
    net.client("desk")
        .unwrap()
        .put_blob(Request::new(put(net.agent("lead").unwrap().id)))
        .await
        .unwrap();
    net.spawn_child(
        "lead",
        child(
            "helper",
            "server",
            vec![waits("hold"), text("Read them."), Step::TurnEnd],
        ),
    )
    .await
    .unwrap();
    until_live(&net, "server", "helper").await;
    net.send("lead", "Pass the notes on.").await.unwrap();
    net.turn_ended("lead", 2).await.unwrap();
    let helper_dir = net.agent_dir("helper").unwrap();
    let delivered = until("helper's provider to read the notes", || {
        let net = &net;
        async move {
            let lines: Vec<String> = net
                .provider_input("helper")
                .unwrap()
                .into_iter()
                .filter(|line| line.contains("notes.txt"))
                .collect();
            (!lines.is_empty())
                .then_some(lines)
                .ok_or_else(|| "nothing yet".to_owned())
        }
    })
    .await
    .unwrap();
    let local = helper_dir.join("blobs").join(&hash);
    let held = local.exists();
    net.shutdown().await.unwrap();
    println!("what helper's Claude read: {delivered:?}; its copy held: {held}");
    assert!(
        delivered
            .iter()
            .any(|line| line.contains(&*local.to_string_lossy())),
        "the file reached the other host's agent naming the sender host's path, not one on its own host: {delivered:?}"
    );
    assert!(held, "the recipient's host holds no copy of the file");
}

/// A child an agent starts on another host without naming a folder starts
/// where that host's agents work, as a person's new agent there would be
/// offered, not in the home folder of the user its daemon runs as: a Codex
/// child given shell commands had the whole home folder around it.
#[tokio::test(flavor = "multi_thread")]
async fn a_far_child_with_no_folder_starts_where_its_hosts_agents_work() {
    let mut net = Net::start(
        Topology::new()
            .host("desk")
            .host("laptop")
            .link("desk", "laptop")
            .agent(lead())
            .agent(
                AgentDecl::new("local", "laptop")
                    .prompt("Tidy up.")
                    .steps(says("Tidied.")),
            ),
    )
    .await
    .unwrap();
    net.settle("local").await.unwrap();
    let work = row(&net, "laptop", "local").await.unwrap().cwd;
    let kid = net
        .spawn_child(
            "lead",
            child("kid", "laptop", vec![waits("never"), Step::TurnEnd]).cwd(""),
        )
        .await
        .unwrap();
    println!("kid starts in {}", kid.cwd);
    assert_eq!(kid.cwd, work, "the far child starts in another folder");
    net.shutdown().await.unwrap();
}
