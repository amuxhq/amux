//! The services on the link streams whose handshake presented a trusted
//! key: the host services a paired host carries its people's and agents'
//! calls to, and the peer service its daemon asks for itself, behind the
//! link's entry layer, which settles who is calling.

use std::sync::Weak;

use futures_util::stream;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::runtime::ProfileRuntime;
use crate::transport::BoxedGrpcIo;

/// Serves what a link serves to paired hosts on `incoming`, until
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
    let routes = crate::api::routes(&runtime, crate::api::Entry::Link);
    tokio::spawn(async move {
        let served = tonic::transport::Server::builder()
            .layer(crate::api::layer(&runtime, crate::api::Entry::Link))
            .add_routes(routes.routes())
            .serve_with_incoming_shutdown(incoming, super::wait_for(shutdown))
            .await;
        if let Err(error) = served {
            tracing::warn!(%error, "the services for paired hosts stopped");
        }
    })
}
