use std::net::SocketAddr;
use std::str::FromStr;

use store::Sqlite;

use super::*;
use crate::payload::ClientRequest;
use crate::{Input, StartOptions, StepBudget};

const PEER: Uuid = Uuid::from_u128(2);
const OTHER: Uuid = Uuid::from_u128(3);

fn at(seconds: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(seconds, 0).unwrap()
}

fn addr(text: &str) -> SocketAddr {
    SocketAddr::from_str(text).unwrap()
}

#[test]
fn trust_store_pairing_upsert_inserts_and_deduplicates_reachability() {
    let mut store = TrustStore::default();
    let ssh = Reachability::Ssh {
        target: "workstation".to_string(),
        profile: Uuid::from_u128(42),
    };

    let upsert = |store: &mut TrustStore, name: &str, reachability: Reachability, when| {
        store
            .upsert_paired_peer(PEER, vec![9; 32], name.to_string(), reachability, at(when))
            .unwrap()
    };
    assert_eq!(
        upsert(&mut store, "first", Reachability::Cloud, 200),
        PairingUpdate::Inserted
    );
    assert_eq!(
        upsert(&mut store, "renamed", Reachability::Cloud, 300),
        PairingUpdate::Updated
    );
    assert_eq!(
        upsert(&mut store, "renamed", ssh.clone(), 400),
        PairingUpdate::Updated
    );

    let entry = store.entry(PEER).unwrap();
    assert_eq!(entry.name, "renamed");
    assert_eq!(entry.pubkey, vec![9; 32]);
    assert_eq!(entry.reachabilities, vec![Reachability::Cloud, ssh]);
}

#[test]
fn trust_store_pairing_rejects_duplicate_pubkey_for_different_host() {
    let mut store = TrustStore::default();
    store
        .upsert_paired_peer(
            PEER,
            vec![9; 32],
            "first".into(),
            Reachability::Cloud,
            at(200),
        )
        .unwrap();

    assert_eq!(
        store.upsert_paired_peer(
            OTHER,
            vec![9; 32],
            "second".into(),
            Reachability::Cloud,
            at(300)
        ),
        Err(TrustError::DuplicatePubkey(PEER))
    );
    assert!(store.entry(OTHER).is_none());
}

#[test]
fn trust_store_pairing_upsert_requires_teardown_before_pubkey_replacement() {
    let mut store = TrustStore::default();
    store
        .upsert_paired_peer(
            PEER,
            vec![7; 32],
            "old".into(),
            Reachability::Cloud,
            at(200),
        )
        .unwrap();
    let direct = Reachability::Direct {
        addrs: vec![addr("127.0.0.1:9000")],
    };

    assert_eq!(
        store
            .upsert_paired_peer(PEER, vec![8; 32], "new".into(), direct.clone(), at(300))
            .unwrap(),
        PairingUpdate::PubkeyReplacementRequired
    );
    let entry = store.entry(PEER).unwrap();
    assert_eq!(entry.name, "old");
    assert_eq!(entry.pubkey, vec![7; 32]);
    assert_eq!(entry.reachabilities, vec![Reachability::Cloud]);
    assert!(store.pubkey_for_host(PEER).is_none());
    assert!(store.host_id_for_pubkey(&[7; 32]).is_none());

    assert_eq!(
        store
            .replace_paired_peer_after_teardown(
                PEER,
                vec![8; 32],
                "new".into(),
                direct.clone(),
                at(300)
            )
            .unwrap(),
        PairingUpdate::ReplacedPubkey
    );
    let entry = store.entry(PEER).unwrap();
    assert_eq!(entry.name, "new");
    assert_eq!(entry.pubkey, vec![8; 32]);
    assert_eq!(entry.reachabilities, vec![direct]);
}

#[test]
fn the_trust_file_round_trips_and_names_its_reachabilities() {
    let mut store = TrustStore::default();
    store.insert_for_test(
        PEER,
        TrustEntry {
            pubkey: vec![9; 32],
            name: "peer".to_string(),
            paired_at: at(200),
            reachabilities: vec![Reachability::Ssh {
                target: "workstation".to_string(),
                profile: Uuid::from_u128(42),
            }],
            signed_in: Some(false),
            platform: wire::Platform::Linux,
            listed: vec![crate::payload::PeerAddress {
                addr: "192.0.2.7:7000".parse().unwrap(),
                source: crate::payload::Source::Listed,
            }],
        },
    );

    let bytes = store.to_file().unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("workstation"));
    assert_eq!(TrustStore::from_file(&bytes).unwrap(), store);
}

