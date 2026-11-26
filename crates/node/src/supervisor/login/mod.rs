//! The login item `start_at_login` writes: it runs `amux supervise` at
//! login and restarts it on failure only, never after a clean exit, so a
//! deliberate stop stays stopped and a crash comes back.
//!
//! Turning it on writes the item for the next login and leaves the running
//! supervisor alone: registering it now would start a second supervisor,
//! which finds the first one's lock and exits, and a service manager told
//! to restart failures would start it again and again. Turning it off
//! removes the item the same way, without stopping amux. Only `amux
//! uninstall` unregisters it at once, since it stops amux anyway.
//!
//! The service manager owns the supervisor and the supervisor owns the
//! daemon; neither may take the agents down. systemd's default stop kills
//! every process in the unit's cgroup and launchd's kills the job's process
//! group, agents and providers included, so the systemd unit says
//! `KillMode=process` and the LaunchAgent `AbandonProcessGroup`.
//!
//! Unix login items start with the service manager's sparse environment, so
//! each carries the PATH of the process that wrote it: agents find their
//! providers by it.
//!
//! On Windows the item is a value under the user's `Run` key, which starts
//! `amux-launch.exe` (the restart policy, as `KeepAlive` and `Restart=` are)
//! in the person's session with their own environment ([`windows`]).
//!
//! Where an item lives and how it is registered is each platform's module:
//! `macos.rs`, `linux.rs` (every other Unix too) and `windows.rs`. What an
//! item says is here, rendered for every platform so it can be read and
//! tested anywhere.

#[cfg(all(unix, not(target_os = "macos")))]
#[path = "linux.rs"]
mod manager;
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod manager;
#[cfg(windows)]
mod windows;

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use file_item as platform;
#[cfg(windows)]
use windows as platform;

/// The LaunchAgent's label and file stem.
pub const LAUNCH_AGENT_LABEL: &str = "sh.amux.supervise";
/// The systemd user unit's file name.
pub const SYSTEMD_UNIT: &str = "amux.service";
/// Names the key under `HKCU` whose values are login items, in place of
/// the `Run` key, for a test on Windows; it must be under
/// `Software\amux-test-`. Ignored elsewhere, where a test sets `HOME`.
pub const TEST_RUN_KEY: &str = "AMUX_TEST_RUN_KEY";
/// The prefix every [`TEST_RUN_KEY`] starts with.
const TEST_KEY_PREFIX: &str = r"Software\amux-test-";

/// What every login item needs to know.
pub struct LoginItem<'a> {
    /// The canonical install path.
    pub binary: &'a Path,
    /// The installation config, when not the default one.
    pub config: Option<&'a Path>,
    /// The PATH agents will see.
    pub path: &'a str,
    /// Where the service manager writes the supervisor's own output.
    pub log: &'a Path,
}

impl LoginItem<'_> {
    fn arguments(&self) -> Vec<String> {
        let mut arguments = vec![self.binary.display().to_string()];
        if let Some(config) = self.config {
            arguments.push("--config".into());
            arguments.push(config.display().to_string());
        }
        arguments.push("supervise".into());
        arguments
    }

    /// `~/Library/LaunchAgents/sh.amux.supervise.plist`: kept alive only
    /// after an unsuccessful exit.
    pub fn launch_agent(&self) -> String {
        let mut arguments = String::new();
        for argument in self.arguments() {
            let _ = writeln!(arguments, "\t\t<string>{}</string>", xml(&argument));
        }
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LAUNCH_AGENT_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
{arguments}	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PATH</key>
		<string>{path}</string>
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>AbandonProcessGroup</key>
	<true/>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#,
            path = xml(self.path),
            log = xml(&self.log.display().to_string()),
        )
    }

    /// `~/.config/systemd/user/amux.service`: restarted on failure only,
    /// and stopping it stops the supervisor alone.
    pub fn systemd_unit(&self) -> String {
        let exec = self
            .arguments()
            .iter()
            .map(|argument| systemd_quote(argument))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "[Unit]
Description=amux supervisor: runs the amux daemon and restarts it

[Service]
ExecStart={exec}
Environment={path}
Restart=on-failure
KillMode=process
StandardOutput=append:{log}
StandardError=append:{log}

[Install]
WantedBy=default.target
",
            path = systemd_quote(&format!("PATH={}", self.path)),
            log = self.log.display(),
        )
    }
}

