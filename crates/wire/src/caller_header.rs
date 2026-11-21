//! The caller header: who a carried call is for, written beside the call
//! by the node that carries it (the protocol's `Caller` message).

use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use prost::Message as _;

use crate::amux::v1::{AgentCaller, Caller, Person, caller};

/// gRPC writes a binary header's value in base64 and lets a sender pad it
/// or not, so a reader takes either.
const HEADER_BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Why a caller header could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallerHeaderError {
    /// Not base64.
    Encoding,
    /// Not a Caller.
    Message,
    /// A Caller naming neither a person nor an agent.
    Empty,
}

impl std::fmt::Display for CallerHeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Encoding => "the caller header is not base64",
            Self::Message => "the caller header does not hold a caller",
            Self::Empty => "the caller header names no one",
        })
    }
}

impl std::error::Error for CallerHeaderError {}

impl Caller {
    /// A person at the carrying host, on the client `surface` names.
    pub fn person(surface: impl Into<String>) -> Self {
        Self {
            of: Some(caller::Of::Person(Person {
                surface: surface.into(),
            })),
        }
    }

    /// One of the carrying host's agents.
    pub fn agent(agent_id: impl Into<Vec<u8>>, name: impl Into<String>) -> Self {
        Self {
            of: Some(caller::Of::Agent(AgentCaller {
                agent_id: agent_id.into(),
                name: name.into(),
            })),
        }
    }

    /// The value of [`crate::CALLER_HEADER`] that carries this caller.
    pub fn to_header(&self) -> String {
        HEADER_BASE64.encode(self.encode_to_vec())
    }

    /// The caller a [`crate::CALLER_HEADER`] value carries. A caller that
    /// names no one is refused here, so whoever reads one always has a
    /// person or an agent.
    pub fn from_header(value: &[u8]) -> Result<Self, CallerHeaderError> {
        let bytes = HEADER_BASE64
            .decode(value)
            .map_err(|_| CallerHeaderError::Encoding)?;
        let caller = Self::decode(bytes.as_slice()).map_err(|_| CallerHeaderError::Message)?;
        if caller.of.is_none() {
            return Err(CallerHeaderError::Empty);
        }
        Ok(caller)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caller_round_trips_through_its_header() {
        for caller in [Caller::person("ios"), Caller::agent([7; 16], "coder")] {
            let header = caller.to_header();
            assert!(
                header.bytes().all(|byte| byte.is_ascii_graphic()),
                "{header} is not a header value"
            );
            assert_eq!(Caller::from_header(header.as_bytes()), Ok(caller));
        }
    }

    /// gRPC lets a sender pad a binary header's base64 or not.
    #[test]
    fn a_padded_header_reads_too() {
        // 26 bytes encoded, which base64 pads.
        let caller = Caller::agent([7; 16], "code");
        let padded = base64::engine::general_purpose::STANDARD.encode(caller.encode_to_vec());
        assert!(padded.ends_with('='));
        assert_eq!(Caller::from_header(padded.as_bytes()), Ok(caller));
    }

    #[test]
    fn a_header_that_names_no_one_is_refused() {
        assert_eq!(
            Caller::from_header(b"not base64!"),
            Err(CallerHeaderError::Encoding)
        );
        assert_eq!(
            Caller::from_header(HEADER_BASE64.encode([0xff, 0xff]).as_bytes()),
            Err(CallerHeaderError::Message)
        );
        assert_eq!(
            Caller::from_header(Caller::default().to_header().as_bytes()),
            Err(CallerHeaderError::Empty)
        );
    }
}
