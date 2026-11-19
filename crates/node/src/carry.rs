//! Carrying a call addressed to another host to it, unread, over the
//! link's channel.
//!
//! A client attached to this node addresses a host service to the host
//! that owns what it acts on, by naming that host's id as the call's
//! authority. The router in front of the entry's handlers sends such a call
//! here, and the carrier passes the request as it arrived to the channel
//! this node keeps to that host, and the response back as it arrives:
//! unary calls and streams alike, each message as it is sent, a caller's
//! drop cancelling the call at the owner. It reads no body and knows no
//! service, so a service this build has never heard of is carried too, and
//! the far host answers "unimplemented" if it does not have it.
//!
//! What it adds is who the call is for, in the caller header, from the
//! caller the entry settled (a client cannot say who it is), and the
//! transport's outcomes as statuses that are the same for every service:
//!
//! - never left (no link, not trusted, no channel within the reach limit):
//!   `FAILED_PRECONDITION`, reason `HOST_UNREACHABLE`;
//! - never left, the relay refusing a lapsed plan: `PERMISSION_DENIED`,
//!   reason `PAYMENT_REQUIRED`;
//! - never left, the host no longer trusting this one: `UNAUTHENTICATED`;
//! - left, and no answer began within the answer limit, or the link failed
//!   under it, mid-body included: `ABORTED`, since the owner may have acted.
//!
//! Anything the owner answered passes through untouched.
//!
//! It deals only in plain HTTP requests and responses with streaming
//! bodies: a link failure is told by the response body ending in an error,
//! and nothing here depends on the HTTP version underneath.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll, ready};
use std::time::Instant;

use futures_util::future::BoxFuture;
use http_body::{Frame, SizeHint};
use tonic::Status;
use tonic::body::Body;
use tonic::codegen::http::{self, HeaderMap, HeaderValue};
use tonic::transport::Channel;
use tower::{Service, ServiceExt as _};
use wire::{ErrorCode, ErrorReason};

use crate::HostId;
use crate::api::Caller;
use crate::grpc::{status, wire_error};
use crate::link::link_log::{CarriedCall, CarriedOutcome};
use crate::reach::{ANSWER_PATIENCE, Unreached};
use crate::runtime::ProfileRuntime;

/// How the carrier reaches a host: the channel this node keeps to it, and
/// the name a person knows it by. A profile's runtime is the one there is;
/// the carrier's tests reach a fake.
pub(crate) trait Reach: Send + Sync + 'static {
    /// The channel calls to `host` ride, or why none could be had within
    /// the reach limit.
    fn channel(&self, host: HostId) -> BoxFuture<'_, Result<Channel, Unreached>>;

    /// `host` as a person knows it.
    fn name(&self, host: HostId) -> String;
}

impl Reach for Weak<ProfileRuntime> {
    fn channel(&self, host: HostId) -> BoxFuture<'_, Result<Channel, Unreached>> {
        Box::pin(async move {
            match self.upgrade() {
                Some(runtime) => runtime.reach(host).await,
                None => Err(Unreached::Unreachable),
            }
        })
    }

    fn name(&self, host: HostId) -> String {
        self.upgrade()
            .map_or_else(|| format!("host {host}"), |runtime| runtime.host_name(host))
    }
}

/// A tower service from an HTTP request addressed to another host to that
/// host's response. It is always ready: it waits for its channel inside
/// each call, so a slow host never holds up the local calls that share an
/// entry's connection with it.
#[derive(Clone)]
pub(crate) struct Carrier {
    own: HostId,
    reach: Arc<dyn Reach>,
}

impl Carrier {
    /// The carrier of host `own`, reaching other hosts through `reach`.
    pub(crate) fn new(own: HostId, reach: Arc<dyn Reach>) -> Self {
        Self { own, reach }
    }

