//! What a daemon asks a paired daemon for itself goes on the peer service
//! and nowhere else: following the host's agents and an agent's session, a
//! page below a copy's block, a child's report to its parent there, and a
//! file a message from one of that host's agents carries. Each call a
//! daemon makes of a peer is in its debug log, by method; a desk and a
//! laptop doing all five ask for nothing else.

use std::sync::Arc;

use node::{Daemon, Edge, LoopbackLink, Owner, ProfileRuntime};
use support::synthetic::*;
use support::*;
use tonic::codegen::http;
use tonic::metadata::MetadataMap;
use wire::amux::staging;

use crate::support;

const SEGMENTS: u64 = 1 << 20;

/// The rows a replica's block opens with: fewer than the desk's agent
/// holds, so the laptop asks the desk for a page below it.
const TAIL: u32 = 2;

/// The words the desk's agent wrote, oldest first.
const SAID: [&str; 5] = ["one", "two", "three", "four", "five"];

/// What each daemon's debug log says it asked a paired host for, by
/// method path, in order.
fn calls_made() -> Vec<String> {
    let log = String::from_utf8_lossy(&captured_log().lock().unwrap()).into_owned();
    log.lines()
        .filter(|line| line.contains("a call to a paired host"))
        .filter_map(|line| line.split("method=").nth(1))
        .filter_map(|method| method.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

/// `message` as the desk's node carries it for `caller`, with the caller
/// header it writes beside it.
fn carried<T>(message: T, caller: &staging::Caller) -> tonic::Request<T> {
    let mut request = tonic::Request::new(message);
    let mut headers = http::HeaderMap::new();
    headers.insert(wire::CALLER_HEADER, caller.to_header().parse().unwrap());
    *request.metadata_mut() = MetadataMap::from_headers(headers);
    request
}

/// A desk whose agent has said more than a replica's tail, with a lead the
/// laptop's finished child reports to, and a laptop running that child and
/// an agent the desk's agent writes to; paired and linked.
struct Pair {
    desk: Install,
    laptop: Install,
    desk_daemon: Daemon,
    laptop_daemon: Daemon,
    desk_runtime: Arc<ProfileRuntime>,
    laptop_runtime: Arc<ProfileRuntime>,
    desk_edge: Arc<Edge>,
    laptop_edge: Arc<Edge>,
    coder: SyntheticAgent,
    lead: SyntheticAgent,
    child: SyntheticAgent,
    notes: SyntheticAgent,
    _link: LoopbackLink,
}

impl Pair {
    async fn new() -> Self {
        // The log is captured from its first line on.
        captured_log();
        let launch = || {
            let mut launch = quiet_launch();
            launch.core.tail_rows = TAIL;
            launch
        };
        let desk = Install::new();
        let mut coder = SyntheticAgent::new(&desk, "coder", SEGMENTS);
        coder.register(&desk);
        coder.append(&snapshot(wire::Phase::Idle, &[], 1_000));
        for (n, said) in SAID.iter().enumerate() {
            coder.append(&item(&format!("i{n}"), said));
        }
        coder.go_live();
        let mut lead = SyntheticAgent::new(&desk, "lead", SEGMENTS);
        lead.register(&desk);
        lead.go_live();

        let laptop = Install::new();
        let mut child = SyntheticAgent::new(&laptop, "child", SEGMENTS);
        child.parent = Some(lead.key(&desk));
        child.register(&laptop);
        child.append(&snapshot(wire::Phase::Working, &[], 2_000));
        child.go_live();
        let mut notes = SyntheticAgent::new(&laptop, "notes", SEGMENTS);
        notes.register(&laptop);
        notes.go_live();

        let desk_daemon = desk.start("boot-1", launch()).await;
        let desk_runtime = runtime(&desk_daemon, &desk);
        let laptop_daemon = laptop.start("boot-1", launch()).await;
        let laptop_runtime = runtime(&laptop_daemon, &laptop);
        let (desk_edge, laptop_edge) =
            (desk_runtime.edge().unwrap(), laptop_runtime.edge().unwrap());
        desk_edge.trust(&laptop_edge).await.unwrap();
        laptop_edge.trust(&desk_edge).await.unwrap();
        let link = laptop_edge.link_in_process(&desk_edge).unwrap();
        assert!(wait_for_route(&laptop_edge, desk_runtime.host(), PATIENCE).await);
        assert!(wait_for_route(&desk_edge, laptop_runtime.host(), PATIENCE).await);
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
            lead,
            child,
            notes,
            _link: link,
        }
    }

    async fn stop(mut self) {
        for agent in [
            &mut self.coder,
            &mut self.lead,
            &mut self.child,
            &mut self.notes,
        ] {
            agent.die().await;
        }
        let Self {
            desk,
            laptop,
            desk_daemon,
            laptop_daemon,
            desk_runtime,
            laptop_runtime,
            desk_edge,
            laptop_edge,
            _link,
            ..
        } = self;
        drop((desk_runtime, laptop_runtime, desk_edge, laptop_edge, _link));
        desk_daemon.shutdown().await.unwrap();
        laptop_daemon.shutdown().await.unwrap();
        drop((desk, laptop));
    }
}

/// A laptop follows a desk, reads a page of an agent's history older than
/// its copy holds, delivers a child's report to its parent there, and
/// fetches the file the desk's agent sent one of its own: every call either
/// daemon makes of the other is on the peer service, and each of its five
/// methods is used.
#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_asks_a_paired_one_only_on_the_peer_service() {
    let mut pair = Pair::new().await;
    let coder = pair.coder.id.as_bytes().to_vec();

    // The laptop learns the desk's agents and copies the coder's tail.
    until("the laptop to copy the desk's coder", || async {
        match pair.laptop_runtime.owner(&coder).await {
            Ok(Owner::Peer(_)) => {}
            other => return Err(format!("{other:?}")),
        }
        let key = pair.coder.key(&pair.desk);
        let held = pair
            .laptop_runtime
            .read_store(move |store| store.agent(&key).unwrap())
            .await
            .unwrap()
            .and_then(|row| row.complete_from_order);
        held.map(|_| ()).ok_or_else(|| "no block yet".to_owned())
    })
    .await
    .unwrap();

    // A page of all of it is the copy's block extended from the desk.
    let page = until("the laptop to read the coder's whole history", || async {
        let page = pair
            .laptop_runtime
            .list_items(&wire::ListItemsRequest {
                agent_id: coder.clone(),
                before_order: None,
                page_size: 20,
            })
            .await
            .map_err(|error| error.to_string())?;
        (page.items.len() == SAID.len())
            .then_some(page)
            .ok_or_else(|| "a short page".to_owned())
    })
    .await
    .unwrap();
    assert!(page.exhausted, "nothing older exists");

    // The laptop's child, its parent the desk's lead, ends its turn once
    // the laptop holds the lead's row, and its report reaches the lead.
    let lead = pair.lead.key(&pair.desk);
    until("the laptop to copy the desk's lead", || async {
        let lead = lead.clone();
        let held = pair
            .laptop_runtime
            .read_store(move |store| store.agent(&lead).unwrap())
            .await
            .unwrap();
        held.map(|_| ()).ok_or_else(|| "not yet".to_owned())
    })
    .await
    .unwrap();
    pair.child.append(&item("last", "the tests pass"));
    pair.child.append(&turn_end(1, "last"));
    pair.child.nudge().await;
    until("the lead to hear its child", || async {
        pair.laptop_runtime.drain_deliveries().await;
        let heard = pair.lead.inputs().into_iter().any(|handed| {
            matches!(handed, Handed::Message(envelope)
                if matches!(envelope.body, Some(wire::envelope::Body::Report(_))))
        });
        heard
            .then_some(())
            .ok_or_else(|| "no report yet".to_owned())
    })
    .await
    .unwrap();
    let left = pair
        .laptop_runtime
        .read_store(|store| store.deliveries().unwrap().len())
        .await
        .unwrap();
    assert_eq!(left, 0, "the delivered row is gone");

    // The desk's agent writes the laptop's notes a message with a file;
    // the laptop fetches the file from the desk.
    let file = pair
        .desk_runtime
        .put_blob(wire::PutBlobRequest {
            agent_id: coder.clone(),
            name: "plan.txt".to_owned(),
            mime: "text/plain".to_owned(),
            bytes: b"the plan".to_vec(),
        })
        .await
        .unwrap();
    let channel = pair
        .desk_edge
        .channel(pair.laptop_runtime.host())
        .await
        .unwrap();
    let sender = staging::Caller::agent(coder.clone(), "coder");
    staging::agent_service_client(channel)
        .send_message(carried(
            staging::SendMessageRequest {
                id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                agent_id: pair.notes.id.as_bytes().to_vec(),
                context: None,
                message: Some(wire::MessageBody {
                    text: "read this".to_owned(),
                    attachments: vec![wire::Attachment {
                        of: Some(wire::attachment::Of::File(file.clone())),
                    }],
                }),
            },
            &sender,
        ))
        .await
        .expect("the laptop takes the message once it holds the file");
    let fetched = pair
        .laptop_runtime
        .get_blob(wire::GetBlobRequest {
            agent_id: pair.notes.id.as_bytes().to_vec(),
            hash: file.hash.clone(),
        })
        .await
        .unwrap();
    assert_eq!(fetched.bytes, b"the plan");

    let calls = calls_made();
    let others: Vec<&String> = calls
        .iter()
        .filter(|method| !method.starts_with("/amux.staging.PeerService/"))
        .collect();
    assert!(
        others.is_empty(),
        "a daemon asked a peer outside the peer service: {others:?}"
    );
    for method in [
        "FollowAgents",
        "FollowSession",
        "ListItems",
        "DeliverReport",
        "GetBlob",
    ] {
        let path = format!("/amux.staging.PeerService/{method}");
        assert!(calls.contains(&path), "no {method} in {calls:?}");
    }
    pair.stop().await;
}
