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
    /// The port the desk listened on before its restart, held so nothing
    /// answers there.
    old_port: Option<std::net::UdpSocket>,
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
            old_port: None,
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
    ///
    /// The laptop's first flight also goes to the old port. An endpoint
    /// still draining its old links there refuses new connections, which
    /// ends the whole dial, so the port is taken once the old endpoint has
    /// let it go.
    async fn restart_desk_elsewhere(&mut self) {
        let desk = self.desk_edge().host_id();
        let old = self.desk_edge().lan_addr().unwrap();
        self.desk_daemon.take().unwrap().shutdown().await.unwrap();
        let old_port = until("the old desk to let its port go", || async move {
            std::net::UdpSocket::bind(old).map_err(|error| error.to_string())
        })
        .await
        .unwrap();
        self.old_port = Some(old_port);
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

/// What the process logs of links and of the streams peers open, for a
/// test that counts what a host saw. Installed once for the whole binary:
/// a subscriber set per test would race the others' callsites.
fn captured() -> &'static Arc<std::sync::Mutex<Vec<u8>>> {
    static LOG: std::sync::OnceLock<Arc<std::sync::Mutex<Vec<u8>>>> = std::sync::OnceLock::new();
    LOG.get_or_init(|| {
        use tracing_subscriber::layer::{Layer as _, SubscriberExt as _};
        use tracing_subscriber::util::SubscriberInitExt as _;

        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = log.clone();
        let filter = tracing_subscriber::filter::Targets::new()
            .with_target(node::link_log::TARGET, tracing::Level::INFO)
            .with_target("node::driver", tracing::Level::DEBUG);
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(move || Captured(writer.clone()))
                    .with_filter(filter),
            )
            .try_init()
            .expect("the only subscriber this binary installs");
        log
    })
}

struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The lines logged since `from` bytes into the log that hold every one
/// of `words`.
fn lines_with(from: usize, words: &[&str]) -> Vec<String> {
    let log = captured().lock().unwrap();
    String::from_utf8_lossy(&log[from.min(log.len())..])
        .lines()
        .filter(|line| words.iter().all(|word| line.contains(word)))
        .map(str::to_owned)
        .collect()
}

