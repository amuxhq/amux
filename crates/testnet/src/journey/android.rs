//! The Android journey driver: starts a served topology with `testnet serve`,
//! installs the debug APK fresh on the pinned Android emulator (`amux-android-1`),
//! forwards the debug door over `adb forward`, launches `MainActivity` with the
//! served network's configuration, drives the real app through its Compose
//! semantics tree via the NDJSON door, checks host/agent/provider records, and
//! compares element geometry against `journeys/goldens/android/<story>/<label>.elements.txt`.
//!
//! The stories are the phone's shared ones (`phone_stories::shared`), told
//! through this driver's [`Driver`] impl; which of the iPhone's own stories
//! Android leaves out, and why, is said there.
//!
//! Each compared screen is also photographed into `target/journeys/android/<story>/actual/`
//! beside its geometry for human inspection; `UPDATE_JOURNEY_GOLDENS=1` rewrites
//! geometry goldens that differ (refused in CI).

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead as _, BufReader};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::{LazyLock, OnceLock, mpsc};
use std::time::{Duration, Instant};

use base64::Engine as _;
use regex::Regex;
use serde_json::{Value, json};

use super::driver::{Client, Driver, Tab};
use super::phone::{AppError, Launch, ask, normalize, run_within};
use super::phone_stories::{Tell, shared};
use super::{Record, door, line_diff, root};
use crate::Binaries;
use crate::door::{Control, Readiness};

const PACKAGE_ID: &str = "sh.amux.android";
const ACTIVITY: &str = "sh.amux.android/.MainActivity";
const PINNED_AVD: &str = "amux-android-1";
const DEVICE_DOOR_PORT: u16 = 19876;
const EMULATOR_HOST_IP: &str = "10.0.2.2";
const SCRATCH_DIR: &str = "/tmp/amux-android-journey";
const SCRATCH_LOCK: &str = "/tmp/amux-android-journey.lock";
const RUST_LOG: &str = "info,node::link=debug,node::services::reachability=debug,node::routing=debug,node::sources=debug,node::dispatcher=debug,node::edge::peer=debug";

/// A surface whose name ends here holds text that moves with the run (an age,
/// a scratch path); its words and moving frame are left out of geometry checks.
const VOLATILE: &str = ".volatile";
/// This phone's own name, key, and app version, which move with the install or build.
const OWN_IDENTITY: [&str; 3] = ["you.name", "you.fingerprint", "you.version"];
/// Horizontal slack in dp allowed on lines containing masked values whose drawn
/// width follows what they said (e.g., `"Worked <t>"`, `"<age>"`).
const MASKED_SLACK: i64 = 16;

static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
        .expect("valid uuid pattern")
});
static PROMPT_HEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bprompt:[0-9a-f]{32}\b").expect("valid prompt hex pattern"));
static FAKE_ROW_RUN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(toolu_fake|msg_fake|msg_|exec-|call_)[0-9a-f]{8}([0-9a-f]{16})\b")
        .expect("valid fake row id pattern")
});
static MASKED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<(?:age|key|device|t|time|id)>").expect("valid masked pattern"));

/// The stories the Android app tells: every shared phone story.
fn stories() -> Vec<(&'static str, Tell<AndroidJourney>)> {
    shared::<AndroidJourney>()
}

