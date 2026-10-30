//! Wire conversions for adjacency events.
//!
//! The wire vocabulary is deliberately tiny: `NeighborUp { host }` claims
//! "I have a direct link to this host", `NeighborDown { host_id }` withdraws
//! it. Nothing transitive is expressible.

use wire::pb;

use crate::HostId;
use crate::routing::{Host, host_to_wire};

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
