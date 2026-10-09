//! The link registry: this daemon's live links and the wire-side adjacency
//! discipline.
//!
//! The registry is the single source of truth for *wire* adjacency. Every
//! `NeighborUp`/`NeighborDown` a peer ever receives from us is emitted here,
//! under one lock, in registration order — so "advertise only adjacency" is
//! structural: there is no API for broadcasting anything else. The handshake
//! snapshot is reconciled here too: registering a link diffs the neighbor
//! set the handshake advertised against the current one and sends the
//! difference down the new link, atomically with registration, which closes
//! the window between composing the snapshot and the link going live.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::{Notify, RwLock, mpsc};
use wire::pb;

use crate::link::LinkCarrier as NativeLinkCarrier;
use crate::routing::types::LinkId;
use crate::routing::wire::{neighbor_down_message, neighbor_up_message};
use crate::routing::{ConnectRole, Host, RuntimeInstance};
use crate::{HostId, Tier, audit};

pub(crate) type LinkOutputTx = mpsc::Sender<pb::Message>;

#[derive(Default)]
pub struct LinkRegistry {
    state: RwLock<LinkRegistryState>,
}

#[derive(Default)]
struct LinkRegistryState {
    writers: HashMap<LinkId, LinkWriter>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkRole {
    Peer,
    CloudRelay,
}

/// The physical carrier underneath a direct routing link. Relay routes are
/// identified by `Route::Via`; direct routes use this tag to distinguish SSH
/// from a network connection.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LinkCarrier {
    Direct,
    Ssh,
}

/// How this daemon authenticated a live link. Only token-admitted links
/// carry entitlement: pinned device links are independent of any account.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum LinkAdmission {
    PinnedKey,
    CloudToken { tier: Tier },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct LinkProperties {
    pub(crate) role: LinkRole,
    pub(crate) admission: LinkAdmission,
    pub(crate) carrier: LinkCarrier,
    /// The life of the peer's runtime this link was opened by.
    pub(crate) runtime_instance: RuntimeInstance,
    pub(crate) direct_order: Option<DirectLinkOrder>,
}

/// Stable ordering for crossed direct dials. A fallback remains valid when it
/// is the only connection; when both directions connect, both peers retain the
/// same preferred physical link.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum DirectLinkOrder {
    Preferred,
    Fallback,
}

impl DirectLinkOrder {
    pub(crate) fn for_hosts(local: HostId, peer: HostId, role: ConnectRole) -> Self {
        let local_opens_preferred = local < peer;
        if local_opens_preferred == (role == ConnectRole::Connector) {
            Self::Preferred
        } else {
            Self::Fallback
        }
    }
}

#[derive(Clone)]
struct LinkWriter {
    host: Host,
    tx: LinkOutputTx,
    close_tx: mpsc::Sender<LinkCloseRequest>,
    closed: Arc<Notify>,
    role: LinkRole,
    admission: LinkAdmission,
    carrier: LinkCarrier,
    runtime_instance: RuntimeInstance,
    direct_order: Option<DirectLinkOrder>,
    native_carrier: Option<Arc<dyn NativeLinkCarrier>>,
}

/// A local request for the link's connect task to close the link. Distinct
/// from the wire `pb::LinkCloseReason`, which names why a *peer* closed it.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum LinkCloseRequest {
    OutgoingQueueFull,
    TrustReplaced,
    Superseded,
    /// This host no longer trusts the peer: the link closes saying
    /// USER_REVOKED, in the close message and as the connection's own
    /// close code, so the peer learns it even when closing the connection
    /// drops the message.
    Revoked,
}

/// A link the registry accepted.
pub(crate) struct Registration {
    /// Receives the registry's requests to close this link.
    pub(crate) close_rx: mpsc::Receiver<LinkCloseRequest>,
    /// Direct links to the same peer this one superseded. They are gone from
    /// the registry already and are asked to close, but their own tasks take
    /// time to finish; whoever routes over links has to forget them now, or a
    /// call in that gap picks a link nothing can open a stream on.
    pub(crate) displaced: Vec<LinkId>,
}

