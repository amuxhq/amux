//! Routing primitives for the amux protocol.
//!
//! Two rules define routing (docs/PROTOCOL.md): advertise only adjacency,
//! forward only to adjacency. The carrier-independent link runtime lives in
//! `crate::link`; this module owns the wire-side adjacency discipline
//! (`link_registry`). The routes, what each host reads as and the host
//! catalogue are the profile's core's.

mod host;
mod link_registry;
mod types;

pub use host::local_kinds;
pub(crate) use host::{
    LiveLocalHost, OwnAddresses, RuntimeInstance, listed_addrs, local_host, local_platform,
};
pub(crate) use link_registry::{
    LinkAdmission, LinkCloseRequest, LinkOutputTx, LinkProperties, Registration, direct_order,
};
pub use link_registry::{LinkRegistry, LinkRole};
pub use types::{Capabilities, Host, LinkId, LiveRoute, Route, host_to_wire};
pub(crate) use types::{core_host, host_from_core, host_from_wire};
