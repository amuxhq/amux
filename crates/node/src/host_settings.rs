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

#[cfg(not(any(target_os = "ios", target_os = "android")))]
use crate::supervisor::login::StartAtLogin;

/// A phone runs no supervisor, so nothing there is started at login and no
/// login item can be named.
#[cfg(any(target_os = "ios", target_os = "android"))]
#[derive(Clone, Debug)]
pub enum StartAtLogin {}

#[cfg(any(target_os = "ios", target_os = "android"))]
impl StartAtLogin {
    fn is_on(&self) -> bool {
        match *self {}
    }

    fn set(&self, _on: bool) -> std::io::Result<()> {
        match *self {}
    }
}

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
    /// Where repository suggestions read the session logs: the person's home
    /// when none.
    pub home: Option<PathBuf>,
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
            // The file holds a value where the change needs a table: the
            // request cannot be made until a person changes the file.
            error @ ConfigError::InTheWay { .. } => SettingsError::Invalid(error.to_string()),
        }
    }
}

pub(crate) struct HostSettings {
    file: Option<PathBuf>,
    login: Option<StartAtLogin>,
    home: Option<PathBuf>,
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
            home,
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
            home: home.or_else(std::env::home_dir),
            held: Mutex::new(Held {
                text,
                config: initial,
                stamp,
            }),
        }
    }

    /// Where repository suggestions look; none on a system with no home.
    pub(crate) fn home(&self) -> Option<&Path> {
        self.home.as_deref()
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
        // From the file as it is when written, not as last polled: a list
        // that keeps names this build does not know keeps the file's.
        self.write(
            |fresh| {
                changes(request, self.home(), fresh).map(|changes| {
                    changes
                        .into_iter()
                        .map(|(key, value)| (key.to_owned(), value))
                        .collect()
                })
            },
            |held| {
                if let (Some(on), Some(login)) = (request.start_at_login, &self.login)
                    && (on != held.start_at_login || on != login.is_on())
                {
                    login.set(on)?;
                }
                Ok(())
            },
        )
    }

    /// Pins `path` at the end of the repository list, or unpins it, and
    /// answers the settings it leaves; None when the list already says so.
    pub(crate) fn pin(
        &self,
        path: &str,
        pinned: bool,
    ) -> Result<Option<InstallationConfig>, SettingsError> {
        let home = self.home();
        let path = repository(path, home)?;
        // Made of the list as the file holds it under the write's lock, not
        // as last polled: a pin made at the same moment, or a hand edit the
        // poll has not seen, would otherwise be written over.
        let mut already = false;
        let config = self.write(
            |fresh| {
                // An entry is the folder however it was written, from home
                // or through a link; one that names no absolute path is no
                // folder this pin can be about, and is kept as written.
                let mut kept = toml_edit::Array::new();
                let mut listed = false;
                for entry in &fresh.repositories {
                    let entry = entry.to_string_lossy();
                    if repository(&entry, home).is_ok_and(|entry| entry == path) {
                        listed = true;
                        if !pinned {
                            continue;
                        }
                    }
                    kept.push(entry.as_ref());
                }
                if listed == pinned {
                    already = true;
                    return Ok(Vec::new());
                }
                if pinned {
                    kept.push(path.as_str());
                }
                Ok(vec![("repositories".to_owned(), Some(kept.into()))])
            },
            |_| Ok(()),
        )?;
        Ok((!already).then_some(config))
    }

    /// Keeps a person's pick as what later agents of its program start
    /// with, under its provider id (`new_agent.<id>.model` and `.effort`).
    /// Answers None when the settings already say so.
    pub(crate) fn remember(
        &self,
        picked: &crate::spec::Picked,
    ) -> Result<Option<InstallationConfig>, SettingsError> {
        let held = self.current().new_agent.of(&picked.provider);
        let mut changes: Vec<OwnedChange> = Vec::new();
        if let Some(model) = picked
            .model
            .as_ref()
            .filter(|model| Some(*model) != held.model.as_ref())
        {
            changes.push((
                format!("new_agent.{}.model", picked.provider),
                Some(model.clone().into()),
            ));
        }
        if let Some(effort) = picked
            .effort
            .as_ref()
            .filter(|effort| Some(*effort) != held.effort.as_ref())
        {
            changes.push((
                format!("new_agent.{}.effort", picked.provider),
                Some(effort.clone().into()),
            ));
        }
        if changes.is_empty() {
            return Ok(None);
        }
        self.write(|_| Ok(changes), |_| Ok(())).map(Some)
    }

    /// Puts the changes `changes_of` makes of the settings as they are now
    /// in the file (or in memory) and answers the settings they leave.
    /// `before` runs once they are known to be valid, before anything is
    /// written, and may refuse them.
    fn write(
        &self,
        changes_of: impl FnOnce(&InstallationConfig) -> Result<Vec<OwnedChange>, SettingsError>,
        before: impl FnOnce(&InstallationConfig) -> Result<(), SettingsError>,
    ) -> Result<InstallationConfig, SettingsError> {
        let mut held = self.held.lock().unwrap();
        // A change made beside the daemon since it last looked is kept, and
        // the changes are made of the file as it reads now, under the lock:
        // what the last poll saw may be up to its interval old.
        let fresh = match &self.file {
            Some(path) => {
                held.text = match std::fs::read_to_string(path) {
                    Ok(text) => text,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(error) => return Err(error.into()),
                };
                // One that no longer reads is the edit's to refuse, unless
                // the change mends it; the last good settings stand in.
                read(&held.text, path).unwrap_or_else(|_| held.config.clone())
            }
            None => held.config.clone(),
        };
        let changes = changes_of(&fresh)?;
        let changes: Vec<Change> = changes
            .iter()
            .map(|(key, value)| (key.as_str(), value.clone()))
            .collect();
        // Nothing to change writes nothing; a change made beside the daemon
        // that the file now holds is the poll's to apply.
        if changes.is_empty() {
            return Ok(held.config.clone());
        }
        let (text, edited) = settings::edit::edit(&held.text, &changes)?;
        let config = match &self.file {
            Some(path) => read(&text, path)?,
            None => InstallationConfig {
                path: None,
                ..edited
            },
        };
        before(&held.config)?;
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
    running: &settings::experiments::InEffect,
) -> wire::Settings {
    let wanted = config.experiments.in_effect();
    wire::Settings {
        host_name: host_name.to_owned(),
        start_at_login: supervised.then_some(config.start_at_login),
        keep_awake: config.keep_awake,
        repositories: config
            .repositories()
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
        experiments: settings::experiments::EXPERIMENTS
            .iter()
            .map(|experiment| wire::Experiment {
                name: experiment.name.to_owned(),
                version: experiment.version.to_owned(),
                description: experiment.description.to_owned(),
                enabled: config.experiments.is_on(experiment),
                applying: wanted.of_experiment(experiment) != running.of_experiment(experiment),
            })
            .collect(),
    }
}

