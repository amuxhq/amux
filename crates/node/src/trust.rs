//! The trust store the handshakes read. The core owns the store, keeps it
//! in the profile's store, and is its only writer: every change is a core
//! step, and the driver publishes each store the core serves into the
//! shared copy here.

pub use daemon_core::trust::{Reachability, TrustChange, TrustEntry, TrustStore};

/// The trust store as the core last published it, for the handshakes and
/// the shell's reads. Only the driver writes it.
pub type SharedTrustStore = std::sync::Arc<std::sync::RwLock<TrustStore>>;
