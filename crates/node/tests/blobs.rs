//! Blobs: PutBlob writes into the named agent's directory by
//! temp-and-rename, GetBlob reads own and replica files, and a write that
//! fails is an error to the caller that leaves nothing behind.

mod support;

use std::path::Path;

use node::BlobError;
use sha2::{Digest as _, Sha256};
use store::{AgentKey, AgentRow};
use support::synthetic::*;
use support::*;
use uuid::Uuid;
use wire::{GetBlobRequest, PutBlobRequest};

const SEGMENTS: u64 = 1 << 20;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn put(agent: &SyntheticAgent, name: &str, bytes: &[u8]) -> PutBlobRequest {
    PutBlobRequest {
        agent_id: agent.id.as_bytes().to_vec(),
        name: name.to_owned(),
        mime: "image/png".to_owned(),
        bytes: bytes.to_vec(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn put_blob_writes_the_agents_directory_and_get_blob_reads_it_back() {
    let install = Install::new();
    let agent = SyntheticAgent::new(&install, "photos", SEGMENTS);
    agent.register_offline(&install);
    let daemon = install.start("boot-1", quiet_launch()).await;
    let runtime = runtime(&daemon, &install);

    let photo = b"\x89PNG not really a picture".to_vec();
    let blob = runtime
        .put_blob(put(&agent, "photo.png", &photo))
        .await
        .unwrap();
    let hash = Sha256::digest(&photo).to_vec();
    assert_eq!(blob.hash, hash, "named by the content's hash");
    assert_eq!(
        (blob.name.as_str(), blob.mime.as_str(), blob.size),
        ("photo.png", "image/png", photo.len() as u64),
        "the reference carries what the caller said, and the size"
    );
    let blobs = agent.dir.join(agent_dir::BLOBS);
    assert_eq!(
        files_in(&blobs),
        vec![hex(&hash)],
        "one file, no temporary left"
    );
    assert_eq!(std::fs::read(blobs.join(hex(&hash))).unwrap(), photo);

    // The same bytes again are the same file.
    let again = runtime
        .put_blob(put(&agent, "again.png", &photo))
        .await
        .unwrap();
    assert_eq!(again.hash, hash);
    assert_eq!(files_in(&blobs), vec![hex(&hash)]);

    let read = runtime
        .get_blob(GetBlobRequest {
            agent_id: agent.id.as_bytes().to_vec(),
            hash: hash.clone(),
        })
        .await
        .unwrap();
    assert_eq!(read.bytes, photo);

    let missing = runtime
        .get_blob(GetBlobRequest {
            agent_id: agent.id.as_bytes().to_vec(),
            hash: vec![0; 32],
        })
        .await
        .unwrap_err();
    assert!(matches!(missing, BlobError::NoBlob(_)), "{missing}");
    let unknown = runtime
        .put_blob(PutBlobRequest {
            agent_id: Uuid::new_v4().as_bytes().to_vec(),
            ..put(&agent, "x", b"x")
        })
        .await
        .unwrap_err();
    assert!(matches!(unknown, BlobError::NoAgent), "{unknown}");
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// A stored blob over the attachment limit is refused, naming the limit,
/// before its bytes are read: an agent's attachment is fetched by GetBlob,
/// on this host or from another, and a response that size cannot be sent.
#[tokio::test(flavor = "multi_thread")]
async fn get_blob_refuses_a_stored_blob_over_the_attachment_limit() {
    let install = Install::new();
    let agent = SyntheticAgent::new(&install, "dumps", SEGMENTS);
    agent.register_offline(&install);
    let daemon = install.start("boot-1", quiet_launch()).await;
    let runtime = runtime(&daemon, &install);
    let blobs = agent.dir.join(agent_dir::BLOBS);
    std::fs::create_dir_all(&blobs).unwrap();
    let hash = vec![7u8; 32];
    std::fs::File::create(blobs.join(hex(&hash)))
        .unwrap()
        .set_len(wire::ATTACHMENT_SIZE_LIMIT as u64 + 1)
        .unwrap();
    let refused = runtime
        .get_blob(GetBlobRequest {
            agent_id: agent.id.as_bytes().to_vec(),
            hash,
        })
        .await
        .unwrap_err();
    assert!(matches!(refused, BlobError::TooLarge { .. }), "{refused}");
    assert!(refused.to_string().contains("64 MB limit"), "{refused}");
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn get_blob_reads_a_replica_file_and_put_blob_refuses_a_replica() {
    let install = Install::new();
    let daemon = install.start("boot-1", quiet_launch()).await;
    let runtime = runtime(&daemon, &install);
    // A replica exists only of a trusted host: the daemon drops every
    // other host's replicas when it starts, which can land after the row.
    let elsewhere = Install::new();
    let peer_daemon = elsewhere.start("boot-1", quiet_launch()).await;
    let peer_edge = support::runtime(&peer_daemon, &elsewhere).edge().unwrap();
    runtime.edge().unwrap().trust(&peer_edge).await.unwrap();
    let (peer, agent) = (peer_edge.host_id(), Uuid::new_v4());
    let key = AgentKey::new(peer.as_bytes().to_vec(), agent.as_bytes().to_vec());
    runtime
        .store()
        .await
        .put_agent(&AgentRow::new(key, KIND, "/elsewhere"))
        .unwrap();

    let bytes = b"fetched from the peer".to_vec();
    let hash = Sha256::digest(&bytes).to_vec();
    let dir = install
        .profile_dir()
        .join(node::REPLICAS)
        .join(peer.to_string())
        .join(node::AGENTS)
        .join(agent.to_string())
        .join(agent_dir::BLOBS);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(hex(&hash)), &bytes).unwrap();

    let read = runtime
        .get_blob(GetBlobRequest {
            agent_id: agent.as_bytes().to_vec(),
            hash,
        })
        .await
        .unwrap();
    assert_eq!(read.bytes, bytes);
    let refused = runtime
        .put_blob(PutBlobRequest {
            agent_id: agent.as_bytes().to_vec(),
            name: "x".into(),
            mime: "text/plain".into(),
            bytes: b"x".to_vec(),
        })
        .await
        .unwrap_err();
    assert!(matches!(refused, BlobError::NotOwn), "{refused}");
    drop(runtime);
    daemon.shutdown().await.unwrap();
    drop(peer_edge);
    peer_daemon.shutdown().await.unwrap();
}

/// A full disk cannot be arranged portably; a directory where the file must
/// go fails the same write at the same step, the rename after the bytes.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_write_is_an_error_to_the_caller_and_leaves_nothing_behind() {
    let install = Install::new();
    let agent = SyntheticAgent::new(&install, "full", SEGMENTS);
    agent.register_offline(&install);
    let daemon = install.start("boot-1", quiet_launch()).await;
    let runtime = runtime(&daemon, &install);

    let bytes = b"will not fit".to_vec();
    let name = hex(&Sha256::digest(&bytes));
    let blobs = agent.dir.join(agent_dir::BLOBS);
    std::fs::create_dir_all(blobs.join(&name).join("occupied")).unwrap();

    let error = runtime
        .put_blob(put(&agent, "big.bin", &bytes))
        .await
        .unwrap_err();
    assert!(matches!(error, BlobError::Write(_)), "{error}");
    println!("PutBlob answered: {}", error.to_wire().message);
    assert_eq!(files_in(&blobs), vec![name], "no temporary file is left");
    drop(runtime);
    daemon.shutdown().await.unwrap();
}
