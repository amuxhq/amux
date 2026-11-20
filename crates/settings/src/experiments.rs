//! The experiments this build implements, and which of them a person
//! switched on.
//!
//! An experiment is a feature whose shape is still moving, shipped outside
//! amux's compatibility promises: it lives in `amux.experimental`, in the
//! entries of the `experimental` maps on `amux.v1` messages, in store tables
//! named `exp_*` and under `[experiments.<name>]` in the config, and any of
//! that may change or go in any release. What switches experiments on
//! (`experiments.enabled`, the settings fields) and the maps themselves are
//! promised like everything else.
//!
//! A person switches an experiment by its name (`workspaces`). A build
//! implements at most one version of each (`workspaces/1`), and the version
//! moves whenever the experiment's wire or stored shape changes
//! incompatibly. Everything stored or sent is keyed by the version, so
//! builds running different versions, or none, never read each other's
//! shapes.
//!
//! The switch is read when the daemon starts, and the daemon restarts
//! itself to apply a change (`node::Daemon::restart_wanted`).

pub mod suspension;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One experiment this build implements.
#[derive(Debug)]
pub struct Experiment {
    /// What a person switches on: lowercase letters only, so a store table's
    /// name (`exp_<name>_<n>_...`) says whose it is unambiguously.
    pub name: &'static str,
    /// The shape this build implements, `<name>/<n>`: the key of its entries
    /// in `experimental` maps, and (as `exp_<name>_<n>_`) the prefix of its
    /// store tables.
    pub version: &'static str,
    /// One line a settings screen shows beside the switch.
    pub description: &'static str,
    /// The settings its own `[experiments.<name>]` table holds, as its
    /// module reads them: what `amux config` gets and sets under
    /// `experiments.<name>.<key>`.
    pub keys: &'static [ExperimentKey],
    /// Its settings as its module reads them from its table, each by key
    /// in words that are equal exactly when the settings are: what a daemon
    /// runs it with. A value the module cannot use reads as the default it
    /// uses instead, and a key it does not read is not there, so neither
    /// changes what a daemon runs.
    pub in_effect: fn(Option<&toml::Table>) -> Vec<(&'static str, String)>,
}

// An experiment is its version: a build implements each once.
impl PartialEq for Experiment {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version
    }
}

impl Eq for Experiment {}

impl std::hash::Hash for Experiment {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.version.hash(state);
    }
}

/// One setting of an experiment's own table. The experiment's module owns
/// it: what its value in effect is, and what a person may write.
#[derive(Debug)]
pub struct ExperimentKey {
    /// Its key in the experiment's table.
    pub name: &'static str,
    /// The value in effect, given the experiment's table: what the table
    /// says when the module can use it, else the default the daemon uses.
    pub value: fn(Option<&toml::Table>) -> toml::Value,
    /// What a person typed, as the table is to hold it, or why it cannot be.
    pub parse: fn(&str) -> Result<toml::Value, String>,
}

// A key is its name: an experiment holds each name once.
impl PartialEq for ExperimentKey {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for ExperimentKey {}

impl std::hash::Hash for ExperimentKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

impl Experiment {
    /// The prefix every store table of this version carries:
    /// `exp_workspaces_1_` for `workspaces/1`.
    pub fn table_prefix(&self) -> String {
        format!("exp_{}_", self.version.replace('/', "_"))
    }
}

/// Every agent runs in a workspace: a plain folder, a git worktree or a wt
/// tree, made, started and stopped with its agents.
pub const WORKSPACES: Experiment = Experiment {
    name: "workspaces",
    version: wire::experimental::WORKSPACE.version(),
    description: "Run each agent in a workspace that amux makes, starts and stops with its agents.",
    keys: &[],
    in_effect: |_| Vec::new(),
};

/// Idle agents without a parent stop their provider and resume on the next
/// message, and a machine restart leaves agents as they were.
pub const SUSPENSION: Experiment = Experiment {
    name: "suspension",
    version: wire::experimental::SUSPENSION.version(),
    description: "Suspend idle agents and resume them when a message arrives.",
    keys: suspension::KEYS,
    in_effect: suspension::in_effect,
};

/// An agent's changes are reviewed round by round on snapshots of its
/// working tree, with comments carried from one round to the next.
pub const REVIEWS: Experiment = Experiment {
    name: "reviews",
    version: wire::experimental::REVIEW_ROW.version(),
    description: "Review an agent's changes round by round, with comments that follow the code.",
    keys: &[],
    in_effect: |_| Vec::new(),
};

/// Every experiment this build implements, in the order settings screens
/// list them.
pub const EXPERIMENTS: &[&Experiment] = &[&WORKSPACES, &SUSPENSION, &REVIEWS];

/// The experiment this build implements under `name`.
pub fn named(name: &str) -> Option<&'static Experiment> {
    EXPERIMENTS
        .iter()
        .copied()
        .find(|experiment| experiment.name == name)
}

