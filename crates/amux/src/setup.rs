//! The verbs that set an install up rather than talk to its agents:
//! `amux update` and `amux uninstall`, and the login item `start_at_login`
//! keeps.

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use node::supervisor::login::StartAtLogin;
use node::supervisor::{Files, Request, ask};
use settings::InstallationConfig;

use crate::connect;
use crate::server::running_supervisor;
use crate::supervise::SUPERVISOR_LOG;

/// How long `amux update` waits for the check, which may download a build.
const CHECK_PATIENCE: Duration = Duration::from_secs(20 * 60);
/// How long it then waits for the new build to answer.
const RESTART_PATIENCE: Duration = Duration::from_secs(180);

/// Writes or removes this installation's login item to match its
/// `start_at_login`, for the next login: the running amux is left alone.
pub fn start_at_login(config: &InstallationConfig, config_path: Option<&Path>) -> Result<()> {
    if !config.supervisor {
        if config.start_at_login {
            println!(
                "This install has no supervisor: the service manager that runs `amux daemon` \
                 starts it, so there is no login item to add."
            );
        }
        return Ok(());
    }
    let login = StartAtLogin::here(&config.root, own_config(config_path))?;
    login
        .set(config.start_at_login)
        .with_context(|| format!("changing the login item ({})", login.location()))?;
    if config.start_at_login {
        println!(
            "amux starts at login ({}); `amux config set start_at_login false` stops that.",
            login.location()
        );
    }
    Ok(())
}

/// The config a login item names: none for the default file, which every
/// amux finds by itself.
fn own_config(config_path: Option<&Path>) -> Option<&Path> {
    config_path.filter(|path| *path != InstallationConfig::default_path())
}

/// `amux uninstall`: stops this machine's running agents, removes the
/// login item (and an earlier build's), stops amux and with it Windows'
/// launcher, deletes the installed programs and what updates left beside
/// them, and takes their folder off PATH; with
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
        for agent in agents {
            client
                .agent(&agent)
                .set_filing(wire::Filing::Closed)
                .await
                .map_err(crate::refused)
                .with_context(|| format!("stopping {}", agent.name))?;
            println!("Stopped {}.", agent.name);
        }
    }
    // The login item goes before amux stops, so its service manager does
    // not start amux again. It is this installation's when it writes the
    // supervisor's log into this root; another installation's stays.
    let login = StartAtLogin::here(&config.root, own_config(config_path))?;
    if login
        .remove_now()
        .with_context(|| format!("removing the login item ({})", login.location()))?
    {
        println!("Removed the login item ({}).", login.location());
    }
    if login
        .remove_earlier()
        .context("removing the login item an earlier amux registered")?
    {
        println!("Removed the scheduled task an earlier amux registered.");
    }
    // Stopping the supervisor on purpose ends the launcher that started
    // it (Windows' login item) too.
    crate::server::stop(config).await?;
    if let Some(hosts) = crate::browser_host::installed(own_config(config_path)) {
        for file in hosts
            .remove()
            .context("removing the browser extension's host")?
        {
            println!("Removed {}.", file.display());
        }
    }
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
        let held = remove_installed(&bin)?;
        for path in &held {
            println!(
                "Left {}, which a running process holds; delete it once that exits.",
                path.display()
            );
        }
        remove_empty_dir(&bin);
        if let Some(parent) = bin.parent() {
            remove_empty_dir(parent);
        }
        match os::paths::remove_from_user_path(&bin) {
            Ok(true) => println!("Took {} off your PATH.", bin.display()),
            Ok(false) => {}
            // Unix: the install script's lines in the shell profiles, below.
            Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {}
            Err(error) => {
                return Err(error).with_context(|| format!("taking {} off PATH", bin.display()));
            }
        }
        unset_shell_path()?;
    }
    Ok(())
}

/// How long the removals wait, all told, for programs that are ending (the
/// launcher once its supervisor has stopped, an agent just stopped) before
/// setting aside what they still hold.
const ENDING_PATIENCE: Duration = Duration::from_secs(10);

