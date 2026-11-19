//! The unary calls a daemon makes of a paired daemon for itself, on that
//! host's peer service: a page of history older than its copy of an agent
//! holds, and a child's report to its parent there. The follows are
//! streams, opened by the driver's origin tasks.
//!
//! None carries a caller header: the caller is this daemon, as the link's
//! handshake authenticated it, and a call that names a person or an agent
//! is refused there. So nothing a daemon asks for itself can be mistaken
//! for a call it carries for one of its clients.

use daemon_core::payload::{Answer, PeerFailure, PeerReply, PeerRequest};
use tonic::{Code, Status};
use wire::ErrorCode;
use wire::amux::staging::PeerServiceListItemsRequest;

use crate::HostId;
use crate::reach::{ANSWER_PATIENCE, Unreached};
use crate::runtime::ProfileRuntime;

impl ProfileRuntime {
    /// Makes a unary call the profile's core asked for of `host`'s peer
    /// service: a page, or a report. Reaching the host and waiting for its
    /// answer have separate limits, so a failure says whether the call went
    /// out. A call the peer service has no method for is none the core
    /// makes this way, and never leaves.
    pub(crate) async fn call_peer_service(&self, host: HostId, request: PeerRequest) -> Answer {
        let mut client = match self.reach(host).await {
            Ok(channel) => crate::link::peer_client(channel),
            Err(unreached) => return Answer::Failed(not_sent(unreached)),
        };
        let call = async {
            match request {
                PeerRequest::ListItems(request) => client
                    .peer
                    .list_items(PeerServiceListItemsRequest {
                        agent_id: request.agent_id,
                        before_order: request.before_order,
                        page_size: request.page_size,
                    })
                    .await
                    .map(|page| {
                        let page = page.into_inner();
                        PeerReply::Fetched(wire::ListItemsResponse {
                            items: page.items,
                            exhausted: page.exhausted,
                        })
                    }),
                PeerRequest::DeliverReport(report) => client
                    .peer
                    .deliver_report(report)
                    .await
                    .map(|_| PeerReply::Done),
                other => Err(Status::internal(format!(
                    "the peer service has no unary call for {other:?}"
                ))),
            }
        };
        match tokio::time::timeout(ANSWER_PATIENCE, call).await {
            Ok(Ok(reply)) => Answer::Reply(Box::new(reply)),
            Ok(Err(status)) => Answer::Failed(failure(&status)),
            Err(_) => Answer::Failed(PeerFailure::Uncertain),
        }
    }
}

/// Why a call that found no channel never left.
fn not_sent(unreached: Unreached) -> PeerFailure {
    match unreached {
        Unreached::Lapsed => PeerFailure::Lapsed,
        Unreached::Unreachable | Unreached::Revoked => PeerFailure::NotSent,
    }
}

/// What a status a call that went out ended with says. A daemon refuses
/// in its own words, carried in the details, and those pass through. A
/// transport failure leaves the call uncertain: the host may have acted
/// on it before its answer was lost. A method the host has no handler for
/// is a host on another build.
fn failure(status: &Status) -> PeerFailure {
    if let Some(error) = crate::net_error::from_status(status) {
        return PeerFailure::Refused(error);
    }
    match status.code() {
        Code::Unavailable | Code::DeadlineExceeded | Code::Cancelled | Code::Unknown => {
            PeerFailure::Uncertain
        }
        Code::Unimplemented => PeerFailure::Refused(crate::grpc::wire_error(
            ErrorCode::Unimplemented,
            wire::HOST_NEEDS_UPDATE,
        )),
        code => PeerFailure::Refused(crate::grpc::wire_error(
            ErrorCode::try_from(code as i32).unwrap_or(ErrorCode::Internal),
            status.message(),
        )),
    }
}