/// The setting `key` of the experiment named `name`, as
/// `experiments.<name>.<key>` addresses it, or a sentence saying what there
/// is instead.
pub fn key(name: &str, key: &str) -> Result<(&'static Experiment, &'static ExperimentKey), String> {
    let Some(experiment) = named(name) else {
        let names: Vec<&str> = EXPERIMENTS
            .iter()
            .map(|experiment| experiment.name)
            .collect();
        return Err(format!(
            "this amux has no experiment named {name:?}; its experiments are {}",
            names.join(", ")
        ));
    };
    if let Some(found) = experiment.keys.iter().find(|known| known.name == key) {
        return Ok((experiment, found));
    }
    if experiment.keys.is_empty() {
        return Err(format!("the {name} experiment has no settings of its own"));
    }
    let keys: Vec<String> = experiment
        .keys
        .iter()
        .map(|known| format!("experiments.{name}.{}", known.name))
        .collect();
    if key.is_empty() {
        return Err(format!(
            "the {name} experiment's settings are {}",
            keys.join(", ")
        ));
    }
    Err(format!(
        "the {name} experiment has no setting {key:?}; its settings are {}",
        keys.join(", ")
    ))
}

/// The `[experiments]` table as the file holds it: the names switched on,
/// and every other key under it (an experiment's own `[experiments.<name>]`
/// table among them), kept raw for that experiment's module to read.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExperimentSettings {
    /// The experiments switched on, by name, as written. A name this build
    /// does not implement is ignored with a warning, and kept in the file
    /// when a client changes the list.
    pub enabled: Vec<String>,
    /// Everything else under `[experiments]`, as written. An entry is an
    /// experiment's own table when its key names an experiment this build
    /// implements and it is a table; any other entry is ignored with a
    /// warning.
    #[serde(flatten)]
    pub tables: BTreeMap<String, toml::Value>,
}

impl ExperimentSettings {
    /// Whether `experiment` is switched on.
    pub fn is_on(&self, experiment: &Experiment) -> bool {
        self.enabled.iter().any(|name| name == experiment.name)
    }

    /// The experiments switched on that this build implements.
    pub fn enabled(&self) -> Enabled {
        Enabled(
            EXPERIMENTS
                .iter()
                .copied()
                .filter(|experiment| self.is_on(experiment))
                .collect(),
        )
    }

    /// The experiments switched on that this build implements, each with
    /// its settings as its module reads them: what a daemon started now
    /// would run.
    pub fn in_effect(&self) -> InEffect {
        InEffect::of(&self.enabled(), self)
    }

    /// Names in `enabled` this build implements no experiment by, each once.
    pub fn unknown_names(&self) -> Vec<&str> {
        let mut unknown: Vec<&str> = Vec::new();
        for name in &self.enabled {
            if named(name).is_none() && !unknown.contains(&name.as_str()) {
                unknown.push(name);
            }
        }
        unknown
    }

