//! The runtime's own log, in a process of its own: the log is the
//! process's one global trace subscriber, which a test net started first
//! in the same process would own instead, leaving the log empty.

#![cfg(unix)]

use std::sync::{Arc, Condvar, Mutex};

use app_embedded::{Cores, EdgeOverrides, EmbeddedRuntime, ProfileId, StartConfig};
use app_runtime::{AppRuntime, Wake};
use client::SystemClock;

fn config(dir: &std::path::Path) -> StartConfig {
    StartConfig {
        data_dir: dir.join("data"),
        device_name: "phone".into(),
        log_path: None,
        discovery_scope: String::new(),
        lan: true,
        lan_bind: None,
        relay_tcp: None,
        relay_quic: None,
        relay_root: None,
        tail: 50,
        telemetry: true,
    }
}

/// The app's queue, as a thread of the test's: the cores step there,
/// whenever they ring, and nowhere else.
fn queue() -> Cores {
    let rang = Arc::new((Mutex::new(false), Condvar::new()));
    let ringing = rang.clone();
    let cores = Cores::new(move || {
        *ringing.0.lock().unwrap() = true;
        ringing.1.notify_all();
    });
    let stepping = cores.clone();
    std::thread::Builder::new()
        .name("app-queue".to_owned())
        .spawn(move || {
            loop {
                let mut rung = rang
                    .1
                    .wait_while(rang.0.lock().unwrap(), |rung| !*rung)
                    .unwrap();
                *rung = false;
                drop(rung);
                stepping.step();
            }
        })
        .unwrap();
    cores
}

fn loopback() -> EdgeOverrides {
    EdgeOverrides {
        lan_bind: ([127, 0, 0, 1], 0).into(),
        ..EdgeOverrides::default()
    }
}

async fn app(embedded: &EmbeddedRuntime, profile: ProfileId) -> AppRuntime {
    let wake = Arc::new(|_: Wake| {});
    AppRuntime::open(
        embedded.client(profile).unwrap(),
        Arc::new(SystemClock),
        embedded.host_id(profile).unwrap(),
        wake,
    )
    .await
    .unwrap()
}

/// The one profile a fresh installation has.
async fn only_profile(embedded: &EmbeddedRuntime) -> ProfileId {
    let profiles = embedded.profiles().unwrap();
    assert_eq!(profiles.len(), 1, "{profiles:?}");
    profiles[0].id.parse().unwrap()
}

/// The runtime traces into the log the app names, and a dump carries it as
/// the daemon's log, redacted.
#[tokio::test(flavor = "multi_thread")]
async fn a_phone_dump_carries_the_runtimes_log_redacted() {
    const KEY: &str = "sk-ant-api03-PLANTEDphonelogkey0001";
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("runtime.log");
    let embedded = EmbeddedRuntime::start_with(
        &StartConfig {
            log_path: Some(log.clone()),
            ..config(dir.path())
        },
        loopback(),
        Arc::new(SystemClock),
        queue(),
    )
    .await
    .unwrap();
    tracing::warn!("a relay call failed: Authorization: Bearer {KEY}");
    let profile = only_profile(&embedded).await;
    let app = app(&embedded, profile).await;

    let bundle = app.dump("from the phone").await.unwrap();
    let carried = std::fs::read_to_string(bundle.join(node::DAEMON_LOG)).unwrap();
    assert!(carried.contains("a relay call failed"), "{carried}");
    assert!(!carried.contains(KEY), "{carried}");
    assert!(
        std::fs::read_to_string(&log).unwrap().contains(KEY),
        "the log itself is what the runtime wrote"
    );
    drop(app);
    embedded.shutdown().await.unwrap();
}
