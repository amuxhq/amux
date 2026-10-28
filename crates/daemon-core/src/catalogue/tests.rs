use store::Sqlite;
use wire::message::Body;
use wire::{Hello, Message, NeighborUp};

use super::*;
use crate::ids::{CallId, JobId};
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
            budget: StepBudget::UNLIMITED,
            ..StartOptions::default()
        },
    );
    {
        daemon.trust_for_test(TrustStore::of_hosts(trusted.iter().copied()));
        step(&mut daemon, Input::Started)
    };
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
        .find(|entry| entry.host_id() == id.0)
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

fn teardown(out: &[Output]) -> JobId {
    out.iter()
        .find_map(|output| match output {
            Output::Job {
                id,
                op: Job::TeardownHost { .. },
            } => Some(*id),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a teardown, got {out:?}"))
}

fn pair(id: HostId, pubkey: Vec<u8>) -> TrustChange {
    TrustChange::Pair {
        host: Uuid::from_bytes(id.0),
        pubkey,
        name: "desk".into(),
        reachability: Some(Reachability::Cloud),
    }
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
    assert_eq!(entry(&daemon, DESK).version(), Some("1.2.3"));

    // Pairing under a new key tears the host down first.
    let out = step(
        &mut daemon,
        Input::ClientCall {
            call: CallId(1),
            request: ClientRequest::ChangeTrust(pair(DESK, vec![8; 32])),
        },
    );
    let teardown = self::teardown(&out);
    assert!(daemon.routing.catalogue.said(DESK).is_none());
    // What arrives meanwhile, on a link or a neighbor's word, is not held.
    linked(&mut daemon, 2, DESK, CarrierKind::Direct, &[]);
    linked(&mut daemon, 3, RELAY, CarrierKind::Relay, &[DESK]);
    assert!(daemon.routing.catalogue.said(DESK).is_none());
    assert!(daemon.routing.catalogue.relays_to(DESK).is_empty());

    step(
        &mut daemon,
        Input::JobDone {
            job: teardown,
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
    step(&mut daemon, Input::ChannelAnswered { channel: after });
    assert!(!daemon.routing.catalogue.revoked(DESK));
    assert!(!entry(&daemon, DESK).revoked());
}

#[test]
fn a_trusted_host_the_relay_claims_is_remembered_as_on_the_account() {
    let mut daemon = daemon(&[DESK]);
    let reachable = |daemon: &Daemon| {
        daemon
            .trust()
            .entry(Uuid::from_bytes(DESK.0))
            .expect("the desk is trusted")
            .on_account
    };
    // Paired on the same network: nothing says the relay reaches it.
    linked(&mut daemon, 1, DESK, CarrierKind::Direct, &[]);
    assert!(!reachable(&daemon));

    // The relay claims it: the next launch may send through the relay.
    step(
        &mut daemon,
        Input::LinkUp {
            link: LinkId(2),
            peer: RELAY,
            kind: CarrierKind::Relay,
            resumed: None,
        },
    );
    step(
        &mut daemon,
        Input::LinkControl {
            link: LinkId(2),
            frame: Message {
                body: Some(Body::NeighborUp(NeighborUp {
                    host: Some(host(DESK)),
                })),
            },
        },
    );
    assert!(reachable(&daemon));

    // A claim on a link that is not the relay's says nothing of the account.
    let mut daemon = self::daemon(&[DESK]);
    linked(&mut daemon, 1, numbered(1), CarrierKind::Direct, &[DESK]);
    assert!(!reachable(&daemon));
}

const NAS: HostId = HostId([3; 16]);

fn candidate(daemon: &Daemon, id: HostId) -> wire::Candidate {
    entry(daemon, id)
        .candidate()
        .cloned()
        .expect("the host is a candidate")
}

fn announcers(candidate: &wire::Candidate) -> Vec<(HostId, bool)> {
    candidate
        .announced_by
        .iter()
        .map(|host| {
            (
                HostId::from_slice(&host.host_id).expect("an id"),
                host.is_cloud_relay(),
            )
        })
        .collect()
}

#[test]
fn a_host_the_relay_announces_and_nobody_paired_is_a_candidate_only_the_relay_reaches() {
    let mut daemon = daemon(&[DESK]);
    linked(&mut daemon, 1, RELAY, CarrierKind::Relay, &[DESK, NAS]);

    let nas = candidate(&daemon, NAS);
    assert_eq!(announcers(&nas), vec![(RELAY, true)]);
    assert!(nas.relay_only());
    assert_eq!(entry(&daemon, NAS).version(), Some("1.2.3"));
    assert!(
        entry(&daemon, DESK).trusted().is_some(),
        "a paired host stays trusted"
    );
    assert!(
        !daemon
            .host_set()
            .iter()
            .any(|entry| entry.host_id() == RELAY.0),
        "the relay is no host to pair with"
    );
}

#[test]
fn a_paired_host_announcing_a_candidate_comes_before_the_relay_and_pairs_it_on_any_plan() {
    let mut daemon = daemon(&[DESK]);
    linked(&mut daemon, 1, RELAY, CarrierKind::Relay, &[DESK, NAS]);
    linked(&mut daemon, 2, DESK, CarrierKind::Direct, &[NAS, RELAY]);

    let nas = candidate(&daemon, NAS);
    assert_eq!(announcers(&nas), vec![(DESK, false), (RELAY, true)]);
    assert!(!nas.relay_only());
    assert_eq!(nas.through_paired().count(), 1);
    assert!(
        !daemon
            .host_set()
            .iter()
            .any(|entry| entry.host_id() == RELAY.0),
        "a relay a neighbor names is no candidate"
    );

    // The relay going leaves the paired host's word.
    step(
        &mut daemon,
        Input::LinkDown {
            link: LinkId(1),
            reason: wire::LinkCloseReason::UserShutdown,
        },
    );
    assert_eq!(announcers(&candidate(&daemon, NAS)), vec![(DESK, false)]);
}

#[test]
fn whether_an_announcer_is_the_cloud_relay_is_how_its_link_was_admitted_not_what_it_says() {
    let mut daemon = daemon(&[]);
    // RELAY's handshake says it serves as a cloud relay, but this host
    // dialled it as a peer: it forwards without asking for a plan.
    linked(&mut daemon, 1, RELAY, CarrierKind::Direct, &[NAS]);
    assert_eq!(announcers(&candidate(&daemon, NAS)), vec![(RELAY, false)]);
}

#[test]
fn a_phone_a_neighbor_announces_is_no_candidate_since_it_shows_no_pairing_code() {
    let phone = HostId([4; 16]);
    let mut daemon = daemon(&[]);
    step(
        &mut daemon,
        Input::LinkUp {
            link: LinkId(1),
            peer: RELAY,
            kind: CarrierKind::Relay,
            resumed: None,
        },
    );
    step(
        &mut daemon,
        Input::LinkControl {
            link: LinkId(1),
            frame: Message {
                body: Some(Body::Hello(Hello {
                    host: Some(host(RELAY)),
                    neighbors: vec![
                        host(NAS),
                        wire::Host {
                            platform: wire::Platform::Ios as i32,
                            ..host(phone)
                        },
                    ],
                    ..Default::default()
                })),
            },
        },
    );
    assert!(
        daemon
            .host_set()
            .iter()
            .any(|entry| entry.host_id() == NAS.0)
    );
    assert!(
        !daemon
            .host_set()
            .iter()
            .any(|entry| entry.host_id() == phone.0),
        "a phone only joins a pairing, so nothing here could pair with it"
    );
}

#[test]
fn a_candidate_goes_when_its_last_announcer_takes_it_back() {
    let mut daemon = daemon(&[]);
    linked(&mut daemon, 1, RELAY, CarrierKind::Relay, &[NAS]);
    assert!(
        daemon
            .host_set()
            .iter()
            .any(|entry| entry.host_id() == NAS.0)
    );
    step(
        &mut daemon,
        Input::LinkControl {
            link: LinkId(1),
            frame: Message {
                body: Some(Body::NeighborDown(wire::NeighborDown {
                    host_id: NAS.0.to_vec(),
                })),
            },
        },
    );
    assert!(
        !daemon
            .host_set()
            .iter()
            .any(|entry| entry.host_id() == NAS.0)
    );
}

#[test]
fn a_host_discovery_finds_and_the_relay_announces_is_one_nearby_candidate() {
    let mut daemon = daemon(&[]);
    step(
        &mut daemon,
        Input::Candidates {
            found: vec![crate::host_set::Candidate {
                host: NAS,
                name: "nas".into(),
                addrs: vec!["192.168.1.9:4000".into()],
            }],
        },
    );
    linked(&mut daemon, 1, RELAY, CarrierKind::Relay, &[NAS]);
    let nas = candidate(&daemon, NAS);
    assert_eq!(nas.addrs, vec!["192.168.1.9:4000".to_owned()]);
    assert_eq!(announcers(&nas), vec![(RELAY, true)]);
    assert!(nas.nearby() && !nas.relay_only());
    assert_eq!(entry(&daemon, NAS).name(), "nas");
}
