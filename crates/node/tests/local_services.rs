//! What a node answers from what it holds, for the clients attached to it:
//! InventoryService, SessionService and DraftService, called through the
//! node's real entries. An agent on its tools socket reads only its own
//! children's conversations, files and drafts; a host name resolves
//! exactly first, then ignoring case; a client opening is counted where it
//! opened; and a file of another host's agent not fetched yet is asked of
//! its host, checked and kept.

use std::path::{Path, PathBuf};

use analytics::{Client, Event, Recording};
use sha2::{Digest as _, Sha256};
use support::synthetic::*;
use support::*;
use tonic::transport::Channel;
use tonic::{Code, Request};
use wire::{
    GetBlobRequest, GetDraftRequest, GetItemRequest, ListItemsRequest, Phase, PutBlobRequest,
    ResolveHostRequest, SubscribeInventoryRequest, SubscribeSessionRequest, UpdateDraftRequest,
};

use crate::peer_server::{Pair, serving};
use crate::support;

const SEGMENTS: u64 = 1 << 20;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The profile's socket, where a person's clients call.
async fn profile_socket(runtime: &node::ProfileRuntime) -> Channel {
    local_channel(&runtime.dir().join(node::PROFILE_SOCKET)).await
}

/// `agent`'s tools socket, once the daemon serves it.
async fn tools_socket(agent: &SyntheticAgent) -> Channel {
    let path = agent.dir.join(agent_dir::TOOLS_SOCK);
    until("the agent's tools socket", || async {
        if listens(&path).await {
            Ok(())
        } else {
            Err("not yet".to_owned())
        }
    })
    .await
    .unwrap();
    local_channel(&path).await
}

/// Every read the session and draft services make of `agent`, as the
/// refusal each came back with, if any.
async fn reads(channel: &Channel, agent: &[u8], hash: &[u8]) -> Vec<(&'static str, Option<Code>)> {
    let mut sessions = wire::session_service_client(channel.clone());
    let mut drafts = wire::draft_service_client(channel.clone());
    let draft = wire::Draft {
        text: "half a thought".to_owned(),
        ..Default::default()
    };
    vec![
        (
            "SubscribeSession",
            sessions
                .subscribe_session(SubscribeSessionRequest {
                    agent_id: agent.to_vec(),
                    tail: 5,
                })
                .await
                .err()
                .map(|status| status.code()),
        ),
        (
            "ListItems",
            sessions
                .list_items(ListItemsRequest {
                    agent_id: agent.to_vec(),
                    before_order: None,
                    page_size: 10,
                })
                .await
                .err()
                .map(|status| status.code()),
        ),
        (
            "GetItem",
            sessions
                .get_item(GetItemRequest {
                    agent_id: agent.to_vec(),
                    key: "first".to_owned(),
                })
                .await
                .err()
                .map(|status| status.code()),
        ),
        (
            "GetBlob",
            sessions
                .get_blob(GetBlobRequest {
                    agent_id: agent.to_vec(),
                    hash: hash.to_vec(),
                })
                .await
                .err()
                .map(|status| status.code()),
        ),
        (
            "UpdateDraft",
            drafts
                .update_draft(UpdateDraftRequest {
                    agent_id: agent.to_vec(),
                    draft: Some(draft),
                })
                .await
                .err()
                .map(|status| status.code()),
        ),
        (
            "GetDraft",
            drafts
                .get_draft(GetDraftRequest {
                    agent_id: agent.to_vec(),
                })
                .await
                .err()
                .map(|status| status.code()),
        ),
    ]
}

