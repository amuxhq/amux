//! Who a call is for. The caller is a fact about the connection a call
//! arrived on, never about its body: it is settled where the call enters
//! this node and handed to the handlers as a value.
//!
//! It is two types, because no call is made by both. A host service
//! answers a person or an agent ([`Caller`]); a paired daemon never calls
//! one for itself. The peer service answers only a paired daemon asking for
//! itself ([`PairedDaemon`]); a person's or an agent's call never reaches
//! it. On a link the two are told apart by the caller header alone: a node
//! carrying a call for someone always writes one, and a daemon asking for
//! itself writes none. So a host service refuses a link call without the
//! header, the peer service refuses one with it, and a client cannot reach
//! the peer service through the node it is attached to, with no list of
//! services anywhere.

use std::convert::Infallible;
use std::task::{Context, Poll};

use futures_util::future::Either;
use tonic::body::Body;
use tonic::codegen::http;
use tonic::server::NamedService;
use tonic::{Request, Status};
use tower::Service;
use wire::ErrorCode;

use crate::grpc::{status, wire_error};
use crate::transport::{BoxedGrpcAuth, BoxedGrpcConnectInfo};
use crate::{AgentId, HostId};

/// Which client a person is using.
pub(crate) type Surface = analytics::Client;

/// Who a host-service call is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Caller {
    /// A person, at this machine or at a paired host, on one of their
    /// clients.
    Person { at: At, surface: Surface },
    /// An agent, of this host or of a paired one, with its name there.
    Agent { agent: AgentRef, name: String },
}

/// Where a person is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum At {
    Here,
    Paired(HostId),
}

/// An agent anywhere in the fleet: its host and its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AgentRef {
    pub(crate) host: HostId,
    pub(crate) agent: AgentId,
}

/// The paired daemon a peer-service call is from, asking for itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PairedDaemon(pub(crate) HostId);

impl Caller {
    /// A person at this machine.
    pub(crate) fn person_here(surface: Surface) -> Self {
        Self::Person {
            at: At::Here,
            surface,
        }
    }

    /// One of `host`'s own agents, which is this host's when it called on
    /// its tools socket.
    pub(crate) fn agent_of(host: HostId, agent: AgentId, name: impl Into<String>) -> Self {
        Self::Agent {
            agent: AgentRef { host, agent },
            name: name.into(),
        }
    }

    /// The caller header this node writes beside a call it carries to
    /// another host for this caller, `own` being this host. None for a
    /// caller of a paired host: a call that came in on a link is answered
    /// where it arrived and never carried on, so this node never speaks
    /// for another host's people or agents.
    pub(crate) fn header(&self, own: HostId) -> Option<String> {
        let caller = match self {
            Self::Person {
                at: At::Here,
                surface,
            } => wire::Caller::person(analytics::Token::as_str(*surface)),
            Self::Agent { agent, name } if agent.host == own => {
                wire::Caller::agent(agent.agent.as_bytes().to_vec(), name.clone())
            }
            _ => return None,
        };
        Some(caller.to_header())
    }

    /// Who a host-service call that arrived on a link from `peer` is for,
    /// from the caller header the carrying node wrote. The header names a
    /// person or an agent and never a host: either is `peer`'s, the host
    /// the link's handshake authenticated, so a paired host speaks only for
    /// its own people and agents. A call with no header is the paired
    /// daemon asking for itself, which no host service answers.
    pub(crate) fn on_link(peer: HostId, header: Option<&[u8]>) -> Result<Self, Status> {
        let Some(header) = header else {
            return Err(refused(
                "a paired host's daemon asks for itself only on the peer service",
            ));
        };
        let caller = wire::Caller::from_header(header)
            .map_err(|error| status(wire_error(ErrorCode::InvalidArgument, error.to_string())))?;
        Ok(match caller.of {
            Some(wire::caller::Of::Person(person)) => Self::Person {
                at: At::Paired(peer),
                surface: surface_named(&person.surface),
            },
            Some(wire::caller::Of::Agent(agent)) => Self::Agent {
                agent: AgentRef {
                    host: peer,
                    agent: crate::grpc::agent_id(&agent.agent_id)?,
                },
                name: agent.name,
            },
            // A header naming no one does not read (Caller::from_header).
            None => unreachable!("a read caller header names someone"),
        })
    }

    /// The caller of a host-service call, as the layer of the entry it
    /// arrived on settled it. Handlers take it from here and nowhere else,
    /// so every entry's rule for who is calling lives in one place. A call
    /// that reached a handler without one was mounted without its entry's
    /// layer, which is a bug in this node, not something a caller did.
    pub(crate) fn of<T>(request: &Request<T>) -> Result<Self, Status> {
        Self::settled(request.extensions())
    }

    /// The caller the entry's layer settled in a request's `extensions`:
    /// see [`Caller::of`].
    pub(crate) fn settled(extensions: &http::Extensions) -> Result<Self, Status> {
        settled(extensions, "no caller was settled for this call")
    }
}