impl LinkRegistry {
    /// Registers a live link and runs the adjacency discipline atomically:
    /// other links learn `NeighborUp(peer)` if this is the first link to the
    /// peer, and this link receives the diff between `advertised_snapshot`
    /// (the neighbor set its handshake carried) and the current one.
    #[cfg(test)]
    pub(crate) async fn register(
        &self,
        link: LinkId,
        host: Host,
        outgoing_tx: LinkOutputTx,
        role: LinkRole,
        advertised_snapshot: &[HostId],
    ) -> mpsc::Receiver<LinkCloseRequest> {
        self.register_with_details(
            link,
            host,
            outgoing_tx,
            LinkProperties {
                role,
                admission: LinkAdmission::PinnedKey,
                carrier: LinkCarrier::Direct,
                runtime_instance: crate::routing::RuntimeInstance::random(),
                direct_order: None,
            },
            advertised_snapshot,
            None,
        )
        .await
        .expect("test registration has no ordered direct duplicate")
    }

    #[cfg(test)]
    pub(crate) async fn register_with_admission(
        &self,
        link: LinkId,
        host: Host,
        outgoing_tx: LinkOutputTx,
        role: LinkRole,
        admission: LinkAdmission,
        advertised_snapshot: &[HostId],
    ) -> mpsc::Receiver<LinkCloseRequest> {
        self.register_with_details(
            link,
            host,
            outgoing_tx,
            LinkProperties {
                role,
                admission,
                carrier: LinkCarrier::Direct,
                runtime_instance: crate::routing::RuntimeInstance::random(),
                direct_order: None,
            },
            advertised_snapshot,
            None,
        )
        .await
        .expect("test registration has no ordered direct duplicate")
    }

    #[cfg(test)]
    pub(crate) async fn register_with_details(
        &self,
        link: LinkId,
        host: Host,
        outgoing_tx: LinkOutputTx,
        properties: LinkProperties,
        advertised_snapshot: &[HostId],
        native_carrier: Option<Arc<dyn NativeLinkCarrier>>,
    ) -> Option<mpsc::Receiver<LinkCloseRequest>> {
        self.register_displacing(
            link,
            host,
            outgoing_tx,
            properties,
            advertised_snapshot,
            native_carrier,
        )
        .await
        .map(|registration| registration.close_rx)
    }

    /// Registers a link and names the direct links it superseded, or refuses
    /// it where the same runtime instance of the peer already holds a direct link
    /// that wins against it.
    pub(crate) async fn register_displacing(
        &self,
        link: LinkId,
        host: Host,
        outgoing_tx: LinkOutputTx,
        properties: LinkProperties,
        advertised_snapshot: &[HostId],
        native_carrier: Option<Arc<dyn NativeLinkCarrier>>,
    ) -> Option<Registration> {
        let LinkProperties {
            role,
            admission,
            carrier,
            runtime_instance,
            direct_order,
        } = properties;
        let (close_tx, close_rx) = mpsc::channel(1);
        let closed = Arc::new(Notify::new());
        let mut state = self.state.write().await;
        let first_link_to_peer = !state
            .writers
            .values()
            .any(|writer| writer.host.id == host.id);

        let mut displaced = Vec::new();
        if role == LinkRole::Peer
            && carrier == LinkCarrier::Direct
            && let Some(order) = direct_order
        {
            let duplicates = state
                .writers
                .iter()
                .filter(|(_, writer)| {
                    writer.host.id == host.id
                        && writer.role == LinkRole::Peer
                        && writer.carrier == LinkCarrier::Direct
                        && writer.direct_order.is_some()
                })
                .map(|(id, writer)| (*id, writer.direct_order.unwrap(), writer.runtime_instance))
                .collect::<Vec<_>>();
            // A duplicate from the same runtime instance is the same live process:
            // a crossed dial, settled by keeping the preferred link. One from
            // another runtime instance belongs to a process that has since died, so
            // it is replaced whichever direction either link was dialled in.
            if duplicates.iter().any(|(_, existing, from)| {
                *from == runtime_instance
                    && (*existing == DirectLinkOrder::Preferred || *existing == order)
            }) {
                return None;
            }
            for (id, _, _) in duplicates {
                if let Some(writer) = state.writers.remove(&id) {
                    displaced.push((id, writer));
                }
            }
        }
        // Reconcile the new link's view: the handshake snapshot plus this
        // diff equals the registry's neighbor set at this instant.
        let advertised: HashSet<HostId> = advertised_snapshot.iter().copied().collect();
        let mut current: HashMap<HostId, Host> = HashMap::new();
        for writer in state.writers.values() {
            current.entry(writer.host.id).or_insert(writer.host.clone());
        }
        for (peer, peer_host) in &current {
            if *peer != host.id && !advertised.contains(peer) {
                try_send_or_request_close(&outgoing_tx, &close_tx, neighbor_up_message(peer_host));
            }
        }
        for advertised_peer in advertised {
            if advertised_peer != host.id && !current.contains_key(&advertised_peer) {
                try_send_or_request_close(
                    &outgoing_tx,
                    &close_tx,
                    neighbor_down_message(advertised_peer),
                );
            }
        }

        if first_link_to_peer {
            let message = neighbor_up_message(&host);
            fanout(&state, message, host.id);
        }

        let old = state.writers.insert(
            link,
            LinkWriter {
                host: host.clone(),
                tx: outgoing_tx,
                close_tx,
                closed,
                role,
                admission,
                carrier,
                runtime_instance,
                direct_order,
                native_carrier,
            },
        );
        drop(state);
        let displaced_ids = displaced.iter().map(|(id, _)| *id).collect();
        for (id, writer) in displaced {
            let _ = writer.close_tx.try_send(LinkCloseRequest::Superseded);
            writer.closed.notify_waiters();
            audit::link_down(writer.host.id, &id, "superseded");
        }
        if let Some(old) = old {
            old.closed.notify_waiters();
            audit::link_down(old.host.id, &link, "replaced");
        }
        audit::link_up(host.id, &link, role);
        Some(Registration {
            close_rx,
            displaced: displaced_ids,
        })
    }