/// This installation's login item: where it lives and what it runs.
#[derive(Clone, Debug)]
pub struct StartAtLogin {
    /// The canonical install path the item runs.
    pub binary: PathBuf,
    /// The installation config, when not the default one.
    pub config: Option<PathBuf>,
    /// The PATH agents will see.
    pub path: String,
    /// The installation's root, where the supervisor's log is.
    pub root: PathBuf,
    /// Where the service manager reads login items from:
    /// `~/Library/LaunchAgents`, the systemd user units, or on Windows the
    /// key under `HKCU` whose values are items (the `Run` key).
    pub items: PathBuf,
    /// Whether to tell the service manager, as well as writing the file:
    /// off only in tests, which have no service manager of their own. On
    /// Windows the value is the registration, and `items` keeps a test's
    /// apart.
    pub register: bool,
}

impl StartAtLogin {
    /// The login item this process would write: itself as the binary, its
    /// PATH and the person's home.
    pub fn here(root: &Path, config: Option<&Path>) -> io::Result<Self> {
        Ok(Self {
            binary: std::env::current_exe()?,
            config: config.map(Path::to_owned),
            path: std::env::var("PATH").unwrap_or_default(),
            root: root.to_owned(),
            items: platform::items()?,
            register: true,
        })
    }

    /// Where the item is, for a person to read: its file, or on Windows
    /// the `Run` value.
    pub fn location(&self) -> String {
        platform::location(self)
    }

    /// What this installation's item says, when there is one.
    pub fn contents(&self) -> Option<String> {
        platform::contents(self)
    }

    /// Whether this installation's item is in place.
    pub fn is_on(&self) -> bool {
        platform::is_on(self)
    }

    /// Writes the item for the next login, or removes this installation's,
    /// leaving whatever runs now alone.
    pub fn set(&self, on: bool) -> io::Result<()> {
        if on {
            platform::write(self)
        } else if self.is_on() {
            platform::remove(self)
        } else {
            Ok(())
        }
    }

    /// Stops the item's supervisor now and removes the item, as uninstalling
    /// does. Says whether there was one of this installation's.
    pub fn remove_now(&self) -> io::Result<bool> {
        if !self.is_on() {
            return Ok(false);
        }
        platform::remove_now(self)?;
        Ok(true)
    }
}

/// Login items no service manager reads, for a test, removed with all
/// they hold when dropped: the platform's place under `home` on Unix,
/// whose `HOME` the test sets, and a key of their own under
/// `HKCU\Software\amux-test-…` on Windows, which [`TEST_RUN_KEY`] names.
#[doc(hidden)]
pub struct ScratchItems {
    items: PathBuf,
}

impl ScratchItems {
    pub fn new(home: &Path) -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let key = format!("{TEST_KEY_PREFIX}{}-{nanos}-{next}", std::process::id());
        Self {
            items: platform::scratch_items(home, &key),
        }
    }

    /// Where a [`StartAtLogin`] for the test keeps its items.
    pub fn items(&self) -> &Path {
        &self.items
    }

    /// The variable that points an amux the test starts here.
    pub fn env(&self) -> (&'static str, &Path) {
        (TEST_RUN_KEY, &self.items)
    }
}

impl Drop for ScratchItems {
    fn drop(&mut self) {
        platform::forget_scratch(&self.items);
    }
}

/// The login item as a file the service manager reads: a LaunchAgent or
/// a systemd unit. An item is this installation's when it writes the
/// supervisor's log into this root.
#[cfg(unix)]
mod file_item {
    use std::ffi::OsString;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::{LoginItem, StartAtLogin, manager};

    pub(super) fn items() -> io::Result<PathBuf> {
        let home = PathBuf::from(
            std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is not set"))?,
        );
        Ok(manager::items(&home, std::env::var_os("XDG_CONFIG_HOME")))
    }

    pub(super) fn scratch_items(home: &Path, _key: &str) -> PathBuf {
        manager::items(home, None::<OsString>)
    }

    /// The test's home goes with its temporary directory.
    pub(super) fn forget_scratch(_items: &Path) {}

    fn file(login: &StartAtLogin) -> PathBuf {
        login.items.join(manager::FILE)
    }

    fn log(login: &StartAtLogin) -> PathBuf {
        login.root.join(crate::SUPERVISOR_LOG)
    }

    pub(super) fn location(login: &StartAtLogin) -> String {
        file(login).display().to_string()
    }

    pub(super) fn contents(login: &StartAtLogin) -> Option<String> {
        let text = String::from_utf8(std::fs::read(file(login)).ok()?).ok()?;
        text.contains(&*log(login).to_string_lossy())
            .then_some(text)
    }

    pub(super) fn is_on(login: &StartAtLogin) -> bool {
        contents(login).is_some()
    }

