//! The install scripts against a manifest the release crate serialized.
//!
//! `scripts/install/install.sh` and `install.ps1` are what
//! `https://amux.sh/install` hands a new machine. They parse the channel
//! manifest by hand, so these cases serve them one written exactly as a
//! deploy writes it, from a loopback server standing in for amux.sh and for
//! the release's assets, and hold what they install: this machine's entry,
//! only when the download's size and sha256 are the entry's, and on
//! Windows the launcher beside it, only when its sha256 is the one the
//! release's `checksums.txt` lists.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};

use release::{Manifest, Release};

/// The stand-in binary: what a correct install leaves in place.
const BINARY: &[u8] = b"#!/bin/sh\necho amux 9.9.9\n";

/// Every target a script may ask for, so a case holds on any machine that
/// runs it.
const TARGETS: &[&str] = &[
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
    "aarch64-pc-windows-msvc",
    "x86_64-pc-windows-msvc",
];

fn script(name: &str) -> PathBuf {
    source_tree::crate_dir!()
        .join("../../scripts/install")
        .join(name)
}

/// A loopback server answering each path with its body, and anything else
/// with 404. It lives as long as the test process.
fn serve(routes: Vec<(String, Vec<u8>)>, listener: TcpListener) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => request.extend_from_slice(&buffer[..read]),
                }
            }
            let request = String::from_utf8_lossy(&request);
            let path = request.split_whitespace().nth(1).unwrap_or_default();
            let found = routes.iter().find(|(route, _)| route == path);
            let (status, body) = match found {
                Some((_, body)) => ("200 OK", body.as_slice()),
                None => ("404 Not Found", &b"not found"[..]),
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
        }
    });
}

fn sha256(bytes: &[u8]) -> String {
    release::sha256_of(bytes)
}

/// A stable manifest naming the stand-in binary at `url` for `targets`.
fn manifest(url: &str, targets: &[&str], sha256: &str, size: u64) -> Manifest {
    let targets: BTreeMap<String, Release> = targets
        .iter()
        .map(|target| {
            (
                (*target).to_owned(),
                Release {
                    version: "9.9.9".into(),
                    url: url.to_owned(),
                    sha256: sha256.to_owned(),
                    size,
                },
            )
        })
        .collect();
    Manifest {
        channel: "stable".into(),
        rollout: Some(10),
        targets,
        signature: "c2lnbmVkK2J5L3RoZT1yZWxlYXNlK2tleQ==".into(),
    }
}

/// What a case serves: where the releases are, and the home the script
/// installs into.
struct Feed {
    releases_url: String,
    home: tempfile::TempDir,
}

impl Feed {
    /// Serves `write(url of the binary)` as the stable manifest, beside
    /// the stand-in binary, the launcher and the release's checksums.
    fn serving(write: impl FnOnce(&str) -> String) -> Self {
        Self::serving_with(write, Vec::new())
    }

