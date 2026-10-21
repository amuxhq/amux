//! Links between hosts: one QUIC connection per pair of hosts, whatever
//! path carries its packets, from a socket of both address families.
//!
//! Every host is a production daemon in this process. Direct links run
//! over loopback QUIC and discovery is the net's scripted bus.

#![cfg(unix)]

mod support;

use node::harness::LiveRoute;
use provider_fakes::script::Step;
use support::*;
use testnet::{AgentDecl, HostDecl, Net, PATIENCE, Topology, observe};

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
        .agent(AgentDecl::new("worker", "desk").steps(vec![
            Step::Text {
                chunks: vec![said.to_owned()],
            },
            Step::TurnEnd,
        ]));
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
    println!("desk and laptop linked directly over IPv6");

    net.current("laptop", "worker").await.unwrap();
    let mut chat = net.observe("laptop", "worker", 10).await.unwrap();
    chat.observe_until(observe::caught_up, PATIENCE)
        .await
        .unwrap();
    net.send("worker", "go").await.unwrap();
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
    println!("the laptop streamed the desk's session live: {said:?}");

    net.shutdown().await.unwrap();
}
