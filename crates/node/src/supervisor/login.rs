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
//! Login items start with the service manager's sparse environment, so each
//! carries the PATH of the process that wrote it: agents find their
//! providers by it.

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The LaunchAgent's label and file stem.
pub const LAUNCH_AGENT_LABEL: &str = "sh.amux.supervise";
/// The systemd user unit's file name.
pub const SYSTEMD_UNIT: &str = "amux.service";
/// The Windows scheduled task's name.
pub const WINDOWS_TASK: &str = "amux";

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

    /// A Task Scheduler definition: run at logon, restart on failure.
    pub fn windows_task(&self) -> String {
        let arguments = self.arguments();
        let command = xml(&arguments[0]);
        let rest = arguments[1..]
            .iter()
            .map(|argument| windows_quote(argument))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Runs amux supervise at logon and restarts it if it fails.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
    </LogonTrigger>
  </Triggers>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>999</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>{rest}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
            rest = xml(&rest),
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
    /// The directory the service manager reads login items from:
    /// `~/Library/LaunchAgents`, the systemd user units, or (on Windows,
    /// where the item is a scheduled task) the root, which keeps the
    /// definition it was made from.
    pub items: PathBuf,
    /// Whether to tell the service manager, as well as writing the file:
    /// off only in tests, which have no service manager of their own.
    pub register: bool,
}

