//! The installer against the built amux binary, in a home of its own: a
//! bundled copy is installed, set up and started; a download is believed
//! only once its manifest's signature and its own hash check out; and an
//! installation already answering is left alone.
//!
//! Each case points `HOME` at its own directory, which the amux it starts
//! inherits; nextest runs every case in a process of its own.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::PathBuf;

use installer::{Error, Options, Outcome, Source, Step};
use release::{Manifest, Release};

struct Home {
    dir: tempfile::TempDir,
    config: PathBuf,
}

impl Home {
    /// A home whose installation has no supervisor, so starting it starts
    /// a daemon that the case stops, and no login item is written.
    fn new(releases_url: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: each case runs in its own process, before any thread
        // that reads the environment.
        unsafe {
            std::env::set_var("HOME", dir.path());
            std::env::set_var("AMUX_TEST_DISCOVERY_MODE", "disabled");
            std::env::remove_var("AMUX_CONFIG");
        }
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            format!(
                "root = {:?}\nsupervisor = false\nkeep_awake = false\ntelemetry = false\n\
                 releases_url = {releases_url:?}\n",
                dir.path().join("data").display().to_string(),
            ),
        )
        .unwrap();
        Self { dir, config }
    }

    fn options(&self, bundled: Option<PathBuf>) -> Options {
        Options {
            bundled,
            login_item: false,
            config: Some(self.config.clone()),
        }
    }

    fn installed(&self) -> PathBuf {
        self.dir.path().join(".amux/bin/amux")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::process::Command::new(self.installed())
            .args(["--config"])
            .arg(&self.config)
            .args(["server", "stop"])
            .output();
    }
}