    /// The raw `[experiments.<name>]` table of `experiment`, for its own
    /// module to parse; none when the file has none.
    pub fn table(&self, experiment: &Experiment) -> Option<&toml::Table> {
        self.tables.get(experiment.name)?.as_table()
    }

    /// Keys under `[experiments]` this build reads nothing from, dotted:
    /// another build's experiment's table, or a key that is not a table.
    pub(crate) fn ignored_keys(&self) -> Vec<String> {
        self.tables
            .iter()
            .filter(|(key, value)| named(key).is_none() || !value.is_table())
            .map(|(key, _)| format!("experiments.{key}"))
            .collect()
    }
}

/// The experiments a daemon runs: those switched on when it started that
/// this build implements. It acts on and writes only their entries in its
/// own rows' and specs' `experimental` maps (each through its key in
/// `wire::experimental`), and clears its own rows of every other version's
/// when it opens a store; a peer's entries it stores and passes on as they
/// came.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Enabled(Vec<&'static Experiment>);

impl Enabled {
    /// None on: what a build that knows no experiments does.
    pub fn none() -> Enabled {
        Enabled(Vec::new())
    }

    /// Exactly these on, for tests and embedders.
    pub fn of(experiments: &[&'static Experiment]) -> Enabled {
        Enabled(experiments.to_vec())
    }

    pub fn is_on(&self, experiment: &Experiment) -> bool {
        self.0.contains(&experiment)
    }

    /// The versions on, in registry order: the keys this daemon writes on
    /// its own rows, and whose store tables it keeps.
    pub fn versions(&self) -> Vec<&'static str> {
        self.0.iter().map(|experiment| experiment.version).collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &'static Experiment> + '_ {
        self.0.iter().copied()
    }
}

/// The experiments a daemon runs, each with its settings as its module
/// reads them. Two are equal exactly when a daemon would run the same: the
/// daemon restarts when the config's differs from the one it started with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InEffect(Vec<(&'static Experiment, Vec<(&'static str, String)>)>);

impl InEffect {
    /// `enabled`, each with the settings `settings` gives it.
    pub fn of(enabled: &Enabled, settings: &ExperimentSettings) -> InEffect {
        InEffect(
            enabled
                .iter()
                .map(|experiment| {
                    (
                        experiment,
                        (experiment.in_effect)(settings.table(experiment)),
                    )
                })
                .collect(),
        )
    }

    /// The experiments on.
    pub fn enabled(&self) -> Enabled {
        Enabled(self.0.iter().map(|(experiment, _)| *experiment).collect())
    }