/// One key's change, its key made as the change is: an agent's picks are
/// keyed by its provider id.
type OwnedChange = (String, Option<toml_edit::Value>);

/// The file's keys a request changes, from the settings it changes.
fn changes(
    request: &UpdateSettingsRequest,
    home: Option<&Path>,
    current: &InstallationConfig,
) -> Result<Vec<Change<'static>>, SettingsError> {
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
    if let Some(repositories) = &request.repositories {
        let mut array = toml_edit::Array::new();
        let mut seen = std::collections::BTreeSet::new();
        for path in &repositories.paths {
            let path = repository(path, home)?;
            if seen.insert(path.clone()) {
                array.push(path);
            }
        }
        changes.push(("repositories", Some(array.into())));
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
    if let Some(list) = &request.experiments {
        changes.push((
            "experiments.enabled",
            Some(experiments(list, current)?.into()),
        ));
    }
    Ok(changes)
}

/// The names `experiments.enabled` takes for a list a client sent: each
/// one this build implements, in the order sent, and after them the names
/// the file held that this build does not know, which no client here can
/// see to keep.
fn experiments(
    list: &wire::ExperimentList,
    current: &InstallationConfig,
) -> Result<toml_edit::Array, SettingsError> {
    let mut names = toml_edit::Array::new();
    let mut seen = std::collections::BTreeSet::new();
    for name in &list.names {
        let name = name.trim();
        if settings::experiments::named(name).is_none() {
            return Err(SettingsError::Invalid(format!(
                "this amux has no experiment named {name:?}"
            )));
        }
        if seen.insert(name.to_owned()) {
            names.push(name);
        }
    }
    for name in current.experiments.unknown_names() {
        if seen.insert(name.to_owned()) {
            names.push(name);
        }
    }
    Ok(names)
}

