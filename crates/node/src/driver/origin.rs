//! The streams the core opens of its peers, on their peer service: a
//! replica's FollowSession to its origin and a host's FollowAgents. Each
//! runs on a task of its own
//! that reads the stream and hands every event to the core as it comes,
//! then how the stream ended; the core decides everything about what it
//! read. A stream ends when the core hangs up on it.

use std::sync::Weak;

use daemon_core::payload::{Answer, PeerFailure, PeerRequest};
use daemon_core::{CallId, HostId, Input};
use wire::amux::staging::{FollowAgentsRequest, FollowSessionRequest};

use super::feed;
use crate::link::EarlyAnswer;
use crate::routing::host_from_core;
use crate::runtime::ProfileRuntime;

/// Opens the stream `request` asks `host` for and reads it to its end,
/// reporting each event under `call`.
pub(super) async fn stream(
    runtime: Weak<ProfileRuntime>,
    host: HostId,
    call: CallId,
    request: PeerRequest,
    inputs: super::WeakFeed,
) {
    let answer = |answer| feed(&inputs, Input::PeerAnswer { call, answer });
    let ended = match request {
        PeerRequest::FollowSession(request) => {
            session(&runtime, host, request, |event| {
                answer(Answer::Session(event))
            })
            .await
        }
        PeerRequest::FollowAgents => {
            inventory(&runtime, host, |event| answer(Answer::Inventory(event))).await
        }
        other => {
            tracing::error!(?other, "the core asked for a stream of a unary call");
            Answer::Failed(PeerFailure::NotSent)
        }
    };
    answer(ended).await;
}

/// A replica's FollowSession to its origin.
async fn session<F: Future<Output = ()>>(
    runtime: &Weak<ProfileRuntime>,
    host: HostId,
    request: FollowSessionRequest,
    mut answer: impl FnMut(wire::SessionEvent) -> F,
) -> Answer {
    let Some(edge) = runtime.upgrade().and_then(|me| me.edge()) else {
        return Answer::Failed(PeerFailure::NotSent);
    };
    let peer = host_from_core(host);
    let early = edge
        .first_flights()
        .take(peer, &PeerRequest::FollowSession(request.clone()))
        .await;
    let opened = match early {
        // The launch's first flight carried this call.
        Some(taken) => match taken.reply.await {
            Ok(EarlyAnswer::Session(stream)) => Ok(stream),
            Ok(_) => Err(tonic::Status::internal(
                "a FollowSession answered otherwise",
            )),
            Err(status) => Err(status),
        },
        None => {
            let mut client = match edge.session_peer(peer).await {
                Ok(client) => client,
                Err(_) => return Answer::Failed(PeerFailure::NotSent),
            };
            drop(edge);
            client
                .peer
                .follow_session(request)
                .await
                .map(|response| response.into_inner())
        }
    };
    let mut stream = match opened {
        Ok(stream) => stream,
        Err(status) => return refused(&status),
    };
    loop {
        match stream.message().await {
            Ok(Some(response)) => {
                if let Some(event) = response.event {
                    answer(event).await;
                }
            }
            Ok(None) => return Answer::Ended,
            Err(_) => return Answer::Failed(PeerFailure::Uncertain),
        }
    }
}

/// A host's FollowAgents.
async fn inventory<F: Future<Output = ()>>(
    runtime: &Weak<ProfileRuntime>,
    host: HostId,
    mut answer: impl FnMut(wire::InventoryEvent) -> F,
) -> Answer {
    let peer = host_from_core(host);
    let Some(edge) = runtime.upgrade().and_then(|me| me.edge()) else {
        return Answer::Failed(PeerFailure::NotSent);
    };
    let early = edge
        .first_flights()
        .take(peer, &PeerRequest::FollowAgents)
        .await;
    let (opened, mut catch_up) = match early {
        // The launch's first flight carried this call.
        Some(taken) => (
            match taken.reply.await {
                Ok(EarlyAnswer::Agents(stream)) => Ok(stream),
                Ok(_) => Err(tonic::Status::internal("a FollowAgents answered otherwise")),
                Err(status) => Err(status),
            },
            taken.catch_up,
        ),
        None => {
            let mut client = match edge.peer(peer).await {
                Ok(client) => client,
                Err(error) => {
                    tracing::debug!(%peer, %error, "no channel for the host's inventory");
                    return Answer::Failed(PeerFailure::NotSent);
                }
            };
            drop(edge);
            (
                client
                    .peer
                    .follow_agents(FollowAgentsRequest {})
                    .await
                    .map(|response| response.into_inner()),
                None,
            )
        }
    };
    let mut stream = match opened {
        Ok(stream) => stream,
        Err(status) => {
            tracing::debug!(%peer, %status, "the host refused its inventory");
            return refused(&status);
        }
    };
    tracing::debug!(%peer, "subscribed to the host's inventory");
    loop {
        let event = match stream.message().await {
            Ok(Some(response)) => response.event,
            Ok(None) => return Answer::Ended,
            Err(_) => return Answer::Failed(PeerFailure::Uncertain),
        };
        let Some(event) = event else { continue };
        if matches!(event.of, Some(wire::inventory_event::Of::CaughtUp(_)))
            && let Some(caught_up) = catch_up.take()
        {
            caught_up.arrived();
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
