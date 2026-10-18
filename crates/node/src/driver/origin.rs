//! The streams the core opens of its peers: a replica's Subscribe to its
//! origin and a host's SubscribeInventory. Each runs on a task of its own
//! that reads the stream and hands every event to the core as it comes,
//! then how the stream ended; the core decides everything about what it
//! read. A stream ends when the core hangs up on it.

use std::sync::Weak;

use daemon_core::payload::{Answer, PeerFailure, PeerRequest};
use daemon_core::{CallId, HostId, Input};
use store::AgentKey;
use tokio::sync::mpsc;
use wire::Empty;

use super::{Fed, feed};
use crate::replicas::SourceVerdict;
use crate::routing::host_from_core;
use crate::runtime::ProfileRuntime;

/// Opens the stream `request` asks `host` for and reads it to its end,
/// reporting each event under `call`.
pub(super) async fn stream(
    runtime: Weak<ProfileRuntime>,
    host: HostId,
    call: CallId,
    request: PeerRequest,
    inputs: mpsc::WeakSender<Fed>,
) {
    let answer = |answer| feed(&inputs, Input::PeerAnswer { call, answer });
    let ended = match request {
        PeerRequest::Subscribe(request) => {
            session(&runtime, host, request, |event| {
                answer(Answer::Session(event))
            })
            .await
        }
        PeerRequest::SubscribeInventory => {
            inventory(&runtime, host, |event| answer(Answer::Inventory(event))).await
        }
        other => {
            tracing::error!(?other, "the core asked for a stream of a unary call");
            Answer::Failed(PeerFailure::NotSent)
        }
    };
    answer(ended).await;
}

/// A replica's Subscribe to its origin. The test hook is read as the stream
/// opens.
async fn session<F: Future<Output = ()>>(
    runtime: &Weak<ProfileRuntime>,
    host: HostId,
    request: wire::PeerSubscribeRequest,
    mut answer: impl FnMut(wire::SessionEvent) -> F,
) -> Answer {
    let key = AgentKey::new(host.0.to_vec(), request.agent_id.clone());
    let (edge, hook) = match runtime.upgrade() {
        Some(me) => (me.edge(), me.replicas.lock().unwrap().hook.clone()),
        None => return Answer::Failed(PeerFailure::NotSent),
    };
    let Some(edge) = edge else {
        return Answer::Failed(PeerFailure::NotSent);
    };
    let mut client = match edge.session_peer(host_from_core(host)).await {
        Ok(client) => client,
        Err(_) => return Answer::Failed(PeerFailure::NotSent),
    };
    drop(edge);
    let mut stream = match client.subscribe(request).await {
        Ok(response) => response.into_inner(),
        Err(status) => return refused(&status),
    };
    loop {
        let event = match stream.message().await {
            Ok(Some(event)) => event,
            Ok(None) => return Answer::Ended,
            Err(_) => return Answer::Failed(PeerFailure::Uncertain),
        };
        if let Some(hook) = &hook {
            match hook(&key, &event) {
                SourceVerdict::Keep => {}
                SourceVerdict::Drop => return Answer::Failed(PeerFailure::Uncertain),
                SourceVerdict::Hold(held) => held.await,
            }
        }
        answer(event).await;
    }
}

/// A host's SubscribeInventory. The test hook is read for every event, so
/// one set while the stream is open takes effect on it.
async fn inventory<F: Future<Output = ()>>(
    runtime: &Weak<ProfileRuntime>,
    host: HostId,
    mut answer: impl FnMut(wire::InventoryEvent) -> F,
) -> Answer {
    let peer = host_from_core(host);
    let Some(edge) = runtime.upgrade().and_then(|me| me.edge()) else {
        return Answer::Failed(PeerFailure::NotSent);
    };
    let mut client = match edge.peer(peer).await {
        Ok(client) => client,
        Err(error) => {
            tracing::debug!(%peer, %error, "no channel for the host's inventory");
            return Answer::Failed(PeerFailure::NotSent);
        }
    };
    drop(edge);
    let mut stream = match client.subscribe_inventory(Empty {}).await {
        Ok(response) => response.into_inner(),
        Err(status) => {
            tracing::debug!(%peer, %status, "the host refused its inventory");
            return refused(&status);
        }
    };
    tracing::debug!(%peer, "subscribed to the host's inventory");
    loop {
        let event = match stream.message().await {
            Ok(Some(event)) => event,
            Ok(None) => return Answer::Ended,
            Err(_) => return Answer::Failed(PeerFailure::Uncertain),
        };
        let hook = runtime
            .upgrade()
            .and_then(|me| me.replicas.lock().unwrap().inventory_hook.clone());
        match hook.map(|hook| hook(peer, &event)) {
            None | Some(SourceVerdict::Keep) => {}
            Some(SourceVerdict::Drop) => return Answer::Failed(PeerFailure::Uncertain),
            Some(SourceVerdict::Hold(held)) => held.await,
        }
        answer(event).await;
    }
}

/// A stream the peer would not open: NotFound in its own words, so the core
/// knows the origin does not hold the agent; anything else went wrong on
/// the way.
fn refused(status: &tonic::Status) -> Answer {
    if status.code() == tonic::Code::NotFound {
        Answer::Failed(PeerFailure::Refused(wire::Error {
            code: wire::ErrorCode::NotFound as i32,
            message: status.message().to_owned(),
            details: Vec::new(),
        }))
    } else {
        Answer::Failed(PeerFailure::Uncertain)
    }
}
