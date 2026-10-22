//! Links between hosts: one QUIC connection per pair of hosts, whatever
//! path carries its packets, from a socket of both address families. Each
//! story reaches one agent's session over one route, streams it live and
//! reads from the link log which path carried the link.
//!
//! Every host is a production daemon in this process. Direct links run
//! over loopback QUIC, discovery is the net's scripted bus, and SSH is the
//! net's stand-in on the PATH, running the far host's own `amux relay`.

#![cfg(unix)]

mod support;

use node::RelayCarrier;
use node::harness::LiveRoute;
use node::link_log::{LinkTransition, PathKind};
use provider_fakes::script::Step;
use support::*;
use testnet::{AgentDecl, HostDecl, Net, PATIENCE, Topology, observe, until};
use wire::begin_pair_request;

/// Two hosts bound to IPv6 alone pair, link over IPv6, and one
/// streams the other's session live.
#[tokio::test(flavor = "multi_thread")]
async fn ipv6_only_hosts_link_and_stream_a_session() {
    let ipv6_only = |name: &str| HostDecl {
        ipv6_only: true,
        ..lan_host(name, "v6")
    };
    let said = "said over IPv6";
    let topology = Topology::new()
        .host_decl(ipv6_only("desk"))
        .host_decl(ipv6_only("laptop"))
        .agent(worker_saying(said));
    let net = Net::start(topology).await.unwrap();
    for host in ["desk", "laptop"] {
        let addr = edge(&net, host).lan_addr().unwrap();
        assert!(
            addr.is_ipv6() && addr.ip().is_loopback(),
            "{host} at {addr}"
        );
        assert!(
            std::net::UdpSocket::bind(("127.0.0.1", addr.port())).is_ok(),
            "{host} holds no IPv4 socket at its port"
        );
        println!("{host} listens at {addr} only");
    }

    // Pairing dials the desk's listener at the IPv6 address its window
    // names, and the link comes up on the address pairing used.
    let peer = pair(&net, "laptop", "desk").await;
    println!("the laptop paired with the desk: {:?}", peer.reachabilities);
    until_via(&net, "laptop", "desk", LiveRoute::Direct).await;
    let desk = host_id(&net, "desk");
    let logged = net.link_log("laptop").unwrap();
    assert!(
        logged.iter().any(|line| line.peer == desk
            && line.event == LinkTransition::Up
            && line.path == PathKind::Lan),
        "the laptop's link log names its link to the desk: {logged:?}"
    );
    println!("desk and laptop linked directly over IPv6; the laptop's link log: {logged:?}");

    streams_live(&net, &["laptop"], "worker", said).await;
    net.shutdown().await.unwrap();
}

/// Two hosts on one network find each other, pair, link directly and one
/// streams the other's session live; both link logs read the link as on
/// the same network.
#[tokio::test(flavor = "multi_thread")]
async fn route_lan_reaches_a_session_on_the_same_network() {
    let said = "said on the same network";
    let topology = Topology::new()
        .host_decl(lan_host("desk", "home"))
        .host_decl(lan_host("laptop", "home"))
        .agent(worker_saying(said));
    let net = Net::start(topology).await.unwrap();

    pair(&net, "laptop", "desk").await;
    until_via(&net, "laptop", "desk", LiveRoute::Direct).await;
    carried_by(&net, "laptop", "desk", PathKind::Lan).await;
    carried_by(&net, "desk", "laptop", PathKind::Lan).await;

    streams_live(&net, &["laptop"], "worker", said).await;
    net.shutdown().await.unwrap();
}

/// Two hosts with no network between them but SSH: the laptop's link to
/// the desk runs through `ssh <target> amux relay` into the desk's link
/// socket, where the pinned handshake admits only a key the desk trusts.
/// Before the desk trusts the laptop the link is refused; once it does,
/// the link comes up over SSH at both ends and the laptop streams the
/// desk's session live.
#[tokio::test(flavor = "multi_thread")]
async fn route_ssh_reaches_a_session_through_the_far_hosts_link_socket() {
    let said = "said over SSH";
    let host = |name: &str| HostDecl {
        name: name.to_owned(),
        ..HostDecl::default()
    };
    let topology = Topology::new()
        .host_decl(host("desk"))
        .host_decl(host("laptop"))
        .agent(worker_saying(said));
    let mut net = Net::start(topology).await.unwrap();
    let (desk, laptop) = (host_id(&net, "desk"), host_id(&net, "laptop"));
    let target = net.ssh_target("desk").unwrap();

    trust_over_ssh(&net, "laptop", "desk").await;
    let refused = until("the laptop's SSH dial to fail", || async {
        edge(&net, "laptop")
            .last_dial_error(desk)
            .await
            .ok_or("no dial error yet")
    })
    .await
    .unwrap();
    assert_ne!(edge(&net, "laptop").via(desk).await, LiveRoute::Ssh);
    assert!(
        net.link_log("desk")
            .unwrap()
            .iter()
            .all(|line| line.peer != laptop),
        "the desk took no link from a key it does not trust"
    );
    println!("the desk's link socket refused the laptop's key it does not trust: {refused}");

    net.trust("desk", "laptop").await.unwrap();
    trust_over_ssh(&net, "laptop", "desk").await;
    until_via(&net, "laptop", "desk", LiveRoute::Ssh).await;
    carried_by(&net, "laptop", "desk", PathKind::Ssh).await;
    carried_by(&net, "desk", "laptop", PathKind::Ssh).await;
    println!("the laptop reached the desk over the stand-in at {target}");

    streams_live(&net, &["laptop"], "worker", said).await;
    net.shutdown().await.unwrap();
}

/// Two hosts on one paid account with no network between them but the
/// relay, reached over UDP alone: each links to the relay over UDP, and
/// their own link, pinned at pairing, runs end to end through the relay's
/// forwarding. The laptop streams the desk's session live over it.
#[tokio::test(flavor = "multi_thread")]
async fn route_relay_udp_reaches_a_session_through_the_relays_forwarding() {
    let said = "said through the relay over UDP";
    let topology = Topology::new()
        .relay(&["ada"])
        .host_decl(on_the_relay("desk"))
        .host_decl(on_the_relay("laptop"))
        .agent(worker_saying(said));
    let mut net = Net::start_with(topology, relay_tcp_unreachable(&["desk", "laptop"]))
        .await
        .unwrap();
    assert_eq!(carrier(&net, "desk"), Some(RelayCarrier::Quic));
    assert_eq!(carrier(&net, "laptop"), Some(RelayCarrier::Quic));
    net.trust("desk", "laptop").await.unwrap();
    net.trust("laptop", "desk").await.unwrap();

    until_via(&net, "laptop", "desk", LiveRoute::Relay).await;
    carried_by(&net, "laptop", "desk", PathKind::RelayUdp).await;
    carried_by(&net, "desk", "laptop", PathKind::RelayUdp).await;
    one_relay_link_each(&net, &["desk", "laptop"]).await;

    streams_live(&net, &["laptop"], "worker", said).await;
    net.shutdown().await.unwrap();
}

/// Signing out ends what the relay carries for this machine: the link to a
/// host the relay alone reaches closes with the relay link, so the laptop
/// reads the desk offline and itself signed out, the two facts a client
/// words as "this machine is signed out". Signing in again reaches the
/// desk's session as before.
#[tokio::test(flavor = "multi_thread")]
async fn route_relay_signing_out_closes_the_links_the_relay_carried_until_signing_in() {
    let said = "said through the relay before signing out";
    let topology = Topology::new()
        .relay(&["ada"])
        .host_decl(on_the_relay("desk"))
        .host_decl(on_the_relay("laptop"))
        .agent(worker_saying(said));
    let mut net = Net::start(topology).await.unwrap();
    let (desk, laptop) = (host_id(&net, "desk"), host_id(&net, "laptop"));
    net.trust("desk", "laptop").await.unwrap();
    net.trust("laptop", "desk").await.unwrap();
    let mut fleet = net.observe_inventory("laptop").await.unwrap();

    until_via(&net, "laptop", "desk", LiveRoute::Relay).await;
    row_until(
        &mut fleet,
        desk,
        "the laptop to read the desk online",
        |row| row.presence() == wire::Presence::Online,
    )
    .await;
    streams_live(&net, &["laptop"], "worker", said).await;

    door(&net, "laptop")
        .await
        .logout_profile(wire::ProfileRequest {
            profile_id: profile(&net, "laptop"),
        })
        .await
        .unwrap();
    row_until(
        &mut fleet,
        laptop,
        "the laptop to read itself signed out",
        |row| row.account_signed_in == Some(false),
    )
    .await;
    row_until(
        &mut fleet,
        desk,
        "the laptop to read the desk offline",
        |row| row.presence() == wire::Presence::Offline,
    )
    .await;

    // Signed in again, the laptop links to the desk through the relay anew
    // and the session is current again.
    net.sign_in("laptop", "ada").await.unwrap();
    until_via(&net, "laptop", "desk", LiveRoute::Relay).await;
    row_until(
        &mut fleet,
        desk,
        "the laptop to read the desk online again",
        |row| row.presence() == wire::Presence::Online,
    )
    .await;
    net.current("laptop", "worker").await.unwrap();
    net.observe("laptop", "worker", 10)
        .await
        .unwrap()
        .observe_until(observe::caught_up, PATIENCE)
        .await
        .unwrap();
    net.shutdown().await.unwrap();
}

