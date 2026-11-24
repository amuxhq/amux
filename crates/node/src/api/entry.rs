//! Where a call enters this node, which settles who may be calling.
//!
//! Every entry's services sit behind one layer that settles the caller
//! from the entry and puts it in the request for the handlers (and the
//! carrier) to read, so every entry's rule for who is calling lives here
//! and nowhere else. A client's entry also has a router in front of its
//! handlers: a call whose authority is another host's id is carried to
//! that host, and anything else is answered here. A link's entry has no
//! router, so a call that arrives from a paired host is answered here or
//! not at all: a call goes one hop, with no code that could pass it on.

use std::convert::Infallible;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};

use futures_util::future::BoxFuture;
use tonic::body::Body;
use tonic::codegen::http;
use tonic::service::Routes;
use tonic::{Code, Status};
use tower::steer::Steer;
use tower::util::BoxCloneService;
use tower::{Layer, Service, ServiceExt as _};

use super::caller::{Caller, PairedDaemon, Surface, Unsettled, link_peer, surface_named};
use crate::carry::Carrier;
use crate::grpc::{status, upgrade, wire_error};
use crate::runtime::ProfileRuntime;
use crate::{AgentId, HostId};

/// A way into this node's services. Each is served its own set of them,
/// and its calls are answered for the caller it implies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    /// The profile's socket: a person at this machine, on whichever client
    /// names itself in its calls. Reaching the socket is the authority.
    Profile,
    /// An agent's tools socket, bound for that agent alone: the agent is
    /// the caller of everything that arrives on it.
    Tools(AgentId),
    /// The phone's runtime, called in memory by the app it runs in: a
    /// person on the iPhone app, the only app that runs a node in itself.
    Phone,
    /// The streams of links whose handshake presented a trusted key. The
    /// paired host is each connection's own, and the caller header says
    /// whom, if anyone, a call is for.
    Link,
}

/// An entry's services as one tower service from a request to a response:
/// what a socket serves, and what the phone calls in memory with the same
/// generated stubs a socket's client uses.
pub(crate) type Entrance = BoxCloneService<http::Request<Body>, http::Response<Body>, Infallible>;

/// The layer `entry`'s services are served behind, for the profile
/// `runtime` runs: the caller settled, and on a client's entry the router
/// and the carrier.
pub(crate) fn layer(
    runtime: &Weak<ProfileRuntime>,
    entry: Entry,
) -> impl Layer<Routes, Service = Entrance> + Clone + use<> {
    let runtime = runtime.clone();
    tower::layer::layer_fn(move |handlers: Routes| entrance(&runtime, entry, handlers))
}

/// `handlers` as `entry` serves them: see [`layer`].
pub(crate) fn entrance<S>(runtime: &Weak<ProfileRuntime>, entry: Entry, handlers: S) -> Entrance
where
    S: Service<http::Request<Body>, Response = http::Response<Body>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    match entry {
        Entry::Link => settled(runtime, entry, BoxCloneService::new(handlers)),
        Entry::Profile | Entry::Tools(_) | Entry::Phone => {
            // The host id is fixed for a runtime's life. One gone before its
            // entry was served answers nothing, so whatever the router
            // picks for it is answered "no longer running" or unreachable.
            let own = runtime
                .upgrade()
                .map_or_else(HostId::nil, |runtime| runtime.host());
            let carrier = Carrier::new(own, Arc::new(runtime.clone()));
            client_entrance(runtime, entry, own, handlers, carrier)
        }
    }
}

/// A client's `entry` to host `own`: its `handlers`, with `carrier` behind
/// the router, and the caller settled in front of both.
pub(crate) fn client_entrance<S, C>(
    runtime: &Weak<ProfileRuntime>,
    entry: Entry,
    own: HostId,
    handlers: S,
    carrier: C,
) -> Entrance
where
    S: Service<http::Request<Body>, Response = http::Response<Body>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    C: Service<http::Request<Body>, Response = http::Response<Body>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    C::Future: Send + 'static,
{
    settled(runtime, entry, routed(own, handlers, carrier))
}

/// Which of a client entry's two services answers a call.
const HANDLERS: usize = 0;
const CARRIER: usize = 1;

/// `handlers` with the router in front, sending a call addressed to a
/// host other than `own` to `carrier`. The router is tower's `Steer`, which
/// asks both to be ready before it routes; both always are.
fn routed<S, C>(own: HostId, handlers: S, carrier: C) -> Entrance
where
    S: Service<http::Request<Body>, Response = http::Response<Body>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    C: Service<http::Request<Body>, Response = http::Response<Body>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    C::Future: Send + 'static,
{
    let services: [Entrance; 2] = [
        BoxCloneService::new(handlers),
        BoxCloneService::new(carrier),
    ];
    BoxCloneService::new(Steer::new(
        services,
        move |request: &http::Request<Body>, _: &[Entrance]| pick(request, own),
    ))
}

/// The whole of the routing: a call goes to the carrier when its authority
/// is a host id other than `own`'s, in the one form a host id is written
/// in; anything else is answered here. That covers the `amux.local` and
/// `amux-tools` clients send, `own`'s id, a name, any other spelling of an
/// id, and a call made in memory with no authority at all.
fn pick(request: &http::Request<Body>, own: HostId) -> usize {
    match request
        .uri()
        .authority()
        .and_then(|authority| wire::host_of_authority(authority.as_str()))
    {
        Some(host) if host != own => CARRIER,
        _ => HANDLERS,
    }
}

/// `inner` with the caller `entry` implies settled in each request.
fn settled(runtime: &Weak<ProfileRuntime>, entry: Entry, inner: Entrance) -> Entrance {
    BoxCloneService::new(Settle {
        inner,
        entry,
        runtime: runtime.clone(),
    })
}

