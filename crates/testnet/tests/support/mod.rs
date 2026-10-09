//! What the network specifications share: reaching a host's edge and front
//! door, waiting for a condition, pairing through the front doors, and
//! reading a peer's inventory over the link.

#![allow(dead_code)]

use std::sync::Arc;

use node::harness::LiveRoute;
use node::{Edge, Observed, RelayCarrier};
use testnet::{HostDecl, Net, PATIENCE, until};
use tonic::transport::Channel;
use uuid::Uuid;
use wire::profile_service_client::ProfileServiceClient;
use wire::{
    BeginPairRequest, Empty, HostRef, InventoryEvent, PairingWindow, PendingPairRequest,
    ProfileRequest, StartPairingRequest, begin_pair_request, host_ref, inventory_event,
    pairing_window, start_pairing_request,
};

/// A host listening for direct links on loopback and on the net's
/// discovery bus, in `scope`.
pub fn lan_host(name: &str, scope: &str) -> HostDecl {
    HostDecl {
        name: name.to_owned(),
        lan: true,
        discovery: true,
        scope: Some(scope.to_owned()),
        ..HostDecl::default()
    }
}

pub fn edge(net: &Net, host: &str) -> Arc<Edge> {
    net.edge(host).expect("the edge runs")
}

pub fn host_id(net: &Net, host: &str) -> Uuid {
    net.host(host).expect("the host").host_id
}

pub fn profile(net: &Net, host: &str) -> Vec<u8> {
    net.host(host)
        .expect("the host")
        .profile
        .as_bytes()
        .to_vec()
}

/// A front-door call naming `host`'s profile.
pub fn profile_request(net: &Net, host: &str) -> ProfileRequest {
    ProfileRequest {
        profile_id: profile(net, host),
    }
}

pub async fn door(net: &Net, host: &str) -> ProfileServiceClient<Channel> {
    net.front_door(host).await.expect("the front door answers")
}

pub fn host_ref(host: Uuid) -> Option<HostRef> {
    Some(HostRef {
        of: Some(host_ref::Of::HostId(host.as_bytes().to_vec())),
    })
}

/// Waits until each host reaches the other over `via`.
pub async fn until_via(net: &Net, a: &str, b: &str, via: LiveRoute) {
    let (a_id, b_id) = (host_id(net, a), host_id(net, b));
    let (near, far) = (edge(net, a), edge(net, b));
    until(&format!("{a} and {b} to reach each other {via:?}"), || {
        let (near, far) = (near.clone(), far.clone());
        async move {
            let seen = (near.via(b_id).await, far.via(a_id).await);
            (seen == (via, via))
                .then_some(())
                .ok_or_else(|| format!("{a} -> {b} {:?}, {b} -> {a} {:?}", seen.0, seen.1))
        }
    })
    .await
    .unwrap();
}

/// The carrier the host's relay link rides, once it is connected.
pub fn carrier(net: &Net, host: &str) -> Option<RelayCarrier> {
    match edge(net, host).observed() {
        Observed::Connected { carrier, .. } => Some(carrier),
        _ => None,
    }
}

/// Reads a peer's inventory to its CaughtUp and returns the hosts it named.
pub async fn peer_inventory_hosts(from: &Edge, to: Uuid) -> Result<Vec<Vec<u8>>, tonic::Status> {
    let mut client = from
        .peer(to)
        .await
        .map_err(|error| tonic::Status::unavailable(error.to_string()))?;
    let mut events = client.subscribe_inventory(Empty {}).await?.into_inner();
    let mut hosts = Vec::new();
    loop {
        let InventoryEvent { of } = events
            .message()
            .await?
            .ok_or_else(|| tonic::Status::aborted("the inventory ended before CaughtUp"))?;
        match of {
            Some(inventory_event::Of::Host(host)) => hosts.push(host.host_id),
            Some(inventory_event::Of::CaughtUp(_)) => return Ok(hosts),
            _ => {}
        }
    }
}

/// Puts `responder`'s profile into pairing mode.
pub async fn start_pairing(
    net: &Net,
    responder: &str,
    pairing: StartPairingRequest,
) -> Result<PairingWindow, tonic::Status> {
    door(net, responder)
        .await
        .start_pairing(StartPairingRequest {
            profile_id: profile(net, responder),
            ..pairing
        })
        .await
        .map(tonic::Response::into_inner)
}

/// A one-shot PIN window, for whichever profile `start_pairing` names.
pub fn pin_mode() -> StartPairingRequest {
    StartPairingRequest {
        method: Some(start_pairing_request::Method::Pin(Empty {})),
        ..StartPairingRequest::default()
    }
}

/// A one-shot QR window, for whichever profile `start_pairing` names.
pub fn qr_mode() -> StartPairingRequest {
    StartPairingRequest {
        method: Some(start_pairing_request::Method::Qr(Empty {})),
        ..StartPairingRequest::default()
    }
}

