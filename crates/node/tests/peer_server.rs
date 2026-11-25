//! What a paired daemon may ask of this host for itself, on PeerService
//! over a real link: this host's own entry and agents, its own agents'
//! conversations and files, and a child's report to its parent here.
//! Nothing it holds of a third host is answered, a call carried for a
//! person or an agent is refused, and the local services are not on a link
//! at all.

use std::sync::Arc;

use node::{Daemon, Edge, Owner, ProfileRuntime};
use prost::Message as _;
use support::synthetic::*;
use support::*;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;
use tonic::{Code, Request};
use wire::{
    DeliverReportRequest, FetchBlobRequest, FetchItemsRequest, FollowAgentsRequest,
    FollowSessionRequest, InventoryEvent, Phase, PutBlobRequest, follow_session_request,
    inventory_event, session_event,
};

use crate::support;

const SEGMENTS: u64 = 1 << 20;

/// A daemon for `install` serving its sockets, as a running daemon does:
/// the front door and each profile's socket, where a person's clients call.
pub(crate) async fn serving(install: &Install, options: node::StartOptions) -> Daemon {
    let options = node::StartOptions {
        front_door: Some(install.path("door.sock")),
        ..options
    };
    node::start(options, None).await.expect("the daemon starts")
}

/// A desk running `coder` and a laptop running `notes`, paired and linked,
/// with the laptop holding a replica of `coder` and its rows.
pub(crate) struct Pair {
    pub(crate) desk: Install,
    pub(crate) laptop: Install,
    desk_daemon: Daemon,
    laptop_daemon: Daemon,
    pub(crate) desk_runtime: Arc<ProfileRuntime>,
    pub(crate) laptop_runtime: Arc<ProfileRuntime>,
    pub(crate) desk_edge: Arc<Edge>,
    laptop_edge: Arc<Edge>,
    pub(crate) coder: SyntheticAgent,
    pub(crate) notes: SyntheticAgent,
    _link: node::LoopbackLink,
}

impl Pair {
    pub(crate) async fn new() -> Self {
        let desk = Install::new();
        let mut coder = SyntheticAgent::new(&desk, "coder", SEGMENTS);
        coder.register(&desk);
        coder.append(&item("c1", "the coder's first item"));
        coder.append(&snapshot(Phase::Idle, &[], 1_000));
        coder.go_live();
        let laptop = Install::new();
        let mut notes = SyntheticAgent::new(&laptop, "notes", SEGMENTS);
        notes.register(&laptop);
        notes.append(&item("n1", "the notes' first item"));
        notes.append(&snapshot(Phase::Idle, &[], 1_000));
        notes.go_live();

        let desk_daemon = serving(&desk, desk.options("boot-1", quiet_launch())).await;
        let desk_runtime = runtime(&desk_daemon, &desk);
        let laptop_daemon = serving(&laptop, laptop.options("boot-1", quiet_launch())).await;
        let laptop_runtime = runtime(&laptop_daemon, &laptop);
        let (desk_edge, laptop_edge) =
            (desk_runtime.edge().unwrap(), laptop_runtime.edge().unwrap());
        desk_edge.trust(&laptop_edge).await.unwrap();
        laptop_edge.trust(&desk_edge).await.unwrap();
        let link = laptop_edge.link_in_process(&desk_edge).unwrap();
        assert!(wait_for_route(&laptop_edge, desk_runtime.host(), PATIENCE).await);
        assert!(wait_for_route(&desk_edge, laptop_runtime.host(), PATIENCE).await);
        until("the laptop to learn the desk's agent", || async {
            match laptop_runtime.owner(coder.id.as_bytes()).await {
                Ok(Owner::Peer(_)) => Ok(()),
                Ok(owner) => Err(format!("{owner:?}")),
                Err(error) => Err(error.to_string()),
            }
        })
        .await
        .unwrap();
        // A person on the laptop opens the desk's agent: the laptop's
        // replica now holds its rows.
        let mut session = laptop_runtime
            .subscribe_session(coder.id.as_bytes(), 10)
            .await
            .unwrap();
        let mut seen = Vec::new();
        read_until(&mut session, &mut seen, "the replica's catch-up", |seen| {
            caught_ups(seen) > 0
                && seen.iter().any(|event| {
                    matches!(&event.of, Some(session_event::Of::Item(item)) if item.key == "c1")
                })
        })
        .await;
        Self {
            desk,
            laptop,
            desk_daemon,
            laptop_daemon,
            desk_runtime,
            laptop_runtime,
            desk_edge,
            laptop_edge,
            coder,
            notes,
            _link: link,
        }
    }

