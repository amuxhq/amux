//! The verbs that set an install up rather than talk to its agents:
//! `amux init` (whether amux starts at login), `amux update` and
//! `amux uninstall`.

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use node::supervisor::login::LoginItem;
use node::supervisor::{Files, Request, ask};
use settings::InstallationConfig;

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
) -> Result<()> {
    if !config.supervisor {
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

/// The login item's file and the commands that unregister it, run before
/// the file goes; a failure among them means there was nothing to remove.
#[cfg(target_os = "macos")]
fn login_item_removal(_config: &InstallationConfig) -> Result<(PathBuf, Vec<Vec<String>>)> {
    use node::supervisor::login::LAUNCH_AGENT_LABEL;
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let file = PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCH_AGENT_LABEL}.plist"));
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    Ok((
        file,
        vec![words(&[
            "launchctl",
            "bootout",
            &format!("gui/{uid}/{LAUNCH_AGENT_LABEL}"),
        ])],
    ))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn login_item_removal(_config: &InstallationConfig) -> Result<(PathBuf, Vec<Vec<String>>)> {
    use node::supervisor::login::SYSTEMD_UNIT;
    let config_home = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".config"),
    };
    Ok((
        config_home.join("systemd/user").join(SYSTEMD_UNIT),
        vec![words(&[
            "systemctl",
            "--user",
            "disable",
            "--now",
            SYSTEMD_UNIT,
        ])],
    ))
}

#[cfg(windows)]
fn login_item_removal(config: &InstallationConfig) -> Result<(PathBuf, Vec<Vec<String>>)> {
    use node::supervisor::login::WINDOWS_TASK;
    Ok((
        config.root.join("amux-task.xml"),
        vec![
            words(&["schtasks", "/End", "/TN", WINDOWS_TASK]),
            words(&["schtasks", "/Delete", "/F", "/TN", WINDOWS_TASK]),
        ],
    ))
}

/// `amux uninstall`: stops this machine's running agents, removes the
/// login item, stops amux, and deletes the installed binary; with
/// `delete_data`, the installation's root and config file too. A person at
/// a terminal is asked before agents are stopped; anyone else passes
/// `yes`. The only way amux is removed: no client deletes these itself.
pub async fn uninstall(
    config: &InstallationConfig,
    config_path: Option<&Path>,
    yes: bool,
    delete_data: bool,
) -> Result<()> {
    let running = running_agents(config).await?;
    if !running.is_empty() && !yes {
        let count = running
            .iter()
            .map(|(_, agents)| agents.len())
            .sum::<usize>();
        let question = format!(
            "Uninstalling stops {count} running agent{}. Continue? [y/N] ",
            if count == 1 { "" } else { "s" }
        );
        if !std::io::stdin().is_terminal() {
            bail!(
                "{count} agent{} still running; `amux uninstall --yes` stops them and uninstalls",
                if count == 1 { " is" } else { "s are" }
            );
        }
        if !ask_no(&question)? {
            bail!("nothing was removed");
        }
    }
    for (client, agents) in running {
        let mut client = client;
        for agent in agents {
            client
                .agents
                .stop_agent(wire::StopAgentRequest {
                    agent_id: agent.agent_id.clone(),
                })
                .await
                .map_err(crate::plain)
                .with_context(|| format!("stopping {}", agent.name))?;
            println!("Stopped {}.", agent.name);
        }
    }
    // The login item goes before amux stops, so its service manager does
    // not start amux again. It is this installation's when it writes the
    // supervisor's log into this root; another installation's stays.
    let (file, commands) = login_item_removal(config)?;
    if login_item_is_ours(&file, config) {
        for command in &commands {
            let _ = Command::new(&command[0])
                .args(&command[1..])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        if remove_file(&file)? {
            println!("Removed the login item ({}).", file.display());
        }
    }
    crate::server::stop(config).await?;
    // The data goes first: on Windows the binary running this command
    // cannot be deleted, only moved aside, and a failure there must not
    // leave the data behind.
    if delete_data {
        if config.root.exists() {
            std::fs::remove_dir_all(&config.root)
                .with_context(|| format!("deleting {}", config.root.display()))?;
            println!("Deleted {}.", config.root.display());
        }
        let config_file = config_path
            .map(Path::to_owned)
            .unwrap_or_else(InstallationConfig::default_path);
        if remove_file(&config_file)? {
            println!("Deleted {}.", config_file.display());
        }
    } else {
        println!(
            "Kept amux's data in {}; `amux uninstall --delete-data` deletes it.",
            config.root.display()
        );
    }
    // The installed binary serves the default installation; one named by
    // --config or AMUX_CONFIG (a worktree's, a test's) leaves it alone.
    if config_path.is_none() {
        let bin = install_dir()?;
        let mut removed = false;
        for name in BINARY_NAMES {
            removed |= remove_binary(&bin.join(name))?;
        }
        if removed {
            println!("Removed amux from {}.", bin.display());
        }
        remove_empty_dir(&bin);
        if let Some(parent) = bin.parent() {
            remove_empty_dir(parent);
        }
        #[cfg(unix)]
        unset_shell_path()?;
    }
    Ok(())
}

/// Removes one installed binary. Windows refuses to delete the image a
/// process is running, and this command is usually that process, so there
/// the running binary is moved aside instead (into the temporary directory,
/// else beside itself when that is on another drive) and the person is told
/// to delete the copy once this exits.
fn remove_binary(path: &Path) -> Result<bool> {
    #[cfg(windows)]
    if std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .is_ok_and(|running| std::fs::canonicalize(path).is_ok_and(|path| path == running))
    {
        let name = format!("amux-uninstalled-{}.exe", std::process::id());
        let temporary = std::env::temp_dir().join(&name);
        let aside = if std::fs::rename(path, &temporary).is_ok() {
            temporary
        } else {
            let beside = path.with_file_name(&name);
            std::fs::rename(path, &beside)
                .with_context(|| format!("moving the running {} aside", path.display()))?;
            beside
        };
        println!(
            "Moved the running amux to {}; delete it once this exits.",
            aside.display()
        );
        return Ok(true);
    }
    remove_file(path)
}

/// Whether the login item at `file` runs this installation: every login
/// item names the supervisor's log in its root.
fn login_item_is_ours(file: &Path, config: &InstallationConfig) -> bool {
    let Ok(bytes) = std::fs::read(file) else {
        return false;
    };
    // Task Scheduler's XML is UTF-16, and sits in the root anyway.
    if cfg!(windows) {
        return true;
    }
    let log = config.root.join(SUPERVISOR_LOG);
    String::from_utf8_lossy(&bytes).contains(&*log.to_string_lossy())
}

/// The binaries an install and its updates leave in the install directory.
#[cfg(unix)]
const BINARY_NAMES: [&str; 3] = ["amux", "amux.staged", "amux.prev"];
#[cfg(windows)]
const BINARY_NAMES: [&str; 3] = ["amux.exe", "amux.staged.exe", "amux.prev.exe"];

/// Where amux is installed: `~/.amux/bin`, on every platform.
fn install_dir() -> Result<PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .context("the home directory is not set")?;
    Ok(PathBuf::from(home).join(".amux").join("bin"))
}

