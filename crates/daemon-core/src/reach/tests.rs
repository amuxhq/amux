use std::net::SocketAddr;

use store::Sqlite;
use uuid::Uuid;
use wire::message::Body;
use wire::{Hello, Message};

use super::*;
use crate::host_set::Candidate;
use crate::io::Input;
use crate::trust::TrustStore;
use crate::{StartOptions, StepBudget};

const OWN: [u8; 16] = *b"own-host-0000000";
const DESK: HostId = HostId([1; 16]);
const LAPTOP: HostId = HostId([2; 16]);
const RELAY: HostId = HostId([9; 16]);

fn addr(text: &str) -> SocketAddr {
    text.parse().unwrap()
}

/// A daemon trusting `trusted`, its driver dialling.
fn daemon(trusted: TrustStore) -> Daemon {
    let mut daemon = Daemon::new(
        Sqlite::open_in_memory(OWN).expect("a store opens"),
        StartOptions {
            budget: StepBudget::UNLIMITED,
            ..StartOptions::default()
        },
    );
    daemon.trust_for_test(trusted);
    step(&mut daemon, Input::Started);
    daemon
}

/// The desk and the laptop trusted, the desk last answering at an address.
fn trusting() -> TrustStore {
    let mut trust = TrustStore::of_hosts([DESK, LAPTOP]);
    let desk = Uuid::from_bytes(DESK.0);
    let mut entry = trust.entry(desk).cloned().unwrap();
    entry.direct = vec![addr("10.0.0.1:7000")];
    trust.insert_for_test(desk, entry);
    trust
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

fn at(daemon: &mut Daemon, now_ms: u64) {
    step(
        daemon,
        Input::Tick {
            now_ms,
            link_ms: now_ms,
        },
    );
}

/// The dials a step asked for, by host.
fn dials(out: &[Output]) -> Vec<(HostId, AttemptId, Plan)> {
    out.iter()
        .filter_map(|output| match output {
            Output::Dial {
                host,
                attempt,
                plan,
                ..
            } => Some((*host, *attempt, plan.clone())),
            _ => None,
        })
        .collect()
}

fn dial_of(out: &[Output], host: HostId) -> (AttemptId, Plan) {
    dials(out)
        .into_iter()
        .find_map(|(dialled, attempt, plan)| (dialled == host).then_some((attempt, plan)))
        .unwrap_or_else(|| panic!("{host:?} is dialled: {out:?}"))
}

fn armed(out: &[Output], id: TimerId) -> Option<u64> {
    out.iter().find_map(|output| match output {
        Output::ArmTimer { id: armed, at_ms } if *armed == id => Some(*at_ms),
        _ => None,
    })
}

fn ended(
    daemon: &mut Daemon,
    host: HostId,
    attempt: AttemptId,
    outcome: AttemptOutcome,
) -> Vec<Output> {
    step(
        daemon,
        Input::AttemptEnded {
            host,
            attempt,
            outcome,
        },
    )
}

fn failed(daemon: &mut Daemon, host: HostId, attempt: AttemptId) -> Vec<Output> {
    ended(
        daemon,
        host,
        attempt,
        AttemptOutcome::Failed("no answer".into()),
    )
}

fn up(daemon: &mut Daemon, link: u64, peer: HostId, kind: CarrierKind) -> Vec<Output> {
    step(
        daemon,
        Input::LinkUp {
            link: LinkId(link),
            peer,
            kind,
            resumed: None,
        },
    )
}

/// The link is lost, with no word from either end.
fn down(daemon: &mut Daemon, link: u64) -> Vec<Output> {
    closed(daemon, link, wire::LinkCloseReason::Unspecified)
}

fn closed(daemon: &mut Daemon, link: u64, reason: wire::LinkCloseReason) -> Vec<Output> {
    step(
        daemon,
        Input::LinkDown {
            link: LinkId(link),
            reason,
        },
    )
}

fn claims(daemon: &mut Daemon, link: u64, hosts: &[HostId]) -> Vec<Output> {
    let hello = Hello {
        neighbors: hosts
            .iter()
            .map(|host| wire::Host {
                host_id: host.0.to_vec(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    step(
        daemon,
        Input::LinkControl {
            link: LinkId(link),
            frame: Message {
                body: Some(Body::Hello(hello)),
            },
        },
    )
}

fn found(daemon: &mut Daemon, host: HostId, addrs: &[&str]) -> Vec<Output> {
    step(
        daemon,
        Input::Candidates {
            found: vec![Candidate {
                host,
                name: "found".into(),
                addrs: addrs.iter().map(|addr| (*addr).to_owned()).collect(),
            }],
        },
    )
}

fn refused(daemon: &mut Daemon, link: u64, host: HostId, reason: ForwardRefusal) -> Vec<Output> {
    step(
        daemon,
        Input::LinkControl {
            link: LinkId(link),
            frame: Message {
                body: Some(Body::ForwardRefused(wire::ForwardRefused {
                    dst: host.0.to_vec(),
                    reason: reason as i32,
                })),
            },
        },
    )
}

#[test]
fn nothing_is_dialled_until_the_driver_dials_and_then_every_trusted_host_with_a_path_is() {
    let mut daemon = daemon(trusting());
    assert_eq!(
        dials(&found(&mut daemon, LAPTOP, &["10.0.0.2:7000"])),
        Vec::new()
    );

    let out = step(&mut daemon, Input::Dialling(Dialling::All));
    let mut hosts: Vec<HostId> = dials(&out).into_iter().map(|(host, ..)| host).collect();
    hosts.sort();
    assert_eq!(hosts, vec![DESK, LAPTOP]);
    assert_eq!(
        dial_of(&out, DESK).1.paths,
        vec![DialPath::Direct(addr("10.0.0.1:7000"))]
    );
    assert_eq!(
        armed(&out, TimerId::Attempt(DESK)),
        Some(ATTEMPT_DEADLINE_MS)
    );
}

#[test]
fn a_dial_goes_on_every_path_at_once() {
    let mut trust = trusting();
    let desk = Uuid::from_bytes(DESK.0);
    let mut entry = trust.entry(desk).cloned().unwrap();
    entry.on_account = true;
    entry.ssh = Some("desk.local".into());
    trust.insert_for_test(desk, entry);
    let mut daemon = daemon(trust);
    up(&mut daemon, 1, LAPTOP, CarrierKind::Direct);
    claims(&mut daemon, 1, &[DESK]);
    found(&mut daemon, DESK, &["192.168.1.5:7000", "10.0.0.1:7000"]);

    let out = step(&mut daemon, Input::Dialling(Dialling::All));
    assert_eq!(
        dial_of(&out, DESK).1,
        Plan {
            paths: vec![
                DialPath::Direct(addr("192.168.1.5:7000")),
                DialPath::Direct(addr("10.0.0.1:7000")),
                DialPath::Ssh("desk.local".into()),
                DialPath::Through(LinkId(1)),
            ],
            through_relay: true,
        },
        "where discovery found it, where it last answered, SSH, the laptop and the relay"
    );
}

#[test]
fn a_host_is_not_dialled_that_is_untrusted_this_host_a_relay_or_linked() {
    let mut daemon = daemon(TrustStore::of_hosts([LAPTOP]));
    step(&mut daemon, Input::Dialling(Dialling::All));
    assert_eq!(
        dials(&found(&mut daemon, DESK, &["10.0.0.1:7000"])),
        Vec::new()
    );
    assert_eq!(
        dials(&found(&mut daemon, HostId(OWN), &["10.0.0.9:7000"])),
        Vec::new()
    );
    up(&mut daemon, 1, LAPTOP, CarrierKind::Direct);
    assert_eq!(
        dials(&found(&mut daemon, LAPTOP, &["10.0.0.2:7000"])),
        Vec::new()
    );
    up(&mut daemon, 2, RELAY, CarrierKind::Relay);
    assert_eq!(dials(&claims(&mut daemon, 2, &[LAPTOP])), Vec::new());
}

#[test]
fn a_host_is_in_one_attempt_at_a_time() {
    let mut daemon = daemon(trusting());
    let out = step(&mut daemon, Input::Dialling(Dialling::All));
    let (first, _) = dial_of(&out, DESK);
    assert_eq!(
        dials(&step(&mut daemon, Input::Wanted { host: DESK })),
        Vec::new()
    );
    assert_eq!(
        dials(&step(&mut daemon, Input::TryAgain { host: DESK })),
        Vec::new(),
        "nothing new to dial"
    );

    // A path the attempt was not given: the attempt gives way to one on
    // every path at once.
    let out = found(&mut daemon, DESK, &["192.168.1.5:7000"]);
    assert!(out.contains(&Output::Abandon {
        host: DESK,
        attempt: first
    }));
    let (second, plan) = dial_of(&out, DESK);
    assert_ne!(first, second);
    assert_eq!(plan.paths.len(), 2);

    // The first attempt's end, arriving late, says nothing.
    assert_eq!(failed(&mut daemon, DESK, first), Vec::new());
    assert_eq!(daemon.reach().attempt(DESK), Some(second));
}

#[test]
fn a_failed_attempt_is_tried_again_after_a_second_doubling_to_thirty() {
    let mut daemon = daemon(trusting());
    let (mut attempt, _) = dial_of(&step(&mut daemon, Input::Dialling(Dialling::All)), DESK);
    let mut pauses = Vec::new();
    let mut now = 0;
    for _ in 0..7 {
        let out = failed(&mut daemon, DESK, attempt);
        let due = armed(&out, TimerId::Retry(DESK)).expect("a retry is armed");
        pauses.push(due - now);
        assert_eq!(daemon.last_dial_error(DESK), Some("no answer"));
        now = due;
        at(&mut daemon, now);
        let out = step(
            &mut daemon,
            Input::Timer {
                id: TimerId::Retry(DESK),
            },
        );
        attempt = dial_of(&out, DESK).0;
    }
    assert_eq!(
        pauses,
        vec![1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000]
    );
}

#[test]
fn an_attempt_past_its_deadline_is_abandoned_and_counts_as_a_failure() {
    let mut daemon = daemon(trusting());
    let (attempt, _) = dial_of(&step(&mut daemon, Input::Dialling(Dialling::All)), DESK);
    at(&mut daemon, ATTEMPT_DEADLINE_MS);
    let out = step(
        &mut daemon,
        Input::Timer {
            id: TimerId::Attempt(DESK),
        },
    );
    assert!(out.contains(&Output::Abandon {
        host: DESK,
        attempt
    }));
    assert_eq!(
        armed(&out, TimerId::Retry(DESK)),
        Some(ATTEMPT_DEADLINE_MS + RETRY_FIRST_MS)
    );
    assert_eq!(
        daemon.last_dial_error(DESK),
        Some("no path answered within 10 seconds")
    );
    // Its end, arriving after, says nothing.
    assert_eq!(failed(&mut daemon, DESK, attempt), Vec::new());
}

#[test]
fn a_new_path_a_claim_or_a_call_wanting_the_host_ends_the_pause() {
    let mut daemon = daemon(trusting());
    let (attempt, _) = dial_of(&step(&mut daemon, Input::Dialling(Dialling::All)), DESK);
    failed(&mut daemon, DESK, attempt);

    let out = step(&mut daemon, Input::Wanted { host: DESK });
    assert!(out.contains(&Output::CancelTimer {
        id: TimerId::Retry(DESK)
    }));
    let (attempt, _) = dial_of(&out, DESK);
    failed(&mut daemon, DESK, attempt);

    let out = found(&mut daemon, DESK, &["192.168.1.5:7000"]);
    let (attempt, _) = dial_of(&out, DESK);
    let out = failed(&mut daemon, DESK, attempt);
    assert_eq!(
        armed(&out, TimerId::Retry(DESK)),
        Some(RETRY_FIRST_MS),
        "the count started afresh"
    );

    up(&mut daemon, 1, LAPTOP, CarrierKind::Direct);
    let out = claims(&mut daemon, 1, &[DESK]);
    assert!(
        dial_of(&out, DESK)
            .1
            .paths
            .contains(&DialPath::Through(LinkId(1)))
    );
}

#[test]
fn a_host_that_refused_this_one_as_revoked_is_not_dialled_until_that_clears() {
    let mut daemon = daemon(trusting());
    let (attempt, _) = dial_of(&step(&mut daemon, Input::Dialling(Dialling::All)), DESK);
    let out = ended(&mut daemon, DESK, attempt, AttemptOutcome::Revoked);
    assert_eq!(armed(&out, TimerId::Retry(DESK)), None);
    assert!(daemon.routing.catalogue.revoked(DESK));
    assert_eq!(
        dials(&found(&mut daemon, DESK, &["192.168.1.5:7000"])),
        Vec::new()
    );
    assert_eq!(
        dials(&step(&mut daemon, Input::Wanted { host: DESK })),
        Vec::new()
    );
    up(&mut daemon, 1, LAPTOP, CarrierKind::Direct);
    assert_eq!(dials(&claims(&mut daemon, 1, &[DESK])), Vec::new());
}

#[test]
fn a_link_lost_soon_after_it_came_up_waits_out_the_rest_of_its_second() {
    let mut daemon = daemon(trusting());
    let (attempt, _) = dial_of(&step(&mut daemon, Input::Dialling(Dialling::All)), DESK);
    at(&mut daemon, 5_000);
    up(&mut daemon, 1, DESK, CarrierKind::Direct);
    ended(
        &mut daemon,
        DESK,
        attempt,
        AttemptOutcome::Linked {
            addr: Some(addr("10.0.0.1:7000")),
        },
    );
    at(&mut daemon, 5_200);
    let out = down(&mut daemon, 1);
    assert_eq!(dials(&out), Vec::new());
    assert!(out.contains(&Output::Rediscover));
    assert_eq!(armed(&out, TimerId::Retry(DESK)), Some(6_000));
    // A call wanting it does not cut that pause short.
    assert_eq!(
        dials(&step(&mut daemon, Input::Wanted { host: DESK })),
        Vec::new()
    );
    at(&mut daemon, 6_000);
    let out = step(
        &mut daemon,
        Input::Timer {
            id: TimerId::Retry(DESK),
        },
    );
    dial_of(&out, DESK);
}

#[test]
fn a_link_that_lived_is_dialled_again_at_once_when_it_goes() {
    let mut daemon = daemon(trusting());
    let (attempt, _) = dial_of(&step(&mut daemon, Input::Dialling(Dialling::All)), DESK);
    up(&mut daemon, 1, DESK, CarrierKind::Direct);
    ended(
        &mut daemon,
        DESK,
        attempt,
        AttemptOutcome::Linked { addr: None },
    );
    at(&mut daemon, 60_000);
    dial_of(&down(&mut daemon, 1), DESK);
}

#[test]
fn a_host_reached_over_ssh_is_dialled_again_after_a_loss() {
    let mut trust = TrustStore::of_hosts([DESK]);
    let desk = Uuid::from_bytes(DESK.0);
    let mut entry = trust.entry(desk).cloned().unwrap();
    entry.ssh = Some("desk.local".into());
    trust.insert_for_test(desk, entry);
    let mut daemon = daemon(trust);
    let (attempt, plan) = dial_of(&step(&mut daemon, Input::Dialling(Dialling::All)), DESK);
    assert_eq!(plan.paths, vec![DialPath::Ssh("desk.local".into())]);
    up(&mut daemon, 1, DESK, CarrierKind::Ssh);
    ended(
        &mut daemon,
        DESK,
        attempt,
        AttemptOutcome::Linked { addr: None },
    );
    at(&mut daemon, 60_000);
    let (_, plan) = dial_of(&down(&mut daemon, 1), DESK);
    assert_eq!(plan.paths, vec![DialPath::Ssh("desk.local".into())]);
}

#[test]
fn an_attempt_every_forwarder_of_which_refused_ends_at_once() {
    let mut daemon = daemon(TrustStore::of_hosts([DESK]));
    step(&mut daemon, Input::Dialling(Dialling::All));
    up(&mut daemon, 1, RELAY, CarrierKind::Relay);
    let (attempt, plan) = dial_of(&claims(&mut daemon, 1, &[DESK]), DESK);
    assert_eq!(
        plan,
        Plan {
            paths: Vec::new(),
            through_relay: true
        }
    );
    let out = refused(&mut daemon, 1, DESK, ForwardRefusal::PaymentRequired);
    assert!(out.contains(&Output::Abandon {
        host: DESK,
        attempt
    }));
    assert_eq!(armed(&out, TimerId::Retry(DESK)), Some(RETRY_FIRST_MS));
}

#[test]
fn an_attempt_with_a_path_of_its_own_outlives_a_forwarders_refusal() {
    let mut daemon = daemon(trusting());
    step(&mut daemon, Input::Dialling(Dialling::All));
    up(&mut daemon, 1, RELAY, CarrierKind::Relay);
    let (attempt, _) = dial_of(&claims(&mut daemon, 1, &[DESK]), DESK);
    let out = refused(&mut daemon, 1, DESK, ForwardRefusal::PaymentRequired);
    assert!(!out.contains(&Output::Abandon {
        host: DESK,
        attempt
    }));
    assert_eq!(daemon.reach().attempt(DESK), Some(attempt));
}

#[test]
fn stopping_abandons_every_attempt_and_pause() {
    let mut daemon = daemon(trusting());
    let out = step(&mut daemon, Input::Dialling(Dialling::All));
    let (desk, _) = dial_of(&out, DESK);
    let (laptop, _) = dial_of(&found(&mut daemon, LAPTOP, &["10.0.0.2:7000"]), LAPTOP);
    failed(&mut daemon, LAPTOP, laptop);
    let out = step(&mut daemon, Input::Dialling(Dialling::Off));
    assert!(out.contains(&Output::Abandon {
        host: DESK,
        attempt: desk
    }));
    assert!(out.contains(&Output::CancelTimer {
        id: TimerId::Retry(LAPTOP)
    }));
    assert_eq!(daemon.reach().attempt(DESK), None);
}

#[test]
fn a_host_that_answered_directly_is_dialled_at_that_address_from_then_on() {
    let mut daemon = daemon(TrustStore::of_hosts([DESK]));
    step(&mut daemon, Input::Dialling(Dialling::All));
    let (attempt, _) = dial_of(&found(&mut daemon, DESK, &["192.168.1.5:7000"]), DESK);
    up(&mut daemon, 1, DESK, CarrierKind::Direct);
    ended(
        &mut daemon,
        DESK,
        attempt,
        AttemptOutcome::Linked {
            addr: Some(addr("192.168.1.5:7000")),
        },
    );
    let entry = daemon.trust().entry(Uuid::from_bytes(DESK.0)).unwrap();
    assert_eq!(entry.direct, vec![addr("192.168.1.5:7000")]);
}

#[test]
fn a_newly_paired_host_is_dialled() {
    let mut daemon = daemon(TrustStore::of_hosts([LAPTOP]));
    step(&mut daemon, Input::Dialling(Dialling::All));
    found(&mut daemon, DESK, &["192.168.1.5:7000"]);
    daemon.trust_for_test(trusting());
    let out = step(&mut daemon, Input::Started);
    dial_of(&out, DESK);
}

/// The desk trusted as the offerer of a pairing commits it, the laptop
/// pairing as the joiner: the desk holds off its own dial.
fn offered(daemon: &mut Daemon) -> Vec<Output> {
    let mut out = Vec::new();
    daemon.expect_link(DESK, &mut out);
    daemon.trust_for_test(trusting());
    out.extend(step(daemon, Input::Started));
    out
}

#[test]
fn the_offerer_of_a_pairing_waits_for_the_joiners_link_instead_of_dialling() {
    let mut daemon = daemon(TrustStore::of_hosts([LAPTOP]));
    step(&mut daemon, Input::Dialling(Dialling::All));
    found(&mut daemon, DESK, &["192.168.1.5:7000"]);
    let out = offered(&mut daemon);
    assert!(
        dials(&out).iter().all(|(host, ..)| *host != DESK),
        "{out:?}"
    );
    assert_eq!(armed(&out, TimerId::Retry(DESK)), Some(ATTEMPT_DEADLINE_MS));

    let out = up(&mut daemon, 1, DESK, CarrierKind::Direct);
    assert!(out.contains(&Output::CancelTimer {
        id: TimerId::Retry(DESK)
    }));
    at(&mut daemon, ATTEMPT_DEADLINE_MS);
    assert_eq!(
        daemon.reach().attempt(DESK),
        None,
        "linked: nothing to dial"
    );
}

/// The desk dialled at start and linked, a minute ago.
fn linked_to_the_desk() -> Daemon {
    let mut daemon = daemon(trusting());
    step(&mut daemon, Input::Listening(true));
    let (attempt, _) = dial_of(&step(&mut daemon, Input::Dialling(Dialling::All)), DESK);
    up(&mut daemon, 1, DESK, CarrierKind::Direct);
    ended(
        &mut daemon,
        DESK,
        attempt,
        AttemptOutcome::Linked { addr: None },
    );
    at(&mut daemon, 60_000);
    daemon
}

#[test]
fn a_host_that_said_goodbye_is_left_to_dial_back_and_dialled_once_an_attempt_passes() {
    let mut daemon = linked_to_the_desk();
    let out = closed(&mut daemon, 1, wire::LinkCloseReason::UserShutdown);
    assert!(
        dials(&out).iter().all(|(host, ..)| *host != DESK),
        "a dial would cross the desk's own as it starts again: {out:?}"
    );
    assert_eq!(
        armed(&out, TimerId::Retry(DESK)),
        Some(60_000 + ATTEMPT_DEADLINE_MS)
    );
    assert!(daemon.reach().expecting(DESK));
    let out = step(&mut daemon, Input::Wanted { host: DESK });
    assert!(dials(&out).is_empty(), "a call waits for the desk's link");

    at(&mut daemon, 60_000 + ATTEMPT_DEADLINE_MS);
    let out = step(
        &mut daemon,
        Input::Timer {
            id: TimerId::Retry(DESK),
        },
    );
    dial_of(&out, DESK);
}

#[test]
fn a_host_that_listens_for_nothing_dials_a_host_that_said_goodbye_as_before() {
    let mut daemon = daemon(trusting());
    let (attempt, _) = dial_of(&step(&mut daemon, Input::Dialling(Dialling::All)), DESK);
    up(&mut daemon, 1, DESK, CarrierKind::Direct);
    ended(
        &mut daemon,
        DESK,
        attempt,
        AttemptOutcome::Linked { addr: None },
    );
    at(&mut daemon, 60_000);
    let out = closed(&mut daemon, 1, wire::LinkCloseReason::UserShutdown);
    dial_of(&out, DESK);
    assert!(!daemon.reach().expecting(DESK));
}

#[test]
fn a_host_that_said_goodbye_and_came_back_is_not_dialled_after_the_wait() {
    let mut daemon = linked_to_the_desk();
    closed(&mut daemon, 1, wire::LinkCloseReason::UserShutdown);
    let out = up(&mut daemon, 2, DESK, CarrierKind::Direct);
    assert!(out.contains(&Output::CancelTimer {
        id: TimerId::Retry(DESK)
    }));
    assert!(!daemon.reach().expecting(DESK));
}

/// The peer on `link` says `host` has just arrived where it is.
fn arrived(daemon: &mut Daemon, link: u64, host: HostId) -> Vec<Output> {
    step(
        daemon,
        Input::LinkControl {
            link: LinkId(link),
            frame: Message {
                body: Some(Body::NeighborUp(wire::NeighborUp {
                    host: Some(wire::Host {
                        host_id: host.0.to_vec(),
                        ..Default::default()
                    }),
                })),
            },
        },
    )
}

#[test]
fn a_host_announced_as_arriving_is_left_to_dial_and_dialled_once_an_attempt_passes() {
    let mut daemon = daemon(trusting());
    let (attempt, _) = dial_of(&step(&mut daemon, Input::Dialling(Dialling::All)), DESK);
    failed(&mut daemon, DESK, attempt);
    up(&mut daemon, 1, RELAY, CarrierKind::Relay);
    at(&mut daemon, 60_000);

    let out = arrived(&mut daemon, 1, DESK);
    assert!(
        dials(&out).iter().all(|(host, ..)| *host != DESK),
        "the desk dials this host, handed the relay's list: {out:?}"
    );
    assert!(daemon.reach().expecting(DESK));
    assert_eq!(
        armed(&out, TimerId::Retry(DESK)),
        Some(60_000 + ATTEMPT_DEADLINE_MS)
    );
    let out = step(&mut daemon, Input::Wanted { host: DESK });
    assert!(dials(&out).is_empty(), "a call waits for the desk's link");

    at(&mut daemon, 60_000 + ATTEMPT_DEADLINE_MS);
    let out = step(
        &mut daemon,
        Input::Timer {
            id: TimerId::Retry(DESK),
        },
    );
    dial_of(&out, DESK);
}

#[test]
fn an_attempt_out_to_a_host_announced_as_arriving_is_dropped_for_its_dial() {
    let mut daemon = daemon(trusting());
    let (attempt, _) = dial_of(&step(&mut daemon, Input::Dialling(Dialling::All)), DESK);
    up(&mut daemon, 1, RELAY, CarrierKind::Relay);
    let out = arrived(&mut daemon, 1, DESK);
    assert!(out.contains(&Output::Abandon {
        host: DESK,
        attempt
    }));
    assert!(dials(&out).is_empty(), "{out:?}");
    assert!(daemon.reach().expecting(DESK));

    let out = up(&mut daemon, 2, DESK, CarrierKind::Direct);
    assert!(out.contains(&Output::CancelTimer {
        id: TimerId::Retry(DESK)
    }));
    assert!(!daemon.reach().expecting(DESK));
}

#[test]
fn a_host_named_in_a_peers_list_or_reached_already_is_dialled_as_before() {
    let mut daemon = daemon(trusting());
    let (attempt, _) = dial_of(&step(&mut daemon, Input::Dialling(Dialling::All)), DESK);
    failed(&mut daemon, DESK, attempt);
    up(&mut daemon, 1, LAPTOP, CarrierKind::Direct);
    let out = claims(&mut daemon, 1, &[DESK]);
    dial_of(&out, DESK);
    assert!(!daemon.reach().expecting(DESK), "this host is the newcomer");

    // Claimed through the laptop already, the desk announced by the relay
    // too has not just arrived: the attempt goes on, on the relay as well.
    up(&mut daemon, 2, RELAY, CarrierKind::Relay);
    let out = arrived(&mut daemon, 2, DESK);
    assert!(dial_of(&out, DESK).1.through_relay, "{out:?}");
    assert!(!daemon.reach().expecting(DESK));
}

#[test]
fn the_offerer_dials_once_no_link_came_within_an_attempts_deadline_and_not_for_a_call() {
    let mut daemon = daemon(TrustStore::of_hosts([LAPTOP]));
    step(&mut daemon, Input::Dialling(Dialling::All));
    found(&mut daemon, DESK, &["192.168.1.5:7000"]);
    offered(&mut daemon);
    at(&mut daemon, ATTEMPT_DEADLINE_MS);
    assert_eq!(daemon.reach().attempt(DESK), None);
    let out = step(
        &mut daemon,
        Input::Timer {
            id: TimerId::Retry(DESK),
        },
    );
    dial_of(&out, DESK);

    let mut daemon = self::daemon(TrustStore::of_hosts([LAPTOP]));
    step(&mut daemon, Input::Dialling(Dialling::All));
    found(&mut daemon, DESK, &["192.168.1.5:7000"]);
    offered(&mut daemon);
    let out = step(&mut daemon, Input::Wanted { host: DESK });
    assert_eq!(dials(&out), Vec::new());
    assert!(
        daemon.reach().expecting(DESK),
        "the call waits for its link"
    );
}

#[test]
fn a_dial_carries_the_calls_the_core_makes_of_the_host_once_linked() {
    let mut daemon = daemon(trusting());
    let out = step(&mut daemon, Input::Dialling(Dialling::All));
    let early = out
        .iter()
        .find_map(|output| match output {
            Output::Dial { host, early, .. } if *host == DESK => Some(early.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(early, vec![PeerRequest::SubscribeInventory]);
}

#[test]
fn a_relay_claiming_a_host_on_the_account_is_no_new_path() {
    let mut trust = trusting();
    let desk = Uuid::from_bytes(DESK.0);
    let mut entry = trust.entry(desk).cloned().unwrap();
    entry.on_account = true;
    trust.insert_for_test(desk, entry);
    let mut daemon = daemon(trust);
    let (attempt, plan) = dial_of(&step(&mut daemon, Input::Dialling(Dialling::All)), DESK);
    assert!(plan.through_relay);
    up(&mut daemon, 1, RELAY, CarrierKind::Relay);
    let out = claims(&mut daemon, 1, &[DESK]);
    assert_eq!(dials(&out), Vec::new());
    assert_eq!(daemon.reach().attempt(DESK), Some(attempt));
}

#[test]
fn a_driver_off_the_local_network_is_given_none_of_the_hosts_own_addresses() {
    let mut daemon = daemon(trusting());
    up(&mut daemon, 1, LAPTOP, CarrierKind::Direct);
    claims(&mut daemon, 1, &[DESK]);
    let out = step(&mut daemon, Input::Dialling(Dialling::Indirect));
    assert_eq!(
        dial_of(&out, DESK).1.paths,
        vec![DialPath::Through(LinkId(1))]
    );
}

#[test]
fn a_host_that_loses_its_link_while_an_attempt_is_out_has_it_restarted_on_what_was_claimed_meanwhile()
 {
    let mut daemon = daemon(trusting());
    let (first, _) = dial_of(&step(&mut daemon, Input::Dialling(Dialling::All)), DESK);
    up(&mut daemon, 1, DESK, CarrierKind::Direct);
    up(&mut daemon, 2, LAPTOP, CarrierKind::Direct);
    claims(&mut daemon, 2, &[DESK]);
    let out = down(&mut daemon, 1);
    assert!(out.contains(&Output::Abandon {
        host: DESK,
        attempt: first
    }));
    assert!(
        dial_of(&out, DESK)
            .1
            .paths
            .contains(&DialPath::Through(LinkId(2)))
    );
}

#[test]
fn a_path_that_turns_an_attempt_away_is_left_out_of_the_next_sent_at_once_within_the_deadline() {
    let mut daemon = daemon(trusting());
    found(&mut daemon, DESK, &["192.168.1.9:7000"]);
    let (first, plan) = dial_of(&step(&mut daemon, Input::Dialling(Dialling::All)), DESK);
    let stranger = DialPath::Direct(addr("192.168.1.9:7000"));
    assert!(plan.paths.contains(&stranger));

    at(&mut daemon, 300);
    let out = ended(
        &mut daemon,
        DESK,
        first,
        AttemptOutcome::TurnedAway {
            path: stranger.clone(),
            error: "invalid peer certificate".into(),
        },
    );
    let (second, plan) = dial_of(&out, DESK);
    assert_ne!(second, first);
    assert_eq!(plan.paths, vec![DialPath::Direct(addr("10.0.0.1:7000"))]);
    assert_eq!(
        armed(&out, TimerId::Attempt(DESK)),
        None,
        "the deadline holds"
    );
    assert_eq!(armed(&out, TimerId::Retry(DESK)), None, "no pause");
    assert_eq!(daemon.last_dial_error(DESK), None);

    // Found there again, it is no new path for this run.
    assert_eq!(
        dials(&found(&mut daemon, DESK, &["192.168.1.9:7000"])),
        Vec::new()
    );
}

#[test]
fn a_run_with_every_path_turned_away_fails_with_the_last_answer_and_the_next_run_tries_them_all() {
    let mut daemon = daemon(trusting());
    let (first, _) = dial_of(&step(&mut daemon, Input::Dialling(Dialling::All)), DESK);
    let out = ended(
        &mut daemon,
        DESK,
        first,
        AttemptOutcome::TurnedAway {
            path: DialPath::Direct(addr("10.0.0.1:7000")),
            error: "the server refused to accept a new connection".into(),
        },
    );
    assert_eq!(dials(&out), Vec::new());
    assert!(out.contains(&Output::CancelTimer {
        id: TimerId::Attempt(DESK)
    }));
    let due = armed(&out, TimerId::Retry(DESK)).expect("a pause after the run");
    assert_eq!(
        daemon.last_dial_error(DESK),
        Some("the server refused to accept a new connection")
    );

    at(&mut daemon, due);
    let out = step(
        &mut daemon,
        Input::Timer {
            id: TimerId::Retry(DESK),
        },
    );
    assert_eq!(
        dial_of(&out, DESK).1.paths,
        vec![DialPath::Direct(addr("10.0.0.1:7000"))]
    );
}

#[test]
fn a_run_whose_other_paths_stay_silent_names_what_turned_it_away() {
    let mut daemon = daemon(trusting());
    found(&mut daemon, DESK, &["192.168.1.9:7000"]);
    let (first, _) = dial_of(&step(&mut daemon, Input::Dialling(Dialling::All)), DESK);
    ended(
        &mut daemon,
        DESK,
        first,
        AttemptOutcome::TurnedAway {
            path: DialPath::Direct(addr("192.168.1.9:7000")),
            error: "192.168.1.9:7000: invalid peer certificate".into(),
        },
    );
    at(&mut daemon, ATTEMPT_DEADLINE_MS);
    step(
        &mut daemon,
        Input::Timer {
            id: TimerId::Attempt(DESK),
        },
    );
    assert_eq!(
        daemon.last_dial_error(DESK),
        Some(
            "192.168.1.9:7000: invalid peer certificate; no other path answered within 10 seconds"
        )
    );
}

/// The relay attempt the step asked for.
fn relay_dial(out: &[Output]) -> AttemptId {
    out.iter()
        .find_map(|output| match output {
            Output::DialRelay { attempt } => Some(*attempt),
            _ => None,
        })
        .expect("a relay dial")
}

fn relay_ends(daemon: &mut Daemon, attempt: AttemptId, outcome: RelayOutcome) -> Vec<Output> {
    step(daemon, Input::RelayEnded { attempt, outcome })
}

fn relay_carriers(daemon: &mut Daemon, attempt: AttemptId) -> Option<RelayCarriers> {
    step(
        daemon,
        Input::RelayPrepared {
            attempt,
            host: "relay.example".into(),
        },
    );
    daemon.relay_reach().carriers(attempt)
}

#[test]
fn the_relay_is_dialled_once_signed_in_on_quic_with_tls_after_its_head_start() {
    let mut daemon = daemon(trusting());
    let attempt = relay_dial(&step(&mut daemon, Input::RelayWanted(true)));
    assert_eq!(daemon.relay_reach().carriers(attempt), None, "no host yet");
    assert_eq!(
        relay_carriers(&mut daemon, attempt),
        Some(RelayCarriers::Both {
            head_start_ms: TCP_HEAD_START_MS
        })
    );
    let again = relay_dial(&step(&mut daemon, Input::RelayWanted(true)));
    assert_ne!(again, attempt, "signing in again starts afresh");
    assert_eq!(daemon.relay_reach().carriers(attempt), None);
}

#[test]
fn a_relay_host_found_to_drop_udp_is_dialled_over_tls_alone_for_an_hour() {
    let mut daemon = daemon(trusting());
    let first = relay_dial(&step(&mut daemon, Input::RelayWanted(true)));
    relay_carriers(&mut daemon, first);
    step(
        &mut daemon,
        Input::RelayUdp {
            host: "relay.example".into(),
            reached: false,
        },
    );
    let out = relay_ends(&mut daemon, first, RelayOutcome::KeptTokenRefused);
    let second = relay_dial(&out);
    assert_eq!(
        relay_carriers(&mut daemon, second),
        Some(RelayCarriers::Tcp)
    );
    at(&mut daemon, UDP_BLOCKED_MS);
    let third = relay_dial(&relay_ends(
        &mut daemon,
        second,
        RelayOutcome::KeptTokenRefused,
    ));
    assert_eq!(
        relay_carriers(&mut daemon, third),
        Some(RelayCarriers::Both {
            head_start_ms: TCP_HEAD_START_MS
        }),
        "the memory lapsed"
    );
}

#[test]
fn a_lost_relay_link_is_dialled_again_after_a_second_doubling_to_five_minutes() {
    let mut daemon = daemon(trusting());
    let mut attempt = relay_dial(&step(&mut daemon, Input::RelayWanted(true)));
    let mut now = 0;
    let mut pauses = Vec::new();
    for _ in 0..11 {
        let out = relay_ends(&mut daemon, attempt, RelayOutcome::Lost("gone".into()));
        let due = armed(&out, TimerId::RelayRetry).expect("a pause");
        pauses.push(due - now);
        at(&mut daemon, due);
        now = due;
        attempt = relay_dial(&step(
            &mut daemon,
            Input::Timer {
                id: TimerId::RelayRetry,
            },
        ));
    }
    assert_eq!(
        pauses,
        vec![
            1_000, 2_000, 4_000, 8_000, 16_000, 32_000, 64_000, 128_000, 256_000, 300_000, 300_000
        ]
    );
}

#[test]
fn a_clean_close_or_a_link_that_lived_starts_the_relay_pause_afresh() {
    let mut daemon = daemon(trusting());
    let first = relay_dial(&step(&mut daemon, Input::RelayWanted(true)));
    let out = relay_ends(&mut daemon, first, RelayOutcome::Lost("gone".into()));
    let due = armed(&out, TimerId::RelayRetry).unwrap();
    at(&mut daemon, due);
    let second = relay_dial(&step(
        &mut daemon,
        Input::Timer {
            id: TimerId::RelayRetry,
        },
    ));
    step(&mut daemon, Input::RelayLinked { attempt: second });
    at(&mut daemon, due + RELAY_LIVED_MS);
    let out = relay_ends(&mut daemon, second, RelayOutcome::Lost("gone".into()));
    assert_eq!(
        armed(&out, TimerId::RelayRetry),
        Some(due + RELAY_LIVED_MS + RELAY_RETRY_FIRST_MS),
        "it lived: the pause is the first again"
    );

    let mut daemon = self::daemon(trusting());
    let first = relay_dial(&step(&mut daemon, Input::RelayWanted(true)));
    relay_ends(&mut daemon, first, RelayOutcome::Lost("gone".into()));
    at(&mut daemon, 1_000);
    let second = relay_dial(&step(
        &mut daemon,
        Input::Timer {
            id: TimerId::RelayRetry,
        },
    ));
    let out = relay_ends(&mut daemon, second, RelayOutcome::Closed);
    assert_eq!(armed(&out, TimerId::RelayRetry), Some(2_000));
}

#[test]
fn a_final_ending_stops_the_relay_until_the_profile_signs_in_again() {
    let mut daemon = daemon(trusting());
    let attempt = relay_dial(&step(&mut daemon, Input::RelayWanted(true)));
    let out = relay_ends(&mut daemon, attempt, RelayOutcome::Final("sign in".into()));
    assert_eq!(armed(&out, TimerId::RelayRetry), None);
    assert_eq!(daemon.relay_reach().attempt(), None);
    relay_dial(&step(&mut daemon, Input::RelayWanted(true)));
}

#[test]
fn signing_out_ends_the_relay_pause_and_a_late_ending_changes_nothing() {
    let mut daemon = daemon(trusting());
    let attempt = relay_dial(&step(&mut daemon, Input::RelayWanted(true)));
    relay_ends(&mut daemon, attempt, RelayOutcome::Lost("gone".into()));
    let out = step(&mut daemon, Input::RelayWanted(false));
    assert!(out.contains(&Output::CancelTimer {
        id: TimerId::RelayRetry
    }));
    let again = relay_dial(&step(&mut daemon, Input::RelayWanted(true)));
    step(&mut daemon, Input::RelayWanted(false));
    let out = relay_ends(&mut daemon, again, RelayOutcome::Lost("stopped".into()));
    assert_eq!(out, Vec::new());
}

#[test]
fn the_relay_pause_takes_jitter_from_the_cores_entropy() {
    let mut daemon = daemon(trusting());
    step(&mut daemon, Input::Entropy([7; 32]));
    let attempt = relay_dial(&step(&mut daemon, Input::RelayWanted(true)));
    let out = relay_ends(&mut daemon, attempt, RelayOutcome::Lost("gone".into()));
    let pause = armed(&out, TimerId::RelayRetry).unwrap();
    assert!((750..=6_250).contains(&pause), "{pause}");
    assert_ne!(pause, 1_000);
}

#[test]
fn a_pairing_dial_takes_the_found_addresses_first_then_the_requested_then_the_forwarders() {
    let mut daemon = daemon(trusting());
    found(&mut daemon, LAPTOP, &["192.168.1.7:7000"]);
    up(&mut daemon, 1, RELAY, CarrierKind::Relay);
    claims(&mut daemon, 1, &[LAPTOP]);
    let plan = daemon.pairing_dial(
        Some(LAPTOP),
        &[addr("10.0.0.9:7000"), addr("192.168.1.7:7000")],
    );
    assert_eq!(
        plan.addrs,
        vec![
            (addr("192.168.1.7:7000"), true),
            (addr("10.0.0.9:7000"), false)
        ]
    );
    assert_eq!(plan.short_try_ms, PAIRING_SHORT_TRY_MS);
    assert_eq!(plan.deadline_ms, PAIRING_DIAL_MS);
    assert_eq!(plan.forwarders, vec![(RELAY, true)]);

    let named = daemon.pairing_dial(None, &[addr("10.0.0.9:7000")]);
    assert_eq!(named.addrs, vec![(addr("10.0.0.9:7000"), false)]);
    assert_eq!(named.forwarders, Vec::new());
}

#[test]
fn a_network_change_ends_the_relay_pause_and_starts_the_count_afresh() {
    let mut daemon = daemon(trusting());
    let first = relay_dial(&step(&mut daemon, Input::RelayWanted(true)));
    let failed = relay_ends(&mut daemon, first, RelayOutcome::Lost("offline".into()));
    assert!(armed(&failed, TimerId::RelayRetry).is_some());
    let out = step(&mut daemon, Input::NetworkChanged);
    assert!(out.contains(&Output::CancelTimer {
        id: TimerId::RelayRetry
    }));
    let second = relay_dial(&out);
    assert!(
        step(
            &mut daemon,
            Input::Timer {
                id: TimerId::RelayRetry
            }
        )
        .is_empty(),
        "a cancelled pause cannot dial again"
    );
    at(&mut daemon, 10_000);
    let again = relay_ends(&mut daemon, second, RelayOutcome::Lost("offline".into()));
    assert_eq!(
        armed(&again, TimerId::RelayRetry),
        Some(10_000 + RELAY_RETRY_FIRST_MS),
        "the failure after the change pauses as a first one does"
    );
}

#[test]
fn a_network_change_leaves_an_attempt_out_or_a_link_up_alone() {
    let mut daemon = daemon(trusting());
    assert!(step(&mut daemon, Input::NetworkChanged).is_empty());
    let first = relay_dial(&step(&mut daemon, Input::RelayWanted(true)));
    assert!(step(&mut daemon, Input::NetworkChanged).is_empty());
    step(&mut daemon, Input::RelayLinked { attempt: first });
    assert!(step(&mut daemon, Input::NetworkChanged).is_empty());
    assert_eq!(daemon.relay_reach().attempt(), Some(first));
}

#[test]
fn a_network_change_never_overrides_sign_out_or_a_final_relay_refusal() {
    for signed_out in [true, false] {
        let mut daemon = daemon(trusting());
        let first = relay_dial(&step(&mut daemon, Input::RelayWanted(true)));
        if signed_out {
            step(&mut daemon, Input::RelayWanted(false));
        } else {
            relay_ends(
                &mut daemon,
                first,
                RelayOutcome::Final("sign in again".into()),
            );
        }
        assert!(step(&mut daemon, Input::NetworkChanged).is_empty());
        assert_eq!(daemon.relay_reach().attempt(), None);
    }
}
