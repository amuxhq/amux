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
//! 3. Otherwise it installs a copy the client bundled, or downloads the
//!    channel's build and checks it against the signed manifest, places it
//!    under a lock so two clients installing at once cannot interleave,
//!    runs `amux init`, and starts it.
//!
//! Then it waits for the front door. Nothing here writes amux's config or
//! its login item: the binary does that itself, through `amux init`, and
//! `amux uninstall` is the only way amux is removed.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use settings::InstallationConfig;
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;

#[cfg(target_os = "macos")]
pub mod update;

/// How long [`ensure`] waits for a started amux to answer.
pub const START_PATIENCE: Duration = Duration::from_secs(30);
/// The most a manifest may be.
const MANIFEST_LIMIT: u64 = 1 << 20;
/// The most a build may be, whatever its manifest says.
const ARTIFACT_LIMIT: u64 = 512 << 20;
const POLL: Duration = Duration::from_millis(100);

/// What a client asks of [`ensure`].
#[derive(Clone, Debug)]
pub struct Options {
    /// A copy of the `amux` binary shipped with the client, installed when
    /// amux is missing. Without one the channel's build is downloaded. The
    /// supervisor moves an installed copy to the channel's current build
    /// within the hour and never downgrades.
    pub bundled: Option<PathBuf>,
    /// Whether a fresh install starts at login. Asked of nobody: a client
    /// with a person in front of it asks them before calling.
    pub login_item: bool,
    /// The installation's config file; `None` is the default one
    /// (`~/.config/amux/config.toml`), which is what every client should
    /// use outside tests.
    pub config: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            bundled: None,
            login_item: true,
            config: None,
        }
    }
}

/// What [`ensure`] found or did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// amux was already answering; nothing was touched.
    Running,
    /// amux was installed but stopped, and is now started.
    Started,
    /// amux was installed from `source`, set up and started.
    Installed { version: String, source: Source },
}

/// Where an installed amux came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Bundled,
    Downloaded,
}

/// What [`ensure`] is doing, for a client that shows progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Installing from the bundled copy or a download.
    Installing(Source),
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
    #[error("this build trusts no release key, so it cannot check a download; bundle amux instead")]
    NoKeys,
    #[error("`{command}` failed ({status}): {output}")]
    Command {
        command: String,
        status: String,
        output: String,
    },
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
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|home| !home.is_empty())
        .ok_or(Error::NoHome)?;
    Ok(PathBuf::from(home).join(".amux").join("bin"))
}

/// The installed binary, `~/.amux/bin/amux` (`amux.exe` on Windows).
pub fn installed_binary() -> Result<PathBuf, Error> {
    Ok(install_dir()?.join(if cfg!(windows) { "amux.exe" } else { "amux" }))
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
    let binary = installed_binary()?;
    let installed = match binary.exists() {
        true => None,
        false => install(&config, options, &binary, &mut on_step).await?,
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
    if installed.is_some() {
        let answer = if options.login_item { "yes" } else { "no" };
        run(&binary, &with_config(&["init", "--login-item", answer])).await?;
    }
    // Starts the supervisor (or, without one, the daemon) unless the login
    // item already has; a running one is left alone.
    if !answering(&door).await {
        run(&binary, &with_config(&["server", "start"])).await?;
    }
    wait_for(&door).await?;
    Ok(match installed {
        Some((version, source)) => Outcome::Installed { version, source },
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

/// Places the bundled copy or a verified download at `binary`, under the
/// install directory's lock. `None` when another client installed it
/// while this one waited for the lock.
async fn install(
    config: &InstallationConfig,
    options: &Options,
    binary: &Path,
    on_step: &mut impl FnMut(Step),
) -> Result<Option<(String, Source)>, Error> {
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
    let placed = async {
        let (version, source) = match &options.bundled {
            Some(bundled) => {
                on_step(Step::Installing(Source::Bundled));
                tokio::fs::copy(bundled, &staged)
                    .await
                    .map_err(io("copying", bundled))?;
                (version_of(&staged).await?, Source::Bundled)
            }
            None => {
                on_step(Step::Installing(Source::Downloaded));
                (
                    download(config, &staged, on_step).await?,
                    Source::Downloaded,
                )
            }
        };
        make_executable(&staged)?;
        tokio::fs::rename(&staged, binary)
            .await
            .map_err(io("placing", binary))?;
        Ok((version, source))
    }
    .await;
    if placed.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    drop(lock);
    placed.map(Some)
}

/// Downloads the channel's build for this target to `staged`, believing
/// nothing in the manifest until its signature verifies against a key this
/// build trusts, and the build only once its size and hash are the
/// manifest's. Returns its version.
async fn download(
    config: &InstallationConfig,
    staged: &Path,
    on_step: &mut impl FnMut(Step),
) -> Result<String, Error> {
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
    artifact(&client, build, staged, |received| {
        on_step(Step::Downloading {
            received,
            total: build.size,
        })
    })
    .await?;
    Ok(build.version.clone())
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

/// The version a binary reports (`amux 0.9.0` prints `0.9.0`).
async fn version_of(binary: &Path) -> Result<String, Error> {
    make_executable(binary)?;
    let output = tokio::process::Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(io("running", binary))?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .split_whitespace()
        .last()
        .unwrap_or_default()
        .to_owned())
}

/// Executable, and on macOS without the quarantine a copy out of a
/// downloaded app inherits: the binary is signed and notarized, and the
/// login item that runs it has nobody to answer Gatekeeper's question.
fn make_executable(path: &Path) -> Result<(), Error> {
    // Windows runs whatever it is handed and keeps no quarantine here.
    #[cfg(not(unix))]
    let _ = path;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(io("making executable", path))?;
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .expect("a path has no interior NUL");
        // SAFETY: both arguments are NUL-terminated strings that outlive
        // the call. A file without the attribute fails harmlessly.
        unsafe {
            libc::removexattr(
                path.as_ptr(),
                c"com.apple.quarantine".as_ptr(),
                libc::XATTR_NOFOLLOW,
            );
        }
    }
    Ok(())
}

/// The PATH a login item should carry. An app opened from the Finder or
/// the Start menu runs with the system's sparse PATH, so the agents its
/// login item starts would not find the providers a person installed;
/// their login shell's PATH is what a terminal would have given them.
#[cfg(unix)]
async fn login_path() -> Option<String> {
    const MARK: &str = "__amux_path__";
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|shell| !shell.is_empty())?;
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

#[cfg(not(unix))]
async fn login_path() -> Option<String> {
    None
}

/// Runs the installed amux with `args` to completion.
async fn run(binary: &Path, args: &[OsString]) -> Result<(), Error> {
    let mut command = tokio::process::Command::new(binary);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        // The installation is the one `ensure` resolved, named by `--config`
        // when it is not the default; an `AMUX_CONFIG` the client inherited
        // would point the command at another.
        .env_remove("AMUX_CONFIG");
    if let Some(path) = login_path().await {
        command.env("PATH", path);
    }
    let output = command.output().await.map_err(io("running", binary))?;
    if output.status.success() {
        return Ok(());
    }
    let words = std::iter::once(binary.as_os_str())
        .chain(args.iter().map(OsString::as_os_str))
        .map(|word| word.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    Err(Error::Command {
        command: words,
        status: output.status.to_string(),
        output: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}