/// This machine's live agents on each profile, with the profile's client;
/// none when amux is not running.
async fn running_agents(
    config: &InstallationConfig,
) -> Result<Vec<(wire::ClientStubs, Vec<wire::Agent>)>> {
    let Some(door) = connect::front_door_now(config).await else {
        return Ok(Vec::new());
    };
    let profiles = connect::installation(door)
        .list_profiles(wire::Empty {})
        .await
        .map_err(crate::plain)?
        .into_inner()
        .profiles;
    let mut running = Vec::new();
    for profile in profiles {
        let own = connect::admin_of(&profile)
            .await?
            .get_device_identity(wire::Empty {})
            .await
            .map_err(crate::plain)?
            .into_inner()
            .host_id;
        let mut client = connect::client_of(&profile).await?;
        let (_, agents) = crate::verbs::inventory(&mut client).await?;
        let live: Vec<wire::Agent> = agents
            .into_iter()
            .filter(|agent| {
                agent.host_id == own
                    && wire::Lifecycle::try_from(agent.lifecycle) != Ok(wire::Lifecycle::Exited)
            })
            .collect();
        if !live.is_empty() {
            running.push((client, live));
        }
    }
    Ok(running)
}

/// Removes a file, saying whether there was one.
fn remove_file(path: &Path) -> Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("deleting {}", path.display())),
    }
}

fn remove_empty_dir(path: &Path) {
    // Fails, and leaves it, when something else is in it.
    let _ = std::fs::remove_dir(path);
}

/// The two lines `https://amux.sh/install` adds to a shell profile, which
/// it adds once and only in this form.
#[cfg(unix)]
const SHELL_PATH_LINES: &str =
    "\n# Added by amux installer\nexport PATH=\"$HOME/.amux/bin:$PATH\"\n";

/// Takes the install script's PATH lines back out of the shell profiles it
/// writes to.
#[cfg(unix)]
fn unset_shell_path() -> Result<()> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Ok(());
    };
    for name in [".zshrc", ".bashrc", ".profile"] {
        let file = home.join(name);
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        if text.contains(SHELL_PATH_LINES) {
            std::fs::write(&file, text.replacen(SHELL_PATH_LINES, "\n", 1))
                .with_context(|| format!("writing {}", file.display()))?;
            println!("Took ~/.amux/bin off PATH in {}.", file.display());
        }
    }
    Ok(())
}

fn ask_no(question: &str) -> Result<bool> {
    print!("{question}");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// `amux update`: asks the supervisor to check the channel now (with
/// `retry`, a build rolled back here too), and waits for a build it
/// installs to answer, or for the supervisor to roll it back.
pub async fn update(config: &InstallationConfig, retry: bool) -> Result<()> {
    if !config.supervisor {
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
                .get_installation(wire::Empty {})
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
