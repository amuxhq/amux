//! A link resuming with a session ticket: what its peer wrote early is
//! served only once the core takes the identity the ticket names, and a
//! ticket naming a key no longer pinned is refused as revoked.

mod support;

use std::sync::Arc;

use node::link_log::EarlyData;
use node::{Daemon, EarlyAnswer, EarlyCall, Edge, LanOptions, ProfileRuntime};
use support::synthetic::quiet_launch;
use support::*;

/// Edge options listening on loopback: on `socket` when given, else on
/// any port.
fn lan(socket: Option<Arc<std::net::UdpSocket>>) -> node::EdgeOptions {
    node::EdgeOptions {
        lan: Some(LanOptions {
            bind: "127.0.0.1:0".parse().unwrap(),
            socket,
        }),
        ..Default::default()
    }
}

/// A desk and a laptop that trust each other, the laptop linked to the
/// desk's LAN listener, so the desk has given it a ticket.
struct Linked {
    desk: Install,
    _laptop: Install,
    desk_daemon: Option<Daemon>,
    /// The port a restarted desk listens on, held for as long as it runs.
    desk_socket: Option<Arc<std::net::UdpSocket>>,
    laptop_daemon: Daemon,
    laptop_edge: Arc<Edge>,
    laptop_runtime: Arc<ProfileRuntime>,
}

impl Linked {
    async fn new() -> Self {
        let desk = Install::new();
        let mut options = desk.options("boot-1", quiet_launch());
        options.edge = lan(None);
        let desk_daemon = node::start(options, None).await.unwrap();
        let laptop = Install::new();
        let mut options = laptop.options("boot-1", quiet_launch());
        options.edge = lan(None);
        let laptop_daemon = node::start(options, None).await.unwrap();
        let laptop_runtime = runtime(&laptop_daemon, &laptop);
        let laptop_edge = laptop_runtime.edge().unwrap();
        let linked = Self {
            desk,
            _laptop: laptop,
            desk_daemon: Some(desk_daemon),
            desk_socket: None,
            laptop_daemon,
            laptop_edge,
            laptop_runtime,
        };
        let desk_edge = linked.desk_edge();
        desk_edge.trust(&linked.laptop_edge).await.unwrap();
        linked.laptop_edge.trust(&desk_edge).await.unwrap();
        linked
            .laptop_edge
            .dial(desk_edge.host_id(), desk_edge.lan_addr().unwrap());
        assert!(
            linked
                .laptop_edge
                .wait_for_route(desk_edge.host_id(), PATIENCE)
                .await,
            "the laptop links to the desk"
        );
        // A resumed dial goes where the last link worked, which the
        // laptop writes down once the link is up.
        let desk_ref = wire::HostRef {
            of: Some(wire::host_ref::Of::HostId(
                desk_edge.host_id().as_bytes().to_vec(),
            )),
        };
        until("the laptop to remember where the desk answered", || {
            let reachabilities = linked
                .laptop_edge
                .peer_entry(desk_ref.clone())
                .map(|entry| entry.reachabilities.len());
            async move {
                match reachabilities {
                    Ok(0) => Err("none yet".to_owned()),
                    Ok(_) => Ok(()),
                    Err(status) => Err(status.to_string()),
                }
            }
        })
        .await
        .unwrap();
        linked
    }

    fn desk_edge(&self) -> Arc<Edge> {
        runtime(self.desk_daemon.as_ref().unwrap(), &self.desk)
            .edge()
            .unwrap()
    }

    /// Restarts the desk listening on another port, and hands the laptop
    /// that port as discovery would. The laptop's own redials after the
    /// link it lost try the port it last worked at first, so the next link
    /// to the desk is the one the test dials; the desk keeps the tickets it
    /// issued across the restart.
    async fn restart_desk_elsewhere(&mut self) {
        let desk = self.desk_edge().host_id();
        self.desk_daemon.take().unwrap().shutdown().await.unwrap();
        let socket = Arc::new(std::net::UdpSocket::bind("127.0.0.1:0").unwrap());
        let addr = socket.local_addr().unwrap();
        let mut options = self.desk.options("boot-1", quiet_launch());
        options.edge = lan(Some(socket.clone()));
        self.desk_daemon = Some(node::start(options, None).await.unwrap());
        self.desk_socket = Some(socket);
        self.laptop_edge
            .hand_over_discovered(vec![node::discovery::Advertisement {
                host_id: desk,
                name: self.desk_edge().host_name().to_owned(),
                version: node::PROTOCOL_VERSION,
                addrs: vec![addr],
                scope: self.laptop_edge.discovery_scope().to_owned(),
            }]);
    }

    async fn stop(self) {
        drop((self.laptop_edge, self.laptop_runtime));
        self.desk_daemon.unwrap().shutdown().await.unwrap();
        self.laptop_daemon.shutdown().await.unwrap();
        drop(self.desk_socket);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn early_calls_on_a_resumed_link_are_answered_once_the_core_takes_it() {
    let mut linked = Linked::new().await;
    let desk = linked.desk_edge().host_id();
    linked.restart_desk_elsewhere().await;

    let resumed = linked
        .laptop_edge
        .open_resumed(desk, vec![EarlyCall::SubscribeInventory])
        .await
        .expect("the desk takes the laptop's ticket");
    assert_eq!(resumed.early_data, EarlyData::Accepted);
    let [reply] = <[_; 1]>::try_from(resumed.replies).unwrap_or_else(|_| panic!("one reply"));
    let EarlyAnswer::Inventory(mut inventory) = reply.await.expect("the early call is answered")
    else {
        panic!("the inventory call answers with the inventory");
    };
    tokio::time::timeout(PATIENCE, inventory.message())
        .await
        .expect("the inventory streams")
        .expect("the stream holds")
        .expect("the desk lists its inventory");
    linked.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_resumed_with_a_ticket_for_a_key_no_longer_pinned_is_refused_as_revoked() {
    let linked = Linked::new().await;
    let desk = linked.desk_edge().host_id();
    let laptop = linked.laptop_edge.host_id();

    // The desk pins another key for the laptop, as pairing a reinstalled
    // laptop under the same name would; the laptop still holds the ticket
    // its old key was given.
    linked
        .desk_edge()
        .trust_ssh_peer(wire::TrustSshPeerRequest {
            profile_id: Vec::new(),
            peer: Some(wire::PairingIdentity {
                host_id: laptop.as_bytes().to_vec(),
                pubkey: vec![7; 32],
                name: "laptop".to_owned(),
                ..Default::default()
            }),
            ssh_target: None,
        })
        .await
        .unwrap();

    let refused = linked
        .laptop_edge
        .open_resumed(desk, vec![EarlyCall::SubscribeInventory])
        .await
        .err()
        .expect("the desk refuses the ticket");
    assert_eq!(refused.to_string(), node::TRUST_REVOKED);
    until("the laptop to read the desk as revoked", || {
        let revoked = linked
            .laptop_runtime
            .host_entry_of(desk)
            .and_then(|entry| entry.revoked);
        async move {
            match revoked {
                Some(true) => Ok(()),
                other => Err(format!("{other:?}")),
            }
        }
    })
    .await
    .unwrap();
    linked.stop().await;
}