    /// Removes a link; if it was the last link to its peer, other links
    /// learn `NeighborDown(peer)`.
    pub(crate) async fn remove(&self, link: &LinkId) {
        let mut state = self.state.write().await;
        let Some(writer) = state.writers.remove(link) else {
            return;
        };
        let peer = writer.host.id;
        let last_link_to_peer = !state.writers.values().any(|other| other.host.id == peer);
        if last_link_to_peer {
            let message = neighbor_down_message(peer);
            fanout(&state, message, peer);
        }
        drop(state);
        writer.closed.notify_waiters();
        audit::link_down(peer, link, "removed");
    }

    /// The current neighbor set, as a handshake snapshot.
    pub(crate) async fn neighbor_snapshot(&self) -> Vec<Host> {
        let state = self.state.read().await;
        let mut seen = HashSet::new();
        let mut snapshot = Vec::new();
        for writer in state.writers.values() {
            if seen.insert(writer.host.id) {
                snapshot.push(writer.host.clone());
            }
        }
        snapshot.sort_unstable_by_key(|host| host.id);
        snapshot
    }

    /// Every peer this registry holds a link to, and how many links each one
    /// is holding. A second link to the same host is a peer that opened a
    /// connection where it should have multiplexed the one it had.
    pub(crate) async fn links_per_peer(&self) -> Vec<(HostId, usize)> {
        let state = self.state.read().await;
        let mut counted: std::collections::BTreeMap<HostId, usize> = Default::default();
        for writer in state.writers.values() {
            *counted.entry(writer.host.id).or_default() += 1;
        }
        counted.into_iter().collect()
    }

    /// Requests closure of every link to `host_id`, for `request`, and waits
    /// until they are gone from the registry.
    pub(crate) async fn close_host(
        &self,
        host_id: HostId,
        request: LinkCloseRequest,
    ) -> Vec<LinkId> {
        let closing = {
            let state = self.state.read().await;
            state
                .writers
                .iter()
                .filter(|(_, writer)| writer.host.id == host_id)
                .map(|(link, writer)| {
                    let _ = writer.close_tx.try_send(request);
                    (*link, writer.closed.clone())
                })
                .collect::<Vec<_>>()
        };
        for (link, closed) in &closing {
            loop {
                let notified = closed.notified();
                if !self.state.read().await.writers.contains_key(link) {
                    break;
                }
                notified.await;
            }
        }
        closing.into_iter().map(|(link, _)| link).collect()
    }