/// The same through the relay where the laptop's network eats UDP: its
/// link to the relay is TLS on TCP, the desk's (reaching the relay over
/// UDP alone) is UDP, and the relay forwards the laptop's link to the desk
/// from one to the other. Each end reads the link as carried on its own
/// way to the relay.
#[tokio::test(flavor = "multi_thread")]
async fn route_relay_tcp_reaches_a_session_from_a_network_that_blocks_udp() {
    let said = "said through the relay over TCP";
    let topology = Topology::new()
        .relay(&["ada"])
        .host("desk")
        .host("laptop")
        .agent(worker_saying(said));
    let mut net = Net::start_with(topology, relay_tcp_unreachable(&["desk"]))
        .await
        .unwrap();
    net.block_udp("laptop", true).unwrap();
    net.sign_in("desk", "ada").await.unwrap();
    net.sign_in("laptop", "ada").await.unwrap();
    assert_eq!(carrier(&net, "desk"), Some(RelayCarrier::Quic));
    assert_eq!(carrier(&net, "laptop"), Some(RelayCarrier::Tcp));
    println!("the desk is on the relay over UDP; the laptop, UDP blocked, over TCP");
    net.trust("desk", "laptop").await.unwrap();
    net.trust("laptop", "desk").await.unwrap();

    until_via(&net, "laptop", "desk", LiveRoute::Relay).await;
    carried_by(&net, "laptop", "desk", PathKind::RelayTcp).await;
    carried_by(&net, "desk", "laptop", PathKind::RelayUdp).await;
    one_relay_link_each(&net, &["desk", "laptop"]).await;

    streams_live(&net, &["laptop"], "worker", said).await;
    net.shutdown().await.unwrap();
}

/// Three hosts and no relay. The laptop and the desk share a network; the
/// phone is on another and has paired with the laptop by address, so the
/// phone and the desk, which trust each other, have no way to each other
/// but the laptop. The laptop forwards for the two hosts it holds pinned
/// links to, and the phone's own link to the desk runs end to end through
/// it, read at both ends as through a daemon.
#[tokio::test(flavor = "multi_thread")]
async fn route_via_daemon_reaches_a_session_through_a_paired_host() {
    let said = "said through another daemon";
    let topology = Topology::new()
        .host_decl(lan_host("desk", "home"))
        .host_decl(lan_host("laptop", "home"))
        .host_decl(lan_host("phone", "cafe"))
        .agent(worker_saying(said));
    let mut net = Net::start(topology).await.unwrap();
    pair(&net, "laptop", "desk").await;
    pair(&net, "phone", "laptop").await;
    until_via(&net, "laptop", "desk", LiveRoute::Direct).await;
    until_via(&net, "phone", "laptop", LiveRoute::Direct).await;
    net.trust("desk", "phone").await.unwrap();
    net.trust("phone", "desk").await.unwrap();

    carried_by(&net, "phone", "desk", PathKind::ViaDaemon).await;
    carried_by(&net, "desk", "phone", PathKind::ViaDaemon).await;
    let desk = host_id(&net, "desk");
    assert!(
        net.link_log("phone")
            .unwrap()
            .iter()
            .all(|line| line.peer != desk || line.path == PathKind::ViaDaemon),
        "the phone reached the desk only through the laptop"
    );

    streams_live(&net, &["phone"], "worker", said).await;
    net.shutdown().await.unwrap();
}

/// A free account: the relay lists the account's hosts, so the phone,
/// on the relay alone, sees the desk and reads it away, refused for the
/// plan; but the relay forwards nothing, so no link to the desk comes up
/// and the desk's session cannot be opened from the phone. The hosts the
/// desk reaches without the relay still reach it: the laptop on the same
/// network, and a box with nothing but SSH to the desk.
#[tokio::test(flavor = "multi_thread")]
async fn entitlement_free_sees_presence_and_carries_nothing_while_lan_and_ssh_reach() {
    let said = "said on a free account";
    let mut topology = Topology::new()
        .relay(&["ada"])
        .host_decl(HostDecl {
            account: Some("ada".to_owned()),
            ..lan_host("desk", "home")
        })
        .host_decl(HostDecl {
            account: Some("ada".to_owned()),
            ..lan_host("laptop", "home")
        })
        .host_decl(on_the_relay("phone"))
        .host_decl(HostDecl {
            name: "box".to_owned(),
            ..HostDecl::default()
        })
        .agent(worker_saying(said));
    topology.relay.as_mut().unwrap().accounts[0].tier = testnet::TierDecl::Free;
    let mut net = Net::start(topology).await.unwrap();
    let desk = host_id(&net, "desk");
    assert_eq!(tier(&net, "phone"), Some(node::Tier::Free));
    let mut refusals = edge(&net, "phone").forward_refusals();
    let mut fleet = net.observe_inventory("phone").await.unwrap();

    net.trust("desk", "phone").await.unwrap();
    net.trust("phone", "desk").await.unwrap();
    pair(&net, "laptop", "desk").await;
    net.trust("desk", "box").await.unwrap();
    trust_over_ssh(&net, "box", "desk").await;

    // The phone sees the desk, through the relay's listing, and reads it
    // away: the relay refused to forward to it for the plan.
    row_until(&mut fleet, desk, "the phone to read the desk away", |row| {
        row.presence() == wire::Presence::Away
    })
    .await;
    let (refused, reason) = tokio::time::timeout(PATIENCE, refusals.recv())
        .await
        .expect("the relay refuses the phone's link to the desk")
        .unwrap();
    assert_eq!(
        (refused, reason),
        (desk, wire::pb::ForwardRefusal::PaymentRequired)
    );
    let worker = net.agent("worker").unwrap().id;
    let call = open_session(&edge(&net, "phone"), desk, worker)
        .await
        .expect_err("nothing reaches the desk through the relay on a free account");
    assert!(
        net.link_log("phone")
            .unwrap()
            .iter()
            .all(|line| line.peer != desk),
        "no link between the phone and the desk came up: {:?}",
        net.link_log("phone").unwrap()
    );
    println!(
        "free: the phone reads the desk away, refused for the plan; \
         its link log holds no link to the desk; opening the session: {:?}",
        call.code()
    );

    // Off the relay the desk is reached as ever.
    carried_by(&net, "laptop", "desk", PathKind::Lan).await;
    until_via(&net, "box", "desk", LiveRoute::Ssh).await;
    carried_by(&net, "box", "desk", PathKind::Ssh).await;
    streams_live(&net, &["laptop", "box"], "worker", said).await;
    net.shutdown().await.unwrap();
}

