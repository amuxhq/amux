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
    /// What a person does to sign in, for when the agent's own sign-in
    /// methods do not say: each checked against the program's own help.
    pub sign_in_hint: &'static str,
}

pub const BUILT_INS: [BuiltIn; 3] = [
    BuiltIn {
        id: "antigravity",
        name: "Antigravity",
        command: "agy",
        args: &["acp"],
        // `agy --help` lists no sign-in command: the program asks for one
        // when it starts signed out.
        sign_in_hint: "Run agy and sign in when it asks.",
    },
    BuiltIn {
        id: "opencode",
        name: "OpenCode",
        command: "opencode",
        args: &["acp"],
        sign_in_hint: "Run `opencode auth login`.",
    },
    BuiltIn {
        id: "grok",
        name: "Grok",
        command: "grok",
        args: &["agent", "stdio"],
        sign_in_hint: "Run `grok login`.",
    },
];

/// The built-in agent with this id, if there is one.
pub fn built_in(id: &str) -> Option<&'static BuiltIn> {
    BUILT_INS.iter().find(|agent| agent.id == id)
}

/// The most characters an agent's id has.
pub const ID_MOST: usize = 64;

/// Why `id` is not shaped as an agent's id can be, if it is not: what a
/// config entry's key and a spawn call's agent are both held to.
pub fn shape_problem(id: &str) -> Option<String> {
    let characters = id.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
    });
    let starts = id
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    (!characters || !starts || id.len() > ID_MOST).then(|| {
        format!(
            "an agent's id is at most {ID_MOST} lowercase letters, digits, hyphens and \
             underscores, starting with a letter or digit"
        )
    })
}

/// Claude Code's provider id, for both of its kinds.
pub const CLAUDE: &str = "claude";
/// Codex's provider id.
pub const CODEX: &str = "codex";

/// What people call amux's own agents, Claude and Codex, by id.
const OWN_NAMES: [(&str, &str); 2] = [(CLAUDE, "Claude"), (CODEX, "Codex")];

/// The name people see for an agent amux knows by its id (Claude, Codex or
/// a built-in ACP agent), for when no host's entry names it: a client
/// relaunched with the agent's host gone, or one whose host has not listed
/// its agents yet. None for an id amux does not know, such as one a
/// person's config added.
pub fn display_name(id: &str) -> Option<&'static str> {
    OWN_NAMES
        .into_iter()
        .chain(BUILT_INS.iter().map(|agent| (agent.id, agent.name)))
        .find(|(known, _)| *known == id)
        .map(|(_, name)| name)
}

/// What people call `id` where no host's entry names it: its known name,
/// else the id itself.
pub fn name_or_id(id: &str) -> &str {
    display_name(id).unwrap_or(id)
}

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

#[cfg(test)]
mod tests {
    use prost::Message as _;

    /// amux's own agents and every built-in have a name people see; an id a
    /// person's config added is its own name.
    #[test]
    fn known_ids_have_display_names() {
        assert_eq!(super::name_or_id(super::CLAUDE), "Claude");
        assert_eq!(super::name_or_id(super::CODEX), "Codex");
        for agent in super::BUILT_INS {
            assert_eq!(super::display_name(agent.id), Some(agent.name));
        }
        assert_eq!(super::display_name("my-own-agent"), None);
        assert_eq!(super::name_or_id("my-own-agent"), "my-own-agent");
    }

    /// A released build named Claude and Codex by an enum, at the tags the
    /// provider ids do not reuse: its host entry and its catalogue request
    /// still decode, as a provider with no id.
    #[test]
    fn a_released_builds_provider_enum_reads_as_no_id() {
        // ProviderOnHost { provider: PROVIDER_CLAUDE, catalogue: [7],
        // signed_in: true } as 0.10.0 wrote it.
        let entry =
            crate::ProviderOnHost::decode([0x08, 0x01, 0x12, 0x01, 0x07, 0x18, 0x01].as_slice())
                .expect("the entry decodes");
        assert_eq!(entry.provider_id, "");
        assert_eq!(entry.catalogue, Some(vec![7]));
        assert!(entry.signed_in);
        // GetCatalogueRequest { host_id: [1], provider: PROVIDER_CODEX }.
        let request = crate::GetCatalogueRequest::decode([0x0a, 0x01, 0x01, 0x10, 0x02].as_slice())
            .expect("the request decodes");
        assert_eq!(request.host_id, [1]);
        assert_eq!(request.provider_id, "");
    }
}
