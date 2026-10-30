//! The network edge between whole daemons: pairing through the front door
//! over the LAN listener on loopback, authenticated links in both
//! directions, a cloud link whose credential the policy clock refreshes, a
//! stranger refused, unpairing, and discovery scoped to one network.
//! Nothing here touches a real LAN, mDNS or relay: the listeners bind
//! 127.0.0.1, discovery is the net's scripted bus, and the relay is the
//! topology's, beside a stand-in cloud.

#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use hyper_util::rt::TokioIo;
use node::Edge;
use node::harness::{LiveRoute, Tier};
use testnet::{
    ClockMode, HostDecl, Net, NetOptions, PATIENCE, Relay, TierDecl, Topology, holds_for, until,
};
use tonic::transport::{Channel, Endpoint};
use uuid::Uuid;
use wire::cloud_standing::Standing;
use wire::installation_service_client::InstallationServiceClient;
use wire::profile_admin_service_client::ProfileAdminServiceClient;
use wire::{
    BeginPairRequest, BindProfileRequest, Empty, HostRef, InventoryEvent, StartPairingRequest,
    UnpairRequest, begin_pair_request, host_ref, inventory_event, pairing_window,
    start_pairing_request,
};

/// The discovery scope the paired hosts share.
const SCOPE: &str = "edge-test";

/// A host listening for direct links on loopback and on the net's
/// discovery bus.
fn lan_host(name: &str, scope: &str) -> HostDecl {
    HostDecl {
        name: name.to_owned(),
        lan: true,
        discovery: true,
        scope: Some(scope.to_owned()),
        ..HostDecl::default()
    }
}

fn edge(net: &Net, host: &str) -> Arc<Edge> {
    net.edge(host).expect("the edge runs")
}

fn id(net: &Net, host: &str) -> Vec<u8> {
    net.host(host).unwrap().profile.as_bytes().to_vec()
}

async fn door(net: &Net, host: &str) -> InstallationServiceClient<Channel> {
    InstallationServiceClient::new(channel(&net.host(host).unwrap().front_door).await)
}

/// The host's profile's administration, on its socket.
async fn admin(net: &Net, host: &str) -> ProfileAdminServiceClient<Channel> {
    let host = net.host(host).unwrap();
    ProfileAdminServiceClient::new(
        channel(&node::profile_dir(&host.data_dir, host.profile).join(node::PROFILE_SOCKET)).await,
    )
}

async fn info(net: &Net, host: &str) -> wire::Profile {
    let profile = id(net, host);
    door(net, host)
        .await
        .list_profiles(wire::ListProfilesRequest {})
        .await
        .unwrap()
        .into_inner()
        .profiles
        .into_iter()
        .find(|listed| listed.id == profile)
        .expect("the profile is listed")
}

async fn channel(path: &Path) -> Channel {
    let path = path.to_owned();
    Endpoint::from_static("http://amux.test")
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();
            async move {
                agent_dir::local_socket::connect(&path)
                    .await
                    .map(TokioIo::new)
            }
        }))
        .await
        .expect("the front door answers")
}

fn host_ref(host: Uuid) -> Option<HostRef> {
    Some(HostRef {
        of: Some(host_ref::Of::HostId(host.as_bytes().to_vec())),
    })
}

/// Waits for the outcome of `host`'s dial to `to`: the error it stored,
/// which a route coming up would clear. `before` is what was stored when
/// the dial was made, so an error from an earlier dial is not taken for
/// this one's. The dial has failed, and nothing about the link is still
/// in flight.
async fn dial_failed(net: &Net, host: &str, to: Uuid, before: Option<String>) -> String {
    until(&format!("{host}'s dial to fail"), || {
        let before = &before;
        async move {
            match edge(net, host).last_dial_error(to).await {
                Some(error) if Some(&error) != before.as_ref() => Ok(error),
                other => Err(format!("stored {other:?}, as before the dial")),
            }
        }
    })
    .await
    .unwrap()
}

/// Waits until a policy timer is armed for exactly `at_ms`: the step that
/// arms it is complete and it has not fired. Policy time stands still, so
/// the deadline is exact. A host's driver sleeps only until its soonest
/// timer, so one due after another is not seen here: this is for a
/// deadline counted from the step that arms it, which nothing else shows,
/// where the clock must not move before that step.
async fn armed(net: &Net, at_ms: i64) {
    tokio::time::timeout(PATIENCE, net.clock().unwrap().armed(at_ms))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "nothing armed at {at_ms}; sleeping: {:?}",
                net.clock().unwrap().sleeping()
            )
        });
}

