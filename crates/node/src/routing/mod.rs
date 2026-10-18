//! Routing primitives for the amux protocol.
//!
//! Two rules define routing (docs/PROTOCOL.md): advertise only adjacency,
//! forward only to adjacency. The carrier-independent link runtime lives in
//! `crate::link`; this module owns the wire-side adjacency discipline
//! (`link_registry`) and the derived local routing table (`core`).

mod core;
mod events;
mod host;
mod link;
mod link_registry;
mod types;
mod wire;

pub use core::RoutingCore;
pub(crate) use core::{RevocationMark, Revocations, RouteUpdateOutcome};

pub use events::RoutingEvent;
pub use host::local_kinds;
pub(crate) use host::{
    LiveLocalHost, RuntimeInstance, local_host, local_platform, validate_remote_host,
};
pub use link::ConnectRole;
pub(crate) use link::{
    ConnectHandshake, ConnectHandshakeEvent, protocol_error_hello_ack, protocol_error_link_close,
};
pub(crate) use link_registry::{
    DirectLinkOrder, LinkAdmission, LinkCloseRequest, LinkProperties, Registration,
};
pub use link_registry::{LinkCarrier, LinkRegistry, LinkRole};
pub use types::{Capabilities, Host, LinkId, LiveRoute, Route, host_to_wire};
pub(crate) use types::{core_host, host_from_wire};
pub(crate) use wire::{inbound_host_from_wire, neighbor_down_from_wire, neighbor_up_from_wire};

pub(crate) use crate::link::run::{
    AuthenticatedLinkContextProvider, LinkConnectorCtx, LinkConnectorRefreshReceiver,
    LinkConnectorRefreshRequest, LinkCtx, spawn_connector_with_auth_establishment_and_shutdown,
    spawn_connector_with_establishment_and_shutdown,
};
pub use crate::link::run::{
    AuthenticatedLinkUser, LinkConnectorAuth, LinkConnectorToken, LinkConnectorTokenRefresher,
    LinkTokenAuthenticator,
};
