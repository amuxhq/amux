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

use node::harness::LiveRoute;
use node::link_log::{LinkTransition, PathKind};
use provider_fakes::script::Step;
use support::*;
use testnet::{AgentDecl, HostDecl, Net, PATIENCE, Topology, observe, until};

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

    streams_live(&net, "laptop", "worker", said).await;
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

    streams_live(&net, "laptop", "worker", said).await;
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

    // The laptop trusts the desk as an SSH pairing commits it, through its
    // front door, with the target and profile it reaches the desk by.
    let trust_desk_over_ssh = async |net: &Net| {
        door(net, "laptop")
            .await
            .trust_ssh_peer(wire::TrustSshPeerRequest {
                profile_id: profile(net, "laptop"),
                peer: Some(wire::PairingIdentity {
                    host_id: desk.as_bytes().to_vec(),
                    pubkey: edge(net, "desk").public_key().to_vec(),
                    name: "desk".to_owned(),
                    expires_at_ms: 0,
                }),
                ssh_target: Some(wire::SshTarget {
                    target: target.clone(),
                    profile_id: profile(net, "desk"),
                }),
            })
            .await
            .expect("the laptop trusts the desk over SSH");
    };
    trust_desk_over_ssh(&net).await;
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
    trust_desk_over_ssh(&net).await;
    until_via(&net, "laptop", "desk", LiveRoute::Ssh).await;
    carried_by(&net, "laptop", "desk", PathKind::Ssh).await;
    carried_by(&net, "desk", "laptop", PathKind::Ssh).await;
    println!("the laptop reached the desk over the stand-in at {target}");

    streams_live(&net, "laptop", "worker", said).await;
    net.shutdown().await.unwrap();
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

/// `host` opens `agent`'s session, reads it to its CaughtUp, and sees
/// `said` arrive live once the agent is sent a prompt.
async fn streams_live(net: &Net, host: &str, agent: &str, said: &str) {
    net.current(host, agent).await.unwrap();
    let mut chat = net.observe(host, agent, 10).await.unwrap();
    chat.observe_until(observe::caught_up, PATIENCE)
        .await
        .unwrap();
    net.send(agent, "go").await.unwrap();
    chat.observe_until(
        |events| {
            events.iter().any(|event| {
                matches!(&event.of, Some(wire::session_event::Of::Item(item)) if item.text == said)
            })
        },
        PATIENCE,
    )
    .await
    .unwrap();
    println!("{host} streamed {agent}'s session live: {said:?}");
}
