//! Wire errors the network edge sends and reads: link handshake refusals,
//! link-close reasons and pairing failures, carried as `wire::Error` in
//! protocol messages and in gRPC status details.

use prost::Message as _;
use wire::{Error, ErrorCode};

pub(crate) use crate::grpc::status;

pub(crate) fn error(code: ErrorCode, message: impl Into<String>) -> Error {
    crate::grpc::wire_error(code, message)
}

pub(crate) fn invalid_argument(message: impl Into<String>) -> Error {
    error(ErrorCode::InvalidArgument, message)
}

/// The one break-glass refusal: the two ends of a link speak different
/// protocol versions.
pub(crate) fn version_mismatch() -> Error {
    Error::because(
        wire::ErrorReason::VersionMismatch,
        "the other machine needs updating",
    )
}

/// The wire error a status carries in its details, when it carries one.
pub(crate) fn from_status(status: &tonic::Status) -> Option<Error> {
    Error::decode(status.details())
        .ok()
        .filter(|error| error.code != 0)
}
