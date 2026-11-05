pub mod edit;

use std::fmt;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

use gethostname::gethostname;
use serde::{Deserialize, Serialize};
use thiserror::Error;
pub use wire::HostName;

fn xdg_dir(env_var: &str, default_suffix: &str) -> PathBuf {
    if let Ok(value) = std::env::var(env_var) {
        return PathBuf::from(value);
    }
    #[cfg(windows)]
    {
        let base = if default_suffix.starts_with(".config") {
            std::env::var("APPDATA").ok()
        } else {
            std::env::var("LOCALAPPDATA")
                .ok()
                .or_else(|| std::env::var("APPDATA").ok())
        };
        if let Some(base) = base {
            return PathBuf::from(base);
        }
    }
    let home = if cfg!(windows) {
        std::env::var("USERPROFILE").ok()
    } else {
        std::env::var("HOME").ok()
    };
    home.map(PathBuf::from)
        .map(|home| home.join(default_suffix))
        .unwrap_or_else(|| PathBuf::from(default_suffix))
}

fn amux_xdg_dir(env_var: &str, default_suffix: &str) -> PathBuf {
    xdg_dir(env_var, default_suffix).join("amux")
}

fn default_data_dir() -> PathBuf {
    amux_xdg_dir("XDG_DATA_HOME", ".local/share")
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
}

/// The amux account service a profile signs in to unless told otherwise.
pub const DEFAULT_CLOUD_URL: &str = "https://amux.sh";

/// The system hostname without its mDNS `.local` suffix.
fn system_host_name() -> String {
    fallback_host_name(
        &gethostname()
            .into_string()
            .unwrap_or_else(|_| "unknown".to_string()),
    )
}

/// The name a host goes by when the config names none: the Mac's Computer
/// Name where there is one, otherwise the system hostname without its mDNS
/// `.local` suffix. On a Mac this runs a subprocess, so the daemon resolves
/// it once at start. A system name that is no host name (a long one, say)
/// is made the nearest one rather than refused: a host must start with no
/// configuration.
pub fn default_host_name() -> HostName {
    #[cfg(target_os = "macos")]
    if let Some(name) = macos_computer_name() {
        return HostName::lossy(&name);
    }
    HostName::lossy(&system_host_name())
}

#[cfg(target_os = "macos")]
fn macos_computer_name() -> Option<String> {
    let output = Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8(output.stdout).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn fallback_host_name(host_name: &str) -> String {
    host_name
        .strip_suffix(".local")
        .unwrap_or(host_name)
        .to_string()
}

/// The host name a configured `host_name` is, or why it is none.
pub fn validate_host_name(host_name: &str) -> Result<HostName, ConfigError> {
    HostName::parse(host_name).map_err(|error| ConfigError::Invalid(format!("host_name: {error}")))
}

/// The front door's file name in the root.
pub const FRONT_DOOR: &str = "amux.sock";

/// A control-key leader parsed from the `ctrl+<char>` format (e.g. `ctrl+a`).
#[derive(Debug, Clone)]
pub struct LeaderKey {
    /// The lowercase character (e.g. 'a' for ctrl+a)
    pub char: u8,
}

impl LeaderKey {
    /// Raw byte value for this key (ctrl+a = 0x01, ctrl+b = 0x02, etc.)
    pub fn raw_byte(&self) -> u8 {
        self.char - b'a' + 1
    }

    /// CSI u escape sequence: ESC[<ascii>;5u
    pub fn csi_u_sequence(&self) -> Vec<u8> {
        let ascii = self.char.to_string();
        let mut seq = vec![27, b'['];
        seq.extend_from_slice(ascii.as_bytes());
        seq.extend_from_slice(b";5u");
        seq
    }

    /// xterm's modifyOtherKeys form: ESC[27;5;<ascii>~
    pub fn modify_other_keys_sequence(&self) -> Vec<u8> {
        format!("\x1b[27;5;{}~", self.char).into_bytes()
    }

    fn parse(s: &str) -> std::result::Result<Self, String> {
        let s = s.trim();
        let lower = s.to_ascii_lowercase();
        let ch = lower
            .strip_prefix("ctrl+")
            .ok_or_else(|| format!("invalid leader key '{s}': expected 'ctrl+<a-z>'"))?;
        if ch.len() != 1 {
            return Err(format!(
                "invalid leader key '{s}': expected single character after 'ctrl+'"
            ));
        }
        let byte = ch.as_bytes()[0];
        if !byte.is_ascii_lowercase() {
            return Err(format!("invalid leader key '{s}': expected 'ctrl+<a-z>'"));
        }
        Ok(Self { char: byte })
    }
}

impl Default for LeaderKey {
    fn default() -> Self {
        Self { char: b'a' }
    }
}

impl fmt::Display for LeaderKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ctrl+{}", self.char as char)
    }
}

