//! An agent's catalogue is a blob of its directory, read with GetBlob under
//! the hash its snapshot names. A paired host forwards a read for its
//! peer's agent to that peer and keeps the exact bytes, so it answers again
//! with the peer away and a catalogue it cannot fully decode keeps its
//! hash; a hash that changes mid-session is read anew; replica retention
//! never evicts the catalogue a replica's current snapshot names.

mod support;

use std::path::{Path, PathBuf};

use prost::Message as _;
use sha2::{Digest as _, Sha256};
use support::synthetic::*;
use support::*;
use tonic::Code;
use wire::client_service_server::ClientService as _;
use wire::{
    BlobRef, Catalogue, GetBlobRequest, GetCatalogueRequest, OfferedModel, Phase, SessionEvent,
    session_event,
};

const SEGMENTS: u64 = 1 << 20;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A catalogue offering `model`, encoded, with the SHA-256 of exactly those
/// bytes.
fn offering(model: &str) -> (Vec<u8>, Vec<u8>) {
    let bytes = Catalogue {
        models: vec![OfferedModel {
            value: model.to_owned(),
            display_name: model.to_owned(),
            ..OfferedModel::default()
        }],
        ..Catalogue::default()
    }
    .encode_to_vec();
    (Sha256::digest(&bytes).to_vec(), bytes)
}

/// What the agent process does: the catalogue's bytes as a blob, then the
/// snapshot that names them.
fn offer(agent: &mut SyntheticAgent, (hash, bytes): &(Vec<u8>, Vec<u8>), at_ms: i64) {
    let dir = agent.dir.join(agent_dir::BLOBS);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(hex(hash)), bytes).unwrap();
    let mut step = snapshot(Phase::Idle, &[], at_ms);
    step.snapshot.as_mut().unwrap().catalogue = Some(hash.clone());
    agent.append(&step);
}

fn blob(agent: &SyntheticAgent, hash: &[u8]) -> tonic::Request<GetBlobRequest> {
    tonic::Request::new(GetBlobRequest {
        agent_id: agent.id.as_bytes().to_vec(),
        hash: hash.to_vec(),
    })
}

async fn read(api: &node::ClientApi, agent: &SyntheticAgent, hash: &[u8]) -> Vec<u8> {
    api.get_blob(blob(agent, hash))
        .await
        .unwrap()
        .into_inner()
        .bytes
}

fn names(event: &SessionEvent, hash: &[u8]) -> bool {
    matches!(
        &event.of,
        Some(session_event::Of::Snapshot(snapshot)) if snapshot.catalogue.as_deref() == Some(hash)
    )
}

fn kept_copy(laptop: &Install, desk: uuid::Uuid, agent: uuid::Uuid, hash: &[u8]) -> PathBuf {
    laptop
        .profile_dir()
        .join(node::REPLICAS)
        .join(desk.to_string())
        .join(node::AGENTS)
        .join(agent.to_string())
        .join(agent_dir::BLOBS)
        .join(hex(hash))
}

fn is_file(path: &Path) -> bool {
    path.metadata().is_ok_and(|meta| meta.is_file())
}

/// A desk running `agent` and a laptop paired with it, linked, with the
/// laptop following the agent as a chat on it would.
struct Pair {
    desk: Install,
    laptop: Install,
    desk_daemon: node::Daemon,
    laptop_daemon: node::Daemon,
    desk_runtime: std::sync::Arc<node::ProfileRuntime>,
    laptop_runtime: std::sync::Arc<node::ProfileRuntime>,
    desk_edge: std::sync::Arc<node::Edge>,
    laptop_edge: std::sync::Arc<node::Edge>,
    link: Option<node::LoopbackLink>,
    subscription: node::Subscription,
    seen: Vec<SessionEvent>,
}

impl Pair {
    async fn start(agent: &mut SyntheticAgent, desk: Install, launch: node::Launch) -> Self {
        agent.go_live();
        let desk_daemon = desk.start("boot-1", quiet_launch()).await;
        let desk_runtime = runtime(&desk_daemon, &desk);
        let laptop = Install::new();
        let laptop_daemon = laptop.start("boot-1", launch).await;
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
        let subscription = until("the laptop to list the desk's agent", || {
            laptop_runtime.subscribe_session(agent.id.as_bytes(), 10)
        })
        .await
        .unwrap();
        Self {
            desk,
            laptop,
            desk_daemon,
            laptop_daemon,
            desk_runtime,
            laptop_runtime,
            desk_edge,
            laptop_edge,
            link: Some(link),
            subscription,
            seen: Vec::new(),
        }
    }

