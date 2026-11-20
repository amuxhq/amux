//! What a failed call says, worded once for every client.
//!
//! A call addressed to another host is carried there by the node the
//! client is attached to, and the carrier turns what happened on the way
//! into the same statuses for every service:
//!
//! - it never left: no link, the host not trusted, or no channel in time
//!   (`FAILED_PRECONDITION`, reason `HOST_UNREACHABLE`); only the relay
//!   reaches the host and the plan lapsed (`PERMISSION_DENIED`, reason
//!   `PAYMENT_REQUIRED`); or the host no longer trusts this one
//!   (`UNAUTHENTICATED`);
//! - it left and was lost: no answer began in time, or the link failed
//!   under it (`ABORTED`), so the owner may have acted on it.
//!
//! Anything else is the owner's own answer, words and all, except that a
//! host asked for a call its build does not have answers with no words at
//! all, and the client says the host needs updating.

use prost::Message as _;
use tonic::{Code, Status};
use wire::{ErrorCode, ErrorReason};

use crate::RpcError;

/// Why a call never left the node the client is attached to for the host
/// it was addressed to. Nothing happened there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotSent {
    /// No link to the host, or none came up in time.
    Unreachable,
    /// Only the relay reaches the host, and the account's plan lapsed to
    /// one that carries no calls between hosts.
    Lapsed,
    /// The host no longer trusts this one.
    Revoked,
}

/// Said when a host answers that it does not trust this one and gives no
/// words of its own.
const REVOKED: &str = "that host no longer trusts this one; pair them again";

impl RpcError {
    /// Why the call never reached the host it was addressed to, when the
    /// node could not carry it there.
    pub fn not_sent(&self) -> Option<NotSent> {
        let RpcError::Refused(error) = self else {
            return None;
        };
        match (error.code(), error.reason()) {
            (_, Some(ErrorReason::HostUnreachable)) => Some(NotSent::Unreachable),
            (_, Some(ErrorReason::PaymentRequired)) => Some(NotSent::Lapsed),
            (ErrorCode::Unauthenticated, _) => Some(NotSent::Revoked),
            _ => None,
        }
    }

    /// Whether the call may have taken effect though no answer came: the
    /// connection failed, or the call left for its host and was lost.
    pub fn uncertain(&self) -> bool {
        match self {
            RpcError::Transport(_) => true,
            RpcError::Refused(error) => error.code() == ErrorCode::Aborted,
        }
    }

    /// The status a server answers with for this error: what a fake of a
    /// node's services hands back, so a client reads it as it reads the
    /// node's own.
    pub fn into_status(self) -> Status {
        match self {
            RpcError::Transport(words) => Status::unavailable(words),
            RpcError::Refused(error) => {
                let code = match Code::from_i32(error.code) {
                    Code::Ok => Code::Internal,
                    code => code,
                };
                Status::with_details(code, error.message.clone(), error.encode_to_vec().into())
            }
        }
    }
}

