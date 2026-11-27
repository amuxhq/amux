//! Finding amux on this machine, installing it when it is missing, and
//! starting it: the first thing every client does.
//!
//! A client never owns amux. Whichever client a person installs first
//! installs it; from then on amux runs on its own, updates itself, and
//! every later client finds it already answering. So [`ensure`] is
//! idempotent and touches nothing that is already there:
//!
//! 1. If the front door (`<root>/amux.sock`) answers, it returns.
//! 2. If amux is installed (`~/.amux/bin/amux`) but stopped, it starts it.
//! 3. Otherwise it downloads the channel's build, checks it against the
//!    signed manifest, places it under a lock so two clients installing at
//!    once cannot interleave, and starts it. On Windows the launcher the
//!    login item runs (`amux-launch.exe`) comes with it, from the same
//!    release, as the install script puts it beside `amux.exe`.
//!
//! amux always comes from the channel's feed, never from a copy a client
//! carries: a client can be older than the channel, and the first amux on a
//! machine should be the one the supervisor would move it to anyway.
//!
//! Then it waits for the front door. Nothing here writes amux's config or
//! a login item: whether amux starts at login is the person's choice, made
//! through the daemon's settings once it answers, and `amux uninstall` is
//! the only way amux is removed.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use settings::InstallationConfig;
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;

pub mod update;

/// How long [`ensure`] waits for a started amux to answer.
pub const START_PATIENCE: Duration = Duration::from_secs(30);
/// The longest `amux server start` may take before [`ensure`] gives up on
/// it: it waits for the front door itself, for half this.
pub const RUN_PATIENCE: Duration = Duration::from_secs(60);
/// The most a manifest may be.
const MANIFEST_LIMIT: u64 = 1 << 20;
/// The most a build may be, whatever its manifest says.
const ARTIFACT_LIMIT: u64 = 512 << 20;
const POLL: Duration = Duration::from_millis(100);

/// What a client asks of [`ensure`].
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The installation's config file; `None` is the default one
    /// (`~/.config/amux/config.toml`), which is what every client should
    /// use outside tests.
    pub config: Option<PathBuf>,
    /// The home folder amux is installed under (`<home>/.amux/bin`), and
    /// the one the amux it starts is given; `None` is the user's
    /// ([`os::paths::home_dir`]), which is what every client should use
    /// outside tests.
    pub home: Option<PathBuf>,
}

impl Options {
    /// Where amux is installed: `<home>/.amux/bin`.
    pub fn install_dir(&self) -> Result<PathBuf, Error> {
        let home = match &self.home {
            Some(home) => home.clone(),
            None => os::paths::home_dir()
                .filter(|home| !home.as_os_str().is_empty())
                .ok_or(Error::NoHome)?,
        };
        Ok(home.join(".amux").join("bin"))
    }

    /// The installed binary, `<home>/.amux/bin/amux` (`amux.exe` on
    /// Windows).
    pub fn installed_binary(&self) -> Result<PathBuf, Error> {
        Ok(self.install_dir()?.join(os::paths::exe_name("amux")))
    }
}

/// What [`ensure`] found or did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// amux was already answering; nothing was touched.
    Running,
    /// amux was installed but stopped, and is now started.
    Started,
    /// amux was downloaded, set up and started.
    Installed { version: String },
}