    /// Requests closure of every device-to-device link and waits until each
    /// has left the registry. Cloud relay links remain available.
    pub async fn close_peer_links(&self) -> Vec<LinkId> {
        let closing = {
            let state = self.state.read().await;
            state
                .writers
                .iter()
                .filter(|(_, writer)| writer.role == LinkRole::Peer)
                .map(|(link, writer)| {
                    let _ = writer.close_tx.try_send(LinkCloseRequest::TrustReplaced);
                    (*link, writer.closed.clone())
                })
                .collect::<Vec<_>>()
        };
        for (link, closed) in &closing {
            loop {
                let notified = closed.notified();
                if !self.state.read().await.writers.contains_key(link) {
                    break;
                }
                notified.await;
            }
        }
        closing.into_iter().map(|(link, _)| link).collect()
    }

    pub(crate) async fn admission(&self, link: &LinkId) -> Option<LinkAdmission> {
        self.state
            .read()
            .await
            .writers
            .get(link)
            .map(|writer| writer.admission)
    }

    pub(crate) async fn carrier(&self, link: &LinkId) -> Option<LinkCarrier> {
        self.state
            .read()
            .await
            .writers
            .get(link)
            .map(|writer| writer.carrier)
    }

    pub(crate) async fn native_carrier(&self, link: &LinkId) -> Option<Arc<dyn NativeLinkCarrier>> {
        self.state
            .read()
            .await
            .writers
            .get(link)
            .and_then(|writer| writer.native_carrier.clone())
    }

    pub async fn native_carrier_to_peer(
        &self,
        peer: HostId,
    ) -> Option<(LinkId, Arc<dyn NativeLinkCarrier>)> {
        self.state
            .read()
            .await
            .writers
            .iter()
            .find_map(|(link, writer)| {
                (writer.host.id == peer)
                    .then(|| {
                        writer
                            .native_carrier
                            .clone()
                            .map(|carrier| (*link, carrier))
                    })
                    .flatten()
            })
    }

    pub(crate) async fn native_route_to_peer(
        &self,
        peer: HostId,
    ) -> Option<(LinkId, Arc<dyn NativeLinkCarrier>, LinkAdmission)> {
        self.state
            .read()
            .await
            .writers
            .iter()
            .find_map(|(link, writer)| {
                if writer.host.id != peer {
                    return None;
                }
                writer
                    .native_carrier
                    .clone()
                    .map(|carrier| (*link, carrier, writer.admission))
            })
    }

    pub(crate) async fn update_cloud_tier(&self, link: &LinkId, tier: Tier) {
        let mut state = self.state.write().await;
        let Some(writer) = state.writers.get_mut(link) else {
            return;
        };
        if matches!(writer.admission, LinkAdmission::CloudToken { .. }) {
            writer.admission = LinkAdmission::CloudToken { tier };
        }
    }

    /// A writer for any live link to `peer` — the forwarding rule's "do I
    /// have a direct link to dst" lookup.
    pub async fn link_to_peer(&self, peer: HostId) -> Option<(LinkId, LinkOutputTx)> {
        self.state
            .read()
            .await
            .writers
            .iter()
            .find(|(_, writer)| writer.host.id == peer)
            .map(|(link, writer)| (*link, writer.tx.clone()))
    }

    /// Whether a live network link already serves this peer directly.
    ///
    /// Link registration precedes routing-table publication. Discovery can
    /// arrive in that interval, so a dial guard that consults routes alone can
    /// open a second connection back to the peer that just connected.
    pub(crate) async fn has_direct_peer_link_to(&self, peer: HostId) -> bool {
        self.state.read().await.writers.values().any(|writer| {
            writer.host.id == peer
                && writer.role == LinkRole::Peer
                && writer.carrier == LinkCarrier::Direct
        })
    }

    /// Whether any live link to `peer` is the authenticated cloud link.
    /// Pairing route selection keys on the link role, never on the peer's
    /// self-asserted capabilities.
    #[cfg(test)]
    pub(crate) async fn link_role(&self, link: &LinkId) -> Option<LinkRole> {
        self.state
            .read()
            .await
            .writers
            .get(link)
            .map(|writer| writer.role)
    }