fn amux() -> PathBuf {
    provider_fakes::binaries::path("amux")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bundled_copy_is_installed_set_up_and_answers_and_then_is_left_alone() {
    let home = Home::new("http://127.0.0.1:9/releases");
    let mut steps = Vec::new();
    let outcome = installer::ensure(&home.options(Some(amux())), |step| steps.push(step))
        .await
        .unwrap();
    let Outcome::Installed { version, source } = outcome else {
        panic!("installed, not {outcome:?}");
    };
    assert_eq!(source, Source::Bundled);
    let reported = std::process::Command::new(amux())
        .arg("--version")
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&reported.stdout)
            .trim()
            .ends_with(&version)
    );
    assert_eq!(steps, [Step::Installing(Source::Bundled), Step::Starting]);
    assert!(home.installed().is_file());
    let door = installer::front_door(Some(&home.config)).unwrap();
    assert!(door.ends_with("data/amux.sock"), "{}", door.display());
    assert!(installer::answering(&door).await);

    // Already answering: nothing is touched.
    let again = installer::ensure(&home.options(Some(amux())), |_| panic!("no steps"))
        .await
        .unwrap();
    assert_eq!(again, Outcome::Running);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_is_installed_only_once_its_signature_and_hash_check_out() {
    let binary = std::fs::read(amux()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let honest = manifest(&base, "/amux", &binary, "9.9.9");
    let mut forged = honest.clone();
    forged.targets.get_mut(release::TARGET).unwrap().version = "9.9.10".into();
    serve(
        vec![
            (
                "/good/stable.json".into(),
                serde_json::to_vec(&honest).unwrap(),
            ),
            (
                "/forged/stable.json".into(),
                serde_json::to_vec(&forged).unwrap(),
            ),
            (
                "/swapped/stable.json".into(),
                serde_json::to_vec(&manifest(&base, "/other", &binary, "9.9.9")).unwrap(),
            ),
            ("/amux".into(), binary.clone()),
            ("/other".into(), b"#!/bin/sh\necho not amux\n".to_vec()),
        ],
        listener,
    );

    // A manifest whose words changed after signing is refused whole.
    let home = Home::new(&format!("{base}/forged"));
    let refused = installer::ensure(&home.options(None), |_| {}).await;
    assert!(
        matches!(refused, Err(Error::Verify(release::VerifyError::Signature))),
        "{refused:?}"
    );
    assert!(!home.installed().exists());
    drop(home);

    // A build that is not the one the manifest hashed is not placed.
    let home = Home::new(&format!("{base}/swapped"));
    let refused = installer::ensure(&home.options(None), |_| {}).await;
    assert!(
        matches!(
            refused,
            Err(Error::Verify(release::VerifyError::Size { .. }))
        ),
        "{refused:?}"
    );
    assert!(!home.installed().exists());
    let leftovers: Vec<_> = std::fs::read_dir(home.dir.path().join(".amux/bin"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name != ".install.lock")
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    drop(home);

    let home = Home::new(&format!("{base}/good"));
    let mut received = 0;
    let outcome = installer::ensure(&home.options(None), |step| {
        if let Step::Downloading {
            received: now,
            total,
        } = step
        {
            assert!(now <= total);
            received = now;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        outcome,
        Outcome::Installed {
            version: "9.9.9".into(),
            source: Source::Downloaded
        }
    );
    assert_eq!(received, binary.len() as u64);
    assert_eq!(std::fs::read(home.installed()).unwrap(), binary);
    assert!(installer::answering(&installer::front_door(Some(&home.config)).unwrap()).await);
}

/// The Mac app's feed: a newer app is downloaded, checked and unpacked
/// beside the running one, a current one is left alone, and an app that is
/// not the one the manifest hashed is never staged.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread")]
async fn the_mac_app_stages_a_newer_version_only_once_it_checks_out() {
    use installer::update::{self, Staged};

    let work = tempfile::tempdir().unwrap();
    let contents = work.path().join("Amux.app/Contents");
    std::fs::create_dir_all(&contents).unwrap();
    std::fs::write(contents.join("Info.plist"), "<plist/>").unwrap();
    let zip = work.path().join("Amux.zip");
    let zipped = std::process::Command::new("ditto")
        .args(["-c", "-k", "--keepParent"])
        .arg(work.path().join("Amux.app"))
        .arg(&zip)
        .status()
        .unwrap();
    assert!(zipped.success());
    let zip = std::fs::read(zip).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = |path: &str| {
        let mut signed = manifest(&base, path, &zip, "0.0.2");
        let entry = signed.targets.remove(release::TARGET).unwrap();
        signed.targets.insert(update::TARGET.to_owned(), entry);
        signed.signature = release::sign(&release::TEST_SEED, &signed);
        serde_json::to_vec(&signed).unwrap()
    };
    serve(
        vec![
            ("/good/mac-stable.json".into(), app("/Amux.zip")),
            ("/swapped/mac-stable.json".into(), app("/other.zip")),
            ("/Amux.zip".into(), zip.clone()),
            ("/other.zip".into(), b"PK not the app".to_vec()),
        ],
        listener,
    );
    let options = |home: &Home, current: &str| update::Options {
        config: Some(home.config.clone()),
        current: current.into(),
        dir: home.dir.path().join("Updates"),
    };

    let home = Home::new(&format!("{base}/swapped"));
    let refused = update::check(&options(&home, "0.0.1")).await;
    assert!(matches!(refused, Err(Error::Verify(_))), "{refused:?}");
    let left: Vec<_> = std::fs::read_dir(home.dir.path().join("Updates"))
        .unwrap()
        .collect();
    assert!(left.is_empty(), "{left:?}");
    drop(home);

    let home = Home::new(&format!("{base}/good"));
    let staged = update::check(&options(&home, "0.0.1")).await.unwrap();
    let app = home.dir.path().join("Updates/0.0.2/Amux.app");
    assert_eq!(
        staged,
        Some(Staged {
            version: "0.0.2".into(),
            app: app.clone(),
        })
    );
    assert!(app.join("Contents/Info.plist").is_file());
    assert!(!home.dir.path().join("Updates/0.0.2.zip").exists());
    // Asked again, the staged app is answered as it is.
    assert_eq!(
        update::check(&options(&home, "0.0.1")).await.unwrap(),
        staged
    );
    // A version staged from another feed is replaced, not taken for this one.
    let ready = home.dir.path().join("Updates/0.0.2/ready");
    std::fs::write(&ready, "staged from a test feed").unwrap();
    assert_eq!(
        update::check(&options(&home, "0.0.1")).await.unwrap(),
        staged
    );
    assert_eq!(
        std::fs::read_to_string(&ready).unwrap(),
        release::sha256_of(&zip)
    );
    // Once it runs, what was staged goes.
    assert_eq!(update::check(&options(&home, "0.0.2")).await.unwrap(), None);
    assert!(!home.dir.path().join("Updates/0.0.2").exists());
}

/// The stable channel naming `path` on `base` for this target, signed with
/// the key debug builds trust.
fn manifest(base: &str, path: &str, binary: &[u8], version: &str) -> Manifest {
    let mut manifest = Manifest {
        channel: "stable".into(),
        rollout: None,
        targets: BTreeMap::from([(
            release::TARGET.to_owned(),
            Release {
                version: version.into(),
                url: format!("{base}{path}"),
                sha256: release::sha256_of(binary),
                size: binary.len() as u64,
            },
        )]),
        signature: String::new(),
    };
    manifest.signature = release::sign(&release::TEST_SEED, &manifest);
    manifest
}

/// A loopback server answering each path with its body and anything else
/// with 404, for as long as the test process lives.
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
            let (status, body) = match routes.iter().find(|(route, _)| route == path) {
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