impl Serialize for LeaderKey {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for LeaderKey {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        LeaderKey::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// Keybind configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Keybinds {
    /// Leader key prefix for keybinds (default: ctrl+a)
    pub leader: LeaderKey,
}

/// Whether the supervisor installs releases itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Updates {
    Auto,
    Manual,
}

/// Which release manifest the supervisor follows. Preview is for people who
/// asked for builds ahead of stable, so it is never the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    Preview,
}

/// Local-network discovery.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscoverySettings {
    /// Matched exactly against other daemons' advertised scope: a daemon
    /// lists only candidates in its own scope. Empty for real machines; a
    /// worktree or a test network sets its own so it never meets them.
    pub scope: String,
}

/// What each agent process is started with, copied into its spec at spawn.
/// A change affects the next spawn or resume, never a running agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSettings {
    /// How long an agent keeps running after its daemon disappears before
    /// it drains and exits. Basis: minutes, enough to ride out a daemon
    /// update or crash restart.
    pub grace_secs: u64,
    /// How long an orphaned agent waits on an open ask while draining.
    /// Basis: minutes, the same order as the grace.
    pub drain_secs: u64,
    /// The interpreter's facts ring: its whole size, split across the two
    /// segments it keeps. Basis: 2–4 MB, enough to replay recent history
    /// into a dump without keeping a second transcript.
    pub facts_ring_mib: u64,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            grace_secs: 300,
            drain_secs: 300,
            facts_ring_mib: 4,
        }
    }
}

/// What a new agent starts with when a person starts one in the terminal
/// client: per agent, the model, reasoning effort and permission. Shipped
/// with real values, so a new agent always shows what it will run; a person
/// changes them in the config file, and starting one beside a running chat
/// copies that chat's instead. A file that sets some of an agent's values
/// keeps the shipped ones for the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAgentSettings {
    pub claude: NewAgentDefaults,
    pub codex: NewAgentDefaults,
    /// Which Claude a new agent runs (`new_agent.claude.kind`); unset
    /// follows `ui.chat_in`. Read it through
    /// [`InstallationConfig::claude_kind`].
    pub claude_kind: Option<ClaudeKindSetting>,
}

/// Which Claude a new agent runs: headless behind amux's chat, or in its
/// own terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClaudeKindSetting {
    Sdk,
    Terminal,
}

impl Serialize for NewAgentSettings {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Claude<'a> {
            model: &'a str,
            effort: &'a str,
            permission: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            kind: Option<ClaudeKindSetting>,
        }
        #[derive(Serialize)]
        struct Both<'a> {
            claude: Claude<'a>,
            codex: &'a NewAgentDefaults,
        }
        Both {
            claude: Claude {
                model: &self.claude.model,
                effort: &self.claude.effort,
                permission: &self.claude.permission,
                kind: self.claude_kind,
            },
            codex: &self.codex,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NewAgentSettings {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct Given {
            model: Option<String>,
            effort: Option<String>,
            permission: Option<String>,
        }
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct GivenClaude {
            model: Option<String>,
            effort: Option<String>,
            permission: Option<String>,
            kind: Option<ClaudeKindSetting>,
        }
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct Both {
            claude: GivenClaude,
            codex: Given,
        }
        fn over(given: Given, shipped: NewAgentDefaults) -> NewAgentDefaults {
            NewAgentDefaults {
                model: given.model.unwrap_or(shipped.model),
                effort: given.effort.unwrap_or(shipped.effort),
                permission: given.permission.unwrap_or(shipped.permission),
            }
        }
        let given = Both::deserialize(deserializer)?;
        let shipped = Self::default();
        let claude = Given {
            model: given.claude.model,
            effort: given.claude.effort,
            permission: given.claude.permission,
        };
        Ok(Self {
            claude: over(claude, shipped.claude),
            codex: over(given.codex, shipped.codex),
            claude_kind: given.claude.kind,
        })
    }
}

