use std::future::Future;
use std::io;
use std::pin::Pin;

use futures_util::future::BoxFuture;
use prost::Message as _;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use wire::{self, pb};

/// A bidirectional byte stream carried by a link.
pub trait AsyncStream: AsyncRead + AsyncWrite + Send + Unpin {
    /// Rejects an unopened stream, preserving the application refusal reason.
    fn reset(&mut self, code: pb::StreamRefusal) -> BoxFuture<'_, io::Result<()>>;

    /// Waits for the far end to accept a stream this end opened. A stream
    /// is handed back as soon as its preface is written, so that the bytes
    /// behind it leave in the same flight; an opener that must know the
    /// answer before it sends, or wants a refusal's reason rather than a
    /// failed read, waits here.
    fn accepted(&mut self) -> BoxFuture<'_, Result<(), OpenError>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

pub type ByteStream = Box<dyn AsyncStream>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CarrierKind {
    Quic,
    // Relay status must distinguish QUIC from direct QUIC even before the
    // relay listener starts constructing this carrier kind.
    #[allow(dead_code)]
    RelayQuic,
    RelayTcp,
    Ssh,
}

impl CarrierKind {
    /// Whether the link's own handshake pinned the adjacent host's key, as
    /// on every host-to-host link; a stream between those two hosts then
    /// needs no handshake of its own. A relay's link pinned only the relay.
    pub fn pins_peer(self) -> bool {
        matches!(self, Self::Quic | Self::Ssh)
    }

    /// The path a carrier of this kind runs on, where nothing more is
    /// known: a QUIC link's address says whether it is on the same network.
    pub fn path(self) -> super::link_log::PathKind {
        use super::link_log::PathKind;
        match self {
            Self::Quic => PathKind::Direct,
            Self::RelayQuic => PathKind::RelayUdp,
            Self::RelayTcp => PathKind::RelayTcp,
            Self::Ssh => PathKind::Ssh,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("stream refused: {0:?}")]
    Refused(pb::StreamRefusal),
    #[error("link closed")]
    LinkClosed,
    #[error("stream I/O failed: {0}")]
    Io(#[source] io::Error),
}

impl OpenError {
    /// A refusal read from a stream that was not waited on, as the I/O
    /// error it surfaces as there.
    pub(crate) fn into_io(self) -> io::Error {
        match self {
            Self::Refused(reason) => {
                io::Error::new(io::ErrorKind::ConnectionRefused, Self::Refused(reason))
            }
            Self::LinkClosed => io::Error::new(io::ErrorKind::NotConnected, Self::LinkClosed),
            Self::Io(error) => error,
        }
    }

    /// The refusal an I/O error carries, if it was one.
    pub(crate) fn refusal(error: &io::Error) -> Option<pb::StreamRefusal> {
        match Self::carried(error) {
            Some(Self::Refused(reason)) => Some(*reason),
            _ => None,
        }
    }

    /// What an I/O error from a stream that was not waited on says about
    /// its open, if that is what it says.
    pub(crate) fn carried(error: &io::Error) -> Option<&Self> {
        error.get_ref()?.downcast_ref::<Self>()
    }
}

pub trait LinkCarrier: Send + Sync + 'static {
    fn kind(&self) -> CarrierKind;
    fn control(&self) -> (ControlSink, ControlSource);
    fn open_stream(
        &self,
        preface: pb::StreamPreface,
    ) -> BoxFuture<'_, Result<ByteStream, OpenError>>;
    fn accept_stream(&self) -> BoxFuture<'_, Option<(pb::StreamPreface, ByteStream)>>;
    fn close(&self, reason: pb::LinkCloseReason);
    fn closed(&self) -> BoxFuture<'_, pb::LinkCloseReason>;
    /// Why the connection closed, if it has: read at once, for a link
    /// whose control stream ended before [`LinkCarrier::closed`] said so.
    fn close_reason(&self) -> Option<pb::LinkCloseReason> {
        None
    }
    /// The path the link's packets take, as the link log names it.
    fn path_kind(&self) -> super::link_log::PathKind {
        self.kind().path()
    }
    /// What the carrier's path has seen so far, for the link's logs: round
    /// trip, losses, packets. Empty where the carrier keeps no such count.
    fn path_stats(&self) -> String {
        String::new()
    }
}

pub(super) struct ControlWrite {
    pub(super) bytes: Vec<u8>,
    pub(super) declared_len: Option<u32>,
    pub(super) result: oneshot::Sender<io::Result<()>>,
}

pub struct ControlSink {
    pub(super) tx: mpsc::Sender<ControlWrite>,
}

pub struct ControlSource {
    pub(super) rx: mpsc::Receiver<io::Result<pb::Message>>,
}

pub async fn write_message(sink: &mut ControlSink, message: &pb::Message) -> io::Result<()> {
    let encoded_len = message.encoded_len();
    if encoded_len > wire::MESSAGE_SIZE_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "control message is {encoded_len} bytes; limit is {}",
                wire::MESSAGE_SIZE_LIMIT
            ),
        ));
    }

    let mut bytes = Vec::with_capacity(encoded_len);
    message
        .encode(&mut bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let (result, completed) = oneshot::channel();
    sink.tx
        .send(ControlWrite {
            bytes,
            declared_len: None,
            result,
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "control stream closed"))?;
    completed
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "control stream closed"))?
}

pub async fn read_message(source: &mut ControlSource) -> io::Result<Option<pb::Message>> {
    source.rx.recv().await.transpose()
}

pub(super) type BoxIoFuture<'a, T> = Pin<Box<dyn Future<Output = io::Result<T>> + Send + 'a>>;
