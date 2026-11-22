//! `[agents.<id>]`: the agent programs a person adds to the ones amux knows,
//! or a built-in one they run differently.
//!
//! ```toml
//! [agents.my-agent]
//! name = "My Agent"
//! command = "my-agent"
//! args = ["--acp"]
//! env = { MY_AGENT_LOG = "1" }
//! ```
//!
//! Each entry is an agent that speaks the Agent Client Protocol on its
//! standard input and output. Under a built-in agent's id, an entry changes
//! only the fields it sets. The daemon merges the entries with the built-in
//! agents (`node::agents`), and that is where an entry with no command and no
//! built-in under its id is skipped.
//!
//! The table is read entry by entry rather than through the config's own
//! schema: a mistake in one agent's entry skips that entry with a warning and
//! never stops amux starting, as a config a release accepted must not.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The ids amux keeps for the agents it runs through their own
/// integrations; an entry cannot take one.
pub const RESERVED: [&str; 2] = [wire::agents::CLAUDE, wire::agents::CODEX];

/// The fields an entry reads.
const FIELDS: [&str; 4] = ["name", "command", "args", "env"];

/// One entry, as the file gives it: each field absent where the entry does
/// not set it, so an entry under a built-in's id changes only what it says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentEntry {
    /// The agent's id: the entry's key, the provider string on the wire.
    pub id: String,
    /// What people call it.
    pub name: Option<String>,
    /// The program, looked up on the daemon's PATH unless it is a path.
    pub command: Option<String>,
    /// What the program is started with.
    pub args: Option<Vec<String>>,
    /// Added to the environment the program starts with.
    pub env: BTreeMap<String, String>,
}

/// The `[agents]` table as the file holds it, kept whole so a broken entry
/// is skipped by itself, and written back as it was read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentTables(Option<toml::Value>);

/// What the entries say: the ones that read, and a sentence for each one
/// skipped and why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Agents {
    pub entries: Vec<AgentEntry>,
    pub warnings: Vec<String>,
}

impl AgentTables {
    /// No `[agents]` table at all.
    pub fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    /// The entries that read, in id order, and why each other one is
    /// skipped. Keys inside an entry this build does not read are not among
    /// the warnings; they are the config's ignored keys.
    pub fn read(&self) -> Agents {
        let mut agents = Agents::default();
        let Some(value) = &self.0 else {
            return agents;
        };
        let Some(table) = value.as_table() else {
            agents
                .warnings
                .push("agents is not a table of agents, so it is ignored".to_owned());
            return agents;
        };
        for (id, value) in table {
            match entry(id, value) {
                Ok(entry) => agents.entries.push(entry),
                Err(why) => agents
                    .warnings
                    .push(format!("agents.{id} is skipped: {why}")),
            }
        }
        agents
    }

    /// Keys inside an entry this build reads nothing from, dotted.
    pub(crate) fn ignored_keys(&self) -> Vec<String> {
        let Some(table) = self.0.as_ref().and_then(toml::Value::as_table) else {
            return Vec::new();
        };
        table
            .iter()
            .filter_map(|(id, value)| Some((id, value.as_table()?)))
            .flat_map(|(id, entry)| {
                entry
                    .keys()
                    .filter(|key| !FIELDS.contains(&key.as_str()))
                    .map(move |key| format!("agents.{id}.{key}"))
            })
            .collect()
    }
}

/// Why `id` cannot name an agent, if it cannot.
pub fn id_problem(id: &str) -> Option<String> {
    if RESERVED.contains(&id) {
        return Some(format!(
            "{id} is amux's own name for {}, which no entry replaces",
            if id == "claude" {
                "Claude Code"
            } else {
                "Codex"
            }
        ));
    }
    wire::agents::shape_problem(id)
}

fn entry(id: &str, value: &toml::Value) -> Result<AgentEntry, String> {
    if let Some(problem) = id_problem(id) {
        return Err(problem);
    }
    let table = value
        .as_table()
        .ok_or("it is not a table; write it as [agents.<id>] with a command")?;
    let string = |key: &str| -> Result<Option<String>, String> {
        match table.get(key) {
            None => Ok(None),
            Some(toml::Value::String(text)) => Ok(Some(text.clone())),
            Some(_) => Err(format!("{key} is not a string")),
        }
    };
    let name = string("name")?.filter(|name| !name.trim().is_empty());
    let command = string("command")?;
    if command
        .as_deref()
        .is_some_and(|command| command.trim().is_empty())
    {
        return Err("command is empty".to_owned());
    }
    let args = match table.get("args") {
        None => None,
        Some(toml::Value::Array(items)) => Some(
            items
                .iter()
                .map(|item| item.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .ok_or("args is not a list of strings")?,
        ),
        Some(_) => return Err("args is not a list of strings".to_owned()),
    };
    let env = match table.get("env") {
        None => BTreeMap::new(),
        Some(toml::Value::Table(env)) => env
            .iter()
            .map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
            .collect::<Option<BTreeMap<_, _>>>()
            .ok_or("env is not a table of strings")?,
        Some(_) => return Err("env is not a table of strings".to_owned()),
    };
    Ok(AgentEntry {
        id: id.to_owned(),
        name,
        command,
        args,
        env,
    })
}

impl Serialize for AgentTables {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.0 {
            Some(value) => value.serialize(serializer),
            None => toml::Table::new().serialize(serializer),
        }
    }
}