/// What [`ensure`] is doing, for a client that shows progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Bytes of the download received so far, of its signed size.
    Downloading { received: u64, total: u64 },
    /// Starting amux and waiting for it to answer.
    Starting,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("reading the amux config: {0}")]
    Config(#[from] settings::ConfigError),
    #[error("the home directory is not set")]
    NoHome,
    #[error("{what} {}: {error}", path.display())]
    Io {
        what: &'static str,
        path: PathBuf,
        error: std::io::Error,
    },
    #[error("reading {url}: {error}")]
    Fetch { url: String, error: reqwest::Error },
    #[error("{url} is not a release manifest: {error}")]
    Manifest {
        url: String,
        error: serde_json::Error,
    },
    #[error("{url} is larger than {limit} bytes")]
    TooLarge { url: String, limit: u64 },
    #[error(transparent)]
    Verify(#[from] release::VerifyError),
    #[error("the {channel} channel has no build for {target}")]
    NoBuild { channel: String, target: String },
    #[error("this build trusts no release key, so it cannot check a download")]
    NoKeys,
    #[error("`{command}` failed ({status}): {output}")]
    Command {
        command: String,
        status: String,
        output: String,
    },
    #[error("`{command}` did not finish within {}s", RUN_PATIENCE.as_secs())]
    TimedOut { command: String },
    #[error("amux did not answer on {} within {}s", path.display(), START_PATIENCE.as_secs())]
    NotAnswering { path: PathBuf },
}

fn io(what: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> Error {
    let path = path.to_owned();
    move |error| Error::Io { what, path, error }
}

/// The installation's config: the named file, else the default file when
/// it exists, else the defaults. Its root holds the front door.
pub fn config(path: Option<&Path>) -> Result<InstallationConfig, Error> {
    match path {
        Some(path) => Ok(InstallationConfig::from_file(path)?),
        None => {
            let default = InstallationConfig::default_path();
            if default.exists() {
                Ok(InstallationConfig::from_file(&default)?)
            } else {
                Ok(InstallationConfig::default())
            }
        }
    }
}

/// Where the installation's front door is: what a client connects to once
/// [`ensure`] returns.
pub fn front_door(path: Option<&Path>) -> Result<PathBuf, Error> {
    Ok(config(path)?.front_door())
}

/// Where amux is installed: `~/.amux/bin`, on every platform.
pub fn install_dir() -> Result<PathBuf, Error> {
    Options::default().install_dir()
}

/// The installed binary, `~/.amux/bin/amux` (`amux.exe` on Windows).
pub fn installed_binary() -> Result<PathBuf, Error> {
    Options::default().installed_binary()
}

/// Whether amux answers on `front_door` now.
pub async fn answering(front_door: &Path) -> bool {
    agent_dir::local_socket::connect(front_door).await.is_ok()
}

/// Makes sure amux is installed and answering, installing and starting it
/// as needed (see the crate's documentation), and says what it did.
/// `on_step` hears each step as it starts.
pub async fn ensure(options: &Options, mut on_step: impl FnMut(Step)) -> Result<Outcome, Error> {
    let config = config(options.config.as_deref())?;
    let door = config.front_door();
    if answering(&door).await {
        return Ok(Outcome::Running);
    }
    let binary = options.installed_binary()?;
    let installed = match binary.exists() {
        true => None,
        false => install(&config, &binary, &mut on_step).await?,
    };
    on_step(Step::Starting);
    let with_config = |words: &[&str]| -> Vec<OsString> {
        let mut args = Vec::new();
        if let Some(path) = &options.config {
            args.push("--config".into());
            args.push(path.clone().into_os_string());
        }
        args.extend(words.iter().map(OsString::from));
        args
    };
    // Starts the supervisor (or, without one, the daemon) unless the login
    // item already has; a running one is left alone.
    if !answering(&door).await {
        run(
            &binary,
            &with_config(&["server", "start"]),
            options.home.as_deref(),
        )
        .await?;
    }
    wait_for(&door).await?;
    Ok(match installed {
        Some(version) => Outcome::Installed { version },
        None => Outcome::Started,
    })
}

/// Waits for the front door to answer.
pub async fn wait_for(front_door: &Path) -> Result<(), Error> {
    let deadline = Instant::now() + START_PATIENCE;
    while !answering(front_door).await {
        if Instant::now() >= deadline {
            return Err(Error::NotAnswering {
                path: front_door.to_owned(),
            });
        }
        tokio::time::sleep(POLL).await;
    }
    Ok(())
}

/// Places a verified download at `binary`, under the
/// install directory's lock. `None` when another client installed it
/// while this one waited for the lock.
async fn install(
    config: &InstallationConfig,
    binary: &Path,
    on_step: &mut impl FnMut(Step),
) -> Result<Option<String>, Error> {
    let dir = binary
        .parent()
        .expect("the binary is in the install directory");
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(io("creating", dir))?;
    let lock_path = dir.join(".install.lock");
    let lock = std::fs::File::create(&lock_path).map_err(io("opening", &lock_path))?;
    let lock = tokio::task::spawn_blocking(move || lock.lock().map(|()| lock))
        .await
        .expect("the lock task does not panic")
        .map_err(io("locking", &lock_path))?;
    if binary.exists() {
        return Ok(None);
    }
    let staged = dir.join(format!(".amux.installing.{}", std::process::id()));
    let launcher = dir.join(os::paths::exe_name(release::LAUNCHER));
    let staged_launcher = dir.join(format!(
        ".{}.installing.{}",
        release::LAUNCHER,
        std::process::id()
    ));
    let placed = async {
        let downloaded = download(config, &staged, &staged_launcher, on_step).await?;
        make_executable(&staged)?;
        // The launcher first: amux in its place is what says an install
        // is done.
        if downloaded.launcher {
            make_executable(&staged_launcher)?;
            tokio::fs::rename(&staged_launcher, &launcher)
                .await
                .map_err(io("placing", &launcher))?;
        }
        tokio::fs::rename(&staged, binary)
            .await
            .map_err(io("placing", binary))?;
        Ok(downloaded.version)
    }
    .await;
    if placed.is_err() {
        let _ = std::fs::remove_file(&staged);
        let _ = std::fs::remove_file(&staged_launcher);
    }
    drop(lock);
    placed.map(Some)
}

/// What [`download`] fetched and checked.
struct Downloaded {
    /// The build's version.
    version: String,
    /// Whether the launcher came too.
    launcher: bool,
}

/// Downloads the channel's build for this target to `staged`, believing
/// nothing in the manifest until its signature verifies against a key this
/// build trusts, and the build only once its size and hash are the
/// manifest's. The launcher Windows' login item runs comes too, to
/// `staged_launcher` and checked the same way, when the manifest names one
/// from the same release beside the target's build
/// ([`release::launcher`]): without it the login item would start
/// `amux.exe` itself, in a window and with no restart. No other target's
/// manifest names one, and one that predates the launcher names none
/// either: amux alone comes.
async fn download(
    config: &InstallationConfig,
    staged: &Path,
    staged_launcher: &Path,
    on_step: &mut impl FnMut(Step),
) -> Result<Downloaded, Error> {
    let url = config.manifest_url();
    let client = http(&url)?;
    let manifest = manifest(&client, &url, config.channel_name()).await?;
    let build = manifest
        .targets
        .get(release::TARGET)
        .ok_or_else(|| Error::NoBuild {
            channel: config.channel_name().to_owned(),
            target: release::TARGET.to_owned(),
        })?;
    let launcher = release::launcher(&manifest, release::TARGET);
    // One download to the person, of both programs' signed sizes.
    let total = build.size + launcher.map_or(0, |launcher| launcher.size);
    artifact(&client, build, staged, |received| {
        on_step(Step::Downloading { received, total })
    })
    .await?;
    if let Some(launcher) = launcher {
        artifact(&client, launcher, staged_launcher, |received| {
            on_step(Step::Downloading {
                received: build.size + received,
                total,
            })
        })
        .await?;
    }
    Ok(Downloaded {
        version: build.version.clone(),
        launcher: launcher.is_some(),
    })
}

/// A client for one feed and the artifacts it names.
pub(crate) fn http(url: &str) -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(15 * 60))
        .build()
        .map_err(fetch(url))
}

