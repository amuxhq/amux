//! What a handle's calls ride: a channel on the profile's socket, or the
//! node's services called in memory.
//!
//! On a socket, a tonic channel has one authority: it writes the authority
//! it was built for over every request's, so a call cannot name another
//! host on the node's channel. Addressing a host is opening a channel for
//! it on the same socket path, built for that host's id. Those channels are
//! kept, one per host, and the node's own id rides the node's channel. In
//! memory the stub's origin is the authority, so one service serves every
//! address.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use futures_util::future::BoxFuture;
use tonic::body::Body;
use tonic::codegen::StdError;
use tonic::codegen::http::{self, HeaderValue, Uri};
use tonic::transport::{Channel, Endpoint};
use tower::util::BoxCloneService;
use tower::{Service, ServiceExt as _};
use uuid::Uuid;

use super::Surface;
use crate::RpcError;

/// The authority a call answered by the node itself is made with. Anything
/// that is not a host id is answered there; this is the one clients send.
const NODE: &str = "amux.local";

/// Where a call goes: the node a client is attached to, or a host by id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Address {
    Node,
    Host(Uuid),
}

impl Address {
    fn authority(self) -> String {
        match self {
            Address::Node => NODE.to_owned(),
            Address::Host(host) => wire::authority(&host),
        }
    }

    /// The origin a stub for this address is built with.
    pub(crate) fn origin(self) -> Uri {
        format!("http://{}", self.authority())
            .parse()
            .expect("an authority of a host id or amux.local is a valid uri")
    }
}

/// How a client reaches its node.
#[derive(Clone)]
pub(crate) enum Transport {
    Socket(Arc<Socket>),
    Memory(Memory),
}

impl Transport {
    /// The service a call to `address` rides, naming the client as
    /// `surface` on every request.
    pub(crate) fn conn(&self, address: Address, surface: Surface) -> Conn {
        let via = match self {
            Transport::Socket(socket) => Via::Channel(socket.channel(address)),
            Transport::Memory(memory) => Via::Memory(memory.clone()),
        };
        Conn {
            via,
            origin: address.origin(),
            surface: HeaderValue::from_static(surface.word()),
        }
    }
}

/// The profile's socket, with the channels opened on it.
pub(crate) struct Socket {
    path: PathBuf,
    node: Channel,
    /// The node's own host id, when it said: calls addressed to it ride the
    /// node's channel.
    own: Option<Uuid>,
    hosts: Mutex<HashMap<Uuid, Channel>>,
}

impl Socket {
    /// Dials the socket at `path` for the node's own calls. Every other
    /// host's channel dials on its first call.
    pub(crate) async fn connect(path: &Path) -> Result<Socket, RpcError> {
        let node = endpoint(Address::Node)
            .connect_with_connector(connector(path))
            .await
            .map_err(|error| RpcError::Transport(error.to_string()))?;
        Ok(Socket {
            path: path.to_owned(),
            node,
            own: None,
            hosts: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn node(&self) -> Channel {
        self.node.clone()
    }

    pub(crate) fn knowing_own(mut self, own: Uuid) -> Socket {
        self.own = Some(own);
        self
    }

    fn channel(&self, address: Address) -> Channel {
        let host = match address {
            Address::Host(host) if Some(host) != self.own => host,
            Address::Host(_) | Address::Node => return self.node.clone(),
        };
        self.hosts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(host)
            .or_insert_with(|| endpoint(address).connect_with_connector_lazy(connector(&self.path)))
            .clone()
    }
}

fn endpoint(address: Address) -> Endpoint {
    Endpoint::from(address.origin())
}

/// Dials the local socket at its path whenever a channel needs a new
/// connection, so a client outlives a daemon restart.
#[derive(Clone)]
struct Dial(Arc<Path>);

fn connector(path: &Path) -> Dial {
    Dial(Arc::from(path))
}

impl Service<Uri> for Dial {
    type Response = hyper_util::rt::TokioIo<agent_dir::local_socket::LocalStream>;
    type Error = std::io::Error;
    type Future = BoxFuture<'static, Result<Self::Response, std::io::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: Uri) -> Self::Future {
        let path = self.0.clone();
        Box::pin(async move {
            agent_dir::local_socket::connect(&path)
                .await
                .map(hyper_util::rt::TokioIo::new)
        })
    }
}

type Boxed = BoxCloneService<http::Request<Body>, http::Response<Body>, StdError>;

/// Services called in memory, shared between threads. The service itself
/// need not be shareable: each call takes a clone of it.
#[derive(Clone)]
pub(crate) struct Memory(Arc<Mutex<Boxed>>);

impl Memory {
    pub(crate) fn new<S>(service: S) -> Memory
    where
        S: Service<http::Request<Body>, Response = http::Response<Body>> + Clone + Send + 'static,
        S::Error: Into<StdError>,
        S::Future: Send + 'static,
    {
        Memory(Arc::new(Mutex::new(BoxCloneService::new(
            service.map_err(Into::into),
        ))))
    }

    fn service(&self) -> Boxed {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[derive(Clone)]
enum Via {
    Channel(Channel),
    Memory(Memory),
}

/// What a stub is built on: a request in, a response out, with the
/// client's surface named on every request.
#[derive(Clone)]
pub struct Conn {
    via: Via,
    /// What a stub on it is built with: the authority it addresses.
    origin: Uri,
    surface: HeaderValue,
}

impl Conn {
    /// Calls on `channel`, answered by the node it ends at.
    pub(crate) fn on_node(channel: Channel, surface: Surface) -> Conn {
        Conn {
            via: Via::Channel(channel),
            origin: Address::Node.origin(),
            surface: HeaderValue::from_static(surface.word()),
        }
    }

    pub(crate) fn origin(&self) -> Uri {
        self.origin.clone()
    }
}

impl Service<http::Request<Body>> for Conn {
    type Response = http::Response<Body>;
    type Error = StdError;
    type Future = BoxFuture<'static, Result<http::Response<Body>, StdError>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), StdError>> {
        // Each call readies its own clone of what it rides.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: http::Request<Body>) -> Self::Future {
        request
            .headers_mut()
            .insert(wire::SURFACE_HEADER, self.surface.clone());
        match &self.via {
            Via::Channel(channel) => {
                let channel = channel.clone();
                Box::pin(async move { channel.oneshot(request).await.map_err(Into::into) })
            }
            Via::Memory(memory) => Box::pin(memory.service().oneshot(request)),
        }
    }
}
