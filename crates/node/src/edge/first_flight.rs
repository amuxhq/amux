//! A launch's first flight: every remembered host is dialled at once, on
//! every path it may be reached by, resuming with the ticket its last link
//! left and carrying as early data the calls the profile's core makes of
//! a host as it begins following it. The host answers them with the
//! Hello. Their answers wait here for those calls, which the core makes
//! once the link is up: each takes its answer rather than asking again.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use daemon_core::payload::PeerRequest;
use tokio::sync::OwnedMutexGuard;
use tokio::time::Instant;

use super::Edge;
use crate::HostId;
use crate::link::link_log::{EarlyData, LinkEvent, LinkTransition};
use crate::link::{EarlyCall, EarlyReply, QuicCarrier};

/// How long an answer the core has not asked for is kept: past it, the
/// core asked differently, or not at all, and the stream is let go.
const UNASKED_PATIENCE: Duration = Duration::from_secs(10);

/// The answers each host's first flight brought, until the core's calls
/// take them.
#[derive(Default)]
pub(crate) struct FirstFlights {
    /// Locked while the host's flight is out, so a call the core makes
    /// meanwhile waits for it rather than asking again.
    hosts: Mutex<HashMap<HostId, Arc<tokio::sync::Mutex<Landed>>>>,
}

#[derive(Default)]
struct Landed {
    answers: Vec<(PeerRequest, EarlyReply)>,
    catch_up: Option<CatchUp>,
}

/// An answer the first flight brought, for the call that asked for it.
pub(crate) struct Taken {
    pub(crate) reply: EarlyReply,
    /// On the inventory's answer: the link log's line to write once its
    /// catch-up arrives.
    pub(crate) catch_up: Option<CatchUp>,
}

/// When a host's first flight left, and the link it brought up.
pub(crate) struct CatchUp {
    own: HostId,
    peer: HostId,
    sent: Instant,
    carrier: Arc<QuicCarrier>,
    early_data: EarlyData,
}

impl CatchUp {
    /// Writes the link log's line: the catch-up arrived, this long after
    /// the first packet left.
    pub(crate) fn arrived(self) {
        let ms = u32::try_from(self.sent.elapsed().as_millis()).unwrap_or(u32::MAX);
        tracing::info!(peer = %self.peer, ms, "a first flight's catch-up arrived");
        LinkEvent {
            peer: self.peer,
            event: LinkTransition::CaughtUp,
            path: self.carrier.path_kind(),
            resumed: self.early_data == EarlyData::Accepted,
            early_data: self.early_data,
            first_packet_to_catch_up_ms: Some(ms),
        }
        .write(self.own);
    }
}

impl FirstFlights {
    /// Marks `host`'s flight as out until the guard is dropped.
    fn begin(&self, host: HostId) -> OwnedMutexGuard<Landed> {
        let landed = Arc::new(tokio::sync::Mutex::new(Landed::default()));
        let guard = landed.clone().try_lock_owned().expect("a new lock is free");
        self.hosts.lock().unwrap().insert(host, landed);
        guard
    }

    /// The answer `host`'s first flight brought to `request`, once the
    /// flight is in, if it carried that call; it is the caller's from then
    /// on.
    pub(crate) async fn take(&self, host: HostId, request: &PeerRequest) -> Option<Taken> {
        let landed = self.hosts.lock().unwrap().get(&host).cloned()?;
        let mut landed = landed.lock().await;
        let at = landed
            .answers
            .iter()
            .position(|(asked, _)| asked == request)?;
        let (_, reply) = landed.answers.remove(at);
        let catch_up = matches!(request, PeerRequest::SubscribeInventory)
            .then(|| landed.catch_up.take())
            .flatten();
        Some(Taken { reply, catch_up })
    }

    /// Lets go of what `host`'s flight brought that no call took.
    fn forget(&self, host: HostId, landed: &Arc<tokio::sync::Mutex<Landed>>) {
        let mut hosts = self.hosts.lock().unwrap();
        if hosts
            .get(&host)
            .is_some_and(|held| Arc::ptr_eq(held, landed))
        {
            hosts.remove(&host);
        }
    }
}

/// The early call a request the core makes of a host is, where it may be
/// one.
fn early_call(request: &PeerRequest) -> Option<EarlyCall> {
    match request {
        PeerRequest::SubscribeInventory => Some(EarlyCall::SubscribeInventory),
        PeerRequest::Subscribe(request) => Some(EarlyCall::Subscribe {
            session: request.agent_id.clone(),
            from: request.from?,
        }),
        _ => None,
    }
}

/// The hosts the profile remembers a way to that a first flight takes:
/// an address, or the relay. A host reached only over SSH is dialled
/// there.
fn remembered(edge: &Edge) -> Vec<HostId> {
    edge.trust
        .read()
        .map(|store| {
            store
                .entries()
                .filter(|(_, entry)| entry.on_account || !entry.direct.is_empty())
                .map(|(host, _)| host)
                .collect()
        })
        .unwrap_or_default()
}

/// Sends the first flight to every host the profile remembers, at once,
/// each with the calls the core will make of it; a host the core has not
/// heard is trusted yet is asked for its inventory.
pub(crate) async fn launch(edge: Weak<Edge>) {
    let Some(runtime) = edge.upgrade().and_then(|edge| edge.runtime.upgrade()) else {
        return;
    };
    let plan = runtime
        .driver()
        .look(|daemon| daemon.first_flight())
        .await
        .unwrap_or_default();
    drop(runtime);
    let mut plan: HashMap<HostId, Vec<PeerRequest>> = plan
        .into_iter()
        .map(|(host, requests)| (crate::routing::host_from_core(host), requests))
        .collect();
    let Some(hosts) = edge.upgrade().map(|edge| remembered(&edge)) else {
        return;
    };
    for host in hosts {
        let requests = plan
            .remove(&host)
            .unwrap_or_else(|| vec![PeerRequest::SubscribeInventory]);
        tokio::spawn(fly(edge.clone(), host, requests));
    }
}

/// Sends `host` its first flight: the link's opening with the calls the
/// core will make of it. Where no link comes up, the host is dialled at
/// its addresses as any other is.
async fn fly(edge: Weak<Edge>, host: HostId, requests: Vec<PeerRequest>) {
    let Some(me) = edge.upgrade() else {
        return;
    };
    let (requests, early): (Vec<PeerRequest>, Vec<EarlyCall>) = requests
        .into_iter()
        .filter_map(|request| Some((early_call(&request)?, request)))
        .map(|(call, request)| (request, call))
        .unzip();
    let mut landed = me.first_flights.begin(host);
    let sent = Instant::now();
    let resumed = me.channels.open_resumed(host, early).await;
    let resumed = match resumed {
        Ok(resumed) => resumed,
        Err(error) => {
            tracing::debug!(%host, %error, "no first flight reached the host");
            drop(landed);
            if !matches!(error, crate::link::ChannelError::Revoked) {
                me.reachability.dial_direct(host);
            }
            return;
        }
    };
    landed.answers = requests.into_iter().zip(resumed.replies).collect();
    landed.catch_up = Some(CatchUp {
        own: me.identity.host_id,
        peer: host,
        sent,
        carrier: resumed.carrier,
        early_data: resumed.early_data,
    });
    let held = OwnedMutexGuard::mutex(&landed).clone();
    drop(landed);
    drop(me);
    tokio::time::sleep(UNASKED_PATIENCE).await;
    if let Some(me) = edge.upgrade() {
        me.first_flights.forget(host, &held);
    }
}