/// One agent's starting model, effort and permission, in the agent's own
/// words: a Claude alias or a Codex model id, the effort as the agent names
/// it, and the permission by the value the agent's catalogue gives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewAgentDefaults {
    pub model: String,
    pub effort: String,
    pub permission: String,
}

impl Default for NewAgentSettings {
    fn default() -> Self {
        Self {
            claude: NewAgentDefaults {
                model: "opus".into(),
                effort: "high".into(),
                permission: "default".into(),
            },
            codex: NewAgentDefaults {
                model: "gpt-6.1-sol".into(),
                effort: "medium".into(),
                permission: "default".into(),
            },
            claude_kind: None,
        }
    }
}

/// Where the palette comes from: the terminal amux was started in, or a
/// shipped theme name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeSetting {
    /// Derived from what the terminal reports about its own colours, with
    /// the shipped dark palette as the fallback for a terminal that does
    /// not answer.
    #[default]
    Terminal,
    Dark,
    Light,
}

/// The terminal colour capability preference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorSetting {
    #[default]
    Auto,
    TrueColor,
    Ansi,
}

/// Where a person chats with their agents: in amux's own chat, or in each
/// agent's own interface (Claude Code's or Codex's), which a new agent is
/// attached to when it starts and `amux attach` opens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "lowercase")]
pub enum ChatInSetting {
    #[default]
    Amux,
    Native,
}

/// Client UI configuration (the TUI; future desktop clients read the same
/// keys).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    /// The palette: the terminal's own colours, or the shipped dark or
    /// light theme.
    pub theme: ThemeSetting,
    /// Whether to detect, force, or disable truecolor output.
    pub color: ColorSetting,
    /// Where agents are chatted with: `amux` or `native`.
    pub chat_in: ChatInSetting,
}

/// Preferences shared by every profile in an installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InstallationConfig {
    /// What paired hosts and the phone call this machine. Absent means
    /// [`default_host_name`], resolved once by the daemon.
    pub host_name: Option<String>,
    /// The folders new-agent pickers offer first, on this machine and from
    /// every paired device. A leading `~/` is the home directory.
    pub repositories: Vec<PathBuf>,
    /// Whether amux starts when the person logs in: the login item that
    /// runs `amux supervise`, written or removed when this changes. Only an
    /// install with a supervisor has one.
    pub start_at_login: bool,
    /// Whether this install runs the daemon under `amux supervise`. A host
    /// whose service manager runs `amux daemon` directly sets it false.
    pub supervisor: bool,
    /// Whether the supervisor also installs releases. Absent means `auto`
    /// under a supervisor and `manual` without one; `auto` without a
    /// supervisor is an error. Read it through [`InstallationConfig::updates`].
    pub updates: Option<Updates>,
    pub channel: Channel,
    /// Whether amux keeps the machine awake: on power whenever it runs, on
    /// battery only while an agent is working.
    pub keep_awake: bool,
    /// Whether a published build sends product analytics to amux.sh. The
    /// `DO_NOT_TRACK` environment variable turns it off as well.
    pub telemetry: bool,
    /// Whether the reports amux writes when one of its checks breaks are
    /// sent to the account the profile is signed in to. A report holds the
    /// records of what amux's parts concluded and amux's own masked logs,
    /// never what anybody wrote; still, only a person turns this on.
    pub auto_reports: bool,
    pub ui: UiSettings,
    pub keybinds: Keybinds,
    pub new_agent: NewAgentSettings,
    /// Where the installation keeps its data: profiles, stores, reports.
    pub root: PathBuf,
    /// Where channel manifests live: the supervisor reads
    /// `<releases_url>/<channel>.json`.
    pub releases_url: String,
    pub discovery: DiscoverySettings,
    pub agent: AgentSettings,
    #[serde(skip)]
    pub path: Option<PathBuf>,
    /// Keys the file set that this version does not know, as dotted paths.
    /// They are ignored rather than refused, so one file still starts an
    /// older or newer amux; whoever loads the file warns about them.
    #[serde(skip)]
    pub ignored: Vec<String>,
}

