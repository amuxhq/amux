//! The verbs that set an install up rather than talk to its agents:
//! `amux init` (whether amux starts at login), `amux update` and
//! `amux config channel`.

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use node::supervisor::login::LoginItem;
use node::supervisor::{Files, Request, ask};
use settings::{Channel, InstallationConfig, Switch};

use crate::connect;
use crate::server::running_supervisor;
use crate::supervise::SUPERVISOR_LOG;

/// How long `amux update` waits for the check, which may download a build.
const CHECK_PATIENCE: Duration = Duration::from_secs(20 * 60);
/// How long it then waits for the new build to answer.
const RESTART_PATIENCE: Duration = Duration::from_secs(180);

/// `amux init`: asks once whether amux starts at login and, on yes, adds
/// the login item that runs `amux supervise`.
pub async fn init(
    config: &InstallationConfig,
    config_path: Option<&Path>,
    login_item: Option<bool>,
    dry_run: bool,
) -> Result<()> {
    if config.supervisor == Switch::Off {
        println!(
            "This install has no supervisor: the service manager that runs `amux daemon` \
             starts it at boot, so there is no login item to add."
        );
        return Ok(());
    }
    let wanted = match login_item {
        Some(wanted) => wanted,
        None if std::io::stdin().is_terminal() => ask_yes("Start amux at login? [Y/n] ")?,
        None => {
            println!(
                "Not asking about a login item without a terminal; \
                 `amux init --login-item yes` adds one."
            );
            return Ok(());
        }
    };
    if !wanted {
        println!(
            "No login item: `amux server start` starts amux, and so does any amux command \
             that finds it stopped."
        );
        return Ok(());
    }
    let binary = std::env::current_exe().context("finding the amux binary")?;
    let path = std::env::var("PATH").unwrap_or_default();
    let log = config.root.join(SUPERVISOR_LOG);
    let item = LoginItem {
        binary: &binary,
        config: config_path,
        path: &path,
        log: &log,
    };
    // The service manager opens the log itself, before amux ever runs.
    std::fs::create_dir_all(&config.root)
        .with_context(|| format!("creating {}", config.root.display()))?;
    let (file, text, commands) = login_item_for_this_platform(&item, config)?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&file, text).with_context(|| format!("writing {}", file.display()))?;
    if dry_run {
        println!("Wrote {}. Would run:", file.display());
        for command in &commands {
            println!("  {}", command.join(" "));
        }
        return Ok(());
    }
    // The login item's supervisor must own the install: stop one started by
    // hand first. Agents keep running and the new daemon picks them up.
    if running_supervisor(&config.root)?.is_some() {
        crate::server::stop(config).await?;
    }
    for command in &commands {
        register(command)?;
    }
    println!(
        "Added a login item ({}): amux starts at login and comes back if it crashes; \
         `amux server stop` stops it until the next login.",
        file.display()
    );
    Ok(())
}

fn ask_yes(question: &str) -> Result<bool> {
    print!("{question}");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(!matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "n" | "no"
    ))
}

/// Runs one registration command; the first word of a failure's command
/// line is how a person would run it again.
fn register(command: &[String]) -> Result<()> {
    let status = Command::new(&command[0])
        .args(&command[1..])
        .status()
        .with_context(|| format!("running {}", command.join(" ")))?;
    // Unloading a job that was never loaded fails harmlessly.
    if !status.success() && !command.iter().any(|word| word == "bootout") {
        bail!("{} failed ({status})", command.join(" "));
    }
    Ok(())
}

type LoginFile = (PathBuf, Vec<u8>, Vec<Vec<String>>);

