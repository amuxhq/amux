//! amux's one scale of how much an agent may do without asking, and where
//! each provider's own permissions sit on it. An agent that starts a
//! subagent picks a level rather than a provider's value, since the two may
//! be different programs; the subagent's host maps the level onto what its
//! provider offers. The interpreters write each value's level into the
//! catalogue, and the daemon, which links no interpreter, maps with the same
//! tables.

use crate::{Catalogue, PermissionLevel};

/// Where a Claude permission mode sits. `acceptEdits` is auto because it
/// matches Codex's auto: edits in the workspace go ahead, while commands
/// still go through Claude's checks.
pub fn claude(value: &str) -> PermissionLevel {
    match value {
        "plan" => PermissionLevel::ReadOnly,
        "default" => PermissionLevel::Ask,
        "acceptEdits" | "auto" => PermissionLevel::Auto,
        "bypassPermissions" => PermissionLevel::Full,
        _ => PermissionLevel::Unspecified,
    }
}

/// Where a Codex permission sits.
pub fn codex(value: &str) -> PermissionLevel {
    match value {
        "read-only" => PermissionLevel::ReadOnly,
        "default" => PermissionLevel::Ask,
        "auto" => PermissionLevel::Auto,
        "full-access" => PermissionLevel::Full,
        _ => PermissionLevel::Unspecified,
    }
}

/// The modes of the built-in ACP agents whose modes are permissions, with
/// their levels. Their interpreter lists these modes among the agent's
/// permissions, not its modes. Grok's modes choose how hard it thinks, not what it may
/// do, so it has none. OpenCode's build mode runs tools as the person's
/// OpenCode config allows, which by default is everything, so it counts as
/// full.
const ACP_MODES: &[(&str, &[(&str, PermissionLevel)])] = &[
    (
        "antigravity",
        &[
            ("plan", PermissionLevel::ReadOnly),
            ("default", PermissionLevel::Ask),
            ("accept-edits", PermissionLevel::Auto),
            ("auto", PermissionLevel::Auto),
        ],
    ),
    (
        "opencode",
        &[
            ("plan", PermissionLevel::ReadOnly),
            ("build", PermissionLevel::Full),
        ],
    ),
];

/// The known modes of the ACP agent `agent`, with their levels: empty for
/// an agent a person added and for a built-in whose modes are not
/// permissions.
pub fn acp_modes(agent: &str) -> &'static [(&'static str, PermissionLevel)] {
    ACP_MODES
        .iter()
        .find(|(known, _)| *known == agent)
        .map_or(&[], |(_, modes)| modes)
}

/// Where a built-in ACP agent's mode sits; unspecified for a mode amux
/// does not know, and for every mode of an agent a person added.
pub fn acp_mode(agent: &str, mode: &str) -> PermissionLevel {
    acp_modes(agent)
        .iter()
        .find(|(known, _)| *known == mode)
        .map_or(PermissionLevel::Unspecified, |(_, level)| *level)
}

/// Where `value`, a permission of the provider `provider` (for an ACP
/// agent, one of its modes that is a permission), sits.
pub fn of(provider: &str, value: &str) -> PermissionLevel {
    match provider {
        crate::agents::CLAUDE => claude(value),
        crate::agents::CODEX => codex(value),
        agent => acp_mode(agent, value),
    }
}

impl PermissionLevel {
    /// The level as the agent tools say it.
    pub fn word(self) -> Option<&'static str> {
        match self {
            Self::ReadOnly => Some("read-only"),
            Self::Ask => Some("ask"),
            Self::Auto => Some("auto"),
            Self::Full => Some("full"),
            Self::Unspecified => None,
        }
    }

    /// The level a word names, as [`Self::word`] says it.
    pub fn from_word(word: &str) -> Option<Self> {
        [Self::ReadOnly, Self::Ask, Self::Auto, Self::Full]
            .into_iter()
            .find(|level| level.word() == Some(word))
    }

    /// Every level on the scale, narrowest first.
    pub const ALL: [Self; 4] = [Self::ReadOnly, Self::Ask, Self::Auto, Self::Full];
}

/// One permission a catalogue offers on the scale: the provider's value and
/// its level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leveled {
    pub value: String,
    pub level: PermissionLevel,
    /// The provider's ordinary one, which wins a tie at its level.
    pub normal: bool,
}

