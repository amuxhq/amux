//! What a paired host may ask of this one. The peer service answers only
//! for this host's own agents and passes nothing on: the inventory it
//! streams is this host's entry and agents, and any call naming an agent it
//! only replicates is not found, so no host can reach a third host's agent
//! through it or speak for one.

mod support;

use std::sync::Arc;

use node::{Daemon, Edge, Owner, ProfileRuntime};
use support::synthetic::*;
use support::*;
use tonic::Code;
use wire::{
    DeleteAgentRequest, DiffRequest, Empty, FetchRequest, GetBlobRequest, GetRequest,
    InventoryEvent, PeerSubscribeRequest, Phase, PutBlobRequest, RenameAgentRequest,
    ResumeAgentRequest, SendInputRequest, StopAgentRequest, StopMode, inventory_event,
    peer_subscribe_request, session_event,
};

const SEGMENTS: u64 = 1 << 20;

/// A desk running `coder` and a laptop running `notes`, paired and linked,
/// with the laptop holding a replica of `coder` and its rows.
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
    notes: SyntheticAgent,
    _link: node::LoopbackLink,
}

impl Pair {
    async fn new() -> Self {
        let desk = Install::new();
        let mut coder = SyntheticAgent::new(&desk, "coder", SEGMENTS);
        coder.register_offline(&desk);
        coder.append(&item("c1", "the coder's first item"));
        coder.append(&snapshot(Phase::Idle, &[], 1_000));
        coder.go_live();
        let laptop = Install::new();
        let mut notes = SyntheticAgent::new(&laptop, "notes", SEGMENTS);
        notes.register_offline(&laptop);
        notes.append(&item("n1", "the notes' first item"));
        notes.append(&snapshot(Phase::Idle, &[], 1_000));
        notes.go_live();

        let desk_daemon = desk.start("boot-1", quiet_launch()).await;
        let desk_runtime = runtime(&desk_daemon, &desk);
        let laptop_daemon = laptop.start("boot-1", quiet_launch()).await;
        let laptop_runtime = runtime(&laptop_daemon, &laptop);
        let (desk_edge, laptop_edge) =
            (desk_runtime.edge().unwrap(), laptop_runtime.edge().unwrap());
        desk_edge.trust(&laptop_edge).await.unwrap();
        laptop_edge.trust(&desk_edge).await.unwrap();
        let link = laptop_edge.link_in_process(&desk_edge).unwrap();
        assert!(
            laptop_edge
                .wait_for_route(desk_runtime.host(), PATIENCE)
                .await
        );
        assert!(
            desk_edge
                .wait_for_route(laptop_runtime.host(), PATIENCE)
                .await
        );
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
        // replica now holds its rows, which a forwarding peer service would
        // have answered from.
        let mut session = laptop_runtime
            .subscribe(coder.id.as_bytes(), 10)
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

    /// The laptop's peer service, as the desk calls it.
    async fn laptop_peer(&self) -> node::PeerClient {
        self.desk_edge
            .peer(self.laptop_runtime.host())
            .await
            .unwrap()
    }

    async fn stop(mut self) {
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

#[tokio::test(flavor = "multi_thread")]
async fn replica_agent_is_refused_on_every_read_and_input() {
    let pair = Pair::new().await;
    let mut peer = pair.laptop_peer().await;
    let coder = pair.coder.id.as_bytes().to_vec();
    let notes = pair.notes.id.as_bytes().to_vec();

    let subscribe = |agent: &[u8]| PeerSubscribeRequest {
        agent_id: agent.to_vec(),
        from: Some(peer_subscribe_request::From::Tail(10)),
    };
    let fetch = |agent: &[u8]| FetchRequest {
        agent_id: agent.to_vec(),
        before_order: None,
        limit: 10,
    };
    let get = |agent: &[u8], key: &str| GetRequest {
        agent_id: agent.to_vec(),
        key: key.to_owned(),
    };
    let input = |agent: &[u8]| SendInputRequest {
        agent_id: agent.to_vec(),
        input: Some(sdk_prompt(b"through-the-laptop", "do this for me")),
    };
    let put = |agent: &[u8]| PutBlobRequest {
        agent_id: agent.to_vec(),
        name: "photo.png".to_owned(),
        mime: "image/png".to_owned(),
        bytes: b"a photo".to_vec(),
    };
    let blob = |agent: &[u8], hash: &[u8]| GetBlobRequest {
        agent_id: agent.to_vec(),
        hash: hash.to_vec(),
    };

    // The laptop holds a blob of its own agent, and a copy of one of the
    // desk's agent, kept under the replica as a forwarded read keeps it.
    let own_blob = pair.laptop_runtime.put_blob(put(&notes)).await.unwrap();
    let desk_blob = pair.desk_runtime.put_blob(put(&coder)).await.unwrap();
    pair.laptop_runtime
        .keep_replica_blob(&coder, &desk_blob.hash, b"a photo")
        .await
        .unwrap();
    pair.laptop_runtime
        .get_blob(blob(&coder, &desk_blob.hash))
        .await
        .expect("the laptop holds the replica's copy");

    // The laptop's own agent is answered.
    peer.subscribe(subscribe(&notes)).await.unwrap();
    let page = peer.fetch(fetch(&notes)).await.unwrap().into_inner();
    assert!(page.items.iter().any(|item| item.key == "n1"), "{page:?}");
    peer.get(get(&notes, "n1")).await.unwrap();
    let read = peer
        .get_blob(blob(&notes, &own_blob.hash))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(read.bytes, b"a photo");

    // The desk's agent, which the laptop holds rows for, is not.
    let refusals = [
        ("Subscribe", peer.subscribe(subscribe(&coder)).await.err()),
        ("Fetch", peer.fetch(fetch(&coder)).await.err()),
        ("Get", peer.get(get(&coder, "c1")).await.err()),
        ("SendInput", peer.send_input(input(&coder)).await.err()),
        (
            "GetBlob",
            peer.get_blob(blob(&coder, &desk_blob.hash)).await.err(),
        ),
        ("PutBlob", peer.put_blob(put(&coder)).await.err()),
        (
            "Diff",
            peer.diff(DiffRequest {
                agent_id: coder.clone(),
                base: None,
                with_patch: false,
            })
            .await
            .err(),
        ),
        (
            "RenameAgent",
            peer.rename_agent(RenameAgentRequest {
                agent_id: coder.clone(),
                name: "taken".to_owned(),
            })
            .await
            .err(),
        ),
        (
            "StopAgent",
            peer.stop_agent(StopAgentRequest {
                agent_id: coder.clone(),
                mode: StopMode::Kill.into(),
            })
            .await
            .err(),
        ),
        (
            "ResumeAgent",
            peer.resume_agent(ResumeAgentRequest {
                agent_id: coder.clone(),
                initial_prompt: None,
            })
            .await
            .err(),
        ),
        (
            "DeleteAgent",
            peer.delete_agent(DeleteAgentRequest {
                agent_id: coder.clone(),
            })
            .await
            .err(),
        ),
    ];
    for (call, refused) in refusals {
        let refused = refused.unwrap_or_else(|| panic!("{call} on a replica was answered"));
        assert_eq!(refused.code(), Code::NotFound, "{call}: {refused:?}");
    }
    assert!(
        pair.coder.inputs().is_empty(),
        "nothing reached the desk's agent through the laptop"
    );
    pair.stop().await;
}

/// The hosts and agents an inventory stream carried.
fn listed(events: &[InventoryEvent]) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut hosts = Vec::new();
    let mut agents = Vec::new();
    for event in events {
        match &event.of {
            Some(inventory_event::Of::Host(host)) => hosts.push(host.host_id.clone()),
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

#[tokio::test(flavor = "multi_thread")]
async fn inventory_is_origin_only() {
    let mut pair = Pair::new().await;
    let laptop = pair.laptop_runtime.host().as_bytes().to_vec();
    let notes = pair.notes.id.as_bytes().to_vec();
    let mut stream = pair
        .laptop_peer()
        .await
        .subscribe_inventory(Empty {})
        .await
        .unwrap()
        .into_inner();
    let mut seen = Vec::new();
    let next = async |stream: &mut tonic::Streaming<InventoryEvent>| {
        tokio::time::timeout(PATIENCE, stream.message())
            .await
            .expect("the inventory stream answers")
            .unwrap()
            .expect("the inventory stream stays open")
    };
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
    // stream.
    pair.coder.append(&snapshot(Phase::Working, &[], 2_000));
    pair.desk_runtime.ingest(pair.coder.id).await.unwrap();
    let coder = pair.coder.id.as_bytes().to_vec();
    until(
        "the laptop's replica to follow the desk's agent",
        || async {
            let rows = pair.laptop_runtime.store().await.agents().unwrap();
            match rows.iter().find(|row| row.agent.agent == coder) {
                Some(row) if row.phase == Phase::Working as i32 => Ok(()),
                Some(row) => Err(format!("phase {}", row.phase)),
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
                if agent.agent_id == notes && agent.phase == Phase::Working as i32
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