#[cfg(target_os = "macos")]
fn login_item_for_this_platform(
    item: &LoginItem,
    _config: &InstallationConfig,
) -> Result<LoginFile> {
    use node::supervisor::login::LAUNCH_AGENT_LABEL;
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let file = PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCH_AGENT_LABEL}.plist"));
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    let file_arg = file.display().to_string();
    Ok((
        file,
        item.launch_agent().into_bytes(),
        vec![
            words(&[
                "launchctl",
                "bootout",
                &format!("gui/{uid}/{LAUNCH_AGENT_LABEL}"),
            ]),
            words(&["launchctl", "bootstrap", &format!("gui/{uid}"), &file_arg]),
            // launchd defers the first spawn of a job kept alive on a
            // condition; start it now rather than whenever it gets to it.
            words(&[
                "launchctl",
                "kickstart",
                &format!("gui/{uid}/{LAUNCH_AGENT_LABEL}"),
            ]),
        ],
    ))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn login_item_for_this_platform(
    item: &LoginItem,
    _config: &InstallationConfig,
) -> Result<LoginFile> {
    use node::supervisor::login::SYSTEMD_UNIT;
    let config_home = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".config"),
    };
    let file = config_home.join("systemd/user").join(SYSTEMD_UNIT);
    Ok((
        file,
        item.systemd_unit().into_bytes(),
        vec![
            words(&["systemctl", "--user", "daemon-reload"]),
            words(&["systemctl", "--user", "enable", "--now", SYSTEMD_UNIT]),
        ],
    ))
}