/// The free tier's refresh interval the signed-in net sets, short enough
/// to step past.
const FREE_REFRESH: Duration = Duration::from_secs(60);

/// How long a test watches, at a policy time that stands still or just
/// short of a deadline, for what must not happen yet: a timer due then
/// fires and is answered on loopback well inside it.
const QUIET: Duration = Duration::from_millis(500);

/// How long a Pro link waits after a fresh credential before refreshing:
/// five minutes before the credential expires.
fn pro_refresh_ms() -> i64 {
    (testnet::CREDENTIAL_TTL - Duration::from_secs(5 * 60)).as_millis() as i64
}

/// Reads a peer's inventory to its CaughtUp and returns the hosts it named.
async fn peer_inventory_hosts(from: &Edge, to: Uuid) -> Vec<Vec<u8>> {
    let mut client = from.peer(to).await.expect("a route to the peer");
    let mut events = client
        .agents
        .subscribe_inventory(wire::SubscribeInventoryRequest {})
        .await
        .expect("the peer answers a trusted caller")
        .into_inner();
    let mut hosts = Vec::new();
    loop {
        let InventoryEvent { of } = events
            .message()
            .await
            .expect("the inventory streams")
            .expect("the inventory reaches CaughtUp");
        match of {
            Some(inventory_event::Of::Host(host)) => hosts.push(host.host_id().to_vec()),
            Some(inventory_event::Of::CaughtUp(_)) => return hosts,
            _ => {}
        }
    }
}

/// Pairs `initiator` with `responder` through both profiles' sockets, with a
/// PIN, over the responder's LAN listener.
async fn pair(net: &Net, initiator: &str, responder: &str) -> wire::Peer {
    let started = admin(net, responder)
        .await
        .start_pairing(StartPairingRequest {
            method: Some(start_pairing_request::Method::Pin(Empty {})),
            ttl_seconds: None,
        })
        .await
        .expect("the responder enters pairing mode")
        .into_inner();
    let Some(pairing_window::Secret::Pin(pin)) = started.secret else {
        panic!("a PIN pairing hands out a PIN");
    };
    assert_eq!(
        started.addrs,
        vec![edge(net, responder).lan_addr().unwrap().to_string()],
        "the invitation names the LAN listener"
    );
    let pending = admin(net, initiator)
        .await
        .begin_pair(BeginPairRequest {
            host_id: edge(net, responder).host_id().as_bytes().to_vec(),
            secret: Some(begin_pair_request::Secret::Pin(pin)),
            addrs: started.addrs,
        })
        .await
        .expect("the SPAKE2 exchange completes")
        .into_inner();
    assert_eq!(
        pending.peer.as_ref().unwrap().pubkey,
        edge(net, responder).public_key(),
        "the pending pairing names the responder's key"
    );
    assert_eq!(pending.via, wire::RouteKind::Direct as i32);
    admin(net, initiator)
        .await
        .confirm_pair(wire::ConfirmPairRequest {
            token: pending.token,
        })
        .await
        .expect("the person confirms the peer")
        .into_inner()
}

