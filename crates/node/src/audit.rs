use std::fmt;

use chrono::{DateTime, Utc};

use crate::HostId;
use crate::routing::{LinkId, LinkRole};
use crate::trust::TrustStorePairingUpdate;

const TARGET: &str = "amux::audit";

pub(crate) mod category {
    pub(crate) const PAIRING_START: &str = "pairing.start";
    pub(crate) const PAIRING_SUCCESS: &str = "pairing.success";
    pub(crate) const PAIRING_FAILURE: &str = "pairing.failure";
    pub(crate) const PAIRING_CANCEL: &str = "pairing.cancel";
    pub(crate) const AUTH_MTLS_HANDSHAKE_FAILURE: &str = "auth.mtls_handshake_failure";
    pub(crate) const AUTH_JWT_FAILURE: &str = "auth.jwt_failure";
    pub(crate) const TRUST_INSERT: &str = "trust.insert";
    pub(crate) const TRUST_UPDATE: &str = "trust.update";
    pub(crate) const TRUST_REPLACE: &str = "trust.replace";
    pub(crate) const TRUST_REMOVE: &str = "trust.remove";
    pub(crate) const LINK_UP: &str = "link.up";
    pub(crate) const LINK_DOWN: &str = "link.down";
}

pub(crate) fn pairing_start(method: &str) {
    tracing::info!(
        target: TARGET,
        category = category::PAIRING_START,
        method,
        "pairing.start"
    );
}

pub(crate) fn pairing_success(method: &str, peer: HostId) {
    tracing::info!(
        target: TARGET,
        category = category::PAIRING_SUCCESS,
        method,
        host_id = %peer,
        "pairing.success"
    );
}

pub(crate) fn pairing_failure(method: &str, reason: impl fmt::Display) {
    tracing::warn!(
        target: TARGET,
        category = category::PAIRING_FAILURE,
        method,
        reason = %reason,
        "pairing.failure"
    );
}

pub(crate) fn pairing_cancel(method: &str) {
    tracing::info!(
        target: TARGET,
        category = category::PAIRING_CANCEL,
        method,
        "pairing.cancel"
    );
}

pub(crate) fn auth_mtls_handshake_failure(reason: impl fmt::Display) {
    tracing::warn!(
        target: TARGET,
        category = category::AUTH_MTLS_HANDSHAKE_FAILURE,
        reason = %reason,
        "auth.mtls_handshake_failure"
    );
}

pub(crate) fn auth_mtls_handshake_failure_for_host(host_id: HostId, reason: impl fmt::Display) {
    tracing::warn!(
        target: TARGET,
        category = category::AUTH_MTLS_HANDSHAKE_FAILURE,
        host_id = %host_id,
        reason = %reason,
        "auth.mtls_handshake_failure"
    );
}

pub(crate) fn auth_jwt_failure(reason: impl fmt::Display) {
    tracing::warn!(
        target: TARGET,
        category = category::AUTH_JWT_FAILURE,
        reason = %reason,
        "auth.jwt_failure"
    );
}

pub(crate) fn trust_pairing_update(host_id: HostId, outcome: &TrustStorePairingUpdate) {
    let category = match outcome {
        TrustStorePairingUpdate::Inserted => category::TRUST_INSERT,
        TrustStorePairingUpdate::Updated => category::TRUST_UPDATE,
        TrustStorePairingUpdate::ReplacedPubkey => category::TRUST_REPLACE,
        TrustStorePairingUpdate::PubkeyReplacementRequired => return,
    };
    tracing::info!(
        target: TARGET,
        category,
        host_id = %host_id,
        outcome = ?outcome,
        "trust update"
    );
}

pub(crate) fn trust_remove(
    host_id: HostId,
    name: &str,
    paired_at: DateTime<Utc>,
    removed_at: DateTime<Utc>,
    reason: &str,
) {
    tracing::info!(
        target: TARGET,
        category = category::TRUST_REMOVE,
        host_id = %host_id,
        name,
        paired_at = %paired_at.to_rfc3339(),
        removed_at = %removed_at.to_rfc3339(),
        reason,
        "trust.remove"
    );
}

pub(crate) fn link_up(host_id: HostId, link: &LinkId, role: LinkRole) {
    tracing::info!(
        target: TARGET,
        category = category::LINK_UP,
        host_id = %host_id,
        link = %link,
        role = ?role,
        "link.up"
    );
}

pub(crate) fn link_down(host_id: HostId, link: &LinkId, reason: impl fmt::Display) {
    tracing::info!(
        target: TARGET,
        category = category::LINK_DOWN,
        host_id = %host_id,
        link = %link,
        reason = %reason,
        "link.down"
    );
}
