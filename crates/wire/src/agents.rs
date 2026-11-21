//! The ACP agents amux knows without being told: each one's id, the name
//! people see, and the command that starts its ACP server. A host offers a
//! built-in only when its command is found; a person's own `[agents.<id>]`
//! entry in the config adds an agent or, under a built-in's id, replaces it.
//!
//! The commands are the ones the agents' makers publish for ACP clients,
//! as listed in the official ACP registry. The registry's own Antigravity
//! entry downloads a separate server binary; the `agy` command people
//! already have serves ACP itself, so that is the one used.

/// One agent amux can start without configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuiltIn {
    /// The agent's id: the provider string on the wire, the key of its
    /// remembered picks and of its `[agents.<id>]` config entry.
    pub id: &'static str,
    /// What people call it.
    pub name: &'static str,
    /// The program, looked up on the daemon's PATH.
    pub command: &'static str,
    /// The arguments that make the program serve ACP on its stdio.
    pub args: &'static [&'static str],
}

pub const BUILT_INS: [BuiltIn; 3] = [
    BuiltIn {
        id: "antigravity",
        name: "Antigravity",
        command: "agy",
        args: &["acp"],
    },
    BuiltIn {
        id: "opencode",
        name: "OpenCode",
        command: "opencode",
        args: &["acp"],
    },
    BuiltIn {
        id: "grok",
        name: "Grok",
        command: "grok",
        args: &["agent", "stdio"],
    },
];

/// The built-in agent with this id, if there is one.
pub fn built_in(id: &str) -> Option<&'static BuiltIn> {
    BUILT_INS.iter().find(|agent| agent.id == id)
}

/// Claude Code's provider id, for both of its kinds.
pub const CLAUDE: &str = "claude";
/// Codex's provider id.
pub const CODEX: &str = "codex";

/// The provider id a kind implies where nothing names one: what a spec or
/// row an earlier build wrote, before every agent named its provider, runs.
/// Empty for an ACP agent, whose kind names no program, and for a kind this
/// build does not know.
pub fn implied_by(kind: crate::Kind) -> &'static str {
    match kind {
        crate::Kind::ClaudePty | crate::Kind::ClaudeSdk => CLAUDE,
        crate::Kind::Codex => CODEX,
        crate::Kind::Acp | crate::Kind::Unspecified => "",
    }
}

/// `provider` where it names one, else the one `kind` implies.
pub fn provider_or_implied(provider: &str, kind: crate::Kind) -> &str {
    if provider.is_empty() {
        implied_by(kind)
    } else {
        provider
    }
}

impl crate::AgentSpec {
    /// The agent's provider id, derived from its kind in a spec an earlier
    /// build wrote without one.
    pub fn provider_id(&self) -> &str {
        provider_or_implied(&self.provider, self.kind())
    }
}

impl crate::Agent {
    /// The row's provider id, derived from its kind in a row an earlier
    /// build sent without one.
    pub fn provider_id(&self) -> &str {
        provider_or_implied(&self.provider, self.kind())
    }
}
