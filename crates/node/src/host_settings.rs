//! The installation's everyday settings as the front door serves them: read
//! from the config file, changed in it key by key as `amux config` changes
//! it, and applied to the running daemon the moment they change, whichever
//! of the two changed them. The deployment keys (the supervisor, updates,
//! where releases and data live, discovery's scope, the agents' tunables)
//! are the file's and `amux config`'s alone.
//!
//! Without a file (a test, the phone's embedded runtime) the settings are
//! kept in memory, starting from what the embedder gave.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use settings::edit::Change;
use settings::{ChatInSetting, ClaudeKindSetting, ConfigError, InstallationConfig};
use wire::{ErrorCode, UpdateSettingsRequest};

use crate::supervisor::login::StartAtLogin;

/// Where an installation's settings live.
#[derive(Clone, Debug, Default)]
pub struct SettingsOptions {
    /// The config file; none keeps them in memory.
    pub file: Option<PathBuf>,
    /// The settings the daemon starts with: the file's, or the embedder's.
    pub initial: InstallationConfig,
    /// The login item `start_at_login` keeps, on an install whose
    /// supervisor a login item would start; none has nothing to start.
    pub login: Option<StartAtLogin>,
}

/// The kind a host's settings resolve to for a new Claude agent.
pub fn claude_kind(config: &InstallationConfig) -> wire::Kind {
    match config.claude_kind() {
        ClaudeKindSetting::Sdk => wire::Kind::ClaudeSdk,
        ClaudeKindSetting::Terminal => wire::Kind::ClaudePty,
    }
}

/// Why a change was refused.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl SettingsError {
    pub(crate) fn to_wire(&self) -> wire::Error {
        let code = match self {
            SettingsError::Invalid(_) => ErrorCode::InvalidArgument,
            SettingsError::Io(_) => ErrorCode::Internal,
        };
        crate::grpc::wire_error(code, self.to_string())
    }
}

impl From<ConfigError> for SettingsError {
    fn from(error: ConfigError) -> Self {
        match error {
            ConfigError::Io(error) => SettingsError::Io(error),
            ConfigError::Invalid(reason) => SettingsError::Invalid(reason),
        }
    }
}

pub(crate) struct HostSettings {
    file: Option<PathBuf>,
    login: Option<StartAtLogin>,
    held: Mutex<Held>,
}

struct Held {
    /// The file as last read or written, or the settings in memory.
    text: String,
    config: InstallationConfig,
    /// The file's modification time and length when last read, so a change
    /// made beside the daemon is noticed.
    stamp: Option<(SystemTime, u64)>,
}

impl HostSettings {
    pub(crate) fn new(options: SettingsOptions) -> HostSettings {
        let SettingsOptions {
            file,
            initial,
            login,
        } = options;
        let (text, stamp) = match &file {
            Some(path) => (
                std::fs::read_to_string(path).unwrap_or_default(),
                stamp(path),
            ),
            None => (toml::to_string(&initial).unwrap_or_default(), None),
        };
        HostSettings {
            file,
            login,
            held: Mutex::new(Held {
                text,
                config: initial,
                stamp,
            }),
        }
    }

    /// The settings now.
    pub(crate) fn current(&self) -> InstallationConfig {
        self.held.lock().unwrap().config.clone()
    }

    /// Whether this install has a login item to keep.
    pub(crate) fn supervised(&self) -> bool {
        self.login.is_some()
    }

    /// Reads the file again when it changed since it was last read, and
    /// answers the settings it now holds; none when it did not change, or
    /// when it no longer reads, which is logged and leaves the last good
    /// settings in force.
    pub(crate) fn reread(&self) -> Option<InstallationConfig> {
        let path = self.file.as_ref()?;
        let now = stamp(path);
        let mut held = self.held.lock().unwrap();
        if now == held.stamp {
            return None;
        }
        held.stamp = now;
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "reading the config");
                return None;
            }
        };
        match read(&text, path) {
            Ok(config) => {
                held.text = text;
                held.config = config.clone();
                Some(config)
            }
            Err(error) => {
                tracing::warn!(%error, "the config no longer reads; keeping its last settings");
                None
            }
        }
    }

    /// Makes the change in the file (or in memory) and answers the settings
    /// it leaves. The login item follows `start_at_login` before the file
    /// says so, so a login item that cannot be written leaves both alone.
    pub(crate) fn update(
        &self,
        request: &UpdateSettingsRequest,
    ) -> Result<InstallationConfig, SettingsError> {
        let changes = changes(request)?;
        let mut held = self.held.lock().unwrap();
        // A change made beside the daemon since it last looked is kept.
        if let Some(path) = &self.file {
            held.text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => return Err(error.into()),
            };
        }
        let (text, edited) = settings::edit::edit(&held.text, &changes)?;
        let config = match &self.file {
            Some(path) => read(&text, path)?,
            None => InstallationConfig {
                path: None,
                ..edited
            },
        };
        if let (Some(on), Some(login)) = (request.start_at_login, &self.login)
            && (on != held.config.start_at_login || on != login.is_on())
        {
            login.set(on)?;
        }
        if let Some(path) = &self.file {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            crate::install::write_durably(path, text.as_bytes())?;
            held.stamp = stamp(path);
        }
        held.text = text;
        held.config = config.clone();
        Ok(config)
    }
}

