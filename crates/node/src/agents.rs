//! The agent programs this host can run as ACP agents: the built-in ones
//! (`wire::agents::BUILT_INS`) merged with a person's `[agents.<id>]`
//! entries, and which of them the host offers, which is the ones whose
//! command is found.
//!
//! amux runs only these commands: a built-in's own, or one a person wrote in
//! their config. It never downloads or installs an agent, so a built-in whose
//! program is not installed is simply not offered.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use settings::agents::AgentEntry;
use wire::agents::BUILT_INS;

/// An agent program the host knows how to start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProgram {
    /// Its id: the provider string on the wire and in its spec.
    pub id: String,
    /// What people call it.
    pub name: String,
    /// The program as configured: a name looked up on the PATH, or a path.
    pub command: String,
    pub args: Vec<String>,
    /// Added to the environment it starts with, over the host's own
    /// additions for every provider.
    pub env: BTreeMap<String, String>,
    /// One amux ships, rather than one a person added. A person's entry
    /// under its id changes it and leaves it built in.
    pub built_in: bool,
}

/// The built-in agents with each entry laid over its own: an entry under a
/// built-in's id changes only the fields it sets, and any other entry adds
/// an agent, which then needs a command. Built-ins come first, in their own
/// order, then the added ones by id. The warnings are sentences, one per
/// entry skipped.
pub fn programs(entries: &[AgentEntry]) -> (Vec<AgentProgram>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut programs: Vec<AgentProgram> = BUILT_INS
        .iter()
        .map(|built_in| {
            let mut program = AgentProgram {
                id: built_in.id.to_owned(),
                name: built_in.name.to_owned(),
                command: built_in.command.to_owned(),
                args: built_in.args.iter().map(|arg| (*arg).to_owned()).collect(),
                env: BTreeMap::new(),
                built_in: true,
            };
            if let Some(entry) = entries.iter().find(|entry| entry.id == built_in.id) {
                if let Some(name) = &entry.name {
                    program.name.clone_from(name);
                }
                if let Some(command) = &entry.command {
                    program.command.clone_from(command);
                }
                if let Some(args) = &entry.args {
                    program.args.clone_from(args);
                }
                program.env.extend(entry.env.clone());
            }
            program
        })
        .collect();
    for entry in entries {
        if wire::agents::built_in(&entry.id).is_some() {
            continue;
        }
        let Some(command) = entry.command.clone() else {
            warnings.push(format!(
                "agents.{} is skipped: it names no command, and no built-in agent has that id",
                entry.id
            ));
            continue;
        };
        programs.push(AgentProgram {
            id: entry.id.clone(),
            // A person who gives no name sees the id they chose.
            name: entry.name.clone().unwrap_or_else(|| entry.id.clone()),
            command,
            args: entry.args.clone().unwrap_or_default(),
            env: entry.env.clone(),
            built_in: false,
        });
    }
    (programs, warnings)
}

/// The agent programs the config describes, and the sentences saying what
/// in it was skipped: the entries that do not read, then those that read
/// but do not make an agent.
pub fn configured(config: &settings::InstallationConfig) -> (Vec<AgentProgram>, Vec<String>) {
    let read = config.agents.read();
    let (programs, more) = programs(&read.entries);
    let mut warnings = read.warnings;
    warnings.extend(more);
    (programs, warnings)
}

/// An agent program the host offers: its command found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offered {
    pub program: AgentProgram,
    /// The file the command resolves to, which is what is started.
    pub path: PathBuf,
}

impl AgentProgram {
    /// Where this program's command is, looked up as it will be started
    /// ([`os::paths::Lookup`] over [`Self::environment`]): on the `PATH`
    /// its own environment gives, else the host's for every provider, else
    /// the one a newly logged-on process would see; on Windows under the
    /// `PATHEXT` its own environment gives, else the host's, else this
    /// process's.
    pub fn find(&self, provider_env: &HashMap<String, String>) -> Option<PathBuf> {
        let lookup = os::paths::Lookup::new(&self.environment(provider_env));
        find_command(&self.command, &lookup, os::paths::home_dir().as_deref())
    }