#[cfg(windows)]
fn login_item_for_this_platform(
    item: &LoginItem,
    config: &InstallationConfig,
) -> Result<LoginFile> {
    use node::supervisor::login::WINDOWS_TASK;
    let file = config.root.join("amux-task.xml");
    // Task Scheduler reads its XML as UTF-16 with a byte-order mark.
    let mut bytes = vec![0xff, 0xfe];
    for unit in item.windows_task().encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let file_arg = file.display().to_string();
    Ok((
        file,
        bytes,
        vec![
            words(&[
                "schtasks",
                "/Create",
                "/F",
                "/TN",
                WINDOWS_TASK,
                "/XML",
                &file_arg,
            ]),
            words(&["schtasks", "/Run", "/TN", WINDOWS_TASK]),
        ],
    ))
}

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// `amux update`: asks the supervisor to check the channel now (with
/// `retry`, a build rolled back here too), and waits for a build it
/// installs to answer, or for the supervisor to roll it back.
pub async fn update(config: &InstallationConfig, retry: bool) -> Result<()> {
    if config.supervisor == Switch::Off {
        println!(
            "This install has no supervisor, so updates are deploys: install the new build \
             the way this host's amux was installed."
        );
        return Ok(());
    }
    if running_supervisor(&config.root)?.is_none() {
        bail!("amux supervise is not running; `amux server start` starts it");
    }
    // The supervisor records a rollback beside the binary it runs, which
    // is this one wherever amux was installed by its installer.
    let files = Files::beside(&std::env::current_exe().context("finding the amux binary")?);
    let recorded = rejection(&files);
    let request = if retry {
        Request::Retry
    } else {
        Request::Check
    };
    let answer = ask(&config.root, request, CHECK_PATIENCE)
        .await
        .context("asking amux supervise to check for a release")?;
    let Some(version) = answer.strip_prefix("installing ") else {
        println!("{}", sentence(&answer));
        return Ok(());
    };
    println!("Installing amux {version}.");
    let deadline = Instant::now() + RESTART_PATIENCE;
    let mut rolled_back = false;
    while Instant::now() < deadline {
        let running = match connect::front_door_now(config).await {
            Some(door) => connect::installation(door)
                .get_installation(wire::GetInstallationRequest {})
                .await
                .ok()
                .map(|info| info.into_inner().version),
            None => None,
        };
        if running.as_deref() == Some(version) {
            println!("amux {version} is running.");
            return Ok(());
        }
        rolled_back = rolled_back
            || rejection(&files)
                .is_some_and(|now| Some(&now) != recorded.as_ref() && now.0 == version);
        // The build put back is up once a daemon answers.
        if rolled_back && let Some(running) = running {
            bail!(
                "amux {version} did not start, so the supervisor put amux {running} back. \
                 Its log is {}; `amux update --retry` tries {version} again.",
                config.root.join(SUPERVISOR_LOG).display()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if rolled_back {
        bail!(
            "amux {version} did not start, so the supervisor is putting the previous build \
             back. Its log is {}.",
            config.root.join(SUPERVISOR_LOG).display()
        );
    }
    println!(
        "amux {version} has not come up yet. If it never does, the supervisor puts the \
         previous build back after three tries; its log is {}.",
        config.root.join(SUPERVISOR_LOG).display()
    );
    Ok(())
}

/// The build the supervisor last rolled back, with when it was recorded,
/// so a rollback of the same build again still reads as new.
fn rejection(files: &Files) -> Option<(String, std::time::SystemTime)> {
    let version = files.rejected()?.to_string();
    let recorded = std::fs::metadata(&files.rejected)
        .and_then(|metadata| metadata.modified())
        .ok()?;
    Some((version, recorded))
}

/// The supervisor's answer as a sentence.
fn sentence(answer: &str) -> String {
    let mut chars = answer.chars();
    match chars.next() {
        Some(first) => format!("{}{}.", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/// `amux config channel`: which releases the supervisor follows. It reads
/// the config before every check, so the change needs no restart.
pub fn set_channel(config_path: Option<&Path>, channel: Channel) -> Result<()> {
    let name = match channel {
        Channel::Stable => "stable",
        Channel::Preview => "preview",
    };
    set_key(config_path, "channel", name)?;
    println!(
        "Channel: {name}. The supervisor follows it from its next check; `amux update` checks now."
    );
    Ok(())
}

/// `amux config telemetry [on|off]`: turns product analytics on or off, or
/// says whether it is on and where it goes. The daemon reads the setting
/// before every upload, so off takes effect at once and on resumes a daemon
/// that started with it on; a daemon that started with it off has no
/// uploader and starts sending when it next starts.
pub fn telemetry(config_path: Option<&Path>, state: Option<Switch>) -> Result<()> {
    if let Some(state) = state {
        let name = match state {
            Switch::On => "on",
            Switch::Off => "off",
        };
        set_key(config_path, "telemetry", name)?;
    }
    let config = crate::connect::load_config(config_path)?;
    let destination = analytics::Endpoint::resolve(node::release::is_published())
        .map(|endpoint| endpoint.describe(settings::DEFAULT_CLOUD_URL));
    let line = match (config.telemetry, destination) {
        (Switch::Off, _) => "Telemetry: off. amux sends no product analytics.".to_owned(),
        (Switch::On, _) if analytics::do_not_track() => {
            // This command sees its own shell's environment, not the
            // daemon's: one started at login never sees a variable set in a
            // shell profile, so only the setting is sure to reach it.
            "Telemetry: on, but DO_NOT_TRACK is set in this shell, so a daemon started from \
             it sends nothing. A daemon started at login does not see it; \
             `amux config telemetry off` turns telemetry off for every daemon."
                .to_owned()
        }
        (Switch::On, None) => {
            "Telemetry: on, but this is a development build, which sends nothing.".to_owned()
        }
        (Switch::On, Some(destination)) => format!(
            "Telemetry: on. Usage events go to {destination}, then to PostHog in the EU. \
             `amux config telemetry off` stops them."
        ),
    };
    println!("{line}");
    if state == Some(Switch::On) {
        println!("A daemon that started with telemetry off starts sending when it next starts.");
    }
    Ok(())
}

/// Sets one top-level key in the installation's config file, keeping the
/// rest as it is, and checks that the file still reads.
fn set_key(config_path: Option<&Path>, key: &str, value: &str) -> Result<()> {
    let path = config_path
        .map(Path::to_owned)
        .unwrap_or_else(InstallationConfig::default_path);
    let mut yaml = match std::fs::read_to_string(&path) {
        Ok(text) => serde_yaml::from_str::<serde_yaml::Value>(&text)
            .with_context(|| format!("reading {}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_yaml::Value::Null,
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    if yaml.is_null() {
        yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    yaml.as_mapping_mut()
        .with_context(|| format!("{} is not a mapping of settings", path.display()))?
        .insert(key.into(), value.into());
    let text = serde_yaml::to_string(&yaml)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    node::write_durably(&path, text.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    InstallationConfig::from_file(&path)
        .with_context(|| format!("reading back {}", path.display()))?;
    Ok(())
}
