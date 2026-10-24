use std::io;

use futures_util::future::BoxFuture;
use prost::Message as _;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use wire::{self, pb};

/// A bidirectional byte stream carried by a link.
pub trait AsyncStream: AsyncRead + AsyncWrite + Send + Unpin {
    /// Abandons the stream both ways: the far end's reads and writes fail.
    fn reset(&mut self);
    /// Whether the far end opened the stream in early data, before the
    /// link's handshake had finished.
    fn arrived_early(&self) -> bool {
        false
    }
}

pub type ByteStream = Box<dyn AsyncStream>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CarrierKind {
    Quic,
    RelayQuic,
    RelayTcp,
    Ssh,
}

impl CarrierKind {
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
    #[error("link closed")]
    LinkClosed,
    #[error("stream I/O failed: {0}")]
    Io(#[source] io::Error),
}

pub trait LinkCarrier: Send + Sync + 'static {
    fn kind(&self) -> CarrierKind;
    fn control(&self) -> (ControlSink, ControlSource);
    fn open_stream(&self) -> BoxFuture<'_, Result<ByteStream, OpenError>>;
    fn accept_stream(&self) -> BoxFuture<'_, Option<ByteStream>>;
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
    /// What became of the data the link sent before its handshake
    /// finished: none was sent unless the dial resumed with a ticket.
    fn early_data(&self) -> super::link_log::EarlyData {
        super::link_log::EarlyData::None
    }
    /// The forwarding link a link through a forwarder is carried on: it
    /// has no other path, so it ends when that link does.
    fn forwarded_through(&self) -> Option<daemon_core::LinkId> {
        None
    }
    /// What the carrier's path has seen so far, for the link's logs: round
    /// trip, losses, packets. Empty where the carrier keeps no such count.
    /// The address the far end's packets arrive from, as this side's QUIC
    /// endpoint sees it: on a link socket, the far end's stand-in.
    fn remote_address(&self) -> Option<std::net::SocketAddr> {
        None
    }
    /// Whether the far end dialled this link. Its address is then the one
    /// the far end sends from, and moves when the far end's network does.
    fn accepted(&self) -> bool {
        false
    }

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
