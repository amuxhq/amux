//! Wire conversions for adjacency events.
//!
//! The wire vocabulary is deliberately tiny: `NeighborUp { host }` claims
//! "I have a direct link to this host", `NeighborDown { host_id }` withdraws
//! it. Nothing transitive is expressible.

use uuid::Uuid;
use wire::pb;

use crate::HostId;
use crate::routing::{Host, host_from_wire, host_to_wire, validate_remote_host};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum WireNeighborEventError {
    #[error("{0} is missing")]
    MissingField(&'static str),
    #[error("{field} is invalid: {reason}")]
    InvalidField { field: &'static str, reason: String },
}

pub(crate) fn neighbor_up_message(host: &Host) -> pb::Message {
    pb::Message {
        body: Some(pb::message::Body::NeighborUp(pb::NeighborUp {
            host: Some(host_to_wire(host)),
        })),
    }
}

pub(crate) fn neighbor_down_message(host_id: HostId) -> pb::Message {
    pb::Message {
        body: Some(pb::message::Body::NeighborDown(pb::NeighborDown {
            host_id: host_id.as_bytes().to_vec(),
        })),
    }
}

pub(crate) fn neighbor_up_from_wire(event: pb::NeighborUp) -> Result<Host, WireNeighborEventError> {
    let host = event
        .host
        .ok_or(WireNeighborEventError::MissingField("NeighborUp.host"))?;
    inbound_host_from_wire(host, "NeighborUp.host")
}

pub(crate) fn neighbor_down_from_wire(
    event: pb::NeighborDown,
) -> Result<HostId, WireNeighborEventError> {
    uuid_from_bytes("NeighborDown.host_id", &event.host_id)
}

/// Decodes and semantically validates one snapshot/event `Host`.
pub(crate) fn inbound_host_from_wire(
    host: pb::Host,
    field: &'static str,
) -> Result<Host, WireNeighborEventError> {
    let host = host_from_wire(host).map_err(|error| WireNeighborEventError::InvalidField {
        field,
        reason: error.to_string(),
    })?;
    validate_remote_host(&host)
        .map_err(|reason| WireNeighborEventError::InvalidField { field, reason })?;
    Ok(host)
}

fn uuid_from_bytes(field: &'static str, bytes: &[u8]) -> Result<Uuid, WireNeighborEventError> {
    Uuid::from_slice(bytes).map_err(|error| WireNeighborEventError::InvalidField {
        field,
        reason: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::Capabilities;

    fn host(id: u128) -> Host {
        Host {
            platform: None,
            id: HostId::from_u128(id),
            name: format!("host-{id}"),
            version: "test".to_string(),
            capabilities: Capabilities {
                cloud_relay: false,
                kinds: vec![wire::Kind::ClaudeSdk as i32],
            },
            signed_in: Some(true),
        }
    }

    #[test]
    fn neighbor_up_round_trips_and_validates_the_host() {
        let pb::Message {
            body: Some(pb::message::Body::NeighborUp(up)),
        } = neighbor_up_message(&host(1))
        else {
            panic!("expected NeighborUp body");
        };

        assert_eq!(neighbor_up_from_wire(up).unwrap(), host(1));
    }

    #[test]
    fn neighbor_up_rejects_missing_and_semantically_invalid_hosts() {
        assert!(matches!(
            neighbor_up_from_wire(pb::NeighborUp { host: None }),
            Err(WireNeighborEventError::MissingField("NeighborUp.host"))
        ));

        let mut nameless = host(1);
        nameless.name.clear();
        assert!(matches!(
            neighbor_up_from_wire(pb::NeighborUp {
                host: Some(host_to_wire(&nameless)),
            }),
            Err(WireNeighborEventError::InvalidField { .. })
        ));
    }

    #[test]
    fn neighbor_down_requires_a_well_formed_host_id() {
        let pb::Message {
            body: Some(pb::message::Body::NeighborDown(down)),
        } = neighbor_down_message(HostId::from_u128(2))
        else {
            panic!("expected NeighborDown body");
        };
        assert_eq!(neighbor_down_from_wire(down).unwrap(), HostId::from_u128(2));

        assert!(matches!(
            neighbor_down_from_wire(pb::NeighborDown {
                host_id: vec![1, 2, 3],
            }),
            Err(WireNeighborEventError::InvalidField {
                field: "NeighborDown.host_id",
                ..
            })
        ));
    }
}