/// An agent that started `helper` reads its conversation, its files and
/// its draft on its tools socket; an agent it did not start it reaches
/// only by messaging, so every read of `other` is refused. A person on the
/// profile's socket reads both.
#[tokio::test(flavor = "multi_thread")]
async fn on_its_tools_socket_an_agent_reads_only_its_own_childrens() {
    let install = Install::new();
    let mut lead = SyntheticAgent::new(&install, "lead", SEGMENTS);
    lead.register(&install);
    lead.go_live();
    let mut helper = SyntheticAgent::new(&install, "helper", SEGMENTS);
    helper.parent = Some(lead.key(&install));
    helper.register(&install);
    helper.append(&item("first", "the helper's first item"));
    helper.append(&snapshot(Phase::Idle, &[], 1_000));
    helper.go_live();
    let mut other = SyntheticAgent::new(&install, "other", SEGMENTS);
    other.register(&install);
    other.append(&item("first", "another agent's first item"));
    other.append(&snapshot(Phase::Idle, &[], 1_000));
    other.go_live();
    let daemon = serving(&install, install.options("boot-1", quiet_launch())).await;
    let runtime = runtime(&daemon, &install);
    let file = |agent: &SyntheticAgent| PutBlobRequest {
        agent_id: agent.id.as_bytes().to_vec(),
        name: "notes.txt".to_owned(),
        mime: "text/plain".to_owned(),
        bytes: b"some notes".to_vec(),
    };
    let helper_file = runtime.put_blob(file(&helper)).await.unwrap();
    let other_file = runtime.put_blob(file(&other)).await.unwrap();

    let tools = tools_socket(&lead).await;
    let answered: Vec<_> = reads(&tools, helper.id.as_bytes(), &helper_file.hash)
        .await
        .into_iter()
        .map(|(call, refused)| (call, refused.is_none()))
        .collect();
    assert!(
        answered.iter().all(|(_, answered)| *answered),
        "its child: {answered:?}"
    );
    for (call, refused) in reads(&tools, other.id.as_bytes(), &other_file.hash).await {
        assert_eq!(
            refused,
            Some(Code::PermissionDenied),
            "{call} of a non-child"
        );
    }
    for (call, refused) in reads(&tools, b"not an agent id", &other_file.hash).await {
        assert_eq!(
            refused,
            Some(Code::InvalidArgument),
            "{call} of no agent id"
        );
    }

    let person = profile_socket(&runtime).await;
    for agent in [&helper, &other] {
        let hash = if agent.id == helper.id {
            &helper_file.hash
        } else {
            &other_file.hash
        };
        for (call, refused) in reads(&person, agent.id.as_bytes(), hash).await {
            assert_eq!(refused, None, "{call} by a person of {}", agent.name);
        }
    }
    let draft = wire::draft_service_client(person)
        .get_draft(GetDraftRequest {
            agent_id: other.id.as_bytes().to_vec(),
        })
        .await
        .unwrap()
        .into_inner()
        .draft
        .unwrap();
    assert_eq!(draft.text, "half a thought", "the draft is kept");

    for agent in [&mut lead, &mut helper, &mut other] {
        agent.die().await;
    }
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// A daemon named `name`, its edge holding that name.
async fn named(install: &Install, name: &str) -> node::Daemon {
    let mut options = install.options("boot-1", quiet_launch());
    options.edge.host_name = name.to_owned();
    serving(install, options).await
}

/// A host name a person writes is matched exactly first, then ignoring
/// case; one that answers to several is ambiguous, and one that answers to
/// none says which hosts this node knows.
#[tokio::test(flavor = "multi_thread")]
async fn resolve_host_matches_exactly_then_ignoring_case_and_names_the_known_hosts() {
    let (desk, upper, lower) = (Install::new(), Install::new(), Install::new());
    let desk_daemon = named(&desk, "desk").await;
    let upper_daemon = named(&upper, "Laptop").await;
    let lower_daemon = named(&lower, "laptop").await;
    let desk_runtime = runtime(&desk_daemon, &desk);
    let (upper_runtime, lower_runtime) = (
        runtime(&upper_daemon, &upper),
        runtime(&lower_daemon, &lower),
    );
    let edge = desk_runtime.edge().unwrap();
    edge.trust(&upper_runtime.edge().unwrap()).await.unwrap();
    edge.trust(&lower_runtime.edge().unwrap()).await.unwrap();
    let mut inventory = wire::inventory_service_client(profile_socket(&desk_runtime).await);
    let mut resolve = async |name: &str| {
        inventory
            .resolve_host(ResolveHostRequest {
                name: name.to_owned(),
            })
            .await
            .map(|response| response.into_inner().host.unwrap())
    };

    let exact = resolve("laptop").await.unwrap();
    assert_eq!(
        exact.host_id(),
        lower_runtime.host().as_bytes(),
        "an exact match wins over one ignoring case"
    );
    assert_eq!(exact.name(), "laptop");
    let own = resolve("DESK").await.unwrap();
    assert_eq!(
        own.host_id(),
        desk_runtime.host().as_bytes(),
        "ignoring case"
    );
    let ambiguous = resolve("LAPTOP").await.unwrap_err();
    assert_eq!(ambiguous.code(), Code::FailedPrecondition, "{ambiguous:?}");
    let missing = resolve("studio").await.unwrap_err();
    assert_eq!(missing.code(), Code::NotFound, "{missing:?}");
    for known in ["desk", "Laptop", "laptop"] {
        assert!(
            missing.message().contains(known),
            "names {known}: {}",
            missing.message()
        );
    }

    drop((desk_runtime, upper_runtime, lower_runtime, edge));
    for daemon in [desk_daemon, upper_daemon, lower_daemon] {
        daemon.shutdown().await.unwrap();
    }
}

/// A person's client listing the fleet on the profile's socket is a client
/// opening, counted by this node as the client it names itself; an agent
/// listing it on its tools socket is no client opening.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_opening_is_counted_on_the_node_it_opened_on() {
    let install = Install::new();
    let mut worker = SyntheticAgent::new(&install, "worker", SEGMENTS);
    worker.register(&install);
    worker.go_live();
    let recording = Recording::new();
    let options = node::StartOptions {
        analytics: node::Telemetry::Record(recording.clone()),
        ..install.options("boot-1", quiet_launch())
    };
    let daemon = serving(&install, options).await;
    let runtime = runtime(&daemon, &install);
    let opened = || {
        recording
            .events()
            .into_iter()
            .filter_map(|(_, event)| match event {
                Event::ClientOpened { client } => Some(client),
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    let mut tools = wire::inventory_service_client(tools_socket(&worker).await);
    drop(
        tools
            .subscribe_inventory(SubscribeInventoryRequest {})
            .await
            .unwrap(),
    );
    assert_eq!(opened(), [], "an agent is no client");

    let mut person = wire::inventory_service_client(profile_socket(&runtime).await);
    let mut request = Request::new(SubscribeInventoryRequest {});
    request
        .metadata_mut()
        .insert(wire::SURFACE_HEADER, "mac".parse().unwrap());
    drop(person.subscribe_inventory(request).await.unwrap());
    assert_eq!(opened(), [Client::Mac]);

    worker.die().await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// The blob file of `agent` on `host`, as this profile keeps a replica's.
fn replica_file(profile: &Path, host: node::HostId, agent: uuid::Uuid, hash: &[u8]) -> PathBuf {
    profile
        .join(node::REPLICAS)
        .join(host.to_string())
        .join(node::AGENTS)
        .join(agent.to_string())
        .join(agent_dir::BLOBS)
        .join(hex(hash))
}

/// A person on the laptop opens a file of the desk's agent the laptop has
/// not fetched yet. The laptop asks the desk for it as itself, keeps it,
/// and answers from its copy; bytes that are not the file they are named
/// by are refused and not kept; a file the desk does not hold either is
/// not found, in the desk's own words.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_not_fetched_yet_is_asked_of_its_host_checked_and_kept() {
    captured_log();
    let pair = Pair::new().await;
    let desk = pair.desk_runtime.host();
    let coder = pair.coder.id;
    let photo = b"a photo of the whiteboard".to_vec();
    let blob = pair
        .desk_runtime
        .put_blob(PutBlobRequest {
            agent_id: coder.as_bytes().to_vec(),
            name: "board.png".to_owned(),
            mime: "image/png".to_owned(),
            bytes: photo.clone(),
        })
        .await
        .unwrap();
    let kept = replica_file(pair.laptop_runtime.dir(), desk, coder, &blob.hash);
    assert!(!kept.exists(), "not fetched yet");
    let mut sessions = wire::session_service_client(profile_socket(&pair.laptop_runtime).await);
    let read = |hash: &[u8]| GetBlobRequest {
        agent_id: coder.as_bytes().to_vec(),
        hash: hash.to_vec(),
    };

    // The fetch is a call to a paired host like every other this daemon
    // makes for itself, so its link channel logs it as one.
    let fetches = || {
        let log = String::from_utf8_lossy(&captured_log().lock().unwrap()).into_owned();
        log.lines()
            .filter(|line| {
                line.contains("a call to a paired host")
                    && line.contains("/amux.v1.PeerService/FetchBlob")
            })
            .count()
    };
    let before = fetches();
    let fetched = sessions.get_blob(read(&blob.hash)).await.unwrap();
    assert_eq!(fetched.into_inner().bytes, photo);
    assert_eq!(fetches(), before + 1, "one call to the desk, logged");
    assert_eq!(
        std::fs::read(&kept).unwrap(),
        photo,
        "kept for the next reader"
    );
    // The next read is the copy's: the desk's file can go and it is still
    // answered.
    std::fs::remove_file(pair.coder.dir.join(agent_dir::BLOBS).join(hex(&blob.hash))).unwrap();
    let again = sessions.get_blob(read(&blob.hash)).await.unwrap();
    assert_eq!(again.into_inner().bytes, photo);

    // The desk's file is damaged: what arrives is not the file its hash
    // names.
    let meant = b"the real diagram".to_vec();
    let hash = Sha256::digest(&meant).to_vec();
    let blobs = pair.coder.dir.join(agent_dir::BLOBS);
    std::fs::write(blobs.join(hex(&hash)), b"something else entirely").unwrap();
    let refused = sessions.get_blob(read(&hash)).await.unwrap_err();
    assert_eq!(refused.code(), Code::Internal, "{refused:?}");
    assert!(
        refused.message().contains("not the blob"),
        "{}",
        refused.message()
    );
    assert!(
        !replica_file(pair.laptop_runtime.dir(), desk, coder, &hash).exists(),
        "bytes that are not the file are not kept"
    );

    let missing = sessions.get_blob(read(&[9; 32])).await.unwrap_err();
    assert_eq!(missing.code(), Code::NotFound, "{missing:?}");
    pair.stop().await;
}
