use std::io;

use prost::Message as _;
use tokio::sync::{mpsc, oneshot};
use wire::{self, pb};

/// A bidirectional byte stream carried by a link.
pub type ByteStream = super::quic::QuicByteStream;

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