#[test]
fn trust_store_remembers_a_peers_signed_in_fact_and_platform() {
    let mut store = TrustStore::default();
    store
        .upsert_paired_peer(
            PEER,
            vec![9; 32],
            "peer".into(),
            Reachability::Cloud,
            at(200),
        )
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

#[test]
fn trust_store_rejects_wrong_length_pubkey_on_load_and_save() {
    let mut store = TrustStore::default();
    store.insert_for_test(
        PEER,
        TrustEntry {
            pubkey: vec![1, 2, 3],
            name: "peer".to_string(),
            paired_at: at(200),
            reachabilities: vec![Reachability::Cloud],
            signed_in: None,
            platform: wire::Platform::Unspecified,
            listed: Vec::new(),
        },
    );
    assert_eq!(store.to_file(), Err(TrustError::BadPubkey { actual: 3 }));

    let json = format!(
        r#"{{
  "{PEER}": {{
    "pubkey": [1, 2, 3],
    "name": "peer",
    "paired_at": "1970-01-01T00:00:00Z",
    "reachabilities": [{{ "type": "cloud" }}]
  }}
}}"#
    );
    assert_eq!(
        TrustStore::from_file(json.as_bytes()),
        Err(TrustError::BadPubkey { actual: 3 })
    );
}

fn daemon() -> Daemon {
    let store = Sqlite::open_in_memory(vec![1; 16]).expect("a store opens");
    let mut daemon = Daemon::new(
        store,
        StartOptions {
            trust: "/profile/trust.json".into(),
            budget: StepBudget::UNLIMITED,
            ..StartOptions::default()
        },
    );
    step(
        &mut daemon,
        Input::TrustRead {
            store: TrustStore::default(),
        },
    );
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

fn written(out: &[Output]) -> (JobId, TrustStore) {
    match job(out) {
        Some((id, Job::Fsync { path, bytes })) => {
            assert_eq!(path, std::path::Path::new("/profile/trust.json"));
            (id, TrustStore::from_file(&bytes).unwrap())
        }
        other => panic!("expected the trust file written, got {other:?}"),
    }
}

fn done(daemon: &mut Daemon, job: JobId, result: JobResult) -> Vec<Output> {
    step(daemon, Input::JobDone { job, result })
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

/// A change is served, and answered, only once the drive holds it.
#[test]
fn a_change_takes_effect_once_its_write_is_done() {
    let mut daemon = daemon();
    let out = change(&mut daemon, 1, pair(7));
    let (write, staged) = written(&out);
    assert!(staged.entry(PEER).is_some());
    assert!(
        daemon.trust().entry(PEER).is_none(),
        "not served before the write"
    );
    assert_eq!(ended(&out, 1), None);

    let out = done(&mut daemon, write, JobResult::Done);
    assert_eq!(daemon.trust().entry(PEER).unwrap().pubkey, vec![7; 32]);
    assert_eq!(published(&out).unwrap(), daemon.trust());
    assert!(out.contains(&Output::Answer {
        call: CallId(1),
        response: Response::Trust(TrustChanged::Paired(PairingUpdate::Inserted)),
    }));
    assert_eq!(ended(&out, 1), Some(Status::Ok));
}

/// A later change waits for the earlier one's write, so it is never
/// written under it.
#[test]
fn changes_are_written_one_at_a_time_in_order() {
    let mut daemon = daemon();
    let (first, _) = written(&change(&mut daemon, 1, pair(7)));
    let out = change(&mut daemon, 2, TrustChange::Remove { host: PEER });
    assert_eq!(job(&out), None, "the second waits for the first");

    let out = done(&mut daemon, first, JobResult::Done);
    let (second, staged) = written(&out);
    assert!(staged.entry(PEER).is_none());
    let out = done(&mut daemon, second, JobResult::Done);
    assert!(daemon.trust().is_empty());
    assert!(matches!(
        out.iter()
            .find(|output| matches!(output, Output::Answer { .. })),
        Some(Output::Answer {
            response: Response::Trust(TrustChanged::Removed(_)),
            ..
        })
    ));
}

#[test]
fn a_change_the_drive_refused_never_shows() {
    let mut daemon = daemon();
    let (write, _) = written(&change(&mut daemon, 1, pair(7)));
    let out = done(&mut daemon, write, JobResult::Failed("disk full".into()));
    assert!(daemon.trust().is_empty());
    assert_eq!(
        ended(&out, 1),
        Some(Status::Failed(CallError::Trust(TrustError::Write(
            "disk full".into()
        ))))
    );
}

#[test]
fn forgetting_a_host_that_is_not_paired_is_refused_without_a_write() {
    let mut daemon = daemon();
    let out = change(&mut daemon, 1, TrustChange::Remove { host: PEER });
    assert_eq!(job(&out), None);
    assert_eq!(
        ended(&out, 1),
        Some(Status::Failed(CallError::Trust(TrustError::NotTrusted(
            PEER
        ))))
    );
}

#[test]
fn a_remembered_fact_that_did_not_move_writes_nothing() {
    let mut daemon = daemon();
    let (write, _) = written(&change(&mut daemon, 1, pair(7)));
    done(&mut daemon, write, JobResult::Done);
    let remember = TrustChange::Remember {
        host: PEER,
        signed_in: None,
        platform: wire::Platform::Unspecified,
    };
    let out = step(&mut daemon, Input::ChangeTrust(remember));
    assert_eq!(job(&out), None);
    let addrs = TrustChange::DirectAddrs {
        host: PEER,
        addrs: vec![addr("10.0.0.2:4000")],
    };
    let (write, _) = written(&step(&mut daemon, Input::ChangeTrust(addrs.clone())));
    done(&mut daemon, write, JobResult::Done);
    assert_eq!(job(&step(&mut daemon, Input::ChangeTrust(addrs))), None);
}

/// A host paired under another key stops being trusted under the old key
/// at once; its connections are torn down before the new key is written.
#[test]
fn replacing_a_key_tears_the_host_down_before_the_write() {
    let mut daemon = daemon();
    let (write, _) = written(&change(&mut daemon, 1, pair(7)));
    done(&mut daemon, write, JobResult::Done);

    let out = change(&mut daemon, 2, pair(8));
    let Some((teardown, Job::TeardownHost { host })) = job(&out) else {
        panic!("expected a teardown first, got {out:?}");
    };
    assert_eq!(host, HostId(*PEER.as_bytes()));
    assert!(
        daemon.trust().pubkey_for_host(PEER).is_none(),
        "the old key is out"
    );
    assert!(published(&out).unwrap().pubkey_for_host(PEER).is_none());

    let (write, staged) = written(&done(&mut daemon, teardown, JobResult::Done));
    assert_eq!(staged.pubkey_for_host(PEER), Some([8; 32].as_slice()));
    let out = done(&mut daemon, write, JobResult::Done);
    assert_eq!(
        daemon.trust().pubkey_for_host(PEER),
        Some([8; 32].as_slice())
    );
    assert!(out.contains(&Output::Answer {
        call: CallId(2),
        response: Response::Trust(TrustChanged::Paired(PairingUpdate::ReplacedPubkey)),
    }));
}

#[test]
fn a_replacement_the_drive_refused_trusts_the_old_key_again() {
    let mut daemon = daemon();
    let (write, _) = written(&change(&mut daemon, 1, pair(7)));
    done(&mut daemon, write, JobResult::Done);
    let Some((teardown, _)) = job(&change(&mut daemon, 2, pair(8))) else {
        panic!("expected a teardown");
    };
    let (write, _) = written(&done(&mut daemon, teardown, JobResult::Done));
    let out = done(&mut daemon, write, JobResult::Failed("disk full".into()));
    assert_eq!(
        daemon.trust().pubkey_for_host(PEER),
        Some([7; 32].as_slice())
    );
    assert!(out.contains(&Output::Shell(ShellWork::ReplacementEnded {
        host: HostId(*PEER.as_bytes()),
    })));
}

/// A change under way, with every reachability shape, survives the
/// checkpoint the core is restored from.
#[test]
fn a_change_under_way_survives_a_checkpoint() {
    let mut daemon = daemon();
    let (write, _) = written(&change(&mut daemon, 1, pair(7)));
    done(&mut daemon, write, JobResult::Done);
    change(
        &mut daemon,
        2,
        TrustChange::Pair {
            host: OTHER,
            pubkey: vec![9; 32],
            name: "other".into(),
            reachability: Some(Reachability::Ssh {
                target: "desk".into(),
                profile: Uuid::from_u128(4),
            }),
        },
    );
    step(
        &mut daemon,
        Input::ChangeTrust(TrustChange::DirectAddrs {
            host: PEER,
            addrs: vec![addr("[::1]:4000")],
        }),
    );
    let checkpoint = daemon.checkpoint();
    let restored = Daemon::restore(
        checkpoint.clone(),
        Sqlite::open_in_memory(vec![1; 16]).unwrap(),
    )
    .expect("it restores");
    assert_eq!(restored.checkpoint(), checkpoint);
    assert_eq!(restored.trust, daemon.trust);
}