/// The settings as the call answers them.
pub(crate) fn to_wire(
    config: &InstallationConfig,
    host_name: &str,
    supervised: bool,
) -> wire::Settings {
    wire::Settings {
        host_name: host_name.to_owned(),
        start_at_login: supervised.then_some(config.start_at_login),
        keep_awake: config.keep_awake,
        projects: config
            .projects()
            .iter()
            .filter_map(|path| path.to_str().map(str::to_owned))
            .collect(),
        chat_in: match config.ui.chat_in {
            ChatInSetting::Amux => wire::ChatIn::Amux,
            ChatInSetting::Native => wire::ChatIn::Native,
        } as i32,
        claude_kind: match config.new_agent.claude_kind {
            None => wire::Kind::Unspecified,
            Some(ClaudeKindSetting::Sdk) => wire::Kind::ClaudeSdk,
            Some(ClaudeKindSetting::Terminal) => wire::Kind::ClaudePty,
        } as i32,
        telemetry: config.telemetry,
        auto_reports: config.auto_reports,
    }
}

/// The file's keys a request changes.
fn changes(request: &UpdateSettingsRequest) -> Result<Vec<Change<'static>>, SettingsError> {
    let mut changes: Vec<Change> = Vec::new();
    if let Some(name) = &request.host_name {
        let name = name.trim();
        changes.push(("host_name", (!name.is_empty()).then(|| name.into())));
    }
    if let Some(on) = request.start_at_login {
        changes.push(("start_at_login", Some(on.into())));
    }
    if let Some(on) = request.keep_awake {
        changes.push(("keep_awake", Some(on.into())));
    }
    if let Some(projects) = &request.projects {
        let mut array = toml_edit::Array::new();
        let mut seen = std::collections::BTreeSet::new();
        for path in &projects.paths {
            let path = project(path)?;
            if seen.insert(path.clone()) {
                array.push(path);
            }
        }
        changes.push(("projects", Some(array.into())));
    }
    if let Some(chat_in) = request.chat_in {
        let word = match wire::ChatIn::try_from(chat_in) {
            Ok(wire::ChatIn::Amux) => "amux",
            Ok(wire::ChatIn::Native) => "native",
            _ => return Err(SettingsError::Invalid("chat_in names no interface".into())),
        };
        changes.push(("ui.chat_in", Some(word.into())));
    }
    if let Some(kind) = request.claude_kind {
        let word = match wire::Kind::try_from(kind) {
            Ok(wire::Kind::Unspecified) => None,
            Ok(wire::Kind::ClaudeSdk) => Some("sdk"),
            Ok(wire::Kind::ClaudePty) => Some("terminal"),
            _ => {
                return Err(SettingsError::Invalid(
                    "claude_kind is a Claude kind, or unspecified to follow chat_in".into(),
                ));
            }
        };
        changes.push(("new_agent.claude.kind", word.map(Into::into)));
    }
    if let Some(on) = request.telemetry {
        changes.push(("telemetry", Some(on.into())));
    }
    if let Some(on) = request.auto_reports {
        changes.push(("auto_reports", Some(on.into())));
    }
    Ok(changes)
}

/// A project as the file keeps it: absolute, and canonical where it exists.
fn project(path: &str) -> Result<String, SettingsError> {
    let given = Path::new(path.trim());
    if !given.is_absolute() {
        return Err(SettingsError::Invalid(format!(
            "a project is an absolute path, not {path:?}"
        )));
    }
    let path = given.canonicalize().unwrap_or_else(|_| given.to_owned());
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| SettingsError::Invalid(format!("{path:?} is not UTF-8")))
}

