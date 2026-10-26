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
use node::harness::{Advertisement, LiveRoute};
use support::*;
use testnet::{Net, RELAY_HOST, Topology, holds_for, until};
use wire::begin_pair_request;
use wire::client_service_server::ClientService as _;

#[tokio::test(flavor = "multi_thread")]
async fn hosts_on_one_account_reach_each_other_through_the_relay_over_quic_or_tcp() {
    // The desk is on a network of its own; the phone's network is its gate
    // to the relay, which a router would take it past.
    let mut net = Net::start(
        Topology::new()
            .relay(&["ada"])
            .host_decl(testnet::HostDecl {
                name: "desk".to_owned(),
                router: Some(testnet::RouterKind::Strict),
                ..testnet::HostDecl::default()
            })
            .host("phone"),
    )
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
    // calls it, over their own link, which the relay forwards but cannot
    // read.
    until_via(&net, "desk", "phone", LiveRoute::Relay).await;
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

    // The relay forwards links but serves nothing: a call addressed to the
    // relay itself is refused.
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
    until_via(&net, "desk", "phone", LiveRoute::Relay).await;
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
    let mut admin = admin(net, host).await;
    admin
        .pause_profile(wire::PauseProfileRequest {})
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
    admin
        .resume_profile(wire::ResumeProfileRequest {})
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
    let started = start_pairing(&net, "desk", pin_window()).await.unwrap();
    for second in [pin_window(), qr_window()] {
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
        assert!(peer.expires_at_ms > now_ms);
        assert!(peer.expires_at_ms <= now_ms + 5 * 60 * 1000 + 1000);
        assert!(edge(&net, "desk").trusted().is_empty());
        assert!(edge(&net, "laptop").trusted().is_empty());
        admin(&net, "laptop")
            .await
            .abandon_pair(wire::AbandonPairRequest {
                token: pending.token,
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
    // refused as locked out, not as a PIN that did not match, until
    // someone opens a new window, and the host says so to whoever opened
    // it.
    let started = start_pairing(&net, "shed", pin_window()).await.unwrap();
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
    assert!(
        closed.code() == tonic::Code::PermissionDenied && closed.message() == "PAIRING_LOCKED",
        "the right PIN after five wrong ones reads as a wrong PIN: {closed:?}"
    );
    assert!(edge(&net, "shed").pairing_locked_out());
    assert!(edge(&net, "shed").trusted().is_empty());
    println!("five wrong guesses closed the window; the right PIN then: {closed:?}");

    // A window lasts its time and no longer, and a new one opens after it.
    let started = start_pairing(
        &net,
        "attic",
        wire::StartPairingRequest {
            ttl_seconds: Some(1),
            ..pin_window()
        },
    )
    .await
    .unwrap();
    assert_eq!(started.ttl_seconds, 1);
    // The window keeps to the net's policy clock, as every daemon's
    // deadlines do.
    net.advance(Duration::from_secs(1)).unwrap();
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
    let started = start_pairing(&net, "attic", pin_window())
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
    let started = start_pairing(&net, "porch", qr_window()).await.unwrap();
    let Some(wire::pairing_window::Secret::QrSecret(secret)) = started.secret else {
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
    assert_eq!(pending.via, wire::RouteKind::Direct as i32);
    confirm_pair(&net, "laptop", pending.token).await.unwrap();
    println!("a QR code paired over its second address after {took:?} on the silent first");

    net.shutdown().await.unwrap();
}

/// A pairing whose start lost its reply to a daemon that went away is run
/// again by the person: the window died with the daemon, so nothing stuck
/// is left open to refuse the re-run, the re-run opens a fresh window with
/// a fresh PIN, and only that PIN pairs.
#[tokio::test(flavor = "multi_thread")]
async fn a_pairing_rerun_after_the_daemon_restarts_opens_a_fresh_window() {
    let mut net = Net::start(
        Topology::new()
            .host_decl(lan_host("desk", "home"))
            .host_decl(lan_host("laptop", "home")),
    )
    .await
    .unwrap();
    let desk = host_id(&net, "desk");
    let first = start_pairing(&net, "desk", pin_window()).await.unwrap();
    assert!(edge(&net, "desk").pairing_active());

    // The daemon crashes mid-pairing, before the PIN reaches anyone.
    net.kill_daemon("desk").await.unwrap();
    net.restart_daemon("desk").await.unwrap();
    let open = admin(&net, "desk")
        .await
        .get_pairing_status(wire::GetPairingStatusRequest {})
        .await
        .unwrap()
        .into_inner()
        .active;
    assert!(!open, "no window outlives the daemon that opened it");

    // The person runs it again: a window opens, not a refusal, and its PIN
    // is a new one.
    let second = start_pairing(&net, "desk", pin_window())
        .await
        .expect("a re-run after the restart opens a window");
    assert_ne!(pin_of(&second), pin_of(&first), "a fresh PIN");
    let stale = begin_pair(
        &net,
        "laptop",
        Some(desk),
        begin_pair_request::Secret::Pin(pin_of(&first)),
        second.addrs.clone(),
    )
    .await
    .expect_err("the lost window's PIN pairs nothing");
    assert!(is_invalid_secret(&stale), "{stale:?}");
    let pending = begin_pair(
        &net,
        "laptop",
        Some(desk),
        begin_pair_request::Secret::Pin(pin_of(&second)),
        second.addrs.clone(),
    )
    .await
    .expect("the fresh PIN pairs");
    let peer = confirm_pair(&net, "laptop", pending.token).await.unwrap();
    assert_eq!(peer.host_id, desk.as_bytes());
    assert!(edge(&net, "laptop").is_trusted(desk));
    println!(
        "restarted mid-pairing: no window left open, the re-run's fresh PIN paired, the lost one did not"
    );

    net.shutdown().await.unwrap();
}

/// `amux pair --demo --pin <digits> --for <duration>`, as an operator
/// leaves a demo machine for an app reviewer: the command returns at once,
/// the daemon holds the chosen PIN open, a wrong guess does not close it,
/// and every device that presents the PIN pairs, until `amux pair
/// --cancel` closes it.
#[tokio::test(flavor = "multi_thread")]
async fn demo_pin_from_the_cli() {
    let net = Net::start(
        Topology::new()
            .host_decl(lan_host("desk", "home"))
            .host_decl(lan_host("laptop", "home"))
            .host_decl(lan_host("phone", "home")),
    )
    .await
    .unwrap();
    let desk = host_id(&net, "desk");
    let config = net.host("desk").unwrap().config.clone();
    let amux = |args: &'static [&'static str]| {
        let config = config.clone();
        async move {
            let ran = tokio::process::Command::new(testnet::Binaries::built().amux())
                .arg("--config")
                .arg(&config)
                .args(args)
                .env_remove("AMUX_LOG")
                .output()
                .await
                .unwrap();
            let out = String::from_utf8_lossy(&ran.stdout).into_owned();
            assert!(
                ran.status.success(),
                "amux {args:?}: {out}{}",
                String::from_utf8_lossy(&ran.stderr)
            );
            out
        }
    };

    let said = amux(&["pair", "--demo", "--pin", "246810", "--for", "30d"]).await;
    assert!(said.contains("Pairing PIN: 246 810"), "{said}");
    assert!(said.contains("Demo pairing is open for 30 days"), "{said}");
    assert!(
        edge(&net, "desk").pairing_active(),
        "the window outlives the command"
    );
    // A second window is refused, and the words say how to close a demo
    // window, which has no command of its own to stop.
    let second = tokio::process::Command::new(testnet::Binaries::built().amux())
        .arg("--config")
        .arg(&config)
        .args(["pair", "--demo", "--pin", "135790", "--for", "1h"])
        .env_remove("AMUX_LOG")
        .output()
        .await
        .unwrap();
    assert!(!second.status.success(), "a second window is refused");
    let refused = String::from_utf8_lossy(&second.stderr);
    assert!(
        refused.contains("`amux pair --cancel` closes it"),
        "{refused}"
    );
    let addrs = vec![edge(&net, "desk").lan_addr().unwrap().to_string()];

    let wrong = begin_pair(
        &net,
        "laptop",
        Some(desk),
        begin_pair_request::Secret::Pin("135790".to_owned()),
        addrs.clone(),
    )
    .await
    .expect_err("a wrong PIN pairs nothing");
    assert!(is_invalid_secret(&wrong), "{wrong:?}");
    for device in ["laptop", "phone"] {
        let pending = begin_pair(
            &net,
            device,
            Some(desk),
            begin_pair_request::Secret::Pin("246810".to_owned()),
            addrs.clone(),
        )
        .await
        .unwrap_or_else(|status| panic!("{device} pairs with the demo PIN: {status:?}"));
        confirm_pair(&net, device, pending.token).await.unwrap();
        assert!(edge(&net, device).is_trusted(desk), "{device}");
        assert!(
            edge(&net, "desk").pairing_active(),
            "{device}'s pairing leaves the window open"
        );
    }

    let said = amux(&["pair", "--cancel"]).await;
    assert!(said.contains("Pairing mode closed."), "{said}");
    assert!(!edge(&net, "desk").pairing_active());
    println!(
        "amux pair --demo held 246810 open through a wrong guess and two pairings; --cancel closed it"
    );

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
    until_via(&net, "laptop", "desk", LiveRoute::Direct).await;
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
        edge(&net, "laptop").via(impostor).await != LiveRoute::Offline
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
        LiveRoute::Offline,
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
    assert_eq!(edge(&net, "laptop").via(desk).await, LiveRoute::Offline);
    assert_eq!(edge(&net, "desk").via(laptop).await, LiveRoute::Offline);
    assert!(
        peer_inventory_hosts(&edge(&net, "laptop"), desk)
            .await
            .is_err()
    );
    pair(&net, "desk", "laptop").await;
    until_via(&net, "laptop", "desk", LiveRoute::Direct).await;
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

/// Pairing with a host under an id this host already trusts replaces the
/// pinned key, and a link the old key still holds open is closed, saying
/// the trust was replaced, before the new key is trusted: nothing opened
/// under the old key outlives the replacement.
#[tokio::test(flavor = "multi_thread")]
async fn a_pairing_that_replaces_a_hosts_key_closes_the_old_keys_live_link_first() {
    // Nothing discovers anything, and the desk listens on no LAN: the one
    // link between the old desk and the laptop is the one held here.
    let mut net = Net::start(
        Topology::new()
            .host_decl(testnet::HostDecl {
                name: "desk".to_owned(),
                ..testnet::HostDecl::default()
            })
            .host_decl(undiscovered_lan_host("laptop", "home")),
    )
    .await
    .unwrap();
    let desk = host_id(&net, "desk");
    net.trust("desk", "laptop").await.unwrap();
    net.trust("laptop", "desk").await.unwrap();
    let old = edge(&net, "desk")
        .link_in_process(&edge(&net, "laptop"))
        .unwrap();
    until_via(&net, "laptop", "desk", LiveRoute::Direct).await;
    let old_key = edge(&net, "desk").public_key().to_vec();

    // Another machine starts under the desk's id with a key of its own,
    // while the old desk and its link stay up.
    net.add_host(undiscovered_lan_host("new-desk", "home"))
        .await
        .unwrap();
    net.stop_daemon("new-desk").await.unwrap();
    net.replace_profile("new-desk", desk).unwrap();
    net.restart_daemon("new-desk").await.unwrap();
    assert_eq!(edge(&net, "new-desk").host_id(), desk);
    let new_key = edge(&net, "new-desk").public_key().to_vec();
    assert_ne!(new_key, old_key);

    // The moment the laptop's side of the old link ends, read what the
    // laptop pins for the desk.
    let closed = tokio::spawn({
        let laptop = edge(&net, "laptop");
        async move {
            let end = old.accepted_end().await;
            let pinned = laptop
                .trusted()
                .into_iter()
                .find(|(host, ..)| *host == desk)
                .map(|(.., key)| key);
            (end, pinned)
        }
    });
    pair(&net, "new-desk", "laptop").await;
    let (end, pinned_at_close) = tokio::time::timeout(Duration::from_secs(10), closed)
        .await
        .expect("the old key's link closes when its key is replaced")
        .unwrap();
    let end = end.expect("the laptop ends the old link with a reason");
    assert!(end.contains("peer trust was replaced"), "{end}");
    assert_ne!(
        pinned_at_close.as_ref(),
        Some(&new_key),
        "the old link closed before the new key was trusted"
    );
    let pinned = edge(&net, "laptop")
        .trusted()
        .into_iter()
        .find(|(host, ..)| *host == desk)
        .map(|(.., key)| key);
    assert_eq!(pinned, Some(new_key), "the pairing replaced the pinned key");
    println!("the old desk's live link closed ({end}) before its replacement key was trusted");

    net.shutdown().await.unwrap();
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
        async || edge(&net, "phone").via(desk).await != LiveRoute::Offline,
    )
    .await;
    let first = edge(&net, "desk").lan_addr().unwrap();
    assert_eq!(first, held.local_addr().unwrap());
    net.discovery()
        .announce(advert(&net, "desk", first, "home"));
    until_via(&net, "phone", "desk", LiveRoute::Direct).await;
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
    until_via(&net, "phone", "desk", LiveRoute::Direct).await;
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

/// A host revoked while it was away has no link to hear the revocation
/// close: it learns it when it next dials, from the desk's handshake
/// refusing its certificate, and reads the desk as no longer trusting it
/// rather than as offline.
#[tokio::test(flavor = "multi_thread")]
async fn a_host_revoked_while_away_reads_the_desk_as_revoked_once_it_dials_through_the_relay() {
    let mut net = Net::start_with(
        Topology::new()
            .relay(&["ada"])
            .host_decl(relay_host("desk", "ada"))
            // The tablet's own dialler runs (a host that dials found hosts
            // dials through a relay too); the desk listens on no LAN, so
            // the relay is the only way to it.
            .host_decl(testnet::HostDecl {
                lan: true,
                ..relay_host("tablet", "ada")
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
    net.trust("desk", "tablet").await.unwrap();
    net.trust("tablet", "desk").await.unwrap();
    until_via(&net, "tablet", "desk", LiveRoute::Relay).await;
    open_session(&edge(&net, "tablet"), desk, worker)
        .await
        .expect("the tablet reaches the desk through the relay");

    net.stop_daemon("tablet").await.unwrap();
    net.untrust("desk", "tablet").await.unwrap();
    net.restart_daemon("tablet").await.unwrap();

    // Back up, the tablet hears from the relay that the desk is online and
    // tries it: the desk's handshake refuses its certificate.
    let mut fleet = net.observe_inventory("tablet").await.unwrap();
    row_until(
        &mut fleet,
        desk,
        "the tablet to read the desk as no longer trusting it",
        |row| row.revoked(),
    )
    .await;
    let refused = open_session(&edge(&net, "tablet"), desk, worker)
        .await
        .expect_err("a revoked host opens nothing");
    assert_eq!(
        (refused.code(), refused.message()),
        (tonic::Code::Unauthenticated, node::TRUST_REVOKED),
        "a call through the relay is refused as no longer trusted"
    );
    // Nor does it go on dialling a host that refused it: once the refused
    // call's own packets have settled, the relay forwards nothing more
    // from it over two of the dialler's backoff pauses (1 s, then 2 s).
    let forwarded = || net.relay().unwrap().forward_counts().forwarded;
    let mut settled = forwarded();
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let now = forwarded();
        if now == settled {
            break;
        }
        settled = now;
    }
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert_eq!(
        forwarded(),
        settled,
        "the tablet dialled the desk again after it was refused as revoked"
    );
    println!(
        "the tablet, revoked while stopped, dialled through the relay once and reads the desk as revoked"
    );

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
            .host_decl(relay_host("tablet", "ada"))
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
    until_via(&net, "phone", "desk", LiveRoute::Direct).await;
    until_via(&net, "tablet", "desk", LiveRoute::Relay).await;

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
            .subscribe_inventory(wire::SubscribeInventoryRequest {})
            .await
            .unwrap()
            .into_inner();
        held.push((host, session, inventory));
    }

    // Revoking closes every stream the revoked host holds, at once, and
    // neither route lets it back in: the desk no longer holds its key, and
    // its link closing said why, so the host reads the desk as no longer
    // trusting it rather than as gone. The relay may still say the host is
    // online, as it says of any host on the account; saying so grants
    // nothing.
    for (host, mut session, mut inventory) in held {
        let id = host_id(&net, host);
        let mut fleet = net.observe_inventory(host).await.unwrap();
        net.untrust("desk", host).await.unwrap();
        let session_end = ended(&mut session, &format!("{host}'s session")).await;
        let inventory_end = ended(&mut inventory, &format!("{host}'s inventory")).await;
        row_until(
            &mut fleet,
            desk,
            &format!("{host} to read the desk as no longer trusting it"),
            |row| row.revoked(),
        )
        .await;
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
             it reads the desk as revoked and its reopen is refused; \
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
    // the exchange runs over a handshake the relay forwards and cannot read,
    // with a QR secret as well as a PIN.
    let links_before = net.relay().unwrap().links("ada").await;
    let started = start_pairing(&net, "desk", qr_window()).await.unwrap();
    let Some(wire::pairing_window::Secret::QrSecret(secret)) = started.secret else {
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
    assert_eq!(pending.via, wire::RouteKind::Relay as i32);
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
    until_via(&net, "desk", "laptop", LiveRoute::Relay).await;
    never("the relay routes between accounts", async || {
        edge(&net, "shop").via(desk).await != LiveRoute::Offline
            || edge(&net, "desk").via(shop).await != LiveRoute::Offline
    })
    .await;
    let started = start_pairing(&net, "desk", pin_window()).await.unwrap();
    let across = begin_pair(
        &net,
        "shop",
        Some(desk),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        Vec::new(),
    )
    .await
    .expect_err("no pairing route through another account's relay");
    // The other host not being reachable is the daemon's answer, typed
    // unreachable; Unavailable would say this host's daemon went away.
    assert_eq!(across.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        <wire::Error as prost::Message>::decode(across.details())
            .ok()
            .and_then(|error| error.reason()),
        Some(wire::ErrorReason::HostUnreachable)
    );
    start_pairing(&net, "desk", qr_window())
        .await
        .expect_err("the desk's PIN window is still open");
    admin(&net, "desk")
        .await
        .cancel_pairing(wire::CancelPairingRequest {})
        .await
        .unwrap();
    let qr = start_pairing(&net, "desk", qr_window()).await.unwrap();
    let Some(wire::pairing_window::Secret::QrSecret(secret)) = qr.secret else {
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
    admin(&net, "desk")
        .await
        .cancel_pairing(wire::CancelPairingRequest {})
        .await
        .unwrap();
    let started = start_pairing(&net, "desk", pin_window()).await.unwrap();
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
    until_via(&net, "shop", "desk", LiveRoute::Direct).await;
    peer_inventory_hosts(&edge(&net, "shop"), desk)
        .await
        .expect("the shop calls the desk it paired with");

    // A second profile on the desk's installation is a host of its own:
    // its own id and key, its own pairing window and attempt budget.
    let work = door(&net, "desk")
        .await
        .create_profile(wire::CreateProfileRequest {
            label: Some("work".to_owned()),
        })
        .await
        .unwrap()
        .into_inner();
    let work_profile = node::ProfileId::from_slice(&work.id).unwrap();
    let work_edge = net.profile_edge("desk", work_profile).unwrap();
    assert_ne!(work_edge.host_id(), desk);
    assert_ne!(work_edge.public_key(), edge(&net, "desk").public_key());
    let work_start = net
        .admin_of("desk", work_profile)
        .await
        .unwrap()
        .start_pairing(pin_window())
        .await
        .expect("the work profile opens its own window beside the desk's")
        .into_inner();
    let desk_start = start_pairing(&net, "desk", pin_window()).await.unwrap();
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
    until_via(&net, "laptop", "desk", LiveRoute::Direct).await;
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
        LiveRoute::Offline,
        "a key one profile pinned never authenticates into another"
    );
    assert_eq!(work_edge.via(laptop).await, LiveRoute::Offline);
    println!("work profile {work_profile}: own key, own window, the desk's peers refused");

    // Administration stays on this machine: a trusted peer's tunnel serves
    // neither the installation's profiles nor a profile's pairing, and a
    // profile's socket serves its own pairing (the work profile's window
    // above opened there) but not the installation's profiles.
    let tunnel = edge(&net, "laptop").channel(desk).await.unwrap();
    let over_tunnel = wire::profile_service_client::ProfileServiceClient::new(tunnel.clone())
        .list_profiles(wire::ListProfilesRequest {})
        .await
        .expect_err("no profile administration over a peer tunnel");
    assert_eq!(over_tunnel.code(), tonic::Code::Unimplemented);
    let pairing_over_tunnel =
        wire::profile_admin_service_client::ProfileAdminServiceClient::new(tunnel)
            .start_pairing(pin_window())
            .await
            .expect_err("no pairing administration over a peer tunnel");
    assert_eq!(pairing_over_tunnel.code(), tonic::Code::Unimplemented);
    let socket = node::profile_dir(
        &net.host("desk").unwrap().data_dir,
        net.host("desk").unwrap().profile,
    )
    .join(node::PROFILE_SOCKET);
    let on_socket = wire::profile_service_client::ProfileServiceClient::new(
        testnet::local_channel(&socket).await.unwrap(),
    )
    .list_profiles(wire::ListProfilesRequest {})
    .await
    .expect_err("no installation administration on a profile's socket");
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
            .subscribe_inventory(wire::SubscribeInventoryRequest {})
            .await
            .unwrap()
            .into_inner();
        let first = loop {
            match events.message().await.unwrap().unwrap().of {
                Some(wire::inventory_event::Of::Host(host)) if host.host_id() == own.as_bytes() => {
                    break host;
                }
                Some(wire::inventory_event::Of::CaughtUp(_)) => {
                    panic!("a profile's socket lists its own host");
                }
                _ => {}
            }
        };
        assert!(first.trusted().is_some());
    }

    net.shutdown().await.unwrap();
}

/// A host on the relay alone: on a network of its own behind a strict
/// router, with no listener and no discovery.
fn relay_host(name: &str, account: &str) -> testnet::HostDecl {
    testnet::HostDecl {
        name: name.to_owned(),
        account: Some(account.to_owned()),
        router: Some(testnet::RouterKind::Strict),
        ..testnet::HostDecl::default()
    }
}

/// An account that lapses from Pro to Free stops being carried at once:
/// the relay stops forwarding between the account's hosts and tells each
/// end, so a host the relay alone reaches is away and nothing reaches it
/// through the relay any more, even over a channel that was opened while
/// the account was Pro. A host told the peer is away must not still be
/// able to call it.
#[tokio::test(flavor = "multi_thread")]
async fn a_lapsed_account_stops_carrying_calls_the_relay_carried_while_it_was_pro() {
    let topology = Topology::new()
        .relay(&["ada"])
        .host_decl(relay_host("desk", "ada"))
        .host_decl(relay_host("laptop", "ada"));
    let mut net = Net::start(topology).await.unwrap();
    // The desk, which makes every call here, trusts last. Trusting first,
    // its first stream could reach the laptop before the laptop trusted it
    // back, be refused, and leave the desk holding that refusal, which its
    // own later trust clears and the laptop's does not.
    net.trust("laptop", "desk").await.unwrap();
    net.trust("desk", "laptop").await.unwrap();
    let laptop = host_id(&net, "laptop");
    until_via(&net, "desk", "laptop", LiveRoute::Relay).await;
    peer_inventory_hosts(&edge(&net, "desk"), laptop)
        .await
        .expect("on Pro the relay carries the desk's call to the laptop");
    println!("Pro: the desk calls the laptop through the relay");

    // The subscription lapses. Both hosts learn it at once and their links
    // reauthenticate as Free; the desk's client is told the laptop is away.
    net.set_tier("ada", testnet::TierDecl::Free).await.unwrap();
    assert_eq!(tier(&net, "desk"), Some(node::Tier::Free));
    assert_eq!(tier(&net, "laptop"), Some(node::Tier::Free));
    let mut fleet = net.observe_inventory("desk").await.unwrap();
    row_until(
        &mut fleet,
        laptop,
        "the laptop away on a free account",
        |row| row.away(),
    )
    .await;
    println!("Free: the desk's client shows the laptop away");

    let call = peer_inventory_hosts(&edge(&net, "desk"), laptop).await;
    assert!(
        call.is_err(),
        "a lapsed account's relay carries nothing, yet the desk's call reached the laptop: {call:?}"
    );

    net.shutdown().await.unwrap();
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

    // On a free account the relay lists the account's hosts and forwards
    // nothing between them, for calls or for pairing; the refused pairing
    // leaves the responder's window open, as the secret never reached it.
    until_via(&net, "desk", "phone", LiveRoute::Relay).await;
    let call = peer_inventory_hosts(&edge(&net, "phone"), desk)
        .await
        .expect_err("nothing forwarded on a free account");
    let mut phones_fleet = net.observe_inventory("phone").await.unwrap();
    row_until(
        &mut phones_fleet,
        desk,
        "the phone to read the desk away, refused for the plan",
        |row| row.away(),
    )
    .await;
    let started = start_pairing(&net, "desk", pin_window()).await.unwrap();
    let pairing = begin_pair(
        &net,
        "phone",
        Some(desk),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        Vec::new(),
    )
    .await
    .expect_err("no pairing forwarded on a free account");
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
    assert_eq!(pending.via, wire::RouteKind::Direct as i32);
    confirm_pair(&net, "tablet", pending.token).await.unwrap();
    row_until(
        &mut fleet,
        desk,
        "the tablet's client told of the direct link",
        |row| row.online() == Some(wire::RouteKind::Direct),
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
        .subscribe_inventory(wire::SubscribeInventoryRequest {})
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
    until_via(&net, "laptop", "desk", LiveRoute::Direct).await;
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
        peer.subscribe_session(wire::PeerSubscribeSessionRequest {
            agent_id: short.clone(),
            from: Some(wire::peer_subscribe_session_request::From::Tail(5)),
        })
        .await
        .map(|_| ()),
    ));
    answers.push((
        "Subscribe to an agent the desk never had",
        peer.subscribe_session(wire::PeerSubscribeSessionRequest {
            agent_id: unknown.clone(),
            from: Some(wire::peer_subscribe_session_request::From::Tail(5)),
        })
        .await
        .map(|_| ()),
    ));
    answers.push((
        "Fetch with a three-byte agent id",
        peer.list_items(wire::ListItemsRequest {
            agent_id: short.clone(),
            before_order: None,
            page_size: 10,
        })
        .await
        .map(|_| ()),
    ));
    answers.push((
        "Get with an empty key",
        peer.get_item(wire::GetItemRequest {
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
        peer.send_message(wire::PeerSendMessageRequest {
            envelope: Some(wire::Envelope::default()),
        })
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
    assert_eq!(edge(&net, "laptop").via(desk).await, LiveRoute::Direct);
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
    until_via(&net, "laptop", "desk", LiveRoute::Direct).await;
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
    profile: &[u8],
    refresh: String,
    adopt: bool,
) -> Result<wire::Profile, tonic::Status> {
    door(net, host)
        .await
        .bind_profile(wire::BindProfileRequest {
            profile_id: Some(profile.to_vec()),
            cloud_url: net.relay().unwrap().url().to_owned(),
            staged_refresh_token: refresh,
            adopt_non_pristine: adopt,
            ..wire::BindProfileRequest::default()
        })
        .await
        .map(tonic::Response::into_inner)
}

async fn profile_info(net: &Net, host: &str, profile: &[u8]) -> wire::Profile {
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

async fn create(net: &Net, host: &str, label: &str) -> (Vec<u8>, std::sync::Arc<node::Edge>) {
    let info = door(net, host)
        .await
        .create_profile(wire::CreateProfileRequest {
            label: Some(label.to_owned()),
        })
        .await
        .unwrap()
        .into_inner();
    let edge = net
        .profile_edge(host, node::ProfileId::from_slice(&info.id).unwrap())
        .unwrap();
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
    until_via(&net, "laptop", "desk", LiveRoute::Direct).await;
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

    // Bob's account is revoked. The relay credential the work profile holds
    // stays valid, so nothing changes until its refresh, five minutes before
    // it expires on this net: the account service refuses that, and the
    // work profile has to sign in again. The main profile's link to the
    // relay refreshes at the same moment and does not notice.
    net.relay().unwrap().revoke("bob");
    net.advance(testnet::CREDENTIAL_TTL - Duration::from_secs(5 * 60))
        .unwrap();
    until("the work profile to need a sign-in", || async {
        let info = profile_info(&net, "desk", &work).await;
        matches!(
            info.relay_state(),
            Some(wire::relay_link::State::SignInAgain(_))
        )
        .then_some(())
        .ok_or_else(|| format!("cloud {:?}", info.cloud))
    })
    .await
    .unwrap();
    assert!(
        profile_info(&net, "desk", &main)
            .await
            .relay_connected()
            .is_some()
    );
    assert_eq!(net.relay().unwrap().links("ada").await, vec![(desk, 1)]);

    // Pausing closes the cloud link only: the laptop's direct link stays.
    // Resuming, twice over, brings back one link.
    admin(&net, "desk")
        .await
        .pause_profile(wire::PauseProfileRequest {})
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
    assert_eq!(edge(&net, "desk").via(laptop).await, LiveRoute::Direct);
    for _ in 0..2 {
        admin(&net, "desk")
            .await
            .resume_profile(wire::ResumeProfileRequest {})
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
    admin(&net, "desk")
        .await
        .sign_out(wire::SignOutRequest {})
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
        profile_info(&net, "desk", &main).await.standing(),
        Some(&wire::cloud_standing::Standing::SignedOut(wire::Empty {}))
    );
    assert_eq!(edge(&net, "desk").via(laptop).await, LiveRoute::Direct);
    assert!(edge(&net, "desk").is_trusted(laptop));
    assert_eq!(edge(&net, "desk").public_key(), key.as_slice());
    let listed = net
        .runtime("desk")
        .unwrap()
        .read_store(|store| store.agents().unwrap().len())
        .await
        .unwrap();
    assert_eq!(listed, 1, "the worker is still the desk's");
    // The signed-out profile keeps its place for the account, across a
    // restart: a login naming no profile lands on it again.
    net.stop_daemon("desk").await.unwrap();
    net.restart_daemon("desk").await.unwrap();
    // A watcher from here on sees everything that follows, in order.
    let mut watch = door(&net, "desk")
        .await
        .subscribe_profiles(wire::SubscribeProfilesRequest {})
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
        (via == LiveRoute::Direct)
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
    drop(old_edge);
    door(&net, "desk")
        .await
        .delete_profile(wire::DeleteProfileRequest {
            profile_id: old.clone(),
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
    assert!(
        net.profile_edge("desk", node::ProfileId::from_slice(&old).unwrap())
            .is_err()
    );
    assert_eq!(
        net.relay().unwrap().links("ada").await,
        vec![(desk, 1)],
        "the other profiles keep routing"
    );
    assert_eq!(edge(&net, "desk").via(laptop).await, LiveRoute::Direct);

    // The watcher saw all of it, the deletion last.
    let mut removed = None;
    let mut upserts = 0;
    let deadline = tokio::time::Instant::now() + testnet::PATIENCE;
    while removed.is_none() {
        let event = tokio::time::timeout_at(deadline, watch.message())
            .await
            .expect("the watcher to see the deletion")
            .unwrap()
            .unwrap();
        match event.of {
            Some(wire::profile_event::Of::Upserted(_)) => upserts += 1,
            Some(wire::profile_event::Of::RemovedId(id)) => removed = Some(id),
            _ => {}
        }
    }
    assert_eq!(removed, Some(old));
    println!("the watcher saw {upserts} changes, then the deletion");

    net.shutdown().await.unwrap();
}

/// Waits until the inventory `fleet` watches describes `host` as `check`
/// wants, and returns that row.
async fn row_until(
    fleet: &mut testnet::InventoryObserver,
    host: uuid::Uuid,
    what: &str,
    check: impl Fn(&wire::HostEntry) -> bool,
) -> wire::HostEntry {
    let found = |events: &[wire::InventoryEvent]| {
        testnet::observe::inventory_hosts(events)
            .into_iter()
            .find(|row| row.host_id() == host.as_bytes())
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
        row.online().is_some() && row.account_signed_in() == Some(true)
    })
    .await;
    assert_eq!(row.online(), Some(wire::RouteKind::Direct));
    let mut desk_fleet = net.observe_inventory("desk").await.unwrap();
    row_until(
        &mut desk_fleet,
        laptop,
        "the laptop, never signed in",
        |row| row.account_signed_in() == Some(false),
    )
    .await;
    admin(&net, "desk")
        .await
        .sign_out(wire::SignOutRequest {})
        .await
        .unwrap();
    net.sever_link("desk", "laptop").unwrap();
    net.restore_link("desk", "laptop").await.unwrap();
    row_until(&mut fleet, desk, "the desk signed out", |row| {
        row.account_signed_in() == Some(false)
    })
    .await;
    net.sign_in("desk", "ada").await.unwrap();
    net.sever_link("desk", "laptop").unwrap();
    net.restore_link("desk", "laptop").await.unwrap();
    row_until(&mut fleet, desk, "the desk signed in again", |row| {
        row.account_signed_in() == Some(true)
    })
    .await;
    println!("the laptop saw the desk sign out and in, each at its next handshake");

    // A host on the account that nobody paired is seen through the relay,
    // and no call is made to it.
    until("the desk to see the tablet through the relay", || async {
        let via = edge(&net, "desk").via(tablet).await;
        (via == LiveRoute::Relay)
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
    let started = start_pairing(&net, "desk", pin_window()).await.unwrap();
    let pending = begin_pair(
        &net,
        "tablet",
        Some(desk),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        Vec::new(),
    )
    .await
    .expect("a PIN pairing through the relay");
    assert_eq!(pending.via, wire::RouteKind::Relay as i32);
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
        (via == LiveRoute::Offline)
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
        row.online().is_none()
    })
    .await;
    assert!(row.trusted().is_some());
    assert_eq!(row.account_signed_in(), Some(true));
    peer_inventory_hosts(&edge(&net, "laptop"), desk)
        .await
        .expect_err("no calls to a host that is down");
    net.restart_daemon("desk").await.unwrap();
    assert_eq!(host_id(&net, "desk"), desk);
    assert_eq!(edge(&net, "desk").public_key(), key.as_slice());
    // What the desk says of itself is what the laptop pinned, and asking
    // opens no pairing window.
    let identity = admin(&net, "desk")
        .await
        .get_device_identity(wire::GetDeviceIdentityRequest {})
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
        row.online().is_some()
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
        |row| row.trusted().is_some(),
    )
    .await;
    println!(
        "the desk forgot the laptop; the laptop still lists {}",
        row.name()
    );

    net.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_direct_link_keeps_its_session_through_a_rebind_a_large_transfer_and_a_poor_network() {
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
                        say("after the transfer"),
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
    until_via(&net, "laptop", "desk", LiveRoute::Direct).await;
    let mut session = open_session(&edge(&net, "laptop"), desk, worker)
        .await
        .unwrap();

    // The laptop's network changes under it: its socket moves, the QUIC
    // connection migrates with it, and the open session carries on.
    let moved = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let moved_to = moved.local_addr().unwrap();
    edge(&net, "laptop")
        .rebind(node::socket::LinkSocket::on_udp(moved).unwrap())
        .unwrap();
    net.open_gate("one").unwrap();
    until_item(&mut session, "after the rebind").await;
    assert_eq!(edge(&net, "laptop").via(desk).await, LiveRoute::Direct);
    println!("the laptop moved to {moved_to}; the session carried on");

    // A blob fetch brings exactly the bytes that were stored.
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
    let fetched = tokio::time::timeout(
        testnet::PATIENCE,
        edge(&net, "laptop")
            .peer(desk)
            .await
            .unwrap()
            .get_blob(wire::GetBlobRequest {
                agent_id: worker.as_bytes().to_vec(),
                hash: stored.hash.clone(),
            }),
    )
    .await
    .expect("the transfer finishes")
    .expect("the blob arrives")
    .into_inner();
    assert!(fetched.bytes == bytes, "the bytes are the ones stored");
    net.open_gate("two").unwrap();
    until_item(&mut session, "after the transfer").await;
    println!("a {} byte transfer brought the bytes stored", bytes.len());

    // Five datagrams in a hundred lost each way, and a hundred
    // milliseconds each way: the link and its session carry on.
    gate.set_faults(testnet::Faults {
        loss_percent: 5,
        delay: Duration::from_millis(100),
        ..testnet::Faults::default()
    });
    net.open_gate("three").unwrap();
    until_item(&mut session, "through a poor network").await;
    peer_inventory_hosts(&edge(&net, "laptop"), desk)
        .await
        .expect("calls go through a poor network");
    println!("five percent loss and 200 ms round trips: the session and calls carry on");

    net.shutdown().await.unwrap();
}

/// A crashed daemon takes all of its work down with it. A host that listens
/// on the LAN and is signed in to a relay account is killed and started
/// again: nothing of the dead run keeps its relay link dialling or its LAN
/// socket bound, so the next run listens on the same port and comes back.
#[tokio::test(flavor = "multi_thread")]
async fn a_killed_host_signed_in_to_the_relay_starts_again_on_its_port() {
    let mut net = Net::start(
        Topology::new()
            .relay(&["ada"])
            .host_decl(testnet::HostDecl {
                lan: true,
                ..relay_host("desk", "ada")
            }),
    )
    .await
    .unwrap();
    let desk = host_id(&net, "desk");
    until("the desk's relay link", || async {
        carrier(&net, "desk").map(|_| ()).ok_or("no carrier")
    })
    .await
    .unwrap();
    assert_eq!(net.relay().unwrap().links("ada").await, vec![(desk, 1)]);

    net.kill_daemon("desk").await.unwrap();
    println!("desk killed while signed in to the relay");

    net.restart_daemon("desk")
        .await
        .expect("the killed desk starts again: nothing of its dead run holds its LAN port");
    until("the restarted desk's relay link", || async {
        carrier(&net, "desk").map(|_| ()).ok_or("no carrier")
    })
    .await
    .unwrap();
    assert_eq!(
        net.relay().unwrap().links("ada").await,
        vec![(desk, 1)],
        "one relay link for the desk: the new run's, none left over from the dead one"
    );
    println!("desk restarted on its port with one relay link");

    net.shutdown().await.unwrap();
}

/// How long a peer may take to notice a dead QUIC connection: off the
/// phone its keepalive and idle timeout close it within forty seconds.
const NOTICE_PATIENCE: Duration = Duration::from_secs(60);

/// How long a host may take to dial a paired peer again once the peer can
/// be reached: a redial backs off, but not past half a minute.
const REDIAL_PATIENCE: Duration = Duration::from_secs(90);

/// A host that listens on the LAN and does not browse it: whatever links
/// it has come from pairing by address, as across a network discovery
/// cannot see (a VPN, another subnet).
fn undiscovered_lan_host(name: &str, scope: &str) -> testnet::HostDecl {
    testnet::HostDecl {
        discovery: false,
        ..lan_host(name, scope)
    }
}

/// A net on wall time with a gated LAN, so a real pairing can be cut,
/// holding desk in process and box as a real daemon process, paired by
/// address with desk dialling and box answering, and discovery off on both.
async fn paired_without_discovery() -> Net {
    paired_without_discovery_with(testnet::NetOptions::default()).await
}

async fn paired_without_discovery_with(options: testnet::NetOptions) -> Net {
    let mut net = Net::start_with(
        Topology::new(),
        testnet::NetOptions {
            clock: testnet::ClockMode::Wall,
            gated_lan: true,
            ..options
        },
    )
    .await
    .unwrap();
    let scope = net.topology().scope.clone();
    net.add_host(testnet::HostDecl {
        process: true,
        ..undiscovered_lan_host("box", &scope)
    })
    .await
    .unwrap();
    net.add_host(undiscovered_lan_host("desk", &scope))
        .await
        .unwrap();
    net.add_link("desk", "box").await.unwrap();
    assert!(net.link_up("desk", "box").await, "desk and box are linked");
    net
}

/// Waits until `a` and `b` route to each other directly, or, with `up`
/// false, until they no longer do.
async fn until_link(net: &Net, a: &str, b: &str, up: bool, within: Duration) {
    testnet::until_within(
        &format!(
            "{a} and {b} {}",
            if up { "linked directly" } else { "unlinked" }
        ),
        within,
        || async move {
            (net.link_up(a, b).await == up)
                .then_some(())
                .ok_or_else(|| format!("direct both ways: {}", !up))
        },
    )
    .await
    .unwrap_or_else(|stuck| panic!("{stuck:?}"));
}

/// A host keeps dialling a paired peer's last working direct address,
/// backing off, while the peer is away, whether or not anything discovers
/// it. Here the host that answered the pairing restarts: it stored no way
/// to reach the dialler, so the dialler must dial it again.
#[tokio::test(flavor = "multi_thread")]
async fn a_host_dials_the_peer_that_answered_its_pairing_again_after_that_peer_restarts() {
    let mut net = paired_without_discovery().await;

    net.stop_daemon("box").await.unwrap();
    net.restart_daemon("box").await.unwrap();
    println!("box, which answered the pairing, stopped and started again");

    until_link(&net, "desk", "box", true, REDIAL_PATIENCE).await;
    println!("desk dialled box again without discovery");

    net.shutdown().await.unwrap();
}

/// The same promise without a restart: the LAN between two paired hosts
/// goes dead until both notice, then comes back. Neither host restarts,
/// so only a redial to the address that last worked brings the link back.
#[tokio::test(flavor = "multi_thread")]
async fn hosts_paired_by_address_link_again_after_the_lan_between_them_comes_back() {
    let mut net = paired_without_discovery().await;

    net.sever_link("desk", "box").unwrap();
    until_link(&net, "desk", "box", false, NOTICE_PATIENCE).await;
    println!("the LAN between desk and box is dead and both have noticed");

    net.restore_link("desk", "box").await.unwrap();
    until_link(&net, "desk", "box", true, REDIAL_PATIENCE).await;
    println!("desk and box linked again without discovery");

    net.shutdown().await.unwrap();
}

/// A paired host whose link goes silently dead (every datagram dropped, no
/// close) is shown away once the link's keepalive and idle timeout run
/// out, not left for QUIC's default two-minute idle timeout: until then
/// every client keeps showing the far host's agents as live and current,
/// answers sent to it sit unconfirmed, and a phone keeps saying
/// "reachable" (QA: hosts and the phone looked current for 70 s to 3 min
/// after a total cut). A link closes at most keepalive plus idle after
/// the cut, forty seconds with a desktop's pair (`node::LinkLiveness`);
/// the desk here runs a pair of a quarter and one second, so the story
/// takes about one. The shorter timeout of a link's two ends wins, so the
/// box's own pair does not matter.
#[tokio::test(flavor = "multi_thread")]
async fn a_host_notices_a_silently_dead_link_at_its_keepalive_and_idle_timeout() {
    let liveness = node::LinkLiveness {
        keep_alive: Duration::from_millis(250),
        idle_timeout: Duration::from_secs(1),
    };
    let mut net = paired_without_discovery_with(testnet::NetOptions {
        edge: Some(std::sync::Arc::new(move |host, edge| {
            if host == "desk" {
                edge.liveness = liveness;
            }
        })),
        ..testnet::NetOptions::default()
    })
    .await;

    net.sever_link("desk", "box").unwrap();
    println!("every datagram between desk and box is dropped from now on");
    until_link(&net, "desk", "box", false, Duration::from_secs(10)).await;

    net.shutdown().await.unwrap();
}

fn sdk_prompt(text: &str) -> wire::Input {
    wire::Input::new(
        uuid::Uuid::new_v4().as_bytes().to_vec(),
        wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
                text: text.to_owned(),
                attachments: Vec::new(),
            })),
        },
    )
}

/// A call to an agent on a host the account's lapse put out of reach is
/// answered at once with the lapse as its reason, never a bare "cannot be
/// reached", so every client, the CLI and an agent's tools can say why.
/// QA: on a lapsed account every surface blamed the network, and agents'
/// sends reported the far host could not be reached while it was up.
#[tokio::test(flavor = "multi_thread")]
async fn a_call_to_a_host_only_a_lapsed_account_reached_names_the_lapse_at_once() {
    let topology = Topology::new()
        .relay(&["ada"])
        .host_decl(relay_host("desk", "ada"))
        .host_decl(relay_host("laptop", "ada"))
        .agent(
            testnet::AgentDecl::new("w", "laptop")
                .prompt("Wait.")
                .steps(vec![
                    provider_fakes::script::Step::Text {
                        chunks: vec!["Ready.".into()],
                    },
                    provider_fakes::script::Step::TurnEnd,
                ]),
        );
    let mut net = Net::start(topology).await.unwrap();
    net.trust("laptop", "desk").await.unwrap();
    net.trust("desk", "laptop").await.unwrap();
    let laptop = host_id(&net, "laptop");
    until_via(&net, "desk", "laptop", LiveRoute::Relay).await;
    net.settle("w").await.unwrap();
    let key = net.agent("w").unwrap().key();
    until("the desk to hold w's row", || {
        let (net, key) = (&net, &key);
        async move {
            let key = key.clone();
            let held = net
                .runtime("desk")
                .unwrap()
                .read_store(move |store| store.agent(&key).unwrap())
                .await
                .unwrap();
            held.map(|_| ()).ok_or_else(|| "not yet".to_owned())
        }
    })
    .await
    .unwrap();
    net.set_tier("ada", testnet::TierDecl::Free).await.unwrap();
    let mut fleet = net.observe_inventory("desk").await.unwrap();
    row_until(
        &mut fleet,
        laptop,
        "the laptop away on a free account",
        |row| row.away(),
    )
    .await;

    let request = wire::SendInputRequest {
        agent_id: net.agent("w").unwrap().id.as_bytes().to_vec(),
        input: Some(sdk_prompt("Are you there?")),
    };
    let started = std::time::Instant::now();
    let answer = tokio::time::timeout(
        Duration::from_secs(5),
        net.client("desk")
            .unwrap()
            .send_input(tonic::Request::new(request)),
    )
    .await;
    let took = started.elapsed();
    // A stop never leaves either: it is answered as unreachable, so a
    // client holds it as it holds the send, in words that name the lapse.
    let stop = tokio::time::timeout(
        Duration::from_secs(5),
        net.client("desk")
            .unwrap()
            .stop_agent(tonic::Request::new(wire::StopAgentRequest {
                agent_id: net.agent("w").unwrap().id.as_bytes().to_vec(),
            })),
    )
    .await
    .expect("the stop is answered at once, not after a deadline");
    net.shutdown().await.unwrap();
    println!("the desk's stop of w: {stop:?}");
    let stop = stop.expect_err("a stop to a host out of reach is not done");
    let stop_error = {
        use prost::Message as _;
        wire::Error::decode(stop.details()).expect("a typed error")
    };
    assert_eq!(
        stop_error.reason(),
        Some(wire::ErrorReason::HostUnreachable),
        "a stop that never left is held like an unreachable one: {stop_error:?}"
    );
    assert!(
        ["lapse", "subscription", "free plan"]
            .iter()
            .any(|word| stop_error.message.to_lowercase().contains(word)),
        "the stop's reason does not name the lapse: {stop_error:?}"
    );
    println!("the desk's send to w answered after {took:?}: {answer:?}");
    let answer = answer.expect("the send is answered at once, not after a deadline");
    // The reason may come as a typed error (payment required) or as a
    // refusal whose words or kind name the lapse; either names it.
    let words = match &answer {
        Err(status) => {
            use prost::Message as _;
            let code = wire::Error::decode(status.details())
                .ok()
                .map(|error| error.code());
            format!("{code:?} {}", status.message())
        }
        Ok(response) => format!("{:?}", response.get_ref()),
    }
    .to_lowercase();
    assert!(
        ["paymentrequired", "lapse", "subscription", "free plan"]
            .iter()
            .any(|word| words.contains(word)),
        "the refusal does not name the lapse: {answer:?}"
    );
}

/// The stated attachment limit is 64 MB of the attachment's own bytes:
/// one at the limit stores through a client's own socket (the channel's
/// message limit sits above it, with room for the request's framing), and
/// one over it is refused in plain words naming the limit. QA: a 64 MB
/// file failed with "h2 protocol error", blaming the runtime.
#[tokio::test(flavor = "multi_thread")]
async fn an_attachment_at_the_stated_limit_stores_and_one_over_it_is_refused_naming_the_limit() {
    let net = Net::start(
        Topology::new().host("desk").agent(
            testnet::AgentDecl::new("w", "desk")
                .prompt("Wait.")
                .steps(vec![
                    provider_fakes::script::Step::Text {
                        chunks: vec!["Ready.".into()],
                    },
                    provider_fakes::script::Step::TurnEnd,
                ]),
        ),
    )
    .await
    .unwrap();
    net.settle("w").await.unwrap();
    let socket = node::profile_dir(
        &net.host("desk").unwrap().data_dir,
        net.host("desk").unwrap().profile,
    )
    .join(node::PROFILE_SOCKET);
    let mut client = wire::client_service_client(testnet::local_channel(&socket).await.unwrap());
    const LIMIT: usize = 64 * 1024 * 1024;
    let put = |size: usize| wire::PutBlobRequest {
        agent_id: net.agent("w").unwrap().id.as_bytes().to_vec(),
        name: "big.bin".into(),
        mime: "application/octet-stream".into(),
        bytes: vec![7; size],
    };
    let at_limit = client.put_blob(put(LIMIT)).await;
    let over = client.put_blob(put(LIMIT + 1)).await;
    drop(client);
    net.shutdown().await.unwrap();
    println!(
        "at the limit: {:?}; over it: {:?}",
        at_limit.as_ref().map(|blob| blob.get_ref().size),
        over.as_ref().map(|blob| blob.get_ref().size)
    );
    assert!(
        at_limit.is_ok(),
        "an attachment of exactly 64 MB did not store: {:?}",
        at_limit.err()
    );
    let refusal = over.expect_err("an attachment over 64 MB is refused");
    assert!(
        refusal.message().contains("64 MB"),
        "the refusal does not name the limit: {refusal:?}"
    );
}

/// The host behind the relay, not the one that opened the session, moves
/// to another network mid-stream. Its socket rebinds; the relay finds its
/// link at the new address, by the connection quinn moved there, and goes
/// on forwarding both ways. The session carries on over the same link:
/// neither end's link log has it going down, and the relay holds one link
/// per host.
#[tokio::test(flavor = "multi_thread")]
async fn a_host_behind_the_relay_rebinds_mid_stream_and_its_session_carries_on() {
    use node::link_log::LinkTransition;
    use provider_fakes::script::Step;

    let say = |text: &str| Step::Text {
        chunks: vec![text.to_owned()],
    };
    let wait = |gate: &str| Step::WaitFor { path: gate.into() };
    let topology = Topology::new()
        .relay(&["ada"])
        .host_decl(relay_host("desk", "ada"))
        .host_decl(relay_host("laptop", "ada"))
        .agent(
            testnet::AgentDecl::new("worker", "desk")
                .prompt("Keep me posted.")
                .steps(vec![
                    wait("one"),
                    say("before the move"),
                    wait("two"),
                    say("after the move"),
                    Step::TurnEnd,
                ]),
        );
    // Both hosts reach the relay over UDP, the path a network change moves.
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .unwrap();
    let options = testnet::NetOptions {
        edge: Some(std::sync::Arc::new(move |_, edge| {
            edge.cloud.relay_tcp = Some(closed);
        })),
        ..testnet::NetOptions::default()
    };
    let mut net = Net::start_with(topology, options).await.unwrap();
    net.trust("desk", "laptop").await.unwrap();
    net.trust("laptop", "desk").await.unwrap();
    let (desk, laptop) = (host_id(&net, "desk"), host_id(&net, "laptop"));
    let worker = net.agent("worker").unwrap().id;
    assert_eq!(carrier(&net, "desk"), Some(RelayCarrier::Quic));
    until_via(&net, "laptop", "desk", LiveRoute::Relay).await;
    let mut session = open_session(&edge(&net, "laptop"), desk, worker)
        .await
        .unwrap();
    net.open_gate("one").unwrap();
    until_item(&mut session, "before the move").await;
    let downs = |host: &str, peer: uuid::Uuid| {
        net.link_log(host)
            .unwrap()
            .into_iter()
            .filter(|line| line.peer == peer && line.event == LinkTransition::Down)
            .count()
    };
    let before = (downs("laptop", desk), downs("desk", laptop));

    println!("{:?}", net.change_network("desk").unwrap());
    net.open_gate("two").unwrap();
    until_item(&mut session, "after the move").await;
    peer_inventory_hosts(&edge(&net, "laptop"), desk)
        .await
        .expect("the laptop's calls reach the desk at its new address");
    assert_eq!(
        (downs("laptop", desk), downs("desk", laptop)),
        before,
        "the link between them never went down"
    );
    let mut links = net.relay().unwrap().links("ada").await;
    links.sort();
    let mut expected = vec![(desk, 1), (laptop, 1)];
    expected.sort();
    assert_eq!(links, expected, "one relay link per host");
    assert_eq!(edge(&net, "laptop").via(desk).await, LiveRoute::Relay);
    println!("the desk moved under an open session; the relay carried it on");

    still_relayed(&net, "laptop", "desk");
    net.shutdown().await.unwrap();
}
