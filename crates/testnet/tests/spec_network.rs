//! The network edge between whole daemons, past what the first edge
//! specification shows: pairing windows that expire and lock out, keys
//! that do not match, profiles and accounts that share nothing, revocation
//! closing what a link carries, the relay's QUIC carrier and its TCP
//! fallback, credential refresh under a live link, malformed peer calls,
//! and a slow subscriber beside one that keeps up.
//!
//! Every host is a production daemon in this process. Direct links run over
//! loopback QUIC, discovery is the net's scripted bus, and the relay is a
//! production relay server on loopback beside a stand-in cloud, its
//! credentials timed by the net's policy clock.

#![cfg(unix)]

mod support;

use std::time::Duration;

use node::RelayCarrier;
use node::harness::{Advertisement, HostVia};
use store::Store as _;
use support::*;
use testnet::{Net, RELAY_HOST, Topology, holds_for, until};
use wire::begin_pair_request;

#[tokio::test(flavor = "multi_thread")]
async fn hosts_on_one_account_reach_each_other_through_the_relay_over_quic_or_tcp() {
    let mut net = Net::start(Topology::new().relay(&["ada"]).host("desk").host("phone"))
        .await
        .unwrap();
    net.trust("desk", "phone").await.unwrap();
    net.trust("phone", "desk").await.unwrap();
    let (desk, phone) = (host_id(&net, "desk"), host_id(&net, "phone"));

    // The phone is on a network that eats UDP: its QUIC dial to the relay
    // never answers, and the TCP carrier started a moment later wins.
    net.block_udp("phone", true).unwrap();
    net.sign_in("desk", "ada").await.unwrap();
    net.sign_in("phone", "ada").await.unwrap();
    assert_eq!(carrier(&net, "desk"), Some(RelayCarrier::Quic));
    assert_eq!(carrier(&net, "phone"), Some(RelayCarrier::Tcp));
    println!("desk on the relay over QUIC; phone, UDP blocked, over TCP");

    // One relay joins the two carriers: each host reaches the other and
    // calls it, through a tunnel the relay pipes but cannot read.
    until_via(&net, "desk", "phone", HostVia::Relay).await;
    let from_phone = peer_inventory_hosts(&edge(&net, "phone"), desk)
        .await
        .expect("the phone calls the desk through the relay");
    let from_desk = peer_inventory_hosts(&edge(&net, "desk"), phone)
        .await
        .expect("the desk calls the phone through the relay");
    assert!(from_phone.contains(&desk.as_bytes().to_vec()));
    assert!(from_desk.contains(&phone.as_bytes().to_vec()));
    let mut links = net.relay().unwrap().links("ada").await;
    links.sort();
    let mut expected = vec![(desk, 1), (phone, 1)];
    expected.sort();
    assert_eq!(links, expected, "one relay link per host");
    println!("desk and phone call each other through the relay");

    // The relay carries links but serves nothing: a stream addressed to
    // the relay itself is refused.
    let relay_id = net.relay().unwrap().host_id();
    let refused = peer_inventory_hosts(&edge(&net, "phone"), relay_id)
        .await
        .expect_err("the relay itself is not adjacent");
    println!("a call addressed to the relay itself: {:?}", refused.code());

    // The phone remembers the blocked network, and dials TCP alone until
    // the memory expires on the policy clock, even once UDP is back.
    until("the phone to remember UDP is blocked", || async {
        edge(&net, "phone")
            .remembers_udp_blocked(RELAY_HOST)
            .then_some(())
            .ok_or("not remembered")
    })
    .await
    .unwrap();
    net.block_udp("phone", false).unwrap();
    net.advance(node::UDP_BLOCKED_MEMORY - Duration::from_millis(1))
        .unwrap();
    assert!(edge(&net, "phone").remembers_udp_blocked(RELAY_HOST));
    reconnect(&net, "phone").await;
    assert_eq!(carrier(&net, "phone"), Some(RelayCarrier::Tcp));
    net.advance(Duration::from_millis(1)).unwrap();
    assert!(!edge(&net, "phone").remembers_udp_blocked(RELAY_HOST));
    reconnect(&net, "phone").await;
    assert_eq!(carrier(&net, "phone"), Some(RelayCarrier::Quic));
    until_via(&net, "desk", "phone", HostVia::Relay).await;
    println!("memory held TCP-only until it expired; the next link rides QUIC");

    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relay_link_whose_credential_lapses_at_the_relay_first_comes_back_on_a_fresh_one() {
    let net = Net::start(Topology::new().relay(&["ada"]).host("desk"))
        .await
        .unwrap();
    // The desk's clock is six minutes behind the relay's: it refreshes five
    // minutes before the expiry it is told, a minute after the relay has
    // already let the credential lapse.
    let ahead = Duration::from_secs(6 * 60);
    net.relay().unwrap().run_ahead(ahead);
    net.sign_in("desk", "ada").await.unwrap();
    let connects = net.relay().unwrap().connects().len();

    net.advance(testnet::CREDENTIAL_TTL - ahead + Duration::from_secs(1))
        .unwrap();
    until("the desk to fetch a fresh credential", || async {
        let now = net.relay().unwrap().connects().len();
        (now > connects)
            .then_some(())
            .ok_or_else(|| format!("{now} connects, {connects} before"))
    })
    .await
    .unwrap();
    until("the desk's relay link to come back", || async {
        carrier(&net, "desk").map(|_| ()).ok_or("no carrier")
    })
    .await
    .unwrap();
    assert_eq!(
        net.relay().unwrap().links("ada").await,
        vec![(host_id(&net, "desk"), 1)]
    );
    println!("the relay let the credential lapse first; the desk reconnected on a fresh one");

    net.shutdown().await.unwrap();
}

/// Pauses and resumes the host's profile, which closes its relay link and
/// dials a new one, and waits for the new one.
async fn reconnect(net: &Net, host: &str) {
    let mut door = door(net, host).await;
    door.pause_profile(operation(profile(net, host)))
        .await
        .expect("the profile pauses");
    until(&format!("{host}'s relay link to close"), || async {
        match carrier(net, host) {
            None => Ok(()),
            Some(carrier) => Err(format!("still on {carrier:?}")),
        }
    })
    .await
    .unwrap();
    door.resume_profile(operation(profile(net, host)))
        .await
        .expect("the profile resumes");
    until(&format!("{host}'s relay link to come back"), || async {
        carrier(net, host).map(|_| ()).ok_or("no carrier")
    })
    .await
    .unwrap();
}

/// The refusal every failed secret reads as, so a guesser learns nothing
/// from which way it failed.
fn is_invalid_secret(status: &tonic::Status) -> bool {
    status.code() == tonic::Code::PermissionDenied && status.message() == "INVALID_PIN"
}

/// A PIN one off from `pin`.
fn wrong_pin(pin: &str) -> String {
    format!("{:06}", (pin.parse::<u32>().unwrap() + 1) % 1_000_000)
}

// Every host in the net dials from 127.0.0.1, and a LAN listener admits ten
// new handshakes a minute from one address, so each part of this story
// pairs with a responder of its own.
#[tokio::test(flavor = "multi_thread")]
async fn a_pairing_window_is_one_shot_attempt_capped_and_expires() {
    let net = Net::start(
        Topology::new()
            .host_decl(lan_host("desk", "home"))
            .host_decl(lan_host("shed", "home"))
            .host_decl(lan_host("attic", "home"))
            .host_decl(testnet::HostDecl {
                name: "porch".to_owned(),
                lan: true,
                ..testnet::HostDecl::default()
            })
            .host_decl(lan_host("laptop", "home"))
            .host_decl(lan_host("intruder", "home")),
    )
    .await
    .unwrap();
    let (desk, shed, attic) = (
        host_id(&net, "desk"),
        host_id(&net, "shed"),
        host_id(&net, "attic"),
    );

    // One responder at a time, whatever the secret's form.
    let started = start_pairing(&net, "desk", pin_mode()).await.unwrap();
    for second in [pin_mode(), qr_mode()] {
        let refused = start_pairing(&net, "desk", second)
            .await
            .expect_err("a second window while one is open");
        assert_eq!(refused.code(), tonic::Code::FailedPrecondition);
        assert_eq!(refused.message(), "PAIR_MODE_ALREADY_ACTIVE");
    }
    let pin = pin_of(&started);

    // A wrong, a malformed and a wrong-length secret fail alike and commit
    // nothing, and the window stays open.
    for (what, secret) in [
        (
            "a wrong PIN",
            begin_pair_request::Secret::Pin(wrong_pin(&pin)),
        ),
        (
            "a malformed PIN",
            begin_pair_request::Secret::Pin("12ab56".to_owned()),
        ),
        (
            "a QR secret of the wrong length",
            begin_pair_request::Secret::QrSecret(vec![7; 5]),
        ),
    ] {
        let status = begin_pair(&net, "laptop", Some(desk), secret, started.addrs.clone())
            .await
            .expect_err("a bad secret pairs nothing");
        assert!(
            is_invalid_secret(&status),
            "{what} reads as every other refusal, not {status:?}"
        );
    }
    assert!(edge(&net, "desk").trusted().is_empty());
    assert!(edge(&net, "laptop").trusted().is_empty());
    assert!(edge(&net, "desk").pairing_active());

    // The correct PIN, at an address alone: the initiator learns whom it
    // reached from the exchange, and nothing is trusted before the person
    // confirms it. Abandoning releases the attempt, so more abandoned
    // attempts than the guess cap still leave the PIN usable.
    let now_ms = chrono::Utc::now().timestamp_millis();
    for _ in 0..6 {
        let pending = begin_pair(
            &net,
            "laptop",
            None,
            begin_pair_request::Secret::Pin(pin.clone()),
            started.addrs.clone(),
        )
        .await
        .expect("the correct PIN");
        let peer = pending.peer.clone().unwrap();
        assert_eq!(peer.host_id, desk.as_bytes());
        assert_eq!(peer.name, "desk");
        assert_eq!(peer.pubkey, edge(&net, "desk").public_key());
        assert!(peer.expires_at_unix_ms > now_ms);
        assert!(peer.expires_at_unix_ms <= now_ms + 5 * 60 * 1000 + 1000);
        assert!(edge(&net, "desk").trusted().is_empty());
        assert!(edge(&net, "laptop").trusted().is_empty());
        door(&net, "laptop")
            .await
            .abandon_pair(wire::ProfilePendingPairRequest {
                profile_id: profile(&net, "laptop"),
                pairing: Some(wire::PendingPairRequest {
                    token: pending.token,
                }),
                ..wire::ProfilePendingPairRequest::default()
            })
            .await
            .expect("the person abandons the pairing");
    }
    assert!(edge(&net, "desk").trusted().is_empty());
    println!(
        "three bad secrets refused alike; six abandoned attempts left the PIN usable and \
         nothing trusted"
    );
    let pending = begin_pair(
        &net,
        "laptop",
        None,
        begin_pair_request::Secret::Pin(pin.clone()),
        started.addrs.clone(),
    )
    .await
    .unwrap();
    confirm_pair(&net, "laptop", pending.token).await.unwrap();
    until("the desk to trust the laptop", || async {
        edge(&net, "desk")
            .is_trusted(host_id(&net, "laptop"))
            .then_some(())
            .ok_or("not trusted")
    })
    .await
    .unwrap();

    // The first success consumes the PIN: the window closes and a second
    // machine cannot race in on it. A host with no window open refuses the
    // connection before reading any PIN, and the initiator answers that
    // refusal with the same INVALID_PIN as a wrong guess, whether it lands
    // during the dial or on the call after it.
    assert!(!edge(&net, "desk").pairing_active());
    let late = begin_pair(
        &net,
        "intruder",
        Some(desk),
        begin_pair_request::Secret::Pin(pin),
        started.addrs.clone(),
    )
    .await
    .expect_err("a consumed PIN pairs nothing");
    assert!(is_invalid_secret(&late), "{late:?}");
    assert!(!edge(&net, "desk").is_trusted(host_id(&net, "intruder")));
    println!("the PIN paired the laptop once; the intruder's late try: {late:?}");

    // Five wrong guesses close the window; after that the right PIN is
    // refused like the consumed one until someone opens a new window.
    let started = start_pairing(&net, "shed", pin_mode()).await.unwrap();
    let pin = pin_of(&started);
    for _ in 0..5 {
        let status = begin_pair(
            &net,
            "intruder",
            Some(shed),
            begin_pair_request::Secret::Pin(wrong_pin(&pin)),
            started.addrs.clone(),
        )
        .await
        .expect_err("a wrong guess");
        assert!(is_invalid_secret(&status), "{status:?}");
    }
    assert!(!edge(&net, "shed").pairing_active());
    let closed = begin_pair(
        &net,
        "intruder",
        Some(shed),
        begin_pair_request::Secret::Pin(pin),
        started.addrs.clone(),
    )
    .await
    .expect_err("the right PIN after the cap");
    assert!(is_invalid_secret(&closed), "{closed:?}");
    assert!(edge(&net, "shed").trusted().is_empty());
    println!("five wrong guesses closed the window; the right PIN then: {closed:?}");

    // A window lasts its time and no longer, and a new one opens after it.
    let started = start_pairing(
        &net,
        "attic",
        wire::StartPairingRequest {
            ttl_seconds: Some(1),
            ..pin_mode()
        },
    )
    .await
    .unwrap();
    assert_eq!(started.ttl_seconds, 1);
    until("the window to expire", || async {
        (!edge(&net, "attic").pairing_active())
            .then_some(())
            .ok_or("pairing still active")
    })
    .await
    .unwrap();
    let expired = begin_pair(
        &net,
        "intruder",
        Some(attic),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        started.addrs.clone(),
    )
    .await
    .expect_err("an expired PIN");
    assert!(is_invalid_secret(&expired), "{expired:?}");
    let started = start_pairing(&net, "attic", pin_mode())
        .await
        .expect("a new window after expiry");

    // A host cannot pair with itself: by its own id before anything is
    // dialled, and by the key it meets when it dials its own listener.
    for named in [Some(attic), None] {
        let refused = begin_pair(
            &net,
            "attic",
            named,
            begin_pair_request::Secret::Pin(pin_of(&started)),
            started.addrs.clone(),
        )
        .await
        .expect_err("pairing with itself");
        assert_eq!(refused.code(), tonic::Code::InvalidArgument);
        assert_eq!(refused.message(), "SELF_PAIRING");
    }
    assert!(edge(&net, "attic").trusted().is_empty());
    println!("the window expired after its second; self-pairing refused by id and by key");

    // A QR code carries the responder's addresses, so it pairs with no
    // discovery and no account; an address that answers nothing, a stale
    // interface say, gets one short try before the next.
    let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let started = start_pairing(&net, "porch", qr_mode()).await.unwrap();
    let Some(wire::start_pairing_response::Secret::QrSecret(secret)) = started.secret else {
        panic!("a QR window hands out a secret");
    };
    let mut addrs = vec![silent.local_addr().unwrap().to_string()];
    addrs.extend(started.addrs.clone());
    let begun = tokio::time::Instant::now();
    let pending = begin_pair(
        &net,
        "laptop",
        Some(host_id(&net, "porch")),
        begin_pair_request::Secret::QrSecret(secret),
        addrs,
    )
    .await
    .expect("the QR pairs past the silent address");
    let took = begun.elapsed();
    assert!(took < Duration::from_secs(4), "{took:?}");
    assert_eq!(pending.via, wire::PeerVia::Direct as i32);
    confirm_pair(&net, "laptop", pending.token).await.unwrap();
    println!("a QR code paired over its second address after {took:?} on the silent first");

    net.shutdown().await.unwrap();
}

/// The desk's advertisement as its own daemon makes it, at `addr`.
fn advert(net: &Net, host: &str, addr: std::net::SocketAddr, scope: &str) -> Advertisement {
    Advertisement {
        host_id: host_id(net, host),
        name: host.to_owned(),
        version: wire::PROTOCOL_VERSION,
        addrs: vec![addr],
        scope: scope.to_owned(),
    }
}

/// Holds for a second that `check` stays false: a window, kept because
/// nothing the test controls gates the dial that must not succeed.
async fn never(what: &str, check: impl AsyncFn() -> bool) {
    holds_for(what, Duration::from_secs(1), || {
        let check = &check;
        async move { !check().await }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_that_does_not_match_the_pinned_one_is_refused() {
    let mut net = Net::start(
        Topology::new()
            .host_decl(lan_host("desk", "home"))
            .host_decl(lan_host("laptop", "home"))
            .host_decl(lan_host("impostor", "home")),
    )
    .await
    .unwrap();
    let (desk, laptop, impostor) = (
        host_id(&net, "desk"),
        host_id(&net, "laptop"),
        host_id(&net, "impostor"),
    );
    pair(&net, "laptop", "desk").await;
    until_via(&net, "laptop", "desk", HostVia::Direct).await;
    let desk_key = edge(&net, "desk").public_key().to_vec();

    // A machine on the network that nobody paired is a candidate to pair
    // with, and nothing more: it is never dialled for a trusted link.
    assert!(
        edge(&net, "laptop")
            .candidates()
            .iter()
            .any(|advert| advert.host_id == impostor)
    );
    never("the laptop links to an unpaired machine", async || {
        edge(&net, "laptop").via(impostor).await != HostVia::Offline
    })
    .await;

    // With the desk gone, a machine claims the desk's id at its own
    // address. The laptop dials it as the desk and meets a key it never
    // pinned: no link, and its trust in the desk's real key stands.
    net.stop_daemon("desk").await.unwrap();
    let impostor_addr = edge(&net, "impostor").lan_addr().unwrap();
    net.discovery()
        .announce(advert(&net, "desk", impostor_addr, "home"));
    until(
        "the laptop to dial the claimed address and fail",
        || async {
            let error = edge(&net, "laptop").last_dial_error(desk).await;
            error
                .as_ref()
                .is_some_and(|error| error.contains(&impostor_addr.to_string()))
                .then_some(())
                .ok_or_else(|| format!("last dial error: {error:?}"))
        },
    )
    .await
    .unwrap();
    // The dialer checks the answering key before it presents its own, so
    // the stranger never sees the laptop's certificate.
    let refused = edge(&net, "laptop").last_dial_error(desk).await.unwrap();
    assert!(
        refused.contains(&format!("{impostor_addr}: QUIC handshake failed"))
            && refused.contains("invalid peer certificate"),
        "{refused}"
    );
    // The dial's outcome is in: the stranger's key never matches the pin.
    assert_eq!(
        edge(&net, "laptop").via(desk).await,
        HostVia::Offline,
        "the laptop never links to a stranger claiming the desk's id"
    );
    let pinned = edge(&net, "laptop")
        .trusted()
        .into_iter()
        .find(|(host, ..)| *host == desk)
        .expect("the laptop still trusts the desk");
    assert_eq!(pinned.2, desk_key, "the pinned key is unchanged");
    assert!(!edge(&net, "impostor").is_trusted(laptop));
    println!("a stranger advertising the desk's id at {impostor_addr} was refused: {refused}");
    net.discovery().withdraw_host(desk);

    // The desk comes back with a new key under the same id. The laptop's
    // pinned key no longer matches, so neither side can link, until the two
    // pair again and the new key replaces the old.
    let key = node::harness::device_key_path(&node::profile_dir(
        &net.host("desk").unwrap().data_dir,
        net.host("desk").unwrap().profile,
    ));
    std::fs::remove_file(&key).unwrap();
    net.restart_daemon("desk").await.unwrap();
    assert_eq!(host_id(&net, "desk"), desk);
    let rotated = edge(&net, "desk").public_key().to_vec();
    assert_ne!(rotated, desk_key, "a new key");
    // Each side's dial fails on the other's key: the laptop stores a new
    // error (the one from the stranger is still held), the rotated desk
    // its first.
    until("the laptop's dial to the rotated desk to fail", || async {
        match edge(&net, "laptop").last_dial_error(desk).await {
            Some(error) if error != refused => Ok(error),
            other => Err(format!("{other:?}")),
        }
    })
    .await
    .unwrap();
    until("the rotated desk's dial to the laptop to fail", || async {
        edge(&net, "desk")
            .last_dial_error(laptop)
            .await
            .ok_or("no dial error stored")
    })
    .await
    .unwrap();
    assert_eq!(edge(&net, "laptop").via(desk).await, HostVia::Offline);
    assert_eq!(edge(&net, "desk").via(laptop).await, HostVia::Offline);
    assert!(
        peer_inventory_hosts(&edge(&net, "laptop"), desk)
            .await
            .is_err()
    );
    pair(&net, "desk", "laptop").await;
    until_via(&net, "laptop", "desk", HostVia::Direct).await;
    let pinned = edge(&net, "laptop")
        .trusted()
        .into_iter()
        .find(|(host, ..)| *host == desk)
        .unwrap();
    assert_eq!(pinned.2, rotated, "pairing again replaced the pinned key");
    peer_inventory_hosts(&edge(&net, "laptop"), desk)
        .await
        .expect("the laptop calls the rotated desk");
    peer_inventory_hosts(&edge(&net, "desk"), laptop)
        .await
        .expect("the rotated desk calls the laptop");
    println!("the rotated desk was refused on its old key and linked again after pairing");

    net.shutdown().await.unwrap();
}

/// The direct addresses `host` keeps for `peer`.
fn stored_addrs(net: &Net, host: &str, peer: &str) -> Vec<String> {
    let peer = host_id(net, peer);
    edge(net, host)
        .list_peers()
        .unwrap()
        .into_iter()
        .find(|entry| entry.host_id == peer.as_bytes())
        .expect("a paired host")
        .reachabilities
        .into_iter()
        .filter_map(|reachability| match reachability.kind {
            Some(wire::peer_reachability::Kind::Direct(direct)) => Some(direct.addrs),
            _ => None,
        })
        .flatten()
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_advertises_a_listener_and_finds_a_paired_host_at_its_new_address() {
    // The phone listens for nothing, as a phone does: it dials, and only
    // what discovery finds or it stored tells it where. The desk first
    // listens on a socket the case holds, so its old port stays bound and
    // answers nothing once it stops.
    let held = std::sync::Arc::new(std::net::UdpSocket::bind("127.0.0.1:0").unwrap());
    let handed = std::sync::Mutex::new(Some(held.clone()));
    let mut net = Net::start_with(
        Topology::new()
            .host_decl(lan_host("desk", "home"))
            .host_decl(lan_host("phone", "home"))
            .host_decl(testnet::HostDecl {
                lan: false,
                ..lan_host("quiet", "home")
            }),
        testnet::NetOptions {
            edge: Some(std::sync::Arc::new(move |host, edge| {
                if host == "phone" {
                    edge.lan = None;
                    edge.dial = true;
                }
                if host == "desk"
                    && let Some(lan) = &mut edge.lan
                {
                    lan.socket = handed.lock().unwrap().take();
                }
            })),
            ..testnet::NetOptions::default()
        },
    )
    .await
    .unwrap();
    let (desk, phone, quiet) = (
        host_id(&net, "desk"),
        host_id(&net, "phone"),
        host_id(&net, "quiet"),
    );

    // Only a profile that listens advertises.
    until("the phone to find the desk", || async {
        let candidates = edge(&net, "phone").candidates();
        candidates
            .iter()
            .any(|advert| advert.host_id == desk)
            .then_some(())
            .ok_or_else(|| format!("candidates: {candidates:?}"))
    })
    .await
    .unwrap();
    let advertised = |host| {
        edge(&net, "desk")
            .candidates()
            .iter()
            .any(|advert| advert.host_id == host)
    };
    assert!(!advertised(quiet), "a profile without a listener");
    assert!(!advertised(phone), "a phone listens for nothing");

    // Trusted after the desk was found, the phone waits for the network to
    // name the desk again, and dials it the moment it does.
    net.trust("desk", "phone").await.unwrap();
    net.trust("phone", "desk").await.unwrap();
    never(
        "the phone to dial before the desk is found again",
        async || edge(&net, "phone").via(desk).await != HostVia::Offline,
    )
    .await;
    let first = edge(&net, "desk").lan_addr().unwrap();
    assert_eq!(first, held.local_addr().unwrap());
    net.discovery()
        .announce(advert(&net, "desk", first, "home"));
    until_via(&net, "phone", "desk", HostVia::Direct).await;
    // The dialler stores an address once its link is established, just
    // after routing already names the link.
    until("the phone to store the desk's address", || async {
        let stored = stored_addrs(&net, "phone", "desk");
        (stored == vec![first.to_string()])
            .then_some(())
            .ok_or_else(|| format!("stored: {stored:?}"))
    })
    .await
    .unwrap();
    println!("the desk found at {first}: the phone dialled it at once and stored the address");

    // The desk stops and withdraws, and comes back on another port while
    // its old one answers nothing. The address discovery finds is dialled
    // before the stale one the phone stored, and replaces it.
    net.stop_daemon("desk").await.unwrap();
    until("the desk's advertisement to go", || async {
        let candidates = edge(&net, "phone").candidates();
        (!candidates.iter().any(|advert| advert.host_id == desk))
            .then_some(())
            .ok_or_else(|| format!("candidates: {candidates:?}"))
    })
    .await
    .unwrap();
    net.restart_daemon("desk").await.unwrap();
    let second = edge(&net, "desk").lan_addr().unwrap();
    assert_ne!(second, first);
    let started = tokio::time::Instant::now();
    until_via(&net, "phone", "desk", HostVia::Direct).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the found address was dialled before the stale one timed out"
    );
    until("the phone to store the desk's new address", || async {
        let stored = stored_addrs(&net, "phone", "desk");
        (stored == vec![second.to_string()])
            .then_some(())
            .ok_or_else(|| format!("stored: {stored:?}"))
    })
    .await
    .unwrap();
    peer_inventory_hosts(&edge(&net, "phone"), desk)
        .await
        .expect("the phone calls the desk at its new address");
    println!("the desk back at {second}; the stale {first} was never waited on");

    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_a_host_closes_what_it_holds_open_over_a_direct_link_and_over_the_relay() {
    // The phone reaches the desk directly, with the relay beside it as a
    // fallback; the tablet reaches it through the relay alone.
    let mut net = Net::start_with(
        Topology::new()
            .relay(&["ada"])
            .host_decl(testnet::HostDecl {
                account: Some("ada".to_owned()),
                ..lan_host("desk", "home")
            })
            .host_decl(testnet::HostDecl {
                account: Some("ada".to_owned()),
                ..lan_host("phone", "home")
            })
            .host_decl(testnet::HostDecl {
                name: "tablet".to_owned(),
                account: Some("ada".to_owned()),
                ..testnet::HostDecl::default()
            })
            .agent(
                testnet::AgentDecl::new("worker", "desk")
                    .prompt("Watch the build.")
                    .steps(vec![
                        provider_fakes::script::Step::Text {
                            chunks: vec!["Watching.".to_owned()],
                        },
                        provider_fakes::script::Step::TurnEnd,
                    ]),
            ),
        testnet::NetOptions::default(),
    )
    .await
    .unwrap();
    let desk = host_id(&net, "desk");
    let worker = net.agent("worker").unwrap().id;
    pair(&net, "phone", "desk").await;
    net.trust("desk", "tablet").await.unwrap();
    net.trust("tablet", "desk").await.unwrap();
    until_via(&net, "phone", "desk", HostVia::Direct).await;
    until_via(&net, "tablet", "desk", HostVia::Relay).await;

    // Each holds the worker's session and the desk's inventory open.
    let mut held = Vec::new();
    for host in ["phone", "tablet"] {
        let session = open_session(&edge(&net, host), desk, worker)
            .await
            .unwrap_or_else(|status| panic!("{host} opens the worker's session: {status:?}"));
        let inventory = edge(&net, host)
            .peer(desk)
            .await
            .unwrap()
            .subscribe_inventory(wire::Empty {})
            .await
            .unwrap()
            .into_inner();
        held.push((host, session, inventory));
    }

    // Revoking closes every stream the revoked host holds, at once, and
    // neither route lets it back in: the desk no longer holds its key, and
    // says so when the host tries. The relay may still say the host is
    // online, as it says of any host on the account; saying so grants
    // nothing.
    for (host, mut session, mut inventory) in held {
        let id = host_id(&net, host);
        net.untrust("desk", host).await.unwrap();
        let session_end = ended(&mut session, &format!("{host}'s session")).await;
        let inventory_end = ended(&mut inventory, &format!("{host}'s inventory")).await;
        let refused = open_session(&edge(&net, host), desk, worker)
            .await
            .expect_err("a revoked host opens nothing");
        let unasked = peer_inventory_hosts(&edge(&net, "desk"), id)
            .await
            .expect_err("the desk calls no host it forgot");
        // The desk refuses the key by name, on either route, so the host
        // can tell it is no longer trusted from any other failure.
        assert_eq!(
            (refused.code(), refused.message()),
            (tonic::Code::Unauthenticated, node::TRUST_REVOKED),
            "{host}'s reopen is refused as no longer trusted"
        );
        println!(
            "{host} revoked: session {session_end}, inventory {inventory_end}; \
             the desk calling it: {:?}; the desk sees it {:?}",
            unasked.code(),
            edge(&net, "desk").via(id).await,
        );
    }
    never("a revoked host gets a call through", async || {
        peer_inventory_hosts(&edge(&net, "phone"), desk)
            .await
            .is_ok()
            || peer_inventory_hosts(&edge(&net, "tablet"), desk)
                .await
                .is_ok()
    })
    .await;

    // The relay's word that the tablet is online is what lets it pair
    // again, by the code the desk shows, with neither side reconnecting:
    // the exchange runs inside a tunnel the relay pipes and cannot read,
    // with a QR secret as well as a PIN.
    let links_before = net.relay().unwrap().links("ada").await;
    let started = start_pairing(&net, "desk", qr_mode()).await.unwrap();
    let Some(wire::start_pairing_response::Secret::QrSecret(secret)) = started.secret else {
        panic!("a QR window hands out a secret");
    };
    let pending = begin_pair(
        &net,
        "tablet",
        Some(desk),
        begin_pair_request::Secret::QrSecret(secret),
        Vec::new(),
    )
    .await
    .expect("pairing through the relay");
    assert_eq!(pending.via, wire::PeerVia::Relay as i32);
    confirm_pair(&net, "tablet", pending.token).await.unwrap();
    until("the tablet's calls to go through again", || async {
        peer_inventory_hosts(&edge(&net, "tablet"), desk)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
    .await
    .unwrap();
    assert_eq!(net.relay().unwrap().links("ada").await, links_before);
    println!("the tablet paired again through the relay with a QR secret, on the same links");

    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn profiles_and_accounts_share_no_keys_windows_presence_or_administration() {
    let net = Net::start(
        Topology::new()
            .relay(&["ada", "bob"])
            .host_decl(testnet::HostDecl {
                account: Some("ada".to_owned()),
                ..lan_host("desk", "home")
            })
            .host_decl(testnet::HostDecl {
                account: Some("ada".to_owned()),
                ..lan_host("laptop", "home")
            })
            // Down the street: on no local network with the others, so the
            // relay is the only way it could find them.
            .host_decl(testnet::HostDecl {
                account: Some("bob".to_owned()),
                ..lan_host("shop", "street")
            }),
    )
    .await
    .unwrap();
    let (desk, laptop, shop) = (
        host_id(&net, "desk"),
        host_id(&net, "laptop"),
        host_id(&net, "shop"),
    );
    let relay = net.relay().unwrap();

    // Two accounts on one relay: each sees its own hosts, and the relay
    // offers no route, and no pairing route, from one to the other.
    let mut ada = relay.links("ada").await;
    ada.sort();
    let mut expected = vec![(desk, 1), (laptop, 1)];
    expected.sort();
    assert_eq!(ada, expected);
    assert_eq!(relay.links("bob").await, vec![(shop, 1)]);
    // Hosts on one account see each other through the relay before any
    // trust, which is what lets them pair there.
    until_via(&net, "desk", "laptop", HostVia::Relay).await;
    never("the relay routes between accounts", async || {
        edge(&net, "shop").via(desk).await != HostVia::Offline
            || edge(&net, "desk").via(shop).await != HostVia::Offline
    })
    .await;
    let started = start_pairing(&net, "desk", pin_mode()).await.unwrap();
    let across = begin_pair(
        &net,
        "shop",
        Some(desk),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        Vec::new(),
    )
    .await
    .expect_err("no pairing route through another account's relay");
    assert_eq!(across.code(), tonic::Code::Unavailable);
    start_pairing(&net, "desk", qr_mode())
        .await
        .expect_err("the desk's PIN window is still open");
    door(&net, "desk")
        .await
        .cancel_pairing(operation(profile(&net, "desk")))
        .await
        .unwrap();
    let qr = start_pairing(&net, "desk", qr_mode()).await.unwrap();
    let Some(wire::start_pairing_response::Secret::QrSecret(secret)) = qr.secret else {
        panic!("a QR window hands out a secret");
    };
    let across_qr = begin_pair(
        &net,
        "shop",
        Some(desk),
        begin_pair_request::Secret::QrSecret(secret),
        Vec::new(),
    )
    .await
    .expect_err("no pairing route for a QR secret either");
    assert_eq!(across_qr.code(), across.code());
    assert_eq!(across_qr.message(), across.message());
    door(&net, "desk")
        .await
        .cancel_pairing(operation(profile(&net, "desk")))
        .await
        .unwrap();
    let started = start_pairing(&net, "desk", pin_mode()).await.unwrap();
    println!("ada's relay links {ada:?}; bob's shop sees none of them: {across:?}");

    // Pairing at an address the desk hands out is authority of its own,
    // whatever account either host is on.
    let pending = begin_pair(
        &net,
        "shop",
        Some(desk),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        started.addrs.clone(),
    )
    .await
    .expect("pairing across accounts at the desk's address");
    confirm_pair(&net, "shop", pending.token).await.unwrap();
    until_via(&net, "shop", "desk", HostVia::Direct).await;
    peer_inventory_hosts(&edge(&net, "shop"), desk)
        .await
        .expect("the shop calls the desk it paired with");

    // A second profile on the desk's installation is a host of its own:
    // its own id and key, its own pairing window and attempt budget.
    let work = door(&net, "desk")
        .await
        .create_profile(wire::CreateProfileRequest {
            label: Some("work".to_owned()),
            ..wire::CreateProfileRequest::default()
        })
        .await
        .unwrap()
        .into_inner();
    let work_profile: node::ProfileId = work.id.parse().unwrap();
    let work_edge = net.profile_edge("desk", work_profile).unwrap();
    assert_ne!(work_edge.host_id(), desk);
    assert_ne!(work_edge.public_key(), edge(&net, "desk").public_key());
    let work_start = door(&net, "desk")
        .await
        .start_pairing(wire::ProfileStartPairingRequest {
            profile_id: work.id.clone(),
            pairing: Some(pin_mode()),
            ..wire::ProfileStartPairingRequest::default()
        })
        .await
        .expect("the work profile opens its own window beside the desk's")
        .into_inner();
    let desk_start = start_pairing(&net, "desk", pin_mode()).await.unwrap();
    let wrong_for_work = begin_pair(
        &net,
        "laptop",
        Some(work_edge.host_id()),
        begin_pair_request::Secret::Pin(pin_of(&desk_start)),
        work_start.addrs.clone(),
    )
    .await
    .expect_err("the desk's PIN at the work profile");
    assert!(is_invalid_secret(&wrong_for_work));
    for _ in 0..4 {
        begin_pair(
            &net,
            "laptop",
            Some(work_edge.host_id()),
            begin_pair_request::Secret::Pin(wrong_pin(&pin_of(&work_start))),
            work_start.addrs.clone(),
        )
        .await
        .expect_err("a wrong guess at the work profile");
    }
    assert!(
        !work_edge.pairing_active(),
        "five guesses closed work's window"
    );
    assert!(
        edge(&net, "desk").pairing_active(),
        "the desk's window keeps its own budget"
    );
    let pending = begin_pair(
        &net,
        "laptop",
        Some(desk),
        begin_pair_request::Secret::Pin(pin_of(&desk_start)),
        desk_start.addrs.clone(),
    )
    .await
    .unwrap();
    confirm_pair(&net, "laptop", pending.token).await.unwrap();
    until_via(&net, "laptop", "desk", HostVia::Direct).await;
    assert!(
        work_edge.trusted().is_empty(),
        "the desk's pairing commits nothing to work"
    );

    // The laptop's key is pinned by the desk's profile only: dialling the
    // work profile's listener with it gets it nowhere.
    edge(&net, "laptop").trust(&work_edge).await.unwrap();
    edge(&net, "laptop").dial(work_edge.host_id(), work_edge.lan_addr().unwrap());
    until(
        "the laptop's dial into the work profile to fail",
        || async {
            edge(&net, "laptop")
                .last_dial_error(work_edge.host_id())
                .await
                .ok_or("no dial error stored")
        },
    )
    .await
    .unwrap();
    assert_eq!(
        edge(&net, "laptop").via(work_edge.host_id()).await,
        HostVia::Offline,
        "a key one profile pinned never authenticates into another"
    );
    assert_eq!(work_edge.via(laptop).await, HostVia::Offline);
    println!(
        "work profile {}: own key, own window, the desk's peers refused",
        work.id
    );

    // Administration is the installation's: a trusted peer's tunnel and a
    // profile's client socket serve neither profiles nor pairing.
    let tunnel = edge(&net, "laptop").channel(desk).await.unwrap();
    let over_tunnel = wire::profile_service_client::ProfileServiceClient::new(tunnel)
        .list_profiles(wire::ListProfilesRequest {})
        .await
        .expect_err("no profile administration over a peer tunnel");
    assert_eq!(over_tunnel.code(), tonic::Code::Unimplemented);
    let socket = node::profile_dir(
        &net.host("desk").unwrap().data_dir,
        net.host("desk").unwrap().profile,
    )
    .join(node::PROFILE_SOCKET);
    let on_socket = wire::profile_service_client::ProfileServiceClient::new(
        testnet::local_channel(&socket).await.unwrap(),
    )
    .start_pairing(wire::ProfileStartPairingRequest {
        profile_id: profile(&net, "desk"),
        pairing: Some(pin_mode()),
        ..wire::ProfileStartPairingRequest::default()
    })
    .await
    .expect_err("no pairing administration on a profile's client socket");
    assert_eq!(on_socket.code(), tonic::Code::Unimplemented);

    // Each profile's socket is its own client surface: two clients, one on
    // each, each list their own profile's host as this one.
    let work_socket = node::profile_dir(&net.host("desk").unwrap().data_dir, work_profile)
        .join(node::PROFILE_SOCKET);
    for (socket, own) in [(&socket, desk), (&work_socket, work_edge.host_id())] {
        let mut client = wire::client_service_client::ClientServiceClient::new(
            testnet::local_channel(socket).await.unwrap(),
        );
        let mut events = client
            .subscribe_inventory(wire::Empty {})
            .await
            .unwrap()
            .into_inner();
        let first = loop {
            match events.message().await.unwrap().unwrap().of {
                Some(wire::inventory_event::Of::Host(host)) if host.host_id == own.as_bytes() => {
                    break host;
                }
                Some(wire::inventory_event::Of::CaughtUp(_)) => {
                    panic!("a profile's socket lists its own host");
                }
                _ => {}
            }
        };
        assert_eq!(first.trust, wire::Trust::Trusted as i32);
    }

    net.shutdown().await.unwrap();
}

/// A host on the relay alone: no listener, no discovery.
fn relay_host(name: &str, account: &str) -> testnet::HostDecl {
    testnet::HostDecl {
        name: name.to_owned(),
        account: Some(account.to_owned()),
        ..testnet::HostDecl::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_free_link_lists_its_hosts_and_opens_nothing_until_a_refresh_brings_pro() {
    // The desk listens on the local network; the phone is on the relay
    // alone; the tablet, in the desk's room, listens for nothing, as a
    // phone app does.
    let mut topology = Topology::new()
        .relay(&["ada"])
        .host_decl(testnet::HostDecl {
            account: Some("ada".to_owned()),
            ..lan_host("desk", "home")
        })
        .host_decl(relay_host("phone", "ada"))
        .host_decl(testnet::HostDecl {
            account: Some("ada".to_owned()),
            ..lan_host("tablet", "home")
        });
    topology.relay.as_mut().unwrap().accounts[0].tier = testnet::TierDecl::Free;
    let mut net = Net::start_with(
        topology,
        testnet::NetOptions {
            edge: Some(std::sync::Arc::new(|host, edge| {
                if host == "tablet" {
                    edge.lan = None;
                    edge.dial = true;
                }
            })),
            ..testnet::NetOptions::default()
        },
    )
    .await
    .unwrap();
    net.trust("desk", "phone").await.unwrap();
    net.trust("phone", "desk").await.unwrap();
    let (desk, phone) = (host_id(&net, "desk"), host_id(&net, "phone"));
    assert_eq!(tier(&net, "desk"), Some(node::Tier::Free));

    // On a free account the relay lists the account's hosts and opens no
    // tunnel between them, for calls or for pairing; the refused pairing
    // leaves the responder's window open, as the secret never reached it.
    until_via(&net, "desk", "phone", HostVia::Relay).await;
    let call = peer_inventory_hosts(&edge(&net, "phone"), desk)
        .await
        .expect_err("no tunnel on a free account");
    let started = start_pairing(&net, "desk", pin_mode()).await.unwrap();
    let pairing = begin_pair(
        &net,
        "phone",
        Some(desk),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        Vec::new(),
    )
    .await
    .expect_err("no pairing tunnel on a free account");
    assert!(edge(&net, "desk").pairing_active());
    println!(
        "free: listed via the relay; call {:?}; pairing {:?}",
        call.code(),
        pairing.code()
    );

    // The tablet sees the desk through the relay and on the local network
    // at once. Pairing takes the local network, the only way it can
    // succeed on a free account, and a client already watching is told
    // the desk is now directly linked.
    let mut fleet = net.observe_inventory("tablet").await.unwrap();
    let pending = begin_pair(
        &net,
        "tablet",
        Some(desk),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        Vec::new(),
    )
    .await
    .expect("pairing on the local network on a free account");
    assert_eq!(pending.via, wire::PeerVia::Direct as i32);
    confirm_pair(&net, "tablet", pending.token).await.unwrap();
    row_until(
        &mut fleet,
        desk,
        "the tablet's client told of the direct link",
        |row| row.via == wire::HostVia::Direct as i32 && row.trust == wire::Trust::Trusted as i32,
    )
    .await;
    peer_inventory_hosts(&edge(&net, "tablet"), desk)
        .await
        .expect("a direct link needs no tier");
    println!("the tablet paired directly on the free account; its client saw the desk go direct");

    // The account buys Pro. The desk, where the purchase happened, asks at
    // once and reauthenticates on its live link; the phone has not asked
    // yet, and a Pro host still opens nothing toward a free one.
    net.relay().unwrap().set_tier("ada", node::Tier::Pro);
    let connects = net.relay().unwrap().connects().len();
    assert_eq!(
        edge(&net, "desk").refresh_entitlement().await.unwrap(),
        node::Tier::Pro
    );
    until("the desk's link to carry Pro", || async {
        let tier = tier(&net, "desk");
        (tier == Some(node::Tier::Pro))
            .then_some(())
            .ok_or_else(|| format!("tier {tier:?}"))
    })
    .await
    .unwrap();
    assert_eq!(tier(&net, "phone"), Some(node::Tier::Free));
    peer_inventory_hosts(&edge(&net, "desk"), phone)
        .await
        .expect_err("a Pro link opens nothing toward a free one");

    // The others ask on their free refresh interval, on the policy clock,
    // and from then on the phone and the desk call each other, on the same
    // links.
    net.advance(node::FREE_TIER_REFRESH_INTERVAL).unwrap();
    until("the others' links to carry Pro", || async {
        let tiers = (tier(&net, "phone"), tier(&net, "tablet"));
        (tiers == (Some(node::Tier::Pro), Some(node::Tier::Pro)))
            .then_some(())
            .ok_or_else(|| format!("phone, tablet tiers {tiers:?}"))
    })
    .await
    .unwrap();
    assert_eq!(
        net.relay().unwrap().connects().len(),
        connects + 3,
        "each host asked once"
    );
    peer_inventory_hosts(&edge(&net, "phone"), desk)
        .await
        .expect("the phone calls the desk on Pro");
    peer_inventory_hosts(&edge(&net, "desk"), phone)
        .await
        .expect("the desk calls the phone on Pro");
    let mut links = net.relay().unwrap().links("ada").await;
    links.sort();
    let mut expected = vec![(desk, 1), (phone, 1), (host_id(&net, "tablet"), 1)];
    expected.sort();
    assert_eq!(links, expected, "Pro arrived on the links already up");
    println!("Pro: the desk at once, the phone after its interval, on the same links");

    // A credential refresh under a live link keeps what the link carries:
    // the phone's inventory of the desk, held open, sees a new agent after
    // the desk's link has reauthenticated on a fresh credential.
    let mut held = edge(&net, "phone")
        .peer(desk)
        .await
        .unwrap()
        .subscribe_inventory(wire::Empty {})
        .await
        .unwrap()
        .into_inner();
    let presented = net.relay().unwrap().presented().len();
    net.advance(testnet::CREDENTIAL_TTL - Duration::from_secs(4 * 60))
        .unwrap();
    until("every link to present a fresh credential", || async {
        let now = net.relay().unwrap().presented().len();
        (now >= presented + 3)
            .then_some(())
            .ok_or_else(|| format!("{now} presented, {presented} before"))
    })
    .await
    .unwrap();
    // A refresh re-authenticates the link it rides and then arms the next
    // one, five minutes before the fresh credential expires: three timers
    // at that deadline say every refresh is through.
    let next_refresh =
        net.now_ms() + (testnet::CREDENTIAL_TTL - Duration::from_secs(5 * 60)).as_millis() as i64;
    until("every link to arm its next refresh", || async {
        let sleeping = net.clock().unwrap().sleeping();
        let armed = sleeping.iter().filter(|at| **at == next_refresh).count();
        (armed >= 3)
            .then_some(())
            .ok_or_else(|| format!("{armed} armed at {next_refresh}; sleeping {sleeping:?}"))
    })
    .await
    .unwrap();
    assert_eq!(
        net.relay().unwrap().links("ada").await.len(),
        3,
        "the three links stayed three"
    );
    let spawned = net
        .spawn(testnet::AgentDecl::new("scout", "desk").prompt("Look around."))
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + testnet::PATIENCE;
    loop {
        let event = tokio::time::timeout_at(deadline, held.message())
            .await
            .expect("the held inventory to show the new agent")
            .expect("the held inventory is still open")
            .expect("the held inventory is still open");
        if let Some(wire::inventory_event::Of::Agent(agent)) = event.of
            && agent.agent_id == spawned.agent_id
        {
            break;
        }
    }
    println!(
        "credentials refreshed on the live links ({} presented); the held inventory kept streaming",
        net.relay().unwrap().presented().len()
    );

    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_peer_requests_and_handshake_floods_are_refused_and_the_link_carries_on() {
    let net = Net::start(
        Topology::new()
            .host_decl(lan_host("desk", "home"))
            .host_decl(lan_host("laptop", "home"))
            .host_decl(lan_host("stranger", "elsewhere"))
            .agent(testnet::AgentDecl::new("worker", "desk").prompt("Hold on.")),
    )
    .await
    .unwrap();
    let desk = host_id(&net, "desk");
    pair(&net, "laptop", "desk").await;
    until_via(&net, "laptop", "desk", HostVia::Direct).await;
    let worker = net.agent("worker").unwrap().id.as_bytes().to_vec();
    let mut peer = edge(&net, "laptop").peer(desk).await.unwrap();

    // Each call names something that cannot be, or leaves out what it
    // needs; each is answered as the caller's mistake, never as a fault of
    // the desk's, and none of them costs the link.
    let short = vec![1, 2, 3];
    let unknown = uuid::Uuid::new_v4().as_bytes().to_vec();
    let mut answers = Vec::new();
    answers.push((
        "Subscribe with a three-byte agent id",
        peer.subscribe(wire::PeerSubscribeRequest {
            agent_id: short.clone(),
            from: Some(wire::peer_subscribe_request::From::Tail(5)),
        })
        .await
        .map(|_| ()),
    ));
    answers.push((
        "Subscribe to an agent the desk never had",
        peer.subscribe(wire::PeerSubscribeRequest {
            agent_id: unknown.clone(),
            from: Some(wire::peer_subscribe_request::From::Tail(5)),
        })
        .await
        .map(|_| ()),
    ));
    answers.push((
        "Fetch with a three-byte agent id",
        peer.fetch(wire::FetchRequest {
            agent_id: short.clone(),
            before_order: None,
            limit: 10,
        })
        .await
        .map(|_| ()),
    ));
    answers.push((
        "Get with an empty key",
        peer.get(wire::GetRequest {
            agent_id: worker.clone(),
            key: String::new(),
        })
        .await
        .map(|_| ()),
    ));
    answers.push((
        "SendInput with no input",
        peer.send_input(wire::SendInputRequest {
            agent_id: worker.clone(),
            input: None,
        })
        .await
        .map(|_| ()),
    ));
    answers.push((
        "CreateAgent with no configuration",
        peer.create_agent(wire::CreateAgentRequest::default())
            .await
            .map(|_| ()),
    ));
    answers.push((
        "SendMessage with no sender or recipient",
        peer.send_message(wire::Envelope::default())
            .await
            .map(|_| ()),
    ));
    answers.push((
        "GetBlob with a malformed hash",
        peer.get_blob(wire::GetBlobRequest {
            agent_id: worker.clone(),
            hash: vec![0; 3],
        })
        .await
        .map(|_| ()),
    ));
    answers.push((
        "StopAgent with a three-byte agent id",
        peer.stop_agent(wire::StopAgentRequest {
            agent_id: short.clone(),
            mode: wire::StopMode::Kill as i32,
        })
        .await
        .map(|_| ()),
    ));
    for (what, answer) in &answers {
        let status = answer.as_ref().expect_err(&format!("{what} is refused"));
        assert!(
            matches!(
                status.code(),
                tonic::Code::InvalidArgument
                    | tonic::Code::NotFound
                    | tonic::Code::PermissionDenied
                    | tonic::Code::FailedPrecondition
            ),
            "{what}: {status:?}"
        );
        println!("{what}: {:?}", status.code());
    }
    assert!(
        peer_inventory_hosts(&edge(&net, "laptop"), desk)
            .await
            .unwrap()
            .contains(&desk.as_bytes().to_vec()),
        "the link carries on"
    );

    // A stranger opening handshake after handshake at the desk's listener
    // is cut off after ten a minute, before the desk spends anything on
    // them, while the laptop's link stays up.
    let desk_addr = edge(&net, "desk").lan_addr().unwrap();
    let mut answered = 0;
    let mut ignored = 0;
    for _ in 0..12 {
        match tokio::time::timeout(
            Duration::from_secs(1),
            edge(&net, "stranger").unpinned_channel(desk_addr),
        )
        .await
        {
            Ok(Ok(_)) | Ok(Err(_)) if ignored == 0 => answered += 1,
            _ => ignored += 1,
        }
    }
    assert!(ignored >= 2, "{answered} answered, {ignored} ignored");
    assert_eq!(edge(&net, "laptop").via(desk).await, HostVia::Direct);
    peer_inventory_hosts(&edge(&net, "laptop"), desk)
        .await
        .expect("the laptop's link is untouched by the flood");
    println!(
        "a flood of 12 handshakes: {answered} answered, {ignored} ignored; the link carries on"
    );

    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subscriber_that_stops_reading_is_closed_while_one_beside_it_keeps_its_stream() {
    use provider_fakes::script::Step;

    // A small fan-out ring on the desk, and an agent that, once let go,
    // says six hundred things of eight kilobytes each, a couple of
    // milliseconds apart: easy going for a reader that reads, and far more
    // than the ring and every buffer between the desk and one that stopped.
    let net = Net::start_with(
        Topology::new()
            .host_decl(lan_host("desk", "home"))
            .host_decl(lan_host("laptop", "home"))
            .agent(
                testnet::AgentDecl::new("chatty", "desk")
                    .prompt("Report everything.")
                    .steps(vec![
                        Step::WaitFor { path: "go".into() },
                        Step::Repeat {
                            times: 600,
                            steps: vec![
                                Step::Text {
                                    chunks: vec!["x".repeat(8 * 1024)],
                                },
                                Step::Pause { ms: 2 },
                            ],
                            played: 0,
                        },
                        Step::Text {
                            chunks: vec!["done".to_owned()],
                        },
                        Step::TurnEnd,
                    ]),
            ),
        testnet::NetOptions {
            launch: Some(std::sync::Arc::new(|host, launch| {
                if host == "desk" {
                    launch.fanout_capacity = 64;
                }
            })),
            ..testnet::NetOptions::default()
        },
    )
    .await
    .unwrap();
    let desk = host_id(&net, "desk");
    pair(&net, "laptop", "desk").await;
    until_via(&net, "laptop", "desk", HostVia::Direct).await;
    let chatty = net.agent("chatty").unwrap().id;

    // Two subscriptions from the laptop, each on a stream of its own over
    // the direct link: one reads everything, the other reads nothing.
    let mut stalled = open_session(&edge(&net, "laptop"), desk, chatty)
        .await
        .unwrap();
    let mut reading = open_session(&edge(&net, "laptop"), desk, chatty)
        .await
        .unwrap();
    let reader = tokio::spawn(async move {
        let mut items = 0_usize;
        loop {
            let event = reading
                .message()
                .await
                .map_err(|status| format!("the reader's stream failed: {status:?}"))?
                .ok_or("the reader's stream ended")?;
            match event.of {
                Some(wire::session_event::Of::Lagged(_)) => {
                    return Err("the reader was told it lagged".to_owned());
                }
                Some(wire::session_event::Of::Item(item)) => {
                    items += 1;
                    if item.text == "done" {
                        return Ok(items);
                    }
                }
                _ => {}
            }
        }
    });
    net.open_gate("go").unwrap();
    let items = tokio::time::timeout(Duration::from_secs(120), reader)
        .await
        .expect("the reader keeps up to the end")
        .unwrap()
        .expect("the reader's stream carries everything");

    // The subscriber that stopped reading was told it lagged, and its
    // stream ended there: the desk never waited on it.
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + testnet::PATIENCE;
    let end = loop {
        match tokio::time::timeout_at(deadline, stalled.message()).await {
            Err(_) => panic!("the stalled stream never ended"),
            Ok(Ok(Some(event))) => seen.push(event),
            Ok(Ok(None)) => break "ended".to_owned(),
            Ok(Err(status)) => break format!("{:?}", status.code()),
        }
    };
    let lagged = seen
        .iter()
        .position(|event| matches!(event.of, Some(wire::session_event::Of::Lagged(_))))
        .expect("the stalled subscriber is told it lagged");
    assert_eq!(lagged, seen.len() - 1, "nothing follows Lagged");
    println!(
        "the reader saw {items} items to the end; the stalled subscriber got {} events, \
         then Lagged, then its stream {end}",
        seen.len() - 1
    );

    net.shutdown().await.unwrap();
}

/// Signs profile `profile` on `host` in with `refresh`, as a login hands
/// the daemon its staged token.
async fn bind(
    net: &Net,
    host: &str,
    profile: &str,
    refresh: String,
    adopt: bool,
) -> Result<wire::ProfileInfo, tonic::Status> {
    door(net, host)
        .await
        .bind_profile(wire::BindProfileRequest {
            profile_id: Some(profile.to_owned()),
            cloud_url: net.relay().unwrap().url().to_owned(),
            staged_refresh_token: refresh,
            adopt_non_pristine: adopt,
            ..wire::BindProfileRequest::default()
        })
        .await
        .map(tonic::Response::into_inner)
}

async fn profile_info(net: &Net, host: &str, profile: &str) -> wire::ProfileInfo {
    door(net, host)
        .await
        .list_profiles(wire::ListProfilesRequest {})
        .await
        .unwrap()
        .into_inner()
        .profiles
        .into_iter()
        .find(|info| info.id == profile)
        .expect("the profile is listed")
}

async fn create(net: &Net, host: &str, label: &str) -> (String, std::sync::Arc<node::Edge>) {
    let info = door(net, host)
        .await
        .create_profile(wire::CreateProfileRequest {
            label: Some(label.to_owned()),
            ..wire::CreateProfileRequest::default()
        })
        .await
        .unwrap()
        .into_inner();
    let edge = net.profile_edge(host, info.id.parse().unwrap()).unwrap();
    (info.id, edge)
}

#[tokio::test(flavor = "multi_thread")]
async fn accounts_sign_in_out_pause_and_go_one_profile_at_a_time() {
    let mut net = Net::start(
        Topology::new()
            .relay(&["ada", "bob", "cara"])
            .host_decl(lan_host("desk", "home"))
            .host_decl(lan_host("laptop", "home"))
            .agent(testnet::AgentDecl::new("worker", "desk").prompt("Keep going.")),
    )
    .await
    .unwrap();
    let (desk, laptop) = (host_id(&net, "desk"), host_id(&net, "laptop"));
    let main = profile(&net, "desk");
    pair(&net, "laptop", "desk").await;
    until_via(&net, "laptop", "desk", HostVia::Direct).await;
    let (work, work_edge) = create(&net, "desk", "work").await;
    let relay_login = testnet::Relay::login;

    // Signing in asks the person to confirm adopting what a profile
    // already holds, and then the desk's main profile is Ada's.
    let refused = bind(&net, "desk", &main, relay_login("ada"), false)
        .await
        .expect_err("a profile with agents and a paired host");
    assert_eq!(refused.code(), tonic::Code::FailedPrecondition);
    net.sign_in("desk", "ada").await.unwrap();
    bind(&net, "desk", &work, relay_login("bob"), false)
        .await
        .expect("an empty profile is adopted without asking");
    until("both relay links", || async {
        let links = net.relay().unwrap().links("bob").await;
        (links == vec![(work_edge.host_id(), 1)])
            .then_some(())
            .ok_or_else(|| format!("bob's links: {links:?}"))
    })
    .await
    .unwrap();
    assert_eq!(net.relay().unwrap().links("ada").await, vec![(desk, 1)]);

    // A login that belongs elsewhere is refused and moves nothing: another
    // account for a bound profile, an account already on another profile,
    // and a login the cloud does not honour.
    for (what, target, refresh, code) in [
        (
            "Cara's login on Ada's profile",
            &main,
            relay_login("cara"),
            tonic::Code::FailedPrecondition,
        ),
        (
            "Ada's login on the work profile",
            &work,
            relay_login("ada"),
            tonic::Code::AlreadyExists,
        ),
        (
            "a login the cloud refuses",
            &work,
            "refresh-nobody".to_owned(),
            tonic::Code::Unauthenticated,
        ),
    ] {
        let status = bind(&net, "desk", target, refresh, true)
            .await
            .expect_err(what);
        assert_eq!(status.code(), code, "{what}: {status:?}");
        println!("{what}: {:?}", status.code());
    }
    assert_eq!(net.relay().unwrap().links("ada").await, vec![(desk, 1)]);
    assert_eq!(
        net.relay().unwrap().links("bob").await,
        vec![(work_edge.host_id(), 1)]
    );
    assert_eq!(
        profile_info(&net, "desk", &main).await.email,
        "ada@example.com"
    );
    assert_eq!(
        profile_info(&net, "desk", &work).await.email,
        "bob@example.com"
    );

    // Bob's account is revoked. The work profile has to sign in again; the
    // main profile's link to the relay does not notice.
    net.relay().unwrap().revoke("bob");
    door(&net, "desk")
        .await
        .pause_profile(operation(work.clone()))
        .await
        .unwrap();
    door(&net, "desk")
        .await
        .resume_profile(operation(work.clone()))
        .await
        .unwrap();
    until("the work profile to need a sign-in", || async {
        let observed = profile_info(&net, "desk", &work).await.observed;
        (observed == wire::Observed::AuthenticationRequired as i32)
            .then_some(())
            .ok_or_else(|| format!("observed {observed}"))
    })
    .await
    .unwrap();
    assert_eq!(
        profile_info(&net, "desk", &main).await.observed,
        wire::Observed::Connected as i32
    );
    assert_eq!(net.relay().unwrap().links("ada").await, vec![(desk, 1)]);

    // Pausing closes the cloud link only: the laptop's direct link stays.
    // Resuming, twice over, brings back one link.
    door(&net, "desk")
        .await
        .pause_profile(operation(main.clone()))
        .await
        .unwrap();
    until("Ada's relay link to close", || async {
        let links = net.relay().unwrap().links("ada").await;
        links
            .is_empty()
            .then_some(())
            .ok_or_else(|| format!("ada's links: {links:?}"))
    })
    .await
    .unwrap();
    assert_eq!(edge(&net, "desk").via(laptop).await, HostVia::Direct);
    for _ in 0..2 {
        door(&net, "desk")
            .await
            .resume_profile(operation(main.clone()))
            .await
            .unwrap();
    }
    until("Ada's relay link to come back", || async {
        let links = net.relay().unwrap().links("ada").await;
        (links == vec![(desk, 1)])
            .then_some(())
            .ok_or_else(|| format!("ada's links: {links:?}"))
    })
    .await
    .unwrap();
    // Two resumes, one link; a window, since no outcome marks the second
    // link that must not open.
    holds_for(
        "Ada's one relay link",
        Duration::from_millis(500),
        || async { net.relay().unwrap().links("ada").await == vec![(desk, 1)] },
    )
    .await
    .unwrap();

    // Signing out forgets the cloud and keeps everything local: the agent,
    // the identity and the paired laptop. Signing in again finds the same
    // host.
    let key = edge(&net, "desk").public_key().to_vec();
    door(&net, "desk")
        .await
        .logout_profile(operation(main.clone()))
        .await
        .unwrap();
    until("Ada's relay link to close", || async {
        let links = net.relay().unwrap().links("ada").await;
        links
            .is_empty()
            .then_some(())
            .ok_or_else(|| format!("ada's links: {links:?}"))
    })
    .await
    .unwrap();
    assert_eq!(
        profile_info(&net, "desk", &main).await.intent,
        wire::Intent::LoggedOut as i32
    );
    assert_eq!(edge(&net, "desk").via(laptop).await, HostVia::Direct);
    assert!(edge(&net, "desk").is_trusted(laptop));
    assert_eq!(edge(&net, "desk").public_key(), key.as_slice());
    let listed = net
        .runtime("desk")
        .unwrap()
        .store()
        .await
        .agents()
        .unwrap()
        .len();
    assert_eq!(listed, 1, "the worker is still the desk's");
    // The signed-out profile keeps its place for the account, across a
    // restart: a login naming no profile lands on it again.
    net.stop_daemon("desk").await.unwrap();
    net.restart_daemon("desk").await.unwrap();
    // A watcher from here on sees everything that follows, in order.
    let mut watch = door(&net, "desk")
        .await
        .watch_profiles(wire::WatchProfilesRequest {})
        .await
        .unwrap()
        .into_inner();
    let again = door(&net, "desk")
        .await
        .bind_profile(wire::BindProfileRequest {
            profile_id: None,
            cloud_url: net.relay().unwrap().url().to_owned(),
            staged_refresh_token: relay_login("ada"),
            ..wire::BindProfileRequest::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(again.id, main);
    until("Ada's relay link after the restart", || async {
        let links = net.relay().unwrap().links("ada").await;
        (links == vec![(desk, 1)])
            .then_some(())
            .ok_or_else(|| format!("ada's links: {links:?}"))
    })
    .await
    .unwrap();
    assert_eq!(host_id(&net, "desk"), desk);
    until("the laptop linked again", || async {
        let via = edge(&net, "desk").via(laptop).await;
        (via == HostVia::Direct)
            .then_some(())
            .ok_or_else(|| format!("via {via:?}"))
    })
    .await
    .unwrap();
    println!("signed out and in again: same host, same key, laptop still paired, worker kept");

    // Two logins for one account at once land on one profile, with one
    // link.
    net.relay().unwrap().add_account("dan", node::Tier::Pro);
    let login = |door: wire::profile_service_client::ProfileServiceClient<_>| {
        let mut door = door;
        let request = wire::BindProfileRequest {
            profile_id: None,
            cloud_url: net.relay().unwrap().url().to_owned(),
            staged_refresh_token: relay_login("dan"),
            ..wire::BindProfileRequest::default()
        };
        async move {
            door.bind_profile(request)
                .await
                .map(tonic::Response::into_inner)
        }
    };
    let (first, second) = tokio::join!(
        login(door(&net, "desk").await),
        login(door(&net, "desk").await)
    );
    let (first, second) = (first.unwrap(), second.unwrap());
    assert_eq!(first.id, second.id, "one profile for one account");
    until("Dan's one relay link", || async {
        let links = net.relay().unwrap().links("dan").await;
        (links.len() == 1)
            .then_some(())
            .ok_or_else(|| format!("dan's links: {links:?}"))
    })
    .await
    .unwrap();
    // Two sign-ins, one link; a window, since no outcome marks the second
    // link that must not open.
    holds_for(
        "Dan's one relay link to stay one",
        Duration::from_millis(300),
        || async { net.relay().unwrap().links("dan").await.len() == 1 },
    )
    .await
    .unwrap();

    // A profile deleted is gone for every caller: its relay link, its
    // listener, its place in the list.
    let (old, old_edge) = create(&net, "desk", "old").await;
    bind(&net, "desk", &old, relay_login("cara"), false)
        .await
        .unwrap();
    until("Cara's relay link", || async {
        let links = net.relay().unwrap().links("cara").await;
        (links == vec![(old_edge.host_id(), 1)])
            .then_some(())
            .ok_or_else(|| format!("cara's links: {links:?}"))
    })
    .await
    .unwrap();
    let old_addr = old_edge.lan_addr().unwrap();
    let revision = profile_info(&net, "desk", &old).await.revision;
    drop(old_edge);
    door(&net, "desk")
        .await
        .delete_profile(wire::DeleteProfileRequest {
            profile_id: old.clone(),
            confirm_revision: revision,
            ..wire::DeleteProfileRequest::default()
        })
        .await
        .unwrap();
    until("Cara's relay link to close", || async {
        let links = net.relay().unwrap().links("cara").await;
        links
            .is_empty()
            .then_some(())
            .ok_or_else(|| format!("cara's links: {links:?}"))
    })
    .await
    .unwrap();
    let reached = tokio::time::timeout(
        Duration::from_secs(1),
        edge(&net, "laptop").unpinned_channel(old_addr),
    )
    .await;
    assert!(
        !matches!(reached, Ok(Ok(_))),
        "the deleted profile's listener is closed"
    );
    assert!(net.profile_edge("desk", old.parse().unwrap()).is_err());
    assert_eq!(
        net.relay().unwrap().links("ada").await,
        vec![(desk, 1)],
        "the other profiles keep routing"
    );
    assert_eq!(edge(&net, "desk").via(laptop).await, HostVia::Direct);

    // The watcher saw all of it, in order: every change numbered, the
    // deletion last.
    let mut sequence = 0;
    let mut removed = None;
    let mut upserts = 0;
    let deadline = tokio::time::Instant::now() + testnet::PATIENCE;
    while removed.is_none() {
        let event = tokio::time::timeout_at(deadline, watch.message())
            .await
            .expect("the watcher to see the deletion")
            .unwrap()
            .unwrap();
        assert!(event.sequence >= sequence, "in order");
        sequence = event.sequence;
        match event.event {
            Some(wire::watch_profiles_response::Event::Upserted(_)) => upserts += 1,
            Some(wire::watch_profiles_response::Event::RemovedId(id)) => removed = Some(id),
            _ => {}
        }
    }
    assert_eq!(removed.as_deref(), Some(old.as_str()));
    println!("the watcher saw {upserts} changes in order, then the deletion");

    net.shutdown().await.unwrap();
}

/// Waits until `viewer`'s own inventory describes `host` as `check` wants,
/// and returns that row.
async fn row_until(
    fleet: &mut testnet::InventoryObserver,
    host: uuid::Uuid,
    what: &str,
    check: impl Fn(&wire::HostEntry) -> bool,
) -> wire::HostEntry {
    let found = |events: &[wire::InventoryEvent]| {
        testnet::observe::inventory_hosts(events)
            .into_iter()
            .find(|row| row.host_id == host.as_bytes())
    };
    let events = fleet
        .observe_until(
            |events| found(events).is_some_and(|row| check(&row)),
            testnet::PATIENCE,
        )
        .await
        .unwrap_or_else(|stuck| panic!("{what}: {stuck}"));
    found(events).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_is_seen_going_and_coming_with_its_identity_and_its_sign_in() {
    let mut net = Net::start(
        Topology::new()
            .relay(&["ada"])
            .host_decl(relay_host("desk", "ada"))
            .host("laptop")
            .host_decl(relay_host("tablet", "ada"))
            .link("desk", "laptop"),
    )
    .await
    .unwrap();
    let (desk, laptop, tablet) = (
        host_id(&net, "desk"),
        host_id(&net, "laptop"),
        host_id(&net, "tablet"),
    );
    let key = edge(&net, "desk").public_key().to_vec();
    let mut fleet = net.observe_inventory("laptop").await.unwrap();

    // Signed in or not, as each host's handshake says; a host that signs
    // in or out says so in its next handshake, with neither end
    // restarting.
    let row = row_until(&mut fleet, desk, "the desk, online and signed in", |row| {
        row.presence == wire::Presence::Online as i32 && row.signed_in == Some(true)
    })
    .await;
    assert_eq!(row.via, wire::HostVia::Direct as i32);
    let mut desk_fleet = net.observe_inventory("desk").await.unwrap();
    row_until(
        &mut desk_fleet,
        laptop,
        "the laptop, never signed in",
        |row| row.signed_in == Some(false),
    )
    .await;
    door(&net, "desk")
        .await
        .logout_profile(operation(profile(&net, "desk")))
        .await
        .unwrap();
    net.sever_link("desk", "laptop").unwrap();
    net.restore_link("desk", "laptop").await.unwrap();
    row_until(&mut fleet, desk, "the desk signed out", |row| {
        row.signed_in == Some(false)
    })
    .await;
    net.sign_in("desk", "ada").await.unwrap();
    net.sever_link("desk", "laptop").unwrap();
    net.restore_link("desk", "laptop").await.unwrap();
    row_until(&mut fleet, desk, "the desk signed in again", |row| {
        row.signed_in == Some(true)
    })
    .await;
    println!("the laptop saw the desk sign out and in, each at its next handshake");

    // A host on the account that nobody paired is seen through the relay,
    // and no call is made to it.
    until("the desk to see the tablet through the relay", || async {
        let via = edge(&net, "desk").via(tablet).await;
        (via == HostVia::Relay)
            .then_some(())
            .ok_or_else(|| format!("via {via:?}"))
    })
    .await
    .unwrap();
    peer_inventory_hosts(&edge(&net, "desk"), tablet)
        .await
        .expect_err("no call to a host nobody paired");

    // That sight is what lets the two pair by a PIN through the relay; and
    // once the tablet goes away, the desk sees it go.
    let started = start_pairing(&net, "desk", pin_mode()).await.unwrap();
    let pending = begin_pair(
        &net,
        "tablet",
        Some(desk),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        Vec::new(),
    )
    .await
    .expect("a PIN pairing through the relay");
    assert_eq!(pending.via, wire::PeerVia::Relay as i32);
    confirm_pair(&net, "tablet", pending.token).await.unwrap();
    until("the tablet's calls to go through", || async {
        peer_inventory_hosts(&edge(&net, "tablet"), desk)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
    .await
    .unwrap();
    net.stop_daemon("tablet").await.unwrap();
    until("the desk to see the tablet go", || async {
        let via = edge(&net, "desk").via(tablet).await;
        (via == HostVia::Offline)
            .then_some(())
            .ok_or_else(|| format!("via {via:?}"))
    })
    .await
    .unwrap();

    // The desk goes down: still listed, as trusted and offline, keeping
    // the last word on its sign-in, and calls to it fail. It comes back
    // with the identity it had, which the laptop's pinned key accepts.
    net.stop_daemon("desk").await.unwrap();
    let row = row_until(&mut fleet, desk, "the desk offline", |row| {
        row.presence != wire::Presence::Online as i32
    })
    .await;
    assert_eq!(row.trust, wire::Trust::Trusted as i32);
    assert_eq!(row.signed_in, Some(true));
    peer_inventory_hosts(&edge(&net, "laptop"), desk)
        .await
        .expect_err("no calls to a host that is down");
    net.restart_daemon("desk").await.unwrap();
    assert_eq!(host_id(&net, "desk"), desk);
    assert_eq!(edge(&net, "desk").public_key(), key.as_slice());
    // What the desk says of itself is what the laptop pinned, and asking
    // opens no pairing window.
    let identity = door(&net, "desk")
        .await
        .get_device_identity(wire::ProfileRequest {
            profile_id: profile(&net, "desk"),
        })
        .await
        .unwrap()
        .into_inner();
    let pinned = edge(&net, "laptop")
        .trusted()
        .into_iter()
        .find(|(host, ..)| *host == desk)
        .unwrap();
    assert_eq!(identity.pubkey, pinned.2);
    assert!(!edge(&net, "desk").pairing_active());
    row_until(&mut fleet, desk, "the desk back online", |row| {
        row.presence == wire::Presence::Online as i32
    })
    .await;
    peer_inventory_hosts(&edge(&net, "laptop"), desk)
        .await
        .expect("the laptop calls the restarted desk");
    println!("the desk was seen down, listed offline, and up again with the same key");

    // Trust is each host's own: the desk forgetting the laptop leaves the
    // laptop's entry for the desk where it was.
    net.untrust("desk", "laptop").await.unwrap();
    assert!(!edge(&net, "desk").is_trusted(laptop));
    assert!(edge(&net, "laptop").is_trusted(desk));
    let row = row_until(
        &mut fleet,
        desk,
        "the laptop still listing the desk",
        |row| row.trust == wire::Trust::Trusted as i32,
    )
    .await;
    println!(
        "the desk forgot the laptop; the laptop still lists {}",
        row.name
    );

    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_direct_link_keeps_its_session_through_a_rebind_a_held_transfer_and_a_poor_network() {
    use provider_fakes::script::Step;
    use wire::client_service_server::ClientService as _;

    let say = |text: &str| Step::Text {
        chunks: vec![text.to_owned()],
    };
    let wait = |gate: &str| Step::WaitFor { path: gate.into() };
    let listening = |name: &str| testnet::HostDecl {
        name: name.to_owned(),
        lan: true,
        ..testnet::HostDecl::default()
    };
    let mut net = Net::start(
        Topology::new()
            .host_decl(listening("desk"))
            .host_decl(listening("laptop"))
            .agent(
                testnet::AgentDecl::new("worker", "desk")
                    .prompt("Keep me posted.")
                    .steps(vec![
                        wait("one"),
                        say("after the rebind"),
                        wait("two"),
                        say("beside a held transfer"),
                        wait("three"),
                        say("through a poor network"),
                        Step::TurnEnd,
                    ]),
            ),
    )
    .await
    .unwrap();
    let desk = host_id(&net, "desk");
    let worker = net.agent("worker").unwrap().id;

    // The laptop reaches the desk's listener through a stretch of network
    // the test controls.
    let gate = testnet::UdpGate::start(edge(&net, "desk").lan_addr().unwrap())
        .await
        .unwrap();
    net.trust("desk", "laptop").await.unwrap();
    net.trust("laptop", "desk").await.unwrap();
    edge(&net, "laptop").dial(desk, gate.addr());
    until_via(&net, "laptop", "desk", HostVia::Direct).await;
    let mut session = open_session(&edge(&net, "laptop"), desk, worker)
        .await
        .unwrap();

    // The laptop's network changes under it: its socket moves, the QUIC
    // connection migrates with it, and the open session carries on.
    let moved = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let moved_to = moved.local_addr().unwrap();
    edge(&net, "laptop").rebind_lan(moved).unwrap();
    net.open_gate("one").unwrap();
    until_item(&mut session, "after the rebind").await;
    assert_eq!(edge(&net, "laptop").via(desk).await, HostVia::Direct);
    println!("the laptop moved to {moved_to}; the session carried on");

    // A blob fetch held mid-transfer does not hold up the session beside
    // it, and when it goes on it brings exactly the bytes that were stored.
    let bytes: Vec<u8> = (0..3 * 1024 * 1024_u32).map(|n| (n % 251) as u8).collect();
    let stored = net
        .client("desk")
        .unwrap()
        .put_blob(tonic::Request::new(wire::PutBlobRequest {
            agent_id: worker.as_bytes().to_vec(),
            name: "build.log".to_owned(),
            mime: "text/plain".to_owned(),
            bytes: bytes.clone(),
        }))
        .await
        .unwrap()
        .into_inner();
    let mut hold = edge(&net, "laptop").hold_next_bulk_response(desk);
    let fetch = tokio::spawn({
        let laptop = edge(&net, "laptop");
        let hash = stored.hash.clone();
        async move {
            laptop
                .bulk_peer(desk)
                .await
                .unwrap()
                .get_blob(wire::GetBlobRequest {
                    agent_id: worker.as_bytes().to_vec(),
                    hash,
                })
                .await
                .map(tonic::Response::into_inner)
        }
    });
    tokio::time::timeout(testnet::PATIENCE, hold.entered())
        .await
        .expect("the transfer's first data arrives")
        .unwrap();
    net.open_gate("two").unwrap();
    until_item(&mut session, "beside a held transfer").await;
    assert!(!fetch.is_finished(), "the transfer is still held");
    hold.release();
    let fetched = tokio::time::timeout(testnet::PATIENCE, fetch)
        .await
        .expect("the released transfer finishes")
        .unwrap()
        .expect("the blob arrives");
    assert_eq!(fetched.blob.unwrap().hash, stored.hash);
    assert!(fetched.bytes == bytes, "the bytes are the ones stored");
    println!(
        "a {} byte transfer held mid-flight; the session went on beside it; the bytes match",
        bytes.len()
    );

    // Five datagrams in a hundred lost each way, and a hundred
    // milliseconds each way: the link and its session carry on.
    gate.set_faults(testnet::Faults {
        loss_percent: 5,
        delay: Duration::from_millis(100),
    });
    net.open_gate("three").unwrap();
    until_item(&mut session, "through a poor network").await;
    peer_inventory_hosts(&edge(&net, "laptop"), desk)
        .await
        .expect("calls go through a poor network");
    println!("five percent loss and 200 ms round trips: the session and calls carry on");

    net.shutdown().await.unwrap();
}
