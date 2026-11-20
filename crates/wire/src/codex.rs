//! Codex's named permission presets: amux's name for each combination of
//! Codex's own settings. Both the daemon (when building a new agent's spec)
//! and the Codex interpreter share this table without the daemon linking any
//! provider crate.

/// A permission Codex offers: amux's name for one combination of Codex's
/// own settings. Settings that match none read as custom.
pub struct NamedPermission {
    pub value: &'static str,
    pub display_name: &'static str,
    pub approval_policy: &'static str,
    pub sandbox: &'static str,
    /// Who answers approvals: the person, or Codex's reviewer model.
    pub approvals_reviewer: &'static str,
    pub normal: bool,
    pub never_asks: bool,
}

const fn named(
    value: &'static str,
    display_name: &'static str,
    approval_policy: &'static str,
    sandbox: &'static str,
    approvals_reviewer: &'static str,
) -> NamedPermission {
    NamedPermission {
        value,
        display_name,
        approval_policy,
        sandbox,
        approvals_reviewer,
        normal: false,
        never_asks: false,
    }
}

/// Codex's permissions, from the least it may do alone to the most.
pub const PERMISSIONS: [NamedPermission; 4] = [
    named("read-only", "Read only", "on-request", "read-only", "user"),
    NamedPermission {
        normal: true,
        ..named("default", "Ask", "on-request", "workspace-write", "user")
    },
    named(
        "auto",
        "Auto",
        "on-request",
        "workspace-write",
        "auto_review",
    ),
    NamedPermission {
        never_asks: true,
        ..named(
            "full-access",
            "Full access",
            "never",
            "danger-full-access",
            "user",
        )
    },
];

/// The permission a catalogue value names.
pub fn permission(value: &str) -> Option<&'static NamedPermission> {
    PERMISSIONS.iter().find(|named| named.value == value)
}