/// A paid account lapses while the laptop streams the desk's session
/// through the relay: the relay stops forwarding between the two at once
/// and tells each end, once, that the plan no longer carries the other.
/// Each closes its link to the other, and each reads the other away.
#[tokio::test(flavor = "multi_thread")]
async fn entitlement_lapse_stops_forwarding_and_both_hosts_read_away_told_once() {
    let said = "said before the lapse";
    let topology = Topology::new()
        .relay(&["ada"])
        .host_decl(on_the_relay("desk"))
        .host_decl(on_the_relay("laptop"))
        .agent(worker_saying(said));
    let mut net = Net::start_with(topology, relay_tcp_unreachable(&["desk", "laptop"]))
        .await
        .unwrap();
    let (desk, laptop) = (host_id(&net, "desk"), host_id(&net, "laptop"));
    net.trust("laptop", "desk").await.unwrap();
    net.trust("desk", "laptop").await.unwrap();
    until_via(&net, "laptop", "desk", LiveRoute::Relay).await;
    carried_by(&net, "laptop", "desk", PathKind::RelayUdp).await;
    carried_by(&net, "desk", "laptop", PathKind::RelayUdp).await;
    streams_live(&net, &["laptop"], "worker", said).await;
    let mut chat = net.observe("laptop", "worker", 10).await.unwrap();
    chat.observe_until(observe::caught_up, PATIENCE)
        .await
        .unwrap();

    // Each refusal an end hears, with when it heard it.
    let told = [
        ("desk", laptop, edge(&net, "desk").forward_refusals()),
        ("laptop", desk, edge(&net, "laptop").forward_refusals()),
    ]
    .map(|(host, peer, mut refusals)| {
        let heard = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recording = heard.clone();
        tokio::spawn(async move {
            while let Ok(refusal) = refusals.recv().await {
                recording
                    .lock()
                    .unwrap()
                    .push((tokio::time::Instant::now(), refusal));
            }
        });
        (host, peer, heard)
    });
    let mut fleets = [
        ("desk", laptop, net.observe_inventory("desk").await.unwrap()),
        (
            "laptop",
            desk,
            net.observe_inventory("laptop").await.unwrap(),
        ),
    ];
    net.set_tier("ada", testnet::TierDecl::Free).await.unwrap();

    for (host, peer, fleet) in &mut fleets {
        row_until(
            fleet,
            *peer,
            &format!("{host} to read its peer away"),
            |row| row.presence() == wire::Presence::Away,
        )
        .await;
    }
    for (host, peer) in [("desk", "laptop"), ("laptop", "desk")] {
        let id = host_id(&net, peer);
        until(&format!("{host}'s link to {peer} to go down"), || async {
            let log = net.link_log(host).unwrap();
            log.iter()
                .rev()
                .find(|line| line.peer == id)
                .is_some_and(|line| line.event == LinkTransition::Down)
                .then_some(())
                .ok_or_else(|| format!("{:?}", log.last()))
        })
        .await
        .unwrap();
    }
    println!("lapsed: the desk and the laptop each read the other away, their link down");

    // Each end was told once, within the interval a forwarder waits before
    // it repeats a refusal, counted from the first refusal it heard, though
    // both kept trying to reach the other.
    for (host, peer, heard) in &told {
        let first = heard
            .lock()
            .unwrap()
            .first()
            .map(|(at, _)| *at)
            .expect("each end heard a refusal before its link went");
        tokio::time::sleep_until(
            first + std::time::Duration::from_millis(node::forwarder::REFUSAL_REPEAT_MS - 1_000),
        )
        .await;
        let heard: Vec<_> = heard
            .lock()
            .unwrap()
            .iter()
            .map(|(_, refusal)| *refusal)
            .collect();
        assert_eq!(
            heard,
            vec![(*peer, wire::pb::ForwardRefusal::PaymentRequired)],
            "{host} is told once that the plan no longer carries its peer"
        );
        println!("{host} was told once: {heard:?}");
    }
    let reopened = net.observe("laptop", "worker", 10).await;
    assert!(
        reopened.is_err() || {
            let mut chat = reopened.unwrap();
            chat.observe_until(observe::caught_up, std::time::Duration::from_secs(2))
                .await
                .is_err()
        },
        "the laptop reaches the desk's session no more"
    );
    drop(chat);
    net.shutdown().await.unwrap();
}

/// Two hosts on a paid account with no network between them pair through
/// the relay: the PIN exchange is the same pinned-handshake pairing as on
/// the same network, carried by the relay's forwarding, and the link that
/// follows runs through the relay as well.
#[tokio::test(flavor = "multi_thread")]
async fn pairing_relay_paid_pairs_through_the_relay_and_reaches_a_session() {
    let said = "said after pairing through the relay";
    let topology = Topology::new()
        .relay(&["ada"])
        .host_decl(on_the_relay("desk"))
        .host_decl(on_the_relay("tablet"))
        .agent(worker_saying(said));
    let net = Net::start_with(topology, relay_tcp_unreachable(&["desk", "tablet"]))
        .await
        .unwrap();
    let desk = host_id(&net, "desk");
    let started = start_pairing(&net, "desk", pin_window()).await.unwrap();
    let pending = begin_pair(
        &net,
        "tablet",
        Some(desk),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        Vec::new(),
    )
    .await
    .expect("pairing through the relay on a paid account");
    assert_eq!(pending.via, wire::RouteKind::Relay as i32);
    confirm_pair(&net, "tablet", pending.token).await.unwrap();
    println!("the tablet paired with the desk through the relay");

    until_via(&net, "tablet", "desk", LiveRoute::Relay).await;
    carried_by(&net, "tablet", "desk", PathKind::RelayUdp).await;
    carried_by(&net, "desk", "tablet", PathKind::RelayUdp).await;
    streams_live(&net, &["tablet"], "worker", said).await;
    net.shutdown().await.unwrap();
}

/// The same on a free account: the relay forwards no pairing either, so
/// the tablet's attempt is refused for the plan before it reaches the
/// desk, the desk's window stays open for a pairing that can succeed, and
/// no link between the two comes up.
#[tokio::test(flavor = "multi_thread")]
async fn pairing_relay_free_refused_leaves_the_window_open() {
    let mut topology = Topology::new()
        .relay(&["ada"])
        .host_decl(on_the_relay("desk"))
        .host_decl(on_the_relay("tablet"))
        .agent(worker_saying("never said"));
    topology.relay.as_mut().unwrap().accounts[0].tier = testnet::TierDecl::Free;
    let net = Net::start(topology).await.unwrap();
    let (desk, tablet) = (host_id(&net, "desk"), host_id(&net, "tablet"));
    let mut refusals = edge(&net, "tablet").forward_refusals();
    let started = start_pairing(&net, "desk", pin_window()).await.unwrap();
    let refused = begin_pair(
        &net,
        "tablet",
        Some(desk),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        Vec::new(),
    )
    .await
    .expect_err("no pairing through the relay on a free account");
    let (to, reason) = tokio::time::timeout(PATIENCE, refusals.recv())
        .await
        .expect("the relay refuses the tablet's pairing")
        .unwrap();
    assert_eq!(
        (to, reason),
        (desk, wire::pb::ForwardRefusal::PaymentRequired)
    );
    assert!(edge(&net, "desk").pairing_active(), "the window stays open");
    let worker = net.agent("worker").unwrap().id;
    open_session(&edge(&net, "tablet"), desk, worker)
        .await
        .expect_err("the tablet reaches nothing of the desk's");
    for (host, peer) in [("tablet", desk), ("desk", tablet)] {
        assert!(
            net.link_log(host)
                .unwrap()
                .iter()
                .all(|line| line.peer != peer),
            "{host}'s link log holds no link to its would-be peer"
        );
    }
    println!(
        "free: pairing through the relay refused ({:?}: {}), told {reason:?}; \
         the desk's window is still open",
        refused.code(),
        refused.message()
    );
    net.shutdown().await.unwrap();
}

/// Net options under which `hosts` cannot reach the relay over TCP, so
/// their links to it are UDP: a host tries TCP a moment after UDP, and on
/// a busy machine TCP can win the race.
fn relay_tcp_unreachable(hosts: &'static [&'static str]) -> testnet::NetOptions {
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .unwrap();
    testnet::NetOptions {
        edge: Some(std::sync::Arc::new(move |host, edge| {
            if hosts.contains(&host) {
                edge.cloud.relay_tcp = Some(closed);
            }
        })),
        ..testnet::NetOptions::default()
    }
}

/// Net options under which `hosts` reach the relay over UDP alone, as
/// [`relay_tcp_unreachable`] has it, and each launch sends a first flight
/// to every host the profile remembers, as a daemon and the phone always
/// do: the net otherwise has only hosts with a listener dial.
fn launching_through_the_relay(hosts: &'static [&'static str]) -> testnet::NetOptions {
    let unreachable = relay_tcp_unreachable(hosts);
    let closed = unreachable.edge.expect("the relay's TCP closed");
    testnet::NetOptions {
        edge: Some(std::sync::Arc::new(move |host, edge| {
            closed(host, edge);
            if hosts.contains(&host) {
                edge.dial = true;
            }
        })),
        ..unreachable
    }
}

/// A host on the relay alone, signed in to the account `ada`: no listener,
/// no discovery.
fn on_the_relay(name: &str) -> HostDecl {
    HostDecl {
        name: name.to_owned(),
        account: Some("ada".to_owned()),
        ..HostDecl::default()
    }
}

/// Waits until the relay holds one link from each of `hosts` and no more.
async fn one_relay_link_each(net: &Net, hosts: &[&str]) {
    let mut expected: Vec<_> = hosts.iter().map(|host| (host_id(net, host), 1)).collect();
    expected.sort();
    let mut links = net.relay().unwrap().links("ada").await;
    links.sort();
    assert_eq!(links, expected, "one relay link per host");
}

/// Waits until the inventory `fleet` watches describes `host` as `check`
/// wants.
async fn row_until(
    fleet: &mut testnet::InventoryObserver,
    host: uuid::Uuid,
    what: &str,
    check: impl Fn(&wire::HostEntry) -> bool,
) {
    let found = |events: &[wire::InventoryEvent]| {
        observe::inventory_hosts(events)
            .into_iter()
            .find(|row| row.host_id == host.as_bytes())
    };
    fleet
        .observe_until(
            |events| found(events).is_some_and(|row| check(&row)),
            PATIENCE,
        )
        .await
        .unwrap_or_else(|stuck| panic!("{what}: {stuck}"));
}

