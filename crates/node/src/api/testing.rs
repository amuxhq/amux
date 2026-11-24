//! The services an entry serves, called in memory as whoever a test names.
//!
//! In a running node the layer in front of each entry settles who is
//! calling, from the socket or the link the call came in on. A test of a
//! handler's rules wants every caller on every entry, a person or an agent
//! of a paired host included, without building the transport that would
//! carry each, so this settles the caller it is told and hands the call to
//! the routes the entry serves.

use std::convert::Infallible;
use std::sync::Arc;
use std::task::{Context, Poll};

use tonic::body::Body;
use tonic::codegen::http;
use tonic::service::Routes;

use super::caller::{AgentRef, At, Caller};
use super::{Entry, routes};
use crate::runtime::ProfileRuntime;
use crate::{AgentId, HostId};

/// A way into a node, as a test names it.
#[derive(Clone, Copy, Debug)]
pub enum Served {
    /// The profile's socket.
    Profile,
    /// The tools socket of this host's agent.
    Tools(AgentId),
    /// The phone's runtime, in memory.
    Phone,
    /// A link from a trusted host.
    Link,
}

/// Who a call is made as.
#[derive(Clone, Debug)]
pub enum Calling {
    /// A person at this machine, on that client.
    PersonHere(analytics::Client),
    /// A person at a paired host, on that client.
    PersonAt(HostId, analytics::Client),
    /// An agent of `host`, which is this host's own on a tools socket, as
    /// its host states it: its name, the incarnation calling and its
    /// permission level.
    Agent {
        host: HostId,
        agent: AgentId,
        name: String,
        incarnation: u32,
        level: wire::PermissionLevel,
    },
}

/// The routes an entry serves, each call answered as one caller.
#[derive(Clone)]
pub struct InMemory {
    routes: Routes,
    calling: Calling,
}

/// The services `served` serves for `runtime`, every call made as
/// `calling`. A client of any service is built on it with the generated
/// constructors, as on a channel.
pub fn served_as(runtime: &Arc<ProfileRuntime>, served: Served, calling: Calling) -> InMemory {
    let entry = match served {
        Served::Profile => Entry::Profile,
        Served::Tools(agent) => Entry::Tools(agent),
        Served::Phone => Entry::Phone,
        Served::Link => Entry::Link,
    };
    InMemory {
        routes: routes(&Arc::downgrade(runtime), entry).routes(),
        calling,
    }
}

impl tower::Service<http::Request<Body>> for InMemory {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = <Routes as tower::Service<http::Request<Body>>>::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        tower::Service::<http::Request<Body>>::poll_ready(&mut self.routes, cx)
    }

    fn call(&mut self, mut request: http::Request<Body>) -> Self::Future {
        let extensions = request.extensions_mut();
        match self.calling.clone() {
            Calling::PersonHere(surface) => {
                extensions.insert(Caller::person_here(surface));
            }
            Calling::PersonAt(host, surface) => {
                extensions.insert(Caller::Person {
                    at: At::Paired(host),
                    surface,
                });
            }
            Calling::Agent {
                host,
                agent,
                name,
                incarnation,
                level,
            } => {
                extensions.insert(Caller::Agent {
                    agent: AgentRef { host, agent },
                    name,
                    incarnation,
                    level,
                });
            }
        }
        self.routes.call(request)
    }
}
