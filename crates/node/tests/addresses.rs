//! What hosts tell each other of where they can be reached: each lists its
//! interface addresses with its listener's port, never loopback or
//! link-local, and the peer keeps the list with where each address came
//! from beside the address it last answered at, across a restart.

mod support;

use std::net::IpAddr;

use node::LanOptions;
use support::synthetic::quiet_launch;
use support::*;

/// Edge options listening on every address of both families, with the
/// machine's interfaces stood in for by `interfaces`.
fn listing(interfaces: &[&str]) -> node::EdgeOptions {
    node::EdgeOptions {
        lan: Some(LanOptions {
            bind: "[::]:0".parse().unwrap(),
            socket: None,
        }),
        interfaces: Some(
            interfaces
                .iter()
                .map(|ip| ip.parse::<IpAddr>().unwrap())
                .collect(),
        ),
        ..Default::default()
    }
}

/// The addresses `install` keeps for `peer`, as `address source` pairs,
/// and the address it last answered at.
fn remembered(install: &Install, peer: node::HostId) -> (Vec<String>, Vec<String>) {
    let row = install.trusted_host(peer);
    let column = |column: fn(&store::TrustedHost) -> &str| -> serde_json::Value {
        row.as_ref()
            .and_then(|row| serde_json::from_str(column(row)).ok())
            .unwrap_or_default()
    };
    let (listed, direct) = (column(|row| &row.listed), column(|row| &row.direct));
    let listed = listed
        .as_array()
        .map(|listed| {
            listed
                .iter()
                .map(|address| format!("{} {}", address["addr"], address["source"]))
                .map(|pair| pair.replace('"', ""))
                .collect()
        })
        .unwrap_or_default();
    let answered = direct
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|addr| addr.as_str().map(str::to_owned))
        .collect();
    (listed, answered)
}

#[tokio::test(flavor = "multi_thread")]
async fn each_host_keeps_the_addresses_its_peer_lists_with_their_sources_and_where_it_answered() {
    let desk = Install::new();
    let mut options = desk.options("boot-1", quiet_launch());
    options.edge = listing(&[
        "192.0.2.7",
        "127.0.0.1",
        "169.254.9.9",
        "fe80::7",
        "2001:db8::7",
    ]);
    let desk_daemon = node::start(options, None).await.unwrap();
    let desk_edge = runtime(&desk_daemon, &desk).edge().unwrap();
    let desk_port = desk_edge.lan_addr().unwrap().port();
    let laptop = Install::new();
    let mut options = laptop.options("boot-1", quiet_launch());
    options.edge = listing(&["198.51.100.4", "::1"]);
    let laptop_daemon = node::start(options, None).await.unwrap();
    let laptop_edge = runtime(&laptop_daemon, &laptop).edge().unwrap();
    let laptop_port = laptop_edge.lan_addr().unwrap().port();
    let (desk_id, laptop_id) = (desk_edge.host_id(), laptop_edge.host_id());
    desk_edge.trust(&laptop_edge).await.unwrap();
    laptop_edge.trust(&desk_edge).await.unwrap();
    let answered_at = format!("127.0.0.1:{desk_port}");
    laptop_edge.dial(desk_id, answered_at.parse().unwrap());
    assert!(laptop_edge.wait_for_route(desk_id, PATIENCE).await);

    // The desk's Hello answer lists its two reachable interfaces, the
    // laptop's Hello its one; both arrived over a link of the hosts' own.
    let desk_listed = vec![
        format!("192.0.2.7:{desk_port} listed"),
        format!("[2001:db8::7]:{desk_port} listed"),
    ];
    let wanted = (desk_listed.clone(), vec![answered_at.clone()]);
    until("the laptop to keep what the desk listed", || {
        let kept = remembered(&laptop, desk_id);
        let wanted = wanted.clone();
        async move { (kept == wanted).then_some(()).ok_or(format!("{kept:?}")) }
    })
    .await
    .unwrap();
    let laptop_listed = vec![format!("198.51.100.4:{laptop_port} listed")];
    until("the desk to keep what the laptop listed", || {
        let (kept, _) = remembered(&desk, laptop_id);
        let wanted = laptop_listed.clone();
        async move { (kept == wanted).then_some(()).ok_or(format!("{kept:?}")) }
    })
    .await
    .unwrap();

    // A restart reads both back, and writes neither away.
    drop(laptop_edge);
    laptop_daemon.shutdown().await.unwrap();
    let mut options = laptop.options("boot-1", quiet_launch());
    options.edge = listing(&["198.51.100.4"]);
    let laptop_daemon = node::start(options, None).await.unwrap();
    let laptop_edge = runtime(&laptop_daemon, &laptop).edge().unwrap();
    laptop_edge.dial(desk_id, answered_at.parse().unwrap());
    assert!(laptop_edge.wait_for_route(desk_id, PATIENCE).await);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(
        remembered(&laptop, desk_id),
        (desk_listed, vec![answered_at])
    );
    drop(laptop_edge);
    laptop_daemon.shutdown().await.unwrap();
    drop(desk_edge);
    desk_daemon.shutdown().await.unwrap();
}