    /// The same, with the release's `other` files beside the binary, by
    /// name.
    fn serving_with(write: impl FnOnce(&str) -> String, other: Vec<(String, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let binary_url = format!("{base}/assets/amux");
        let mut routes = vec![
            (
                "/releases/stable.json".to_owned(),
                write(&binary_url).into_bytes(),
            ),
            ("/assets/amux".to_owned(), BINARY.to_vec()),
        ];
        routes.extend(
            other
                .into_iter()
                .map(|(name, body)| (format!("/assets/{name}"), body)),
        );
        serve(routes, listener);
        Self {
            releases_url: format!("{base}/releases"),
            home: tempfile::tempdir().unwrap(),
        }
    }

    /// The manifest as a deploy writes it: pretty-printed.
    fn deployed(targets: &[&str], sha256: &str, size: u64) -> Self {
        Self::serving(|url| {
            let json = serde_json::to_string_pretty(&manifest(url, targets, sha256, size)).unwrap();
            format!("{json}\n")
        })
    }

    fn honest() -> Self {
        Self::deployed(TARGETS, &sha256(BINARY), BINARY.len() as u64)
    }
}

fn said(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[cfg(unix)]
mod sh {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn install(feed: &Feed, more: &[(&str, &str)]) -> Output {
        let mut command = Command::new("sh");
        command
            .arg(script("install.sh"))
            .env("HOME", feed.home.path())
            .env("SHELL", "/bin/zsh")
            .env("AMUX_RELEASES_URL", &feed.releases_url)
            .env_remove("AMUX_NO_MODIFY_PATH");
        for (name, value) in more {
            command.env(name, value);
        }
        command.output().unwrap()
    }

    fn installed(feed: &Feed) -> PathBuf {
        feed.home.path().join(".amux/bin/amux")
    }

    #[test]
    fn it_installs_the_stable_manifests_build_for_this_machine() {
        let feed = Feed::honest();
        let output = install(&feed, &[]);
        assert!(output.status.success(), "{}", said(&output));
        let binary = installed(&feed);
        assert_eq!(std::fs::read(&binary).unwrap(), BINARY);
        let mode = std::fs::metadata(&binary).unwrap().permissions().mode();
        assert_ne!(mode & 0o111, 0, "the installed binary is executable");
        assert!(
            said(&output).contains("amux 9.9.9 has been installed"),
            "{}",
            said(&output)
        );
        let profile = std::fs::read_to_string(feed.home.path().join(".zshrc")).unwrap();
        assert!(profile.contains(".amux/bin"), "{profile}");

        // Again: the same binary, and the profile is not told twice.
        let output = install(&feed, &[]);
        assert!(output.status.success(), "{}", said(&output));
        let profile = std::fs::read_to_string(feed.home.path().join(".zshrc")).unwrap();
        assert_eq!(profile.matches(".amux/bin").count(), 1, "{profile}");
    }

    #[test]
    fn it_reads_a_manifest_however_it_is_spaced() {
        let feed = Feed::serving(|url| {
            serde_json::to_string(&manifest(
                url,
                TARGETS,
                &sha256(BINARY),
                BINARY.len() as u64,
            ))
            .unwrap()
        });
        let output = install(&feed, &[]);
        assert!(output.status.success(), "{}", said(&output));
        assert_eq!(std::fs::read(installed(&feed)).unwrap(), BINARY);
    }

    #[test]
    fn it_refuses_a_download_whose_sha256_is_not_the_manifests() {
        let feed = Feed::deployed(TARGETS, &sha256(b"another binary"), BINARY.len() as u64);
        let output = install(&feed, &[]);
        assert!(!output.status.success(), "{}", said(&output));
        assert!(
            said(&output).contains("checksum verification failed"),
            "{}",
            said(&output)
        );
        assert!(!installed(&feed).exists(), "nothing is installed");
    }

    #[test]
    fn it_refuses_a_download_whose_size_is_not_the_manifests() {
        let feed = Feed::deployed(TARGETS, &sha256(BINARY), BINARY.len() as u64 + 1);
        let output = install(&feed, &[]);
        assert!(!output.status.success(), "{}", said(&output));
        assert!(
            said(&output).contains("the manifest says"),
            "{}",
            said(&output)
        );
        assert!(!installed(&feed).exists(), "nothing is installed");
    }

    #[test]
    fn it_says_so_when_the_channel_has_no_build_for_this_machine() {
        let feed = Feed::deployed(
            &["riscv64gc-unknown-linux-gnu"],
            &sha256(BINARY),
            BINARY.len() as u64,
        );
        let output = install(&feed, &[]);
        assert!(!output.status.success(), "{}", said(&output));
        assert!(
            said(&output).contains("the stable channel has no build for"),
            "{}",
            said(&output)
        );
        assert!(!installed(&feed).exists(), "nothing is installed");
    }

    #[test]
    fn it_leaves_the_profile_alone_when_asked() {
        let feed = Feed::honest();
        let output = install(&feed, &[("AMUX_NO_MODIFY_PATH", "1")]);
        assert!(output.status.success(), "{}", said(&output));
        assert_eq!(std::fs::read(installed(&feed)).unwrap(), BINARY);
        assert!(!feed.home.path().join(".zshrc").exists());
    }
}

#[cfg(windows)]
mod powershell {
    use std::process::{Child, Stdio};

    use super::*;

    /// The stand-in launcher installed beside amux, and the name the
    /// release publishes it under.
    const LAUNCHER: &[u8] = b"amux-launch 9.9.9";
    const LAUNCHER_ASSET: &str = "amux-launch-windows-x86_64.exe";

    /// What the release's `checksums.txt` says of the launcher.
    #[derive(Clone, Copy)]
    enum Launcher {
        /// Its own sha256.
        Listed,
        /// Another file's: the download is not the release's.
        Tampered,
        /// Nothing: a release from before the launcher shipped.
        Unlisted,
    }

    /// A deployed manifest for every target, with the launcher and the
    /// release's checksums beside the binary.
    fn feed(launcher: Launcher) -> Feed {
        let mut checksums = format!("{}  amux-windows-x86_64.exe\n", sha256(BINARY));
        match launcher {
            Launcher::Listed => checksums += &format!("{}  {LAUNCHER_ASSET}\n", sha256(LAUNCHER)),
            Launcher::Tampered => {
                checksums += &format!("{} *{LAUNCHER_ASSET}\n", sha256(b"another launcher"));
            }
            Launcher::Unlisted => {}
        }
        Feed::serving_with(
            |url| {
                let manifest = manifest(url, TARGETS, &sha256(BINARY), BINARY.len() as u64);
                format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap())
            },
            vec![
                ("checksums.txt".to_owned(), checksums.into_bytes()),
                (LAUNCHER_ASSET.to_owned(), LAUNCHER.to_vec()),
            ],
        )
    }

    fn honest() -> Feed {
        feed(Launcher::Listed)
    }

    /// The script in a scratch environment: the person's homes, temporary
    /// folder and user PATH are never this test's to write. `path_key`
    /// names the scratch key the user PATH is kept in; without one the
    /// script is asked to leave PATH alone.
    fn install(feed: &Feed, path_key: Option<&ScratchKey>) -> Output {
        let home = feed.home.path();
        let (appdata, local, temp) = (
            home.join("AppData").join("Roaming"),
            home.join("AppData").join("Local"),
            home.join("AppData").join("Local").join("Temp"),
        );
        for dir in [&appdata, &local, &temp] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let mut command = Command::new("powershell");
        command
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(script("install.ps1"))
            .env("USERPROFILE", home)
            .env("APPDATA", &appdata)
            .env("LOCALAPPDATA", &local)
            .env("TEMP", &temp)
            .env("TMP", &temp)
            .env("AMUX_RELEASES_URL", &feed.releases_url)
            .stdin(Stdio::null());
        match path_key {
            Some(key) => command
                .env("AMUX_TEST_ENVIRONMENT_KEY", &key.name)
                .env_remove("AMUX_NO_MODIFY_PATH"),
            None => command
                .env("AMUX_NO_MODIFY_PATH", "1")
                .env_remove("AMUX_TEST_ENVIRONMENT_KEY"),
        };
        command.output().unwrap()
    }

    fn bin(feed: &Feed) -> PathBuf {
        feed.home.path().join(".amux").join("bin")
    }

    fn installed(feed: &Feed) -> PathBuf {
        bin(feed).join("amux.exe")
    }

    fn launcher(feed: &Feed) -> PathBuf {
        bin(feed).join("amux-launch.exe")
    }

    /// The names in the install folder, sorted.
    fn listing(feed: &Feed) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(bin(feed))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A key of the test's own under `HKCU\Software\amux-test-…`, standing
    /// in for `HKCU\Environment`, deleted with what it holds when dropped.
    /// Written and read with reg.exe, as the person's own tools would.
    struct ScratchKey {
        name: String,
    }

    impl ScratchKey {
        fn new() -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self {
                name: format!(r"Software\amux-test-xtask-{}-{nanos}", std::process::id()),
            }
        }

        fn reg(&self, verb: &str, arguments: &[&str]) -> Output {
            Command::new("reg")
                .arg(verb)
                .arg(format!(r"HKCU\{}", self.name))
                .args(arguments)
                .stdin(Stdio::null())
                .output()
                .unwrap()
        }

        fn set_path(&self, value: &str) {
            let output = self.reg(
                "add",
                &["/v", "Path", "/t", "REG_EXPAND_SZ", "/d", value, "/f"],
            );
            assert!(output.status.success(), "{}", said(&output));
        }

        /// The `Path` value as stored, `%VARIABLES%` and all, after its
        /// kind.
        fn path(&self) -> Option<(String, String)> {
            let output = self.reg("query", &["/v", "Path"]);
            if !output.status.success() {
                return None;
            }
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            let line = text
                .lines()
                .map(str::trim_start)
                .find(|line| line.starts_with("Path "))?;
            let mut fields = line.splitn(3, "    ");
            let (_, kind, value) = (fields.next()?, fields.next()?, fields.next()?);
            Some((kind.trim().to_owned(), value.trim_end().to_owned()))
        }
    }

