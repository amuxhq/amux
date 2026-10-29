//! The link registry: this daemon's live links and the wire-side adjacency
//! discipline.
//!
//! The registry holds each live link's writer. Which links stay and what
//! each is told of its neighbors are the forwarding core's decisions
//! (`relay_core`): every link coming up or going is stepped through it
//! under the registry's one lock, and the `NeighborUp`/`NeighborDown`
//! messages it names are sent from here, in the order it named them, so
//! the core's view and the writers held never disagree. A new link is told
//! the difference between its handshake's neighbor snapshot and the
//! neighbors there are in the same step that registers it, which closes the
//! window between composing the snapshot and the link going live.

use std::collections::HashMap;
use std::sync::Arc;

use daemon_core::payload::CarrierKind;
use tokio::sync::{Notify, RwLock, mpsc};
use wire::pb;

use crate::driver::CoreLinks;
use crate::link::QuicCarrier;
use crate::link::link_log::PathKind;
use crate::link::paths::PathDriver;
use crate::link::relaying::Relaying;
use crate::routing::types::{LinkId, core_host, host_from_core};
use crate::routing::wire::{neighbor_down_message, neighbor_up_message};
use crate::routing::{ConnectRole, Host, RuntimeInstance};
use crate::{HostId, Tier};

pub(crate) type LinkOutputTx = mpsc::Sender<pb::Message>;

impl LinkRegistry {
    /// The registry of a host whose own id is `own`: a daemon's links are
    /// all one account's, and it forwards between them on its own.
    pub(crate) fn new(own: HostId) -> Self {
        Self::relayed(Relaying::new(own), relay_core::UserId::default())
    }

    /// A profile's registry, whose links its core hears of.
    pub(crate) fn for_core(own: HostId, core: CoreLinks) -> Self {
        Self {
            core: Some(core),
            ..Self::new(own)
        }
    }

    /// One account's registry on a relay: its links are reported to the
    /// relay's core, which every account shares, under `user`.
    pub(crate) fn relayed(relaying: Arc<Relaying>, user: relay_core::UserId) -> Self {
        Self {
            state: RwLock::default(),
            core: None,
            relaying,
            user,
            refusals: tokio::sync::broadcast::channel(16).0,
            last_refusals: std::sync::Mutex::default(),
            paths: std::sync::OnceLock::new(),
        }
    }

    /// Tells `paths` of every peer link from now on.
    pub(crate) fn drive_paths(&self, paths: &Arc<PathDriver>) {
        let _ = self.paths.set(Arc::downgrade(paths));
    }

    /// The profile's path machines, when this is a profile's registry.
    pub(crate) fn paths(&self) -> Option<Arc<PathDriver>> {
        self.paths.get()?.upgrade()
    }

    /// A peer link came up: its peer's path machine chooses its path from
    /// now on.
    fn path_up(
        &self,
        link: &LinkId,
        host: HostId,
        role: LinkRole,
        carrier: Option<&Arc<QuicCarrier>>,
    ) {
        if let (LinkRole::Peer, Some(carrier), Some(paths)) = (role, carrier, self.paths()) {
            paths.link_up(link.core(), host, carrier.as_ref());
        }
    }

    /// The core that decides which links forward to each other.
    pub(crate) fn relaying(&self) -> &Arc<Relaying> {
        &self.relaying
    }

    fn report_down(&self, state: &LinkRegistryState, link: &LinkId) {
        let out = self
            .relaying
            .step([relay_core::Input::LinkDown { link: link.core() }]);
        tell_links(state, out, None);
    }

    /// The profile's core, when this is a profile's registry.
    pub(crate) fn core(&self) -> Option<&CoreLinks> {
        self.core.as_ref()
    }
}