/// The channel's manifest at `url`, believed only once its signature
/// verifies against a key this build trusts for that feed.
pub(crate) async fn manifest(
    client: &reqwest::Client,
    url: &str,
    channel: &str,
) -> Result<release::Manifest, Error> {
    let keys = release::feed_keys(url).ok_or(Error::NoKeys)?;
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(fetch(url))?;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(fetch(url))? {
        if body.len() as u64 + chunk.len() as u64 > MANIFEST_LIMIT {
            return Err(Error::TooLarge {
                url: url.to_owned(),
                limit: MANIFEST_LIMIT,
            });
        }
        body.extend_from_slice(&chunk);
    }
    let manifest: release::Manifest =
        serde_json::from_slice(&body).map_err(|error| Error::Manifest {
            url: url.to_owned(),
            error,
        })?;
    release::verify_manifest(&manifest, channel, &keys)?;
    Ok(manifest)
}

/// Downloads `build` to `staged`, writing no more than its signed size and
/// keeping it only once its size and hash are the manifest's.
pub(crate) async fn artifact(
    client: &reqwest::Client,
    build: &release::Release,
    staged: &Path,
    mut on_received: impl FnMut(u64),
) -> Result<(), Error> {
    if build.size > ARTIFACT_LIMIT {
        return Err(Error::TooLarge {
            url: build.url.clone(),
            limit: ARTIFACT_LIMIT,
        });
    }
    let fetched = async {
        let mut response = client
            .get(&build.url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(fetch(&build.url))?;
        let mut file = tokio::fs::File::create(staged)
            .await
            .map_err(io("creating", staged))?;
        let mut hash = Sha256::new();
        let mut received = 0u64;
        while let Some(chunk) = response.chunk().await.map_err(fetch(&build.url))? {
            received += chunk.len() as u64;
            // The signed size is the most that is written: a server sending
            // more is not sending the build.
            if received > build.size {
                return Err(release::VerifyError::Size {
                    expected: build.size,
                    actual: received,
                }
                .into());
            }
            hash.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(io("writing", staged))?;
            on_received(received);
        }
        file.sync_all().await.map_err(io("writing", staged))?;
        release::verify_artifact(build, &release::sha256_hex(&hash.finalize()), received)?;
        Ok(())
    }
    .await;
    if fetched.is_err() {
        let _ = std::fs::remove_file(staged);
    }
    fetched
}

fn fetch(url: &str) -> impl FnOnce(reqwest::Error) -> Error {
    let url = url.to_owned();
    move |error| Error::Fetch { url, error }
}

/// Executable, and on macOS without a quarantine attribute, should the
/// process that wrote it be one that marks what it downloads: the login
/// item that runs it has nobody to answer Gatekeeper's question.
fn make_executable(path: &Path) -> Result<(), Error> {
    os::fs::make_executable(path).map_err(io("making executable", path))?;
    // A file whose quarantine stays is Gatekeeper's to ask about.
    let _ = os::fs::clear_quarantine(path);
    Ok(())
}

/// The PATH a login item should carry. An app opened from the Finder or
/// the Start menu runs with the system's sparse PATH, so the agents its
/// login item starts would not find the providers a person installed;
/// their login shell's PATH is what a terminal would have given them.
/// None on Windows, which has no login shell: amux reads the logged-on
/// PATH there itself (`os::paths::current_path`).
async fn login_path() -> Option<String> {
    const MARK: &str = "__amux_path__";
    let shell = os::paths::login_shell()?;
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(shell)
            .args(["-ilc", &format!("printf '\\n{MARK}%s' \"$PATH\"")])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let path = text.rsplit_once(MARK)?.1.trim();
    (!path.is_empty()).then(|| path.to_owned())
}

/// Runs the installed amux with `args` to completion, within
/// [`RUN_PATIENCE`], with `home` as its home when one is given.
///
/// Its standard output goes nowhere and its standard error to a file of
/// its own, never to a pipe: whatever it leaves running in the background
/// may hold its standard streams for as long as that runs, and a pipe this
/// waited to see the end of would end only then (a desktop app's first
/// start would hang on it). The file is read once the command exits.
///
/// It starts in the background ([`os::process::background`]): on Windows
/// with no window, since a desktop app has no console for it to share and
/// a console program started from one would otherwise open its own.
async fn run(binary: &Path, args: &[OsString], home: Option<&Path>) -> Result<(), Error> {
    let words = std::iter::once(binary.as_os_str())
        .chain(args.iter().map(OsString::as_os_str))
        .map(|word| word.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let (stderr, said) = said_file()?;
    let mut command = tokio::process::Command::new(binary);
    // Where the caller runs, as a plain start would; the binary's folder
    // when that is gone.
    let cwd = std::env::current_dir()
        .ok()
        .or_else(|| binary.parent().map(Path::to_owned))
        .unwrap_or_else(std::env::temp_dir);
    os::process::background(&mut command, os::process::Detach::Group, &cwd);
    command
        .args(args)
        // The installation is the one `ensure` resolved, named by `--config`
        // when it is not the default; an `AMUX_CONFIG` the client inherited
        // would point the command at another.
        .env_remove("AMUX_CONFIG");
    if let Some(home) = home {
        // Where each OS's home is read from (os::paths::home_dir).
        command.env("HOME", home).env("USERPROFILE", home);
    }
    if let Some(path) = login_path().await {
        command.env("PATH", path);
    }
    let given = os::process::Given {
        stdin: os::process::Stream::Null,
        stdout: os::process::Stream::Null,
        stderr: os::process::Stream::File(stderr),
        files: &[],
    };
    let mut child = os::process::spawn_background(&mut command, given)
        .map_err(io("running", binary))?
        .child;
    let status = tokio::time::timeout(RUN_PATIENCE, child.wait()).await;
    if status.is_err() {
        // Given up on: it ends here, as a dropped child once did.
        if let Ok(process) = child.pin() {
            let _ = process.kill();
        }
    }
    let status = status.map(|waited| waited.map_err(io("running", binary)));
    let output = std::fs::read_to_string(&said).unwrap_or_default();
    let _ = std::fs::remove_file(&said);
    let status = status.map_err(|_| Error::TimedOut {
        command: words.clone(),
    })??;
    if status.success() {
        return Ok(());
    }
    Err(Error::Command {
        command: words,
        status: status.to_string(),
        output: output.trim().to_owned(),
    })
}

/// A new file in the temporary folder for a command's standard error: the
/// handle to give it, and its path to read.
fn said_file() -> Result<(std::fs::File, PathBuf), Error> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir();
    loop {
        let path = dir.join(format!(
            "amux-start-{}-{}.err",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io("creating", &path)(error)),
        }
    }
}