    impl Drop for ScratchKey {
        fn drop(&mut self) {
            let _ = self.reg("delete", &["/f"]);
        }
    }

    /// Ends a process when dropped.
    struct Running(Child);

    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn it_installs_the_stable_manifests_build_and_its_launcher() {
        let feed = honest();
        let output = install(&feed, None);
        assert!(output.status.success(), "{}", said(&output));
        assert_eq!(std::fs::read(installed(&feed)).unwrap(), BINARY);
        assert_eq!(std::fs::read(launcher(&feed)).unwrap(), LAUNCHER);
        assert!(
            said(&output).contains("amux 9.9.9 has been installed"),
            "{}",
            said(&output)
        );
        assert_eq!(listing(&feed), ["amux-launch.exe", "amux.exe"]);
    }

    #[test]
    fn it_refuses_a_download_whose_sha256_is_not_the_manifests() {
        let feed = Feed::deployed(TARGETS, &sha256(b"another binary"), BINARY.len() as u64);
        let output = install(&feed, None);
        assert!(!output.status.success(), "{}", said(&output));
        assert!(!installed(&feed).exists(), "nothing is installed");
    }

    /// Every release publishes `checksums.txt`; without it the launcher
    /// can't be checked, and nothing is installed.
    #[test]
    fn it_installs_nothing_from_a_release_without_its_checksums() {
        let feed = Feed::honest();
        let output = install(&feed, None);
        assert!(!output.status.success(), "{}", said(&output));
        assert!(!installed(&feed).exists(), "nothing is installed");
    }

