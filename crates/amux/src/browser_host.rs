//! Registering `amux internal browser` with the browsers on this machine,
//! so the amux extension can start it.
//!
//! A Chromium browser finds a native messaging host by a manifest file in
//! its `NativeMessagingHosts` folder: the host's name, the program to run,
//! and the extensions allowed to start it. The browser runs that program
//! with the calling extension's origin as its argument, so the program is a
//! small script that runs `amux internal browser`, kept beside the installed
//! binary.
//!
//! The supervisor writes the script and a manifest for each installed
//! Chromium browser when it starts, so an install made before a browser was
//! put on the machine gains it at the next start, and only when it is the
//! installed `~/.amux/bin/amux`: a build run from anywhere else (a test, a
//! development tree) must never point a person's browsers at itself.
//! `amux uninstall` removes them. A manifest is rewritten only when its
//! bytes would change.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::json;

/// The host's name, as the extension asks for it.
pub const HOST: &str = "sh.amux.browser";

/// The extension allowed to start the host: the ID its public key gives
/// it, in the store and unpacked alike.
pub const EXTENSION_ID: &str = "omnhphmiepghpgpbfgjebeaajfppkhie";

/// The script a manifest runs, beside the installed binary.
pub const SCRIPT: &str = "amux-browser-host";

/// What registering needs to know.
#[derive(Clone, Debug)]
pub struct Hosts {
    /// The installed binary the script runs.
    pub binary: PathBuf,
    /// The installation config, when not the default one.
    pub config: Option<PathBuf>,
    /// The person's home, which every browser's folder is under.
    pub home: PathBuf,
}

impl Hosts {
    /// The script's path, beside the binary.
    pub fn script(&self) -> PathBuf {
        self.binary.with_file_name(SCRIPT)
    }

    /// The script's text: the binary, with its config, as the browser
    /// host, passing on what the browser gives it.
    pub fn script_text(&self) -> String {
        let mut command = format!("exec {}", quoted(&self.binary));
        if let Some(config) = &self.config {
            command.push_str(&format!(" --config {}", quoted(config)));
        }
        format!(
            "#!/bin/sh\n# Written by amux: the amux browser extension's way in.\n{command} internal browser \"$@\"\n"
        )
    }

    /// The manifest each browser reads.
    pub fn manifest(&self) -> String {
        let manifest = json!({
            "name": HOST,
            "description": "amux",
            "path": self.script(),
            "type": "stdio",
            "allowed_origins": [format!("chrome-extension://{EXTENSION_ID}/")],
        });
        let mut text = serde_json::to_string_pretty(&manifest).expect("a manifest serialises");
        text.push('\n');
        text
    }

    /// The `NativeMessagingHosts` folder of each Chromium browser whose own
    /// folder exists here.
    pub fn folders(&self) -> Vec<PathBuf> {
        browser_dirs(&self.home)
            .into_iter()
            .filter(|dir| dir.is_dir())
            .map(|dir| dir.join("NativeMessagingHosts"))
            .collect()
    }

    fn manifest_file(folder: &Path) -> PathBuf {
        folder.join(format!("{HOST}.json"))
    }

    /// Writes the script and every installed browser's manifest, each only
    /// when it would change, and says which manifests it wrote.
    pub fn register(&self) -> io::Result<Vec<PathBuf>> {
        let folders = self.folders();
        if folders.is_empty() {
            return Ok(Vec::new());
        }
        write_if_changed(&self.script(), self.script_text().as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(self.script(), std::fs::Permissions::from_mode(0o755))?;
        }
        let manifest = self.manifest();
        let mut written = Vec::new();
        for folder in folders {
            std::fs::create_dir_all(&folder)?;
            let file = Self::manifest_file(&folder);
            if write_if_changed(&file, manifest.as_bytes())? {
                written.push(file);
            }
        }
        Ok(written)
    }

    /// Removes the manifests that run this installation's script, and the
    /// script, and says which manifests it removed.
    pub fn remove(&self) -> io::Result<Vec<PathBuf>> {
        let mut removed = Vec::new();
        let script = self.script().to_string_lossy().into_owned();
        for folder in self.folders() {
            let file = Self::manifest_file(&folder);
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            // Another installation's manifest stays.
            let ours = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .is_some_and(|manifest| manifest["path"] == script.as_str());
            if ours {
                std::fs::remove_file(&file)?;
                removed.push(file);
            }
        }
        match std::fs::remove_file(self.script()) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        Ok(removed)
    }
}