    /// The channel the desk's calls to the laptop ride.
    pub(crate) async fn to_laptop(&self) -> Channel {
        self.desk_edge
            .channel(self.laptop_runtime.host())
            .await
            .unwrap()
    }

    /// The laptop's peer service, as the desk's daemon calls it for itself.
    async fn laptop_peer(&self) -> wire::peer_service_client::PeerServiceClient<Channel> {
        wire::peer_service_client(self.to_laptop().await)
    }

    pub(crate) async fn stop(mut self) {
        self.coder.die().await;
        self.notes.die().await;
        let Self {
            desk_daemon,
            laptop_daemon,
            desk_runtime,
            laptop_runtime,
            desk_edge,
            laptop_edge,
            desk,
            laptop,
            _link,
            ..
        } = self;
        drop((_link, desk_runtime, laptop_runtime, desk_edge, laptop_edge));
        desk_daemon.shutdown().await.unwrap();
        laptop_daemon.shutdown().await.unwrap();
        drop((desk, laptop));
    }
}

fn put(agent: &SyntheticAgent, bytes: &[u8]) -> PutBlobRequest {
    PutBlobRequest {
        agent_id: agent.id.as_bytes().to_vec(),
        name: "photo.png".to_owned(),
        mime: "image/png".to_owned(),
        bytes: bytes.to_vec(),
    }
}

fn tail(agent: &SyntheticAgent, rows: u32) -> FollowSessionRequest {
    FollowSessionRequest {
        agent_id: agent.id.as_bytes().to_vec(),
        from: Some(follow_session_request::From::Tail(rows)),
    }
}

fn page(agent: &SyntheticAgent) -> FetchItemsRequest {
    FetchItemsRequest {
        agent_id: agent.id.as_bytes().to_vec(),
        before_order: None,
        page_size: 10,
    }
}

fn blob(agent: &SyntheticAgent, hash: &[u8]) -> FetchBlobRequest {
    FetchBlobRequest {
        agent_id: agent.id.as_bytes().to_vec(),
        hash: hash.to_vec(),
    }
}

/// The hosts and agents an inventory stream carried.
fn listed(events: &[InventoryEvent]) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut hosts = Vec::new();
    let mut agents = Vec::new();
    for event in events {
        match &event.of {
            Some(inventory_event::Of::Host(host)) => hosts.push(host.host_id().to_vec()),
            Some(inventory_event::Of::HostRemoved(removed)) => hosts.push(removed.host_id.clone()),
            Some(inventory_event::Of::Agent(agent)) => agents.push(agent.agent_id.clone()),
            Some(inventory_event::Of::AgentRemoved(removed)) => {
                agents.push(removed.agent_id.clone())
            }
            Some(inventory_event::Of::CaughtUp(_) | inventory_event::Of::Lagged(_)) | None => {}
        }
    }
    (hosts, agents)
}

