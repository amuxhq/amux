//! amux's errors: `google.rpc.Status` values, gRPC's richer error model.
//!
//! The code is gRPC's own. What the code alone does not say rides in the
//! details as Any: a `google.rpc.ErrorInfo` naming one of amux's
//! [`ErrorReason`]s.

use crate::ErrorReason;
use crate::rpc::{Code, ErrorInfo, Status};

/// The domain of every `ErrorInfo` amux writes.
const DOMAIN: &str = "amux.sh";

const REASON_PREFIX: &str = "ERROR_REASON_";

impl Status {
    pub fn new(code: Code, message: impl Into<String>) -> Status {
        Status {
            code: code as i32,
            message: message.into(),
            details: Vec::new(),
        }
    }

    /// An error the code alone does not say: `reason`'s code, and an
    /// `ErrorInfo` naming it.
    pub fn because(reason: ErrorReason, message: impl Into<String>) -> Status {
        let code = match reason {
            ErrorReason::Unspecified => Code::Unknown,
            ErrorReason::HostUnreachable | ErrorReason::VersionMismatch => Code::FailedPrecondition,
            ErrorReason::PaymentRequired => Code::PermissionDenied,
        };
        let info = ErrorInfo {
            reason: reason
                .as_str_name()
                .trim_start_matches(REASON_PREFIX)
                .to_owned(),
            domain: DOMAIN.to_owned(),
            metadata: Default::default(),
        };
        let mut status = Status::new(code, message);
        status
            .details
            .push(prost_types::Any::from_msg(&info).expect("a message encodes into a Vec"));
        status
    }

    /// The code, or UNKNOWN for a number this build does not know.
    pub fn code(&self) -> Code {
        Code::try_from(self.code).unwrap_or(Code::Unknown)
    }

    /// The amux reason an `ErrorInfo` in the details names.
    pub fn reason(&self) -> Option<ErrorReason> {
        let info = self.detail::<ErrorInfo>()?;
        if info.domain != DOMAIN {
            return None;
        }
        ErrorReason::from_str_name(&format!("{REASON_PREFIX}{}", info.reason))
    }

    /// The first detail of type `M`.
    pub fn detail<M: prost::Name + Default>(&self) -> Option<M> {
        self.details
            .iter()
            .find(|any| any.type_url == M::type_url())
            .and_then(|any| any.to_msg().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reason_rides_as_error_info_on_its_code() {
        let status = Status::because(ErrorReason::HostUnreachable, "the desk is away");
        assert_eq!(status.code(), Code::FailedPrecondition);
        assert_eq!(status.reason(), Some(ErrorReason::HostUnreachable));
        let info = status.detail::<ErrorInfo>().expect("an ErrorInfo");
        assert_eq!(
            (info.reason.as_str(), info.domain.as_str()),
            ("HOST_UNREACHABLE", DOMAIN)
        );
        assert_eq!(
            status.details[0].type_url,
            "type.googleapis.com/google.rpc.ErrorInfo"
        );
        assert_eq!(Status::new(Code::NotFound, "no such agent").reason(), None);
    }
}