/// Runs the named Android journey stories, or every shared one.
pub(crate) fn run(named: Vec<String>) -> ExitCode {
    let wanted = match selected(named) {
        Ok(wanted) => wanted,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let apk = debug_apk();
    if !apk.is_file() {
        eprintln!(
            "debug APK not found at {:?}; `just android journey` builds it and names it in AMUX_ANDROID_APK",
            apk
        );
        return ExitCode::FAILURE;
    }
    let adb = match find_adb() {
        Ok(adb) => adb,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let serial = match boot_emulator() {
        Ok(serial) => serial,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    let mut failed = 0;
    for name in &wanted {
        let (_, tell) = stories()
            .into_iter()
            .find(|(id, _)| id == name)
            .expect("known");
        let began = Instant::now();
        let mut journey = match android_story(name)
            .and_then(|topology| AndroidJourney::start(name, &topology, &apk, &adb, &serial))
        {
            Ok(journey) => journey,
            Err(error) => {
                eprintln!("FAIL {name}: {error}");
                failed += 1;
                continue;
            }
        };
        let told = tell(&mut journey);
        match &told {
            Ok(proved) => journey.finish(proved),
            Err(error) => journey.fail(error),
        }
        let closed = journey.close();
        match (told, closed) {
            (Ok(proved), Ok(())) => {
                for line in proved {
                    println!("- {line}");
                }
                println!("{name}: journey took {:.1}s", began.elapsed().as_secs_f64());
                println!("{name}: passed");
            }
            (Err(error), _) | (Ok(_), Err(error)) => {
                eprintln!("FAIL {name}: {error}");
                failed += 1;
            }
        }
    }
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn selected(named: Vec<String>) -> Result<Vec<String>, String> {
    let wanted = if named.is_empty() {
        stories().iter().map(|(id, _)| (*id).to_owned()).collect()
    } else {
        named
    };
    let unknown: Vec<&str> = wanted
        .iter()
        .filter(|name| !stories().iter().any(|(id, _)| id == name))
        .map(String::as_str)
        .collect();
    if unknown.is_empty() {
        Ok(wanted)
    } else {
        let known: Vec<&str> = stories().iter().map(|(id, _)| *id).collect();
        Err(format!(
            "no android story written for {}; one of {}",
            unknown.join(", "),
            known.join(", ")
        ))
    }
}

fn android_story(name: &str) -> Result<PathBuf, String> {
    let story = super::manifest()?
        .into_iter()
        .find(|story| story.id == name)
        .ok_or_else(|| format!("no journey named {name:?} in journeys/manifest.json"))?;
    let declared = story
        .phone_topology
        .as_deref()
        .or(story.topology.as_deref())
        .ok_or_else(|| format!("journey {name:?} names no topology"))?;
    Ok(root().join(declared))
}

/// The debug APK Buck2 built (`//apps/android/app:debug`), which
/// `just android journey` names in `AMUX_ANDROID_APK`.
fn debug_apk() -> PathBuf {
    std::env::var_os("AMUX_ANDROID_APK")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn find_adb() -> Result<PathBuf, String> {
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Ok(sdk) = std::env::var(var) {
            let candidate = PathBuf::from(sdk).join("platform-tools/adb");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        for sub in [
            "Library/Android/sdk/platform-tools/adb",
            "Android/Sdk/platform-tools/adb",
        ] {
            let candidate = PathBuf::from(&home).join(sub);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Ok(PathBuf::from("adb"))
}

fn boot_emulator() -> Result<String, String> {
    let mut command = Command::new(root().join("scripts/py"));
    command
        .args(["-B", "apps/android/tools/android-emulator.py"])
        .current_dir(root());
    let said = run_within(command, Duration::from_secs(300))?;
    parse_emulator_serial(&said)
        .ok_or_else(|| format!("android-emulator.py named no serial for {PINNED_AVD}: {said}"))
}

fn parse_emulator_serial(output: &str) -> Option<String> {
    let prefix = format!("android ({PINNED_AVD}): ");
    output.lines().find_map(|line| {
        let rest = line.trim().strip_prefix(&prefix)?;
        let serial = rest.split_whitespace().next()?;
        Some(serial.to_owned())
    })
}

fn free_port() -> Result<u16, String> {
    let probe = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    probe
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| error.to_string())
}

fn rewrite_for_emulator(addr: SocketAddr) -> String {
    if addr.ip().is_loopback() {
        format!("{EMULATOR_HOST_IP}:{}", addr.port())
    } else {
        addr.to_string()
    }
}

fn rewrite_url_for_emulator(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let (authority, tail) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    if let Ok(addr) = authority.parse::<SocketAddr>() {
        return format!("{scheme}://{}{tail}", rewrite_for_emulator(addr));
    }
    if let Some(port) = authority.strip_prefix("localhost:") {
        return format!("{scheme}://{EMULATOR_HOST_IP}:{port}{tail}");
    }
    if authority == "localhost" || authority == "127.0.0.1" {
        return format!("{scheme}://{EMULATOR_HOST_IP}{tail}");
    }
    url.to_owned()
}

fn shell_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', "'\\''"))
}

fn rust_log() -> String {
    std::env::var("RUST_LOG").unwrap_or_else(|_| RUST_LOG.to_owned())
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}

/// Replaces any occurrence of `token` in `text` with `<redacted>` so errors and
/// logs never leak the per-launch debug door secret.
fn redact_token(text: &str, token: &str) -> String {
    if token.is_empty() {
        text.to_owned()
    } else {
        text.replace(token, "<redacted>")
    }
}

fn redact_app_error(error: AppError, token: &str) -> AppError {
    match error {
        AppError::Refused(msg) => AppError::Refused(redact_token(&msg, token)),
        AppError::Unreachable(msg) => AppError::Unreachable(redact_token(&msg, token)),
    }
}

fn loopback_port(url: &str) -> Option<u16> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split('/').next()?;
    if let Ok(addr) = authority.parse::<SocketAddr>() {
        return addr.ip().is_loopback().then_some(addr.port());
    }
    authority
        .strip_prefix("localhost:")
        .and_then(|port| port.parse().ok())
}

#[derive(Clone)]
struct UndoState {
    adb: PathBuf,
    serial: String,
    host_port: u16,
    reversed_cloud_port: u16,
    control: SocketAddr,
}

static UNDO: OnceLock<std::sync::Mutex<Option<UndoState>>> = OnceLock::new();

fn undo_on_terminate() {
    let undo = UNDO.get_or_init(|| std::sync::Mutex::new(None));
    static WATCHING: OnceLock<()> = OnceLock::new();
    WATCHING.get_or_init(|| {
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async {
                if let Ok(mut terminate) =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                {
                    terminate.recv().await;
                }
            });
            if let Some(state) = undo.lock().ok().and_then(|held| held.clone()) {
                let _ = Command::new(&state.adb)
                    .args(["-s", &state.serial, "shell", "am", "force-stop", PACKAGE_ID])
                    .output();
                if state.host_port != 0 {
                    let _ = Command::new(&state.adb)
                        .args([
                            "-s",
                            &state.serial,
                            "forward",
                            "--remove",
                            &format!("tcp:{}", state.host_port),
                        ])
                        .output();
                }
                if state.reversed_cloud_port != 0 {
                    let _ = Command::new(&state.adb)
                        .args([
                            "-s",
                            &state.serial,
                            "reverse",
                            "--remove",
                            &format!("tcp:{}", state.reversed_cloud_port),
                        ])
                        .output();
                }
                let _ = door(state.control, &Control::Shutdown, Duration::from_secs(10));
            }
            std::process::exit(143);
        });
    });
}

struct AndroidJourney {
    adb: PathBuf,
    serial: String,
    apk: PathBuf,
    output: PathBuf,
    goldens: PathBuf,
    update: bool,
    topology: Value,
    binaries: Binaries,
    _lock: File,
    process: Option<Child>,
    ready: Readiness,
    record: Record,
    host_port: u16,
    reversed_cloud_port: u16,
    door_token: String,
    extra_ids: BTreeMap<String, String>,
}

impl AndroidJourney {
    fn start(
        story_id: &str,
        topology_path: &Path,
        apk: &Path,
        adb: &Path,
        serial: &str,
    ) -> Result<Self, String> {
        let root = root();
        let named = |error: &dyn std::fmt::Display| format!("{}: {error}", topology_path.display());
        let topology: Value = serde_json::from_str(
            &std::fs::read_to_string(topology_path).map_err(|error| named(&error))?,
        )
        .map_err(|error| named(&error))?;
        let output = root.join("target/journeys/android").join(story_id);
        let goldens = root.join("journeys/goldens/android").join(story_id);
        let _ = std::fs::remove_dir_all(&output);
        std::fs::create_dir_all(&output).map_err(|error| error.to_string())?;
        let lock = File::create(SCRATCH_LOCK).map_err(|error| error.to_string())?;
        lock.lock().map_err(|error| error.to_string())?;
        let scratch = Path::new(SCRATCH_DIR);
        let _ = std::fs::remove_dir_all(scratch);
        std::fs::create_dir_all(scratch).map_err(|error| error.to_string())?;

        let binaries = Binaries::for_driver()?;
        let testnet = std::env::current_exe().map_err(|error| error.to_string())?;
        let mut command = Command::new(&testnet);
        command
            .arg("serve")
            .arg(topology_path)
            .arg("--root-in")
            .arg(scratch)
            .current_dir(&root)
            .env_remove("AMUX_LOG")
            .env_remove("AMUX_CONFIG")
            .env("TMPDIR", scratch)
            .env("TMP", scratch)
            .env("TEMP", scratch)
            .env("AMUX_TEST_DISCOVERY_MODE", "disabled")
            .env("RUST_LOG", rust_log())
            .env(crate::PREBUILT_ENV, binaries.dir())
            .stdout(Stdio::piped())
            .stderr(File::create(output.join("testnet.log")).map_err(|error| error.to_string())?);

        let mut process = command
            .spawn()
            .map_err(|error| format!("testnet serve: {error}"))?;
        let stdout = process.stdout.take().expect("piped");
        let (sent, readiness) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            let _ = sent.send(line);
        });
        let ready = match readiness.recv_timeout(Duration::from_secs(120)) {
            Ok(line) if !line.is_empty() => serde_json::from_str::<Readiness>(&line)
                .map_err(|error| format!("readiness: {error}")),
            Ok(_) => Err("testnet exited before readiness".to_owned()),
            Err(_) => Err("testnet readiness timed out".to_owned()),
        };
        let ready = match ready {
            Ok(ready) => ready,
            Err(error) => {
                let _ = process.kill();
                let _ = process.wait();
                return Err(error);
            }
        };

        undo_on_terminate();
        if let Some(undo) = UNDO.get()
            && let Ok(mut held) = undo.lock()
        {
            *held = Some(UndoState {
                adb: adb.to_path_buf(),
                serial: serial.to_owned(),
                host_port: 0,
                reversed_cloud_port: 0,
                control: ready.control,
            });
        }

        let mut record = Record::default();
        record
            .actions
            .push(format!("ready control={} serial={serial}", ready.control));

        Ok(Self {
            adb: adb.to_path_buf(),
            serial: serial.to_owned(),
            apk: apk.to_path_buf(),
            output,
            goldens,
            update: std::env::var("UPDATE_JOURNEY_GOLDENS").is_ok_and(|value| value == "1"),
            topology,
            binaries,
            _lock: lock,
            process: Some(process),
            ready,
            record,
            host_port: 0,
            reversed_cloud_port: 0,
            door_token: String::new(),
            extra_ids: BTreeMap::new(),
        })
    }

    /// The port a served machine's LAN listener is on, which a phone on its
    /// network dials.
    fn host_lan_port(&self, host: &str) -> Result<u16, String> {
        self.ready
            .hosts
            .iter()
            .find(|ready| ready.name == host)
            .and_then(|ready| ready.lan_port)
            .ok_or_else(|| format!("host {host:?} has no lan_port in readiness"))
    }

    // --- the emulator & Android app --------------------------------------

    fn adb(&self, args: &[&str], timeout: Duration) -> Result<String, String> {
        let mut command = Command::new(&self.adb);
        command.args(["-s", &self.serial]).args(args);
        run_within(command, timeout).map_err(|error| redact_token(&error, &self.door_token))
    }

    fn launch_fresh(&mut self) -> Result<(), String> {
        let _ = self.adb(
            &["shell", "am", "force-stop", PACKAGE_ID],
            Duration::from_secs(30),
        );
        self.adb(
            &["install", "-r", "-t", self.apk.to_string_lossy().as_ref()],
            Duration::from_secs(180),
        )?;
        self.adb(
            &["shell", "pm", "clear", PACKAGE_ID],
            Duration::from_secs(30),
        )?;
        self.launch_activity(true)
    }

    fn relaunch(&mut self) -> Result<(), String> {
        let _ = self.adb(
            &["shell", "am", "force-stop", PACKAGE_ID],
            Duration::from_secs(30),
        );
        self.launch_activity(false)
    }

    fn quit(&mut self) -> Result<(), String> {
        self.adb(
            &["shell", "am", "force-stop", PACKAGE_ID],
            Duration::from_secs(30),
        )?;
        self.record.actions.push("quit the Android app".to_owned());
        Ok(())
    }

    fn launch_activity(&mut self, fresh_start: bool) -> Result<(), String> {
        if self.host_port != 0 {
            let _ = self.adb(
                &["forward", "--remove", &format!("tcp:{}", self.host_port)],
                Duration::from_secs(15),
            );
        }
        self.host_port = free_port()?;
        self.adb(
            &[
                "forward",
                &format!("tcp:{}", self.host_port),
                &format!("tcp:{DEVICE_DOOR_PORT}"),
            ],
            Duration::from_secs(30),
        )?;
        if let Some(cloud_port) = self.ready.cloud_url.as_deref().and_then(loopback_port)
            && self.reversed_cloud_port != cloud_port
        {
            if self.reversed_cloud_port != 0 {
                let _ = self.adb(
                    &[
                        "reverse",
                        "--remove",
                        &format!("tcp:{}", self.reversed_cloud_port),
                    ],
                    Duration::from_secs(15),
                );
                self.reversed_cloud_port = 0;
            }
            self.adb(
                &[
                    "reverse",
                    &format!("tcp:{cloud_port}"),
                    &format!("tcp:{cloud_port}"),
                ],
                Duration::from_secs(30),
            )?;
            self.reversed_cloud_port = cloud_port;
        }
        if let Some(undo) = UNDO.get()
            && let Ok(mut held) = undo.lock()
        {
            *held = Some(UndoState {
                adb: self.adb.clone(),
                serial: self.serial.clone(),
                host_port: self.host_port,
                reversed_cloud_port: self.reversed_cloud_port,
                control: self.ready.control,
            });
        }

        let routed = self.topology["hosts"]
            .as_array()
            .is_some_and(|hosts| hosts.iter().any(|host| truthy(&host["router"])));
        let relay_quic_override = if routed {
            let reply = self.request(Control::RelayGate {}, None)?;
            reply["addr"]
                .as_str()
                .and_then(|raw| raw.parse::<SocketAddr>().ok())
                .map(rewrite_for_emulator)
        } else {
            None
        };

        let scope = self.topology["scope"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        self.door_token = uuid::Uuid::new_v4().to_string();
        let start_config = build_start_config(
            &self.ready,
            &scope,
            fresh_start,
            relay_quic_override.as_deref(),
        );
        let start_config_json =
            serde_json::to_string(&start_config).map_err(|error| error.to_string())?;
        let launch_cmd = format!(
            "am start -W -n {ACTIVITY} --ei amux_door_port {DEVICE_DOOR_PORT} --es amux_door_token {} --es amux_start_config {}",
            shell_quote(&self.door_token),
            shell_quote(&start_config_json)
        );
        self.adb(&["shell", &launch_cmd], Duration::from_secs(60))?;
        self.record.actions.push(format!(
            "launched {ACTIVITY} (fresh={fresh_start}) with door port {}",
            self.host_port
        ));

        self.await_door()?;
        self.app(json!({"kind": "awaitReconciled", "seconds": 30.0}))?;
        self.record
            .actions
            .push("embedded runtime reconciled".to_owned());
        Ok(())
    }

    fn await_door(&self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if self.app(json!({"kind": "ready"})).is_ok() {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err("the Android app's door did not open within a minute".to_owned());
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    }

    fn door(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.host_port))
    }

    // --- screens & geometry goldens --------------------------------------

    fn screen(&mut self, label: &str, volatile: &[&str]) -> Result<(), String> {
        self.app(json!({"kind": "settle"}))?;
        let state = self.query()?;
        let elements = state["elements"].as_array().map_or(&[][..], Vec::as_slice);
        validate_element_frames(elements)?;

        let actual = self.output.join("actual");
        std::fs::create_dir_all(&actual).map_err(|error| error.to_string())?;
        let png_path = actual.join(format!("{label}.png"));
        self.capture_png(&png_path)?;

        let named: Vec<&str> = volatile.iter().copied().chain(OWN_IDENTITY).collect();
        let mut ids: BTreeMap<String, String> = self
            .ready
            .hosts
            .iter()
            .map(|host| (host.host_id.to_string(), host.name.clone()))
            .collect();
        ids.extend(
            self.ready
                .agents
                .iter()
                .map(|agent| (agent.id.to_string(), agent.name.clone())),
        );
        ids.extend(self.extra_ids.clone());

        let drawn = format_elements(elements, &named, &ids);
        std::fs::write(actual.join(format!("{label}.elements.txt")), &drawn)
            .map_err(|error| error.to_string())?;
        self.record.actions.push(format!("captured {label}"));

        if self.update && std::env::var_os("CI").is_some() {
            return Err("rewriting goldens is refused in CI".to_owned());
        }
        let expected = self.goldens.join(format!("{label}.elements.txt"));
        let differs = if !expected.exists() {
            Some(format!(
                "missing golden {}; review a rewritten one",
                expected.display()
            ))
        } else {
            let approved = std::fs::read_to_string(&expected).map_err(|error| error.to_string())?;
            (!same_geometry(&approved, &drawn)).then(|| {
                format!(
                    "geometry differs: {}\n{}",
                    expected.display(),
                    line_diff(&approved, &drawn)
                )
            })
        };
        match differs {
            Some(differs) if self.update => {
                std::fs::create_dir_all(&self.goldens).map_err(|error| error.to_string())?;
                std::fs::write(&expected, &drawn).map_err(|error| error.to_string())?;
                self.record.actions.push(format!(
                    "rewrote {label}: {}",
                    differs.lines().next().unwrap_or_default()
                ));
                Ok(())
            }
            Some(differs) => Err(differs),
            None => Ok(()),
        }
    }

    fn capture_png(&self, path: &Path) -> Result<(), String> {
        if let Ok(reply) = self.app(json!({"kind": "capture"}))
            && let Some(encoded) = reply["base64"].as_str()
            && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded)
            && !bytes.is_empty()
        {
            return std::fs::write(path, bytes).map_err(|error| error.to_string());
        }
        self.adb_screencap(path)
    }

    fn adb_screencap(&self, path: &Path) -> Result<(), String> {
        let output = Command::new(&self.adb)
            .args(["-s", &self.serial, "exec-out", "screencap", "-p"])
            .output()
            .map_err(|error| format!("adb screencap: {error}"))?;
        if !output.status.success() || output.stdout.is_empty() {
            return Err(format!(
                "adb screencap failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        std::fs::write(path, &output.stdout).map_err(|error| error.to_string())
    }

    fn finish(&self, proved: &[String]) {
        let lines: String = proved.iter().map(|line| format!("- {line}\n")).collect();
        self.record.write(&self.output, &format!("PASS\n{lines}"));
    }

    fn fail(&mut self, error: &str) {
        let failed = self.output.join("failed.png");
        if let Err(missed) = self.adb_screencap(&failed) {
            self.record
                .actions
                .push(format!("no picture of the failure: {missed}"));
        }
        if let Ok(logcat) = self.adb(&["logcat", "-d", "-t", "200"], Duration::from_secs(15)) {
            let scrubbed = redact_token(&logcat, &self.door_token);
            let _ = std::fs::write(self.output.join("logcat.txt"), scrubbed);
        }
        self.record
            .write(&self.output, &format!("FAIL\n- {error}\n"));
    }

    fn close(mut self) -> Result<(), String> {
        let _ = self.adb(
            &["shell", "am", "force-stop", PACKAGE_ID],
            Duration::from_secs(30),
        );
        if self.host_port != 0 {
            let _ = self.adb(
                &["forward", "--remove", &format!("tcp:{}", self.host_port)],
                Duration::from_secs(15),
            );
        }
        if self.reversed_cloud_port != 0 {
            let _ = self.adb(
                &[
                    "reverse",
                    "--remove",
                    &format!("tcp:{}", self.reversed_cloud_port),
                ],
                Duration::from_secs(15),
            );
        }
        let mut result = Ok(());
        if let Some(mut process) = self.process.take()
            && process.try_wait().ok().flatten().is_none()
        {
            let _ = door(
                self.ready.control,
                &Control::Shutdown,
                Duration::from_secs(10),
            );
            let deadline = Instant::now() + Duration::from_secs(60);
            while process.try_wait().ok().flatten().is_none() {
                if Instant::now() >= deadline {
                    let _ = process.kill();
                    let _ = process.wait();
                    result = Err("testnet did not shut down within a minute".to_owned());
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if let Some(undo) = UNDO.get()
            && let Ok(mut held) = undo.lock()
        {
            *held = None;
        }
        let _ = std::fs::write(
            self.output.join("actions.txt"),
            self.record.actions.join("\n") + "\n",
        );
        let _ = std::fs::remove_dir_all(SCRATCH_DIR);
        result
    }
}

fn build_start_config(
    ready: &Readiness,
    scope: &str,
    fresh_start: bool,
    relay_quic_override: Option<&str>,
) -> Value {
    let mut obj = json!({
        "device_name": PINNED_AVD,
        "discovery_scope": scope,
        "lan": true,
        "lan_bind": "0.0.0.0:0",
        "scripted_cloud": ready.cloud_url.is_some(),
        "use_system_discovery": false,
        "fresh_start": fresh_start,
        "notifications_dismissed": true,
    });
    if let Some(cloud_url) = &ready.cloud_url {
        obj["cloud_url"] = json!(rewrite_url_for_emulator(cloud_url));
    }
    if let Some(tcp) = ready.relay_tcp
        && relay_quic_override.is_none()
    {
        obj["relay_tcp"] = json!(rewrite_for_emulator(tcp));
    }
    if let Some(quic) = relay_quic_override {
        obj["relay_quic"] = json!(quic);
    } else if let Some(quic) = ready.relay_quic {
        obj["relay_quic"] = json!(rewrite_for_emulator(quic));
    }
    if let Some(root) = &ready.relay_root {
        obj["relay_root"] = json!(root);
    }
    obj
}

fn element_enabled(element: &Value) -> bool {
    match element.get("enabled") {
        None => true,
        Some(Value::Bool(enabled)) => *enabled,
        Some(Value::Null) => false,
        Some(_) => true,
    }
}

fn is_volatile(identifier: &str, named: &[&str]) -> bool {
    identifier.ends_with(VOLATILE) || named.contains(&identifier)
}

fn named_ids(text: &str, ids: &BTreeMap<String, String>) -> String {
    let text = UUID.replace_all(text, |found: &regex::Captures| {
        format!("<{}>", ids.get(&found[0]).map_or("id", String::as_str))
    });
    let text = PROMPT_HEX.replace_all(&text, "prompt:<id>");
    FAKE_ROW_RUN
        .replace_all(&text, "${1}<run>${2}")
        .into_owned()
}

fn validate_element_frames(elements: &[Value]) -> Result<(), String> {
    for element in elements {
        let name = element["identifier"].as_str().unwrap_or_default();
        let frame = &element["frame"];
        for key in ["x", "y", "width", "height"] {
            let Some(val) = frame[key].as_f64() else {
                return Err(format!("element {name:?} missing numeric frame.{key}"));
            };
            if !val.is_finite() {
                return Err(format!("element {name:?} has non-finite frame.{key}={val}"));
            }
            if (key == "width" || key == "height") && val < 0.0 {
                return Err(format!("element {name:?} has negative frame.{key}={val}"));
            }
        }
    }
    Ok(())
}

fn format_elements(
    elements: &[Value],
    volatile: &[&str],
    ids: &BTreeMap<String, String>,
) -> String {
    let mut lines = Vec::new();
    for element in elements {
        let name = element["identifier"].as_str().unwrap_or_default();
        if is_volatile(name, volatile) {
            lines.push(named_ids(&format!("{name} | <volatile>"), ids));
            continue;
        }
        let frame = &element["frame"];
        let coords: Vec<String> = ["x", "y", "width", "height"]
            .iter()
            .map(|key| {
                (frame[key].as_f64().unwrap_or_default().round_ties_even() as i64).to_string()
            })
            .collect();
        let label = element["label"]
            .as_str()
            .unwrap_or_default()
            .replace('\n', "\\n");
        let value = element["value"]
            .as_str()
            .unwrap_or_default()
            .replace('\n', "\\n");
        let words = normalize(&format!("{label} | {value}"));
        let state = if element_enabled(element) {
            ""
        } else {
            " (disabled)"
        };
        lines.push(named_ids(
            &format!("{name} | {words} | {}{state}", coords.join(",")),
            ids,
        ));
    }
    lines.join("\n") + "\n"
}

fn same_line(approved: &str, drawn: &str) -> bool {
    if approved == drawn {
        return true;
    }
    if !MASKED.is_match(approved) {
        return false;
    }
    let parts = |line: &str| -> Option<(String, String, Vec<i64>)> {
        let (words, place) = line.rsplit_once(" | ").unwrap_or(("", line));
        let (frame, state) = place.split_once(' ').unwrap_or((place, ""));
        let numbers: Vec<i64> = frame
            .split(',')
            .map(|value| value.parse().ok())
            .collect::<Option<_>>()?;
        (numbers.len() == 4).then(|| (words.to_owned(), state.to_owned(), numbers))
    };
    let (Some((words, state, frame)), Some((drawn_words, drawn_state, drawn_frame))) =
        (parts(approved), parts(drawn))
    else {
        return false;
    };
    (words, state) == (drawn_words, drawn_state)
        && (frame[1], frame[3]) == (drawn_frame[1], drawn_frame[3])
        && (frame[0] - drawn_frame[0]).abs() <= MASKED_SLACK
        && (frame[2] - drawn_frame[2]).abs() <= MASKED_SLACK
}

fn same_geometry(approved: &str, drawn: &str) -> bool {
    let mut left: Vec<&str> = drawn.lines().collect();
    let mut near = Vec::new();
    for line in approved.lines() {
        match left.iter().position(|candidate| *candidate == line) {
            Some(at) => {
                left.remove(at);
            }
            None => near.push(line),
        }
    }
    for line in near {
        match left.iter().position(|candidate| same_line(line, candidate)) {
            Some(at) => {
                left.remove(at);
            }
            None => return false,
        }
    }
    left.is_empty()
}

/// The Android app on the pinned emulator, as a phone story drives it.
impl Driver for AndroidJourney {
    const CLIENT: Client = Client::Android;

    fn ready(&self) -> &Readiness {
        &self.ready
    }
    fn record(&mut self) -> &mut Record {
        &mut self.record
    }
    fn output(&self) -> &Path {
        &self.output
    }
    fn binaries(&self) -> &Binaries {
        &self.binaries
    }
    fn scratch(&self) -> &Path {
        Path::new(SCRATCH_DIR)
    }
    /// The emulator reaches this Mac's loopback at its host alias.
    fn cloud_url(&self) -> String {
        self.ready
            .cloud_url
            .as_deref()
            .map(rewrite_url_for_emulator)
            .unwrap_or_default()
    }
    /// Installed and cleared, so no earlier run's identity or trust is on the
    /// emulator. What a launch tells an iPhone about its browser and its
    /// first run has no Android counterpart: the emulator finds only what
    /// `found` says.
    fn launch(&mut self, _launch: Launch) -> Result<(), String> {
        self.launch_fresh()
    }
    fn relaunch(&mut self, _launch: Launch) -> Result<(), String> {
        AndroidJourney::relaunch(self)
    }
    fn quit(&mut self) -> Result<(), String> {
        AndroidJourney::quit(self)
    }
    /// Every request carries the launch's door token, which the door
    /// refuses to answer without; it never appears in an error.
    fn app_within(&self, mut request: Value, timeout: Duration) -> Result<Value, AppError> {
        if let Some(fields) = request.as_object_mut() {
            fields.insert(
                "door_token".to_owned(),
                Value::String(self.door_token.clone()),
            );
        }
        ask(self.door(), &request, timeout)
            .map_err(|error| redact_app_error(error, &self.door_token))
    }
    fn screen(&mut self, label: &str, volatile: &[&str]) -> Result<(), String> {
        AndroidJourney::screen(self, label, volatile)
    }
    fn tap(&mut self, name: &str) -> Result<(), String> {
        self.wait_within(
            |drawn| drawn.get(name).is_some_and(element_enabled),
            &format!("{name} there to tap"),
            Duration::from_secs(15),
        )?;
        self.app(json!({"kind": "tap", "identifier": name}))?;
        self.record.actions.push(format!("tap {name}"));
        Ok(())
    }
    fn type_text(&mut self, name: &str, text: &str) -> Result<(), String> {
        self.wait_for(&[name])?;
        self.app(json!({"kind": "type", "identifier": name, "text": text}))?;
        self.record
            .actions
            .push(format!("type {text:?} into {name}"));
        Ok(())
    }
    /// The navigation bar's items carry tags of their own.
    fn tab(&mut self, tab: Tab) -> Result<(), String> {
        self.tap(match tab {
            Tab::Agents => "tab.agents",
            Tab::Devices => "tab.hosts",
        })
    }
    /// The account disc on the agent list, then Settings in the sheet it
    /// raises. Settings is a page of its own over the tabs.
    fn settings(&mut self) -> Result<(), String> {
        self.tab(Tab::Agents)?;
        self.tap("home.accounts")?;
        self.choose("Settings")?;
        self.wait_for(&["you"])?;
        Ok(())
    }
    /// The page's own back, as the iPhone's back leaves a page: the top
    /// bar's arrow where the page draws one, else the system's back. The
    /// system's back is not the page's on every page: on a chat whose ask
    /// is past its first question it steps the ask back a question instead.
    fn back(&mut self) -> Result<(), String> {
        if self.elements()?.has("nav.back") {
            self.tap("nav.back")?;
        } else {
            self.app(json!({"kind": "back"}))?;
            self.record().actions.push("back".to_owned());
        }
        Ok(())
    }
    /// Settings is left by the system's back, as every Android page is.
    fn close_settings(&mut self) -> Result<(), String> {
        self.back()?;
        self.wait(|drawn| !drawn.has("you"), "Settings put away")?;
        Ok(())
    }
    /// What the phone's look for machines found: the served machine at its
    /// LAN listener, reached from the emulator through the Mac's host alias.
    fn found(&mut self, host: &str, pairing_open: bool) -> Result<(), String> {
        let found = json!([{
            "host_id": self.host_id(host),
            "name": host,
            "addrs": [format!("{EMULATOR_HOST_IP}:{}", self.host_lan_port(host)?)],
            "scope": self.topology["scope"].as_str().unwrap_or_default(),
            "pairing_open": pairing_open,
        }]);
        self.app(json!({"kind": "discovered", "found": found}))?;
        self.record.actions.push(format!("discovered {found}"));
        Ok(())
    }
    fn large_text(&mut self) -> Result<String, String> {
        self.app(json!({"kind": "fontScale", "scale": ACCESSIBLE_FONT_SCALE}))?;
        let reported = self.query()?["fontScale"].as_f64().unwrap_or_default();
        if (reported - ACCESSIBLE_FONT_SCALE).abs() > 0.05 {
            return Err(format!("the app says it draws at font scale {reported}"));
        }
        Ok(format!("font scale {ACCESSIBLE_FONT_SCALE}"))
    }
    /// Push payloads are sealed with HPKE (X25519, ChaCha20-Poly1305), which
    /// Android's Conscrypt provides; the door opens a vector the Rust side
    /// sealed, on the device, before anything relies on it.
    fn self_checks(&mut self) -> Result<Vec<String>, String> {
        self.app(json!({"kind": "hpkeSelfCheck"}))?;
        Ok(vec![
            "Conscrypt's HPKE opened a push vector the Rust side sealed, on the emulator"
                .to_owned(),
        ])
    }
}

/// The font scale the large-text story reads Android at: Settings'
/// largest step short of the extremes.
const ACCESSIBLE_FONT_SCALE: f64 = 1.5;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_loopback_addresses_and_urls_to_emulator_host_alias() {
        let loopback: SocketAddr = "127.0.0.1:53990".parse().unwrap();
        assert_eq!(rewrite_for_emulator(loopback), "10.0.2.2:53990");
        let lan: SocketAddr = "192.168.1.40:53990".parse().unwrap();
        assert_eq!(rewrite_for_emulator(lan), "192.168.1.40:53990");
        assert_eq!(
            rewrite_url_for_emulator("http://127.0.0.1:8080/api"),
            "http://10.0.2.2:8080/api"
        );
        assert_eq!(
            rewrite_url_for_emulator("https://amux.sh/api"),
            "https://amux.sh/api"
        );
        assert_eq!(loopback_port("http://127.0.0.1:54372"), Some(54372));
        assert_eq!(loopback_port("http://localhost:8080/x"), Some(8080));
        assert_eq!(loopback_port("https://amux.sh"), None);
    }

    #[test]
    fn normalizes_provider_fake_row_run_prefixes_and_prompt_hashes() {
        let ids = BTreeMap::new();
        assert_eq!(
            named_ids("chat.row.toolu_fakefe2b46590000000000000003", &ids),
            "chat.row.toolu_fake<run>0000000000000003"
        );
        assert_eq!(
            named_ids("chat.row.msg_faked8a327b90000000000000002:0", &ids),
            "chat.row.msg_fake<run>0000000000000002:0"
        );
        assert_eq!(
            named_ids("chat.row.exec-717f5db50000000000000003", &ids),
            "chat.row.exec-<run>0000000000000003"
        );
        assert_eq!(
            named_ids("chat.row.prompt:9c03555191344a4f9bb93eb812ed5d3c", &ids),
            "chat.row.prompt:<id>"
        );
    }

    #[test]
    fn parses_emulator_serial_summary() {
        let output = "android (amux-android-1): emulator-5554 (booted, animations=0)\n";
        assert_eq!(
            parse_emulator_serial(output).as_deref(),
            Some("emulator-5554")
        );
    }

    #[test]
    fn shell_quote_preserves_json_for_adb_shell() {
        let quoted = shell_quote(r#"{"scope":"a'b"}"#);
        assert_eq!(quoted, r#"'{"scope":"a'\''b"}'"#);
    }

    #[test]
    fn redacts_door_token_from_errors() {
        let secret = "01234567-89ab-cdef-0123-456789abcdef";
        let err = redact_app_error(
            AppError::Refused(format!(
                r#"the app refused {{"door_token":"{secret}","kind":"tap"}}: boom"#
            )),
            secret,
        );
        let msg: String = err.into();
        assert!(!msg.contains(secret));
        assert!(msg.contains("<redacted>"));
    }

    #[test]
    fn start_config_rewrites_cloud_url_and_respects_relay_gate() {
        let ready: Readiness = serde_json::from_value(json!({
            "control": "127.0.0.1:1",
            "root": "/net",
            "gates": "/gates",
            "hosts": [],
            "agents": [],
            "cloud_url": "http://127.0.0.1:9",
            "relay_tcp": "127.0.0.1:8",
            "relay_quic": "127.0.0.1:7",
            "relay_root": "3082"
        }))
        .unwrap();
        let config = build_start_config(&ready, "phone-reach", true, None);
        assert_eq!(config["lan_bind"], "0.0.0.0:0");
        assert_eq!(config["cloud_url"], "http://10.0.2.2:9");
        assert_eq!(config["relay_tcp"], "10.0.2.2:8");
        assert_eq!(config["relay_quic"], "10.0.2.2:7");
        assert_eq!(config["discovery_scope"], "phone-reach");
        assert_eq!(config["use_system_discovery"], false);
        assert_eq!(config["fresh_start"], true);

        let gated = build_start_config(&ready, "phone-authority", true, Some("10.0.2.2:6"));
        assert_eq!(gated["relay_quic"], "10.0.2.2:6");
        assert!(gated.get("relay_tcp").is_none());
    }

    #[test]
    fn geometry_normalizes_volatile_ids_and_allows_masked_slack() {
        let elements = [
            json!({
                "identifier": "chat.row.turn-end",
                "label": "Worked for 3s",
                "value": null,
                "enabled": true,
                "frame": {"x": 16.0, "y": 271.0, "width": 69.0, "height": 16.0}
            }),
            json!({
                "identifier": "you.fingerprint",
                "label": "abcd ef01",
                "value": null,
                "enabled": true,
                "frame": {"x": 0.0, "y": 0.0, "width": 100.0, "height": 20.0}
            }),
            json!({
                "identifier": "chat.send",
                "label": "Send",
                "value": null,
                "enabled": false,
                "frame": {"x": 350.0, "y": 700.0, "width": 48.0, "height": 48.0}
            }),
        ];
        assert!(validate_element_frames(&elements).is_ok());
        let formatted = format_elements(&elements, &OWN_IDENTITY, &BTreeMap::new());
        assert_eq!(
            formatted.lines().collect::<Vec<_>>(),
            [
                "chat.row.turn-end | Worked for <t> |  | 16,271,69,16",
                "you.fingerprint | <volatile>",
                "chat.send | Send |  | 350,700,48,48 (disabled)",
            ]
        );
        assert!(same_geometry(
            &formatted,
            &formatted.replace("69,16", "73,16")
        ));
        assert!(!same_geometry(
            &formatted,
            &formatted.replace("48,48", "52,48")
        ));
    }
}