    pub(crate) async fn has_cloud_relay_link_to(&self, peer: HostId) -> bool {
        self.state
            .read()
            .await
            .writers
            .values()
            .any(|writer| writer.host.id == peer && writer.role == LinkRole::CloudRelay)
    }

    pub(crate) async fn send_link_close_to_all(&self, reason: pb::LinkCloseReason) {
        let outgoing = {
            let state = self.state.read().await;
            state
                .writers
                .values()
                .map(|writer| writer.tx.clone())
                .collect::<Vec<_>>()
        };
        let message = link_close_message(reason);
        for outgoing_tx in outgoing {
            try_send_or_spawn(outgoing_tx, message.clone());
        }
    }
}

/// Enqueues `message` to every writer except links to `skip_peer` (a host is
/// never told about itself). A full queue requests the link's closure; the
/// connect task then removes it through the normal path.
fn fanout(state: &LinkRegistryState, message: pb::Message, skip_peer: HostId) {
    for writer in state.writers.values() {
        if writer.host.id == skip_peer {
            continue;
        }
        try_send_or_request_close(&writer.tx, &writer.close_tx, message.clone());
    }
}

fn try_send_or_request_close(
    tx: &LinkOutputTx,
    close_tx: &mpsc::Sender<LinkCloseRequest>,
    message: pb::Message,
) {
    match tx.try_send(message) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::warn!("link outgoing queue full; requesting link close");
            let _ = close_tx.try_send(LinkCloseRequest::OutgoingQueueFull);
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {}
    }
}

fn link_close_message(reason: pb::LinkCloseReason) -> pb::Message {
    pb::Message {
        body: Some(pb::message::Body::LinkClose(pb::LinkClose {
            reason: reason as i32,
            error: None,
        })),
    }
}