/// Edge options listening on every address of both families, so a host
/// is reached at 127.0.0.1 and at ::1 alike, and dialling the hosts the
/// profile remembers.
fn dual_stack() -> node::EdgeOptions {
    node::EdgeOptions {
        lan: Some(LanOptions {
            bind: "[::]:0".parse().unwrap(),
            socket: None,
        }),
        dial: true,
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn first_flight_on_every_path_reaches_the_host_once() {
    use support::synthetic::{SyntheticAgent, snapshot};

    captured();
    let desk = Install::new();
    let mut agent = SyntheticAgent::new(&desk, "coder", 1 << 20);
    agent.register_offline(&desk);
    agent.append(&snapshot(wire::Phase::Idle, &[], 1_000));
    agent.go_live();
    let mut options = desk.options("boot-1", quiet_launch());
    options.edge = dual_stack();
    let desk_daemon = node::start(options, None).await.unwrap();
    let desk_edge = runtime(&desk_daemon, &desk).edge().unwrap();
    let port = desk_edge.lan_addr().unwrap().port();
    let laptop = Install::new();
    let mut options = laptop.options("boot-1", quiet_launch());
    options.edge = dual_stack();
    let laptop_daemon = node::start(options, None).await.unwrap();
    let laptop_runtime = runtime(&laptop_daemon, &laptop);
    let laptop_edge = laptop_runtime.edge().unwrap();
    let (desk_id, laptop_id) = (desk_edge.host_id(), laptop_edge.host_id());
    desk_edge.trust(&laptop_edge).await.unwrap();
    laptop_edge.trust(&desk_edge).await.unwrap();
    laptop_edge.dial(desk_id, format!("127.0.0.1:{port}").parse().unwrap());
    assert!(laptop_edge.wait_for_route(desk_id, PATIENCE).await);
    until("the laptop to replicate the desk's agent", || async {
        match laptop_runtime.owner(agent.id.as_bytes()).await {
            Ok(node::Owner::Peer(_)) => Ok(()),
            Ok(owner) => Err(format!("{owner:?}")),
            Err(error) => Err(error.to_string()),
        }
    })
    .await
    .unwrap();
    let trust_file = laptop.profile_dir().join("trust.json");
    until("the laptop to remember where the desk answered", || {
        let trust = std::fs::read_to_string(&trust_file).unwrap_or_default();
        async move {
            if trust.contains(&format!("127.0.0.1:{port}")) {
                Ok(())
            } else {
                Err(trust)
            }
        }
    })
    .await
    .unwrap();
    drop((laptop_edge, laptop_runtime));
    laptop_daemon.shutdown().await.unwrap();

    // The desk lists both its addresses, as its Hello says them: the
    // laptop's first flight goes to each.
    let mut trust: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&trust_file).unwrap()).unwrap();
    let reachabilities = trust[desk_id.to_string()]["reachabilities"]
        .as_array_mut()
        .expect("the desk's entry lists its reachabilities");
    let direct = reachabilities
        .iter_mut()
        .find(|reachability| reachability["type"] == "direct")
        .expect("the desk is remembered at an address");
    direct["addrs"] = serde_json::json!([format!("127.0.0.1:{port}"), format!("[::1]:{port}")]);
    std::fs::write(&trust_file, serde_json::to_vec_pretty(&trust).unwrap()).unwrap();

    let mark = captured().lock().unwrap().len();
    let mut options = laptop.options("boot-1", quiet_launch());
    options.edge = dual_stack();
    let laptop_daemon = node::start(options, None).await.unwrap();
    let laptop_host = format!("host={laptop_id}");
    let desk_host = format!("host={desk_id}");
    let caught_up = until("the laptop's first flight to catch up", || {
        let lines = lines_with(mark, &[&laptop_host, "event=caught_up"]);
        async move { lines.into_iter().next().ok_or("not yet") }
    })
    .await
    .unwrap();
    assert!(caught_up.contains("early_data=accepted"), "{caught_up}");
    assert!(
        !caught_up.contains("first_packet_to_catch_up_ms=none"),
        "{caught_up}"
    );
    // Long enough for a second link or a second subscription to show.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let laptop_peer = format!("peer={laptop_id}");
    let hellos = lines_with(mark, &[&desk_host, &laptop_peer, "event=up"]);
    assert_eq!(hellos.len(), 1, "{hellos:#?}");
    for stream in ["stream=\"inventory\"", "stream=\"session\""] {
        let opened = lines_with(
            mark,
            &[&desk_host, &laptop_peer, "a peer opened a stream", stream],
        );
        assert_eq!(opened.len(), 1, "{stream}: {opened:#?}");
    }
    agent.die().await;
    laptop_daemon.shutdown().await.unwrap();
    desk_daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_first_flight_with_no_ticket_to_an_address_that_never_answers_gives_up_in_time() {
    let desk = Install::new();
    let mut options = desk.options("boot-1", quiet_launch());
    options.edge = lan(None);
    let desk_daemon = node::start(options, None).await.unwrap();
    let desk_edge = runtime(&desk_daemon, &desk).edge().unwrap();
    let laptop = Install::new();
    let mut options = laptop.options("boot-1", quiet_launch());
    options.edge = lan(None);
    let laptop_daemon = node::start(options, None).await.unwrap();
    let laptop_edge = runtime(&laptop_daemon, &laptop).edge().unwrap();
    let desk_id = desk_edge.host_id();
    desk_edge.trust(&laptop_edge).await.unwrap();
    laptop_edge.trust(&desk_edge).await.unwrap();
    drop(laptop_edge);
    laptop_daemon.shutdown().await.unwrap();

    // The laptop remembers the desk at an address that takes packets and
    // answers none, and has never linked to the desk, so holds no ticket
    // for it: its dial there goes through a whole handshake.
    let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let trust_file = laptop.profile_dir().join("trust.json");
    let mut trust: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&trust_file).unwrap()).unwrap();
    trust[desk_id.to_string()]["reachabilities"] = serde_json::json!([{
        "type": "direct",
        "addrs": [silent.local_addr().unwrap().to_string()],
    }]);
    std::fs::write(&trust_file, serde_json::to_vec_pretty(&trust).unwrap()).unwrap();
    let mut options = laptop.options("boot-1", quiet_launch());
    options.edge = lan(None);
    let laptop_daemon = node::start(options, None).await.unwrap();
    let laptop_runtime = runtime(&laptop_daemon, &laptop);
    let laptop_edge = laptop_runtime.edge().unwrap();

    // Well inside the QUIC idle timeout the dial would otherwise wait out.
    let started = std::time::Instant::now();
    let failed = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        laptop_edge.open_resumed(desk_id, vec![EarlyCall::SubscribeInventory]),
    )
    .await
    .expect("the dial gives up within its handshake deadline");
    assert!(failed.is_err(), "nothing answers at the remembered address");
    assert!(started.elapsed() >= std::time::Duration::from_secs(1));

    // The host's dial is free again: a dial to where the desk is goes
    // ahead.
    laptop_edge.dial(desk_id, desk_edge.lan_addr().unwrap());
    assert!(
        laptop_edge.wait_for_route(desk_id, PATIENCE).await,
        "the laptop links to the desk at its real address"
    );
    drop((laptop_edge, laptop_runtime, desk_edge));
    laptop_daemon.shutdown().await.unwrap();
    desk_daemon.shutdown().await.unwrap();
    drop(silent);
}