/// `near` trusts `far` as an SSH pairing commits it, through its front
/// door, with the target and profile it reaches `far` by.
async fn trust_over_ssh(net: &Net, near: &str, far: &str) {
    door(net, near)
        .await
        .trust_ssh_peer(wire::TrustSshPeerRequest {
            profile_id: profile(net, near),
            peer: Some(wire::PairingIdentity {
                host_id: host_id(net, far).as_bytes().to_vec(),
                pubkey: edge(net, far).public_key().to_vec(),
                name: far.to_owned(),
                expires_at_ms: 0,
            }),
            ssh_target: Some(wire::SshTarget {
                target: net.ssh_target(far).unwrap(),
                profile_id: profile(net, far),
            }),
        })
        .await
        .unwrap_or_else(|status| panic!("{near} trusts {far} over SSH: {status:?}"));
}

/// An agent on the desk whose one turn says `said`.
fn worker_saying(said: &str) -> AgentDecl {
    AgentDecl::new("worker", "desk").steps(vec![
        Step::Text {
            chunks: vec![said.to_owned()],
        },
        Step::TurnEnd,
    ])
}

/// Waits until `host`'s link log says `path` carries its link to `peer`.
async fn carried_by(net: &Net, host: &str, peer: &str, path: PathKind) {
    until(&format!("{host}'s link to {peer} on {path}"), || async {
        match net.carrying_path(host, peer) {
            Ok(Some(carrying)) if carrying == path => Ok(()),
            other => Err(format!("{other:?}")),
        }
    })
    .await
    .unwrap();
    println!(
        "{host}'s link log: its link to {peer} is carried on {path}: {:?}",
        net.link_log(host).unwrap().last()
    );
}

/// Each of `hosts` opens `agent`'s session and reads it to its CaughtUp,
/// and each sees `said` arrive live once the agent is sent a prompt.
async fn streams_live(net: &Net, hosts: &[&str], agent: &str, said: &str) {
    let mut chats = Vec::new();
    for host in hosts {
        net.current(host, agent).await.unwrap();
        let mut chat = net.observe(host, agent, 10).await.unwrap();
        chat.observe_until(observe::caught_up, PATIENCE)
            .await
            .unwrap();
        chats.push((host, chat));
    }
    net.send(agent, "go").await.unwrap();
    for (host, chat) in &mut chats {
        chat.observe_until(|events| says(events, said), PATIENCE)
            .await
            .unwrap();
        println!("{host} streamed {agent}'s session live: {said:?}");
    }
}

/// Whether `events` hold an item saying `said`.
fn says(events: &[wire::SessionEvent], said: &str) -> bool {
    events.iter().any(
        |event| matches!(&event.of, Some(wire::session_event::Of::Item(item)) if item.text == said),
    )
}

/// How long the gate in front of the laptop holds each datagram, each way:
/// one round trip between the laptop and the desk is twice this.
const ONE_WAY: std::time::Duration = std::time::Duration::from_millis(150);
/// How long a story keeps a path cut for an outage rather than a blip.
const LONG_OUTAGE: std::time::Duration = std::time::Duration::from_secs(20);

/// A desk and a laptop on one network whose every datagram between them
/// crosses a gate holding it [`ONE_WAY`], paired at the gate's address and
/// linked there, which the laptop now remembers as where the desk last
/// answered.
async fn linked_through_a_slow_network() -> Net {
    let mut net = Net::start_with(
        Topology::new()
            .host_decl(undiscovered("desk"))
            .host_decl(undiscovered("laptop"))
            .agent(worker_saying("said after the launch")),
        testnet::NetOptions {
            gated_lan: true,
            ..testnet::NetOptions::default()
        },
    )
    .await
    .unwrap();
    net.set_net_faults(
        "laptop",
        testnet::Faults {
            delay: ONE_WAY,
            ..testnet::Faults::default()
        },
    )
    .unwrap();
    let started = start_pairing(&net, "desk", pin_window()).await.unwrap();
    let gate = net.pair_gate("laptop", "desk").unwrap();
    let pending = begin_pair(
        &net,
        "laptop",
        Some(host_id(&net, "desk")),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        vec![gate.to_string()],
    )
    .await
    .expect("the laptop pairs with the desk through the gate");
    confirm_pair(&net, "laptop", pending.token).await.unwrap();
    until_via(&net, "laptop", "desk", LiveRoute::Direct).await;
    remembers_where_the_desk_answered(&net).await;
    net
}

/// A host with a listener on the network `home` that discovery does not
/// announce: its peers reach it only where they remember it answering.
fn undiscovered(name: &str) -> HostDecl {
    HostDecl {
        discovery: false,
        ..lan_host(name, "home")
    }
}

/// Waits until the laptop has written down an address the desk answered
/// at, which its next launch's first flight goes to.
async fn remembers_where_the_desk_answered(net: &Net) {
    let desk = wire::HostRef {
        of: Some(wire::host_ref::Of::HostId(
            host_id(net, "desk").as_bytes().to_vec(),
        )),
    };
    until("the laptop to remember where the desk answered", || {
        let remembered = edge(net, "laptop")
            .peer_entry(desk.clone())
            .map(|entry| entry.reachabilities.len());
        async move {
            match remembered {
                Ok(0) => Err("nowhere yet".to_owned()),
                Ok(_) => Ok(()),
                Err(status) => Err(status.to_string()),
            }
        }
    })
    .await
    .unwrap();
}

/// Stops `host`'s daemon and starts it again, as quitting and opening the
/// app does, and returns how long `host`'s link log was before the launch.
async fn relaunch(net: &mut Net, host: &str) -> usize {
    net.stop_daemon(host).await.unwrap();
    let mark = net.link_log(host).unwrap().len();
    net.restart_daemon(host).await.unwrap();
    mark
}

/// Waits for the line in `host`'s link log, past `mark`, saying its link
/// to `peer` went through `event`, and returns it.
async fn logged(
    net: &Net,
    host: &str,
    peer: &str,
    event: LinkTransition,
    mark: usize,
) -> node::link_log::LinkEvent {
    let peer_id = host_id(net, peer);
    let line = until(
        &format!("{host}'s link log to say {event:?} for {peer}"),
        || async {
            let log = net.link_log(host).unwrap();
            log.iter()
                .skip(mark)
                .find(|line| line.peer == peer_id && line.event == event)
                .copied()
                .ok_or_else(|| format!("{:?}", &log[mark.min(log.len())..]))
        },
    )
    .await
    .unwrap();
    println!("{host}'s link log: {line:?}");
    line
}

/// The first launch after two hosts come to trust each other has no ticket
/// to resume with: a full handshake, no early data, and the catch-up at
/// least two round trips after the first packet.
#[tokio::test(flavor = "multi_thread")]
async fn first_launch_full_handshake_costs_two_round_trips() {
    let mut net = linked_through_a_slow_network().await;
    // The laptop forgets every ticket the desk gave it, as on a first
    // launch ever.
    net.stop_daemon("laptop").await.unwrap();
    let info = net.host("laptop").unwrap().clone();
    std::fs::remove_dir_all(
        node::profile_dir(&info.data_dir, info.profile).join(node::CLIENT_TICKETS_DIR),
    )
    .unwrap();
    let mark = net.link_log("laptop").unwrap().len();
    net.restart_daemon("laptop").await.unwrap();

    let up = logged(&net, "laptop", "desk", LinkTransition::Up, mark).await;
    assert!(!up.resumed, "nothing to resume: {up:?}");
    assert_eq!(up.early_data, node::link_log::EarlyData::None);
    let caught_up = logged(&net, "laptop", "desk", LinkTransition::CaughtUp, mark).await;
    let ms = caught_up
        .first_packet_to_catch_up_ms
        .expect("the catch-up timed");
    println!(
        "full handshake: caught up {ms} ms after the first packet, one round trip {} ms",
        2 * ONE_WAY.as_millis()
    );
    assert!(
        u128::from(ms) >= 4 * ONE_WAY.as_millis(),
        "two round trips at least: {ms} ms"
    );
    streams_live(&net, &["laptop"], "worker", "said after the launch").await;
    net.shutdown().await.unwrap();
}

