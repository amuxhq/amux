use std::net::SocketAddr;
use std::str::FromStr;

use store::Sqlite;

use super::*;
use crate::payload::ClientRequest;
use crate::{Input, Job, JobId, StartOptions, StepBudget};

const PEER: Uuid = Uuid::from_u128(2);
const OTHER: Uuid = Uuid::from_u128(3);

fn addr(text: &str) -> SocketAddr {
    SocketAddr::from_str(text).unwrap()
}

#[test]
fn trust_store_pairing_upsert_inserts_and_deduplicates_reachability() {
    let mut store = TrustStore::default();
    let target = "workstation".to_string();
    let ssh = Reachability::Ssh(target.clone());

    let upsert = |store: &mut TrustStore, name: &str, reachability: Reachability| {
        store
            .upsert_paired_peer(PEER, vec![9; 32], name.to_string(), reachability)
            .unwrap()
    };
    assert_eq!(
        upsert(&mut store, "first", Reachability::Cloud),
        PairingUpdate::Inserted
    );
    assert_eq!(
        upsert(&mut store, "renamed", Reachability::Cloud),
        PairingUpdate::Updated
    );
    assert_eq!(
        upsert(&mut store, "renamed", ssh.clone()),
        PairingUpdate::Updated
    );

    let entry = store.entry(PEER).unwrap();
    assert_eq!(entry.name, "renamed");
    assert_eq!(entry.pubkey, vec![9; 32]);
    assert!(entry.on_account);
    assert_eq!(entry.ssh, Some(target));
}

#[test]
fn trust_store_pairing_rejects_duplicate_pubkey_for_different_host() {
    let mut store = TrustStore::default();
    store
        .upsert_paired_peer(PEER, vec![9; 32], "first".into(), Reachability::Cloud)
        .unwrap();

    assert_eq!(
        store.upsert_paired_peer(OTHER, vec![9; 32], "second".into(), Reachability::Cloud),
        Err(TrustError::DuplicatePubkey(PEER))
    );
    assert!(store.entry(OTHER).is_none());
}

#[test]
fn trust_store_pairing_refuses_a_paired_host_under_another_key() {
    let mut store = TrustStore::default();
    store
        .upsert_paired_peer(PEER, vec![7; 32], "old".into(), Reachability::Cloud)
        .unwrap();
    let direct = Reachability::Direct {
        addrs: vec![addr("127.0.0.1:9000")],
    };

    assert_eq!(
        store.upsert_paired_peer(PEER, vec![8; 32], "new".into(), direct),
        Err(TrustError::PairedUnderAnotherKey {
            host: PEER,
            name: "old".into(),
        })
    );
    let entry = store.entry(PEER).unwrap();
    assert_eq!(entry.name, "old");
    assert_eq!(entry.pubkey, vec![7; 32]);
    assert!(entry.direct.is_empty());
    assert_eq!(store.pubkey_for_host(PEER), Some([7; 32].as_slice()));
    assert_eq!(store.host_id_for_pubkey(&[7; 32]), Some(PEER));
    assert_eq!(store.host_id_for_pubkey(&[8; 32]), None);
}

fn a_full_entry() -> TrustEntry {
    TrustEntry {
        pubkey: vec![9; 32],
        name: "peer".to_string(),
        push_key: None,
        on_account: true,
        direct: vec![addr("192.0.2.7:7001"), addr("[::1]:4000")],
        ssh: Some("workstation".to_string()),
        signed_in: Some(false),
        platform: wire::Platform::Linux,
        listed: vec![crate::payload::PeerAddress {
            addr: "192.0.2.7:7000".parse().unwrap(),
            source: crate::payload::Source::Listed,
        }],
    }
}

#[test]
fn a_paired_host_round_trips_through_the_store() {
    let mut trust = TrustStore::default();
    trust.insert_for_test(PEER, a_full_entry());
    let mut store = Sqlite::open_in_memory(vec![1; 16]).unwrap();
    trust.write_for_test(&mut store);
    assert_eq!(TrustStore::load(&store), trust);
}

#[test]
fn a_row_with_a_bad_key_is_left_out_and_the_rest_read() {
    let mut store = Sqlite::open_in_memory(vec![1; 16]).unwrap();
    let mut trust = TrustStore::default();
    trust.insert_for_test(PEER, a_full_entry());
    trust.write_for_test(&mut store);
    let mut bad = a_full_entry().to_row(OTHER);
    bad.pubkey = vec![1, 2, 3];
    store.put_trusted_host(&bad, Durability::Normal).unwrap();
    assert_eq!(TrustStore::load(&store), trust);
}