/// Where each Chromium browser keeps its data under `home`. None on Windows,
/// where a browser finds its hosts in the registry, which this does not
/// write yet.
fn browser_dirs(home: &Path) -> Vec<PathBuf> {
    if cfg!(windows) {
        return Vec::new();
    }
    #[cfg(target_os = "macos")]
    let (base, names): (PathBuf, &[&str]) = (
        home.join("Library/Application Support"),
        &[
            "Google/Chrome",
            "Google/Chrome Beta",
            "Google/Chrome Canary",
            "Chromium",
            "Microsoft Edge",
            "BraveSoftware/Brave-Browser",
            "Arc/User Data",
            "Vivaldi",
        ],
    );
    #[cfg(not(target_os = "macos"))]
    let (base, names): (PathBuf, &[&str]) = (
        home.join(".config"),
        &[
            "google-chrome",
            "google-chrome-beta",
            "chromium",
            "microsoft-edge",
            "BraveSoftware/Brave-Browser",
            "vivaldi",
        ],
    );
    names.iter().map(|name| base.join(name)).collect()
}

/// A path as one shell word.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    if std::fs::read(path).is_ok_and(|held| held == bytes) {
        return Ok(false);
    }
    node::write_durably(path, bytes)?;
    Ok(true)
}

/// The hosts the installed binary registers, when this process is it.
pub fn installed(config: Option<&Path>) -> Option<Hosts> {
    let binary =
        crate::setup::install_dir()
            .ok()?
            .join(if cfg!(windows) { "amux.exe" } else { "amux" });
    let here = std::env::current_exe().ok()?;
    let same = std::fs::canonicalize(&here).ok()? == std::fs::canonicalize(&binary).ok()?;
    let home = binary.parent()?.parent()?.parent()?.to_owned();
    same.then(|| Hosts {
        binary,
        config: config.map(Path::to_owned),
        home,
    })
}

// The host script is a POSIX shell script and Windows registers nothing yet.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn hosts(home: &Path) -> Hosts {
        Hosts {
            binary: home.join(".amux/bin/amux"),
            config: None,
            home: home.to_owned(),
        }
    }

    #[test]
    fn the_manifest_runs_the_script_for_the_extension_alone() {
        let hosts = hosts(Path::new("/home/someone"));
        let manifest: serde_json::Value = serde_json::from_str(&hosts.manifest()).unwrap();
        assert_eq!(manifest["name"], HOST);
        assert_eq!(manifest["type"], "stdio");
        assert_eq!(
            manifest["path"],
            "/home/someone/.amux/bin/amux-browser-host"
        );
        assert_eq!(
            manifest["allowed_origins"],
            json!([format!("chrome-extension://{EXTENSION_ID}/")])
        );
        assert_eq!(
            hosts.script_text().lines().last().unwrap(),
            "exec '/home/someone/.amux/bin/amux' internal browser \"$@\""
        );
        let named = Hosts {
            config: Some(PathBuf::from("/etc/it's.toml")),
            ..hosts
        };
        assert!(
            named
                .script_text()
                .contains(r"--config '/etc/it'\''s.toml' internal browser")
        );
    }

    /// Only browsers that are installed get a manifest; registering twice
    /// writes nothing new; removing leaves another installation's.
    #[test]
    fn registering_follows_the_installed_browsers() {
        let home = tempfile::tempdir().unwrap();
        let hosts = hosts(home.path());
        std::fs::create_dir_all(hosts.binary.parent().unwrap()).unwrap();
        assert_eq!(hosts.register().unwrap(), Vec::<PathBuf>::new());
        assert!(!hosts.script().exists(), "no browser, no script");

        let chrome = &browser_dirs(home.path())[0];
        let other = &browser_dirs(home.path())[3];
        std::fs::create_dir_all(chrome).unwrap();
        std::fs::create_dir_all(other).unwrap();
        let written = hosts.register().unwrap();
        assert_eq!(written.len(), 2);
        assert!(hosts.script().exists());
        assert_eq!(hosts.register().unwrap(), Vec::<PathBuf>::new());

        let theirs = Hosts {
            binary: home.path().join("elsewhere/amux"),
            ..hosts.clone()
        };
        let other_manifest = other
            .join("NativeMessagingHosts")
            .join(format!("{HOST}.json"));
        std::fs::write(&other_manifest, theirs.manifest()).unwrap();
        let removed = hosts.remove().unwrap();
        assert_eq!(
            removed,
            [chrome
                .join("NativeMessagingHosts")
                .join(format!("{HOST}.json"))]
        );
        assert!(other_manifest.exists());
        assert!(!hosts.script().exists());
    }
}