    async fn carry(self, mut request: http::Request<Body>) -> http::Response<Body> {
        let started = Instant::now();
        // The router sends only a call addressed to another host's id here.
        let Some(host) = request
            .uri()
            .authority()
            .and_then(|authority| wire::host_of_authority(authority.as_str()))
        else {
            return Status::internal("a carried call names no host").into_http();
        };
        // A client does not say who it is: the header is this node's word,
        // from the entry the call came in on.
        request.headers_mut().remove(wire::CALLER_HEADER);
        let caller = match Caller::settled(request.extensions()) {
            Ok(caller) => caller,
            Err(refusal) => return refusal.into_http(),
        };
        let Some(header) = caller
            .header(self.own)
            .and_then(|header| HeaderValue::from_str(&header).ok())
        else {
            // Only a link's callers belong to another host, and a link's
            // entry has no carrier: a call goes one hop.
            return Status::internal("a paired host's caller is never carried on").into_http();
        };
        request.headers_mut().insert(wire::CALLER_HEADER, header);

        let mut line = Line {
            own: self.own,
            peer: host,
            method: request.uri().path().to_owned(),
            started,
            written: false,
        };
        let name = || self.reach.name(host);
        let channel = match self.reach.channel(host).await {
            Ok(channel) => channel,
            Err(unreached) => {
                let (outcome, refusal) = match unreached {
                    Unreached::Unreachable => (
                        CarriedOutcome::Unreachable,
                        status(wire::Error::because(
                            ErrorReason::HostUnreachable,
                            format!("{} cannot be reached", name()),
                        )),
                    ),
                    Unreached::Lapsed => (
                        CarriedOutcome::PaymentRequired,
                        status(wire::Error::because(
                            ErrorReason::PaymentRequired,
                            lapsed(&name()),
                        )),
                    ),
                    Unreached::Revoked => (
                        CarriedOutcome::Revoked,
                        crate::link::ChannelError::Revoked.status(),
                    ),
                };
                line.end(outcome, refusal.code());
                return refusal.into_http();
            }
        };
        let aborted = status(wire_error(
            ErrorCode::Aborted,
            format!(
                "{} stopped answering; the call may or may not have arrived",
                name()
            ),
        ));
        match tokio::time::timeout(ANSWER_PATIENCE, channel.oneshot(request)).await {
            Ok(Ok(response)) => {
                let (parts, body) = response.into_parts();
                // A refusal with no messages carries its status in the
                // headers, and its body ends with nothing more to read.
                let answered = Status::from_header_map(&parts.headers).map(|status| status.code());
                http::Response::from_parts(
                    parts,
                    Body::new(Carried {
                        inner: body,
                        aborted: Some(aborted),
                        answered,
                        line,
                    }),
                )
            }
            // It went out: whether it arrived before the link failed, or
            // before the limit, nobody here can know.
            Ok(Err(_)) | Err(_) => {
                line.end(CarriedOutcome::Aborted, aborted.code());
                aborted.into_http()
            }
        }
    }
}

impl Service<http::Request<Body>> for Carrier {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<Self::Response, Infallible>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let carrier = self.clone();
        Box::pin(async move { Ok(carrier.carry(request).await) })
    }
}

/// What a call to `name` says when only the relay reaches it and the plan
/// lapsed: said as such, so no surface blames the network.
fn lapsed(name: &str) -> String {
    format!(
        "{name} can be reached only through the relay, and this account's subscription has \
         lapsed to the free plan, which carries no calls between hosts; renew it to reach {name}"
    )
}

/// The link log's line for one carried call, written once, when it ends:
/// at the latest when it is dropped, which is a caller letting go of it.
struct Line {
    own: HostId,
    peer: HostId,
    method: String,
    started: Instant,
    written: bool,
}

impl Line {
    fn end(&mut self, outcome: CarriedOutcome, code: tonic::Code) {
        if std::mem::replace(&mut self.written, true) {
            return;
        }
        CarriedCall {
            peer: self.peer,
            method: std::mem::take(&mut self.method),
            outcome,
            code: code as i32,
            ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
        .write(self.own);
    }
}

impl Drop for Line {
    fn drop(&mut self) {
        self.end(CarriedOutcome::Dropped, tonic::Code::Cancelled);
    }
}

/// The owner's response body as it arrives, ending in `ABORTED` if the
/// link fails under it: the caller is told in the trailers a gRPC response
/// ends with, whatever the transport between here and the caller, and a
/// message cut in half is never read as whole.
struct Carried {
    inner: Body,
    /// The status the body ends with if the link fails; taken once it has.
    aborted: Option<Status>,
    /// The owner's status, when its headers carried it.
    answered: Option<tonic::Code>,
    line: Line,
}

impl http_body::Body for Carried {
    type Data = bytes::Bytes;
    type Error = Status;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let Some(aborted) = &self.aborted else {
            return Poll::Ready(None);
        };
        let aborted_code = aborted.code();
        Poll::Ready(match ready!(Pin::new(&mut self.inner).poll_frame(cx)) {
            Some(Ok(frame)) => {
                if let Some(trailers) = frame.trailers_ref() {
                    let code = Status::from_header_map(trailers)
                        .map_or(tonic::Code::Unknown, |status| status.code());
                    self.line.end(CarriedOutcome::Answered, code);
                }
                Some(Ok(frame))
            }
            Some(Err(_)) => {
                let aborted = self.aborted.take().expect("checked above");
                self.line.end(CarriedOutcome::Aborted, aborted_code);
                let mut trailers = HeaderMap::new();
                if aborted.add_header(&mut trailers).is_err() {
                    trailers.insert("grpc-status", HeaderValue::from(aborted_code as i32));
                }
                Some(Ok(Frame::trailers(trailers)))
            }
            None => {
                let code = self.answered.unwrap_or(tonic::Code::Unknown);
                self.line.end(CarriedOutcome::Answered, code);
                self.aborted = None;
                None
            }
        })
    }

    fn is_end_stream(&self) -> bool {
        self.aborted.is_none() || self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for Carried {
    fn drop(&mut self) {
        // A body nobody read to its end whose headers held the owner's
        // whole answer was answered; any other was let go of, as the line
        // says when it drops.
        if let Some(code) = self.answered {
            self.line.end(CarriedOutcome::Answered, code);
        }
    }
}

#[cfg(test)]
mod tests;
