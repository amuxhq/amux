//! TCP socket helpers.

use std::io;

use futures_util::{Stream, stream};
use tokio::net::{TcpListener, TcpStream};

pub(crate) fn tcp_incoming(
    listener: TcpListener,
) -> impl Stream<Item = io::Result<TcpStream>> + Send + 'static {
    stream::unfold(listener, |listener| async move {
        let item = match listener.accept().await {
            Ok((stream, _addr)) => {
                if let Err(error) = stream.set_nodelay(true) {
                    tracing::warn!(error = %error, "failed to set TCP_NODELAY");
                }
                Ok(stream)
            }
            Err(error) => Err(error),
        };
        Some((item, listener))
    })
}