impl Default for InstallationConfig {
    fn default() -> Self {
        Self {
            host_name: None,
            repositories: Vec::new(),
            start_at_login: false,
            supervisor: true,
            updates: None,
            channel: Channel::default(),
            keep_awake: true,
            telemetry: true,
            auto_reports: false,
            ui: UiSettings::default(),
            keybinds: Keybinds::default(),
            new_agent: NewAgentSettings::default(),
            root: default_data_dir(),
            releases_url: format!("{DEFAULT_CLOUD_URL}/releases"),
            discovery: DiscoverySettings::default(),
            agent: AgentSettings::default(),
            path: None,
            ignored: Vec::new(),
        }
    }
}

impl InstallationConfig {
    pub fn default_path() -> PathBuf {
        amux_xdg_dir("XDG_CONFIG_HOME", ".config").join("config.toml")
    }

    /// Parses installation TOML, refusing an impossible supervisor and
    /// updates pair. Unknown keys are collected in
    /// [`ignored`](Self::ignored).
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let mut ignored = Vec::new();
        let invalid = |error: toml::de::Error| ConfigError::Invalid(error.to_string());
        let document = toml::Deserializer::parse(text).map_err(invalid)?;
        let mut config: Self =
            serde_ignored::deserialize(document, |key| ignored.push(key.to_string()))
                .map_err(invalid)?;
        config.ignored = ignored;
        config.updates()?;
        Ok(config)
    }

    /// The effective updates mode; `auto` without a supervisor is an error,
    /// since only a supervisor installs releases.
    pub fn updates(&self) -> Result<Updates, ConfigError> {
        match (self.supervisor, self.updates) {
            (false, Some(Updates::Auto)) => Err(ConfigError::Invalid(
                "updates = \"auto\" needs supervisor = true; without a supervisor, updates are deploys (updates = \"manual\")".into(),
            )),
            (_, Some(updates)) => Ok(updates),
            (true, None) => Ok(Updates::Auto),
            (false, None) => Ok(Updates::Manual),
        }
    }

    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let path = absolute_path(path, &std::env::current_dir()?)?;
        let text = std::fs::read_to_string(&path)?;
        let mut config = Self::from_toml(&text)
            .map_err(|error| ConfigError::Invalid(format!("{}: {error}", path.display())))?;
        let base = path.parent().unwrap();
        config.root = absolute_path(&config.root, base)?;
        config.path = Some(path);
        config.validate()?;
        Ok(config)
    }

    /// Checks what parsing alone cannot: the supervisor and updates pair
    /// and the host name.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.updates()?;
        match &self.host_name {
            Some(host_name) => validate_host_name(host_name).map(drop),
            None => Ok(()),
        }
    }

    /// The front door, where `InstallationService` answers: `amux.sock` in
    /// the root, so whoever finds the root finds the daemon. A path too long
    /// for a Unix socket address is reached through a short link, and on
    /// Windows the file names the pipe the daemon listens on; both ends
    /// derive the same address from the path
    /// (`agent_dir::local_socket`).
    pub fn front_door(&self) -> PathBuf {
        self.root.join(FRONT_DOOR)
    }

    /// The name this host goes by: the configured one, else
    /// [`default_host_name`]. A configured name [`Self::validate`] refuses
    /// is taken as the nearest host name to it.
    pub fn host_name(&self) -> HostName {
        match &self.host_name {
            Some(host_name) => HostName::lossy(host_name),
            None => default_host_name(),
        }
    }

    /// Which Claude a new agent runs: the configured one, else the one the
    /// chat interface implies (amux's chat runs it headless, the agent's
    /// own interface in its terminal).
    pub fn claude_kind(&self) -> ClaudeKindSetting {
        self.new_agent.claude_kind.unwrap_or(match self.ui.chat_in {
            ChatInSetting::Amux => ClaudeKindSetting::Sdk,
            ChatInSetting::Native => ClaudeKindSetting::Terminal,
        })
    }

    /// The repository list with a leading `~/` made the home directory.
    pub fn repositories(&self) -> Vec<PathBuf> {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
        self.repositories
            .iter()
            .map(|path| match (path.strip_prefix("~"), &home) {
                (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
                _ => path.clone(),
            })
            .collect()
    }

    /// The configured channel as the manifests name it.
    pub fn channel_name(&self) -> &'static str {
        match self.channel {
            Channel::Stable => "stable",
            Channel::Preview => "preview",
        }
    }

    /// The manifest the supervisor follows for the configured channel.
    pub fn manifest_url(&self) -> String {
        format!(
            "{}/{}.json",
            self.releases_url.trim_end_matches('/'),
            self.channel_name()
        )
    }
}

