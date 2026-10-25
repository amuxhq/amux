pub use relay_core::Tier;
use serde::Deserialize;

/// Claims from a cloud routing connection token.
#[derive(Debug, Deserialize)]
pub struct ConnectionClaims {
    /// User ID (subject).
    pub sub: String,
    /// Auth client that requested the cloud connection token.
    pub client_id: String,
    /// Expected host this token is for.
    pub host: String,
    /// Expected port this token is for.
    pub port: u16,
    /// Expiration time as seconds since Unix epoch.
    pub exp: u64,
    /// Relay entitlement carried by this token.
    pub tier: Tier,
}
