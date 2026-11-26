//! What every gRPC server of a profile shares: errors as statuses, the
//! connections a local socket accepts, and serving an entry's services on
//! one: the profile's socket for clients, or an agent's tools socket for
//! its MCP server, where the socket is the caller's identity. What each
//! entry serves, and who it answers as, is the services' own (see
//! [`crate::api`]).
//!
//! A service holds the runtime weakly: a connection that outlives its
//! runtime (a restart in process, a deleted profile) is answered
//! unavailable, never kept alive.

use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};

use agent_dir::local_socket::{LocalListener, LocalStream};
use futures_util::Stream;
use prost::Message as _;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::task::JoinHandle;
use tonic::{Code, Status};
use uuid::Uuid;
use wire::ErrorCode;
use wire::profile_admin_service_server::ProfileAdminServiceServer;

use crate::runtime::{AgentId, ProfileRuntime};

/// A settings change refused, as a gRPC status.
pub(crate) fn settings_status(error: crate::host_settings::SettingsError) -> Status {
    status(error.to_wire())
}

/// A wire error as a gRPC status: the coarse code for generic clients, and
/// the whole error, details included, in the status details for ours.
pub fn status(error: wire::Error) -> Status {
    // The error's code is gRPC's own; the whole error rides in the details,
    // as gRPC's richer error model carries it.
    let code = match Code::from_i32(error.code) {
        // An error that names no failure is still one.
        Code::Ok => Code::Internal,
        code => code,
    };
    Status::with_details(code, error.message.clone(), error.encode_to_vec().into())
}

pub(crate) fn wire_error(code: ErrorCode, message: impl Into<String>) -> wire::Error {
    wire::Error {
        code: code as i32,
        message: message.into(),
        details: Vec::new(),
    }
}

pub use wire::SURFACE_HEADER;

/// The profile runtime a service answers for, while it runs.
pub(crate) fn upgrade(runtime: &Weak<ProfileRuntime>) -> Result<Arc<ProfileRuntime>, Status> {
    runtime
        .upgrade()
        .ok_or_else(|| Status::unavailable("the profile is no longer running"))
}

pub(crate) fn agent_id(bytes: &[u8]) -> Result<AgentId, Status> {
    Uuid::from_slice(bytes).map_err(|_| {
        status(wire_error(
            ErrorCode::InvalidArgument,
            format!("an agent id is 16 bytes, not {}", bytes.len()),
        ))
    })
}

pub(crate) type EventStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

/// A connection accepted on a local socket, as tonic serves it.
pub(crate) struct Connection(LocalStream);

impl tonic::transport::server::Connected for Connection {
    type ConnectInfo = ();

    fn connect_info(&self) -> Self::ConnectInfo {}
}

impl AsyncRead for Connection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for Connection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

/// Every connection the listener accepts, for the listener's life: a
/// failed accept is logged and the next one tried, so one bad moment never
/// leaves the socket unserved.
pub(crate) fn incoming(
    listener: LocalListener,
) -> impl Stream<Item = std::io::Result<Connection>> + Send + 'static {
    futures_util::stream::unfold(listener, |mut listener| async move {
        loop {
            match listener.accept().await {
                Ok(stream) => return Some((Ok(Connection(stream)), listener)),
                Err(error) => {
                    tracing::warn!(socket = %listener.path().display(), %error, "accepting a connection failed");
                    tokio::time::sleep(ACCEPT_RETRY).await;
                }
            }
        }
    })
}

/// How long an accept loop waits after a failed accept before the next.
pub(crate) const ACCEPT_RETRY: std::time::Duration = std::time::Duration::from_millis(100);

/// Serves the services `entry` is served on a listener, with the profile's
/// administration on the profile's own socket, behind the entry's layer
/// (see [`crate::api::layer`]), until the task is aborted,
/// which drops the listener and removes its socket, and closes every
/// connection it accepted.
pub(crate) fn serve_client(
    listener: LocalListener,
    runtime: Weak<ProfileRuntime>,
    entry: crate::api::Entry,
    admin: Option<ProfileAdminServiceServer<crate::ProfileAdmin>>,
) -> JoinHandle<()> {
    let routes = crate::api::routes(&runtime, entry);
    tokio::spawn(async move {
        // The signal never fires; asking for graceful shutdown at all is
        // what ties each accepted connection to this future. Without it a
        // connection outlives the abort, and a client on it is answered
        // "no longer running" by a dead runtime instead of redialling the
        // socket, where a restarted runtime may already be listening.
        let served = tonic::transport::Server::builder()
            .layer(crate::api::layer(&runtime, entry))
            .add_routes(routes.routes())
            .add_optional_service(admin)
            .serve_with_incoming_shutdown(incoming(listener), std::future::pending::<()>())
            .await;
        if let Err(error) = served {
            tracing::warn!(%error, "a client socket stopped serving");
        }
    })
}
