//! The pairing window as a first run uses it: one window that takes the
//! PIN or the QR code, a QR code that is the cloud's pairing page, the
//! network advertisement saying while it is open, and its status as a
//! stream that says when a joiner is confirming and whom it paired.

#![cfg(unix)]

use support::*;
use testnet::{Net, PATIENCE, Topology};
use wire::{Empty, StartPairingRequest, begin_pair_request, pairing_window, start_pairing_request};

use crate::support;

const SCOPE: &str = "first-run";

fn either_window() -> StartPairingRequest {
    StartPairingRequest {
        method: Some(start_pairing_request::Method::PinOrQr(Empty {})),
        ..StartPairingRequest::default()
    }
}

/// The next status the stream says, within the patience.
async fn next(statuses: &mut tonic::Streaming<wire::PairingStatus>) -> wire::PairingStatus {
    tokio::time::timeout(PATIENCE, statuses.message())
        .await
        .expect("the status stream says something")
        .expect("the status stream stays up")
        .expect("the status stream does not end")
}

/// Whether the candidate `host` shows on `on`'s inventory has its window
/// open, once the inventory says `open`.
async fn candidate_open(net: &Net, on: &str, host: &str, open: bool) {
    let host = host_id(net, host);
    let mut fleet = net.observe_inventory(on).await.unwrap();
    fleet
        .observe_until(
            |events| {
                testnet::observe::inventory_hosts(events)
                    .into_iter()
                    .find(|row| row.host_id() == host.as_bytes())
                    .and_then(|row| row.candidate().map(|candidate| candidate.pairing_open))
                    == Some(open)
            },
            PATIENCE,
        )
        .await
        .unwrap_or_else(|stuck| panic!("the candidate's window open {open}: {stuck}"));
}

/// The QR secret pairs, the PIN of the same window then pairs nothing, the
/// QR code is the cloud's page, and the window's watchers hear the joiner
/// confirming, then whom it paired. The laptop's inventory shows the desk's
/// window open while it is, and closed after.
#[tokio::test(flavor = "multi_thread")]
async fn one_window_takes_the_qr_code_and_says_who_paired() {
    let net = Net::start(
        Topology::new()
            .host_decl(lan_host("desk", SCOPE))
            .host_decl(lan_host("laptop", SCOPE)),
    )
    .await
    .unwrap();
    let desk = host_id(&net, "desk");
    candidate_open(&net, "laptop", "desk", false).await;

    let mut statuses = admin(&net, "desk")
        .await
        .watch_pairing(Empty {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(next(&mut statuses).await, wire::PairingStatus::default());

    let started = start_pairing(&net, "desk", either_window()).await.unwrap();
    let Some(pairing_window::Secret::PinAndQr(secrets)) = started.secret.clone() else {
        panic!("the window answers both secrets: {started:?}");
    };
    assert_eq!(secrets.pin.len(), 6);
    assert!(
        started.qr_link.starts_with("https://amux.sh/p#"),
        "a signed-out desk's QR code is amux.sh's page: {}",
        started.qr_link
    );
    let invitation = node::parse_pair_link(&started.qr_link).unwrap();
    assert_eq!(invitation.host_id, desk);
    assert_eq!(invitation.secret, secrets.qr_secret);
    assert!(next(&mut statuses).await.active);
    candidate_open(&net, "laptop", "desk", true).await;

    let pending = begin_pair(
        &net,
        "laptop",
        Some(desk),
        begin_pair_request::Secret::QrSecret(invitation.secret),
        invitation.addrs.iter().map(ToString::to_string).collect(),
    )
    .await
    .expect("the QR secret pairs");
    let joining = next(&mut statuses).await;
    assert!(joining.active && joining.joining, "{joining:?}");
    let peer = confirm_pair(&net, "laptop", pending.token).await.unwrap();
    assert_eq!(peer.host_id, desk.as_bytes());

    let mut done = next(&mut statuses).await;
    while done.active || done.joining {
        done = next(&mut statuses).await;
    }
    let paired = done.paired.expect("the window says whom it paired");
    assert_eq!(paired.host_id, host_id(&net, "laptop").as_bytes());
    assert_eq!(paired.name, "laptop");
    let polled = admin(&net, "desk")
        .await
        .get_pairing_status(Empty {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        polled.paired.map(|peer| peer.name).as_deref(),
        Some("laptop")
    );
    println!("the QR secret of a PIN-or-QR window paired, and the stream said who");
    net.shutdown().await.unwrap();
}

/// The same window pairs by its PIN, and closes with the desk's
/// advertisement saying so to a host still looking.
#[tokio::test(flavor = "multi_thread")]
async fn one_window_takes_the_pin_and_the_advertisement_follows_it() {
    let net = Net::start(
        Topology::new()
            .host_decl(lan_host("desk", SCOPE))
            .host_decl(lan_host("laptop", SCOPE))
            .host_decl(lan_host("tablet", SCOPE)),
    )
    .await
    .unwrap();
    let desk = host_id(&net, "desk");
    let started = start_pairing(&net, "desk", either_window()).await.unwrap();
    let Some(pairing_window::Secret::PinAndQr(secrets)) = started.secret.clone() else {
        panic!("the window answers both secrets: {started:?}");
    };
    candidate_open(&net, "tablet", "desk", true).await;
    let pending = begin_pair(
        &net,
        "laptop",
        Some(desk),
        begin_pair_request::Secret::Pin(secrets.pin),
        started.addrs.clone(),
    )
    .await
    .expect("the PIN pairs");
    confirm_pair(&net, "laptop", pending.token).await.unwrap();
    candidate_open(&net, "tablet", "desk", false).await;
    let refused = begin_pair(
        &net,
        "tablet",
        Some(desk),
        begin_pair_request::Secret::QrSecret(secrets.qr_secret),
        started.addrs,
    )
    .await
    .expect_err("one pairing uses the window up, both secrets with it");
    println!("PIN paired; the QR secret after it: {refused:?}");
    net.shutdown().await.unwrap();
}