/// The laptop holds a row for the desk's agent, and FollowAgents still
/// carries only the laptop's own entry and its own agents, on opening and
/// live: what it knows of another host is that host's to tell.
#[tokio::test(flavor = "multi_thread")]
async fn follow_agents_carries_only_the_hosts_own_entry_and_agents() {
    let mut pair = Pair::new().await;
    let laptop = pair.laptop_runtime.host().as_bytes().to_vec();
    let notes = pair.notes.id.as_bytes().to_vec();
    let mut stream = pair
        .laptop_peer()
        .await
        .follow_agents(FollowAgentsRequest {})
        .await
        .unwrap()
        .into_inner();
    let next = async |stream: &mut tonic::Streaming<wire::FollowAgentsResponse>| {
        tokio::time::timeout(PATIENCE, stream.message())
            .await
            .expect("the follow answers")
            .unwrap()
            .expect("the follow stays open")
            .event
            .expect("a response holds an event")
    };
    let mut seen = Vec::new();
    loop {
        let event = next(&mut stream).await;
        let caught_up = matches!(event.of, Some(inventory_event::Of::CaughtUp(_)));
        seen.push(event);
        if caught_up {
            break;
        }
    }
    let (hosts, agents) = listed(&seen);
    assert_eq!(hosts, vec![laptop.clone()], "only the laptop's own entry");
    assert_eq!(agents, vec![notes.clone()], "only the laptop's own agents");

    // Live: the desk's agent changes and the laptop's replica follows,
    // then the laptop's own agent changes. Only the second reaches the
    // follow.
    pair.coder.append(&snapshot(Phase::Working, &[], 2_000));
    pair.desk_runtime.ingest(pair.coder.id).await.unwrap();
    let coder = pair.coder.id.as_bytes().to_vec();
    until(
        "the laptop's replica to follow the desk's agent",
        || async {
            let rows = pair
                .laptop_runtime
                .read_store(|store| store.agents().unwrap())
                .await
                .unwrap();
            match rows.iter().find(|row| row.agent.agent == coder) {
                Some(row) if row.phase() == Phase::Working => Ok(()),
                Some(row) => Err(format!("phase {:?}", row.phase())),
                None => Err("no replica row".to_owned()),
            }
        },
    )
    .await
    .unwrap();
    pair.notes.append(&snapshot(Phase::Working, &[], 2_000));
    pair.laptop_runtime.ingest(pair.notes.id).await.unwrap();
    let mut live = Vec::new();
    loop {
        let event = next(&mut stream).await;
        let notes_working = matches!(
            &event.of,
            Some(inventory_event::Of::Agent(agent))
                if agent.agent_id == notes && agent.phase() == Phase::Working
        );
        live.push(event);
        if notes_working {
            break;
        }
    }
    let (hosts, agents) = listed(&live);
    assert!(
        hosts.iter().all(|host| *host == laptop),
        "a host entry not the laptop's: {live:#?}"
    );
    assert!(
        agents.iter().all(|agent| *agent == notes),
        "an agent not the laptop's: {live:#?}"
    );
    pair.stop().await;
}

/// The laptop's own agent is answered on every read; the desk's agent,
/// whose rows and file the laptop holds as a copy, is not found on any.
/// So a paired host cannot read a third host's conversation or files
/// through this one, and a file this host holds only as a copy is fetched
/// from its own host.
#[tokio::test(flavor = "multi_thread")]
async fn a_replicas_agent_is_not_found_on_any_read_and_its_file_is_not_given() {
    let pair = Pair::new().await;
    let mut peer = pair.laptop_peer().await;
    let photo = b"a photo".to_vec();
    let own_blob = pair
        .laptop_runtime
        .put_blob(put(&pair.notes, &photo))
        .await
        .unwrap();
    // The laptop holds a copy of the desk's file, as a read-through keeps
    // one.
    let desk_blob = pair
        .desk_runtime
        .put_blob(put(&pair.coder, &photo))
        .await
        .unwrap();
    let coder = pair.coder.id.as_bytes().to_vec();
    pair.laptop_runtime
        .keep_replica_blob(&coder, &desk_blob.hash, &photo)
        .await
        .unwrap();
    pair.laptop_runtime
        .get_blob(wire::GetBlobRequest {
            agent_id: coder.clone(),
            hash: desk_blob.hash.clone(),
        })
        .await
        .expect("the laptop holds the replica's copy");

    // The laptop's own agent: a tail, a cursor, a page and its file.
    let mut opened = peer
        .follow_session(tail(&pair.notes, 10))
        .await
        .unwrap()
        .into_inner();
    let first = tokio::time::timeout(PATIENCE, opened.message())
        .await
        .unwrap()
        .unwrap()
        .expect("the follow opens");
    assert!(first.event.is_some(), "{first:?}");
    peer.follow_session(FollowSessionRequest {
        agent_id: pair.notes.id.as_bytes().to_vec(),
        from: Some(follow_session_request::From::After(wire::After::default())),
    })
    .await
    .expect("a copy follows from its cursor");
    let items = peer
        .fetch_items(page(&pair.notes))
        .await
        .unwrap()
        .into_inner();
    assert!(items.items.iter().any(|item| item.key == "n1"), "{items:?}");
    let read = peer
        .fetch_blob(blob(&pair.notes, &own_blob.hash))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(read.bytes, photo);

    // The desk's agent, held here only as a copy.
    let refusals = [
        (
            "FollowSession",
            peer.follow_session(tail(&pair.coder, 10)).await.err(),
        ),
        ("ListItems", peer.fetch_items(page(&pair.coder)).await.err()),
        (
            "GetBlob",
            peer.fetch_blob(blob(&pair.coder, &desk_blob.hash))
                .await
                .err(),
        ),
    ];
    for (call, refused) in refusals {
        let refused = refused.unwrap_or_else(|| panic!("{call} on a replica was answered"));
        assert_eq!(refused.code(), Code::NotFound, "{call}: {refused:?}");
    }
    pair.stop().await;
}