/// A repository as the file keeps it: absolute, and canonical where it
/// exists. A path typed from home (`~` or `~/src/app`) is taken from this
/// machine's home, as a shell would.
fn repository(path: &str, home: Option<&Path>) -> Result<String, SettingsError> {
    let path = path.trim();
    let from_home = match (path.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            Some(home.join(rest.trim_start_matches('/')))
        }
        _ => None,
    };
    let given = from_home.as_deref().unwrap_or(Path::new(path));
    if !given.is_absolute() {
        return Err(SettingsError::Invalid(format!(
            "a repository is an absolute path, not {path:?}"
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
    use settings::experiments::{Enabled, InEffect};

    use super::*;

    fn request() -> UpdateSettingsRequest {
        UpdateSettingsRequest::default()
    }

    #[test]
    fn a_repository_typed_from_home_is_taken_from_this_machines_home() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        let repository = dir.path().join("src/app");
        std::fs::create_dir_all(&repository).unwrap();
        let settings = HostSettings::new(SettingsOptions {
            file: Some(path.clone()),
            initial: InstallationConfig::from_file(&path).unwrap(),
            login: None,
            home: Some(dir.path().to_owned()),
        });
        let config = settings
            .update(&UpdateSettingsRequest {
                repositories: Some(wire::RepositoryList {
                    paths: vec!["~/src/app".into(), " ~ ".into()],
                }),
                ..request()
            })
            .unwrap();
        assert_eq!(
            config.repositories,
            [
                repository.canonicalize().unwrap(),
                dir.path().canonicalize().unwrap()
            ]
        );
        let refused = settings.update(&UpdateSettingsRequest {
            repositories: Some(wire::RepositoryList {
                paths: vec!["~someone/app".into()],
            }),
            ..request()
        });
        assert!(
            matches!(refused, Err(SettingsError::Invalid(_))),
            "{refused:?}"
        );
    }

    #[test]
    fn a_pin_adds_a_folder_at_the_end_once_and_an_unpin_takes_it_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        let (app, lib) = (dir.path().join("src/app"), dir.path().join("src/lib"));
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(&lib).unwrap();
        let settings = HostSettings::new(SettingsOptions {
            file: Some(path.clone()),
            initial: InstallationConfig::from_file(&path).unwrap(),
            login: None,
            home: Some(dir.path().to_owned()),
        });
        let (app, lib) = (app.canonicalize().unwrap(), lib.canonicalize().unwrap());
        let pinned = settings.pin("~/src/app", true).unwrap().unwrap();
        assert_eq!(pinned.repositories, std::slice::from_ref(&app));
        let pinned = settings.pin(lib.to_str().unwrap(), true).unwrap().unwrap();
        assert_eq!(pinned.repositories, [app.clone(), lib.clone()]);
        assert!(
            settings.pin("~/src/app", true).unwrap().is_none(),
            "a folder already pinned changes nothing"
        );
        let unpinned = settings.pin("~/src/app", false).unwrap().unwrap();
        assert_eq!(unpinned.repositories, [lib]);
        assert_eq!(
            InstallationConfig::from_file(&path).unwrap().repositories,
            unpinned.repositories,
            "the file says it"
        );
    }

    /// A pin is made of the list the file holds when it is written, so a
    /// hand edit the daemon has not polled yet is kept, and so are pins
    /// made at the same moment.
    #[test]
    fn a_pin_keeps_what_the_file_gained_since_it_was_last_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        let folder = |name: &str| {
            let folder = dir.path().join(name);
            std::fs::create_dir_all(&folder).unwrap();
            folder.canonicalize().unwrap()
        };
        let (edited, app) = (folder("edited"), folder("app"));
        let settings = HostSettings::new(SettingsOptions {
            file: Some(path.clone()),
            initial: InstallationConfig::from_file(&path).unwrap(),
            login: None,
            home: Some(dir.path().to_owned()),
        });
        std::fs::write(&path, format!("repositories = [{:?}]\n", edited)).unwrap();
        let pinned = settings.pin(app.to_str().unwrap(), true).unwrap().unwrap();
        assert_eq!(pinned.repositories, [edited.clone(), app.clone()]);

        let many: Vec<PathBuf> = (0..8).map(|n| folder(&format!("many/{n}"))).collect();
        std::thread::scope(|scope| {
            for folder in &many {
                let settings = &settings;
                scope.spawn(move || settings.pin(folder.to_str().unwrap(), true).unwrap());
            }
        });
        let listed = InstallationConfig::from_file(&path).unwrap().repositories;
        for folder in [&edited, &app].into_iter().chain(&many) {
            assert!(
                listed.contains(folder),
                "{folder:?} is missing from {listed:?}"
            );
        }
    }

    /// An entry names its folder however the person wrote it, from home or
    /// through a link; unpinning that folder takes the entry out, and an
    /// entry that no longer resolves stays as written.
    #[test]
    fn an_unpin_finds_the_entry_however_it_was_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let (app, lib) = (dir.path().join("src/app"), dir.path().join("src/lib"));
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(&lib).unwrap();
        let linked = dir.path().join("lib-link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&lib, &linked).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&lib, &linked).unwrap();
        let gone = dir.path().join("gone");
        std::fs::write(
            &path,
            format!(
                "repositories = [\"~/src/app\", {:?}, {:?}, \"relative/one\"]\n",
                linked, gone
            ),
        )
        .unwrap();
        let settings = HostSettings::new(SettingsOptions {
            file: Some(path.clone()),
            initial: InstallationConfig::from_file(&path).unwrap(),
            login: None,
            home: Some(dir.path().to_owned()),
        });
        let (app, lib) = (app.canonicalize().unwrap(), lib.canonicalize().unwrap());
        assert!(
            settings.pin(app.to_str().unwrap(), true).unwrap().is_none(),
            "a folder listed from home is pinned already"
        );
        assert!(
            settings.pin(lib.to_str().unwrap(), true).unwrap().is_none(),
            "a folder listed through a link is pinned already"
        );
        let unpinned = settings
            .pin(app.to_str().unwrap(), false)
            .unwrap()
            .expect("the entry written from home goes");
        assert_eq!(
            unpinned.repositories,
            [linked.clone(), gone.clone(), PathBuf::from("relative/one")]
        );
        let unpinned = settings
            .pin(lib.to_str().unwrap(), false)
            .unwrap()
            .expect("the entry written through a link goes");
        assert_eq!(
            unpinned.repositories,
            [gone.clone(), PathBuf::from("relative/one")],
            "entries that do not resolve are kept as written"
        );
        assert_eq!(
            InstallationConfig::from_file(&path).unwrap().repositories,
            unpinned.repositories,
            "the file says it"
        );
        let unpinned = settings
            .pin(gone.to_str().unwrap(), false)
            .unwrap()
            .expect("a folder that went is unpinned by its path");
        assert_eq!(unpinned.repositories, [PathBuf::from("relative/one")]);
    }

    #[test]
    fn a_change_is_written_to_the_file_key_by_key_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# mine\nchannel = \"preview\"\nkeep_awake = true\n").unwrap();
        let repository = dir.path().join("app");
        std::fs::create_dir(&repository).unwrap();
        let settings = HostSettings::new(SettingsOptions {
            file: Some(path.clone()),
            initial: InstallationConfig::from_file(&path).unwrap(),
            login: None,
            home: None,
        });
        let config = settings
            .update(&UpdateSettingsRequest {
                keep_awake: Some(false),
                repositories: Some(wire::RepositoryList {
                    paths: vec![
                        repository.to_str().unwrap().to_owned(),
                        repository.join("../app").to_str().unwrap().to_owned(),
                    ],
                }),
                claude_kind: Some(wire::Kind::ClaudePty as i32),
                host_name: Some("desk".into()),
                ..request()
            })
            .unwrap();
        assert!(!config.keep_awake);
        assert_eq!(config.repositories, [repository.canonicalize().unwrap()]);
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

        let wire = to_wire(&config, "desk", false, &InEffect::default());
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
                repositories: Some(wire::RepositoryList {
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

    /// Switching an experiment writes `experiments.enabled` and keeps the
    /// rest of the file, comments, another build's experiment and an
    /// experiment's own table among it. The daemon runs what the file said
    /// when it started, so the settings answer the switch and say it is
    /// being applied while it differs from what runs.
    #[test]
    fn switching_an_experiment_writes_the_names_and_keeps_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let text = "# mine\ntelemetry = false\n\n\
                    [experiments]\n# what I try\nenabled = [\"teleport\"]\n\n\
                    [experiments.suspension]\nafter = \"5m\" # quick\n";
        std::fs::write(&path, text).unwrap();
        let settings = HostSettings::new(SettingsOptions {
            file: Some(path.clone()),
            initial: InstallationConfig::from_file(&path).unwrap(),
            login: None,
            home: None,
        });
        let wire = to_wire(&settings.current(), "desk", false, &InEffect::default());
        let listed: Vec<(&str, &str, bool)> = wire
            .experiments
            .iter()
            .map(|experiment| {
                (
                    experiment.name.as_str(),
                    experiment.version.as_str(),
                    experiment.enabled,
                )
            })
            .collect();
        assert_eq!(
            listed,
            [
                ("workspaces", "workspaces/1", false),
                ("suspension", "suspension/1", false),
                ("reviews", "reviews/1", false)
            ]
        );
        assert!(wire.experiments.iter().all(|e| !e.description.is_empty()));

        let switch = |names: &[&str]| UpdateSettingsRequest {
            experiments: Some(wire::ExperimentList {
                names: names.iter().map(|name| (*name).to_owned()).collect(),
            }),
            ..request()
        };
        let config = settings
            .update(&switch(&["suspension", "workspaces", "suspension"]))
            .unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.starts_with("# mine\ntelemetry = false\n"),
            "{written}"
        );
        assert!(written.contains("# what I try\n"), "{written}");
        assert!(written.contains("after = \"5m\" # quick"), "{written}");
        assert!(
            written.contains("enabled = [\"suspension\", \"workspaces\", \"teleport\"]"),
            "{written}"
        );
        let read = InstallationConfig::from_file(&path).unwrap();
        assert_eq!(read.experiments, config.experiments);
        assert_eq!(
            read.experiments.enabled().versions(),
            ["workspaces/1", "suspension/1"]
        );
        let applying = |running: &[&'static settings::experiments::Experiment]| {
            to_wire(
                &config,
                "desk",
                false,
                &InEffect::of(&Enabled::of(running), &config.experiments),
            )
            .experiments
            .iter()
            .map(|experiment| (experiment.enabled, experiment.applying))
            .collect::<Vec<_>>()
        };
        // Started with neither, both are being applied; with one, the other.
        // The one left off and not running is not being applied.
        assert_eq!(applying(&[]), [(true, true), (true, true), (false, false)]);
        assert_eq!(
            applying(&[&settings::experiments::SUSPENSION]),
            [(true, true), (true, false), (false, false)]
        );
        let both = [
            &settings::experiments::WORKSPACES,
            &settings::experiments::SUSPENSION,
        ];
        assert_eq!(
            applying(&both),
            [(true, false), (true, false), (false, false)]
        );

        // Off again; the name this build does not know stays.
        let config = settings.update(&switch(&[])).unwrap();
        assert_eq!(config.experiments.enabled, ["teleport"]);
        assert!(
            to_wire(
                &config,
                "desk",
                false,
                &InEffect::of(
                    &Enabled::of(settings::experiments::EXPERIMENTS),
                    &config.experiments
                )
            )
            .experiments
            .iter()
            .all(|experiment| !experiment.enabled && experiment.applying),
            "switched off while running: being applied"
        );
        // Absent leaves the list alone.
        let config = settings
            .update(&UpdateSettingsRequest {
                telemetry: Some(true),
                ..request()
            })
            .unwrap();
        assert_eq!(config.experiments.enabled, ["teleport"]);
        let refused = settings.update(&switch(&["teleport"]));
        assert!(
            matches!(refused, Err(SettingsError::Invalid(_))),
            "{refused:?}"
        );
    }

    /// A hand edit lands between the daemon's last read and a client's
    /// switch, before the next poll would see it: the names the switch
    /// keeps are the file's as it is now, so the edit's unknown name stays
    /// and the one it removed does not come back.
    #[test]
    fn a_switch_keeps_the_unknown_names_the_file_has_now() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[experiments]\nenabled = [\"teleport\"]\n").unwrap();
        let settings = HostSettings::new(SettingsOptions {
            file: Some(path.clone()),
            initial: InstallationConfig::from_file(&path).unwrap(),
            login: None,
            home: None,
        });
        std::fs::write(&path, "[experiments]\nenabled = [\"warp\"]\n").unwrap();
        let config = settings
            .update(&UpdateSettingsRequest {
                experiments: Some(wire::ExperimentList {
                    names: vec!["suspension".to_owned()],
                }),
                ..request()
            })
            .unwrap();
        assert_eq!(config.experiments.enabled, ["suspension", "warp"]);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("enabled = [\"suspension\", \"warp\"]")
        );
    }

    /// The store keeps tables by version and knows no registry; each
    /// version it declares tables for is one this build implements, with
    /// the prefix the registry gives it.
    #[test]
    fn every_experiments_store_tables_belong_to_a_registered_version() {
        for tables in store::experiments::EXPERIMENT_TABLES {
            let experiment = settings::experiments::EXPERIMENTS
                .iter()
                .find(|experiment| experiment.version == tables.version)
                .unwrap_or_else(|| panic!("{} is in no registry entry", tables.version));
            assert_eq!(experiment.table_prefix(), tables.prefix());
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
            home: None,
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

    fn picked(kind: wire::Kind, model: Option<&str>, effort: Option<&str>) -> crate::spec::Picked {
        crate::spec::Picked {
            kind,
            provider: wire::agents::implied_by(kind).to_owned(),
            model: model.map(str::to_owned),
            effort: effort.map(str::to_owned),
        }
    }

    #[test]
    fn a_pick_is_kept_under_its_agent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# mine\ntelemetry = false\n").unwrap();
        let settings = HostSettings::new(SettingsOptions {
            file: Some(path.clone()),
            ..Default::default()
        });

        let kept = settings
            .remember(&picked(wire::Kind::ClaudeSdk, Some("sonnet"), Some("low")))
            .unwrap()
            .expect("a change");
        assert_eq!(kept.new_agent.of("claude").model.as_deref(), Some("sonnet"));
        assert_eq!(kept.new_agent.of("claude").effort.as_deref(), Some("low"));
        assert_eq!(kept.new_agent.of("codex"), Default::default());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# mine\ntelemetry = false\n"), "{text}");

        assert!(
            settings
                .remember(&picked(wire::Kind::ClaudeSdk, Some("sonnet"), None))
                .unwrap()
                .is_none(),
            "already so: the file is left alone"
        );

        // An ACP agent's are kept under its own id.
        let acp = crate::spec::Picked {
            kind: wire::Kind::Acp,
            provider: "opencode".into(),
            model: Some("anthropic/sonnet".into()),
            effort: None,
        };
        let kept = settings.remember(&acp).unwrap().expect("a change");
        assert_eq!(
            kept.new_agent.of("opencode").model.as_deref(),
            Some("anthropic/sonnet")
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("[new_agent.opencode]\nmodel = \"anthropic/sonnet\""),
            "{text}"
        );
    }
}