/// A relaunch resumes the link with the ticket the desk gave the laptop:
/// its Hello and catch-up calls go as 0-RTT data the desk accepts, and with
/// the address-validation token its last process kept no Retry intervenes,
/// so the link log times the catch-up within one round trip of the link's
/// first packet.
#[tokio::test(flavor = "multi_thread")]
async fn resume_accepted_catches_up_within_one_round_trip() {
    let mut net = linked_through_a_slow_network().await;
    let mark = relaunch(&mut net, "laptop").await;

    let up = logged(&net, "laptop", "desk", LinkTransition::Up, mark).await;
    assert!(up.resumed, "{up:?}");
    assert_eq!(up.early_data, node::link_log::EarlyData::Accepted);
    let caught_up = logged(&net, "laptop", "desk", LinkTransition::CaughtUp, mark).await;
    let ms = caught_up
        .first_packet_to_catch_up_ms
        .expect("the catch-up timed");
    let round_trip = 2 * ONE_WAY.as_millis();
    println!("resumed: caught up {ms} ms after the first packet, one round trip {round_trip} ms");
    assert!(
        u128::from(ms) >= round_trip,
        "a round trip at least: {ms} ms"
    );
    // The laptop presents the address-validation token the desk gave its
    // last process, so no Retry costs a second round trip.
    assert!(
        u128::from(ms) < 2 * round_trip,
        "within one round trip of the first packet: {ms} ms"
    );
    streams_live(&net, &["laptop"], "worker", "said after the launch").await;
    net.shutdown().await.unwrap();
}

/// The desk loses the tickets it issued while it is down, as a host whose
/// store was wiped does: the laptop's relaunch offers its ticket and its
/// calls as 0-RTT data, the desk can resume nothing and refuses the early
/// data, and the laptop sends the same calls again once the handshake
/// completes. The session is reached as on any launch.
#[tokio::test(flavor = "multi_thread")]
async fn resume_rejected_resends_the_early_calls_after_the_handshake() {
    let mut net = linked_through_a_slow_network().await;
    net.stop_daemon("laptop").await.unwrap();
    net.stop_daemon("desk").await.unwrap();
    let desk = net.host("desk").unwrap().clone();
    std::fs::remove_dir_all(
        node::profile_dir(&desk.data_dir, desk.profile).join(node::TICKETS_DIR),
    )
    .unwrap();
    net.restart_daemon("desk").await.unwrap();
    // The laptop comes back on another port, so the desk, redialling it
    // since its own launch, cannot bring a link up before the laptop's
    // first flight does.
    let laptop = net.host("laptop").unwrap().clone();
    std::fs::remove_file(
        node::profile_dir(&laptop.data_dir, laptop.profile).join(node::LAN_PORT_FILE),
    )
    .unwrap();
    let mark = net.link_log("laptop").unwrap().len();
    net.restart_daemon("laptop").await.unwrap();

    let up = logged(&net, "laptop", "desk", LinkTransition::Up, mark).await;
    assert!(!up.resumed, "the desk holds no ticket to resume: {up:?}");
    assert_eq!(up.early_data, node::link_log::EarlyData::Rejected);
    let caught_up = logged(&net, "laptop", "desk", LinkTransition::CaughtUp, mark).await;
    assert_eq!(caught_up.early_data, node::link_log::EarlyData::Rejected);
    let ms = caught_up
        .first_packet_to_catch_up_ms
        .expect("the catch-up timed");
    println!(
        "rejected: the calls went again after the handshake, caught up {ms} ms after the first packet"
    );
    streams_live(&net, &["laptop"], "worker", "said after the launch").await;
    net.shutdown().await.unwrap();
}

/// The desk restarts while the laptop is away, keeping the tickets it
/// issued: the laptop's next launch still resumes with 0-RTT accepted.
#[tokio::test(flavor = "multi_thread")]
async fn resume_after_host_restart_still_resumes() {
    let mut net = linked_through_a_slow_network().await;
    net.stop_daemon("laptop").await.unwrap();
    net.stop_daemon("desk").await.unwrap();
    net.restart_daemon("desk").await.unwrap();
    // The laptop comes back on another port, so the desk, redialling it
    // since its own launch, cannot cross the laptop's first flight with a
    // link of its own: one that won the crossing would leave the flight's
    // early calls unanswered, and the launch would resume nothing.
    let laptop = net.host("laptop").unwrap().clone();
    std::fs::remove_file(
        node::profile_dir(&laptop.data_dir, laptop.profile).join(node::LAN_PORT_FILE),
    )
    .unwrap();
    let mark = net.link_log("laptop").unwrap().len();
    net.restart_daemon("laptop").await.unwrap();

    let caught_up = logged(&net, "laptop", "desk", LinkTransition::CaughtUp, mark).await;
    assert!(caught_up.resumed, "{caught_up:?}");
    assert_eq!(caught_up.early_data, node::link_log::EarlyData::Accepted);
    println!(
        "the desk restarted; the laptop's launch resumed and caught up {:?} ms after the first packet",
        caught_up.first_packet_to_catch_up_ms
    );
    streams_live(&net, &["laptop"], "worker", "said after the launch").await;
    net.shutdown().await.unwrap();
}

/// The desk revokes the laptop while the laptop is away. The laptop still
/// holds a ticket the desk gave it, and its next launch resumes with it:
/// the desk refuses the resumed link with the revocation before serving
/// anything it sent early, and the laptop reads the desk as no longer
/// trusting it. Neither link log holds a link between the two.
#[tokio::test(flavor = "multi_thread")]
async fn revoked_ticket_refused_with_the_revocation() {
    let mut net = linked_through_a_slow_network().await;
    let worker = net.agent("worker").unwrap().id;
    let desk = host_id(&net, "desk");
    net.stop_daemon("laptop").await.unwrap();
    let laptop = net.host("laptop").unwrap().clone();
    let tickets =
        node::profile_dir(&laptop.data_dir, laptop.profile).join(node::CLIENT_TICKETS_DIR);
    assert!(
        std::fs::read_dir(&tickets).unwrap().next().is_some(),
        "the laptop keeps a ticket from the desk"
    );
    net.untrust("desk", "laptop").await.unwrap();
    let (laptop_mark, desk_mark) = (
        net.link_log("laptop").unwrap().len(),
        net.link_log("desk").unwrap().len(),
    );
    net.restart_daemon("laptop").await.unwrap();

    let mut fleet = net.observe_inventory("laptop").await.unwrap();
    row_until(
        &mut fleet,
        desk,
        "the laptop to read the desk as no longer trusting it",
        |row| row.revoked == Some(true),
    )
    .await;
    let refused = open_session(&edge(&net, "laptop"), desk, worker)
        .await
        .expect_err("a revoked host opens nothing");
    for (host, peer, mark) in [
        ("laptop", "desk", laptop_mark),
        ("desk", "laptop", desk_mark),
    ] {
        let id = host_id(&net, peer);
        let log = net.link_log(host).unwrap();
        assert!(
            log[mark..].iter().all(|line| line.peer != id),
            "{host}'s link log holds no link to {peer}: {:?}",
            &log[mark..]
        );
    }
    println!(
        "revoked: the laptop's resumed launch was refused with the revocation; it reads the desk \
         as revoked and opens nothing there ({})",
        refused.message()
    );
    net.shutdown().await.unwrap();
}

/// A desk and a laptop on the paid account `ada` with no way to each other
/// but the relay, over UDP, linked through it; the laptop remembers the
/// desk as reached through the relay and holds a session ticket from it,
/// so its next launch can resume. On the driven clock, signed in once
/// `ttl` is the credentials' lifetime.
async fn linked_through_the_relay(ttl: std::time::Duration) -> Net {
    let net = Net::start_with(
        Topology::new()
            .relay(&["ada"])
            .host_decl(off_the_relay("desk"))
            .host_decl(off_the_relay("laptop"))
            .agent(worker_saying("said through the relay")),
        testnet::NetOptions {
            clock: testnet::ClockMode::Driven,
            ..launching_through_the_relay(&["desk", "laptop"])
        },
    )
    .await
    .unwrap();
    net.relay().unwrap().set_credential_ttl(ttl);
    net.sign_in("desk", "ada").await.unwrap();
    net.sign_in("laptop", "ada").await.unwrap();
    pair_through_the_relay(&net).await;
    carried_by(&net, "laptop", "desk", PathKind::RelayUdp).await;
    carried_by(&net, "desk", "laptop", PathKind::RelayUdp).await;
    holds_a_ticket(&net, "laptop", "desk").await;
    net
}

/// Waits until `host` keeps on disk a session ticket `peer` gave it. Only a
/// link `host` dialled brings one, and the link both ends read as carried
/// may be the one `peer` dialled while `host`'s own dial is still on its
/// way; a host stopped before its dial lands relaunches with nothing to
/// resume with.
async fn holds_a_ticket(net: &Net, host: &str, peer: &str) {
    let info = net.host(host).unwrap().clone();
    let tickets = node::profile_dir(&info.data_dir, info.profile)
        .join(node::CLIENT_TICKETS_DIR)
        .join(host_id(net, peer).to_string());
    until(&format!("{host} to keep a ticket from {peer}"), || async {
        let kept = tickets_under(&tickets);
        (kept > 0)
            .then_some(())
            .ok_or_else(|| format!("no ticket under {}", tickets.display()))
    })
    .await
    .unwrap();
}