pub struct LinkRegistry {
    state: RwLock<LinkRegistryState>,
    /// The profile's core, told of every link registered here and asked
    /// for every channel opened over one. A relay's registry has none.
    core: Option<CoreLinks>,
    /// The forwarding core, told of every link registered here and asked
    /// for every stream a link opens toward another host.
    relaying: Arc<Relaying>,
    /// The account these links are on, as the forwarding core knows it.
    user: relay_core::UserId,
    refusals: tokio::sync::broadcast::Sender<(HostId, pb::ForwardRefusal)>,
    last_refusals: std::sync::Mutex<HashMap<HostId, pb::ForwardRefusal>>,
    /// The profile's path machines, told of every peer link that comes up
    /// or goes on the profile's socket.
    paths: std::sync::OnceLock<std::sync::Weak<PathDriver>>,
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
    pub(crate) carrier: CarrierKind,
    /// The life of the peer's runtime this link was opened by.
    pub(crate) runtime_instance: RuntimeInstance,
    pub(crate) direct_order: Option<DirectLinkOrder>,
}

/// Which of a crossed dial's two links both peers keep: the forwarding
/// core's to decide.
pub(crate) type DirectLinkOrder = relay_core::DialOrder;

/// The order of a direct link `local` holds to `peer`, in the role it
/// played opening it.
pub(crate) fn direct_order(local: HostId, peer: HostId, role: ConnectRole) -> DirectLinkOrder {
    relay_core::DialOrder::for_hosts(
        core_host(local),
        core_host(peer),
        role == ConnectRole::Connector,
    )
}

#[derive(Clone)]
struct LinkWriter {
    host: Host,
    tx: LinkOutputTx,
    close_tx: mpsc::Sender<LinkCloseRequest>,
    closed: Arc<Notify>,
    role: LinkRole,
    admission: LinkAdmission,
    carrier: CarrierKind,
    native_carrier: Option<Arc<QuicCarrier>>,
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
    /// The link it was carried through, to a forwarder, went.
    ForwarderGone,
    /// The peer said on another link that it no longer trusts this host.
    PeerRevoked,
    /// A call could not open a channel on the link while it stayed up: it
    /// closes as a dead connection would, and the host reads offline until
    /// a link that replaces it comes up.
    CallsFailed,
}

