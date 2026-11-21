//! What a handle's calls ride: a channel on the profile's socket, or the
//! node's services called in memory.
//!
//! On a socket, a tonic channel has one authority: it writes the authority
//! it was built for over every request's, so a call cannot name another
//! host on the node's channel. Addressing a host is opening a channel for
//! it on the same socket path, built for that host's id. Those channels are
//! kept, one per host. A call addressed to the node's own id rides the
//! node's channel instead, which needs the node's id: it is asked for on
//! the first call addressed to a host, not when the client connects, so a
//! client that only reads the node's own services never pays for it. In
//! memory the stub's origin is the authority, so one service serves every
//! address.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::task::{Context, Poll};

use futures_util::future::BoxFuture;
use tonic::body::Body;
use tonic::codegen::StdError;
use tonic::codegen::http::{self, HeaderValue, Uri};
use tonic::transport::{Channel, Endpoint};
use tower::util::BoxCloneService;
use tower::{Service, ServiceExt as _};
use uuid::Uuid;

use crate::{RpcError, Surface};

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
        let via = match (self, address) {
            (Transport::Socket(socket), Address::Node) => Via::Channel(socket.node()),
            (Transport::Socket(socket), Address::Host(host)) => Via::Host(socket.clone(), host),
            (Transport::Memory(memory), _) => Via::Memory(memory.clone()),
        };
        Conn {
            via,
            origin: address.origin(),
            surface,
        }
    }
}

/// The profile's socket, with the channels opened on it.
pub(crate) struct Socket {
    path: PathBuf,
    node: Channel,
    /// The node's own host id, once a call addressed to a host asked for
    /// it: calls addressed to it ride the node's channel.
    own: OnceLock<Uuid>,
    /// The channels of hosts other than the node's own, or of every host
    /// addressed while the node's id could not be learned.
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
            own: OnceLock::new(),
            hosts: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn node(&self) -> Channel {
        self.node.clone()
    }

    /// The channel a call addressed to `host` rides: the node's own when
    /// `host` is the node, else one of its own for that host. A node that
    /// does not say who it is leaves its own id on a channel of its own,
    /// which the node answers all the same.
    async fn channel(&self, host: Uuid, surface: Surface) -> Channel {
        if let Some(channel) = self.held(host) {
            return channel;
        }
        if self.own(surface).await == Some(host) {
            return self.node.clone();
        }
        self.hosts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(host)
            .or_insert_with(|| {
                endpoint(Address::Host(host)).connect_with_connector_lazy(connector(&self.path))
            })
            .clone()
    }

    fn held(&self, host: Uuid) -> Option<Channel> {
        self.hosts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&host)
            .cloned()
    }

    /// The node's own host id, asked of it once. A failed ask is not kept,
    /// so the next call addressed to a host asks again.
    async fn own(&self, surface: Surface) -> Option<Uuid> {
        if let Some(own) = self.own.get() {
            return Some(*own);
        }
        let identity = crate::Admin::on(&self.node, surface)
            .get_device_identity()
            .await
            .ok()?;
        let own = Uuid::from_slice(&identity.host_id).ok()?;
        Some(*self.own.get_or_init(|| own))
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
    /// A host on the socket, whose channel is settled at the call.
    Host(Arc<Socket>, Uuid),
    Memory(Memory),
}

/// What a stub is built on: a request in, a response out, with the
/// client's surface named on every request.
#[derive(Clone)]
pub struct Conn {
    via: Via,
    /// What a stub on it is built with: the authority it addresses.
    origin: Uri,
    surface: Surface,
}

impl Conn {
    /// Calls on `channel`, answered by the node it ends at.
    pub(crate) fn on_node(channel: Channel, surface: Surface) -> Conn {
        Conn {
            via: Via::Channel(channel),
            origin: Address::Node.origin(),
            surface,
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
        request.headers_mut().insert(
            wire::SURFACE_HEADER,
            HeaderValue::from_static(self.surface.word()),
        );
        match &self.via {
            Via::Channel(channel) => {
                let channel = channel.clone();
                Box::pin(async move { channel.oneshot(request).await.map_err(Into::into) })
            }
            Via::Host(socket, host) => {
                let (socket, host, surface) = (socket.clone(), *host, self.surface);
                Box::pin(async move {
                    let channel = socket.channel(host, surface).await;
                    channel.oneshot(request).await.map_err(Into::into)
                })
            }
            Via::Memory(memory) => Box::pin(memory.service().oneshot(request)),
        }
    }
}