/// How many tickets lie anywhere under `dir`.
fn tickets_under(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                tickets_under(&path)
            } else {
                usize::from(path.extension().is_some_and(|ext| ext == "ticket"))
            }
        })
        .sum()
}

/// The laptop pairs with the desk through the relay, and so remembers the
/// desk as reached there, and the two link through it.
async fn pair_through_the_relay(net: &Net) {
    let started = start_pairing(net, "desk", pin_window()).await.unwrap();
    let pending = begin_pair(
        net,
        "laptop",
        Some(host_id(net, "desk")),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        Vec::new(),
    )
    .await
    .expect("the laptop pairs with the desk through the relay");
    confirm_pair(net, "laptop", pending.token).await.unwrap();
    until_via(net, "laptop", "desk", LiveRoute::Relay).await;
}

/// A host with no listener and no discovery, signed in to nothing yet.
fn off_the_relay(name: &str) -> HostDecl {
    HostDecl {
        name: name.to_owned(),
        ..HostDecl::default()
    }
}

/// A week, as the account service issues relay credentials.
const WEEK: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);
/// How often a connected host refreshes its relay credential.
const HOUR: std::time::Duration = std::time::Duration::from_secs(3600);

/// A relaunch's first flight leaves through the relay before the relay has
/// admitted the laptop's own link to it: the relay holds those packets and
/// lets them through once it admits the laptop, and the link to the desk
/// resumes through the relay.
#[tokio::test(flavor = "multi_thread")]
async fn hold_buffer_released_on_admission_carries_the_first_flight() {
    let mut net = linked_through_the_relay(WEEK).await;
    let before = net.relay().unwrap().forward_counts();
    let mark = relaunch(&mut net, "laptop").await;

    // The desk redials the laptop as it comes back, so the first link up
    // may be the desk's; the first flight's own line is its catch-up.
    let caught_up = logged(&net, "laptop", "desk", LinkTransition::CaughtUp, mark).await;
    assert_eq!(caught_up.path, PathKind::RelayUdp);
    assert!(caught_up.resumed, "{caught_up:?}");
    let after = net.relay().unwrap().forward_counts();
    println!("the relay's forwarder before the relaunch {before:?}, after {after:?}");
    assert!(after.held > before.held, "the first flight was held");
    assert!(
        after.released > before.released,
        "and let through on admission"
    );
    streams_live(&net, &["laptop"], "worker", "said through the relay").await;
    net.shutdown().await.unwrap();
}

/// The laptop relaunches holding a credential the relay no longer takes,
/// and the account service refuses it a fresh one: the relay never admits
/// its link, so the first flight it held for the desk is dropped, nothing
/// is sent for it to the desk or back to the laptop, and no link between
/// the two comes up.
#[tokio::test(flavor = "multi_thread")]
async fn hold_buffer_bad_token_drops_the_held_packets_unanswered() {
    let mut net = Net::start_with(
        Topology::new()
            .relay(&["ada"])
            .host_decl(on_the_relay("desk"))
            .host_decl(off_the_relay("laptop"))
            .agent(worker_saying("never said")),
        testnet::NetOptions {
            clock: testnet::ClockMode::Driven,
            ..launching_through_the_relay(&["desk", "laptop"])
        },
    )
    .await
    .unwrap();
    let relay = net.relay().unwrap();
    // The relay's clock runs nine minutes ahead while the laptop signs in:
    // the credential it keeps lapses at the relay a minute later, while the
    // laptop still reads nine minutes left on it.
    relay.run_ahead(std::time::Duration::from_secs(9 * 60));
    net.sign_in("laptop", "ada").await.unwrap();
    net.relay().unwrap().run_ahead(std::time::Duration::ZERO);
    pair_through_the_relay(&net).await;

    net.stop_daemon("laptop").await.unwrap();
    net.advance(std::time::Duration::from_secs(2 * 60)).unwrap();
    net.relay().unwrap().revoke("ada");
    let before = net.relay().unwrap().forward_counts();
    let (laptop_mark, desk_mark) = (
        net.link_log("laptop").unwrap().len(),
        net.link_log("desk").unwrap().len(),
    );
    net.restart_daemon("laptop").await.unwrap();

    let relay = net.relay().unwrap();
    let after = until("the held first flight to be dropped", || async {
        let now = relay.forward_counts();
        (now.held > before.held && now.dropped - before.dropped == now.held - before.held)
            .then_some(now)
            .ok_or_else(|| format!("before {before:?}, now {now:?}"))
    })
    .await
    .unwrap();
    println!("the relay's forwarder before the relaunch {before:?}, after {after:?}");
    assert_eq!(
        after.released, before.released,
        "nothing held was let through"
    );
    assert_eq!(
        after.forwarded, before.forwarded,
        "nothing went to the desk for it"
    );
    // Long enough for a link the relay let through after all to show.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert_eq!(relay.forward_counts().forwarded, before.forwarded);
    for (host, peer, mark) in [
        ("laptop", "desk", laptop_mark),
        ("desk", "laptop", desk_mark),
    ] {
        let id = host_id(&net, peer);
        let log = net.link_log(host).unwrap();
        assert!(
            log[mark..].iter().all(|line| line.peer != id),
            "{host}'s link log holds no link to {peer}: {:?}",
            &log[mark..]
        );
    }
    println!(
        "the relay refused the laptop's credential ({:?} presented): its first flight was held and dropped, the link never up",
        relay.presented()
    );
    net.shutdown().await.unwrap();
}

/// A relaunch presents the relay credential the laptop kept and makes no
/// call to the account service, and its link to the desk comes up through
/// the relay as before.
#[tokio::test(flavor = "multi_thread")]
async fn token_relaunch_no_connect_reuses_the_kept_credential() {
    let mut net = linked_through_the_relay(WEEK).await;
    let connects = net.relay().unwrap().connects();
    println!("signed in: the account service was called {connects:?}");
    let mark = relaunch(&mut net, "laptop").await;

    let up = logged(&net, "laptop", "desk", LinkTransition::Up, mark).await;
    assert_eq!(up.path, PathKind::RelayUdp);
    assert_eq!(
        net.relay().unwrap().connects(),
        connects,
        "the relaunch made no call to the account service"
    );
    println!(
        "relaunched: presented {:?}, the account service's connects unchanged",
        net.relay().unwrap().presented()
    );
    streams_live(&net, &["laptop"], "worker", "said through the relay").await;
    net.shutdown().await.unwrap();
}

/// The account lapses at the account service while its hosts' week-long
/// credentials still say paid: the relay keeps forwarding until the hourly
/// refresh brings it credentials saying free, then stops and tells both
/// ends, and each link log says the link went down. The lapse is noticed
/// within the hour.
#[tokio::test(flavor = "multi_thread")]
async fn token_lapse_within_hour_is_noticed_at_the_hourly_refresh() {
    let net = linked_through_the_relay(WEEK).await;
    let (desk, laptop) = (host_id(&net, "desk"), host_id(&net, "laptop"));
    let mut told = [
        ("desk", laptop, edge(&net, "desk").forward_refusals()),
        ("laptop", desk, edge(&net, "laptop").forward_refusals()),
    ];
    let refresh_at = net.now_ms() + HOUR.as_millis() as i64;
    tokio::time::timeout(PATIENCE, net.clock().unwrap().armed(refresh_at))
        .await
        .expect("the hourly refresh is armed");

    net.relay()
        .unwrap()
        .set_tier("ada", node::harness::Tier::Free);
    streams_live(&net, &["laptop"], "worker", "said through the relay").await;
    for (host, _, refusals) in &mut told {
        assert!(
            refusals.try_recv().is_err(),
            "{host} is told nothing before the refresh"
        );
    }
    println!("lapsed at the account service: the relay still forwards on the credentials it holds");

    net.advance(HOUR).unwrap();
    for (host, peer, refusals) in &mut told {
        let refusal = tokio::time::timeout(PATIENCE, refusals.recv())
            .await
            .unwrap_or_else(|_| panic!("{host} is told at the refresh"))
            .unwrap();
        assert_eq!(refusal, (*peer, wire::pb::ForwardRefusal::PaymentRequired));
    }
    for (host, peer) in [("desk", "laptop"), ("laptop", "desk")] {
        let id = host_id(&net, peer);
        until(&format!("{host}'s link to {peer} to go down"), || async {
            let log = net.link_log(host).unwrap();
            log.iter()
                .rev()
                .find(|line| line.peer == id)
                .is_some_and(|line| line.event == LinkTransition::Down)
                .then_some(())
                .ok_or_else(|| format!("{:?}", log.last()))
        })
        .await
        .unwrap();
    }
    println!(
        "an hour on, the refresh presented {:?}: the relay stopped forwarding and both links went down",
        net.relay().unwrap().presented()
    );
    net.shutdown().await.unwrap();
}