/// `request` as a node carrying it for `caller` writes it.
fn carried<T>(message: T, caller: &wire::Caller) -> Request<T> {
    let mut request = Request::new(message);
    request.metadata_mut().insert_bin(
        wire::CALLER_HEADER,
        MetadataValue::from_bytes(&caller.encode_to_vec()),
    );
    request
}

/// A call with a caller header is one a node carried for a person or an
/// agent, and the peer service answers only a daemon asking for itself: so
/// no client reaches it through the node it is attached to.
#[tokio::test(flavor = "multi_thread")]
async fn the_peer_service_refuses_a_call_carried_for_someone() {
    let pair = Pair::new().await;
    let mut peer = pair.laptop_peer().await;
    let callers = [
        wire::Caller::person("terminal"),
        wire::Caller::agent(
            pair.coder.id.as_bytes().to_vec(),
            "coder",
            1,
            wire::PermissionLevel::Auto,
        ),
    ];
    for caller in &callers {
        let report = DeliverReportRequest {
            id: uuid::Uuid::new_v4().as_bytes().to_vec(),
            child_agent_id: pair.coder.id.as_bytes().to_vec(),
            child_name: "coder".to_owned(),
            parent_agent_id: pair.notes.id.as_bytes().to_vec(),
            report: Some(turn_report(1, "carried")),
        };
        let refusals = [
            (
                "FollowAgents",
                peer.follow_agents(carried(FollowAgentsRequest {}, caller))
                    .await
                    .err(),
            ),
            (
                "FollowSession",
                peer.follow_session(carried(tail(&pair.notes, 10), caller))
                    .await
                    .err(),
            ),
            (
                "ListItems",
                peer.fetch_items(carried(page(&pair.notes), caller))
                    .await
                    .err(),
            ),
            (
                "GetBlob",
                peer.fetch_blob(carried(blob(&pair.notes, &[0; 32]), caller))
                    .await
                    .err(),
            ),
            (
                "DeliverReport",
                peer.deliver_report(carried(report, caller)).await.err(),
            ),
        ];
        for (call, refused) in refusals {
            let refused = refused.unwrap_or_else(|| panic!("{call} carried for someone answered"));
            assert_eq!(
                refused.code(),
                Code::PermissionDenied,
                "{call} for {caller:?}: {refused:?}"
            );
        }
    }
    assert!(
        pair.notes.inputs().is_empty(),
        "no carried report reached the laptop's agent"
    );
    pair.stop().await;
}

/// A host service answers a person or an agent, and a call on a link
/// without the caller header is the paired daemon asking for itself: every
/// host service refuses it alike, before any handler runs, so none answers
/// a paired daemon by forgetting to ask who is calling.
#[tokio::test(flavor = "multi_thread")]
async fn a_host_service_refuses_the_paired_daemon_asking_for_itself() {
    let pair = Pair::new().await;
    let channel = pair.to_laptop().await;
    let notes = pair.notes.id.as_bytes().to_vec();
    let refusals = [
        (
            "GetCatalogue",
            wire::host_service_client(channel.clone())
                .get_catalogue(wire::GetCatalogueRequest {
                    provider: "codex".to_owned(),
                })
                .await
                .err(),
        ),
        (
            "PinFolder",
            wire::host_service_client(channel.clone())
                .pin_folder(wire::PinFolderRequest {
                    path: pair.laptop.path("code").display().to_string(),
                    pinned: true,
                })
                .await
                .err(),
        ),
        (
            "GetChanges",
            wire::review_service_client(channel.clone())
                .get_changes(wire::GetChangesRequest {
                    agent_id: notes.clone(),
                    ..Default::default()
                })
                .await
                .err(),
        ),
        (
            "RenameAgent",
            wire::agent_service_client(channel)
                .rename_agent(wire::RenameAgentRequest {
                    agent_id: notes,
                    name: "renamed".to_owned(),
                })
                .await
                .err(),
        ),
    ];
    for (call, refused) in refusals {
        let refused = refused.unwrap_or_else(|| panic!("{call} answered the paired daemon"));
        assert_eq!(
            refused.code(),
            Code::PermissionDenied,
            "{call}: {refused:?}"
        );
        assert!(
            refused
                .message()
                .contains("asks for itself only on the peer service"),
            "{call}: {refused:?}"
        );
    }
    pair.stop().await;
}

