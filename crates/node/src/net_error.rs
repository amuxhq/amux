//! Wire errors the network edge sends and reads: link handshake refusals,
//! link-close reasons and pairing failures, carried as `wire::Error` in
//! protocol messages and in gRPC status details.

use prost::Message as _;
use wire::{Error, ErrorCode, ProtocolVersionMismatch};

pub(crate) use crate::grpc::status;

pub(crate) fn error(code: ErrorCode, message: impl Into<String>) -> Error {
    crate::grpc::wire_error(code, message)
}

pub(crate) fn invalid_argument(message: impl Into<String>) -> Error {
    error(ErrorCode::InvalidArgument, message)
}

/// The one break-glass refusal: the peers share no protocol version.
pub(crate) fn version_mismatch(supported: Vec<u32>, peer: Vec<u32>) -> Error {
    let detail = ProtocolVersionMismatch {
        supported_protocol_versions: supported.clone(),
        peer_supported_protocol_versions: peer.clone(),
    };
    Error::new(
        ErrorCode::FailedPrecondition,
        format!(
            "the other machine needs updating (this one speaks protocol versions {supported:?}, \
             it speaks {peer:?})"
        ),
    )
    .with_detail(&detail)
}

/// Whether an error is the version refusal.
pub(crate) fn is_version_mismatch(error: &Error) -> bool {
    error.detail::<ProtocolVersionMismatch>().is_some()
}

/// The wire error a status carries in its details, when it carries one.
pub(crate) fn from_status(status: &tonic::Status) -> Option<Error> {
    Error::decode(status.details())
        .ok()
        .filter(|error| error.code != 0)
}
