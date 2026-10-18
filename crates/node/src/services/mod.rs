//! The network edge's services: pairing over the link, the direct links to
//! trusted peers discovery and trust make reachable, the cloud link, and the
//! relay a cloud deployment runs.

mod pairing;
mod reachability;

pub(crate) use pairing::{LocalPairingIdentity, PairingService};
pub(crate) use reachability::Dialer;
pub use reachability::ReachabilityLinkConnector;