impl PairedDaemon {
    /// The paired daemon a peer-service call is from, as the link's layer
    /// settled it: see [`Caller::of`].
    pub(crate) fn of<T>(request: &Request<T>) -> Result<Self, Status> {
        settled(
            request.extensions(),
            "no paired daemon was settled for this call",
        )
    }

    /// The paired daemon a peer-service call that arrived on a link from
    /// `peer` is from. A call with a caller header was carried for a person
    /// or an agent, and the peer service answers none.
    pub(crate) fn on_link(peer: HostId, header: Option<&[u8]>) -> Result<Self, Status> {
        if header.is_some() {
            return Err(refused(
                "the peer service answers a paired daemon asking for itself, not a call carried for someone",
            ));
        }
        Ok(Self(peer))
    }
}

/// `server`, answering only a call whose [`Caller`] its entry settled.
/// Every service that answers a person or an agent is mounted through this,
/// so a call that has none (on a link, one without the caller header: the
/// paired daemon asking for itself) is refused with the entry's reason
/// before any handler runs, and a handler cannot forget to check. A
/// handler still reads who is calling with [`Caller::of`].
pub(in crate::api) fn for_a_caller<S>(server: S) -> ForACaller<S> {
    ForACaller(server)
}

/// A service's server behind the check that its call has a caller: see
/// [`for_a_caller`].
#[derive(Clone)]
pub(in crate::api) struct ForACaller<S>(S);

impl<S: NamedService> NamedService for ForACaller<S> {
    const NAME: &'static str = S::NAME;
}

impl<S> Service<http::Request<Body>> for ForACaller<S>
where
    S: Service<http::Request<Body>, Response = http::Response<Body>, Error = Infallible>,
{
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = Either<std::future::Ready<Result<http::Response<Body>, Infallible>>, S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        match Caller::settled(request.extensions()) {
            Ok(_) => Either::Right(self.0.call(request)),
            Err(refusal) => Either::Left(std::future::ready(Ok(refusal.into_http()))),
        }
    }
}

/// Why the entry's layer could not settle a `T` for a call: the refusal the
/// service that needs one answers with. The layer settles what it can and
/// refuses nothing itself, so a call to a service no entry mounts is still
/// answered "unimplemented", and only a service that needs a caller it
/// could not have refuses: one that answers a person or an agent before
/// its handlers run ([`for_a_caller`]), the peer service in each handler.
#[derive(Clone, Debug)]
pub(crate) struct Unsettled<T>(pub(crate) Status, std::marker::PhantomData<fn() -> T>);

impl<T> Unsettled<T> {
    pub(crate) fn new(refusal: Status) -> Self {
        Self(refusal, std::marker::PhantomData)
    }
}

/// The `T` the entry's layer settled in `extensions`, or its refusal. A
/// call with neither was served without its entry's layer, which is a bug
/// in this node, not something a caller did.
fn settled<T: Clone + Send + Sync + 'static>(
    extensions: &http::Extensions,
    unlayered: &'static str,
) -> Result<T, Status> {
    if let Some(value) = extensions.get::<T>() {
        return Ok(value.clone());
    }
    Err(match extensions.get::<Unsettled<T>>() {
        Some(Unsettled(refusal, _)) => refusal.clone(),
        None => Status::internal(unlayered),
    })
}

/// The paired host a link call came from, from the request's extensions.
/// Only a link stream whose handshake presented a trusted key is served
/// the link's services, so a call without one is refused rather than
/// answered as nobody.
pub(in crate::api) fn link_peer(extensions: &http::Extensions) -> Result<HostId, Status> {
    match extensions.get::<BoxedGrpcConnectInfo>() {
        Some(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::Trusted { peer },
        }) => Ok(*peer),
        _ => Err(Status::unauthenticated("the caller is not a trusted host")),
    }
}

/// The client a person's surface word names. A word this build does not
/// know reads as the terminal, the client that names itself with none.
pub(crate) fn surface_named(word: &str) -> Surface {
    match word {
        "terminal" => Surface::Terminal,
        "phone" => Surface::Phone,
        "mac" => Surface::Mac,
        "browser" => Surface::Browser,
        _ => Surface::Other,
    }
}

