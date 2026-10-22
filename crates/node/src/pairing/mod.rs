//! Pairing as a person runs it: the SSH exchange, the QR code's payload,
//! and the front door's side of an SSH pairing. The pairing window and the
//! secret exchange are the profile's core's (`daemon_core::pairing`).

pub(crate) mod qr;
pub(crate) mod ssh;

use std::time::Duration;

/// Stores the trust an SSH identity exchange established: the command
/// running either end of an SSH pairing implements this over the profile's
/// front door.
#[async_trait::async_trait]
pub trait PairingAdmin: Sync {
    fn profile_id(&self) -> crate::ProfileId;
    async fn pair_ssh_peer(
        &self,
        peer: ssh::SshPairingPeer,
        target: Option<ssh::SshTarget>,
    ) -> Result<(), String>;
}

/// How long a pairing window stays open unless asked otherwise.
pub(crate) const PAIR_MODE_TTL: Duration = Duration::from_secs(5 * 60);