    /// The launcher is the release's only when `checksums.txt` says so:
    /// another download installs nothing, amux included.
    #[test]
    fn it_refuses_a_launcher_whose_sha256_is_not_the_releases() {
        let feed = feed(Launcher::Tampered);
        let output = install(&feed, None);
        assert!(!output.status.success(), "{}", said(&output));
        assert!(
            said(&output).contains("Checksum verification failed for amux-launch.exe"),
            "{}",
            said(&output)
        );
        assert!(!installed(&feed).exists(), "nothing is installed");
        assert!(!launcher(&feed).exists(), "nothing is installed");
    }

    /// A release from before the launcher shipped installs amux alone.
    #[test]
    fn a_release_without_the_launcher_installs_amux_alone() {
        let feed = feed(Launcher::Unlisted);
        let output = install(&feed, None);
        assert!(output.status.success(), "{}", said(&output));
        assert_eq!(std::fs::read(installed(&feed)).unwrap(), BINARY);
        assert!(!launcher(&feed).exists());
    }

    #[test]
    fn it_says_so_when_the_channel_has_no_build_for_this_machine() {
        let feed = Feed::deployed(
            &["riscv64gc-unknown-linux-gnu"],
            &sha256(BINARY),
            BINARY.len() as u64,
        );
        let output = install(&feed, None);
        assert!(!output.status.success(), "{}", said(&output));
        assert!(
            said(&output).contains("has no build for"),
            "{}",
            said(&output)
        );
        assert!(!installed(&feed).exists(), "nothing is installed");
    }

    /// The install folder goes first on the user's PATH in the registry,
    /// once however often the script runs, and the entries already there
    /// stay as written: a `%VARIABLE%` is never expanded for good, and the
    /// value stays a REG_EXPAND_SZ.
    #[test]
    fn it_puts_the_folder_on_the_user_path_once_leaving_the_rest_as_written() {
        let feed = honest();
        let key = ScratchKey::new();
        key.set_path(r"%SystemRoot%\keep;C:\elsewhere");
        let expected = (
            "REG_EXPAND_SZ".to_owned(),
            format!(r"{};%SystemRoot%\keep;C:\elsewhere", bin(&feed).display()),
        );

        let output = install(&feed, Some(&key));
        assert!(output.status.success(), "{}", said(&output));
        assert!(said(&output).contains("to your PATH"), "{}", said(&output));
        assert_eq!(key.path(), Some(expected.clone()));

        let output = install(&feed, Some(&key));
        assert!(output.status.success(), "{}", said(&output));
        assert!(!said(&output).contains("to your PATH"), "{}", said(&output));
        assert_eq!(key.path(), Some(expected), "the folder is listed once");
    }

    /// An installed amux that is running can't be replaced: the script
    /// stops amux, moves what still runs aside to a fresh `.old.<n>`, and
    /// installs; the next install sweeps the copy away once it has exited.
    /// A copy of ping.exe stands in for a running amux.
    #[test]
    fn a_running_amux_is_moved_aside_and_swept_once_it_has_exited() {
        let feed = honest();
        let output = install(&feed, None);
        assert!(output.status.success(), "{}", said(&output));
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        std::fs::copy(system.join("PING.EXE"), installed(&feed)).unwrap();
        let running = Running(
            Command::new(installed(&feed))
                .args(["-n", "120", "127.0.0.1"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );

        let output = install(&feed, None);
        let text = said(&output);
        assert!(output.status.success(), "{text}");
        assert!(text.contains("amux server stop"), "{text}");
        assert!(text.contains("moved aside to amux.exe.old.1"), "{text}");
        assert_eq!(std::fs::read(installed(&feed)).unwrap(), BINARY);
        assert_eq!(
            listing(&feed),
            ["amux-launch.exe", "amux.exe", "amux.exe.old.1"]
        );

        drop(running);
        let output = install(&feed, None);
        assert!(output.status.success(), "{}", said(&output));
        assert_eq!(listing(&feed), ["amux-launch.exe", "amux.exe"]);
    }
}