impl StartAtLogin {
    /// The login item this process would write: itself as the binary, its
    /// PATH and the person's home.
    pub fn here(root: &Path, config: Option<&Path>) -> io::Result<Self> {
        #[cfg(unix)]
        let home = PathBuf::from(
            std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is not set"))?,
        );
        #[cfg(target_os = "macos")]
        let items = home.join("Library/LaunchAgents");
        #[cfg(all(unix, not(target_os = "macos")))]
        let items = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => home.join(".config"),
        }
        .join("systemd/user");
        #[cfg(windows)]
        let items = root.to_owned();
        Ok(Self {
            binary: std::env::current_exe()?,
            config: config.map(Path::to_owned),
            path: std::env::var("PATH").unwrap_or_default(),
            root: root.to_owned(),
            items,
            register: true,
        })
    }

    fn log(&self) -> PathBuf {
        self.root.join(crate::SUPERVISOR_LOG)
    }

    /// The file the item is, or (on Windows) the definition it was made
    /// from.
    pub fn file(&self) -> PathBuf {
        #[cfg(target_os = "macos")]
        return self.items.join(format!("{LAUNCH_AGENT_LABEL}.plist"));
        #[cfg(all(unix, not(target_os = "macos")))]
        return self.items.join(SYSTEMD_UNIT);
        #[cfg(windows)]
        return self.items.join("amux-task.xml");
    }

    /// Whether this installation's item is in place: one is, when it
    /// writes the supervisor's log into this root.
    pub fn is_on(&self) -> bool {
        let Ok(bytes) = std::fs::read(self.file()) else {
            return false;
        };
        // Task Scheduler's XML is UTF-16, and sits in the root anyway.
        cfg!(windows) || String::from_utf8_lossy(&bytes).contains(&*self.log().to_string_lossy())
    }

    /// Writes the item for the next login, or removes this installation's,
    /// leaving whatever runs now alone.
    pub fn set(&self, on: bool) -> io::Result<()> {
        let file = self.file();
        if on {
            let log = self.log();
            let item = LoginItem {
                binary: &self.binary,
                config: self.config.as_deref(),
                path: &self.path,
                log: &log,
            };
            // The service manager opens the log itself, before amux runs.
            std::fs::create_dir_all(&self.root)?;
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            crate::install::write_durably(&file, &item.file_bytes())?;
            self.run(&enable_commands(&file), true)
        } else {
            if !self.is_on() {
                return Ok(());
            }
            self.run(&disable_commands(), false)?;
            remove(&file)?;
            self.run(&after_removal_commands(), false)
        }
    }

    /// Stops the item's supervisor now and removes the item, as uninstalling
    /// does. Says whether there was one of this installation's.
    pub fn remove_now(&self) -> io::Result<bool> {
        if !self.is_on() {
            return Ok(false);
        }
        self.run(&stop_commands(), false)?;
        remove(&self.file())?;
        Ok(true)
    }

    /// Runs the service manager's commands; a failure among them matters
    /// only when `strict` (removing one that was never registered is fine).
    fn run(&self, commands: &[Vec<String>], strict: bool) -> io::Result<()> {
        if !self.register {
            return Ok(());
        }
        for command in commands {
            let status = Command::new(&command[0])
                .args(&command[1..])
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

impl LoginItem<'_> {
    /// The item's file as this platform's service manager reads it.
    fn file_bytes(&self) -> Vec<u8> {
        #[cfg(target_os = "macos")]
        return self.launch_agent().into_bytes();
        #[cfg(all(unix, not(target_os = "macos")))]
        return self.systemd_unit().into_bytes();
        #[cfg(windows)]
        {
            // Task Scheduler reads its XML as UTF-16 with a byte-order mark.
            let mut bytes = vec![0xff, 0xfe];
            for unit in self.windows_task().encode_utf16() {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            bytes
        }
    }
}

fn remove(file: &Path) -> io::Result<()> {
    match std::fs::remove_file(file) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// launchd loads every LaunchAgent at login: the file alone is the item.
#[cfg(target_os = "macos")]
fn enable_commands(_file: &Path) -> Vec<Vec<String>> {
    Vec::new()
}

#[cfg(target_os = "macos")]
fn disable_commands() -> Vec<Vec<String>> {
    Vec::new()
}

#[cfg(target_os = "macos")]
fn after_removal_commands() -> Vec<Vec<String>> {
    Vec::new()
}

#[cfg(target_os = "macos")]
fn stop_commands() -> Vec<Vec<String>> {
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    vec![words(&[
        "launchctl",
        "bootout",
        &format!("gui/{uid}/{LAUNCH_AGENT_LABEL}"),
    ])]
}

#[cfg(all(unix, not(target_os = "macos")))]
fn enable_commands(_file: &Path) -> Vec<Vec<String>> {
    vec![
        words(&["systemctl", "--user", "daemon-reload"]),
        words(&["systemctl", "--user", "enable", SYSTEMD_UNIT]),
    ]
}

#[cfg(all(unix, not(target_os = "macos")))]
fn disable_commands() -> Vec<Vec<String>> {
    vec![words(&["systemctl", "--user", "disable", SYSTEMD_UNIT])]
}

#[cfg(all(unix, not(target_os = "macos")))]
fn after_removal_commands() -> Vec<Vec<String>> {
    vec![words(&["systemctl", "--user", "daemon-reload"])]
}

#[cfg(all(unix, not(target_os = "macos")))]
fn stop_commands() -> Vec<Vec<String>> {
    vec![words(&[
        "systemctl",
        "--user",
        "disable",
        "--now",
        SYSTEMD_UNIT,
    ])]
}

#[cfg(windows)]
fn enable_commands(file: &Path) -> Vec<Vec<String>> {
    let file = file.display().to_string();
    vec![words(&[
        "schtasks",
        "/Create",
        "/F",
        "/TN",
        WINDOWS_TASK,
        "/XML",
        &file,
    ])]
}

#[cfg(windows)]
fn disable_commands() -> Vec<Vec<String>> {
    vec![words(&["schtasks", "/Delete", "/F", "/TN", WINDOWS_TASK])]
}

#[cfg(windows)]
fn after_removal_commands() -> Vec<Vec<String>> {
    Vec::new()
}

#[cfg(windows)]
fn stop_commands() -> Vec<Vec<String>> {
    vec![
        words(&["schtasks", "/End", "/TN", WINDOWS_TASK]),
        words(&["schtasks", "/Delete", "/F", "/TN", WINDOWS_TASK]),
    ]
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

/// Quotes one Windows command-line argument when it needs it.
fn windows_quote(word: &str) -> String {
    if !word.is_empty() && !word.chars().any(|c| c.is_whitespace() || c == '"') {
        return word.to_owned();
    }
    format!("\"{}\"", word.replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every login item runs `amux supervise` with the install's config,
    /// quoted for its service manager, comes back after a failure only,
    /// and never takes the agents down with the supervisor.
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

        let task = item.windows_task();
        assert!(task.contains(
            "<Arguments>--config &quot;/Users/someone/Library/Application Support/amux/config \
             &amp; more.toml&quot; supervise</Arguments>"
        ));
        assert!(task.contains("<LogonTrigger>") && task.contains("<RestartOnFailure>"));
    }

    /// Turning it on writes this installation's item; off removes it, and
    /// another installation's item is never taken for this one's.
    #[test]
    fn start_at_login_writes_and_removes_only_this_installations_item() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("data");
        let login = StartAtLogin {
            binary: PathBuf::from("/opt/amux/bin/amux"),
            config: None,
            path: "/usr/bin:/bin".into(),
            root: root.clone(),
            items: home.path().join("items"),
            register: false,
        };
        assert!(!login.is_on());
        login.set(true).unwrap();
        assert!(login.is_on(), "{}", login.file().display());
        // Unix installations share the service manager's directory; on
        // Windows each keeps its definition in its own root.
        let elsewhere = home.path().join("elsewhere");
        let other = StartAtLogin {
            root: elsewhere.clone(),
            items: if cfg!(windows) {
                elsewhere
            } else {
                login.items.clone()
            },
            ..login.clone()
        };
        assert!(!other.is_on());
        other.set(false).unwrap();
        assert!(login.is_on());
        login.set(false).unwrap();
        assert!(!login.is_on() && !login.file().exists());
        login.set(true).unwrap();
        assert!(login.remove_now().unwrap());
        assert!(!login.remove_now().unwrap());
    }
}