/// Two hosts on one paid account, each behind its own imitated router of
/// `kind`, signed in to the relay with nothing else between them: their
/// link comes up through the relay's forwarding, and each learns where
/// the relay sees the other.
async fn behind_routers(kind: testnet::RouterKind, said: &str) -> Net {
    let topology = Topology::new()
        .relay(&["ada"])
        .host_decl(on_the_relay("desk"))
        .host_decl(on_the_relay("laptop"))
        .agent(worker_saying(said));
    let mut net = Net::start_with(topology, relay_tcp_unreachable(&["desk", "laptop"]))
        .await
        .unwrap();
    // A router is in place from the start of a run, so each host reaches
    // the relay from its router's public port alone.
    for host in ["desk", "laptop"] {
        net.set_router(host, kind).unwrap();
        net.stop_daemon(host).await.unwrap();
        net.restart_daemon(host).await.unwrap();
    }
    net.trust("desk", "laptop").await.unwrap();
    net.trust("laptop", "desk").await.unwrap();
    // The link comes up through the relay; behind ordinary routers it may
    // be punched by the time anyone looks, so read its Up line.
    for (host, peer) in [("laptop", "desk"), ("desk", "laptop")] {
        let id = host_id(&net, peer);
        until(
            &format!("{host}'s link to {peer} up on relay_udp"),
            || async {
                let log = net.link_log(host).unwrap();
                log.iter()
                    .find(|line| line.peer == id && line.event == LinkTransition::Up)
                    .map(|line| line.path)
                    .filter(|path| *path == PathKind::RelayUdp)
                    .map(|_| ())
                    .ok_or_else(|| format!("{log:?}"))
            },
        )
        .await
        .unwrap();
    }
    println!("behind {kind:?} routers: the link came up through the relay");
    net
}

/// Behind ordinary routers, which keep one public port for everything a
/// host sends and let in what comes from wherever it sent: each host
/// probes the address the relay saw the other at, both routers open, and
/// the link moves off the relay onto the punched path with the session
/// still streaming.
#[tokio::test(flavor = "multi_thread")]
async fn punch_ordinary_routers_move_the_link_onto_a_punched_path() {
    let said = "said over the punched path";
    let net = behind_routers(testnet::RouterKind::Ordinary, said).await;
    carried_by(&net, "laptop", "desk", PathKind::Punched).await;
    carried_by(&net, "desk", "laptop", PathKind::Punched).await;
    streams_live(&net, &["laptop"], "worker", said).await;
    assert_eq!(
        net.carrying_path("laptop", "desk").unwrap(),
        Some(PathKind::Punched),
        "the session streamed over the punched path"
    );
    net.shutdown().await.unwrap();
}

/// Behind strict routers, which open a public port per destination and let
/// in only what comes back from it: the address the relay saw a host at is
/// a port the host's router opened toward the relay alone, so each host's
/// probes to the other are dropped and the link stays on the relay, still
/// streaming.
#[tokio::test(flavor = "multi_thread")]
async fn punch_strict_routers_keep_the_link_on_the_relay() {
    let said = "said through the relay behind strict routers";
    let net = behind_routers(testnet::RouterKind::Strict, said).await;
    for (host, peer) in [("laptop", "desk"), ("desk", "laptop")] {
        until(
            &format!("{host} to learn where the relay saw {peer}"),
            || async {
                let observed = observed_addresses(&net, host, peer);
                (!observed.is_empty())
                    .then_some(())
                    .ok_or_else(|| "nothing observed yet".to_owned())
            },
        )
        .await
        .unwrap();
        println!(
            "{host} probes {peer} at the address the relay saw it: {:?}",
            observed_addresses(&net, host, peer)
        );
    }
    // Long enough for every punch to time out the times that drop it.
    tokio::time::sleep(punch_given_up()).await;
    for (host, peer) in [("laptop", "desk"), ("desk", "laptop")] {
        assert_eq!(
            net.carrying_path(host, peer).unwrap(),
            Some(PathKind::RelayUdp),
            "{host}'s link to {peer} stays on the relay"
        );
        assert!(
            !ever_carried_by(&net, host, peer, PathKind::Punched),
            "{host}'s link to {peer} was never punched"
        );
        println!("{host}'s link to {peer} stayed on relay_udp");
    }
    streams_live(&net, &["laptop"], "worker", said).await;
    net.shutdown().await.unwrap();
}

/// Two hosts on one free account on the same network: their link is on the
/// LAN, and neither ever learns where the relay sees the other, so neither
/// has anything to punch to and the link stays where it is.
#[tokio::test(flavor = "multi_thread")]
async fn punch_free_never_probes_and_the_link_stays_on_the_lan() {
    let said = "said over the LAN on a free account";
    let on_the_lan = |name: &str| HostDecl {
        account: Some("ada".to_owned()),
        ..lan_host(name, "home")
    };
    let topology = Topology::new()
        .relay(&["ada"])
        .host_decl(on_the_lan("desk"))
        .host_decl(on_the_lan("laptop"))
        .agent(worker_saying(said));
    let mut net = Net::start(topology).await.unwrap();
    net.set_tier("ada", testnet::TierDecl::Free).await.unwrap();
    pair(&net, "laptop", "desk").await;
    until_via(&net, "laptop", "desk", LiveRoute::Direct).await;
    carried_by(&net, "laptop", "desk", PathKind::Lan).await;
    carried_by(&net, "desk", "laptop", PathKind::Lan).await;
    streams_live(&net, &["laptop"], "worker", said).await;

    tokio::time::sleep(punch_given_up()).await;
    for (host, peer) in [("laptop", "desk"), ("desk", "laptop")] {
        assert_eq!(
            observed_addresses(&net, host, peer),
            Vec::<String>::new(),
            "{host} never learns where the relay sees {peer}: nothing to punch to"
        );
        assert_eq!(
            net.carrying_path(host, peer).unwrap(),
            Some(PathKind::Lan),
            "{host}'s link to {peer} stays on the LAN"
        );
        assert!(
            !ever_carried_by(&net, host, peer, PathKind::Punched),
            "{host}'s link to {peer} was never punched"
        );
        println!("{host}: no observed address for {peer}, link on lan throughout");
    }
    net.shutdown().await.unwrap();
}

/// A lapse takes the punched path away: once the account is free the link
/// leaves the path it was punched onto, and with no free path between two
/// routers it goes down rather than moving back to the relay, which no
/// longer forwards for the account.
#[tokio::test(flavor = "multi_thread")]
async fn punch_lapse_drops_the_punched_path() {
    let said = "said before the lapse";
    let mut net = behind_routers(testnet::RouterKind::Ordinary, said).await;
    carried_by(&net, "laptop", "desk", PathKind::Punched).await;
    carried_by(&net, "desk", "laptop", PathKind::Punched).await;
    // A host whose relay link is down as the account lapses learns it only
    // when that link comes back, presenting the paid credential it kept:
    // wait for both so both learn it now.
    for host in ["desk", "laptop"] {
        until(&format!("{host}'s relay link connected"), || async {
            let id = profile(&net, host);
            let seen = door(&net, host)
                .await
                .list_profiles(wire::Empty {})
                .await
                .map_err(|status| status.message().to_owned())?
                .into_inner()
                .profiles
                .into_iter()
                .find(|info| info.id == id)
                .map(|info| info.observed());
            (seen == Some(wire::Observed::Connected))
                .then_some(())
                .ok_or_else(|| format!("observed {seen:?}"))
        })
        .await
        .unwrap();
    }
    let lapsed = net.set_tier("ada", testnet::TierDecl::Free).await.unwrap();
    println!("{lapsed:?}");
    // A host that stops sending on the punched path with nowhere else to
    // go writes no new path for its link; the link log next says it went
    // down, at the link's idle timeout (30 s).
    for (host, peer) in [("laptop", "desk"), ("desk", "laptop")] {
        testnet::observe::until_within(
            &format!("{host}'s link to {peer} to leave the punched path"),
            std::time::Duration::from_secs(45),
            || async {
                match net.carrying_path(host, peer) {
                    Ok(Some(PathKind::Punched)) => Err("still punched".to_owned()),
                    other => {
                        println!("{host}'s link to {peer} after the lapse: {other:?}");
                        Ok(())
                    }
                }
            },
        )
        .await
        .unwrap();
    }
    tokio::time::sleep(punch_given_up()).await;
    for (host, peer) in [("laptop", "desk"), ("desk", "laptop")] {
        assert_ne!(
            net.carrying_path(host, peer).unwrap(),
            Some(PathKind::Punched),
            "{host} punches {peer} no more while free"
        );
    }
    net.shutdown().await.unwrap();
}