    pub(super) fn write(login: &StartAtLogin) -> io::Result<()> {
        let file = file(login);
        let log = log(login);
        let item = LoginItem {
            binary: &login.binary,
            config: login.config.as_deref(),
            path: &login.path,
            log: &log,
        };
        // The service manager opens the log itself, before amux runs.
        std::fs::create_dir_all(&login.root)?;
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::install::write_durably(&file, manager::contents(&item).as_bytes())?;
        run(login, &manager::enable_commands(), true)
    }

    pub(super) fn remove(login: &StartAtLogin) -> io::Result<()> {
        run(login, &manager::disable_commands(), false)?;
        remove_file(&file(login))?;
        run(login, &manager::after_removal_commands(), false)
    }

    pub(super) fn remove_now(login: &StartAtLogin) -> io::Result<()> {
        run(login, &manager::stop_commands(), false)?;
        remove_file(&file(login))
    }

    fn remove_file(file: &Path) -> io::Result<()> {
        match std::fs::remove_file(file) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Runs the service manager's commands; a failure among them matters
    /// only when `strict` (removing one that was never registered is fine).
    fn run(login: &StartAtLogin, commands: &[Vec<String>], strict: bool) -> io::Result<()> {
        if !login.register {
            return Ok(());
        }
        for command in commands {
            let status = Command::new(&command[0])
                .args(&command[1..])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()?;
            if strict && !status.success() {
                return Err(io::Error::other(format!(
                    "{} failed ({status})",
                    command.join(" ")
                )));
            }
        }
        Ok(())
    }
}

/// One command's words, owned.
#[cfg(unix)]
fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Quotes one systemd command-line word when it needs it.
fn systemd_quote(word: &str) -> String {
    if !word.is_empty()
        && !word
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '$' | '%' | ';'))
    {
        return word.to_owned();
    }
    let escaped = word
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every Unix login item runs `amux supervise` with the install's
    /// config, quoted for its service manager, comes back after a failure
    /// only, and never takes the agents down with the supervisor.
    #[test]
    fn a_login_item_restarts_on_failure_only_and_leaves_the_agents_alone() {
        let item = LoginItem {
            binary: Path::new("/Users/someone/.amux/bin/amux"),
            config: Some(Path::new(
                "/Users/someone/Library/Application Support/amux/config & more.toml",
            )),
            path: "/opt/homebrew/bin:/usr/bin:/bin",
            log: Path::new("/Users/someone/.amux/supervisor.log"),
        };

        let launch_agent = item.launch_agent();
        assert!(launch_agent.contains(
            "\t\t<string>--config</string>\n\t\t<string>/Users/someone/Library/Application \
             Support/amux/config &amp; more.toml</string>\n\t\t<string>supervise</string>\n"
        ));
        assert!(launch_agent.contains("<key>AbandonProcessGroup</key>\n\t<true/>"));
        assert!(launch_agent.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));

        let unit = item.systemd_unit();
        assert!(unit.contains(
            "\nExecStart=/Users/someone/.amux/bin/amux --config \"/Users/someone/Library/Application \
             Support/amux/config & more.toml\" supervise\n"
        ));
        assert!(unit.contains("\nKillMode=process\n"));
        assert!(unit.contains("\nRestart=on-failure\n"));
        assert!(!unit.contains("Restart=always"));
    }

    /// Turning it on writes this installation's item, which runs
    /// `supervise` with its config; off removes it, and another
    /// installation's item is never taken for this one's.
    #[test]
    fn start_at_login_writes_and_removes_only_this_installations_item() {
        let home = tempfile::tempdir().unwrap();
        let scratch = ScratchItems::new(home.path());
        let config = home.path().join("config.toml");
        let login = StartAtLogin {
            binary: home.path().join("bin").join("amux"),
            config: Some(config.clone()),
            path: "/usr/bin:/bin".into(),
            root: home.path().join("data"),
            items: scratch.items().to_owned(),
            register: false,
        };
        assert!(!login.is_on() && login.contents().is_none());
        login.set(true).unwrap();
        assert!(login.is_on(), "{}", login.location());
        let contents = login.contents().unwrap();
        assert!(
            contents.contains("supervise") && contents.contains(&*config.to_string_lossy()),
            "{contents}"
        );
        // Another installation's root, with the same place for items.
        let other = StartAtLogin {
            root: home.path().join("elsewhere"),
            ..login.clone()
        };
        assert!(!other.is_on());
        other.set(false).unwrap();
        assert!(login.is_on());
        login.set(false).unwrap();
        assert!(!login.is_on() && login.contents().is_none());
        login.set(true).unwrap();
        assert!(login.remove_now().unwrap());
        assert!(!login.remove_now().unwrap());
        assert!(!login.is_on());
    }
}