/// Removes amux from `bin` with everything its updates left there, as
/// [`Files::beside`] names it: the binary and the launcher Windows' login
/// item runs, the previous build, the record of one rolled back, the
/// downloads not yet installed, finished or not, and each build moved
/// aside while it ran (`.old.<n>`). Windows refuses to delete an image a process is running,
/// and this command is usually one, so such a file is moved aside to a
/// fresh `<program>.old.<n>` instead, as an update would, and returned: the
/// one thing an uninstall leaves.
fn remove_installed(bin: &Path) -> Result<Vec<PathBuf>> {
    let files = Files::beside(&bin.join(os::paths::exe_name("amux")));
    let asides = files.asides();
    let mut paths = vec![
        bin.join(os::paths::exe_name("amux")),
        files.prev.clone(),
        files.rejected.clone(),
        files.staged.clone(),
        files.launcher.clone(),
        files.launcher_staged.clone(),
        Files::part(&files.staged),
        Files::part(&files.launcher_staged),
    ];
    paths.extend(asides.iter().cloned());

    let running = std::env::current_exe().and_then(std::fs::canonicalize).ok();
    let deadline = Instant::now() + ENDING_PATIENCE;
    let mut removed = false;
    let mut held = Vec::new();
    for path in paths {
        if std::fs::symlink_metadata(&path).is_err() {
            continue;
        }
        let ours = running
            .as_ref()
            .is_some_and(|running| std::fs::canonicalize(&path).is_ok_and(|p| p == *running));
        let patience = if ours {
            Duration::ZERO
        } else {
            deadline.saturating_duration_since(Instant::now())
        };
        match os::fs::remove_patiently(&path, patience)
            .with_context(|| format!("deleting {}", path.display()))?
        {
            os::fs::Removed::Gone => removed = true,
            // One moved aside already keeps its name.
            os::fs::Removed::Later if asides.contains(&path) => held.push(path),
            os::fs::Removed::Later => held.push(
                files
                    .set_aside(&path)
                    .with_context(|| format!("moving the running {} aside", path.display()))?,
            ),
        }
    }
    if removed || !held.is_empty() {
        println!("Removed amux from {}.", bin.display());
    }
    Ok(held)
}

/// Where amux is installed: `~/.amux/bin`, on every platform.
pub(crate) fn install_dir() -> Result<PathBuf> {
    let home = os::paths::home_dir().context("the home directory is not set")?;
    Ok(home.join(".amux").join("bin"))
}

/// This machine's live agents on each profile, with the profile's client;
/// none when amux is not running.
async fn running_agents(
    config: &InstallationConfig,
) -> Result<Vec<(client::Client, Vec<wire::Agent>)>> {
    let Some(door) = connect::front_door_now(config).await else {
        return Ok(Vec::new());
    };
    let profiles = connect::installation(door)
        .list_profiles(wire::ListProfilesRequest {})
        .await
        .map_err(crate::plain)?
        .into_inner()
        .profiles;
    let mut running = Vec::new();
    for profile in profiles {
        let own = connect::admin_of(&profile)
            .await?
            .get_device_identity(wire::GetDeviceIdentityRequest {})
            .await
            .map_err(crate::plain)?
            .into_inner()
            .identity;
        let own = crate::held(own)?.host_id;
        let client = connect::client_of(&profile).await?;
        let (_, agents) = crate::verbs::inventory(&client).await?;
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
const SHELL_PATH_LINES: &str =
    "\n# Added by amux installer\nexport PATH=\"$HOME/.amux/bin:$PATH\"\n";

/// Takes the Unix install script's PATH lines back out of the shell
/// profiles it writes to. Windows' script writes none, so there are none
/// to find there.
fn unset_shell_path() -> Result<()> {
    let Some(home) = os::paths::home_dir() else {
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

/// `amux update`: asks the supervisor to check the channel now (with
/// `retry`, a build rolled back here too), and waits for a build it
/// installs to answer, or for the supervisor to roll it back.
pub async fn update(config: &InstallationConfig, retry: bool) -> Result<()> {
    if !config.supervisor {
        println!(
            "This install has no supervisor, so updates are deploys: install the new build \
             the way this device's amux was installed."
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
                .and_then(|info| info.into_inner().installation)
                .map(|installation| installation.version),
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