/// Inventory, sessions and drafts are this node's, for the clients
/// attached to it: none is mounted on a link, so a paired host cannot ask
/// about third hosts, resolve a name here or read a draft.
#[tokio::test(flavor = "multi_thread")]
async fn the_local_services_are_not_served_on_a_link() {
    let pair = Pair::new().await;
    let channel = pair.to_laptop().await;
    let notes = pair.notes.id.as_bytes().to_vec();
    let mut inventory = wire::inventory_service_client(channel.clone());
    let mut sessions = wire::session_service_client(channel.clone());
    let mut drafts = wire::draft_service_client(channel);
    // Both as the daemon asking for itself and carried for a person: no
    // header settles anything that is not mounted.
    let person = wire::Caller::person("phone");
    for header in [None, Some(&person)] {
        let calls = [
            (
                "SubscribeInventory",
                inventory
                    .subscribe_inventory(carried_or(wire::SubscribeInventoryRequest {}, header))
                    .await
                    .err(),
            ),
            (
                "ResolveAgent",
                inventory
                    .resolve_agent(carried_or(
                        wire::ResolveAgentRequest {
                            name: "notes".to_owned(),
                        },
                        header,
                    ))
                    .await
                    .err(),
            ),
            (
                "ResolveHost",
                inventory
                    .resolve_host(carried_or(
                        wire::ResolveHostRequest {
                            name: "desk".to_owned(),
                        },
                        header,
                    ))
                    .await
                    .err(),
            ),
            (
                "SubscribeSession",
                sessions
                    .subscribe_session(carried_or(
                        wire::SubscribeSessionRequest {
                            agent_id: notes.clone(),
                            tail: 10,
                        },
                        header,
                    ))
                    .await
                    .err(),
            ),
            (
                "ListItems",
                sessions
                    .list_items(carried_or(
                        wire::ListItemsRequest {
                            agent_id: notes.clone(),
                            before_order: None,
                            page_size: 10,
                        },
                        header,
                    ))
                    .await
                    .err(),
            ),
            (
                "GetItem",
                sessions
                    .get_item(carried_or(
                        wire::GetItemRequest {
                            agent_id: notes.clone(),
                            key: "n1".to_owned(),
                        },
                        header,
                    ))
                    .await
                    .err(),
            ),
            (
                "GetBlob",
                sessions
                    .get_blob(carried_or(
                        wire::GetBlobRequest {
                            agent_id: notes.clone(),
                            hash: vec![0; 32],
                        },
                        header,
                    ))
                    .await
                    .err(),
            ),
            (
                "UpdateDraft",
                drafts
                    .update_draft(carried_or(
                        wire::UpdateDraftRequest {
                            agent_id: notes.clone(),
                            draft: Some(wire::Draft::default()),
                        },
                        header,
                    ))
                    .await
                    .err(),
            ),
            (
                "GetDraft",
                drafts
                    .get_draft(carried_or(
                        wire::GetDraftRequest {
                            agent_id: notes.clone(),
                        },
                        header,
                    ))
                    .await
                    .err(),
            ),
        ];
        for (call, refused) in calls {
            let refused = refused.unwrap_or_else(|| panic!("{call} answered on a link"));
            assert_eq!(refused.code(), Code::Unimplemented, "{call}: {refused:?}");
        }
    }
    pair.stop().await;
}

fn carried_or<T>(message: T, caller: Option<&wire::Caller>) -> Request<T> {
    match caller {
        Some(caller) => carried(message, caller),
        None => Request::new(message),
    }
}