/// A link the registry accepted.
pub(crate) struct Registration {
    /// Receives the registry's requests to close this link.
    pub(crate) close_rx: mpsc::Receiver<LinkCloseRequest>,
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
                carrier: CarrierKind::Direct,
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
                carrier: CarrierKind::Direct,
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
        native_carrier: Option<Arc<QuicCarrier>>,
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
        native_carrier: Option<Arc<QuicCarrier>>,
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
        let carrier_for_paths = native_carrier.clone();
        if carrier == CarrierKind::Forwarded {
            self.last_refusals
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&host.id);
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
                    native_carrier,
                },
            );
            drop(state);
            if let Some(old) = old {
                old.closed.notify_waiters();
            }
            self.path_up(&link, host.id, role, carrier_for_paths.as_ref());
            return Some(Registration { close_rx });
        }
        // The forwarding core settles which of two direct links to one peer
        // stays and what every link is told of its neighbors; it is asked
        // under the lock, so what it says and the writers held agree.
        let direct = match (role, carrier, direct_order) {
            (LinkRole::Peer, CarrierKind::Direct, Some(order)) => Some(order),
            _ => None,
        };
        let out = self.relaying.step([relay_core::Input::LinkUp {
            link: link.core(),
            user: self.user,
            host: core_host(host.id),
            tier: match admission {
                LinkAdmission::PinnedKey => None,
                LinkAdmission::CloudToken { tier } => Some(tier),
            },
            instance: runtime_instance.bytes(),
            direct,
            advertised: advertised_snapshot.iter().copied().map(core_host).collect(),
        }]);
        if out
            .iter()
            .any(|output| matches!(output, relay_core::Output::RefuseLink { .. }))
        {
            return None;
        }
        let mut displaced = Vec::new();
        for output in &out {
            if let relay_core::Output::Displace { link: gone } = output
                && let Some(id) = state.writers.keys().find(|id| id.core() == *gone).copied()
                && let Some(writer) = state.writers.remove(&id)
            {
                displaced.push((id, writer));
            }
        }
        // A link admitted by a token is a relay's link to a host, which
        // forwards for the host rather than through it.
        let through = match (role, admission) {
            (LinkRole::CloudRelay, LinkAdmission::PinnedKey) => Some(
                native_carrier
                    .as_ref()
                    .map_or(PathKind::RelayUdp, |carrier| carrier.path_kind()),
            ),
            (LinkRole::Peer, LinkAdmission::PinnedKey) => Some(PathKind::ViaDaemon),
            (_, LinkAdmission::CloudToken { .. }) => None,
        };
        // Packets are forwarded through a link on the profile's own socket:
        // the host's link to the relay, over UDP or TCP, and a pinned link
        // to a paired daemon. A link over a byte stream of its own has no
        // stand-in there, and is passed over.
        if let Some(carrier) = &native_carrier {
            self.relaying.admitted(
                link.core(),
                outgoing_tx.clone(),
                host.id,
                carrier.clone(),
                through,
            );
        }
        let new = NewLink {
            link: link.core(),
            host: &host,
            tx: &outgoing_tx,
            close_tx: &close_tx,
        };
        tell_links(&state, out, Some(new));

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
                native_carrier,
            },
        );
        drop(state);
        for (_, writer) in displaced {
            let _ = writer.close_tx.try_send(LinkCloseRequest::Superseded);
            writer.closed.notify_waiters();
        }
        if let Some(old) = old {
            old.closed.notify_waiters();
        }
        self.path_up(&link, host.id, role, carrier_for_paths.as_ref());
        Some(Registration { close_rx })
    }

    /// Removes a link; if it was the last link to its peer, other links
    /// learn `NeighborDown(peer)`. The links carried through it close: a
    /// link through a forwarder has no other path.
    pub(crate) async fn remove(&self, link: &LinkId) {
        let mut state = self.state.write().await;
        let Some(writer) = state.writers.remove(link) else {
            return;
        };
        if writer.carrier != CarrierKind::Forwarded {
            self.report_down(&state, link);
        }
        for carried in state.writers.values().filter(|carried| {
            carried
                .native_carrier
                .as_ref()
                .and_then(|carrier| carrier.forwarded_through())
                == Some(link.core())
        }) {
            let _ = carried.close_tx.try_send(LinkCloseRequest::ForwarderGone);
        }
        drop(state);
        if let Some(paths) = self.paths() {
            paths.link_down(link.core());
        }
        writer.closed.notify_waiters();
    }

    /// `host` said on `told` that it no longer trusts this host: its other
    /// links are no use, so they leave the registry at once, before anyone
    /// is told and a call picks one, and are asked to close.
    pub(crate) async fn forsake(&self, host: HostId, told: LinkId) {
        let others: Vec<LinkId> = {
            let state = self.state.read().await;
            state
                .writers
                .iter()
                .filter(|(link, writer)| writer.host.id == host && **link != told)
                .map(|(link, writer)| {
                    let _ = writer.close_tx.try_send(LinkCloseRequest::PeerRevoked);
                    *link
                })
                .collect()
        };
        for link in others {
            self.remove(&link).await;
        }
    }

    /// The current neighbor set, as a handshake snapshot.
    pub(crate) async fn neighbor_snapshot(&self) -> Vec<Host> {
        let state = self.state.read().await;
        self.relaying
            .neighbors(self.user)
            .into_iter()
            .filter_map(|host| state.host(host).cloned())
            .collect()
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
        self.close_where(|_, writer| writer.host.id == host_id, request)
            .await
    }

    /// Closes every link to `host_id` made before the instance `before`
    /// and waits until they are gone; a link made since stays.
    pub(crate) async fn close_host_before(
        &self,
        host_id: HostId,
        before: u64,
        request: LinkCloseRequest,
    ) -> Vec<LinkId> {
        self.close_where(
            |link, writer| writer.host.id == host_id && link.instance < before,
            request,
        )
        .await
    }

    /// Requests closure of `link`, for `request`, and waits until it is
    /// gone from the registry.
    pub(crate) async fn close_link(&self, link: LinkId, request: LinkCloseRequest) {
        self.close_where(|id, _| *id == link, request).await;
    }

    /// Closes every link to `host` carried through `through`, whose
    /// forwarder refused to carry it, and waits until they are gone;
    /// whoever dials one now is told. A link to the host through another
    /// forwarder stays: the relay refusing for the plan says nothing of a
    /// paired daemon.
    pub(crate) async fn close_forwarded(
        &self,
        host: HostId,
        through: daemon_core::LinkId,
        reason: pb::ForwardRefusal,
    ) {
        self.last_refusals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(host, reason);
        let _ = self.refusals.send((host, reason));
        self.close_where(
            |_, writer| {
                writer.host.id == host
                    && writer
                        .native_carrier
                        .as_ref()
                        .and_then(|carrier| carrier.forwarded_through())
                        == Some(through)
            },
            LinkCloseRequest::Superseded,
        )
        .await;
    }

    /// Why a forwarder last refused to carry links to `host`, until a link
    /// to it through one comes up. A forwarder repeats a refusal only every
    /// few seconds, so a dial in between hears nothing and this is what it
    /// was refused for.
    pub(crate) fn last_refusal(&self, host: HostId) -> Option<pb::ForwardRefusal> {
        self.last_refusals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&host)
            .copied()
    }

    /// The refusals forwarders send from now on, by the host refused.
    pub(crate) fn forward_refusals(
        &self,
    ) -> tokio::sync::broadcast::Receiver<(HostId, pb::ForwardRefusal)> {
        self.refusals.subscribe()
    }

    async fn close_where(
        &self,
        closes: impl Fn(&LinkId, &LinkWriter) -> bool,
        request: LinkCloseRequest,
    ) -> Vec<LinkId> {
        let closing = {
            let state = self.state.read().await;
            state
                .writers
                .iter()
                .filter(|(link, writer)| closes(link, writer))
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

    #[cfg(test)]
    pub(crate) async fn admission(&self, link: &LinkId) -> Option<LinkAdmission> {
        self.state
            .read()
            .await
            .writers
            .get(link)
            .map(|writer| writer.admission)
    }

    pub(crate) async fn native_carrier(&self, link: &LinkId) -> Option<Arc<QuicCarrier>> {
        self.state
            .read()
            .await
            .writers
            .get(link)
            .and_then(|writer| writer.native_carrier.clone())
    }

    pub async fn native_carrier_to_peer(&self, peer: HostId) -> Option<(LinkId, Arc<QuicCarrier>)> {
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

    /// The link `carrier` carries, while it is registered.
    pub(crate) async fn link_carried_by(&self, carrier: &Arc<QuicCarrier>) -> Option<LinkId> {
        self.state
            .read()
            .await
            .writers
            .iter()
            .find(|(_, writer)| {
                writer
                    .native_carrier
                    .as_ref()
                    .is_some_and(|held| Arc::ptr_eq(held, carrier))
            })
            .map(|(link, _)| *link)
    }

    pub(crate) async fn update_cloud_tier(&self, link: &LinkId, tier: Tier) {
        let mut state = self.state.write().await;
        let Some(writer) = state.writers.get_mut(link) else {
            return;
        };
        if matches!(writer.admission, LinkAdmission::CloudToken { .. }) {
            writer.admission = LinkAdmission::CloudToken { tier };
            let out = self.relaying.step([relay_core::Input::TierChanged {
                link: link.core(),
                tier,
            }]);
            tell_links(&state, out, None);
        }
    }

    /// A token link's token ran out before a fresh one came: nothing is
    /// forwarded for it from now on.
    pub(crate) async fn expire(&self, link: &LinkId) {
        let state = self.state.read().await;
        if let Some(LinkWriter {
            admission: LinkAdmission::CloudToken { .. },
            ..
        }) = state.writers.get(link)
        {
            self.relaying
                .step([relay_core::Input::Expired { link: link.core() }]);
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
                && writer.carrier == CarrierKind::Direct
        })
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

/// A link the core has been told of that is not among the writers yet.
struct NewLink<'a> {
    link: daemon_core::LinkId,
    host: &'a Host,
    tx: &'a LinkOutputTx,
    close_tx: &'a mpsc::Sender<LinkCloseRequest>,
}

/// Sends each neighbor the core told of, and each request for a fresh
/// credential, down the link it named. A full queue requests the link's
/// closure; the connect task then removes it through the normal path.
fn tell_links(state: &LinkRegistryState, out: Vec<relay_core::Output>, new: Option<NewLink<'_>>) {
    for output in out {
        let (to, host, up) = match output {
            relay_core::Output::Neighbor { to, host, up } => (to, host, up),
            relay_core::Output::RefreshCredential { link } => {
                send_to(state, new.as_ref(), link, refresh_credential_message());
                continue;
            }
            _ => continue,
        };
        let host = host_from_core(host);
        let message = if up {
            let described = match &new {
                Some(new) if new.host.id == host => Some(new.host),
                _ => state.host(core_host(host)),
            };
            let Some(described) = described else {
                continue;
            };
            neighbor_up_message(described)
        } else {
            neighbor_down_message(host)
        };
        send_to(state, new.as_ref(), to, message);
    }
}

/// Writes `message` on the link the core named: the link coming up, which
/// is not yet among the writers, or one already held.
fn send_to(
    state: &LinkRegistryState,
    new: Option<&NewLink<'_>>,
    to: daemon_core::LinkId,
    message: pb::Message,
) {
    match new {
        Some(new) if new.link == to => {
            try_send_or_request_close(new.tx, new.close_tx, message);
        }
        _ => {
            if let Some(writer) = state.writers.iter().find(|(id, _)| id.core() == to) {
                try_send_or_request_close(&writer.1.tx, &writer.1.close_tx, message);
            }
        }
    }
}

fn refresh_credential_message() -> pb::Message {
    pb::Message {
        body: Some(pb::message::Body::RefreshCredential(
            pb::RefreshCredential {},
        )),
    }
}

impl LinkRegistryState {
    /// What `host` said of itself on a link held to it.
    fn host(&self, host: daemon_core::HostId) -> Option<&Host> {
        let host = host_from_core(host);
        self.writers
            .values()
            .find(|writer| writer.host.id == host)
            .map(|writer| &writer.host)
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
            platform: wire::Platform::Unspecified,
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

    /// The link `instance` to `peer`. Instances are unique within the
    /// process, as the cores name links by them alone.
    fn link(peer: u128, instance: u128) -> LinkId {
        LinkId {
            peer: Uuid::from_u128(peer),
            instance: u64::try_from(peer * 1_000 + instance).expect("a test link's instance fits"),
        }
    }

    fn direct(order: DirectLinkOrder, runtime_instance: RuntimeInstance) -> LinkProperties {
        LinkProperties {
            role: LinkRole::Peer,
            admission: LinkAdmission::PinnedKey,
            carrier: CarrierKind::Direct,
            runtime_instance,
            direct_order: Some(order),
        }
    }

    /// A link to a host carried through a forwarder: one end of a QUIC
    /// pair, marked as carried through `through`.
    async fn carried_through(through: LinkId) -> Arc<QuicCarrier> {
        let (near, _far) =
            crate::link::quic::stream_pair(crate::link::framed::FramedKind::InProcess).await;
        let near = Arc::try_unwrap(near).unwrap_or_else(|_| panic!("one holder"));
        Arc::new(near.carried_through(Some(through.core())))
    }

    /// The relay refusing to carry a host (its plan lapsed) closes the link
    /// to the host the relay carried, and leaves the one through a paired
    /// daemon up: only the forwarder that refused stopped carrying it.
    #[tokio::test]
    async fn link_registry_relay_refusal_leaves_a_via_daemon_link_up() {
        let registry = Arc::new(LinkRegistry::new(HostId::nil()));
        let phone = host(3);
        let (relay, laptop) = (link(1, 1), link(2, 1));
        let forwarded = || LinkProperties {
            role: LinkRole::Peer,
            admission: LinkAdmission::PinnedKey,
            carrier: CarrierKind::Forwarded,
            runtime_instance: RuntimeInstance::random(),
            direct_order: None,
        };
        let mut closes = Vec::new();
        for (instance, through) in [(1, relay), (2, laptop)] {
            let (tx, _) = mpsc::channel(8);
            closes.push(
                registry
                    .register_with_details(
                        link(3, instance),
                        phone.clone(),
                        tx,
                        forwarded(),
                        &[],
                        Some(carried_through(through).await),
                    )
                    .await
                    .expect("a forwarded link is usable"),
            );
        }
        let mut via_daemon = closes.pop().unwrap();
        let mut via_relay = closes.pop().unwrap();
        // The link's run loop leaves the registry when asked to close.
        let closer = tokio::spawn({
            let registry = registry.clone();
            async move {
                let request = via_relay.recv().await;
                registry.remove(&link(3, 1)).await;
                request
            }
        });
        tokio::time::timeout(
            Duration::from_secs(10),
            registry.close_forwarded(phone.id, relay.core(), pb::ForwardRefusal::PaymentRequired),
        )
        .await
        .expect("the link through the relay closes");
        assert_eq!(closer.await.unwrap(), Some(LinkCloseRequest::Superseded));
        assert!(
            via_daemon.try_recv().is_err(),
            "the via-daemon link was not asked to close"
        );
        assert_eq!(registry.links_per_peer().await, vec![(phone.id, 1)]);
        assert!(registry.native_carrier(&link(3, 2)).await.is_some());
    }

    /// A call that timed out closes the links to the host it had to use,
    /// and leaves one that came up while it waited.
    #[tokio::test]
    async fn a_timed_out_call_closes_the_links_made_before_it_began() {
        let registry = Arc::new(LinkRegistry::new(HostId::nil()));
        let phone = host(3);
        let forwarded = || LinkProperties {
            role: LinkRole::Peer,
            admission: LinkAdmission::PinnedKey,
            carrier: CarrierKind::Forwarded,
            runtime_instance: RuntimeInstance::random(),
            direct_order: None,
        };
        let mut closes = Vec::new();
        for instance in [1, 2] {
            let (tx, _) = mpsc::channel(8);
            closes.push(
                registry
                    .register_with_details(
                        link(3, instance),
                        phone.clone(),
                        tx,
                        forwarded(),
                        &[],
                        Some(carried_through(link(1, instance)).await),
                    )
                    .await
                    .expect("a forwarded link is usable"),
            );
        }
        let mut newer = closes.pop().unwrap();
        let mut older = closes.pop().unwrap();
        let closer = tokio::spawn({
            let registry = registry.clone();
            async move {
                let request = older.recv().await;
                registry.remove(&link(3, 1)).await;
                request
            }
        });
        let closed = tokio::time::timeout(
            Duration::from_secs(10),
            registry.close_host_before(
                phone.id,
                link(3, 2).instance,
                LinkCloseRequest::CallsFailed,
            ),
        )
        .await
        .expect("the older link closes");
        assert_eq!(closed, vec![link(3, 1)]);
        assert_eq!(closer.await.unwrap(), Some(LinkCloseRequest::CallsFailed));
        assert!(
            newer.try_recv().is_err(),
            "the newer link was not asked to close"
        );
        assert_eq!(registry.links_per_peer().await, vec![(phone.id, 1)]);
    }

    #[tokio::test]
    async fn a_link_from_a_new_runtime_instance_replaces_the_old_one_in_either_direction() {
        use relay_core::DialOrder::{Fallback, Preferred};
        for (held, arriving) in [
            (Fallback, Fallback),
            (Fallback, Preferred),
            (Preferred, Fallback),
            (Preferred, Preferred),
        ] {
            let registry = LinkRegistry::new(HostId::nil());
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
            let _registration = registry
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
            assert_eq!(dead_close.recv().await, Some(LinkCloseRequest::Superseded));
            assert_eq!(registry.links_per_peer().await, vec![(peer.id, 1)]);
        }
    }

    #[tokio::test]
    async fn one_runtime_instance_keeps_its_first_link_in_each_direction() {
        for order in [DirectLinkOrder::Fallback, DirectLinkOrder::Preferred] {
            let registry = LinkRegistry::new(HostId::nil());
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
    async fn snapshot_lists_each_neighbor_once() {
        let registry = LinkRegistry::new(HostId::nil());
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
        let registry = LinkRegistry::new(HostId::nil());
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
        let registry = LinkRegistry::new(HostId::nil());
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
        let registry = LinkRegistry::new(HostId::nil());
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
        let registry = LinkRegistry::new(HostId::nil());
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
}