fn try_send_or_spawn(tx: LinkOutputTx, message: pb::Message) {
    match tx.try_send(message) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(message)) => {
            tokio::spawn(async move {
                let _ = tx.send(message).await;
            });
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {}
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use uuid::Uuid;

    use super::*;
    use crate::routing::Capabilities;

    fn host(id: u128) -> Host {
        Host {
            platform: None,
            id: Uuid::from_u128(id),
            name: format!("host-{id}"),
            version: "test".to_string(),
            capabilities: Capabilities {
                cloud_relay: false,
                kinds: Vec::new(),
            },
            signed_in: Some(true),
        }
    }

    fn link(peer: u128, instance: u128) -> LinkId {
        LinkId {
            peer: Uuid::from_u128(peer),
            instance: Uuid::from_u128(instance),
        }
    }

    async fn recv_message(rx: &mut mpsc::Receiver<pb::Message>) -> pb::Message {
        tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for a registry message")
            .expect("registry writer channel closed")
    }

    fn neighbor_up_host_id(message: &pb::Message) -> Vec<u8> {
        let Some(pb::message::Body::NeighborUp(up)) = &message.body else {
            panic!("expected NeighborUp, got {message:?}");
        };
        up.host.as_ref().expect("NeighborUp.host").host_id.clone()
    }

    fn neighbor_down_host_id(message: &pb::Message) -> Vec<u8> {
        let Some(pb::message::Body::NeighborDown(down)) = &message.body else {
            panic!("expected NeighborDown, got {message:?}");
        };
        down.host_id.clone()
    }

    #[tokio::test]
    async fn registering_a_link_announces_the_new_neighbor_to_existing_links() {
        let registry = LinkRegistry::default();
        let (first_tx, mut first_rx) = mpsc::channel(8);
        registry
            .register(link(1, 1), host(1), first_tx, LinkRole::Peer, &[])
            .await;

        let (second_tx, _second_rx) = mpsc::channel(8);
        registry
            .register(link(2, 1), host(2), second_tx, LinkRole::Peer, &[])
            .await;

        assert_eq!(
            neighbor_up_host_id(&recv_message(&mut first_rx).await),
            Uuid::from_u128(2).as_bytes()
        );
        assert!(first_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn registering_a_link_reconciles_its_handshake_snapshot() {
        let registry = LinkRegistry::default();
        let (existing_tx, _existing_rx) = mpsc::channel(8);
        registry
            .register(link(1, 1), host(1), existing_tx, LinkRole::Peer, &[])
            .await;

        // The new link's handshake advertised a now-gone neighbor (3) and
        // missed the existing one (1): registration sends the difference.
        let (new_tx, mut new_rx) = mpsc::channel(8);
        registry
            .register(
                link(2, 1),
                host(2),
                new_tx,
                LinkRole::Peer,
                &[Uuid::from_u128(3)],
            )
            .await;

        let first = recv_message(&mut new_rx).await;
        let second = recv_message(&mut new_rx).await;
        let (up, down) = match &first.body {
            Some(pb::message::Body::NeighborUp(_)) => (first, second),
            _ => (second, first),
        };
        assert_eq!(neighbor_up_host_id(&up), Uuid::from_u128(1).as_bytes());
        assert_eq!(neighbor_down_host_id(&down), Uuid::from_u128(3).as_bytes());
        assert!(new_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn only_the_last_link_to_a_peer_announces_neighbor_down() {
        let registry = LinkRegistry::default();
        let (observer_tx, mut observer_rx) = mpsc::channel(8);
        registry
            .register(link(9, 1), host(9), observer_tx, LinkRole::Peer, &[])
            .await;

        let (first_tx, _first_rx) = mpsc::channel(8);
        let (second_tx, _second_rx) = mpsc::channel(8);
        registry
            .register(link(2, 1), host(2), first_tx, LinkRole::Peer, &[])
            .await;
        registry
            .register(
                link(2, 2),
                host(2),
                second_tx,
                LinkRole::Peer,
                &[Uuid::from_u128(9)],
            )
            .await;
        // One NeighborUp for the peer's first link; the second link is silent.
        assert_eq!(
            neighbor_up_host_id(&recv_message(&mut observer_rx).await),
            Uuid::from_u128(2).as_bytes()
        );
        assert!(observer_rx.try_recv().is_err());

        registry.remove(&link(2, 1)).await;
        assert!(observer_rx.try_recv().is_err(), "one link is still up");

        registry.remove(&link(2, 2)).await;
        assert_eq!(
            neighbor_down_host_id(&recv_message(&mut observer_rx).await),
            Uuid::from_u128(2).as_bytes()
        );
    }

    #[tokio::test]
    async fn crossed_direct_dials_converge_on_the_same_physical_link() {
        let registry = LinkRegistry::default();
        let peer = host(2);
        let fallback = link(2, 1);
        let preferred = link(2, 2);
        let life = crate::routing::RuntimeInstance::random();

        let (fallback_tx, _) = mpsc::channel(8);
        let mut fallback_close = registry
            .register_with_details(
                fallback,
                peer.clone(),
                fallback_tx,
                LinkProperties {
                    role: LinkRole::Peer,
                    admission: LinkAdmission::PinnedKey,
                    carrier: LinkCarrier::Direct,
                    runtime_instance: life,
                    direct_order: Some(DirectLinkOrder::Fallback),
                },
                &[],
                None,
            )
            .await
            .expect("a lone fallback link is usable");

        let (preferred_tx, _) = mpsc::channel(8);
        let registration = registry
            .register_displacing(
                preferred,
                peer.clone(),
                preferred_tx,
                LinkProperties {
                    role: LinkRole::Peer,
                    admission: LinkAdmission::PinnedKey,
                    carrier: LinkCarrier::Direct,
                    runtime_instance: life,
                    direct_order: Some(DirectLinkOrder::Preferred),
                },
                &[],
                None,
            )
            .await
            .expect("a preferred link supersedes a fallback");
        assert_eq!(registration.displaced, vec![fallback]);
        assert_eq!(
            fallback_close.recv().await,
            Some(LinkCloseRequest::Superseded)
        );
        assert_eq!(registry.links_per_peer().await, vec![(peer.id, 1)]);

        let (late_fallback_tx, _) = mpsc::channel(8);
        assert!(
            registry
                .register_with_details(
                    link(2, 3),
                    peer,
                    late_fallback_tx,
                    LinkProperties {
                        role: LinkRole::Peer,
                        admission: LinkAdmission::PinnedKey,
                        carrier: LinkCarrier::Direct,
                        runtime_instance: life,
                        direct_order: Some(DirectLinkOrder::Fallback),
                    },
                    &[],
                    None,
                )
                .await
                .is_none()
        );
        assert_eq!(
            registry.links_per_peer().await,
            vec![(Uuid::from_u128(2), 1)]
        );
    }

    fn direct(order: DirectLinkOrder, runtime_instance: RuntimeInstance) -> LinkProperties {
        LinkProperties {
            role: LinkRole::Peer,
            admission: LinkAdmission::PinnedKey,
            carrier: LinkCarrier::Direct,
            runtime_instance,
            direct_order: Some(order),
        }
    }

    #[tokio::test]
    async fn a_link_from_a_new_runtime_instance_replaces_the_old_one_in_either_direction() {
        use DirectLinkOrder::{Fallback, Preferred};
        for (held, arriving) in [
            (Fallback, Fallback),
            (Fallback, Preferred),
            (Preferred, Fallback),
            (Preferred, Preferred),
        ] {
            let registry = LinkRegistry::default();
            let peer = host(2);
            let dead = link(2, 1);
            let (dead_tx, _) = mpsc::channel(8);
            let mut dead_close = registry
                .register_with_details(
                    dead,
                    peer.clone(),
                    dead_tx,
                    direct(held, RuntimeInstance::random()),
                    &[],
                    None,
                )
                .await
                .expect("a lone link is usable");

            let (restarted_tx, _) = mpsc::channel(8);
            let registration = registry
                .register_displacing(
                    link(2, 2),
                    peer.clone(),
                    restarted_tx,
                    direct(arriving, RuntimeInstance::random()),
                    &[],
                    None,
                )
                .await
                .unwrap_or_else(|| panic!("{arriving:?} from a restart was refused by {held:?}"));
            assert_eq!(registration.displaced, vec![dead]);
            assert_eq!(dead_close.recv().await, Some(LinkCloseRequest::Superseded));
            assert_eq!(registry.links_per_peer().await, vec![(peer.id, 1)]);
        }
    }

    #[tokio::test]
    async fn one_runtime_instance_keeps_its_first_link_in_each_direction() {
        for order in [DirectLinkOrder::Fallback, DirectLinkOrder::Preferred] {
            let registry = LinkRegistry::default();
            let peer = host(2);
            let life = RuntimeInstance::random();
            let (first_tx, _) = mpsc::channel(8);
            registry
                .register_with_details(
                    link(2, 1),
                    peer.clone(),
                    first_tx,
                    direct(order, life),
                    &[],
                    None,
                )
                .await
                .expect("a lone link is usable");

            let (second_tx, _) = mpsc::channel(8);
            assert!(
                registry
                    .register_with_details(
                        link(2, 2),
                        peer.clone(),
                        second_tx,
                        direct(order, life),
                        &[],
                        None
                    )
                    .await
                    .is_none(),
                "a second {order:?} link from the same process was accepted"
            );
            assert_eq!(registry.links_per_peer().await, vec![(peer.id, 1)]);
        }
    }

    #[tokio::test]
    async fn a_neighbor_is_never_told_about_itself() {
        let registry = LinkRegistry::default();
        let (peer_tx, mut peer_rx) = mpsc::channel(8);
        registry
            .register(link(2, 1), host(2), peer_tx, LinkRole::Peer, &[])
            .await;

        // A second link to the same peer: no announcement to the peer.
        let (second_tx, mut second_rx) = mpsc::channel(8);
        registry
            .register(
                link(2, 2),
                host(2),
                second_tx,
                LinkRole::Peer,
                &[Uuid::from_u128(2)],
            )
            .await;

        assert!(peer_rx.try_recv().is_err());
        assert!(second_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn snapshot_lists_each_neighbor_once() {
        let registry = LinkRegistry::default();
        let (first_tx, _first_rx) = mpsc::channel(8);
        let (second_tx, _second_rx) = mpsc::channel(8);
        let (other_tx, _other_rx) = mpsc::channel(8);
        registry
            .register(link(2, 1), host(2), first_tx, LinkRole::Peer, &[])
            .await;
        registry
            .register(link(2, 2), host(2), second_tx, LinkRole::Peer, &[])
            .await;
        registry
            .register(link(3, 1), host(3), other_tx, LinkRole::Peer, &[])
            .await;

        let snapshot = registry.neighbor_snapshot().await;
        assert_eq!(
            snapshot.iter().map(|host| host.id).collect::<Vec<_>>(),
            vec![Uuid::from_u128(2), Uuid::from_u128(3)]
        );
    }

    #[tokio::test]
    async fn full_outgoing_queue_requests_link_close() {
        let registry = LinkRegistry::default();
        let (full_tx, _full_rx) = mpsc::channel(1);
        full_tx.try_send(pb::Message { body: None }).unwrap();
        let mut close_rx = registry
            .register(link(1, 1), host(1), full_tx, LinkRole::Peer, &[])
            .await;

        let (new_tx, _new_rx) = mpsc::channel(8);
        registry
            .register(link(2, 1), host(2), new_tx, LinkRole::Peer, &[])
            .await;

        assert_eq!(
            close_rx.recv().await,
            Some(LinkCloseRequest::OutgoingQueueFull)
        );
    }

    #[tokio::test]
    async fn link_close_notification_is_best_effort_for_full_link_queues() {
        let registry = LinkRegistry::default();
        let (tx, _rx) = mpsc::channel(1);
        tx.try_send(pb::Message { body: None }).unwrap();
        registry
            .register(link(1, 1), host(1), tx, LinkRole::Peer, &[])
            .await;

        tokio::time::timeout(
            Duration::from_millis(50),
            registry.send_link_close_to_all(pb::LinkCloseReason::UserShutdown),
        )
        .await
        .expect("link close send must not wait on a full queue");
    }

    #[tokio::test]
    async fn send_link_close_to_all_sends_typed_reason() {
        let registry = LinkRegistry::default();
        let (tx, mut rx) = mpsc::channel(8);
        registry
            .register(link(99, 1), host(99), tx, LinkRole::Peer, &[])
            .await;

        registry
            .send_link_close_to_all(pb::LinkCloseReason::UserShutdown)
            .await;

        let Some(pb::Message {
            body: Some(pb::message::Body::LinkClose(close)),
        }) = rx.recv().await
        else {
            panic!("expected LinkClose message");
        };
        assert_eq!(close.reason, pb::LinkCloseReason::UserShutdown as i32);
        assert!(close.error.is_none());
    }

    #[tokio::test]
    async fn cloud_admission_tier_updates_without_changing_pinned_links() {
        let registry = LinkRegistry::default();
        let cloud = link(1, 1);
        let pinned = link(2, 1);
        let (cloud_tx, _cloud_rx) = mpsc::channel(8);
        registry
            .register_with_admission(
                cloud,
                host(1),
                cloud_tx,
                LinkRole::CloudRelay,
                LinkAdmission::CloudToken { tier: Tier::Free },
                &[],
            )
            .await;
        let (pinned_tx, _pinned_rx) = mpsc::channel(8);
        registry
            .register(pinned, host(2), pinned_tx, LinkRole::Peer, &[])
            .await;

        registry.update_cloud_tier(&cloud, Tier::Pro).await;
        registry.update_cloud_tier(&pinned, Tier::Free).await;

        assert_eq!(
            registry.admission(&cloud).await,
            Some(LinkAdmission::CloudToken { tier: Tier::Pro })
        );
        assert_eq!(
            registry.admission(&pinned).await,
            Some(LinkAdmission::PinnedKey)
        );
    }

    #[tokio::test]
    async fn cloud_relay_role_is_tracked_per_link() {
        let registry = LinkRegistry::default();
        let (cloud_tx, _cloud_rx) = mpsc::channel(1);
        let (peer_tx, _peer_rx) = mpsc::channel(1);
        registry
            .register(link(1, 1), host(1), cloud_tx, LinkRole::CloudRelay, &[])
            .await;
        registry
            .register(link(2, 1), host(2), peer_tx, LinkRole::Peer, &[])
            .await;

        assert_eq!(
            registry.link_role(&link(1, 1)).await,
            Some(LinkRole::CloudRelay)
        );
        assert_eq!(registry.link_role(&link(2, 1)).await, Some(LinkRole::Peer));
        assert!(registry.has_cloud_relay_link_to(Uuid::from_u128(1)).await);
        assert!(!registry.has_cloud_relay_link_to(Uuid::from_u128(2)).await);
    }
}