/// The permissions `provider` offers on the scale: its catalogue's
/// settable permissions at the levels the catalogue states, a value with
/// none left out. With no catalogue to read, what amux knows the provider
/// always offers: Claude's permissions that need no particular model or
/// setting, Codex's, and a built-in ACP agent's known permission modes.
pub fn offered(provider: &str, catalogue: Option<&Catalogue>) -> Vec<Leveled> {
    if let Some(catalogue) = catalogue.filter(|catalogue| !catalogue.permissions.is_empty()) {
        return catalogue
            .permissions
            .iter()
            .filter(|offered| offered.settable && offered.level() != PermissionLevel::Unspecified)
            .map(|offered| Leveled {
                value: offered.value.clone(),
                level: offered.level(),
                normal: offered.normal,
            })
            .collect();
    }
    let known: Vec<(&str, PermissionLevel)> = match provider {
        crate::agents::CLAUDE => ["plan", "default", "acceptEdits"]
            .into_iter()
            .map(|value| (value, claude(value)))
            .collect(),
        crate::agents::CODEX => crate::codex::PERMISSIONS
            .iter()
            .map(|named| (named.value, codex(named.value)))
            .collect(),
        agent => acp_modes(agent).to_vec(),
    };
    known
        .into_iter()
        .map(|(value, level)| Leveled {
            value: value.to_owned(),
            level,
            normal: value == "default",
        })
        .collect()
}

/// The widest of `offered` at or below `level`: the provider's ordinary one
/// where several share that level, else the first. None when nothing
/// offered is that narrow.
pub fn at(offered: &[Leveled], level: PermissionLevel) -> Option<Leveled> {
    let widest = offered
        .iter()
        .map(|leveled| leveled.level)
        .filter(|offered| *offered as i32 <= level as i32)
        .max_by_key(|offered| *offered as i32)?;
    let at: Vec<&Leveled> = offered
        .iter()
        .filter(|leveled| leveled.level == widest)
        .collect();
    at.iter()
        .find(|leveled| leveled.normal)
        .or(at.first())
        .map(|leveled| (*leveled).clone())
}

/// The levels among `offered`, narrowest first, each once.
pub fn levels(offered: &[Leveled]) -> Vec<PermissionLevel> {
    PermissionLevel::ALL
        .into_iter()
        .filter(|level| offered.iter().any(|leveled| leveled.level == *level))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OfferedPermission;

    fn permission(value: &str, level: PermissionLevel, normal: bool) -> OfferedPermission {
        OfferedPermission {
            value: value.to_owned(),
            settable: true,
            normal,
            level: level as i32,
            ..OfferedPermission::default()
        }
    }

    /// A level maps onto the widest permission at or below it, the normal
    /// one winning a tie, and onto nothing when every offer is wider.
    #[test]
    fn a_level_maps_onto_the_widest_permission_at_or_below_it() {
        let catalogue = Catalogue {
            permissions: vec![
                permission("default", PermissionLevel::Ask, true),
                permission("acceptEdits", PermissionLevel::Auto, false),
                permission("auto", PermissionLevel::Auto, false),
                permission("bypassPermissions", PermissionLevel::Full, false),
            ],
            ..Catalogue::default()
        };
        let offered = offered("claude", Some(&catalogue));
        let at = |level| at(&offered, level).map(|leveled| leveled.value);
        assert_eq!(
            at(PermissionLevel::Full).as_deref(),
            Some("bypassPermissions")
        );
        assert_eq!(at(PermissionLevel::Auto).as_deref(), Some("acceptEdits"));
        assert_eq!(at(PermissionLevel::Ask).as_deref(), Some("default"));
        assert_eq!(at(PermissionLevel::ReadOnly), None);
        assert_eq!(
            levels(&offered),
            [
                PermissionLevel::Ask,
                PermissionLevel::Auto,
                PermissionLevel::Full
            ]
        );
    }

    /// A permission with no level is never picked, and a built-in ACP
    /// agent whose catalogue has not been read offers its known
    /// permission modes; an agent a person added offers none.
    #[test]
    fn an_unleveled_permission_is_never_picked() {
        let catalogue = Catalogue {
            permissions: vec![
                permission("default", PermissionLevel::Ask, true),
                permission("plan", PermissionLevel::ReadOnly, false),
                permission("custom", PermissionLevel::Unspecified, false),
            ],
            ..Catalogue::default()
        };
        let listed = offered("my-agent", Some(&catalogue));
        assert_eq!(
            at(&listed, PermissionLevel::Full)
                .map(|leveled| leveled.value)
                .as_deref(),
            Some("default")
        );
        assert_eq!(
            levels(&listed),
            [PermissionLevel::ReadOnly, PermissionLevel::Ask]
        );
        let known = offered("antigravity", None);
        assert_eq!(
            at(&known, PermissionLevel::ReadOnly)
                .map(|leveled| leveled.value)
                .as_deref(),
            Some("plan")
        );
        assert!(offered("my-agent", None).is_empty());
    }

    #[test]
    fn levels_round_trip_through_their_words() {
        for level in PermissionLevel::ALL {
            assert_eq!(
                PermissionLevel::from_word(level.word().unwrap()),
                Some(level)
            );
        }
        assert_eq!(PermissionLevel::from_word("bypass"), None);
        assert_eq!(of("claude", "plan"), PermissionLevel::ReadOnly);
        assert_eq!(of("codex", "full-access"), PermissionLevel::Full);
        assert_eq!(of("antigravity", "plan"), PermissionLevel::ReadOnly);
        assert_eq!(of("my-agent", "plan"), PermissionLevel::Unspecified);
    }
}