/// The layer that settles a call's caller from its entry. It refuses
/// nothing itself: what it cannot settle it records as the refusal, which
/// the service that needs the caller answers with (see [`Unsettled`]), so a
/// call to a service the entry does not mount is still answered
/// "unimplemented".
#[derive(Clone)]
struct Settle {
    inner: Entrance,
    entry: Entry,
    runtime: Weak<ProfileRuntime>,
}

impl Service<http::Request<Body>> for Settle {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<http::Response<Body>, Infallible>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        // What it passes calls to is always ready; it is readied per call.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: http::Request<Body>) -> Self::Future {
        let inner = self.inner.clone();
        let (entry, runtime) = (self.entry, self.runtime.clone());
        Box::pin(async move {
            settle(entry, &runtime, &mut request).await;
            inner.oneshot(request).await
        })
    }
}

/// Puts who `request` is for in its extensions, as `entry` settles it.
async fn settle(entry: Entry, runtime: &Weak<ProfileRuntime>, request: &mut http::Request<Body>) {
    let extensions = match entry {
        Entry::Link => {
            // The caller header is the carrying node's word, for its own
            // people and agents, on a link whose handshake named the host.
            let header = request
                .headers()
                .get(wire::CALLER_HEADER)
                .map(http::HeaderValue::as_bytes);
            let peer = link_peer(request.extensions());
            let caller = peer.clone().and_then(|peer| Caller::on_link(peer, header));
            let daemon = peer.and_then(|peer| PairedDaemon::on_link(peer, header));
            (caller, Some(daemon))
        }
        Entry::Profile | Entry::Tools(_) | Entry::Phone => {
            // A client does not say who it is: reaching the entry does.
            request.headers_mut().remove(wire::CALLER_HEADER);
            (client_caller(entry, runtime, request.headers()).await, None)
        }
    };
    let (caller, daemon) = extensions;
    let extensions = request.extensions_mut();
    match caller {
        Ok(caller) => {
            extensions.insert(caller);
        }
        Err(refusal) => {
            extensions.insert(Unsettled::<Caller>::new(refusal));
        }
    }
    match daemon {
        Some(Ok(daemon)) => {
            extensions.insert(daemon);
        }
        Some(Err(refusal)) => {
            extensions.insert(Unsettled::<PairedDaemon>::new(refusal));
        }
        None => {}
    }
}

/// The caller of a call on a client's entry.
async fn client_caller(
    entry: Entry,
    runtime: &Weak<ProfileRuntime>,
    headers: &http::HeaderMap,
) -> Result<Caller, Status> {
    match entry {
        Entry::Profile => Ok(Caller::person_here(
            headers
                .get(wire::SURFACE_HEADER)
                .and_then(|value| value.to_str().ok())
                .map_or(Surface::Other, surface_named),
        )),
        Entry::Phone => Ok(Caller::person_here(Surface::Ios)),
        Entry::Tools(agent) => {
            // The socket knows the agent by its id alone, and its name can
            // change while it runs, so the name is read as the call comes.
            let runtime = upgrade(runtime)?;
            match runtime.row_by_id(agent.as_bytes()).await {
                // Its incarnation and permission level too, from its row,
                // so a host the call is carried to takes them as this host's
                // word rather than judging them from its copy of the row.
                Ok(Some(row)) => {
                    let level = row
                        .settings
                        .as_ref()
                        .map_or(wire::PermissionLevel::Unspecified, |settings| {
                            settings.permission_level()
                        });
                    Ok(Caller::agent_of(
                        runtime.host(),
                        agent,
                        row.name,
                        row.incarnation,
                        level,
                    ))
                }
                Ok(None) => Err(status(wire_error(
                    wire::ErrorCode::NotFound,
                    "the agent this socket serves is gone",
                ))),
                Err(error) => Err(Status::new(Code::Unavailable, error.to_string())),
            }
        }
        Entry::Link => unreachable!("a link's callers are settled from its header"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(authority: Option<&str>) -> http::Request<Body> {
        let uri = match authority {
            Some(authority) => format!("http://{authority}/amux.v1.HostService/PinFolder"),
            None => "/amux.v1.HostService/PinFolder".to_owned(),
        };
        http::Request::builder()
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    /// Only another host's id, written as host ids are, is carried.
    #[test]
    fn the_router_carries_only_another_hosts_canonical_id() {
        let own = HostId::from_u128(0x0192_f0c8_7a51_7c3e_9d2f_aaaa_aaaa_aaaa);
        let desk = HostId::from_u128(0x0192_f0c8_7a51_7c3e_9d2f_3b1c_5e6a_7d80);
        let own_id = wire::authority(&own);
        let desk_id = wire::authority(&desk);
        let cases = [
            ("amux.local", Some("amux.local"), HANDLERS),
            ("the tools socket's", Some("amux-tools"), HANDLERS),
            ("in memory, none", None, HANDLERS),
            ("own id", Some(own_id.as_str()), HANDLERS),
            ("another host's id", Some(desk_id.as_str()), CARRIER),
            ("an uppercase id", Some(&desk_id.to_uppercase()), HANDLERS),
            ("a name", Some("desk"), HANDLERS),
            (
                "a name with the old suffix",
                Some(&format!("{desk_id}.amux")),
                HANDLERS,
            ),
            (
                "an id with a port",
                Some(&format!("{desk_id}:443")),
                HANDLERS,
            ),
        ];
        for (case, authority, picked) in cases {
            assert_eq!(pick(&request(authority), own), picked, "{case}");
        }
    }
}