#[test]
fn trust_store_remembers_a_peers_signed_in_fact_and_platform() {
    let mut store = TrustStore::default();
    store
        .upsert_paired_peer(PEER, vec![9; 32], "peer".into(), Reachability::Cloud)
        .unwrap();

    assert!(store.remember_signed_in(PEER, Some(false)));
    assert!(!store.remember_signed_in(PEER, Some(false)));
    assert!(!store.remember_signed_in(OTHER, Some(true)));
    // An offline peer still says what kind of machine it is: the last
    // platform it announced is kept, and one announcing none keeps it.
    assert!(store.remember_platform(PEER, wire::Platform::Linux));
    assert!(!store.remember_platform(PEER, wire::Platform::Linux));
    assert!(!store.remember_platform(PEER, wire::Platform::Unspecified));
    assert!(!store.remember_platform(OTHER, wire::Platform::Macos));
    let entry = store.entry(PEER).unwrap();
    assert_eq!(entry.signed_in, Some(false));
    assert_eq!(entry.platform, wire::Platform::Linux);
}

fn daemon() -> Daemon {
    let store = Sqlite::open_in_memory(vec![1; 16]).expect("a store opens");
    let mut daemon = Daemon::new(
        store,
        StartOptions {
            budget: StepBudget::UNLIMITED,
            ..StartOptions::default()
        },
    );
    step(&mut daemon, Input::Started);
    daemon
}

fn step(daemon: &mut Daemon, input: Input) -> Vec<Output> {
    let mut out = Vec::new();
    daemon.step(input, &mut out);
    out
}

fn change(daemon: &mut Daemon, call: u64, change: TrustChange) -> Vec<Output> {
    step(
        daemon,
        Input::ClientCall {
            call: CallId(call),
            request: ClientRequest::ChangeTrust(change),
        },
    )
}

fn pair(pubkey: u8) -> TrustChange {
    TrustChange::Pair {
        host: PEER,
        pubkey: vec![pubkey; 32],
        name: "peer".into(),
        reachability: Some(Reachability::Cloud),
    }
}

fn job(out: &[Output]) -> Option<(JobId, Job)> {
    out.iter().find_map(|output| match output {
        Output::Job { id, op } => Some((*id, op.clone())),
        _ => None,
    })
}

fn ended(out: &[Output], call: u64) -> Option<Status> {
    out.iter().find_map(|output| match output {
        Output::EndCall {
            call: ended,
            status,
        } if *ended == CallId(call) => Some(status.clone()),
        _ => None,
    })
}

fn published(out: &[Output]) -> Option<&TrustStore> {
    out.iter().rev().find_map(|output| match output {
        Output::Shell(ShellWork::Trust { store, .. }) => Some(&**store),
        _ => None,
    })
}

/// What the profile store holds of the paired hosts now.
fn held(daemon: &Daemon) -> TrustStore {
    TrustStore::load(daemon.store())
}

/// Makes every write of a paired host fail, as a full drive would.
fn refuse_writes(daemon: &Daemon) {
    daemon
        .store()
        .connection()
        .execute_batch(
            "CREATE TRIGGER refuse_insert BEFORE INSERT ON trusted_hosts \
             BEGIN SELECT RAISE(ABORT, 'disk full'); END; \
             CREATE TRIGGER refuse_update BEFORE UPDATE ON trusted_hosts \
             BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
        )
        .unwrap();
}

/// A change is committed, served and answered in the step that asks for
/// it.
#[test]
fn a_change_is_committed_served_and_answered_in_its_step() {
    let mut daemon = daemon();
    let out = change(&mut daemon, 1, pair(7));
    assert_eq!(job(&out), None, "nothing is left for a job to write");
    assert_eq!(held(&daemon).entry(PEER).unwrap().pubkey, vec![7; 32]);
    assert_eq!(daemon.trust(), &held(&daemon));
    assert_eq!(published(&out).unwrap(), daemon.trust());
    assert!(out.contains(&Output::Answer {
        call: CallId(1),
        response: Response::Trust(TrustChanged::Paired(PairingUpdate::Inserted)),
    }));
    assert_eq!(ended(&out, 1), Some(Status::Ok));

    let out = change(&mut daemon, 2, TrustChange::Remove { host: PEER });
    assert!(held(&daemon).is_empty() && daemon.trust().is_empty());
    assert!(out.iter().any(|output| matches!(
        output,
        Output::Answer {
            response: Response::Trust(TrustChanged::Removed(_)),
            ..
        }
    )));
}

#[test]
fn a_change_the_store_refused_never_shows() {
    let mut daemon = daemon();
    refuse_writes(&daemon);
    let out = change(&mut daemon, 1, pair(7));
    assert!(daemon.trust().is_empty() && held(&daemon).is_empty());
    assert!(matches!(
        ended(&out, 1),
        Some(Status::Failed(CallError::Trust(TrustError::Write(error)))) if error.contains("disk full")
    ));
}

