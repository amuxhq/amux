//! The agent programs this host can run as ACP agents: the built-in ones
//! (`interpret::acp::BUILT_INS`) merged with a person's `[agents.<id>]`
//! entries, and which of them the host offers, which is the ones whose
//! command is found.
//!
//! amux runs only these commands: a built-in's own, or one a person wrote in
//! their config. It never downloads or installs an agent, so a built-in whose
//! program is not installed is simply not offered.

use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use interpret::acp::BUILT_INS;
use settings::agents::AgentEntry;

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
        if interpret::acp::built_in(&entry.id).is_some() {
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
    /// Where this program's command is, looked up as it will be started:
    /// on the PATH its own environment gives, else the host's for every
    /// provider, else the daemon's.
    pub fn find(&self, provider_env: &HashMap<String, String>) -> Option<PathBuf> {
        Lookup::for_program(&self.env, provider_env).find(&self.command)
    }

    /// The environment it starts with: the host's additions for every
    /// provider, with its own over them.
    pub fn environment(&self, provider_env: &HashMap<String, String>) -> HashMap<String, String> {
        let mut env = provider_env.clone();
        env.extend(self.env.clone());
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

/// How a command is looked up: the PATH, and on Windows the extensions an
/// extensionless name may stand for (`PATHEXT`). Platform-shaped only by
/// `windows`, so the Windows rules run in a test on any machine.
#[derive(Clone, Debug)]
pub struct Lookup {
    pub path: Option<OsString>,
    pub pathext: Option<OsString>,
    pub home: Option<PathBuf>,
    pub windows: bool,
}

/// What Windows tries when `PATHEXT` is unset.
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

impl Lookup {
    /// The lookup the program will be started under on this machine.
    fn for_program(own: &BTreeMap<String, String>, provider_env: &HashMap<String, String>) -> Self {
        // Windows names a variable whatever its case (`Path` is `PATH`).
        let named = |key: &String, name: &str| {
            if cfg!(windows) {
                key.eq_ignore_ascii_case(name)
            } else {
                key == name
            }
        };
        let var = |name: &str| -> Option<OsString> {
            own.iter()
                .chain(provider_env.iter())
                .find(|(key, _)| named(key, name))
                .map(|(_, value)| OsString::from(value))
                .or_else(|| std::env::var_os(name))
        };
        Self {
            path: var("PATH"),
            pathext: var("PATHEXT"),
            home: std::env::home_dir(),
            windows: cfg!(windows),
        }
    }

    /// The file `command` names. A command holding a path separator is that
    /// path, a leading `~/` the home; any other is looked up in each PATH
    /// directory in turn. On Windows a name is found with each `PATHEXT`
    /// extension in turn, which is how npm's `.cmd` shims are found; the
    /// extensionless file npm puts beside a shim is a shell script Windows
    /// cannot start, so a name without an extension of its own is never
    /// taken bare there.
    pub fn find(&self, command: &str) -> Option<PathBuf> {
        if command.is_empty() {
            return None;
        }
        let separated = command.contains('/') || (self.windows && command.contains('\\'));
        if separated {
            let path = match (command.strip_prefix("~/"), &self.home) {
                (Some(rest), Some(home)) => home.join(rest),
                _ => PathBuf::from(command),
            };
            return self
                .candidates(&path)
                .into_iter()
                .find(|path| runnable(path));
        }
        let dirs = self.path.as_deref()?;
        split_paths(dirs, self.windows)
            .into_iter()
            .filter(|dir| !dir.as_os_str().is_empty())
            .flat_map(|dir| self.candidates(&dir.join(command)))
            .find(|path| runnable(path))
    }

    /// The files `path` may be: itself on Unix; on Windows itself when its
    /// extension is one `PATHEXT` lists, then itself with each listed one.
    fn candidates(&self, path: &Path) -> Vec<PathBuf> {
        if !self.windows {
            return vec![path.to_owned()];
        }
        let pathext = self
            .pathext
            .as_deref()
            .and_then(OsStr::to_str)
            .unwrap_or(DEFAULT_PATHEXT);
        let extensions: Vec<&str> = pathext
            .split(';')
            .map(str::trim)
            .filter(|extension| extension.starts_with('.') && extension.len() > 1)
            .collect();
        let own = path
            .extension()
            .and_then(OsStr::to_str)
            .map(|extension| format!(".{extension}"));
        let mut candidates = Vec::new();
        if own.as_deref().is_some_and(|own| {
            extensions
                .iter()
                .any(|listed| listed.eq_ignore_ascii_case(own))
        }) {
            candidates.push(path.to_owned());
        }
        for extension in extensions {
            let mut name = path.as_os_str().to_owned();
            name.push(extension);
            candidates.push(PathBuf::from(name));
            // Windows matches names whatever their case; another file
            // system running the same rules does not.
            let lower = extension.to_ascii_lowercase();
            if lower != extension {
                let mut name = path.as_os_str().to_owned();
                name.push(lower);
                candidates.push(PathBuf::from(name));
            }
        }
        candidates
    }
}

/// The directories a PATH value lists, split as the platform it is for
/// splits them: `;` on Windows, where a directory may be quoted, `:`
/// elsewhere.
fn split_paths(value: &OsStr, windows: bool) -> Vec<PathBuf> {
    let text = value.to_string_lossy();
    let separator = if windows { ';' } else { ':' };
    text.split(separator)
        .map(|dir| PathBuf::from(if windows { dir.trim_matches('"') } else { dir }))
        .collect()
}

/// A file that can be started: on Unix one with an execute bit.
fn runnable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn unix(path: &OsStr) -> Lookup {
        Lookup {
            path: Some(path.to_owned()),
            pathext: None,
            home: None,
            windows: false,
        }
    }

    #[test]
    fn a_command_is_found_in_the_first_path_directory_holding_it() {
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (dir.path().join("first"), dir.path().join("second"));
        executable(&second.join("agy"));
        executable(&first.join("opencode"));
        executable(&second.join("opencode"));
        let path = std::env::join_paths([&first, &second]).unwrap();
        let lookup = unix(&path);
        assert_eq!(lookup.find("agy"), Some(second.join("agy")));
        assert_eq!(lookup.find("opencode"), Some(first.join("opencode")));
        assert_eq!(lookup.find("grok"), None, "not installed: not offered");
        assert_eq!(unix(OsStr::new("")).find("agy"), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_without_an_execute_bit_is_not_a_command() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agy"), "not a program").unwrap();
        assert_eq!(unix(dir.path().as_os_str()).find("agy"), None);
    }

    #[test]
    fn a_command_with_a_path_is_that_file_and_a_tilde_is_the_home() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("bin").join("my-agent");
        executable(&program);
        let lookup = Lookup {
            home: Some(dir.path().to_owned()),
            ..unix(OsStr::new(""))
        };
        assert_eq!(
            lookup.find(program.to_str().unwrap()),
            Some(program.clone())
        );
        assert_eq!(lookup.find("~/bin/my-agent"), Some(program));
        assert_eq!(lookup.find("~/bin/missing"), None);
    }

    /// Windows finds a name by `PATHEXT`, so npm's `opencode.cmd` shim is
    /// found and the extensionless shell script beside it never is; an
    /// `.exe` earlier in the extensions wins in the same directory, and the
    /// directories go in PATH order, split at `;`.
    #[test]
    fn windows_finds_npm_shims_by_pathext() {
        let dir = tempfile::tempdir().unwrap();
        let npm = dir.path().join("npm");
        let tools = dir.path().join("tools");
        executable(&npm.join("opencode"));
        executable(&npm.join("opencode.cmd"));
        executable(&npm.join("opencode.ps1"));
        executable(&npm.join("grok.cmd"));
        executable(&tools.join("grok.exe"));
        executable(&tools.join("only-bare"));
        let path = format!("\"{}\";{}", npm.display(), tools.display());
        let lookup = Lookup {
            path: Some(path.into()),
            pathext: Some(".com;.exe;.bat;.cmd".into()),
            home: None,
            windows: true,
        };
        assert_eq!(lookup.find("opencode"), Some(npm.join("opencode.cmd")));
        assert_eq!(
            lookup.find("grok"),
            Some(npm.join("grok.cmd")),
            "PATH order first"
        );
        assert_eq!(lookup.find("grok.exe"), Some(tools.join("grok.exe")));
        assert_eq!(
            lookup.find("only-bare"),
            None,
            "no extension Windows starts"
        );
        let unset = Lookup {
            pathext: None,
            ..lookup.clone()
        };
        // The default spells its extensions upper case, which a file system
        // that ignores case, as Windows' does, finds either way.
        let lower = |path: Option<PathBuf>| path.map(|path| path.to_string_lossy().to_lowercase());
        assert_eq!(
            lower(unset.find("opencode")),
            lower(Some(npm.join("opencode.cmd"))),
            "the default PATHEXT"
        );
        assert_eq!(
            lookup.find(&format!("{}\\opencode", npm.display())),
            cfg!(windows).then(|| PathBuf::from(format!("{}\\opencode.cmd", npm.display()))),
            "a backslash path is a path on Windows"
        );
    }

    /// The program's own PATH is where its command is looked up, as it is
    /// where the program would look.
    #[test]
    fn a_programs_own_path_wins() {
        let dir = tempfile::tempdir().unwrap();
        executable(&dir.path().join("my-agent"));
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
            Some(dir.path().join("my-agent"))
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
}
