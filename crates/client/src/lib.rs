//! One client library over the generated stubs, for every amux client.
//!
//! A [`Client`] is attached to one node: a desktop daemon over the
//! profile's socket, or the phone's runtime called in memory. What it
//! offers is sorted by who answers:
//!
//! - **Local services** answer from the node's own copy of the fleet, so
//!   they answer whether or not any other host can be reached:
//!   [`Client::inventory`], [`Client::sessions`] and [`Client::drafts`],
//!   and the profile's administration, [`Client::admin`].
//! - **Host services** are answered by the host that owns what they act
//!   on, and a call is addressed to that host by its authority, the host's
//!   id. The node passes a call addressed to another host on to it unread.
//!   Their stubs exist only through [`Client::host`] and [`Client::agent`],
//!   so no call to a host service can be made without saying which host:
//!
//! ```compile_fail,E0599
//! # fn unaddressed(client: &client::Client) {
//! // There is no agent service but an addressed one.
//! client.agents();
//! # }
//! ```
//!
//! ```compile_fail,E0624
//! // Nor any way to make an agent's handle but from the client and the
//! // agent's address.
//! let _ = client::Agent::new(todo!(), b"agent".to_vec());
//! ```
//!
//! ```no_run
//! # async fn addressed(client: client::Client, row: wire::Agent) {
//! // An agent's row carries its host and its id together, so the call
//! // goes to its host and names it there, from the one value.
//! client.agent(&row).rename("docs").await.unwrap();
//! // A new agent has no row yet, so it starts from a host.
//! let desk = client.host(&row.host_id);
//! desk.catalogue("claude").await.unwrap();
//! # }
//! ```
//!
//! Every call names the client it is ([`Surface`]), which the node counts
//! and the owning host records as where the act came from.

mod admin;
mod host;
mod installation;
mod local;
mod reviews;
mod status;
mod transport;

use std::path::Path;
use std::pin::Pin;

pub use admin::Admin;
pub use agent_dir::{Clock, ManualClock, SystemClock};
use futures_util::{Stream, StreamExt as _};
pub use host::{Agent, Host, HostAgents};
pub use installation::Installation;
pub use local::{Drafts, Inventory, Sessions};
pub use reviews::AgentReviews;
pub use status::NotSent;
use tonic::Status;
use tonic::body::Body;
use tonic::codegen::{StdError, http};
pub use transport::Conn;
use transport::{Address, Memory, Socket, Transport};
use uuid::Uuid;
use wire::ErrorCode;

/// Which client makes a call, as every call names it to the node. Each
/// of amux's apps is named for itself, the iPhone app as `Ios`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Terminal,
    Mac,
    Ios,
    Browser,
}

impl Surface {
    /// The word a call names the client with, in `amux-surface`.
    pub fn word(self) -> &'static str {
        match self {
            Surface::Terminal => "terminal",
            Surface::Mac => "mac",
            Surface::Ios => "ios",
            Surface::Browser => "browser",
        }
    }
}

/// Something that names one agent where it lives: its host and its id,
/// held together, as every agent row and every reference to an agent is.
pub trait AgentAddress {
    /// The id of the host that owns the agent.
    fn host_id(&self) -> &[u8];
    fn agent_id(&self) -> &[u8];
}

impl AgentAddress for wire::Agent {
    fn host_id(&self) -> &[u8] {
        &self.host_id
    }

    fn agent_id(&self) -> &[u8] {
        &self.agent_id
    }
}

impl AgentAddress for wire::AgentRef {
    fn host_id(&self) -> &[u8] {
        &self.host_id
    }

    fn agent_id(&self) -> &[u8] {
        &self.agent_id
    }
}

impl<T: AgentAddress + ?Sized> AgentAddress for &T {
    fn host_id(&self) -> &[u8] {
        (**self).host_id()
    }

    fn agent_id(&self) -> &[u8] {
        (**self).agent_id()
    }
}

/// A client of one node: its local services, its administration, and the
/// host services of every host it can reach, addressed by host.
#[derive(Clone)]
pub struct Client {
    transport: Transport,
    surface: Surface,
}

impl Client {
    /// A client of the daemon behind the profile's socket at `path`. The
    /// node's channel redials the socket on the next call after the
    /// connection drops, so a client outlives a daemon restart; a stream
    /// open at the time ends, and its reader subscribes again.
    pub async fn connect(path: &Path, surface: Surface) -> Result<Client, RpcError> {
        let socket = Socket::connect(path).await?;
        Ok(Client {
            transport: Transport::Socket(std::sync::Arc::new(socket)),
            surface,
        })
    }

    /// A client of a node's services called in memory: the phone's
    /// runtime, whose entry routes and carries a call as a socket's does,
    /// or a test's fake of them.
    pub fn in_memory<S>(service: S, surface: Surface) -> Client
    where
        S: tower::Service<http::Request<Body>, Response = http::Response<Body>>
            + Clone
            + Send
            + 'static,
        S::Error: Into<StdError>,
        S::Future: Send + 'static,
    {
        Client {
            transport: Transport::Memory(Memory::new(service)),
            surface,
        }
    }

    /// The fleet as this node holds it: hosts, agent rows, names resolved.
    pub fn inventory(&self) -> Inventory {
        Inventory::new(self.conn(Address::Node))
    }

