//! In-process tonic transport helpers.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_util::sync::CancellationToken;

pub(crate) struct ShutdownIo<T> {
    inner: T,
    _cancellation: Arc<CancellationToken>,
    cancelled: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl<T> ShutdownIo<T> {
    pub(crate) fn new_shared(inner: T, cancellation: Arc<CancellationToken>) -> Self {
        Self {
            inner,
            cancelled: Box::pin(cancellation.as_ref().clone().cancelled_owned()),
            _cancellation: cancellation,
        }
    }
}

impl<T: tonic::transport::server::Connected> tonic::transport::server::Connected for ShutdownIo<T> {
    type ConnectInfo = T::ConnectInfo;
    fn connect_info(&self) -> Self::ConnectInfo {
        self.inner.connect_info()
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for ShutdownIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for ShutdownIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "profile connection closed",
            )));
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "profile connection closed",
            )));
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