/// The settings `text` holds as the file at `path`, its root resolved
/// beside it.
fn read(text: &str, path: &Path) -> Result<InstallationConfig, ConfigError> {
    let mut config = InstallationConfig::from_toml(text)?;
    if config.root.is_relative()
        && let Some(base) = path.parent()
    {
        config.root = base.join(&config.root);
    }
    config.path = Some(path.to_owned());
    config.validate()?;
    Ok(config)
}

fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> UpdateSettingsRequest {
        UpdateSettingsRequest::default()
    }

    #[test]
    fn a_change_is_written_to_the_file_key_by_key_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# mine\nchannel = \"preview\"\nkeep_awake = true\n").unwrap();
        let project = dir.path().join("app");
        std::fs::create_dir(&project).unwrap();
        let settings = HostSettings::new(SettingsOptions {
            file: Some(path.clone()),
            initial: InstallationConfig::from_file(&path).unwrap(),
            login: None,
        });
        let config = settings
            .update(&UpdateSettingsRequest {
                keep_awake: Some(false),
                projects: Some(wire::ProjectList {
                    paths: vec![
                        project.to_str().unwrap().to_owned(),
                        project.join("../app").to_str().unwrap().to_owned(),
                    ],
                }),
                claude_kind: Some(wire::Kind::ClaudePty as i32),
                host_name: Some("desk".into()),
                ..request()
            })
            .unwrap();
        assert!(!config.keep_awake);
        assert_eq!(config.projects, [project.canonicalize().unwrap()]);
        assert_eq!(claude_kind(&config), wire::Kind::ClaudePty);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# mine\nchannel = \"preview\"\n"),
            "{text}"
        );
        assert_eq!(
            InstallationConfig::from_file(&path).unwrap().host_name(),
            "desk"
        );
        assert!(settings.reread().is_none(), "its own write is no news");

        let wire = to_wire(&config, "desk", false);
        assert_eq!(wire.start_at_login, None);
        assert_eq!(wire.claude_kind, wire::Kind::ClaudePty as i32);

        // Back to following the interface, and to the default name.
        let config = settings
            .update(&UpdateSettingsRequest {
                claude_kind: Some(wire::Kind::Unspecified as i32),
                host_name: Some(String::new()),
                ..request()
            })
            .unwrap();
        assert_eq!(config.new_agent.claude_kind, None);
        assert_eq!(config.host_name, None);

        for refused in [
            UpdateSettingsRequest {
                projects: Some(wire::ProjectList {
                    paths: vec!["relative/app".into()],
                }),
                ..request()
            },
            UpdateSettingsRequest {
                claude_kind: Some(wire::Kind::Codex as i32),
                ..request()
            },
            UpdateSettingsRequest {
                host_name: Some("desk\u{7}".into()),
                ..request()
            },
        ] {
            assert!(settings.update(&refused).is_err(), "{refused:?}");
        }
    }

    #[test]
    fn a_change_made_beside_the_daemon_is_noticed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "keep_awake = true\n").unwrap();
        let settings = HostSettings::new(SettingsOptions {
            file: Some(path.clone()),
            initial: InstallationConfig::from_file(&path).unwrap(),
            login: None,
        });
        assert!(settings.reread().is_none());
        std::fs::write(&path, "keep_awake = false\ntelemetry = false\n").unwrap();
        let config = settings.reread().expect("a change");
        assert!(!config.keep_awake && !config.telemetry);
        std::fs::write(&path, "keep_awake = \"sometimes\"\n").unwrap();
        assert!(settings.reread().is_none(), "a file that does not read");
        assert!(!settings.current().keep_awake);
    }

    #[test]
    fn without_a_file_the_settings_are_kept_in_memory() {
        let settings = HostSettings::new(SettingsOptions::default());
        let config = settings
            .update(&UpdateSettingsRequest {
                telemetry: Some(false),
                chat_in: Some(wire::ChatIn::Native as i32),
                ..request()
            })
            .unwrap();
        assert!(!config.telemetry);
        assert_eq!(claude_kind(&config), wire::Kind::ClaudePty);
        assert!(!settings.current().telemetry);
        assert!(settings.reread().is_none());
    }
}
