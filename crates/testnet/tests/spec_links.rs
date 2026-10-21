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

    let mut told = [
        ("desk", laptop, edge(&net, "desk").forward_refusals()),
        ("laptop", desk, edge(&net, "laptop").forward_refusals()),
    ];
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
    // it repeats a refusal, though both kept trying to reach the other.
    tokio::time::sleep(std::time::Duration::from_millis(
        node::forwarder::REFUSAL_REPEAT_MS - 1_000,
    ))
    .await;
    for (host, peer, refusals) in &mut told {
        let mut heard = Vec::new();
        while let Ok(refusal) = refusals.try_recv() {
            heard.push(refusal);
        }
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