    /// Waits until the laptop's copy of the agent names `hash`.
    async fn until_named(&mut self, what: &str, hash: &[u8]) {
        read_until(&mut self.subscription, &mut self.seen, what, |seen| {
            seen.iter().any(|event| names(event, hash))
        })
        .await;
    }

    fn kept(&self, agent: &SyntheticAgent, hash: &[u8]) -> PathBuf {
        kept_copy(&self.laptop, self.desk_runtime.host(), agent.id, hash)
    }

    async fn stop(self, mut agent: SyntheticAgent) {
        drop((self.subscription, self.link));
        agent.die().await;
        drop((
            self.desk_runtime,
            self.laptop_runtime,
            self.desk_edge,
            self.laptop_edge,
        ));
        self.desk_daemon.shutdown().await.unwrap();
        self.laptop_daemon.shutdown().await.unwrap();
        drop((self.desk, self.laptop));
    }
}

fn desk_agent() -> (Install, SyntheticAgent) {
    let desk = Install::new();
    let agent = SyntheticAgent::new(&desk, "coder", SEGMENTS);
    agent.register_offline(&desk);
    (desk, agent)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paired_host_reads_its_peers_catalogue_keeps_a_copy_and_follows_a_new_hash() {
    let (desk, mut agent) = desk_agent();
    let first = offering("gpt-1");
    offer(&mut agent, &first, 1_000);
    let laptop_launch = Install::new().launch("idle", Vec::new());
    let mut pair = Pair::start(&mut agent, desk, laptop_launch).await;

    // The agent's own host reads the blob its snapshot names.
    let on_desk = node::ClientApi::new(&pair.desk_runtime, None);
    assert_eq!(read(&on_desk, &agent, &first.0).await, first.1);

    pair.until_named("the first catalogue", &first.0).await;
    let on_laptop = node::ClientApi::new(&pair.laptop_runtime, None);
    assert_eq!(
        read(&on_laptop, &agent, &first.0).await,
        first.1,
        "the desk answers through the laptop"
    );
    assert!(
        is_file(&pair.kept(&agent, &first.0)),
        "the laptop keeps what it read"
    );

    // Mid-session the agent offers something else: the new hash is read
    // anew, and the old copy stays.
    let second = offering("gpt-2");
    offer(&mut agent, &second, 2_000);
    agent.nudge().await;
    pair.until_named("the second catalogue", &second.0).await;
    assert_eq!(read(&on_laptop, &agent, &second.0).await, second.1);
    assert!(is_file(&pair.kept(&agent, &second.0)));
    assert!(is_file(&pair.kept(&agent, &first.0)));

    // With the desk away the laptop still answers from its copies, so each
    // hash was fetched once.
    drop(pair.link.take());
    assert_eq!(
        read(&on_laptop, &agent, &second.0).await,
        second.1,
        "the copy answers with the desk away"
    );
    assert_eq!(read(&on_laptop, &agent, &first.0).await, first.1);
    // A hash the laptop never read has no answer with the desk away.
    on_laptop
        .get_blob(blob(&agent, &offering("gpt-3").0))
        .await
        .unwrap_err();

    drop((on_desk, on_laptop));
    pair.stop(agent).await;
}

/// A catalogue written by a newer agent, with a field this daemon's
/// Catalogue does not have, reaches the reader byte for byte: what the
/// replica keeps and serves still hashes to what the snapshot names.
#[tokio::test(flavor = "multi_thread")]
async fn catalogue_with_unknown_field_keeps_its_hash_through_a_replica() {
    let (desk, mut agent) = desk_agent();
    let (_, mut bytes) = offering("gpt-1");
    // Field 99, a varint 1: unknown to this build's Catalogue.
    bytes.extend_from_slice(&[0x98, 0x06, 0x01]);
    assert_ne!(
        Catalogue::decode(bytes.as_slice()).unwrap().encode_to_vec(),
        bytes,
        "re-encoding would drop the unknown field"
    );
    let newer = (Sha256::digest(&bytes).to_vec(), bytes);
    offer(&mut agent, &newer, 1_000);
    let laptop_launch = Install::new().launch("idle", Vec::new());
    let mut pair = Pair::start(&mut agent, desk, laptop_launch).await;

    pair.until_named("the newer catalogue", &newer.0).await;
    let on_laptop = node::ClientApi::new(&pair.laptop_runtime, None);
    let fetched = read(&on_laptop, &agent, &newer.0).await;
    assert_eq!(fetched, newer.1, "the bytes pass through unchanged");
    assert_eq!(Sha256::digest(&fetched).to_vec(), newer.0);
    assert_eq!(
        std::fs::read(pair.kept(&agent, &newer.0)).unwrap(),
        newer.1,
        "the kept copy is the origin's bytes"
    );
    let offered = Catalogue::decode(fetched.as_slice()).unwrap();
    assert_eq!(
        offered.models[0].value, "gpt-1",
        "what it knows still reads"
    );

    drop(on_laptop);
    pair.stop(agent).await;
}

/// Under blob budget pressure, the replica sweep evicts what was read
/// least recently except the catalogue the replica's current snapshot
/// names: without it the agent's picker would lose its offers with the
/// origin away. A catalogue no longer named is evicted like any blob.
#[tokio::test(flavor = "multi_thread")]
async fn current_catalogue_is_pinned() {
    let (desk, mut agent) = desk_agent();
    let first = offering("gpt-1");
    offer(&mut agent, &first, 1_000);
    let laptop = Install::new();
    let mut launch = laptop.launch("idle", Vec::new());
    launch.replica_blob_budget_bytes = 0;
    let mut pair = Pair::start(&mut agent, desk, launch).await;
    let on_desk = node::ClientApi::new(&pair.desk_runtime, None);
    let on_laptop = node::ClientApi::new(&pair.laptop_runtime, None);

    pair.until_named("the first catalogue", &first.0).await;
    read(&on_laptop, &agent, &first.0).await;
    let attachment: BlobRef = on_desk
        .put_blob(tonic::Request::new(wire::PutBlobRequest {
            agent_id: agent.id.as_bytes().to_vec(),
            name: "notes.txt".into(),
            mime: "text/plain".into(),
            bytes: b"an attachment the laptop read".to_vec(),
        }))
        .await
        .unwrap()
        .into_inner();
    read(&on_laptop, &agent, &attachment.hash).await;
    assert!(is_file(&pair.kept(&agent, &attachment.hash)));

    let removed = pair
        .laptop_runtime
        .sweep_retention()
        .await
        .unwrap()
        .replica_blobs;
    assert!(
        is_file(&pair.kept(&agent, &first.0)),
        "the named catalogue survives a sweep over budget"
    );
    assert!(!is_file(&pair.kept(&agent, &attachment.hash)));
    assert!(removed.contains(&pair.kept(&agent, &attachment.hash)));

    // The agent offers something else: the old catalogue loses its pin.
    let second = offering("gpt-2");
    offer(&mut agent, &second, 2_000);
    agent.nudge().await;
    pair.until_named("the second catalogue", &second.0).await;
    read(&on_laptop, &agent, &second.0).await;
    pair.laptop_runtime.sweep_retention().await.unwrap();
    assert!(
        is_file(&pair.kept(&agent, &second.0)),
        "the new one is pinned"
    );
    assert!(
        !is_file(&pair.kept(&agent, &first.0)),
        "the one no longer named goes"
    );

    // With the desk away the picker still reads what the agent offers.
    drop(pair.link.take());
    assert_eq!(read(&on_laptop, &agent, &second.0).await, second.1);

    drop((on_desk, on_laptop, laptop));
    pair.stop(agent).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_catalogue_is_not_found() {
    let (install, mut agent) = desk_agent();
    let named = offering("gpt-1");
    offer(&mut agent, &named, 1_000);
    let daemon = install.start("boot-1", quiet_launch()).await;
    let runtime = runtime(&daemon, &install);
    let api = node::ClientApi::new(&runtime, None);

    let unknown = api
        .get_blob(blob(&agent, &offering("gpt-2").0))
        .await
        .unwrap_err();
    assert_eq!(unknown.code(), Code::NotFound, "{unknown:?}");
    let no_agent = api
        .get_blob(tonic::Request::new(GetBlobRequest {
            agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
            hash: named.0.clone(),
        }))
        .await
        .unwrap_err();
    assert_eq!(no_agent.code(), Code::NotFound, "{no_agent:?}");
    let no_host = api
        .get_catalogue(tonic::Request::new(GetCatalogueRequest::default()))
        .await
        .unwrap_err();
    assert_eq!(no_host.code(), Code::InvalidArgument, "{no_host:?}");
    let host = api
        .get_catalogue(tonic::Request::new(GetCatalogueRequest {
            host_id: runtime.host().as_bytes().to_vec(),
            provider: wire::Provider::Codex as i32,
        }))
        .await
        .unwrap_err();
    // This host cannot start the helper that would ask its Codex.
    assert_eq!(host.code(), Code::Unavailable, "{host:?}");

    drop((api, runtime));
    daemon.shutdown().await.unwrap();
}