/// A reusable demo window holding `pin` for `ttl_seconds`.
pub fn demo_mode(pin: &str, ttl_seconds: u64) -> StartPairingRequest {
    StartPairingRequest {
        method: Some(start_pairing_request::Method::DemoPin(pin.to_owned())),
        ttl_seconds: Some(ttl_seconds),
        ..StartPairingRequest::default()
    }
}

pub fn pin_of(started: &PairingWindow) -> String {
    match &started.secret {
        Some(pairing_window::Secret::Pin(pin)) => pin.clone(),
        other => panic!("a PIN pairing hands out a PIN, not {other:?}"),
    }
}

/// `initiator` begins pairing with the host at `addrs` using `secret`,
/// naming `host` when it knows it.
pub async fn begin_pair(
    net: &Net,
    initiator: &str,
    host: Option<Uuid>,
    secret: begin_pair_request::Secret,
    addrs: Vec<String>,
) -> Result<wire::PendingPair, tonic::Status> {
    door(net, initiator)
        .await
        .begin_pair(BeginPairRequest {
            profile_id: profile(net, initiator),
            host_id: host
                .map(|host| host.as_bytes().to_vec())
                .unwrap_or_default(),
            secret: Some(secret),
            addrs,
        })
        .await
        .map(tonic::Response::into_inner)
}

pub async fn confirm_pair(
    net: &Net,
    initiator: &str,
    token: Vec<u8>,
) -> Result<wire::PeerEntry, tonic::Status> {
    door(net, initiator)
        .await
        .confirm_pair(PendingPairRequest {
            profile_id: profile(net, initiator),
            token,
        })
        .await
        .map(tonic::Response::into_inner)
}

/// Pairs `initiator` with `responder` through both front doors with a PIN
/// over the responder's LAN listener.
pub async fn pair(net: &Net, initiator: &str, responder: &str) -> wire::PeerEntry {
    let started = start_pairing(net, responder, pin_mode())
        .await
        .expect("the responder enters pairing mode");
    let pending = begin_pair(
        net,
        initiator,
        Some(host_id(net, responder)),
        begin_pair_request::Secret::Pin(pin_of(&started)),
        started.addrs,
    )
    .await
    .expect("the SPAKE2 exchange completes");
    confirm_pair(net, initiator, pending.token)
        .await
        .expect("the person confirms the peer")
}

/// Opens `agent`'s session on `host` from `from` over the link, the way a
/// peer's source does, and reads it to its CaughtUp.
pub async fn open_session(
    from: &Edge,
    host: Uuid,
    agent: Uuid,
) -> Result<tonic::Streaming<wire::SessionEvent>, tonic::Status> {
    let mut client = from
        .session_peer(host)
        .await
        .map_err(|error| tonic::Status::unavailable(error.to_string()))?;
    let mut events = client
        .subscribe(wire::PeerSubscribeRequest {
            agent_id: agent.as_bytes().to_vec(),
            from: Some(wire::peer_subscribe_request::From::Tail(10)),
        })
        .await?
        .into_inner();
    loop {
        let event = events
            .message()
            .await?
            .ok_or_else(|| tonic::Status::aborted("the session ended before CaughtUp"))?;
        if matches!(event.of, Some(wire::session_event::Of::CaughtUp(_))) {
            return Ok(events);
        }
    }
}

/// Waits for a stream to end, with an error or cleanly, and says how.
pub async fn ended<T>(stream: &mut tonic::Streaming<T>, what: &str) -> String {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        match tokio::time::timeout_at(deadline, stream.message()).await {
            Err(_) => panic!("timed out waiting for {what} to end"),
            Ok(Ok(Some(_))) => continue,
            Ok(Ok(None)) => return "ended".to_owned(),
            Ok(Err(status)) => return format!("{:?}", status.code()),
        }
    }
}

/// The tier the host's relay link carries, once it is connected.
pub fn tier(net: &Net, host: &str) -> Option<node::Tier> {
    match edge(net, host).observed() {
        Observed::Connected { tier, .. } => Some(tier),
        _ => None,
    }
}

/// Reads a session until an item with `text` arrives, failing on Lagged,
/// on the stream's end, or after the patience.
pub async fn until_item(stream: &mut tonic::Streaming<wire::SessionEvent>, text: &str) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let event = tokio::time::timeout_at(deadline, stream.message())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {text:?}"))
            .unwrap_or_else(|status| panic!("the session failed waiting for {text:?}: {status:?}"))
            .unwrap_or_else(|| panic!("the session ended waiting for {text:?}"));
        match event.of {
            Some(wire::session_event::Of::Item(item)) if item.text == text => return,
            Some(wire::session_event::Of::Lagged(_)) => panic!("lagged waiting for {text:?}"),
            _ => {}
        }
    }
}
