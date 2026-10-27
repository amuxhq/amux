//! `amux supervise`: the daemon's parent under a desktop install.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use node::supervisor::{Inherited, Params, SuperviseOptions, UpdatePolicy, UpdateSource};
use settings::{InstallationConfig, Updates};

/// The supervisor's log file under the data dir.
pub const SUPERVISOR_LOG: &str = "supervisor.log";

/// What the config says the supervisor may install. A build without a
/// release key installs nothing.
fn policy(config: &InstallationConfig) -> Result<UpdatePolicy> {
    let auto = config.updates()? == Updates::Auto;
    let source = node::release::release_key().map(|key| UpdateSource {
        channel: config.channel_name().to_owned(),
        manifest_url: config.manifest_url(),
        key,
    });
    Ok(UpdatePolicy { auto, source })
}

pub fn run(
    config: &InstallationConfig,
    config_path: Option<&Path>,
    inherit: Option<&str>,
) -> Result<()> {
    if !config.supervisor {
        bail!(
            "this install has no supervisor (supervisor: off): the service manager runs amux daemon"
        );
    }
    let inherited = inherit
        .map(|state| Inherited::parse(state).context("--inherit is not a handed-over state"))
        .transpose()?;
    crate::server::log_to(&config.root.join(SUPERVISOR_LOG))?;
    let started = policy(config)?;
    if started.source.is_none() {
        tracing::warn!(
            "this build trusts no release key: it restarts the daemon but installs nothing"
        );
    }
    // The config is read again before every check, so `amux config
    // channel` takes effect without a restart.
    let reread = config_path.map(Path::to_owned);
    let updates = Arc::new(move || {
        match crate::connect::load_config(reread.as_deref()).and_then(|config| policy(&config)) {
            Ok(policy) => policy,
            Err(error) => {
                tracing::warn!(%error, "reading the config for an update check");
                started.clone()
            }
        }
    });
    let mut args: Vec<OsString> = Vec::new();
    if let Some(path) = config_path {
        args.push("--config".into());
        args.push(PathBuf::from(path).into_os_string());
    }
    let options = SuperviseOptions {
        binary: std::env::current_exe().context("finding the amux binary")?,
        args,
        data_dir: config.root.clone(),
        running: node::version()
            .parse()
            .context("this build's version is not semver")?,
        target: node::release::TARGET.to_owned(),
        updates,
        keep_awake: config.keep_awake,
        clock: Arc::new(agent_dir::SystemClock),
        params: Params::default(),
        inherited,
    };
    tracing::info!(
        version = node::version(),
        manifest = %config.manifest_url(),
        "supervising"
    );
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(node::supervisor::supervise(options))
        .context("supervising the daemon")
}
