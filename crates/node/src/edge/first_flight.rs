//! A dial's first flight carries as early data the calls the profile's
//! core makes of a host as it begins following it, and the host answers
//! them with the Hello. Their answers wait here for those calls, which the
//! core makes once the link is up: each takes its answer rather than
//! asking again.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use daemon_core::payload::PeerRequest;
use tokio::sync::OwnedMutexGuard;
use tokio::time::Instant;

use crate::HostId;
use crate::link::link_log::{EarlyData, LinkEvent, LinkTransition};
use crate::link::{EarlyReply, QuicCarrier, ResumedLink};

/// How long an answer the core has not asked for is kept: past it, the
/// core asked differently, or not at all, and the stream is let go.
const UNASKED_PATIENCE: Duration = Duration::from_secs(10);

/// The answers each host's last first flight brought, until the core's
/// calls take them.
#[derive(Default)]
pub(crate) struct FirstFlights {
    /// Locked while the host's flight is out, so a call the core makes
    /// meanwhile waits for it rather than asking again.
    hosts: Mutex<HashMap<HostId, Arc<tokio::sync::Mutex<Landed>>>>,
}

#[derive(Default)]
pub(crate) struct Landed {
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
    pub(crate) fn begin(&self, host: HostId) -> OwnedMutexGuard<Landed> {
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

    /// `host`'s flight, begun at `sent` with `landed`, brought `resumed` up
    /// carrying `requests`: their answers wait for the core's calls for a
    /// while, and are let go after it.
    pub(crate) fn landed(
        self: &Arc<Self>,
        mut landed: OwnedMutexGuard<Landed>,
        own: HostId,
        host: HostId,
        sent: Instant,
        requests: Vec<PeerRequest>,
        resumed: ResumedLink,
    ) {
        landed.answers = requests.into_iter().zip(resumed.replies).collect();
        landed.catch_up = Some(CatchUp {
            own,
            peer: host,
            sent,
            carrier: resumed.carrier,
            early_data: resumed.early_data,
        });
        let held = OwnedMutexGuard::mutex(&landed).clone();
        drop(landed);
        let flights = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(UNASKED_PATIENCE).await;
            if let Some(flights) = flights.upgrade() {
                flights.forget(host, &held);
            }
        });
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