/// Long enough for the relay to tell each host where it sees it (it looks
/// every 2 s), for that to reach the peer, and for every probe on a path
/// that drops them to time out the three times that give it up (2 s each).
fn punch_given_up() -> std::time::Duration {
    std::time::Duration::from_secs(10)
}

/// Whether `host`'s link log ever named `path` carrying its link to `peer`.
fn ever_carried_by(net: &Net, host: &str, peer: &str, path: PathKind) -> bool {
    let peer = host_id(net, peer);
    net.link_log(host)
        .unwrap()
        .iter()
        .any(|line| line.peer == peer && line.path == path && line.event != LinkTransition::Down)
}

/// The addresses `host` keeps for `peer` that the relay saw `peer` at, as
/// its trust file lists them.
fn observed_addresses(net: &Net, host: &str, peer: &str) -> Vec<String> {
    let info = net.host(host).unwrap();
    let trust = node::profile_dir(&info.data_dir, info.profile).join("trust.json");
    let trust: serde_json::Value = std::fs::read(trust)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    trust[host_id(net, peer).to_string()]["listed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|address| address["source"] == "observed")
        .filter_map(|address| address["addr"].as_str().map(str::to_owned))
        .collect()
}

/// An agent on the desk that says one line after each named gate opens:
/// a session whose items the story releases one step at a time.
fn worker_gated(lines: &[(&str, &str)]) -> AgentDecl {
    let mut steps = Vec::new();
    for (gate, said) in lines {
        steps.push(Step::WaitFor {
            path: (*gate).into(),
        });
        steps.push(Step::Text {
            chunks: vec![(*said).to_owned()],
        });
    }
    steps.push(Step::TurnEnd);
    AgentDecl::new("worker", "desk")
        .prompt("Keep me posted.")
        .steps(steps)
}

/// Opens `gate` and waits until `chat` reads `said`.
async fn released(net: &Net, chat: &mut testnet::Observer, gate: &str, said: &str) {
    net.open_gate(gate).unwrap();
    chat.observe_until(|events| says(events, said), PATIENCE)
        .await
        .unwrap();
    println!("the laptop's open subscription read {said:?}");
}

/// The subscription `chat` held throughout read every line once, in order,
/// with one opening and no reset, lag or detach, and `host`'s link to
/// `peer` came up once and never went down: the link moved, it was not
/// replaced.
fn no_gap(net: &Net, chat: &mut testnet::Observer, host: &str, peer: &str, lines: &[&str]) {
    let marks = observe::marks(chat.events());
    let openings = marks
        .iter()
        .filter(|mark| matches!(mark, observe::Mark::Opening(_)))
        .count();
    assert_eq!(openings, 1, "one subscription throughout: {marks:?}");
    assert!(
        !marks.iter().any(|mark| matches!(
            mark,
            observe::Mark::Reset | observe::Mark::Lagged | observe::Mark::Detached
        )),
        "no reset, lag or detach: {marks:?}"
    );
    let read: Vec<&str> = chat
        .events()
        .iter()
        .filter_map(|event| match &event.of {
            Some(wire::session_event::Of::Item(item)) if lines.contains(&item.text.as_str()) => {
                Some(item.text.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(read, lines, "every line once, in order");
    let id = host_id(net, peer);
    let log: Vec<_> = net
        .link_log(host)
        .unwrap()
        .into_iter()
        .filter(|line| line.peer == id)
        .collect();
    let ups = log
        .iter()
        .filter(|line| line.event == LinkTransition::Up)
        .count();
    assert_eq!(ups, 1, "{host}'s link to {peer} came up once: {log:?}");
    assert!(
        !log.iter().any(|line| line.event == LinkTransition::Down),
        "{host}'s link to {peer} never went down: {log:?}"
    );
    println!(
        "{host}'s link to {peer}, by its link log: {:?}",
        log.iter()
            .map(|line| format!("{:?} {}", line.event, line.path))
            .collect::<Vec<_>>()
    );
}

/// Two paid hosts on one network, also signed in to the relay: the laptop
/// holds the desk's session open while the network between them is cut,
/// so the link moves onto the relay, and restored, so it moves back. The
/// cut lasts long enough for many probes on the LAN address to go
/// unanswered, and the link still comes back. The one subscription reads
/// every line said on each path with no reset, resubscribe or gap, and the
/// link never goes down.
#[tokio::test(flavor = "multi_thread")]
async fn path_change_lan_relay_lan_keeps_the_subscription_open() {
    let lines = [
        "said on the LAN",
        "said on the relay",
        "said back on the LAN",
    ];
    let mut net = Net::start_with(
        Topology::new()
            .relay(&["ada"])
            .host_decl(lan_host("desk", "home"))
            .host_decl(lan_host("laptop", "home"))
            .agent(worker_gated(&[
                ("lan", lines[0]),
                ("relay", lines[1]),
                ("back", lines[2]),
            ])),
        testnet::NetOptions {
            gated_lan: true,
            ..relay_tcp_unreachable(&["desk", "laptop"])
        },
    )
    .await
    .unwrap();
    pair(&net, "laptop", "desk").await;
    carried_by(&net, "laptop", "desk", PathKind::Lan).await;
    carried_by(&net, "desk", "laptop", PathKind::Lan).await;
    // Signed in once linked, so the relay's forwarding is a second path
    // to the link the LAN already carries rather than a link of its own.
    net.sign_in("desk", "ada").await.unwrap();
    net.sign_in("laptop", "ada").await.unwrap();
    net.current("laptop", "worker").await.unwrap();
    let mut chat = net.observe("laptop", "worker", 10).await.unwrap();
    chat.observe_until(observe::caught_up, PATIENCE)
        .await
        .unwrap();
    released(&net, &mut chat, "lan", lines[0]).await;

    net.sever_link("desk", "laptop").unwrap();
    let severed = std::time::Instant::now();
    carried_by(&net, "laptop", "desk", PathKind::RelayUdp).await;
    carried_by(&net, "desk", "laptop", PathKind::RelayUdp).await;
    released(&net, &mut chat, "relay", lines[1]).await;

    // A LAN probe goes every 5 s and times out after 2, so in 20 s four in
    // a row go unanswered: an outage, not a lost packet.
    tokio::time::sleep(LONG_OUTAGE.saturating_sub(severed.elapsed())).await;
    println!(
        "the LAN between them cut for {} ms",
        severed.elapsed().as_millis()
    );
    net.restore_link("desk", "laptop").await.unwrap();
    carried_by(&net, "laptop", "desk", PathKind::Lan).await;
    carried_by(&net, "desk", "laptop", PathKind::Lan).await;
    released(&net, &mut chat, "back", lines[2]).await;

    no_gap(&net, &mut chat, "laptop", "desk", &lines);
    no_gap(&net, &mut chat, "desk", "laptop", &lines);
    net.shutdown().await.unwrap();
}

/// A host's network changes under it mid-stream, as a phone's does leaving
/// Wi-Fi: its socket is bound afresh, its path machines are told, and the
/// link carries on from the new address with the subscription reading
/// every line, no gap, and the link never down.
#[tokio::test(flavor = "multi_thread")]
async fn path_change_phone_network_keeps_the_subscription_open() {
    let lines = ["said before the change", "said after the change"];
    let net = Net::start(
        Topology::new()
            .host_decl(lan_host("desk", "home"))
            .host_decl(lan_host("laptop", "home"))
            .agent(worker_gated(&[("before", lines[0]), ("after", lines[1])])),
    )
    .await
    .unwrap();
    pair(&net, "laptop", "desk").await;
    carried_by(&net, "laptop", "desk", PathKind::Lan).await;
    carried_by(&net, "desk", "laptop", PathKind::Lan).await;
    net.current("laptop", "worker").await.unwrap();
    let mut chat = net.observe("laptop", "worker", 10).await.unwrap();
    chat.observe_until(observe::caught_up, PATIENCE)
        .await
        .unwrap();
    released(&net, &mut chat, "before", lines[0]).await;

    let moved = net.change_network("laptop").unwrap();
    println!("{moved:?}");
    let rebinds = testnet::link_log::rebinds(host_id(&net, "laptop"));
    println!("the laptop's rebind lines: {rebinds:?}");
    released(&net, &mut chat, "after", lines[1]).await;
    carried_by(&net, "laptop", "desk", PathKind::Lan).await;
    carried_by(&net, "desk", "laptop", PathKind::Lan).await;

    no_gap(&net, &mut chat, "laptop", "desk", &lines);
    no_gap(&net, &mut chat, "desk", "laptop", &lines);
    net.shutdown().await.unwrap();
}