impl From<Status> for RpcError {
    /// The node and the hosts behind it put their whole error in the
    /// status details; a status without one came from the transport, or
    /// from a service that is no longer running, and says nothing about
    /// whether the call landed.
    fn from(status: Status) -> RpcError {
        if let Ok(error) = wire::Error::decode(status.details())
            && error.code != ErrorCode::Ok as i32
        {
            return RpcError::Refused(error);
        }
        match status.code() {
            Code::Unavailable
            | Code::Unknown
            | Code::Cancelled
            | Code::DeadlineExceeded
            | Code::Internal
            | Code::Aborted
            | Code::Ok => RpcError::Transport(status.message().to_owned()),
            // gRPC answers a method the server does not have with no words.
            Code::Unimplemented if status.message().is_empty() => RpcError::Refused(
                wire::Error::new(ErrorCode::Unimplemented, wire::HOST_NEEDS_UPDATE),
            ),
            Code::Unauthenticated if status.message().is_empty() => {
                RpcError::Refused(wire::Error::new(ErrorCode::Unauthenticated, REVOKED))
            }
            // tonic's codes are gRPC's, as an error's are.
            code => RpcError::Refused(wire::Error::new(
                ErrorCode::try_from(code as i32).unwrap_or(ErrorCode::Unknown),
                status.message(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A status as the node writes one: the whole error in the details.
    fn written(error: wire::Error) -> Status {
        RpcError::Refused(error).into_status()
    }

    #[test]
    fn a_status_carrying_the_runtimes_error_is_that_error() {
        let error = wire::Error::because(
            ErrorReason::HostUnreachable,
            "older history is held by the agent's host",
        );
        let rpc = RpcError::from(written(error.clone()));
        assert_eq!(rpc, RpcError::Refused(error));
        assert_eq!(rpc.code(), Some(ErrorCode::FailedPrecondition));
        assert_eq!(rpc.reason(), Some(ErrorReason::HostUnreachable));
    }

    /// Each way a carried call fails, as the carrier writes it, read the
    /// way every client reads it.
    #[test]
    fn each_carried_failure_says_whether_it_left_and_keeps_its_words() {
        let unreachable = RpcError::from(written(wire::Error::because(
            ErrorReason::HostUnreachable,
            "desk cannot be reached",
        )));
        assert_eq!(unreachable.not_sent(), Some(NotSent::Unreachable));
        assert!(!unreachable.uncertain());
        assert_eq!(unreachable.to_string(), "desk cannot be reached");

        let lapsed = RpcError::from(written(wire::Error::because(
            ErrorReason::PaymentRequired,
            "desk can be reached only through the relay",
        )));
        assert_eq!(lapsed.not_sent(), Some(NotSent::Lapsed));
        assert_eq!(lapsed.code(), Some(ErrorCode::PermissionDenied));
        assert!(!lapsed.uncertain());

        let revoked = RpcError::from(Status::unauthenticated("this host's trust was revoked"));
        assert_eq!(revoked.not_sent(), Some(NotSent::Revoked));
        assert_eq!(revoked.to_string(), "this host's trust was revoked");
        let wordless = RpcError::from(Status::unauthenticated(""));
        assert_eq!(wordless.not_sent(), Some(NotSent::Revoked));
        assert_eq!(wordless.to_string(), REVOKED);

        let aborted = RpcError::from(written(wire::Error::new(
            ErrorCode::Aborted,
            "desk stopped answering; the call may or may not have arrived",
        )));
        assert_eq!(aborted.not_sent(), None);
        assert!(aborted.uncertain());
        assert!(RpcError::from(Status::aborted("link reset")).uncertain());

        let answered = RpcError::from(written(wire::Error::new(
            ErrorCode::PermissionDenied,
            "an agent settles only its own children",
        )));
        assert_eq!(answered.not_sent(), None);
        assert!(!answered.uncertain());
    }

    /// gRPC answers a method the server does not have with no words at all.
    #[test]
    fn an_unknown_method_says_the_host_needs_updating() {
        let rpc = RpcError::from(Status::unimplemented(""));
        assert_eq!(rpc.code(), Some(ErrorCode::Unimplemented));
        assert_eq!(rpc.to_string(), wire::HOST_NEEDS_UPDATE);
        assert_eq!(
            RpcError::from(Status::unimplemented("no such thing here")).to_string(),
            "no such thing here",
            "a refusal with its own words keeps them"
        );
    }

    #[test]
    fn a_bare_status_is_a_transport_failure_only_when_it_says_nothing_about_the_call() {
        for code in [
            Code::Unavailable,
            Code::Unknown,
            Code::Cancelled,
            Code::Internal,
        ] {
            assert!(
                RpcError::from(Status::new(code, "connection reset")).is_transport(),
                "{code:?}"
            );
        }
        let refused = RpcError::from(Status::not_found("no such agent"));
        assert_eq!(refused.code(), Some(ErrorCode::NotFound));
    }

    /// What a fake hands back reads as what it was handed.
    #[test]
    fn an_error_round_trips_through_its_status() {
        for error in [
            RpcError::Transport("connection reset".into()),
            RpcError::Refused(wire::Error::new(ErrorCode::NotFound, "no such agent")),
            RpcError::Refused(wire::Error::because(ErrorReason::PaymentRequired, "lapsed")),
        ] {
            assert_eq!(RpcError::from(error.clone().into_status()), error);
        }
    }
}
