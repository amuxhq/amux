//! The install scripts against a manifest the release crate serialized.
//!
//! `scripts/install/install.sh` and `install.ps1` are what
//! `https://amux.sh/install` hands a new machine. They parse the channel
//! manifest by hand, so these cases serve them one written exactly as a
//! deploy writes it, from a loopback server standing in for amux.sh and for
//! the release's assets, and hold what they install: this machine's entry,
//! only when the download's size and sha256 are the entry's.

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
    /// the stand-in binary.
    fn serving(write: impl FnOnce(&str) -> String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let binary_url = format!("{base}/assets/amux");
        serve(
            vec![
                (
                    "/releases/stable.json".to_owned(),
                    write(&binary_url).into_bytes(),
                ),
                ("/assets/amux".to_owned(), BINARY.to_vec()),
            ],
            listener,
        );
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
    use super::*;

    /// The user PATH lives in the registry, which is not this test's to
    /// write: every run asks the script to leave it alone.
    fn install(feed: &Feed) -> Output {
        Command::new("powershell")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(script("install.ps1"))
            .env("USERPROFILE", feed.home.path())
            .env("AMUX_RELEASES_URL", &feed.releases_url)
            .env("AMUX_NO_MODIFY_PATH", "1")
            .output()
            .unwrap()
    }

    fn installed(feed: &Feed) -> PathBuf {
        feed.home.path().join(".amux").join("bin").join("amux.exe")
    }

    #[test]
    fn it_installs_the_stable_manifests_build_for_this_machine() {
        let feed = Feed::honest();
        let output = install(&feed);
        assert!(output.status.success(), "{}", said(&output));
        assert_eq!(std::fs::read(installed(&feed)).unwrap(), BINARY);
        assert!(
            said(&output).contains("amux 9.9.9 has been installed"),
            "{}",
            said(&output)
        );
    }

    #[test]
    fn it_refuses_a_download_whose_sha256_is_not_the_manifests() {
        let feed = Feed::deployed(TARGETS, &sha256(b"another binary"), BINARY.len() as u64);
        let output = install(&feed);
        assert!(!output.status.success(), "{}", said(&output));
        assert!(!installed(&feed).exists(), "nothing is installed");
    }

    #[test]
    fn it_says_so_when_the_channel_has_no_build_for_this_machine() {
        let feed = Feed::deployed(
            &["riscv64gc-unknown-linux-gnu"],
            &sha256(BINARY),
            BINARY.len() as u64,
        );
        let output = install(&feed);
        assert!(!output.status.success(), "{}", said(&output));
        assert!(
            said(&output).contains("has no build for"),
            "{}",
            said(&output)
        );
        assert!(!installed(&feed).exists(), "nothing is installed");
    }
}
