use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use uuid::Uuid;

use crate::routing::{Capabilities, Host};

pub(crate) const MAX_KINDS: usize = 64;
pub(crate) const MAX_HOST_NAME_BYTES: usize = 256;

/// One life of a host's runtime, drawn at random when the runtime starts.
///
/// A host id outlives any process that holds it, so a peer cannot tell from
/// the id alone whether a new link comes from the process it already has a
/// link to. A killed or crashed process closes nothing, and its links look
/// healthy until they idle out. Every link handshake carries the sender's
/// runtime instance id, and a different one proves the older links are dead.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct Incarnation(Uuid);

impl Incarnation {
    pub(crate) fn random() -> Self {
        Self(Uuid::new_v4())
    }

    pub(crate) fn to_wire(self) -> Vec<u8> {
        self.0.as_bytes().to_vec()
    }

    pub(crate) fn from_wire(bytes: &[u8]) -> Result<Self, String> {
        Uuid::from_slice(bytes).map(Self).map_err(|_| {
            format!(
                "a runtime instance id must be 16 bytes, got {}",
                bytes.len()
            )
        })
    }
}

/// The stable local identity plus the profile binding fact read by each new
/// link handshake. Credential changes do not restart routing services, so the
/// binding bit must remain live while the rest of the host description stays
/// immutable.
#[derive(Clone)]
pub(crate) struct LiveLocalHost {
    host: Host,
    signed_in: Arc<AtomicBool>,
    incarnation: Incarnation,
}

impl LiveLocalHost {
    pub(crate) fn new(host: Host) -> Self {
        Self {
            signed_in: Arc::new(AtomicBool::new(host.signed_in.unwrap_or(false))),
            host,
            incarnation: Incarnation::random(),
        }
    }

    pub(crate) fn id(&self) -> Uuid {
        self.host.id
    }

    pub(crate) fn incarnation(&self) -> Incarnation {
        self.incarnation
    }

    pub(crate) fn snapshot(&self) -> Host {
        let mut host = self.host.clone();
        host.signed_in = Some(self.signed_in.load(Ordering::Acquire));
        host
    }

    pub(crate) fn set_signed_in(&self, signed_in: bool) {
        self.signed_in.store(signed_in, Ordering::Release);
    }
}

pub(crate) fn local_host(
    host_id: Uuid,
    host_name: &str,
    capabilities: Capabilities,
    signed_in: bool,
) -> Host {
    Host {
        id: host_id,
        name: host_name.to_string(),
        version: crate::version().to_owned(),
        capabilities,
        signed_in: Some(signed_in),
        platform: Some(local_platform().to_string()),
    }
}

/// The name this machine goes by on a screen listing machines.
///
/// The operating system the daemon was compiled for, and nothing more. A
/// model name like "Mac Studio" is not something a process can read without a
/// hardware lookup table, and a table of marketing names would be this
/// program guessing what computer it is running on.
pub(crate) fn local_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macOS",
        "linux" => "Linux",
        "windows" => "Windows",
        other => other,
    }
}

/// The agent kinds this build hosts on this platform: terminal Claude is
/// hosted on Unix only.
pub fn local_kinds() -> Vec<wire::Kind> {
    let mut kinds = Vec::new();
    if cfg!(unix) {
        kinds.push(wire::Kind::ClaudePty);
    }
    kinds.extend([wire::Kind::ClaudeSdk, wire::Kind::Codex]);
    kinds
}

pub(crate) fn validate_remote_host(host: &Host) -> std::result::Result<(), String> {
    if host.id == Uuid::nil() {
        return Err("host_id must be non-zero".to_string());
    }
    if host.name.is_empty() {
        return Err("host name must be non-empty".to_string());
    }
    if host.name.len() > MAX_HOST_NAME_BYTES {
        return Err(format!(
            "host name must be at most {MAX_HOST_NAME_BYTES} bytes"
        ));
    }
    if host.version.is_empty() {
        return Err("host version must be non-empty".to_string());
    }
    if host.capabilities.kinds.len() > MAX_KINDS {
        return Err(format!("a host runs at most {MAX_KINDS} agent kinds"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_remote_host_rejects_too_many_kinds() {
        let mut host = local_host(Uuid::from_u128(1), "peer", Capabilities::default(), false);
        host.capabilities.kinds = vec![1; MAX_KINDS + 1];

        let error = validate_remote_host(&host).expect_err("host should exceed the cap");

        assert!(error.contains("agent kinds"));
    }

    #[test]
    fn validate_remote_host_rejects_oversized_name() {
        let host = local_host(
            Uuid::from_u128(1),
            &"a".repeat(MAX_HOST_NAME_BYTES + 1),
            Capabilities::default(),
            false,
        );

        let error = validate_remote_host(&host).expect_err("host name should exceed the cap");

        assert!(error.contains("host name"));
    }

    #[test]
    fn cloud_local_host_advertises_no_kinds() {
        let host = local_host(
            Uuid::from_u128(1),
            "cloud",
            Capabilities {
                cloud_relay: true,
                ..Default::default()
            },
            false,
        );

        assert!(host.capabilities.kinds.is_empty());
        assert!(host.capabilities.cloud_relay);
    }

    #[test]
    fn supplied_capabilities_are_advertised_verbatim() {
        let host = local_host(Uuid::from_u128(1), "host", Capabilities::default(), false);

        assert!(host.capabilities.kinds.is_empty());
    }
}