    /// The settings `experiment` runs with; none when it is off.
    pub fn of_experiment(&self, experiment: &Experiment) -> Option<&[(&'static str, String)]> {
        self.0
            .iter()
            .find(|(on, _)| *on == experiment)
            .map(|(_, settings)| settings.as_slice())
    }

    /// Each setting of each experiment on, as `suspension.after = 30m`.
    pub fn describe(&self) -> Vec<String> {
        self.0
            .iter()
            .flat_map(|(experiment, settings)| {
                settings
                    .iter()
                    .map(|(key, value)| format!("{}.{key} = {value}", experiment.name))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InstallationConfig;

    /// What a daemon runs is what each module reads: the same delay written
    /// another way, a value the module cannot use and a key it does not
    /// read change nothing, and an experiment that is off runs with nothing.
    #[test]
    fn what_runs_is_what_each_module_reads() {
        let in_effect = |text: &str| {
            InstallationConfig::from_toml(text)
                .unwrap()
                .experiments
                .in_effect()
        };
        let on = "[experiments]\nenabled = [\"suspension\"]\n";
        let default = in_effect(on);
        assert_eq!(default.describe(), ["suspension.after = 30m"]);
        assert_eq!(
            in_effect(&format!("{on}[experiments.suspension]\nafter = \"30m\"\n")),
            default
        );
        assert_eq!(
            in_effect(&format!(
                "{on}[experiments.suspension]\nafter = \"1800s\"\n"
            )),
            default,
            "the same delay"
        );
        assert_eq!(
            in_effect(&format!("{on}[experiments.suspension]\nafter = \"soon\"\n")),
            default,
            "unusable: the default"
        );
        assert_eq!(
            in_effect(&format!("{on}[experiments.suspension]\nbefore = \"1h\"\n")),
            default,
            "a key it does not read"
        );
        let longer = in_effect(&format!("{on}[experiments.suspension]\nafter = \"45m\"\n"));
        assert_ne!(longer, default);
        assert_eq!(longer.describe(), ["suspension.after = 45m"]);
        assert_eq!(
            in_effect(&format!(
                "{on}[experiments.suspension]\nafter = \"1500ms\"\n"
            ))
            .describe(),
            ["suspension.after = 1500ms"]
        );
        let off = in_effect("[experiments.suspension]\nafter = \"45m\"\n");
        assert_eq!(off, in_effect(""), "off runs with nothing");
        assert_eq!(off.of_experiment(&SUSPENSION), None);
    }

    #[test]
    fn every_experiment_is_named_and_versioned_so_its_traces_are_found_by_name() {
        let mut names = std::collections::BTreeSet::new();
        for experiment in EXPERIMENTS {
            assert!(
                !experiment.name.is_empty()
                    && experiment
                        .name
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase()),
                "{}: a name is lowercase letters",
                experiment.name
            );
            let number = experiment
                .version
                .strip_prefix(experiment.name)
                .and_then(|rest| rest.strip_prefix('/'))
                .unwrap_or_else(|| panic!("{} is not {}/<n>", experiment.version, experiment.name));
            assert!(
                number
                    .parse::<u32>()
                    .is_ok_and(|n| n > 0 && n.to_string() == number),
                "{}: the version is a positive number",
                experiment.version
            );
            assert!(names.insert(experiment.name), "{} twice", experiment.name);
            assert!(!experiment.description.is_empty());
            assert!(!experiment.description.contains('\n'));
        }
        assert_eq!(WORKSPACES.table_prefix(), "exp_workspaces_1_");
        assert_eq!(REVIEWS.table_prefix(), "exp_reviews_1_");
        // The reviews experiment keys an entry on two maps by its one version.
        assert_eq!(REVIEWS.version, wire::experimental::SENT_REVIEW.version());
        for experiment in EXPERIMENTS {
            for (index, known) in experiment.keys.iter().enumerate() {
                assert!(
                    !experiment.keys[..index].contains(known),
                    "{}.{} twice",
                    experiment.name,
                    known.name
                );
                assert_eq!(key(experiment.name, known.name), Ok((*experiment, known)));
            }
        }
    }

    #[test]
    fn the_switch_reads_names_and_hands_each_experiment_its_own_table() {
        let config = InstallationConfig::from_toml(
            "[experiments]\nenabled = [\"suspension\", \"teleport\", \"teleport\"]\n\
             [experiments.suspension]\nafter = \"30m\"\n\
             [experiments.teleport]\nrange = 3\n",
        )
        .unwrap();
        let experiments = &config.experiments;
        assert!(experiments.is_on(&SUSPENSION));
        assert!(!experiments.is_on(&WORKSPACES));
        assert_eq!(experiments.enabled().versions(), ["suspension/1"]);
        assert_eq!(experiments.unknown_names(), ["teleport"]);
        assert_eq!(
            experiments.table(&SUSPENSION).unwrap()["after"].as_str(),
            Some("30m")
        );
        assert_eq!(experiments.table(&WORKSPACES), None);
        // Another build's experiment's table is a key this build ignores.
        assert_eq!(config.ignored, ["experiments.teleport"]);

        let none = InstallationConfig::from_toml("").unwrap();
        assert_eq!(none.experiments.enabled(), Enabled::none());
        assert!(none.ignored.is_empty());
        // What it writes back reads the same.
        let again = InstallationConfig::from_toml(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(again.experiments, config.experiments);
    }
}
