use store::Sqlite;
use wire::message::Body;
use wire::{Hello, Message, NeighborUp};

use super::*;
use crate::ids::{CallId, JobId, TimerId};
use crate::io::{Input, Job};
use crate::payload::{CarrierKind, ChannelClass, ClientRequest, JobResult, PublicKey};
use crate::trust::{Reachability, TrustStore};
use crate::{StartOptions, StepBudget};

const OWN: [u8; 16] = *b"own-host-0000000";
const DESK: HostId = HostId([1; 16]);
const RELAY: HostId = HostId([9; 16]);

fn daemon(trusted: &[HostId]) -> Daemon {
    let mut daemon = Daemon::new(
        Sqlite::open_in_memory(OWN).expect("a store opens"),
        StartOptions {
            trust: "/profile/trust.json".into(),
            budget: StepBudget::UNLIMITED,
            ..StartOptions::default()
        },
    );
    step(
        &mut daemon,
        Input::TrustRead {
            store: TrustStore::of_hosts(trusted.iter().copied()),
        },
    );
    daemon
}

fn step(daemon: &mut Daemon, input: Input) -> Vec<Output> {
    let mut out = Vec::new();
    daemon.step(input, &mut out);
    assert!(
        !out.iter()
            .any(|output| matches!(output, Output::Dump { .. })),
        "no step breaks an invariant: {out:?}"
    );
    out
}

fn host(id: HostId) -> wire::Host {
    wire::Host {
        host_id: id.0.to_vec(),
        version: "1.2.3".into(),
        capabilities: Some(wire::Capabilities {
            cloud_relay: id == RELAY,
            kinds: vec![wire::Kind::ClaudeSdk as i32],
        }),
        ..Default::default()
    }
}

fn numbered(n: u32) -> HostId {
    let mut id = [0x40; 16];
    id[..4].copy_from_slice(&n.to_be_bytes());
    HostId(id)
}

/// A link to `peer` comes up and its handshake says who it is and whom it
/// reaches.
fn linked(daemon: &mut Daemon, link: u64, peer: HostId, kind: CarrierKind, reaches: &[HostId]) {
    step(
        daemon,
        Input::LinkUp {
            link: LinkId(link),
            peer,
            kind,
            resumed: None,
        },
    );
    step(
        daemon,
        Input::LinkControl {
            link: LinkId(link),
            frame: Message {
                body: Some(Body::Hello(Hello {
                    host: Some(host(peer)),
                    neighbors: reaches.iter().copied().map(host).collect(),
                    ..Default::default()
                })),
            },
        },
    );
}

fn neighbor_up(daemon: &mut Daemon, link: u64, id: HostId) {
    step(
        daemon,
        Input::LinkControl {
            link: LinkId(link),
            frame: Message {
                body: Some(Body::NeighborUp(NeighborUp {
                    host: Some(host(id)),
                })),
            },
        },
    );
}

fn entry(daemon: &Daemon, id: HostId) -> wire::HostEntry {
    daemon
        .host_set()
        .iter()
        .find(|entry| entry.host_id == id.0)
        .cloned()
        .expect("the host is in the host set")
}

/// Opens a calls channel to `peer` over `link`; returns its id.
fn channel(daemon: &mut Daemon, link: u64, peer: HostId) -> ChannelId {
    let out = step(
        daemon,
        Input::WantChannel {
            link: LinkId(link),
            peer,
            class: ChannelClass::Calls,
            pin: Some(PublicKey(vec![7; 32])),
        },
    );
    let channel = out
        .iter()
        .find_map(|output| match output {
            Output::OpenChannel { channel, .. } => Some(*channel),
            _ => None,
        })
        .expect("the core opens the channel");
    step(
        daemon,
        Input::ChannelOpened {
            channel,
            link: LinkId(link),
            peer,
            class: ChannelClass::Calls,
        },
    );
    channel
}

fn written_or_none(out: &[Output]) -> Option<JobId> {
    out.iter().find_map(|output| match output {
        Output::Job {
            id,
            op: Job::Fsync { .. } | Job::TeardownHost { .. },
        } => Some(*id),
        _ => None,
    })
}

fn written(out: &[Output]) -> JobId {
    written_or_none(out).unwrap_or_else(|| panic!("expected a trust job, got {out:?}"))
}

fn pair(id: HostId, pubkey: Vec<u8>) -> TrustChange {
    TrustChange::Pair {
        host: Uuid::from_bytes(id.0),
        pubkey,
        name: "desk".into(),
        reachability: Some(Reachability::Cloud),
    }
}