    /// The environment it starts with: the host's additions for every
    /// provider, with its own over them, whatever the case of either where
    /// the OS names a variable whatever its case ([`os::paths::set_vars`]):
    /// its own `PATH` replaces the host's `Path` on Windows, so it is found
    /// on the `PATH` it starts with.
    pub fn environment(&self, provider_env: &HashMap<String, String>) -> HashMap<String, String> {
        let mut env = provider_env.clone();
        os::paths::set_vars(&mut env, self.env.clone());
        env
    }
}

/// The programs among `programs` whose command is found, in their order.
pub fn offered(programs: &[AgentProgram], provider_env: &HashMap<String, String>) -> Vec<Offered> {
    programs
        .iter()
        .filter_map(|program| {
            Some(Offered {
                path: program.find(provider_env)?,
                program: program.clone(),
            })
        })
        .collect()
}

/// The file `command` names, as an absolute path: a command holding a path
/// separator is that path, a leading `~/` the home; any other is looked up
/// by `lookup` as the operating system starts a program
/// ([`os::paths::find_program`]: the execute bit on Unix, `PATHEXT` on
/// Windows, so npm's `.cmd` shims are found and the extensionless script
/// beside one never is). A relative command or `PATH` entry is checked
/// against the daemon's working directory, so the path returned is
/// absolute: the program starts in the agent's folder, where the same
/// relative path would name another file or none.
fn find_command(command: &str, lookup: &os::paths::Lookup, home: Option<&Path>) -> Option<PathBuf> {
    let homed = match (command.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(command),
    };
    lookup.find(homed.as_os_str())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    /// A lookup on `path` alone.
    fn on(path: &OsStr) -> os::paths::Lookup {
        os::paths::Lookup::new(&HashMap::from([(
            "PATH".to_owned(),
            path.to_string_lossy().into_owned(),
        )]))
    }

    fn entry(id: &str) -> AgentEntry {
        AgentEntry {
            id: id.to_owned(),
            ..AgentEntry::default()
        }
    }

    #[test]
    fn with_no_entries_the_programs_are_the_built_ins() {
        let (merged, warnings) = programs(&[]);
        assert!(warnings.is_empty());
        assert_eq!(
            merged
                .iter()
                .map(|program| (
                    program.id.as_str(),
                    program.command.as_str(),
                    program.args.join(" ")
                ))
                .collect::<Vec<_>>(),
            [
                ("antigravity", "agy", "acp".to_owned()),
                ("opencode", "opencode", "acp".to_owned()),
                ("grok", "grok", "agent stdio".to_owned()),
            ]
        );
        assert!(merged.iter().all(|program| program.built_in));
    }

    /// An entry under a built-in's id changes the fields it sets and keeps
    /// the rest; the program stays the built-in one.
    #[test]
    fn an_entry_overrides_a_built_in_field_by_field() {
        let (merged, _) = programs(&[AgentEntry {
            command: Some("/opt/opencode/bin/opencode".into()),
            env: [("OPENCODE_CONFIG".into(), "/etc/opencode.json".into())].into(),
            ..entry("opencode")
        }]);
        let opencode = merged
            .iter()
            .find(|program| program.id == "opencode")
            .unwrap();
        assert_eq!(
            opencode,
            &AgentProgram {
                id: "opencode".into(),
                name: "OpenCode".into(),
                command: "/opt/opencode/bin/opencode".into(),
                args: vec!["acp".into()],
                env: [("OPENCODE_CONFIG".into(), "/etc/opencode.json".into())].into(),
                built_in: true,
            }
        );
        let (renamed, _) = programs(&[AgentEntry {
            name: Some("Grok (work)".into()),
            args: Some(vec![
                "agent".into(),
                "stdio".into(),
                "--profile".into(),
                "work".into(),
            ]),
            ..entry("grok")
        }]);
        let grok = renamed.iter().find(|program| program.id == "grok").unwrap();
        assert_eq!(grok.name, "Grok (work)");
        assert_eq!(grok.command, "grok");
        assert_eq!(grok.args, ["agent", "stdio", "--profile", "work"]);
    }

    #[test]
    fn an_added_agent_needs_a_command_and_goes_by_its_id_unnamed() {
        let (merged, warnings) = programs(&[
            AgentEntry {
                command: Some("gemini".into()),
                args: Some(vec!["--experimental-acp".into()]),
                ..entry("gemini")
            },
            AgentEntry {
                name: Some("Nameless".into()),
                ..entry("nameless")
            },
        ]);
        let added: Vec<_> = merged.iter().filter(|program| !program.built_in).collect();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].name, "gemini");
        assert_eq!(added[0].args, ["--experimental-acp"]);
        assert_eq!(
            warnings,
            ["agents.nameless is skipped: it names no command, and no built-in agent has that id"]
        );
    }

    /// The config's own warnings come first, then the merge's.
    #[test]
    fn the_config_says_what_it_skipped() {
        let config = settings::InstallationConfig::from_toml(
            "[agents.claude]\ncommand = \"x\"\n[agents.lonely]\nname = \"Lonely\"\n\
             [agents.gemini]\ncommand = \"gemini\"\n",
        )
        .unwrap();
        let (merged, warnings) = configured(&config);
        assert!(merged.iter().any(|program| program.id == "gemini"));
        assert_eq!(
            warnings,
            [
                "agents.claude is skipped: claude is amux's own name for Claude Code, which no entry replaces",
                "agents.lonely is skipped: it names no command, and no built-in agent has that id",
            ]
        );
    }

    fn executable(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        os::fs::make_executable(path).unwrap();
    }

    /// A program's file name as this platform starts it.
    fn exe(name: &str) -> String {
        os::paths::exe_name(name)
    }

    /// A name is found in the first PATH directory holding a program by
    /// it, as this OS starts programs (os::paths has the rules of each:
    /// the execute bit, `PATHEXT` and npm's shims).
    #[test]
    fn a_command_is_found_in_the_first_path_directory_holding_it() {
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (dir.path().join("first"), dir.path().join("second"));
        executable(&second.join(exe("agy")));
        executable(&first.join(exe("opencode")));
        executable(&second.join(exe("opencode")));
        let path = std::env::join_paths([&first, &second]).unwrap();
        let find = |command: &str| find_command(command, &on(&path), None);
        assert_eq!(find("agy"), Some(second.join(exe("agy"))));
        assert_eq!(find("opencode"), Some(first.join(exe("opencode"))));
        assert_eq!(find("grok"), None, "not installed: not offered");
        assert_eq!(find_command("agy", &on(OsStr::new("")), None), None);
    }

    #[test]
    fn a_command_with_a_path_is_that_file_and_a_tilde_is_the_home() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("bin").join(exe("my-agent"));
        executable(&program);
        let find = |command: &str| find_command(command, &on(OsStr::new("")), Some(dir.path()));
        assert_eq!(find(program.to_str().unwrap()), Some(program.clone()));
        assert_eq!(find("~/bin/my-agent"), Some(program));
        assert_eq!(find("~/bin/missing"), None);
    }

    /// A relative command, or a relative PATH entry, was checked against
    /// the daemon's working directory, so that is the file named, by its
    /// absolute path: the program starts in the agent's folder, where the
    /// relative path would name another file.
    #[test]
    fn a_relative_command_or_path_entry_is_found_as_an_absolute_path() {
        let here = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir_in(&here).unwrap();
        let folder = dir.path().file_name().unwrap().to_owned();
        executable(&dir.path().join(exe("my-agent")));
        let expected = here.join(&folder).join(exe("my-agent"));
        let relative = format!("./{}/my-agent", folder.to_string_lossy());
        let found = find_command(&relative, &on(OsStr::new("")), None).unwrap();
        assert!(found.is_absolute(), "{found:?}");
        assert_eq!(found, expected);
        let found = find_command("my-agent", &on(&folder), None).unwrap();
        assert!(found.is_absolute(), "{found:?}");
        assert_eq!(found, expected);
    }

    /// The program's own PATH is where its command is looked up, as it is
    /// where the program would look.
    #[test]
    fn a_programs_own_path_wins() {
        let dir = tempfile::tempdir().unwrap();
        executable(&dir.path().join(exe("my-agent")));
        let program = AgentProgram {
            id: "mine".into(),
            name: "Mine".into(),
            command: "my-agent".into(),
            args: Vec::new(),
            env: [("PATH".into(), dir.path().to_string_lossy().into_owned())].into(),
            built_in: false,
        };
        assert_eq!(
            program.find(&HashMap::new()),
            Some(dir.path().join(exe("my-agent")))
        );
        let offered = offered(std::slice::from_ref(&program), &HashMap::new());
        assert_eq!(offered.len(), 1);
        let env = program.environment(
            &[
                ("PATH".into(), "/elsewhere".into()),
                ("A".into(), "1".into()),
            ]
            .into(),
        );
        assert_eq!(env["A"], "1");
        assert_eq!(env["PATH"], dir.path().to_string_lossy());
    }

    /// The program's own `PATH` stands over the host's whatever the case
    /// of either: on Windows, which names a variable whatever its case,
    /// the host's `Path` gives way to the program's `PATH`, so the program
    /// is looked for on its own and starts with that one `PATH`. (On Unix
    /// `Path` is another variable, and stays.)
    #[test]
    fn a_programs_own_path_stands_over_the_hosts_whatever_its_case() {
        let host = tempfile::tempdir().unwrap();
        let own = tempfile::tempdir().unwrap();
        for dir in [&host, &own] {
            executable(&dir.path().join(exe("x")));
        }
        // The program's folder first, then this process's `PATH`, which the
        // fake started below may need to load.
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let own_path = std::env::join_paths(
            std::iter::once(own.path().to_owned()).chain(std::env::split_paths(&inherited)),
        )
        .unwrap()
        .to_string_lossy()
        .into_owned();
        let program = AgentProgram {
            id: "x".into(),
            name: "X".into(),
            command: "x".into(),
            args: Vec::new(),
            env: [("PATH".into(), own_path.clone())].into(),
            built_in: false,
        };
        let provider_env: HashMap<String, String> =
            [("Path".into(), host.path().to_string_lossy().into_owned())].into();
        let paths = |env: &HashMap<String, String>| {
            env.iter()
                .filter(|(key, _)| os::paths::same_variable(key, "PATH"))
                .map(|(_, value)| value.clone())
                .collect::<Vec<_>>()
        };
        let env = program.environment(&provider_env);
        assert_eq!(paths(&env), std::slice::from_ref(&own_path));
        assert_eq!(program.find(&provider_env), Some(own.path().join(exe("x"))));

        // Started with that environment, the program sees one `PATH`, its
        // own.
        let log = own.path().join("env.json");
        let started = std::process::Command::new(provider_fakes::binaries::path("fake-acp"))
            // Lists the script's models, none, once its environment is
            // logged, and exits.
            .arg("models")
            .envs(&env)
            .env(provider_fakes::script::ENV_LOG_ENV, &log)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(started.success(), "{started}");
        let seen: HashMap<String, String> =
            serde_json::from_slice(&std::fs::read(&log).unwrap()).unwrap();
        assert_eq!(paths(&seen), [own_path]);
    }

    /// Where a command is found under more than one extension, the
    /// program's own `PATHEXT` picks the file, else the host's for every
    /// provider, before this process's (which puts `.EXE` first): as the
    /// program would be started on Windows. Unix has no `PATHEXT`, and no
    /// file is named `x` alone.
    #[test]
    fn a_programs_own_pathext_picks_the_file_its_command_names() {
        let dir = tempfile::tempdir().unwrap();
        for file in ["x.cmd", "x.exe"] {
            executable(&dir.path().join(file));
        }
        let path = dir.path().to_string_lossy().into_owned();
        let program = |env: &[(&str, &str)]| AgentProgram {
            id: "x".into(),
            name: "X".into(),
            command: "x".into(),
            args: Vec::new(),
            env: env
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .chain([("PATH".to_owned(), path.clone())])
                .collect(),
            built_in: false,
        };
        let found = |program: AgentProgram, provider_env: &[(&str, &str)]| {
            let provider_env = provider_env
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect();
            program
                .find(&provider_env)
                .map(|found| found.file_name().unwrap().to_string_lossy().to_lowercase())
        };
        let shim = cfg!(windows).then(|| "x.cmd".to_owned());
        assert_eq!(found(program(&[("PATHEXT", ".CMD;.EXE")]), &[]), shim);
        assert_eq!(found(program(&[]), &[("PATHEXT", ".CMD;.EXE")]), shim);
        assert_eq!(
            found(
                program(&[("PATHEXT", ".CMD;.EXE")]),
                &[("PATHEXT", ".EXE;.CMD")]
            ),
            shim
        );
    }
}
