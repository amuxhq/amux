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

fn down(daemon: &mut Daemon, link: u64) -> Vec<Output> {
    step(
        daemon,
        Input::LinkDown {
            link: LinkId(link),
            reason: wire::LinkCloseReason::UserShutdown,
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