fn retried_now(out: &[Output], id: HostId, now: u64) -> bool {
    out.contains(&Output::ArmTimer {
        id: TimerId::Retry(id),
        at_ms: now,
    })
}

#[test]
fn past_the_cap_a_claim_evicts_the_oldest_untrusted_host_held_only_by_a_claim() {
    let mut daemon = daemon(&[DESK]);
    // The relay's own entry and the hosts it claims fill the cap; the
    // trusted desk does not count toward it.
    let claimed: Vec<HostId> = (0..HOST_CAP as u32 - 1).map(numbered).collect();
    let reaches: Vec<HostId> = std::iter::once(DESK).chain(claimed.clone()).collect();
    linked(&mut daemon, 1, RELAY, CarrierKind::Relay, &reaches);
    let catalogue = &daemon.routing.catalogue;
    assert!(catalogue.said(claimed[0]).is_some());
    assert_eq!(catalogue.relays_to(claimed[0]), vec![RELAY]);

    let newcomer = numbered(HOST_CAP as u32);
    neighbor_up(&mut daemon, 1, newcomer);
    let catalogue = &daemon.routing.catalogue;
    assert!(catalogue.said(newcomer).is_some(), "the newcomer is held");
    assert!(
        catalogue.said(claimed[0]).is_none(),
        "the oldest claim went"
    );
    assert!(catalogue.said(claimed[1]).is_some());
    assert!(
        catalogue.said(DESK).is_some(),
        "a trusted host is never evicted"
    );
    assert!(
        catalogue.said(RELAY).is_some(),
        "a linked host is never evicted"
    );

    // A direct link is held past the cap too, and the next oldest claim
    // makes room.
    let dialed = numbered(HOST_CAP as u32 + 1);
    linked(&mut daemon, 2, dialed, CarrierKind::Direct, &[]);
    let catalogue = &daemon.routing.catalogue;
    assert!(catalogue.said(dialed).is_some());
    assert!(catalogue.said(claimed[1]).is_none());
    assert_eq!(catalogue.untrusted(&BTreeSet::from([DESK])), HOST_CAP);
}

#[test]
fn a_hosts_key_replacement_holds_nothing_of_it_until_the_new_key_is_written() {
    let mut daemon = daemon(&[DESK]);
    linked(&mut daemon, 1, DESK, CarrierKind::Direct, &[]);
    assert_eq!(entry(&daemon, DESK).version.as_deref(), Some("1.2.3"));

    // Pairing under a new key tears the host down first.
    let out = step(
        &mut daemon,
        Input::ClientCall {
            call: CallId(1),
            request: ClientRequest::ChangeTrust(pair(DESK, vec![8; 32])),
        },
    );
    let teardown = written(&out);
    assert!(daemon.routing.catalogue.said(DESK).is_none());
    // What arrives meanwhile, on a link or a neighbor's word, is not held.
    linked(&mut daemon, 2, DESK, CarrierKind::Direct, &[]);
    linked(&mut daemon, 3, RELAY, CarrierKind::Relay, &[DESK]);
    assert!(daemon.routing.catalogue.said(DESK).is_none());
    assert!(daemon.routing.catalogue.relays_to(DESK).is_empty());

    let write = written(&step(
        &mut daemon,
        Input::JobDone {
            job: teardown,
            result: JobResult::Done,
        },
    ));
    step(
        &mut daemon,
        Input::JobDone {
            job: write,
            result: JobResult::Done,
        },
    );
    neighbor_up(&mut daemon, 3, DESK);
    assert_eq!(daemon.routing.catalogue.relays_to(DESK), vec![RELAY]);
}

#[test]
fn only_a_channel_begun_after_a_revocation_takes_it_back() {
    let mut daemon = daemon(&[DESK]);
    linked(&mut daemon, 1, DESK, CarrierKind::Direct, &[]);
    let before = channel(&mut daemon, 1, DESK);
    step(&mut daemon, Input::PeerRevoked { host: DESK });

    // The earlier channel's answer may predate the refusal.
    step(&mut daemon, Input::ChannelAnswered { channel: before });
    assert!(daemon.routing.catalogue.revoked(DESK));

    let after = channel(&mut daemon, 1, DESK);
    let out = step(&mut daemon, Input::ChannelAnswered { channel: after });
    assert!(!daemon.routing.catalogue.revoked(DESK));
    assert!(retried_now(&out, DESK, 0), "tried at once: {out:?}");
    assert_eq!(entry(&daemon, DESK).revoked, None);
}