/// Every key the installation's file takes, dotted as `amux config` names
/// them, in alphabetical order: the leaves of a default config, and the
/// three that are absent until set.
pub fn installation_keys() -> Vec<String> {
    fn leaves(prefix: &str, table: &toml::Table, out: &mut Vec<String>) {
        for (key, value) in table {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            match value {
                toml::Value::Table(inner) => leaves(&path, inner, out),
                _ => out.push(path),
            }
        }
    }
    let default = toml::Table::try_from(InstallationConfig::default())
        .expect("the default config serialises");
    let mut keys = vec![
        "host_name".to_owned(),
        "new_agent.claude.kind".to_owned(),
        "updates".to_owned(),
    ];
    leaves("", &default, &mut keys);
    keys.sort();
    keys
}

/// Whether a key is about how amux is deployed or developed rather than an
/// everyday preference: the supervisor and updates, where releases and data
/// live, discovery's scope and the agent processes' tunables. They stay in
/// the file and `amux config`, but no settings screen shows them.
pub fn is_deployment_key(key: &str) -> bool {
    matches!(
        key,
        "supervisor" | "updates" | "releases_url" | "discovery.scope" | "root"
    ) || key.starts_with("agent.")
}

// Resolve aliases in the existing ancestor, even before a socket or state file
// exists. This gives /tmp and /private/tmp the same identity on macOS.
fn absolute_path(path: &Path, base: &Path) -> Result<PathBuf, ConfigError> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    };
    match std::fs::canonicalize(&path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or_else(|| {
                ConfigError::Invalid(format!("cannot resolve {}", path.display()))
            })?;
            let parent = absolute_path(parent, base)?;
            match path.file_name() {
                Some(name) => Ok(parent.join(name)),
                None => Err(ConfigError::Invalid(format!(
                    "cannot resolve {}",
                    path.display()
                ))),
            }
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installation(text: &str) -> Result<InstallationConfig, ConfigError> {
        InstallationConfig::from_toml(text)
    }

    /// The front door sits in the root the file names, resolved beside the
    /// file like the root itself.
    #[test]
    fn the_front_door_is_in_the_configured_root() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "root = \"install\"\n").unwrap();
        let config = InstallationConfig::from_file(&path).unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().join("install");
        assert_eq!(config.root, root);
        assert_eq!(config.front_door(), root.join("amux.sock"));
    }

    /// With no file at all, the front door is in the default root.
    #[test]
    fn the_default_front_door_is_in_the_data_dir() {
        assert_eq!(
            InstallationConfig::default().front_door(),
            default_data_dir().join("amux.sock")
        );
    }

    #[test]
    fn the_front_door_follows_the_root() {
        let config = installation("root = \"/srv/amux\"\n").unwrap();
        assert_eq!(config.front_door(), Path::new("/srv/amux/amux.sock"));
    }

    #[test]
    fn an_agent_setting_given_alone_keeps_the_shipped_rest() {
        let config = installation("[new_agent.claude]\nmodel = \"sonnet\"\n").unwrap();
        assert_eq!(config.new_agent.claude.model, "sonnet");
        assert_eq!(
            config.new_agent.claude.effort,
            NewAgentSettings::default().claude.effort
        );
        assert_eq!(config.new_agent.codex, NewAgentSettings::default().codex);
    }

    #[test]
    fn root_defaults_to_the_data_dir() {
        assert_eq!(InstallationConfig::default().root, default_data_dir());
        assert_eq!(installation("").unwrap().root, default_data_dir());
    }

    #[test]
    fn leader_key_ctrl_b() {
        let leader = LeaderKey::parse("ctrl+b").unwrap();
        assert_eq!(leader.char, b'b');
        assert_eq!(leader.raw_byte(), 0x02);
        // ESC[98;5u
        assert_eq!(
            leader.csi_u_sequence(),
            vec![27, b'[', b'9', b'8', b';', b'5', b'u']
        );
    }

    #[test]
    fn leader_key_case_insensitive() {
        let leader = LeaderKey::parse("Ctrl+A").unwrap();
        assert_eq!(leader.char, b'a');
    }

    #[test]
    fn leader_key_invalid() {
        assert!(LeaderKey::parse("alt+a").is_err());
        assert!(LeaderKey::parse("ctrl+1").is_err());
        assert!(LeaderKey::parse("ctrl+ab").is_err());
        assert!(LeaderKey::parse("a").is_err());
    }

    #[test]
    fn config_with_keybinds() {
        let config = installation("[keybinds]\nleader = \"ctrl+b\"\n").unwrap();
        assert_eq!(config.keybinds.leader.char, b'b');
    }

    #[test]
    fn config_without_keybinds_uses_default() {
        let config = installation("host_name = \"test\"\n").unwrap();
        assert_eq!(config.keybinds.leader.char, b'a');
        assert_eq!(config.host_name(), "test");
    }

    #[test]
    fn ui_theme_and_color_defaults_are_terminal_and_auto() {
        let config = InstallationConfig::default();
        assert_eq!(config.ui.theme, ThemeSetting::Terminal);
        assert_eq!(config.ui.color, ColorSetting::Auto);

        let parsed = installation("[ui]\n").unwrap();
        assert_eq!(parsed.ui.theme, ThemeSetting::Terminal);
        assert_eq!(parsed.ui.color, ColorSetting::Auto);
    }

    #[test]
    fn ui_theme_and_color_roundtrip() {
        let config = installation("[ui]\ntheme = \"light\"\ncolor = \"ansi\"\n").unwrap();
        assert_eq!(config.ui.theme, ThemeSetting::Light);
        assert_eq!(config.ui.color, ColorSetting::Ansi);

        let parsed = installation(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(parsed.ui.theme, ThemeSetting::Light);
        assert_eq!(parsed.ui.color, ColorSetting::Ansi);
    }

    #[test]
    fn unknown_ui_color_is_rejected() {
        let error = installation("[ui]\ncolor = \"millions\"\n").unwrap_err();
        assert!(error.to_string().contains("unknown variant"));
    }

    #[test]
    fn unknown_keys_are_ignored_and_named() {
        let config = installation(
            "host_name = \"desk\"\nprevent_idle_sleep = true\n\
             [retention]\nown_budget_mib = 1\n\
             [agent]\nfacts_ring_segments = 3\n\
             [new_agent.claude]\nmodel = \"opus\"\neffort = \"high\"\npermission = \"default\"\nflavour = 1\n",
        )
        .unwrap();
        assert_eq!(config.host_name(), "desk");
        assert_eq!(config.new_agent.claude.model, "opus");
        assert_eq!(
            config.ignored,
            [
                "agent.facts_ring_segments",
                "new_agent.claude.flavour",
                "prevent_idle_sleep",
                "retention",
            ]
        );
        assert!(
            installation("host_name = \"desk\"\n")
                .unwrap()
                .ignored
                .is_empty()
        );
    }

    #[test]
    fn a_yaml_file_is_refused() {
        assert!(installation("host_name: desk\nkeep_awake: off\n").is_err());
    }

    #[test]
    fn validate_rejects_empty_host_name() {
        let config = InstallationConfig {
            host_name: Some(String::new()),
            ..InstallationConfig::default()
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("host_name"));
    }

    #[test]
    fn validate_accepts_long_host_name() {
        let config = InstallationConfig {
            host_name: Some("a".repeat(wire::MAX_HOST_NAME_CHARS)),
            ..InstallationConfig::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_rejects_oversized_host_name() {
        let config = InstallationConfig {
            host_name: Some("a".repeat(wire::MAX_HOST_NAME_CHARS + 1)),
            ..InstallationConfig::default()
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("host_name"));
    }

    #[test]
    fn validate_rejects_a_host_name_with_control_characters() {
        let config = InstallationConfig {
            host_name: Some("desk\u{1b}]52;c;aGk=\u{7}".to_owned()),
            ..InstallationConfig::default()
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("control"), "{err}");
    }

    #[test]
    fn an_unnamed_host_goes_by_a_default_name() {
        assert!(
            !InstallationConfig::default()
                .host_name()
                .as_str()
                .is_empty()
        );
    }

    #[test]
    fn fallback_host_name_strips_the_mdns_suffix() {
        assert_eq!(fallback_host_name("Adas-MacBook.local"), "Adas-MacBook");
        assert_eq!(fallback_host_name("build-host"), "build-host");
    }

    #[test]
    fn supervisor_and_updates_cover_every_cell_of_the_table() {
        // (supervisor, updates) as written -> effective updates, or an error.
        let cells: &[(&str, Option<Updates>)] = &[
            (
                "supervisor = true\nupdates = \"auto\"\n",
                Some(Updates::Auto),
            ),
            (
                "supervisor = true\nupdates = \"manual\"\n",
                Some(Updates::Manual),
            ),
            (
                "supervisor = false\nupdates = \"manual\"\n",
                Some(Updates::Manual),
            ),
            ("supervisor = false\nupdates = \"auto\"\n", None),
            ("supervisor = true\n", Some(Updates::Auto)),
            ("supervisor = false\n", Some(Updates::Manual)),
            ("", Some(Updates::Auto)),
            ("updates = \"manual\"\n", Some(Updates::Manual)),
        ];
        for (text, expected) in cells {
            match (installation(text), expected) {
                (Ok(config), Some(updates)) => {
                    assert_eq!(config.updates().unwrap(), *updates, "{text}");
                }
                (Err(error), None) => {
                    assert!(
                        error
                            .to_string()
                            .contains("updates = \"auto\" needs supervisor = true"),
                        "{text}: {error}"
                    );
                }
                (outcome, expected) => panic!("{text}: {outcome:?}, expected {expected:?}"),
            }
        }
    }

    #[test]
    fn a_constructed_config_with_auto_and_no_supervisor_fails_validation() {
        let config = InstallationConfig {
            supervisor: false,
            updates: Some(Updates::Auto),
            ..InstallationConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn supervisor_updates_channel_and_keep_awake_parse_and_default() {
        let defaults = installation("").unwrap();
        assert!(defaults.supervisor);
        assert_eq!(defaults.channel, Channel::Stable);
        assert!(defaults.keep_awake);
        assert!(defaults.telemetry);

        let set = installation(
            "supervisor = false\nchannel = \"preview\"\nkeep_awake = false\ntelemetry = false\n",
        )
        .unwrap();
        assert!(!set.supervisor);
        assert_eq!(set.channel, Channel::Preview);
        assert!(!set.keep_awake);
        assert!(!set.telemetry);

        for bad in [
            "supervisor = \"on\"\n",
            "updates = \"sometimes\"\n",
            "channel = \"beta\"\n",
            "keep_awake = \"off\"\n",
            "telemetry = 0\n",
        ] {
            assert!(installation(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn discovery_and_agent_parameters_parse_with_starting_points() {
        let defaults = installation("").unwrap();
        assert_eq!(defaults.discovery.scope, "");
        assert_eq!(defaults.agent, AgentSettings::default());
        assert_eq!(defaults.agent.grace_secs, 300);
        assert_eq!(defaults.agent.drain_secs, 300);

        let set = installation(
            "[discovery]\nscope = \"rearchitect\"\n\
             [agent]\ngrace_secs = 60\ndrain_secs = 30\nfacts_ring_mib = 2\n",
        )
        .unwrap();
        assert_eq!(set.discovery.scope, "rearchitect");
        assert_eq!(
            set.agent,
            AgentSettings {
                grace_secs: 60,
                drain_secs: 30,
                facts_ring_mib: 2,
            }
        );
    }

    #[test]
    fn the_claude_kind_follows_the_interface_until_set() {
        let config = installation("").unwrap();
        assert_eq!(config.claude_kind(), ClaudeKindSetting::Sdk);
        let native = installation("[ui]\nchat_in = \"native\"\n").unwrap();
        assert_eq!(native.claude_kind(), ClaudeKindSetting::Terminal);
        let set = installation(
            "[ui]\nchat_in = \"native\"\n[new_agent.claude]\nkind = \"sdk\"\nmodel = \"haiku\"\n",
        )
        .unwrap();
        assert_eq!(set.claude_kind(), ClaudeKindSetting::Sdk);
        assert_eq!(set.new_agent.claude.model, "haiku");
        assert!(set.ignored.is_empty(), "{:?}", set.ignored);
        // It writes back where it was read from.
        let again = installation(&toml::to_string(&set).unwrap()).unwrap();
        assert_eq!(again.new_agent, set.new_agent);
        assert!(installation_keys().contains(&"new_agent.claude.kind".to_owned()));
        assert!(!installation_keys().contains(&"new_agent.codex.kind".to_owned()));
    }

    #[test]
    fn repositories_and_start_at_login_parse_and_default() {
        let defaults = installation("").unwrap();
        assert!(defaults.repositories.is_empty());
        assert!(!defaults.start_at_login);
        let set =
            installation("repositories = [\"/srv/app\", \"~/code\"]\nstart_at_login = true\n")
                .unwrap();
        assert!(set.start_at_login);
        let repositories = set.repositories();
        assert_eq!(repositories[0], Path::new("/srv/app"));
        assert!(repositories[1].is_absolute() && repositories[1].ends_with("code"));
        assert!(
            installation("repository_roots = [\"/srv\"]\n")
                .unwrap()
                .ignored
                == ["repository_roots"]
        );
    }

    #[test]
    fn deployment_keys_are_told_from_everyday_ones() {
        for key in [
            "supervisor",
            "updates",
            "releases_url",
            "discovery.scope",
            "root",
            "agent.grace_secs",
        ] {
            assert!(is_deployment_key(key), "{key}");
        }
        for key in [
            "host_name",
            "repositories",
            "keep_awake",
            "telemetry",
            "ui.chat_in",
            "channel",
        ] {
            assert!(!is_deployment_key(key), "{key}");
        }
        for key in installation_keys() {
            assert!(
                is_deployment_key(&key) || !key.starts_with("agent."),
                "{key}"
            );
        }
    }

    #[test]
    fn new_agent_defaults_parse_per_agent() {
        let set = installation(
            "[new_agent.claude]\nmodel = \"haiku\"\neffort = \"low\"\npermission = \"default\"\n",
        )
        .unwrap();
        assert_eq!(set.new_agent.claude.model, "haiku");
        assert_eq!(set.new_agent.codex, NewAgentSettings::default().codex);
    }
}