#[tokio::test]
async fn paired_hosts_link_both_ways_refuse_strangers_and_unpair() {
    let net = Net::start(
        Topology::new()
            .host_decl(lan_host("desk", SCOPE))
            .host_decl(lan_host("laptop", SCOPE))
            .host_decl(lan_host("stranger", "elsewhere")),
    )
    .await
    .unwrap();
    let (desk_id, laptop_id, stranger_id) = (
        edge(&net, "desk").host_id(),
        edge(&net, "laptop").host_id(),
        edge(&net, "stranger").host_id(),
    );

    // Discovery lists only the machines in this network's scope.
    until("the laptop in the desk's candidates", || async {
        let candidates = edge(&net, "desk").candidates();
        candidates
            .iter()
            .any(|advert| advert.host_id == laptop_id)
            .then_some(())
            .ok_or_else(|| format!("candidates: {candidates:?}"))
    })
    .await
    .unwrap();
    let desk_candidates = edge(&net, "desk")
        .candidates()
        .into_iter()
        .map(|advert| (advert.name, advert.scope))
        .collect::<Vec<_>>();
    assert_eq!(
        desk_candidates,
        vec![("laptop".to_owned(), SCOPE.to_owned())]
    );
    assert!(
        edge(&net, "stranger").candidates().is_empty(),
        "a host in another scope lists none of these"
    );
    println!("desk candidates: {desk_candidates:?}; stranger candidates: []");

    // The device identity the profile reports is the one pairing pins.
    let identity = admin(&net, "desk")
        .await
        .get_device_identity(wire::GetDeviceIdentityRequest {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(identity.host_id, desk_id.as_bytes());
    assert_eq!(identity.pubkey, edge(&net, "desk").public_key());

    let peer = pair(&net, "laptop", "desk").await;
    assert_eq!(peer.host_id, desk_id.as_bytes());
    assert_eq!(peer.name, "desk");
    println!(
        "laptop paired with {} over {:?}",
        peer.name,
        wire::RouteKind::Direct
    );

    // Both sides now trust each other, and the laptop dialled the desk.
    until("the desk to trust the laptop", || async {
        edge(&net, "desk")
            .is_trusted(laptop_id)
            .then_some(())
            .ok_or("not trusted")
    })
    .await
    .unwrap();
    until("a direct link both ways", || async {
        let via = (
            edge(&net, "desk").via(laptop_id).await,
            edge(&net, "laptop").via(desk_id).await,
        );
        (via == (LiveRoute::Direct, LiveRoute::Direct))
            .then_some(())
            .ok_or_else(|| format!("desk -> laptop, laptop -> desk: {via:?}"))
    })
    .await
    .unwrap();
    let desk_peers = admin(&net, "desk")
        .await
        .list_peers(wire::ListPeersRequest {})
        .await
        .unwrap()
        .into_inner()
        .peers;
    assert_eq!(
        desk_peers
            .iter()
            .map(|peer| peer.name.as_str())
            .collect::<Vec<_>>(),
        vec!["laptop"]
    );

    // Authenticated calls run in both directions, each answered by the
    // other host's own runtime.
    let from_laptop = peer_inventory_hosts(&edge(&net, "laptop"), desk_id).await;
    let from_desk = peer_inventory_hosts(&edge(&net, "desk"), laptop_id).await;
    assert!(from_laptop.contains(&desk_id.as_bytes().to_vec()));
    assert!(from_desk.contains(&laptop_id.as_bytes().to_vec()));
    println!("the peer services answered laptop -> desk and desk -> laptop");

    // A stranger that trusts the desk but that the desk never paired with
    // is refused: its pinned dial never becomes a link, and on the desk's
    // listener, without a trusted key, there are no peer services to reach.
    edge(&net, "stranger")
        .trust(&edge(&net, "desk"))
        .await
        .unwrap();
    let before = edge(&net, "stranger").last_dial_error(desk_id).await;
    edge(&net, "stranger").dial(desk_id, edge(&net, "desk").lan_addr().unwrap());
    let refused = dial_failed(&net, "stranger", desk_id, before).await;
    assert_eq!(
        edge(&net, "desk").via(stranger_id).await,
        LiveRoute::Offline
    );
    assert_eq!(
        edge(&net, "stranger").via(desk_id).await,
        LiveRoute::Offline
    );
    println!("the stranger's dial was refused: {refused}");
    let desk_addr = edge(&net, "desk").lan_addr().unwrap();
    let outside_pairing = match edge(&net, "stranger").unpinned_channel(desk_addr).await {
        Ok(channel) => wire::peer_agent_service_client(channel)
            .subscribe_inventory(wire::SubscribeInventoryRequest {})
            .await
            .map(|_| ())
            .expect_err("a stranger reaches no peer service")
            .code(),
        Err(_) => tonic::Code::Unavailable,
    };
    // In pairing mode a stranger's stream reaches the pairing service, and
    // only that.
    admin(&net, "desk")
        .await
        .start_pairing(StartPairingRequest {
            method: Some(start_pairing_request::Method::Qr(Empty {})),
            ttl_seconds: None,
        })
        .await
        .unwrap();
    let in_pairing = wire::peer_agent_service_client(
        edge(&net, "stranger")
            .unpinned_channel(desk_addr)
            .await
            .expect("pairing mode admits an unpinned stream"),
    )
    .subscribe_inventory(wire::SubscribeInventoryRequest {})
    .await
    .map(|_| ())
    .expect_err("pairing mode serves only pairing")
    .code();
    assert_eq!(in_pairing, tonic::Code::Unimplemented);
    admin(&net, "desk")
        .await
        .cancel_pairing(wire::CancelPairingRequest {})
        .await
        .unwrap();
    println!(
        "stranger refused: pinned dial never links; unpinned peer services call {outside_pairing:?}, \
         in pairing mode {in_pairing:?}"
    );
    assert!(!edge(&net, "desk").is_trusted(stranger_id));

    // Unpairing removes the laptop's key, closes its link, and a redial
    // with the now unknown key is refused.
    let removed = admin(&net, "desk")
        .await
        .unpair(UnpairRequest {
            peer: host_ref(laptop_id),
            reason: "retired".to_owned(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(removed.name, "laptop");
    until("the link to close both ways", || async {
        let via = (
            edge(&net, "desk").via(laptop_id).await,
            edge(&net, "laptop").via(desk_id).await,
        );
        (via == (LiveRoute::Offline, LiveRoute::Offline))
            .then_some(())
            .ok_or_else(|| format!("desk -> laptop, laptop -> desk: {via:?}"))
    })
    .await
    .unwrap();
    let gone = admin(&net, "desk")
        .await
        .get_peer(wire::GetPeerRequest {
            peer: Some(host_ref(laptop_id).unwrap()),
        })
        .await
        .expect_err("an unpaired host is no peer");
    assert_eq!(gone.code(), tonic::Code::NotFound);
    // The desk said why as it closed the link: the laptop still trusts it,
    // but reads it as no longer trusting the laptop, and does not dial it.
    until(
        "the laptop to read the desk as no longer trusting it",
        || async {
            net.runtime("laptop")
                .unwrap()
                .host_entry_of(desk_id)
                .is_some_and(|entry| entry.revoked())
                .then_some(())
                .ok_or("still trusted")
        },
    )
    .await
    .unwrap();
    assert_eq!(edge(&net, "desk").via(laptop_id).await, LiveRoute::Offline);
    println!("desk unpaired the laptop, which reads the desk as no longer trusting it");

    net.shutdown().await.unwrap();
}

#[tokio::test]
async fn trusted_edges_link_in_process_until_the_link_is_severed() {
    // Off the network, so nothing redials once the link is cut: two hosts
    // that find each other would link again directly.
    let net = Net::start(Topology::new().host("one").host("two"))
        .await
        .unwrap();
    let (one_id, two_id) = (edge(&net, "one").host_id(), edge(&net, "two").host_id());

    assert!(
        edge(&net, "one")
            .link_in_process(&edge(&net, "two"))
            .is_err(),
        "hosts that do not trust each other are not linked"
    );
    edge(&net, "one").trust(&edge(&net, "two")).await.unwrap();
    edge(&net, "two").trust(&edge(&net, "one")).await.unwrap();
    let link = edge(&net, "one")
        .link_in_process(&edge(&net, "two"))
        .unwrap();
    until("the in-process link", || async {
        let via = (
            edge(&net, "one").via(two_id).await,
            edge(&net, "two").via(one_id).await,
        );
        (via == (LiveRoute::Direct, LiveRoute::Direct))
            .then_some(())
            .ok_or_else(|| format!("one -> two, two -> one: {via:?}"))
    })
    .await
    .unwrap();
    assert!(
        peer_inventory_hosts(&edge(&net, "two"), one_id)
            .await
            .contains(&one_id.as_bytes().to_vec())
    );

    link.sever();
    until("the severed link to leave both hosts", || async {
        let via = (
            edge(&net, "one").via(two_id).await,
            edge(&net, "two").via(one_id).await,
        );
        (via == (LiveRoute::Offline, LiveRoute::Offline))
            .then_some(())
            .ok_or_else(|| format!("one -> two, two -> one: {via:?}"))
    })
    .await
    .unwrap();

    net.shutdown().await.unwrap();
}

/// A week, as the account service issues relay credentials.
const WEEK: Duration = Duration::from_secs(7 * 24 * 3600);
/// How old a kept credential grows before the link refreshes it.
const HOUR: Duration = Duration::from_secs(3600);

/// A net with one Pro account and a signed-out desk on the driven clock,
/// whose cloud mints credentials that live `ttl`.
async fn pro_desk(ttl: Duration) -> Net {
    let net = Net::start_with(
        Topology::new()
            .relay(&["ada"])
            .host_decl(lan_host("desk", SCOPE)),
        NetOptions {
            clock: ClockMode::Driven,
            ..NetOptions::default()
        },
    )
    .await
    .unwrap();
    net.relay().unwrap().set_credential_ttl(ttl);
    net
}

/// Binds the desk to Ada and waits for its relay link.
async fn sign_in_desk(net: &Net) {
    door(net, "desk")
        .await
        .bind_profile(BindProfileRequest {
            profile_id: Some(id(net, "desk")),
            cloud_url: net.relay().unwrap().url().to_owned(),
            staged_refresh_token: Relay::login("ada"),
            ..BindProfileRequest::default()
        })
        .await
        .expect("the profile binds to the account");
    relay_link_up(net).await;
}

/// Waits for the desk's relay link, as the desk and the relay see it.
async fn relay_link_up(net: &Net) {
    let relay = net.relay().unwrap();
    until("the desk's relay link", || async {
        let info = info(net, "desk").await;
        let links = relay.links("ada").await;
        (info.relay_connected().is_some() && links.len() == 1)
            .then_some(())
            .ok_or_else(|| format!("cloud {:?}, ada's links {links:?}", info.cloud))
    })
    .await
    .unwrap();
}

/// Waits until the relay has been shown `count` credentials.
async fn presented(relay: &Relay, count: usize) -> Vec<String> {
    until("the relay to see the credential", || async {
        let presented = relay.presented();
        (presented.len() >= count)
            .then(|| presented.clone())
            .ok_or_else(|| format!("presented: {presented:?}"))
    })
    .await
    .unwrap()
}

/// Restarts the desk's daemon, as a relaunch does.
async fn relaunch_desk(net: &mut Net) {
    net.stop_daemon("desk").await.unwrap();
    net.restart_daemon("desk").await.unwrap();
}

#[tokio::test]
async fn a_relaunch_presents_its_kept_relay_credential_without_calling_the_cloud() {
    let mut net = pro_desk(WEEK).await;
    sign_in_desk(&net).await;
    assert_eq!(net.relay().unwrap().connects().len(), 1);
    println!("signed in: the cloud issued relay-ada-1, a week long");

    relaunch_desk(&mut net).await;
    relay_link_up(&net).await;
    assert_eq!(
        presented(net.relay().unwrap(), 2).await,
        ["relay-ada-1", "relay-ada-1"],
        "the relaunch presented the kept credential in Hello"
    );
    assert_eq!(
        net.relay().unwrap().connects().len(),
        1,
        "the relaunch made no call to the account service"
    );
    // The credential is not an hour old: its refresh waits for that, to
    // the millisecond, counted from its fetch.
    net.advance(HOUR - Duration::from_millis(1)).unwrap();
    holds_for("nothing fetched short of the hour", QUIET, || async {
        net.relay().unwrap().connects().len() == 1
    })
    .await
    .unwrap();
    net.advance(Duration::from_millis(1)).unwrap();
    assert_eq!(
        presented(net.relay().unwrap(), 3).await.last().unwrap(),
        "relay-ada-2",
        "the hour-old credential is refreshed"
    );
    assert_eq!(net.relay().unwrap().connects().len(), 2);
    println!(
        "relaunched: relay-ada-1 presented again; the cloud's connects stayed at 1 until the hour"
    );

    net.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_kept_relay_credential_over_an_hour_old_is_refreshed_at_once_and_then_hourly() {
    let mut net = pro_desk(WEEK).await;
    sign_in_desk(&net).await;
    net.stop_daemon("desk").await.unwrap();
    net.advance(2 * HOUR).unwrap();
    net.restart_daemon("desk").await.unwrap();
    relay_link_up(&net).await;
    assert_eq!(
        presented(net.relay().unwrap(), 3).await,
        ["relay-ada-1", "relay-ada-1", "relay-ada-2"],
        "the two-hour-old credential opened the link and a fresh one followed in Reauth"
    );
    assert_eq!(net.relay().unwrap().connects().len(), 2);
    println!("relaunched two hours later: Hello with relay-ada-1, Reauth with relay-ada-2");

    // Connected, the link refreshes every hour, counted from each fetch:
    // a refresh armed after the clock got there fires as it is armed.
    for (count, token) in [(4, "relay-ada-3"), (5, "relay-ada-4")] {
        net.advance(HOUR).unwrap();
        assert_eq!(
            presented(net.relay().unwrap(), count).await.last().unwrap(),
            token
        );
        println!("clock +1h: Reauth with {token}");
    }
    assert_eq!(net.relay().unwrap().links("ada").await.len(), 1);

    net.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_expired_kept_relay_credential_is_never_presented() {
    let mut net = pro_desk(testnet::CREDENTIAL_TTL).await;
    sign_in_desk(&net).await;
    net.stop_daemon("desk").await.unwrap();
    net.advance(testnet::CREDENTIAL_TTL).unwrap();
    net.restart_daemon("desk").await.unwrap();
    relay_link_up(&net).await;
    assert_eq!(
        presented(net.relay().unwrap(), 2).await,
        ["relay-ada-1", "relay-ada-2"],
        "the expired credential stayed home; a fresh one opened the link"
    );
    assert_eq!(net.relay().unwrap().connects().len(), 2);
    println!("relaunched past the expiry: relay-ada-1 never presented, relay-ada-2 fetched");

    net.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_kept_relay_credential_the_relay_refuses_is_replaced_by_a_fresh_one() {
    let mut net = pro_desk(testnet::CREDENTIAL_TTL).await;
    // The relay's clock runs nine minutes ahead: the credential the desk
    // keeps lapses at the relay a minute after it is minted, while the
    // desk still reads nine minutes left on it.
    net.relay().unwrap().run_ahead(Duration::from_secs(9 * 60));
    sign_in_desk(&net).await;
    net.relay().unwrap().run_ahead(Duration::ZERO);
    net.stop_daemon("desk").await.unwrap();
    net.advance(Duration::from_secs(2 * 60)).unwrap();
    net.restart_daemon("desk").await.unwrap();
    relay_link_up(&net).await;
    assert_eq!(
        presented(net.relay().unwrap(), 3).await,
        ["relay-ada-1", "relay-ada-1", "relay-ada-2"],
        "the refused credential was forgotten and a fresh one fetched"
    );
    assert_eq!(net.relay().unwrap().connects().len(), 2);
    println!("the relay refused the kept relay-ada-1; relay-ada-2 opened the link");

    net.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_signed_in_profile_refreshes_its_relay_credential_on_the_runtime_clock() {
    let mut topology = Topology::new()
        .relay(&["ada"])
        .host_decl(lan_host("desk", SCOPE));
    topology.relay.as_mut().unwrap().accounts[0].tier = TierDecl::Free;
    let net = Net::start_with(
        topology,
        NetOptions {
            clock: ClockMode::Driven,
            edge: Some(Arc::new(|_, edge| {
                edge.cloud.free_refresh_interval = Some(FREE_REFRESH);
            })),
            ..NetOptions::default()
        },
    )
    .await
    .unwrap();
    let relay = net.relay().unwrap();
    let host_id = edge(&net, "desk").host_id();
    let unbound = info(&net, "desk").await;
    assert!(matches!(unbound.standing(), Some(Standing::Unbound(_))));

    let bound = door(&net, "desk")
        .await
        .bind_profile(BindProfileRequest {
            profile_id: Some(id(&net, "desk")),
            cloud_url: relay.url().to_owned(),
            staged_refresh_token: Relay::login("ada"),
            ..BindProfileRequest::default()
        })
        .await
        .expect("the profile binds to the account")
        .into_inner();
    assert!(matches!(bound.standing(), Some(Standing::SignedIn(_))));
    assert_eq!(bound.email, "ada@example.com");
    assert_eq!(bound.account_name, "ada");

    until("the cloud link to connect", || async {
        let info = info(&net, "desk").await;
        (info.relay_connected().map(wire::RelayConnected::tier) == Some(wire::Tier::Free))
            .then_some(())
            .ok_or_else(|| format!("cloud {:?}", info.cloud))
    })
    .await
    .unwrap();
    assert_eq!(relay.links("ada").await, vec![(host_id, 1)]);
    assert_eq!(relay.connects().len(), 1);
    println!("bound as Ada; connected to the relay on the free tier");

    // The account buys Pro. Nothing happens until the runtime's clock
    // reaches the free tier's refresh interval this net sets to a minute:
    // the refresh is armed on the policy clock, which has not moved since
    // the link connected, and has not fired.
    relay.set_tier("ada", Tier::Pro);
    armed(&net, net.now_ms() + FREE_REFRESH.as_millis() as i64).await;
    assert_eq!(relay.connects().len(), 1);
    net.advance(Duration::from_secs(60)).unwrap();
    until("the refreshed credential to carry the new tier", || async {
        let info = info(&net, "desk").await;
        let tier = info.relay_connected().map(wire::RelayConnected::tier);
        (tier == Some(wire::Tier::Pro))
            .then_some(())
            .ok_or_else(|| format!("tier {tier:?}"))
    })
    .await
    .unwrap();
    assert_eq!(relay.connects().len(), 2);
    // The desk reports the new tier as it sends Reauth, so the relay may
    // not have read it yet.
    assert_eq!(
        presented(relay, 2).await,
        ["relay-ada-1", "relay-ada-2"],
        "the relay saw the first credential in Hello and the second in Reauth"
    );
    println!("clock +60s: Reauth with relay-ada-2, tier Pro");
    // On Pro the next refresh is armed five minutes before the fresh
    // credential expires, so nothing refreshes again at once.
    holds_for(
        "a fresh Pro credential not to be refreshed again at once",
        QUIET,
        || async { relay.connects().len() == 2 },
    )
    .await
    .unwrap();

    // On Pro the link refreshes five minutes before its credential expires.
    // Past the first credential's expiry on the relay's clock, the link
    // lives on a refreshed one.
    net.advance(Duration::from_secs(550)).unwrap();
    until("the Pro credential's refresh before expiry", || async {
        let presented = relay.presented();
        presented
            .contains(&"relay-ada-3".to_owned())
            .then_some(())
            .ok_or_else(|| format!("presented: {presented:?}"))
    })
    .await
    .unwrap();
    // The refresh went through: the next comes at its own boundary, five
    // minutes before relay-ada-3 expires.
    net.advance(Duration::from_millis(pro_refresh_ms() as u64))
        .unwrap();
    until("the next refresh at its boundary", || async {
        let presented = relay.presented();
        presented
            .contains(&"relay-ada-4".to_owned())
            .then_some(())
            .ok_or_else(|| format!("presented: {presented:?}"))
    })
    .await
    .unwrap();
    assert_eq!(relay.links("ada").await, vec![(host_id, 1)]);
    assert!(info(&net, "desk").await.relay_connected().is_some());
    println!(
        "clock +910s: relay-ada-1 expired, the link lived on relay-ada-3 and then relay-ada-4"
    );

    // Pausing drops the link and keeps the credential; resuming brings it
    // back; signing out forgets the credential and keeps the binding.
    let paused = admin(&net, "desk")
        .await
        .pause_profile(wire::PauseProfileRequest {})
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(paused.standing(), Some(Standing::Paused(_))));
    until("the relay link to go with the pause", || async {
        let links = relay.links("ada").await;
        links
            .is_empty()
            .then_some(())
            .ok_or_else(|| format!("ada's links: {links:?}"))
    })
    .await
    .unwrap();
    admin(&net, "desk")
        .await
        .resume_profile(wire::ResumeProfileRequest {})
        .await
        .unwrap();
    until("the relay link to come back", || async {
        let links = relay.links("ada").await;
        (!links.is_empty())
            .then_some(())
            .ok_or_else(|| format!("ada's links: {links:?}"))
    })
    .await
    .unwrap();
    let signed_out = admin(&net, "desk")
        .await
        .sign_out(wire::SignOutRequest {})
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(
        signed_out.standing(),
        Some(Standing::SignedOut(_))
    ));
    assert_eq!(signed_out.email, "ada@example.com");
    until("the relay link to go with the sign-out", || async {
        let links = relay.links("ada").await;
        links
            .is_empty()
            .then_some(())
            .ok_or_else(|| format!("ada's links: {links:?}"))
    })
    .await
    .unwrap();
    println!("paused, resumed and signed out");

    net.shutdown().await.unwrap();
}