fn refused(why: &str) -> Status {
    status(wire_error(ErrorCode::PermissionDenied, why))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(byte: u8) -> HostId {
        HostId::from_bytes([byte; 16])
    }

    /// Who a host-service call on a link from `peer`, with `header` beside
    /// it when there is one, is for, as the link's layer reads it.
    fn on_link(peer: HostId, header: Option<String>) -> Result<Caller, Status> {
        Caller::on_link(peer, header.as_deref().map(str::as_bytes))
    }

    /// Who a peer-service call on a link is from, as the layer reads it.
    fn daemon_on_link(peer: HostId, header: Option<String>) -> Result<PairedDaemon, Status> {
        PairedDaemon::on_link(peer, header.as_deref().map(str::as_bytes))
    }

    #[test]
    fn a_host_service_call_on_a_link_with_no_header_is_refused() {
        let refused = on_link(host(1), None).unwrap_err();
        assert_eq!(refused.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn a_peer_service_call_with_a_header_is_refused() {
        let here = host(9);
        for caller in [
            Caller::person_here(Surface::Phone),
            Caller::agent_of(here, AgentId::from_bytes([2; 16]), "coder"),
        ] {
            let header = caller.header(here);
            assert!(header.is_some());
            let refused = daemon_on_link(here, header).unwrap_err();
            assert_eq!(refused.code(), tonic::Code::PermissionDenied);
        }
        assert_eq!(
            daemon_on_link(host(1), None).unwrap(),
            PairedDaemon(host(1))
        );
    }

    /// The carrying node writes the header for its own people and agents;
    /// the answering host reads them as the link's host's.
    #[test]
    fn a_header_names_someone_of_the_links_own_host() {
        let (laptop, desk) = (host(1), host(2));
        let agent = AgentId::from_bytes([3; 16]);
        let person = Caller::person_here(Surface::Phone).header(laptop);
        assert_eq!(
            on_link(laptop, person).unwrap(),
            Caller::Person {
                at: At::Paired(laptop),
                surface: Surface::Phone,
            }
        );
        let coder = Caller::agent_of(laptop, agent, "coder").header(laptop);
        assert_eq!(
            on_link(laptop, coder.clone()).unwrap(),
            Caller::agent_of(laptop, agent, "coder")
        );
        // The same header on another host's link names that host's agent:
        // nothing in it can name a host.
        assert_eq!(
            on_link(desk, coder).unwrap(),
            Caller::agent_of(desk, agent, "coder")
        );
    }

    /// A node never speaks for another host's callers, so a call that came
    /// in on a link has no header to be carried on with.
    #[test]
    fn a_paired_hosts_caller_gets_no_header() {
        let (laptop, desk) = (host(1), host(2));
        let agent = AgentId::from_bytes([3; 16]);
        assert_eq!(
            Caller::Person {
                at: At::Paired(desk),
                surface: Surface::Terminal,
            }
            .header(laptop),
            None
        );
        assert_eq!(Caller::agent_of(desk, agent, "coder").header(laptop), None);
    }

    #[test]
    fn a_header_that_does_not_read_or_names_a_short_id_is_refused() {
        let peer = host(1);
        let short = wire::Caller::agent(vec![3; 4], "coder").to_header();
        for header in ["not base64!".to_owned(), String::new(), short] {
            let refused = on_link(peer, Some(header.clone()));
            assert_eq!(
                refused.unwrap_err().code(),
                tonic::Code::InvalidArgument,
                "{header:?}"
            );
        }
    }

    #[test]
    fn a_call_not_from_a_trusted_link_is_refused() {
        let request = Request::new(());
        assert_eq!(
            link_peer(request.extensions()).unwrap_err().code(),
            tonic::Code::Unauthenticated
        );
        let mut trusted = Request::new(());
        trusted.extensions_mut().insert(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::Trusted { peer: host(1) },
        });
        assert_eq!(link_peer(trusted.extensions()).unwrap(), host(1));
    }

    #[test]
    fn a_surface_this_build_does_not_know_reads_as_another_client() {
        assert_eq!(surface_named("watch"), Surface::Other);
        for surface in Surface::ALL.iter().copied() {
            assert_eq!(surface_named(analytics::Token::as_str(surface)), surface);
        }
    }

    #[test]
    fn a_handler_reads_the_caller_its_entry_settled_and_nothing_else() {
        let mut request = Request::new(());
        assert_eq!(
            Caller::of(&request).unwrap_err().code(),
            tonic::Code::Internal
        );
        assert_eq!(
            PairedDaemon::of(&request).unwrap_err().code(),
            tonic::Code::Internal
        );

        let person = Caller::person_here(Surface::Phone);
        request.extensions_mut().insert(person.clone());
        request.extensions_mut().insert(PairedDaemon(host(7)));
        assert_eq!(Caller::of(&request).unwrap(), person);
        assert_eq!(PairedDaemon::of(&request).unwrap(), PairedDaemon(host(7)));
    }

    /// What the entry could not settle is refused by the handler that asks
    /// for it, and only by that one.
    #[test]
    fn a_handler_answers_the_refusal_its_entry_settled() {
        let mut request = Request::new(());
        request
            .extensions_mut()
            .insert(Unsettled::<Caller>::new(refused("no")));
        request.extensions_mut().insert(PairedDaemon(host(7)));
        assert_eq!(
            Caller::of(&request).unwrap_err().code(),
            tonic::Code::PermissionDenied
        );
        assert_eq!(PairedDaemon::of(&request).unwrap(), PairedDaemon(host(7)));
    }
}
