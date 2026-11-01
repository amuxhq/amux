//! The client services on the link streams whose handshake presented a
//! trusted key: a paired daemon calls the same AgentService and
//! SessionService a client does, answered by [`ClientApi::for_peer`] only
//! for this host's own agents.

use std::sync::Weak;

use futures_util::stream;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::grpc::ClientApi;
use crate::runtime::ProfileRuntime;
use crate::transport::BoxedGrpcIo;

/// Serves the client services to paired daemons on `incoming`, until
/// shutdown.
pub(super) fn serve(
    runtime: Weak<ProfileRuntime>,
    incoming: mpsc::Receiver<BoxedGrpcIo>,
    shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    let incoming = stream::unfold(incoming, |mut incoming| async move {
        incoming
            .recv()
            .await
            .map(|io| (Ok::<_, std::io::Error>(io), incoming))
    });
    let service = ClientApi::for_peer(runtime);
    tokio::spawn(async move {
        let served = tonic::transport::Server::builder()
            .add_service(wire::agent_service_server(service.clone()))
            .add_service(wire::session_service_server(service))
            .serve_with_incoming_shutdown(incoming, super::wait_for(shutdown))
            .await;
        if let Err(error) = served {
            tracing::warn!(%error, "the services for paired hosts stopped");
        }
    })
}