#[test]
fn forgetting_a_host_that_is_not_paired_is_refused_without_a_write() {
    let mut daemon = daemon();
    let before = daemon.store().changes();
    let out = change(&mut daemon, 1, TrustChange::Remove { host: PEER });
    assert_eq!(daemon.store().changes(), before);
    assert_eq!(
        ended(&out, 1),
        Some(Status::Failed(CallError::Trust(TrustError::NotTrusted(
            PEER
        ))))
    );
}

#[test]
fn a_remembered_fact_is_kept_and_one_that_did_not_move_writes_nothing() {
    let mut daemon = daemon();
    change(&mut daemon, 1, pair(7));
    let before = daemon.store().changes();
    let remember = TrustChange::Remember {
        host: PEER,
        signed_in: None,
        platform: wire::Platform::Unspecified,
    };
    step(&mut daemon, Input::ChangeTrust(remember));
    assert_eq!(daemon.store().changes(), before);
    let addrs = TrustChange::DirectAddrs {
        host: PEER,
        addrs: vec![addr("10.0.0.2:4000")],
    };
    step(&mut daemon, Input::ChangeTrust(addrs.clone()));
    assert_eq!(
        held(&daemon).entry(PEER).unwrap().direct,
        vec![addr("10.0.0.2:4000")]
    );
    let before = daemon.store().changes();
    step(&mut daemon, Input::ChangeTrust(addrs));
    assert_eq!(daemon.store().changes(), before);
}

/// Pairing a paired host under another key is refused, and the old key
/// stays trusted and held.
#[test]
fn pairing_a_host_under_another_key_is_refused_without_a_write() {
    let mut daemon = daemon();
    change(&mut daemon, 1, pair(7));
    let before = daemon.store().changes();

    let out = change(&mut daemon, 2, pair(8));
    assert_eq!(
        ended(&out, 2),
        Some(Status::Failed(CallError::Trust(
            TrustError::PairedUnderAnotherKey {
                host: PEER,
                name: "peer".into(),
            }
        )))
    );
    assert_eq!(daemon.store().changes(), before);
    assert_eq!(
        daemon.trust().pubkey_for_host(PEER),
        Some([7; 32].as_slice())
    );
    assert_eq!(held(&daemon), *daemon.trust());
}

/// A change asked for while the WAL moves into the main file waits until
/// the checkpoint has landed, so the main file and the core it is
/// serialized beside agree on the paired hosts.
#[test]
fn a_change_waits_out_a_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let store = Sqlite::open(&path, vec![1; 16]).unwrap();
    store.leave_checkpoints_to_owner().unwrap();
    let mut daemon = Daemon::new(
        store,
        StartOptions {
            budget: StepBudget::UNLIMITED,
            ..StartOptions::default()
        },
    );
    step(&mut daemon, Input::Started);
    change(&mut daemon, 1, pair(7));
    let started = step(
        &mut daemon,
        Input::Timer {
            id: crate::TimerId::Checkpoint,
        },
    );
    let Some((wal, Job::CheckpointWal)) = job(&started) else {
        panic!("the timer starts the WAL job: {started:?}");
    };
    let addrs = TrustChange::DirectAddrs {
        host: PEER,
        addrs: vec![addr("10.0.0.2:4000")],
    };
    step(&mut daemon, Input::ChangeTrust(addrs));
    assert!(held(&daemon).entry(PEER).unwrap().direct.is_empty());
    Sqlite::checkpoint_wal(&path).unwrap();
    step(
        &mut daemon,
        Input::JobDone {
            job: wal,
            result: crate::payload::JobResult::Done,
        },
    );
    assert!(
        held(&daemon).entry(PEER).unwrap().direct.is_empty(),
        "not in the step that serialized the core"
    );
    step(&mut daemon, Input::Checkpointed);
    assert_eq!(
        held(&daemon).entry(PEER).unwrap().direct,
        vec![addr("10.0.0.2:4000")]
    );
}

/// The paired hosts come back from the store a core is restored beside.
#[test]
fn a_restored_core_reads_its_paired_hosts_from_the_store() {
    let mut daemon = daemon();
    change(&mut daemon, 1, pair(7));
    let checkpoint = daemon.checkpoint();
    let store = Sqlite::restore(&daemon.store().dump().unwrap()).unwrap();
    let restored = Daemon::restore(&checkpoint, store, Default::default()).expect("it restores");
    assert_eq!(restored.checkpoint(), checkpoint);
    assert_eq!(restored.trust, daemon.trust);
    assert_eq!(
        restored.trust().pubkey_for_host(PEER),
        Some([7; 32].as_slice())
    );
}