/// A child's report of a finished turn, for its parent's `incarnation`.
fn turn_report(incarnation: u32, text: &str) -> wire::ChildReport {
    wire::ChildReport {
        to_incarnation: incarnation,
        from_incarnation: 1,
        end: Some(wire::child_report::End::TurnEnded(wire::TurnEnded {
            text: text.to_owned(),
            outcome: wire::TurnOutcome::Completed as i32,
            last: false,
        })),
    }
}

/// The reports `agent` was handed, by their words.
fn reports(agent: &SyntheticAgent) -> Vec<(Vec<u8>, String)> {
    agent
        .inputs()
        .into_iter()
        .filter_map(|handed| match handed {
            Handed::Message(envelope) => match envelope.body {
                Some(wire::envelope::Body::Report(wire::ChildReport {
                    end: Some(wire::child_report::End::TurnEnded(turn)),
                    ..
                })) => {
                    let child = envelope
                        .from
                        .and_then(|from| from.value)
                        .and_then(|value| match value {
                            wire::sender::Value::Agent(sender) => sender.agent,
                            wire::sender::Value::Person(_) => None,
                        })
                        .map(|agent| agent.host_id)
                        .unwrap_or_default();
                    Some((child, turn.text))
                }
                _ => None,
            },
            Handed::Input(_) => None,
        })
        .collect()
}

/// The desk's daemon reports its child's turn to the parent here. It
/// reaches the parent's incarnation it is for, as from the desk's agent;
/// one for another incarnation is dropped; and an exited parent is not
/// woken by one, which is dropped too.
#[tokio::test(flavor = "multi_thread")]
async fn a_report_reaches_its_parents_incarnation_and_never_wakes_it() {
    let mut pair = Pair::new().await;
    let mut peer = pair.laptop_peer().await;
    let desk = pair.desk_runtime.host().as_bytes().to_vec();
    let (coder, notes) = (pair.coder.id, pair.notes.id);
    let report = |incarnation: u32, text: &str| DeliverReportRequest {
        id: uuid::Uuid::new_v4().as_bytes().to_vec(),
        child_agent_id: coder.as_bytes().to_vec(),
        child_name: "coder".to_owned(),
        parent_agent_id: notes.as_bytes().to_vec(),
        report: Some(turn_report(incarnation, text)),
    };

    peer.deliver_report(report(pair.notes.incarnation, "the tests pass"))
        .await
        .expect("a report for the parent's incarnation is taken");
    until("the parent to hear the report", || async {
        match reports(&pair.notes).as_slice() {
            [_] => Ok(()),
            heard => Err(format!("{heard:?}")),
        }
    })
    .await
    .unwrap();
    assert_eq!(
        reports(&pair.notes),
        vec![(desk.clone(), "the tests pass".to_owned())],
        "from the desk's agent: the link's host, never one the request names"
    );

    let stale = peer
        .deliver_report(report(pair.notes.incarnation + 1, "for another run"))
        .await
        .expect_err("a report for another incarnation is dropped");
    assert_eq!(stale.code(), Code::NotFound, "{stale:?}");

    // The parent exits; a report for the incarnation that exited does not
    // start it again, and is stale: the parent's next life is a new run.
    pair.notes.exit(wire::ExitCause::Finished, None).await;
    until("the parent to exit", || async {
        match pair.laptop_runtime.agent(notes).await {
            Ok(agent) if agent.lifecycle == wire::Lifecycle::Exited as i32 => Ok(()),
            Ok(agent) => Err(format!("{:?}", agent.lifecycle())),
            Err(error) => Err(error.to_string()),
        }
    })
    .await
    .unwrap();
    let refused = peer
        .deliver_report(report(pair.notes.incarnation, "after it exited"))
        .await
        .expect_err("a report does not wake an exited parent");
    assert_eq!(refused.code(), Code::NotFound, "{refused:?}");
    let agent = pair.laptop_runtime.agent(notes).await.unwrap();
    assert_eq!(
        (agent.lifecycle(), agent.incarnation),
        (wire::Lifecycle::Exited, pair.notes.incarnation),
        "still exited, in the same incarnation"
    );
    assert_eq!(reports(&pair.notes).len(), 1, "nothing more was heard");
    pair.stop().await;
}