    /// The conversations this node holds, its own and its copies.
    pub fn sessions(&self) -> Sessions {
        Sessions::new(self.conn(Address::Node))
    }

    /// What this machine's person has typed and not sent.
    pub fn drafts(&self) -> Drafts {
        Drafts::new(self.conn(Address::Node))
    }

    /// The profile's administration, on its own node only.
    pub fn admin(&self) -> Admin {
        Admin::new(self.conn(Address::Node))
    }

    /// The host services of host `host_id`: what it offers, and making an
    /// agent there.
    pub fn host(&self, host_id: &[u8]) -> Host {
        Host::new(self.addressed(host_id))
    }

    /// The host services for one agent, addressed to its host with its id
    /// filled from the same value.
    pub fn agent(&self, agent: &impl AgentAddress) -> Agent {
        Agent::new(self.addressed(agent.host_id()), agent.agent_id().to_vec())
    }

    fn conn(&self, address: Address) -> Conn {
        self.transport.conn(address, self.surface)
    }

    fn addressed(&self, host_id: &[u8]) -> Addressed {
        match host_id_of(host_id) {
            Ok(host) => Addressed::To(self.conn(Address::Host(host))),
            Err(refusal) => Addressed::Nowhere(refusal),
        }
    }
}

/// A host service's connection, or why there can be none: a row whose host
/// id is not one names no host a call could be addressed to.
#[derive(Clone)]
enum Addressed {
    To(Conn),
    Nowhere(RpcError),
}

impl Addressed {
    fn conn(&self) -> Result<(Conn, http::Uri), RpcError> {
        match self {
            Addressed::To(conn) => Ok((conn.clone(), conn.origin())),
            Addressed::Nowhere(refusal) => Err(refusal.clone()),
        }
    }
}

fn host_id(bytes: &[u8]) -> Option<Uuid> {
    Uuid::from_slice(bytes).ok()
}

fn host_id_of(bytes: &[u8]) -> Result<Uuid, RpcError> {
    host_id(bytes).ok_or_else(|| {
        RpcError::Refused(wire::Error::new(
            wire::ErrorCode::InvalidArgument,
            format!(
                "a host is addressed by its 16-byte id, not {} bytes",
                bytes.len()
            ),
        ))
    })
}

/// Builds a generated stub on `conn`, its origin the address it rides and
/// its messages as large as the largest attachment.
macro_rules! stub {
    ($client:ty, $conn:expr) => {{
        let (conn, origin): (crate::Conn, tonic::codegen::http::Uri) = $conn;
        <$client>::with_origin(conn, origin)
            .max_decoding_message_size(wire::CHANNEL_MESSAGE_SIZE_LIMIT)
            .max_encoding_message_size(wire::CHANNEL_MESSAGE_SIZE_LIMIT)
    }};
}
pub(crate) use stub;

/// A server stream. An `Err` item or the end of the stream both mean the
/// stream is over; only the first says the transport failed.
pub type EventStream<T> = Pin<Box<dyn Stream<Item = Result<T, RpcError>> + Send>>;

/// Why a call did not return its answer.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum RpcError {
    /// The runtime could not be reached, or the connection failed before
    /// the answer arrived: whether the call took effect is unknown.
    #[error("the local runtime is not answering: {0}")]
    Transport(String),
    /// The runtime answered, with an error.
    #[error("{}", .0.message)]
    Refused(wire::Error),
}

impl RpcError {
    /// The error's code, when the runtime answered.
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            RpcError::Transport(_) => None,
            RpcError::Refused(error) => Some(error.code()),
        }
    }

    /// The amux reason the runtime gave, when it gave one.
    pub fn reason(&self) -> Option<wire::ErrorReason> {
        match self {
            RpcError::Transport(_) => None,
            RpcError::Refused(error) => error.reason(),
        }
    }

    pub fn is_transport(&self) -> bool {
        matches!(self, RpcError::Transport(_))
    }
}

/// A gRPC channel over the local socket at `path`, which redials it on the
/// next call after the connection drops.
pub async fn local_channel(path: &Path) -> Result<tonic::transport::Channel, RpcError> {
    let path = path.to_owned();
    tonic::transport::Endpoint::from_static("http://amux.local")
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();
            async move {
                agent_dir::local_socket::connect(&path)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .map_err(|error| RpcError::Transport(error.to_string()))
}

fn events<T, S>(stream: S) -> EventStream<T>
where
    S: Stream<Item = Result<T, Status>> + Send + 'static,
{
    Box::pin(stream.map(|item| item.map_err(RpcError::from)))
}

/// A stream of what each of its responses holds. A response holding nothing
/// is dropped, as an event this build does not know is.
fn held_events<R, T, S>(stream: S, event: fn(R) -> Option<T>) -> EventStream<T>
where
    R: 'static,
    T: Send + 'static,
    S: Stream<Item = Result<R, Status>> + Send + 'static,
{
    Box::pin(stream.filter_map(move |item| {
        futures_util::future::ready(match item {
            Ok(response) => event(response).map(Ok),
            Err(status) => Some(Err(RpcError::from(status))),
        })
    }))
}

/// The value a response holds. A response without it never came from a
/// runtime that answered the call, so it says nothing of whether it landed.
fn held<T>(value: Option<T>) -> Result<T, RpcError> {
    value.ok_or_else(|| RpcError::Transport("the answer held nothing".to_owned()))
}