// Any value reads, so the config around it always does; `read` says what is
// wrong with it.
impl<'de> Deserialize<'de> for AgentTables {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        toml::Value::deserialize(deserializer).map(|value| Self(Some(value)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InstallationConfig;

    fn read(text: &str) -> (Agents, InstallationConfig) {
        let config = InstallationConfig::from_toml(text).expect("the config reads");
        (config.agents.read(), config)
    }

    #[test]
    fn an_entry_reads_every_field() {
        let (agents, config) = read(
            "[agents.gemini]\nname = \"Gemini CLI\"\ncommand = \"gemini\"\n\
             args = [\"--experimental-acp\"]\nenv = { GEMINI_API_KEY = \"k\" }\n",
        );
        assert_eq!(agents.warnings, Vec::<String>::new());
        assert_eq!(
            agents.entries,
            [AgentEntry {
                id: "gemini".into(),
                name: Some("Gemini CLI".into()),
                command: Some("gemini".into()),
                args: Some(vec!["--experimental-acp".into()]),
                env: [("GEMINI_API_KEY".into(), "k".into())].into(),
            }]
        );
        assert!(config.ignored.is_empty(), "{:?}", config.ignored);
    }

    /// An entry under a built-in's id may set only what it changes: what it
    /// leaves out stays absent for the daemon to take from the built-in.
    #[test]
    fn an_override_sets_only_what_it_changes() {
        let (agents, _) = read("[agents.opencode]\ncommand = \"/opt/opencode/bin/opencode\"\n");
        assert_eq!(
            agents.entries,
            [AgentEntry {
                id: "opencode".into(),
                command: Some("/opt/opencode/bin/opencode".into()),
                ..AgentEntry::default()
            }]
        );
    }

    /// A broken entry is skipped with a sentence saying why, and the config
    /// and every other entry still read.
    #[test]
    fn a_broken_entry_is_skipped_with_a_warning() {
        let (agents, config) = read(
            "host_name = \"desk\"\n\
             [agents.good]\ncommand = \"good-agent\"\n\
             [agents.numbered]\ncommand = 3\n\
             [agents.listed]\ncommand = \"x\"\nargs = \"--acp\"\n\
             [agents.mixed]\ncommand = \"x\"\nargs = [\"a\", 1]\n\
             [agents.envy]\ncommand = \"x\"\nenv = { KEY = 1 }\n\
             [agents.blank]\ncommand = \"  \"\n\
             [agents.Shouty]\ncommand = \"x\"\n",
        );
        assert_eq!(config.host_name(), "desk");
        assert_eq!(
            agents
                .entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["good"]
        );
        assert_eq!(
            agents.warnings,
            [
                "agents.Shouty is skipped: an agent's id is at most 64 lowercase letters, digits, hyphens and underscores, starting with a letter or digit",
                "agents.blank is skipped: command is empty",
                "agents.envy is skipped: env is not a table of strings",
                "agents.listed is skipped: args is not a list of strings",
                "agents.mixed is skipped: args is not a list of strings",
                "agents.numbered is skipped: command is not a string",
            ]
        );
    }

    #[test]
    fn an_entry_that_is_no_table_and_an_agents_key_that_is_no_table_are_skipped() {
        let (agents, _) = read("[agents]\ngemini = \"gemini\"\n");
        assert!(agents.entries.is_empty());
        assert_eq!(
            agents.warnings,
            [
                "agents.gemini is skipped: it is not a table; write it as [agents.<id>] with a command"
            ]
        );
        let (agents, _) = read("agents = 3\n");
        assert!(agents.entries.is_empty());
        assert_eq!(
            agents.warnings,
            ["agents is not a table of agents, so it is ignored"]
        );
    }

    #[test]
    fn claude_and_codex_are_never_entries() {
        let (agents, _) = read(
            "[agents.claude]\ncommand = \"my-claude\"\n[agents.codex]\ncommand = \"my-codex\"\n",
        );
        assert!(agents.entries.is_empty());
        assert_eq!(
            agents.warnings,
            [
                "agents.claude is skipped: claude is amux's own name for Claude Code, which no entry replaces",
                "agents.codex is skipped: codex is amux's own name for Codex, which no entry replaces",
            ]
        );
    }

    /// A key an entry does not read is one of the config's ignored keys,
    /// warned about with the rest; the entry itself still reads.
    #[test]
    fn an_unknown_key_in_an_entry_is_ignored_and_named() {
        let (agents, config) =
            read("[agents.gemini]\ncommand = \"gemini\"\nicon = \"gemini.png\"\n");
        assert_eq!(agents.entries.len(), 1);
        assert_eq!(config.ignored, ["agents.gemini.icon"]);
    }

    /// The table is written back as it was read, so a client that saves the
    /// config keeps a person's entries, the broken ones among them.
    #[test]
    fn the_table_round_trips() {
        let text = "[agents.gemini]\ncommand = \"gemini\"\n\n[agents.numbered]\ncommand = 3\n";
        let (_, config) = read(text);
        let again = InstallationConfig::from_toml(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(again.agents, config.agents);
        assert!(
            InstallationConfig::from_toml("").unwrap().agents.is_empty(),
            "no table, nothing to write"
        );
    }
}
