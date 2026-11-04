//! The Mac app's own updates, apart from amux's.
//!
//! amux updates itself through its supervisor; the Mac app is a client and
//! versions on its own. Its feed is `<releases_url>/mac-<channel>.json`:
//! the same signed manifest as amux's, with one target, [`TARGET`], whose
//! artifact is the zipped app. A daemon looks itself up by its triple, so a
//! Mac feed served in amux's place holds nothing a daemon would install.
//!
//! [`check`] reads the feed for the installation's channel, downloads a
//! newer app to `<dir>/<version>.zip` once the manifest verifies, keeps it
//! only when its size and hash are the manifest's, unpacks it to
//! `<dir>/<version>/Amux.app` and answers that path. The app swaps it in
//! when it quits; nothing here touches the running app. Every other staged
//! version is removed, and everything once the running app is current, so
//! the directory holds at most one.

use std::path::{Path, PathBuf};

use crate::{Error, artifact, config, http, io, manifest};

/// The Mac app's entry in its feed's manifest.
pub const TARGET: &str = "mac-app";
/// The app's bundle, as its zip holds it and as the update is staged.
pub const BUNDLE: &str = "Amux.app";
/// Written beside a staged version once it is whole, so a check stopped
/// partway is redone rather than offered.
const READY: &str = "ready";

/// What [`check`] needs to know.
#[derive(Clone, Debug)]
pub struct Options {
    /// The installation config whose `releases_url` and `channel` name the
    /// feed; `None` is the default one.
    pub config: Option<PathBuf>,
    /// The running app's version.
    pub current: String,
    /// Where updates are staged.
    pub dir: PathBuf,
}

/// A newer app, unpacked and ready to swap in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    pub version: String,
    pub app: PathBuf,
}

/// The installation's Mac feed: `mac-<channel>.json` beside amux's own.
pub fn feed_url(config: &settings::InstallationConfig) -> String {
    format!(
        "{}/mac-{}.json",
        config.releases_url.trim_end_matches('/'),
        config.channel_name()
    )
}

/// The newer app the feed names, staged, or `None` when the running one is
/// current or the feed has no Mac build.
pub async fn check(options: &Options) -> Result<Option<Staged>, Error> {
    let config = config(options.config.as_deref())?;
    let url = feed_url(&config);
    let client = http(&url)?;
    let manifest = manifest(&client, &url, config.channel_name()).await?;
    // Nothing newer: whatever is staged has been swapped in, or is older
    // than the running app.
    let Some(build) = manifest
        .targets
        .get(TARGET)
        .filter(|build| newer(&build.version, &options.current))
    else {
        remove_others(&options.dir, None);
        return Ok(None);
    };
    let staged = options.dir.join(&build.version);
    let app = staged.join(BUNDLE);
    if staged.join(READY).is_file() && app.is_dir() {
        return Ok(Some(Staged {
            version: build.version.clone(),
            app,
        }));
    }
    tokio::fs::create_dir_all(&options.dir)
        .await
        .map_err(io("creating", &options.dir))?;
    remove_others(&options.dir, None);
    let zip = options.dir.join(format!("{}.zip", build.version));
    artifact(&client, build, &zip, |_| {}).await?;
    let unpacked = unpack(&zip, &staged).await;
    let _ = std::fs::remove_file(&zip);
    unpacked?;
    if !app.is_dir() {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(Error::NoBuild {
            channel: config.channel_name().to_owned(),
            target: format!("{TARGET} ({} has no {BUNDLE})", build.url),
        });
    }
    std::fs::write(staged.join(READY), &build.version).map_err(io("writing", &staged))?;
    remove_others(&options.dir, Some(&build.version));
    Ok(Some(Staged {
        version: build.version.clone(),
        app,
    }))
}

/// Whether `offered` is a later version than `current`. A version that does
/// not parse is never newer: the feed cannot move the app to something it
/// cannot order.
fn newer(offered: &str, current: &str) -> bool {
    match (
        semver::Version::parse(offered),
        semver::Version::parse(current),
    ) {
        (Ok(offered), Ok(current)) => offered > current,
        _ => false,
    }
}

/// Unpacks the zip into `into`, as `ditto` keeps an app's signature,
/// extended attributes and symbolic links whole.
async fn unpack(zip: &Path, into: &Path) -> Result<(), Error> {
    let _ = std::fs::remove_dir_all(into);
    let output = tokio::process::Command::new("/usr/bin/ditto")
        .arg("-x")
        .arg("-k")
        .arg(zip)
        .arg(into)
        .output()
        .await
        .map_err(io("unpacking", zip))?;
    if !output.status.success() {
        let _ = std::fs::remove_dir_all(into);
        return Err(Error::Command {
            command: format!("ditto -x -k {}", zip.display()),
            status: output.status.to_string(),
            output: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(())
}

/// Removes every staged version but `keep`.
fn remove_others(dir: &Path, keep: Option<&str>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if keep.is_some_and(|keep| name.to_str() == Some(keep)) {
            continue;
        }
        let path = entry.path();
        let _ = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_later_version_is_newer() {
        assert!(newer("0.0.2", "0.0.1"));
        assert!(newer("0.1.0", "0.0.9"));
        assert!(!newer("0.0.1", "0.0.1"));
        assert!(!newer("0.0.1", "0.0.2"));
        assert!(!newer("latest", "0.0.1"));
        assert!(!newer("0.0.2", "1"));
    }

    #[test]
    fn the_mac_feed_sits_beside_amuxs_on_the_installations_channel() {
        let mut config = settings::InstallationConfig {
            releases_url: "https://amux.sh/releases/".into(),
            ..Default::default()
        };
        assert_eq!(
            feed_url(&config),
            "https://amux.sh/releases/mac-stable.json"
        );
        config.channel = settings::Channel::Preview;
        assert_eq!(
            feed_url(&config),
            "https://amux.sh/releases/mac-preview.json"
        );
    }
}
