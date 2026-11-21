//! The Android journey driver: starts a served topology with `testnet serve`,
//! installs the debug APK fresh on the pinned Android emulator (`amux-android-1`),
//! forwards the debug door over `adb forward`, launches `MainActivity` with the
//! served network's configuration, drives the real app through its Compose
//! semantics tree via the NDJSON door, checks host/agent/provider records, and
//! compares element geometry against `journeys/goldens/android/<story>/<label>.elements.txt`.
//!
//! Each compared screen is also photographed into `target/journeys/android/<story>/actual/`
//! beside its geometry for human inspection; `UPDATE_JOURNEY_GOLDENS=1` rewrites
//! geometry goldens that differ (refused in CI).
//!
//! Intentionally excluded phone journeys from `journeys/manifest.json` in v1:
//! - `push-wake`: FCM push delivery is stubbed behind `PushRegistrar` in v1.
//! - `purchase-restore`: Google Play Billing is stubbed behind `BillingStoreFront` in v1.
//! - `local-network`: tests the iOS-only Local Network pre-permission refusal card;
//!   Android uses `NsdManager` directly with no local-network permission gate.

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

use super::common::*;
use super::phone::{AppError, Drawn, Printing, ask, labelled, normalize, printed_pin, run_within};
use super::{
    Proved, Record, door, line_diff, negative_control, never_sent, phase, prompts, quoted,
    reflected_once, root,
};
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
const OWN_IDENTITY: [&str; 3] = [
    "settings.phone.name",
    "settings.phone.fingerprint",
    "settings.phone.version",
];
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

type Tell = fn(&mut AndroidJourney) -> Proved;

const STORIES: &[(&str, Tell)] = &[
    ("reach-host", reach_host),
    ("conversation-decision-claude-sdk", |j| {
        conversation_decision(j, "decision-sdk", true)
    }),
    ("conversation-decision-claude-pty", |j| {
        conversation_decision(j, "decision-pty", false)
    }),
    ("conversation-decision-codex", |j| {
        conversation_decision(j, "decision-codex", true)
    }),
    ("manage-agent", manage_agent),
    ("decide-plan-claude-sdk", decide_plan_claude_sdk),
    ("answer-questions", answer_questions),
    ("tool-server-asks", tool_server_asks),
    ("queue-and-steer", queue_and_steer),
    ("send-while-away", send_while_away),
    ("composer-limits", composer_limits),
    ("keep-authority", keep_authority),
    ("leave-and-recover", leave_and_recover),
    ("account-sign-in", account_sign_in),
    ("one-tap-sign-in", one_tap_sign_in),
    ("accessibility", accessibility),
    ("report", report),
];

/// Runs the named Android journey stories, or every story written here.
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
            "debug APK not found at {}; run `just android build` first",
            apk.display()
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
        let (_, tell) = STORIES.iter().find(|(id, _)| id == name).expect("known");
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
        STORIES.iter().map(|(id, _)| (*id).to_owned()).collect()
    } else {
        named
    };
    let unknown: Vec<&str> = wanted
        .iter()
        .filter(|name| !STORIES.iter().any(|(id, _)| id == name))
        .map(String::as_str)
        .collect();
    if unknown.is_empty() {
        Ok(wanted)
    } else {
        let known: Vec<&str> = STORIES.iter().map(|(id, _)| *id).collect();
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

fn debug_apk() -> PathBuf {
    root().join("apps/android/app/build/outputs/apk/debug/app-debug.apk")
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
        .args(["-B", "apps/android/tools/android_emulator.py"])
        .current_dir(root());
    let said = run_within(command, Duration::from_secs(300))?;
    parse_emulator_serial(&said)
        .ok_or_else(|| format!("android_emulator.py named no serial for {PINNED_AVD}: {said}"))
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

    // --- the served net --------------------------------------------------

    fn request(&mut self, request: Control, label: Option<&str>) -> Result<Value, String> {
        self.record.actions.push(format!(
            "net {}",
            serde_json::to_string(&request).unwrap_or_default()
        ));
        let reply = door(self.ready.control, &request, Duration::from_secs(90))?;
        if let Some(label) = label {
            self.record
                .observations
                .insert(label.to_owned(), reply.clone());
        }
        Ok(reply)
    }

    fn chat(&mut self, host: &str, agent: &str, label: &str) -> Result<Value, String> {
        self.request(
            Control::Chat {
                host: host.to_owned(),
                agent: agent.to_owned(),
            },
            Some(label),
        )
    }

    fn wait_chat(
        &mut self,
        host: &str,
        agent: &str,
        ready: impl Fn(&Value) -> bool,
        description: &str,
    ) -> Result<Value, String> {
        let control = self.ready.control;
        let request = Control::Chat {
            host: host.to_owned(),
            agent: agent.to_owned(),
        };
        match super::poll(
            Duration::from_secs(60),
            Duration::from_millis(200),
            || door(control, &request, Duration::from_secs(90)),
            |chat| ready(chat),
        ) {
            Ok(chat) => {
                self.record
                    .observations
                    .insert(description.to_owned(), chat.clone());
                self.record
                    .actions
                    .push(format!("observed at {host}: {description}"));
                Ok(chat)
            }
            Err((last, why)) => Err(format!(
                "timed out waiting for {description} ({why}); {host} holds {}",
                last.unwrap_or_default()
            )),
        }
    }

    fn wait_inventory(
        &mut self,
        host: &str,
        ready: impl Fn(&[Value]) -> bool,
        description: &str,
    ) -> Result<Vec<Value>, String> {
        let control = self.ready.control;
        let request = Control::Inventory {
            host: host.to_owned(),
        };
        match super::poll(
            Duration::from_secs(60),
            Duration::from_millis(200),
            || {
                door(control, &request, Duration::from_secs(90))
                    .map(|reply| reply["agents"].as_array().cloned().unwrap_or_default())
            },
            |agents| ready(agents),
        ) {
            Ok(agents) => {
                self.record
                    .observations
                    .insert(description.to_owned(), Value::Array(agents.clone()));
                self.record
                    .actions
                    .push(format!("observed at {host}: {description}"));
                Ok(agents)
            }
            Err((last, why)) => Err(format!(
                "timed out waiting for {description} ({why}); {host} lists {}",
                Value::Array(last.unwrap_or_default())
            )),
        }
    }

    fn provider_input(&mut self, agent: &str, label: &str) -> Result<Vec<String>, String> {
        let reply = self.request(
            Control::ProviderInput {
                agent: agent.to_owned(),
            },
            Some(label),
        )?;
        Ok(reply["lines"]
            .as_array()
            .map(|lines| {
                lines
                    .iter()
                    .filter_map(|line| line.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }

    fn host_id(&self, host: &str) -> String {
        self.ready
            .hosts
            .iter()
            .find(|ready| ready.name == host)
            .map(|ready| ready.host_id.to_string())
            .unwrap_or_default()
    }

    fn agent_id(&self, name: &str) -> String {
        self.ready
            .agents
            .iter()
            .find(|agent| agent.name == name)
            .map(|agent| agent.id.to_string())
            .unwrap_or_default()
    }

    fn host_lan_port(&self, host: &str) -> Result<u16, String> {
        self.ready
            .hosts
            .iter()
            .find(|ready| ready.name == host)
            .and_then(|ready| ready.lan_port)
            .ok_or_else(|| format!("host {host:?} has no lan_port in readiness"))
    }

    fn config(&self, host: &str) -> PathBuf {
        self.ready
            .hosts
            .iter()
            .find(|ready| ready.name == host)
            .map(|ready| ready.config.clone())
            .unwrap_or_default()
    }

    fn cloud_url_for_emulator(&self) -> Option<String> {
        self.ready
            .cloud_url
            .as_deref()
            .map(rewrite_url_for_emulator)
    }

    fn amux(&mut self, host: &str, args: &[&str]) -> Result<Printing, String> {
        self.record
            .actions
            .push(format!("at {host}: amux {}", args.join(" ")));
        let scratch = Path::new(SCRATCH_DIR);
        let mut child = Command::new(self.binaries.amux())
            .arg("--config")
            .arg(self.config(host))
            .args(args)
            .env_remove("AMUX_LOG")
            .env_remove("AMUX_CONFIG")
            .env("TMPDIR", scratch)
            .env("TMP", scratch)
            .env("TEMP", scratch)
            .env("AMUX_TEST_DISCOVERY_MODE", "disabled")
            .env("RUST_LOG", rust_log())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("amux: {error}"))?;
        let (sent, lines) = mpsc::channel();
        for pipe in [
            child
                .stdout
                .take()
                .map(|pipe| Box::new(pipe) as Box<dyn std::io::Read + Send>),
            child
                .stderr
                .take()
                .map(|pipe| Box::new(pipe) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let sent = sent.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    if sent.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        Ok(Printing::new(child, lines))
    }

    fn pairing_link(&mut self, host: &str) -> Result<(Printing, String), String> {
        let mut process = self.amux(host, &["pair", "--qr", "--print-link"])?;
        match process.find(Duration::from_secs(60), |line| {
            line.strip_prefix("Pairing link: ")
                .map(|link| link.trim().to_owned())
        }) {
            Some(link) => Ok((process, link)),
            None => {
                process.kill();
                Err(format!("{host} printed no pairing link"))
            }
        }
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

    fn app(&self, request: Value) -> Result<Value, AppError> {
        self.app_within(request, Duration::from_secs(90))
    }

    fn app_within(&self, mut request: Value, timeout: Duration) -> Result<Value, AppError> {
        if let Some(obj) = request.as_object_mut() {
            obj.insert(
                "door_token".to_owned(),
                Value::String(self.door_token.clone()),
            );
        }
        ask(self.door(), &request, timeout)
            .map_err(|error| redact_app_error(error, &self.door_token))
    }

    fn query(&self) -> Result<Value, String> {
        Ok(self.app(json!({"kind": "query"}))?["state"].take())
    }

    fn elements(&self) -> Result<Drawn, String> {
        let state = self.query()?;
        let elements = state["elements"].as_array().map_or(&[][..], Vec::as_slice);
        Ok(Drawn::of(elements))
    }

    fn wait(&mut self, ready: impl Fn(&Drawn) -> bool, description: &str) -> Result<Drawn, String> {
        self.wait_within(ready, description, Duration::from_secs(60))
    }

    fn wait_within(
        &mut self,
        ready: impl Fn(&Drawn) -> bool,
        description: &str,
        timeout: Duration,
    ) -> Result<Drawn, String> {
        let deadline = Instant::now() + timeout;
        let mut last = Drawn::default();
        while Instant::now() < deadline {
            last = self.elements()?;
            if ready(&last) {
                self.record.actions.push(format!("reached {description}"));
                return Ok(last);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let drawn: Vec<String> = last
            .iter()
            .map(|(name, element)| format!("{name} | {} | {}", element["label"], element["value"]))
            .collect();
        Err(format!(
            "timed out waiting for {description}; the Android app draws:\n{}",
            drawn.join("\n")
        ))
    }

    fn wait_for(&mut self, names: &[&str]) -> Result<Drawn, String> {
        self.wait(
            |drawn| names.iter().all(|name| drawn.has(name)),
            &format!("{names:?}"),
        )
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

    fn clear(&mut self, name: &str) -> Result<(), String> {
        self.wait_for(&[name])?;
        self.app(json!({"kind": "clear", "identifier": name}))?;
        self.record.actions.push(format!("clear {name}"));
        Ok(())
    }

    fn back(&mut self) -> Result<(), String> {
        self.app(json!({"kind": "back"}))?;
        self.record.actions.push("back".to_owned());
        Ok(())
    }

    fn discovered(&mut self, found: Value) -> Result<(), String> {
        self.app(json!({"kind": "discovered", "found": found}))?;
        self.record.actions.push(format!("discovered {found}"));
        Ok(())
    }

    fn pair_link(&mut self, link: &str) -> Result<String, AppError> {
        let reply = self.app(json!({"kind": "pair", "qr": link}))?;
        let host = reply["host"].as_str().unwrap_or_default().to_owned();
        self.record
            .actions
            .push(format!("pair by link, answered by {host}"));
        Ok(host)
    }

    fn system_screenshot(&mut self) -> Result<(), String> {
        self.app(json!({"kind": "screenshot"}))?;
        self.record
            .actions
            .push("triggered system screenshot callback".to_owned());
        Ok(())
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

// --- shared Android acts & checks ----------------------------------------

fn announce_host(
    journey: &mut AndroidJourney,
    host: &str,
    pairing_open: bool,
) -> Result<(), String> {
    let host_id = journey.host_id(host);
    let port = journey.host_lan_port(host)?;
    let scope = journey.topology["scope"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let addr = format!("{EMULATOR_HOST_IP}:{port}");
    journey.discovered(json!([{
        "host_id": host_id,
        "name": host,
        "addrs": [addr],
        "scope": scope,
        "pairing_open": pairing_open,
    }]))
}

fn pair_by_code(journey: &mut AndroidJourney, host: &str) -> Result<(), String> {
    journey.wait_for(&["tab.hosts"])?;
    journey.tap("tab.hosts")?;
    journey.wait_for(&["hosts"])?;
    announce_host(journey, host, false)?;
    journey.wait_for(&[&format!("hosts.found.{host}")])?;

    let mut pairing = journey.amux(host, &["pair"])?;
    let pin = printed_pin(&mut pairing)?;
    announce_host(journey, host, true)?;

    journey.wait_for(&["pairing.sheet", "pairing.digits"])?;
    journey.type_text("pairing.digits", &pin)?;
    journey.wait_for(&["pairing.confirm", "pairing.fingerprint"])?;
    journey.tap("pairing.confirm")?;
    let row = format!("hosts.row.{host}");
    journey.wait(
        |drawn| !drawn.has("pairing.sheet") && drawn.has(&row),
        &format!("{host} paired"),
    )?;
    pairing.wait(Duration::from_secs(30))
}

fn pair_by_link_retry(journey: &mut AndroidJourney, link: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match journey.pair_link(link) {
            Ok(_) => return Ok(()),
            Err(AppError::Refused(error)) => {
                if Instant::now() > deadline {
                    return Err(error);
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn pair_through_the_relay(journey: &mut AndroidJourney, host: &str) -> Result<(), String> {
    let (mut pairing, link) = journey.pairing_link(host)?;
    if let Err(error) = pair_by_link_retry(journey, &link) {
        pairing.kill();
        return Err(error);
    }
    pairing.wait(Duration::from_secs(30))
}

fn connect(journey: &mut AndroidJourney, account: &str) -> Result<(), String> {
    let relay = journey.cloud_url_for_emulator().unwrap_or_default();
    journey.app(json!({
        "kind": "connect",
        "relay": relay,
        "token": format!("refresh-{account}"),
        "user": account,
    }))?;
    journey
        .record
        .actions
        .push(format!("connected account {account}"));
    Ok(())
}

fn relay_cloud(journey: &mut AndroidJourney, fields: Value) -> Result<Value, String> {
    let cloud = journey.cloud_url_for_emulator().unwrap_or_default();
    let authority = cloud
        .split_once("://")
        .map_or(cloud.as_str(), |(_, rest)| rest);
    let authority = authority.split('/').next().unwrap_or_default();
    let (host, port) = authority.rsplit_once(':').unwrap_or((authority, ""));
    let mut fields = fields;
    let account = fields
        .as_object_mut()
        .and_then(|fields| fields.remove("account"))
        .and_then(|account| account.as_str().map(str::to_owned))
        .unwrap_or_else(|| "ada".to_owned());
    let mut script = json!({
        "account": account,
        "email": format!("{account}@example.com"),
        "token": format!("refresh-{account}"),
        "relayHost": host,
        "relayPort": port.parse::<u16>().ok(),
    });
    if let (Some(script), Some(fields)) = (script.as_object_mut(), fields.as_object()) {
        script.extend(fields.clone());
    }
    journey.app(json!({"kind": "cloud", "cloud": script}))?;
    journey
        .record
        .actions
        .push(format!("scripted cloud {script}"));
    Ok(script)
}

fn cloud_calls(journey: &mut AndroidJourney, label: &str) -> Result<Vec<String>, String> {
    let reply = journey.app(json!({"kind": "calls"}))?;
    journey
        .record
        .observations
        .insert(label.to_owned(), reply["cloud"].clone());
    Ok(strings(&reply["cloud"]))
}

fn asked_once(journey: &mut AndroidJourney, call: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let reply = journey.app(json!({"kind": "calls"}))?;
        let asked = strings(&reply["cloud"]);
        match asked.iter().filter(|asked| *asked == call).count() {
            1 => return Ok(()),
            0 if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                return Err(format!(
                    "the account service was not asked {call:?} once: {asked:?}"
                ));
            }
        }
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn open_agent(journey: &mut AndroidJourney, name: &str) -> Result<(), String> {
    let drawn = journey.elements()?;
    if drawn.has("tab.agents") {
        journey.tap("tab.agents")?;
    }
    let row = format!("home.row.{name}");
    journey.wait_for(&[&row])?;
    journey.tap(&row)?;
    journey.wait_for(&["screen.conversation", "chat.field"])?;
    Ok(())
}

fn send(journey: &mut AndroidJourney, text: &str) -> Result<(), String> {
    journey.type_text("chat.field", text)?;
    journey.wait(
        |drawn| drawn.enabled("chat.send"),
        "a message ready to send",
    )?;
    journey.tap("chat.send")
}

fn leave_chat(journey: &mut AndroidJourney) -> Result<(), String> {
    if journey.elements()?.has("nav.back") {
        journey.tap("nav.back")?;
    } else {
        journey.back()?;
    }
    journey.wait(
        |drawn| !drawn.has("screen.conversation") && (drawn.has("home") || drawn.has("settled")),
        "back from conversation",
    )?;
    Ok(())
}

fn reopen(
    journey: &mut AndroidJourney,
    agent_name: &str,
    ready: impl Fn(&Drawn) -> bool,
) -> Result<Drawn, String> {
    leave_chat(journey)?;
    let row = format!("home.row.{agent_name}");
    journey.wait_for(&[&row])?;
    journey.tap(&row)?;
    journey.wait(
        |drawn| drawn.has("chat.field") && ready(drawn),
        "the chat reopened",
    )
}

fn reopen_card(
    journey: &mut AndroidJourney,
    agent_name: &str,
    ready: impl Fn(&Drawn) -> bool,
) -> Result<Drawn, String> {
    leave_chat(journey)?;
    let row = format!("home.row.{agent_name}");
    journey.wait_for(&[&row])?;
    journey.tap(&row)?;
    journey.wait(
        |drawn| drawn.has("ask") && ready(drawn),
        "the chat reopened on the ask card",
    )
}

fn open_settings(journey: &mut AndroidJourney) -> Result<(), String> {
    let drawn = journey.elements()?;
    if drawn.has("tab.agents") && !drawn.has("home.account-menu") {
        journey.tap("tab.agents")?;
    }
    journey.wait_for(&["home.account-menu"])?;
    journey.tap("home.account-menu")?;
    journey.wait_for(&["accounts.sheet", "accounts.settings"])?;
    journey.tap("accounts.settings")?;
    journey.wait_for(&["screen.settings"])?;
    Ok(())
}

fn close_settings(journey: &mut AndroidJourney) -> Result<(), String> {
    journey.back()?;
    journey.wait(|drawn| !drawn.has("screen.settings"), "Settings closed")?;
    Ok(())
}

fn choice(drawn: &Drawn, label: &str) -> Result<String, String> {
    let choices = || {
        drawn
            .iter()
            .filter(|(name, _)| name.starts_with("ask.choice."))
    };
    choices()
        .find(|(_, element)| element["label"].as_str() == Some(label))
        .map(|(name, _)| name.to_owned())
        .ok_or_else(|| {
            let offered: Vec<&Value> = choices().map(|(_, element)| &element["label"]).collect();
            format!("no choice reads {label:?}; the card offers {offered:?}")
        })
}

fn asking(drawn: &Drawn, question: &str) -> bool {
    drawn.label("ask.question") == question
}

fn plan_card(drawn: &Drawn) -> bool {
    drawn.has("ask") && drawn.has("ask.approve")
}

fn no_plan_file(drawn: Drawn) -> Result<Drawn, String> {
    let shown: Vec<String> = drawn
        .iter()
        .filter(|(_, element)| {
            let label = element["label"].as_str().unwrap_or_default();
            ["/.claude/plans/", "move-the-journal.md"]
                .iter()
                .any(|part| label.contains(part))
        })
        .map(|(name, element)| format!("{name}: {}", element["label"]))
        .collect();
    if shown.is_empty() {
        Ok(drawn)
    } else {
        Err(format!("the plan file shows as a step: {shown:?}"))
    }
}

fn send_back(journey: &mut AndroidJourney, card: &Drawn, note: &str) -> Result<(), String> {
    journey.tap(&choice(card, "Send back…")?)?;
    journey.wait_for(&["ask.note"])?;
    journey.type_text("ask.note", note)?;
    let drawn = journey.wait(
        |drawn| {
            drawn.iter().any(|(name, element)| {
                name.starts_with("ask.choice.")
                    && element["label"] == "Send back"
                    && element_enabled(element)
            })
        },
        "a note ready to send back",
    )?;
    journey.tap(&choice(&drawn, "Send back")?)
}

fn kept_unsent(drawn: &Drawn, text: &str) -> bool {
    drawn.value("chat.field") == text && !drawn.enabled("chat.send")
}

fn composer_current(drawn: &Drawn) -> bool {
    drawn.label("chat.field") == "Reply"
}

fn replies(drawn: &Drawn, text: &str) -> usize {
    let markdown = drawn
        .iter()
        .filter(|(name, element)| {
            name.starts_with("markdown.root")
                && element["label"]
                    .as_str()
                    .is_some_and(|label| label.contains(text))
        })
        .count();
    if markdown > 0 {
        return markdown;
    }
    drawn
        .iter()
        .filter(|(name, element)| {
            name.starts_with("chat.row.")
                && !name.starts_with("chat.row.kind.")
                && element["label"]
                    .as_str()
                    .is_some_and(|label| label.contains(text))
        })
        .count()
}

fn rectangle(drawn: &Drawn, name: &str) -> Option<[f64; 4]> {
    let frame = drawn.field(name, "frame");
    let (x, y) = (frame["x"].as_f64()?, frame["y"].as_f64()?);
    Some([
        x,
        y,
        x + frame["width"].as_f64()?,
        y + frame["height"].as_f64()?,
    ])
}

fn across_the_screen(drawn: &Drawn, prefix: &str) -> Result<(), String> {
    let width = rectangle(drawn, "shell").ok_or("the shell reports no frame")?[2];
    let cut: Vec<String> = drawn
        .names()
        .filter(|name| name.starts_with(prefix))
        .filter_map(|name| {
            let [left, _, right, _] = rectangle(drawn, name)?;
            (left < -0.5 || right > width + 0.5).then(|| format!("{name} at {left}..{right}"))
        })
        .collect();
    if cut.is_empty() {
        Ok(())
    } else {
        Err(format!("past the {width} dp screen: {}", cut.join(", ")))
    }
}

fn apart(drawn: &Drawn, names: &[&str]) -> Result<(), String> {
    for (at, first) in names.iter().enumerate() {
        for second in &names[at + 1..] {
            let (Some(a), Some(b)) = (rectangle(drawn, first), rectangle(drawn, second)) else {
                return Err(format!("{first} or {second} reports no frame"));
            };
            let across = a[2].min(b[2]) - a[0].max(b[0]);
            let down = a[3].min(b[3]) - a[1].max(b[1]);
            if across > 1.0 && down > 1.0 {
                return Err(format!("{first} is drawn over {second}"));
            }
        }
    }
    Ok(())
}

fn link_done(results: &[Value]) -> Result<(), String> {
    let actions: Vec<&Value> = results.iter().map(|result| &result["action"]).collect();
    if actions == [&json!("accept"), &json!("accept")] && results[1]["content"].is_null() {
        Ok(())
    } else {
        Err(format!("Codex was answered {results:?}"))
    }
}

fn stop_daemon(host: &str) -> Control {
    Control::StopDaemon {
        host: host.to_owned(),
    }
}

fn restart_daemon(host: &str) -> Control {
    Control::RestartDaemon {
        host: host.to_owned(),
    }
}

// --- Android stories -----------------------------------------------------

fn reach_host(journey: &mut AndroidJourney) -> Proved {
    let desk_id = journey.host_id("desk");
    let desk_port = journey.host_lan_port("desk")?;
    let scope = journey.topology["scope"]
        .as_str()
        .unwrap_or("phone-reach")
        .to_owned();
    let desk_addr = format!("{EMULATOR_HOST_IP}:{desk_port}");
    let prompt = "Hello from the phone.";
    let reply = "The desk is reachable.";

    journey.launch_fresh()?;
    journey.app(json!({"kind": "hpkeSelfCheck"}))?;
    journey
        .record
        .actions
        .push("Conscrypt HPKE self-check passed".to_owned());
    journey.wait_for(&["tab.agents", "tab.hosts"])?;

    journey.tap("tab.hosts")?;
    journey.wait_for(&["hosts"])?;
    announce_host(journey, "desk", false)?;
    journey.wait_for(&["hosts.found.desk"])?;
    journey.screen("found-not-paired", &[])?;

    let mut pairing = journey.amux("desk", &["pair"])?;
    let pin = printed_pin(&mut pairing)?;
    announce_host(journey, "desk", true)?;

    journey.wait(
        |drawn| {
            drawn.has("pairing.sheet")
                && drawn.has("pairing.digits")
                && rectangle(drawn, "pairing.sheet").is_some_and(|[_, top, _, _]| top < 500.0)
        },
        "pairing sheet raised above keyboard",
    )?;
    journey.screen("pair-offered", &[])?;

    journey.type_text("pairing.digits", &pin)?;
    let confirm = journey.wait(
        |drawn| {
            drawn.has("pairing.confirm")
                && drawn.has("pairing.fingerprint")
                && rectangle(drawn, "pairing.sheet").is_some_and(|[_, _, _, bottom]| bottom > 800.0)
        },
        "pairing confirmation sheet settled at rest",
    )?;
    let title = confirm.label("pairing.title");
    if !title.contains("desk") {
        return Err(format!(
            "pairing confirmation title was {title:?}, expected desk"
        ));
    }
    journey.screen("pair-confirm", &["pairing.fingerprint"])?;

    journey.tap("pairing.confirm")?;
    let paired_hosts = journey.wait(
        |drawn| {
            !drawn.has("pairing.sheet")
                && drawn.has("hosts.row.desk")
                && drawn.label("hosts.row.desk").contains("direct")
        },
        "desk paired and reachable directly",
    )?;
    pairing.wait(Duration::from_secs(30))?;
    journey.screen("paired-hosts", &[])?;
    let host_label = paired_hosts.label("hosts.row.desk").to_owned();

    journey.tap("tab.agents")?;
    let agents = journey.wait_for(&["home.row.desk-work"])?;
    let agent_label = agents.label("home.row.desk-work").to_owned();
    journey.screen("paired-agents", &[])?;

    journey.tap("home.row.desk-work")?;
    journey.wait_for(&["screen.conversation", "chat.field"])?;
    send(journey, prompt)?;
    let heard = journey.wait_chat(
        "desk",
        "desk-work",
        |chat| phase(chat) == "IDLE" && prompts(chat, prompt).len() == 1,
        "desk-heard",
    )?;
    reflected_once(&heard, prompt)?;
    journey.wait(
        |drawn| labelled(drawn, reply),
        "the desk's reply on the phone",
    )?;

    reopen(journey, "desk-work", |drawn| {
        drawn.has("chat.row.turn-end") && labelled(drawn, reply)
    })?;
    journey.screen("usable-chat", &["chat.row.turn-end"])?;
    journey.back()?;
    journey.wait(
        |drawn| !drawn.has("screen.conversation") && drawn.has("home.row.desk-work"),
        "returned to Agents home via system back",
    )?;

    Ok(vec![
        "Android Conscrypt X25519 + ChaCha20-Poly1305 HPKE decrypted Rust-sealed push vector on device"
            .to_owned(),
        format!("Android app discovered desk ({desk_id}) on {desk_addr} in scope {scope}"),
        "desk's open pairing window raised the Compose pairing bottom sheet automatically"
            .to_owned(),
        "typing the 6-digit PIN reached desk over QUIC/UDP and displayed its fingerprint"
            .to_owned(),
        format!("confirming trust listed desk on Computers tab: {host_label}"),
        format!("reconciled fleet listed desk-work on Agents tab: {agent_label}"),
        format!(
            "the desk's agent answered and recorded {} once",
            quoted(prompt)
        ),
    ])
}

fn conversation_decision(journey: &mut AndroidJourney, agent: &str, provider_logs: bool) -> Proved {
    journey.launch_fresh()?;
    pair_by_code(journey, "desk")?;
    open_agent(journey, agent)?;
    send(journey, PROMPT)?;

    let card = journey.wait(
        |drawn| drawn.has("ask") && labelled(drawn, "deploy --check") && labelled(drawn, "Allow"),
        "the permission card",
    )?;
    let sent = journey.wait_chat(
        "desk",
        agent,
        |chat| !prompts(chat, PROMPT).is_empty(),
        "prompt-reflected",
    )?;
    reflected_once(&sent, PROMPT)?;
    let control = negative_control(reflected_once(&sent, &format!("{PROMPT} (a wrong prompt)")))?;
    journey.screen("permission", &[])?;

    journey.tap(&choice(&card, "Allow")?)?;
    journey.wait(
        |drawn| !drawn.has("ask") && labelled(drawn, REPLY),
        "the settled turn",
    )?;
    let settled = journey.wait_chat(
        "desk",
        agent,
        |chat| phase(chat) == "IDLE" && super::holds(chat, REPLY),
        "turn-settled",
    )?;
    reflected_once(&settled, PROMPT)?;

    let mut proved = vec![
        format!("{} reflected once in the desk's chat", quoted(PROMPT)),
        control,
        "the permission card offered its outcomes and Allow was tapped".to_owned(),
        format!("the reply {} arrived and the desk says idle", quoted(REPLY)),
    ];
    if provider_logs {
        let lines = journey.provider_input(agent, "provider-input")?;
        let answers = lines
            .iter()
            .filter(|line| {
                let line = line.to_lowercase();
                line.contains("allow") || line.contains("accept")
            })
            .count();
        if answers != 1 {
            return Err(format!("the provider did not receive one allow: {lines:?}"));
        }
        proved.push("the provider received exactly one allow".to_owned());
    }

    reopen(journey, agent, |drawn| {
        drawn.has("chat.row.turn-end") && labelled(drawn, REPLY)
    })?;
    journey.screen("settled", &["chat.row.turn-end"])?;
    Ok(proved)
}

fn manage_agent(journey: &mut AndroidJourney) -> Proved {
    journey.launch_fresh()?;
    pair_by_code(journey, "desk")?;
    let before = ids(&journey.wait_inventory("desk", |_| true, "inventory-before")?);

    // Create a Claude agent on the desk from the Agents tab FAB.
    journey.tap("tab.agents")?;
    journey.wait_for(&["home.fab.new-agent"])?;
    journey.tap("home.fab.new-agent")?;
    journey.wait_for(&["new-agent", "new-agent.provider.claude"])?;
    journey.tap("new-agent.provider.claude")?;
    journey.wait(
        |drawn| drawn.enabled("new-agent.start"),
        "New Agent ready to start",
    )?;
    journey.tap("new-agent.start")?;

    let created = journey.wait_inventory(
        "desk",
        |agents| ids(agents).difference(&before).count() == 1,
        "created",
    )?;
    let id = ids(&created)
        .difference(&before)
        .next()
        .cloned()
        .unwrap_or_default();
    let named = listed(&created, &id)
        .and_then(|agent| agent["name"].as_str())
        .unwrap_or_default()
        .to_owned();
    lifecycle(&created, &id, &named, LIVE)?;
    journey
        .extra_ids
        .insert(id.clone(), "created-agent".to_owned());

    journey.wait_for(&["screen.conversation", "chat.field"])?;
    send(journey, HELLO)?;
    journey.wait(
        |drawn| replies(drawn, READY) == 1 && drawn.has("chat.row.turn-end"),
        "the first reply",
    )?;
    // Leave and reopen the newly created agent (its display row uses the
    // provider's session title before an explicit rename).
    leave_chat(journey)?;
    let home = journey.wait(
        |drawn| {
            drawn.iter().any(|(name, element)| {
                name.starts_with("home.row.")
                    && !name.ends_with(".menu")
                    && element["label"]
                        .as_str()
                        .is_some_and(|label| label.contains(READY))
            })
        },
        "the created agent on Agents home",
    )?;
    let created_row = home
        .iter()
        .find(|(name, element)| {
            name.starts_with("home.row.")
                && !name.ends_with(".menu")
                && element["label"]
                    .as_str()
                    .is_some_and(|label| label.contains(READY))
        })
        .map(|(name, _)| name.to_owned())
        .ok_or("created agent row disappeared")?;
    journey.tap(&created_row)?;
    journey.wait(
        |drawn| {
            drawn.has("chat.field") && replies(drawn, READY) == 1 && drawn.has("chat.row.turn-end")
        },
        "created chat reopened",
    )?;
    journey.screen("created", &["chat.row.turn-end"])?;

    // Rename from the chat overflow menu.
    journey.tap("chat.more")?;
    journey.wait_for(&["chat.menu.rename"])?;
    journey.tap("chat.menu.rename")?;
    journey.wait_for(&["chat.rename.field"])?;
    journey.clear("chat.rename.field")?;
    journey.type_text("chat.rename.field", "helper")?;
    journey.tap("chat.rename.confirm")?;
    let renamed = journey.wait_inventory(
        "desk",
        |agents| listed(agents, &id).is_some_and(|agent| agent["name"] == "helper"),
        "renamed",
    )?;
    lifecycle(&renamed, &id, "helper", LIVE)?;
    journey.wait(
        |drawn| drawn.label("chat.title").contains("helper"),
        "the new name in the chat top bar",
    )?;

    // Settle from the chat overflow menu.
    journey.tap("chat.more")?;
    journey.wait_for(&["chat.menu.settle"])?;
    journey.tap("chat.menu.settle")?;
    let settled = journey.wait_inventory(
        "desk",
        |agents| {
            listed(agents, &id)
                .is_some_and(|agent| agent["lifecycle"] == EXITED && agent["filing"] == SETTLED)
        },
        "settled",
    )?;
    lifecycle(&settled, &id, "helper", EXITED)?;
    let control = negative_control(lifecycle(&settled, &id, "helper", LIVE))?;
    let kept = journey.chat("desk", &id, "history-kept")?;
    reflected_once(&kept, HELLO)?;
    if !super::holds(&kept, READY) {
        return Err(format!(
            "the settled agent's history lost its reply: {kept}"
        ));
    }

    // Open the settled agent from the Settled route and verify exited composer.
    leave_chat(journey)?;
    journey.wait_for(&["home.settled-row"])?;
    journey.tap("home.settled-row")?;
    journey.wait_for(&["settled", "settled.row.helper"])?;
    journey.tap("settled.row.helper")?;
    journey.wait(
        |drawn| {
            drawn.has("chat.field")
                && drawn.has("chat.exited")
                && drawn.has("chat.resume")
                && replies(drawn, READY) == 1
        },
        "the settled chat",
    )?;
    journey.screen("exited-and-resumable", &["chat.row.turn-end"])?;

    // Resume from the exited composer with a message.
    journey.type_text("chat.field", BACK)?;
    journey.wait(
        |drawn| drawn.enabled("chat.resume"),
        "a message ready to resume with",
    )?;
    journey.tap("chat.resume")?;
    let resumed = journey.wait_inventory(
        "desk",
        |agents| {
            listed(agents, &id)
                .is_some_and(|agent| agent["lifecycle"] == LIVE && agent["filing"] == ACTIVE)
        },
        "resumed",
    )?;
    lifecycle(&resumed, &id, "helper", LIVE)?;
    let chat = journey.wait_chat(
        "desk",
        &id,
        |chat| prompts(chat, BACK).len() == 1 && phase(chat) == "IDLE",
        "resumed-chat",
    )?;
    reflected_once(&chat, HELLO)?;
    reflected_once(&chat, BACK)?;
    journey.wait(
        |drawn| replies(drawn, READY) == 2 && !drawn.has("chat.exited"),
        "the resumed reply",
    )?;

    // Return via Settled (now empty) to Agents home, which lists helper again.
    journey.back()?;
    journey.wait(
        |drawn| drawn.has("settled.empty") && !drawn.has("screen.conversation"),
        "Settled empty after resume",
    )?;
    journey.back()?;
    journey.wait_for(&["home.row.helper"])?;
    journey.tap("home.row.helper")?;
    journey.wait(
        |drawn| replies(drawn, READY) == 2 && drawn.has("chat.row.turn-end"),
        "resumed chat reopened",
    )?;
    journey.screen("resumed", &["chat.row.turn-end"])?;

    // Settle again, then delete from its settled chat.
    journey.tap("chat.more")?;
    journey.wait_for(&["chat.menu.settle"])?;
    journey.tap("chat.menu.settle")?;
    journey.wait_inventory(
        "desk",
        |agents| {
            listed(agents, &id)
                .is_some_and(|agent| agent["lifecycle"] == EXITED && agent["filing"] == SETTLED)
        },
        "settled-again",
    )?;
    leave_chat(journey)?;
    journey.wait_for(&["home.settled-row"])?;
    journey.tap("home.settled-row")?;
    journey.wait_for(&["settled", "settled.row.helper"])?;
    journey.tap("settled.row.helper")?;
    journey.wait_for(&["chat.exited"])?;
    journey.tap("chat.more")?;
    journey.wait_for(&["chat.menu.delete"])?;
    journey.tap("chat.menu.delete")?;
    journey.wait_for(&["chat.delete.confirm"])?;
    journey.tap("chat.delete.confirm")?;

    journey.wait(
        |drawn| {
            !drawn.has("screen.conversation")
                && drawn.has("settled.empty")
                && !drawn.has("settled.row.helper")
        },
        "Settled empty after delete",
    )?;
    journey.wait_inventory("desk", |agents| listed(agents, &id).is_none(), "deleted")?;
    journey.screen("deleted", &[])?;

    Ok(vec![
        format!(
            "New Agent on the desk created Claude agent {named:?}, which answered {}",
            quoted(HELLO)
        ),
        "Rename from the chat menu updated both the chat header and the desk inventory to helper"
            .to_owned(),
        "Settle marked helper exited and settled on the desk with history kept and moved it to Settled"
            .to_owned(),
        control,
        "Resuming from the exited composer returned the same agent id to live/active with both prompts held once"
            .to_owned(),
        "Deleting the settled agent removed it from both the phone and the desk".to_owned(),
    ])
}

fn decide_plan_claude_sdk(journey: &mut AndroidJourney) -> Proved {
    let agent = "planner";
    journey.launch_fresh()?;
    pair_by_code(journey, "desk")?;
    open_agent(journey, agent)?;
    send(journey, PLAN_PROMPT)?;

    let card = no_plan_file(journey.wait(
        |drawn| plan_card(drawn) && labelled(drawn, "Delete the old journal."),
        "the plan and its decision",
    )?)?;
    journey.screen("plan", &[])?;

    send_back(journey, &card, PLAN_NOTE)?;
    no_plan_file(journey.wait(
        |drawn| {
            plan_card(drawn)
                && labelled(drawn, "Plan sent back")
                && labelled(drawn, PLAN_NOTE)
                && labelled(drawn, "Keep the old journal for a week.")
        },
        "the revised plan",
    )?)?;
    journey.screen("revised", &[])?;

    journey.tap("ask.approve")?;
    journey.wait(
        |drawn| !drawn.has("ask") && labelled(drawn, PLAN_DONE),
        "the approved plan's work",
    )?;
    let settled = journey.wait_chat(
        "desk",
        agent,
        |chat| phase(chat) == "IDLE" && super::holds(chat, PLAN_DONE),
        "plan-settled",
    )?;
    reflected_once(&settled, PLAN_PROMPT)?;
    plans_only(&settled)?;
    let mut extra = super::items(&settled).to_vec();
    extra.push(json!({"key": "toolu_write", "text": ""}));

    let decisions = control_responses(&journey.provider_input(agent, "plan-decisions")?)?;
    sent_back_then_approved(&decisions, PLAN_NOTE)?;
    let note_control =
        negative_control(sent_back_then_approved(&decisions, "a note never written"))?;
    let plan_control = negative_control(plans_only(&json!({ "items": extra })))?;

    no_plan_file(reopen(journey, agent, |drawn| {
        labelled(drawn, PLAN_DONE) && drawn.has("chat.row.turn-end")
    })?)?;
    journey.screen("approved", &["chat.row.turn-end"])?;

    Ok(vec![
        format!("{} reflected once in the desk's chat", quoted(PLAN_PROMPT)),
        "the plan showed in the chat with its decision in the ask card and no plan-file step"
            .to_owned(),
        "sent back with a note, a revised plan arrived; approved, the work ran".to_owned(),
        plan_control,
        "Claude received the plan sent back with the note, then one approval".to_owned(),
        note_control,
    ])
}

fn answer_questions(journey: &mut AndroidJourney) -> Proved {
    let asker = "asker";
    journey.launch_fresh()?;
    pair_by_code(journey, "desk")?;
    open_agent(journey, asker)?;
    send(journey, "Add a settings screen.")?;

    let first = |drawn: &Drawn| asking(drawn, "How should the settings screen be laid out?");
    journey.wait(first, "the first question")?;
    reopen_card(journey, asker, first)?;
    journey.screen("first-question", &[])?;

    // Pick Stacked -> preview updates.
    journey.tap("ask.option.1")?;
    journey.wait(
        |drawn| {
            drawn.field("ask.option.1", "selected") == true && labelled(drawn, "Palette  terminal")
        },
        "Stacked's preview",
    )?;
    journey.screen("stacked-preview", &[])?;
    journey.tap("ask.next")?;

    // Second question answered via Something else.
    journey.wait(
        |drawn| asking(drawn, "Where should the screen open from?"),
        "the second question",
    )?;
    journey.tap("ask.option.other")?;
    journey.wait_for(&["ask.other"])?;
    journey.type_text("ask.other", OTHER)?;
    journey.wait(|drawn| drawn.enabled("ask.next"), "an answer typed")?;
    journey.tap("ask.next")?;

    // Third question skipped -> review shows all three.
    journey.wait(
        |drawn| asking(drawn, SKIPPED) && drawn.has("ask.skip"),
        "the third question",
    )?;
    journey.tap("ask.skip")?;
    let review = journey.wait(
        |drawn| {
            drawn.has("ask.send")
                && labelled(drawn, "Stacked")
                && labelled(drawn, &format!("“{OTHER}”"))
                && labelled(drawn, "Skipped")
        },
        "the answers for review",
    )?;
    if !review.label("ask.review.2").contains("Skipped") {
        return Err(format!(
            "the review does not hold the third question skipped: {:?}",
            review.get("ask.review.2")
        ));
    }
    reopen_card(journey, asker, |drawn| drawn.has("ask.send"))?;
    journey.screen("review", &[])?;

    journey.tap("ask.send")?;
    let done = "Thanks, I'll build it that way.";
    journey.wait(
        |drawn| !drawn.has("ask") && labelled(drawn, done),
        "the reply",
    )?;
    journey.wait_chat("desk", asker, |chat| phase(chat) == "IDLE", "answered")?;
    reopen(journey, asker, |drawn| {
        labelled(drawn, done) && drawn.has("chat.row.turn-end")
    })?;
    journey.screen("answered", &["chat.row.turn-end"])?;

    // Second ask: reply instead.
    send(journey, PALETTE)?;
    journey.wait(
        |drawn| asking(drawn, "Which palette should it open in?") && drawn.has("ask.reply"),
        "the second ask",
    )?;
    journey.tap("ask.reply")?;
    journey.wait_for(&["ask.reply.text"])?;
    journey.type_text("ask.reply.text", QUESTION_REPLY)?;
    let written = |drawn: &Drawn| drawn.enabled("ask.reply.send");
    journey.wait(written, "a reply written")?;
    reopen_card(journey, asker, written)?;
    journey.screen("reply-instead", &[])?;
    journey.tap("ask.reply.send")?;

    let later = "I'll leave the palette for later.";
    journey.wait(
        |drawn| !drawn.has("ask") && labelled(drawn, later),
        "the reply to the reply",
    )?;
    journey.wait_chat(
        "desk",
        asker,
        |chat| phase(chat) == "IDLE" && prompts(chat, PALETTE).len() == 1,
        "replied",
    )?;
    reopen(journey, asker, |drawn| {
        labelled(drawn, later) && labelled(drawn, QUESTION_REPLY)
    })?;
    journey.screen("replied", &["chat.row.turn-end"])?;

    let decisions = control_responses(&journey.provider_input(asker, "answers")?)?;
    answered(&decisions, &answers())?;
    replied(&decisions, QUESTION_REPLY)?;
    let mut wrong = answers();
    wrong[SKIPPED] = json!("Relay");
    Ok(vec![
        "picking Stacked updated the preview above the options".to_owned(),
        "pick + Something else + skip showed together on review and sent exactly the two answers"
            .to_owned(),
        "a second ask replied to instead reached Claude as refused with the typed words".to_owned(),
        negative_control(answered(&decisions, &wrong))?,
        negative_control(replied(&decisions, "Pick Terminal."))?,
    ])
}

fn tool_server_asks(journey: &mut AndroidJourney) -> Proved {
    journey.launch_fresh()?;
    pair_by_code(journey, "desk")?;

    // Form ask on Claude.
    let filer = "filer";
    open_agent(journey, filer)?;
    send(journey, "File the reconnect flake.")?;
    journey.wait_for(&["ask.field.title", "ask.field.team", "ask.field.estimate"])?;
    reopen_card(journey, filer, |drawn| drawn.has("ask.field.title"))?;
    journey.screen("form", &[])?;

    journey.type_text("ask.field.title", FORM_TITLE)?;
    journey.tap("ask.field.team")?;
    let team_menu = journey.wait(
        |drawn| {
            drawn.iter().any(|(name, element)| {
                name.starts_with("ask.field.team.option.")
                    && element["label"].as_str() == Some("FOX")
            })
        },
        "FOX option in team dropdown",
    )?;
    let fox_tag = team_menu
        .iter()
        .find(|(name, element)| {
            name.starts_with("ask.field.team.option.") && element["label"].as_str() == Some("FOX")
        })
        .map(|(name, _)| name.to_owned())
        .ok_or("FOX option disappeared")?;
    journey.tap(&fox_tag)?;
    journey.type_text("ask.field.estimate", "3")?;
    let filled = |drawn: &Drawn| {
        drawn.enabled("ask.submit")
            && drawn.value("ask.field.title") == FORM_TITLE
            && drawn.label("ask.field.team").contains("FOX")
            && drawn.value("ask.field.estimate") == "3"
    };
    journey.wait(filled, "the form filled and ready to submit")?;
    reopen_card(journey, filer, filled)?;
    journey.screen("form-kept", &[])?;

    journey.tap("ask.submit")?;
    let filed = "Filed it in Linear.";
    journey.wait(
        |drawn| !drawn.has("ask") && labelled(drawn, filed),
        "the filed reply",
    )?;
    journey.wait_chat("desk", filer, |chat| phase(chat) == "IDLE", "filed")?;
    reopen(journey, filer, |drawn| {
        labelled(drawn, filed) && drawn.has("chat.row.turn-end")
    })?;
    journey.screen("form-sent", &["chat.row.turn-end"])?;
    let decisions = control_responses(&journey.provider_input(filer, "form-answer")?)?;
    form_sent(&decisions, &form())?;
    let mut wrong = form();
    wrong["team"] = json!("CORE");
    let control = negative_control(form_sent(&decisions, &wrong))?;

    // Link ask on Codex.
    leave_chat(journey)?;
    let linker = "linker";
    open_agent(journey, linker)?;
    send(journey, "Check the relay's error rate.")?;
    let card = journey.wait(
        |drawn| drawn.has("ask") && labelled(drawn, "grafana"),
        "the tool's ask",
    )?;
    journey.tap(&choice(&card, "Allow")?)?;
    journey.wait(
        |drawn| drawn.has("ask.open") && labelled(drawn, "https://grafana.example.com/login"),
        "the link ask",
    )?;
    reopen_card(journey, linker, |drawn| drawn.has("ask.open"))?;
    journey.screen("link", &[])?;
    let drawn = journey.elements()?;
    journey.tap(&choice(&drawn, "I’m done")?)?;
    let reply = "I'll read the dashboards another way.";
    journey.wait(
        |drawn| !drawn.has("ask") && labelled(drawn, reply),
        "the reply",
    )?;
    journey.wait_chat("desk", linker, |chat| phase(chat) == "IDLE", "linked")?;
    let results = codex_results(&journey.provider_input(linker, "link-answers")?)?;
    link_done(&results)?;
    reopen(journey, linker, |drawn| {
        labelled(drawn, reply) && drawn.has("chat.row.turn-end")
    })?;
    journey.screen("link-done", &["chat.row.turn-end"])?;

    Ok(vec![
        "the tool-server form preserved draft values across navigation and submitted exact field values"
            .to_owned(),
        control,
        "Codex tool permission was allowed and the follow-up link ask was answered with I’m done"
            .to_owned(),
        negative_control(link_done(&[results[0].clone(), json!({"action": "decline"})]))?,
    ])
}

fn queue_and_steer(journey: &mut AndroidJourney) -> Proved {
    let worker = "worker";
    journey.launch_fresh()?;
    pair_by_code(journey, "desk")?;
    open_agent(journey, worker)?;
    send(journey, RUN)?;
    journey.wait(
        |drawn| labelled(drawn, "scripts/soak --for 1h") && drawn.has("chat.activity"),
        "the long run",
    )?;

    send(journey, STEER)?;
    send(journey, WITHDRAWN)?;
    let both = |drawn: &Drawn| {
        drawn.label("chat.queued.0").contains(STEER)
            && drawn.label("chat.queued.0").contains("queued")
            && drawn.label("chat.queued.1").contains(WITHDRAWN)
            && drawn.label("chat.queued.1").contains("queued")
    };
    journey.wait(both, "two queued prompts")?;
    reopen(journey, worker, both)?;
    journey.screen("queued", &["chat.activity"])?;

    // Withdraw newest -> back in composer as draft, then clear it.
    journey.tap("chat.queued.1.withdraw")?;
    journey.wait(
        |drawn| {
            drawn.value("chat.field") == WITHDRAWN
                && drawn.label("chat.queued.0").contains(STEER)
                && !drawn.has("chat.queued.1")
        },
        "the withdrawn prompt back in the composer",
    )?;
    journey.clear("chat.field")?;
    journey.wait(
        |drawn| drawn.value("chat.field").is_empty(),
        "the draft cleared",
    )?;

    // Steer remaining queued prompt into the running turn.
    journey.tap("chat.queued.0.sendNow")?;
    let steered = |drawn: &Drawn| {
        drawn.label("chat.queued.0").contains(STEER)
            && drawn.label("chat.queued.0").contains("steered")
    };
    journey.wait(steered, "the prompt steered into the turn")?;
    reopen(journey, worker, steered)?;
    journey.screen("steered", &["chat.activity"])?;

    journey.request(open_gate("run-done"), None)?;
    journey.wait(
        |drawn| {
            !drawn.has("chat.queued.0")
                && labelled(drawn, "The run finished.")
                && !drawn.has("chat.activity")
        },
        "the turn's end",
    )?;
    let settled = journey.wait_chat("desk", worker, |chat| phase(chat) == "IDLE", "settled")?;
    reflected_once(&settled, RUN)?;
    reflected_once(&settled, STEER)?;
    never_sent(&settled, WITHDRAWN)?;
    let control = negative_control(never_sent(&settled, STEER))?;

    reopen(journey, worker, |drawn| {
        labelled(drawn, "The run finished.") && drawn.has("chat.row.turn-end")
    })?;
    journey.screen("finished", &["chat.row.turn-end"])?;

    Ok(vec![
        "two prompts sent while the agent worked queued under the feed".to_owned(),
        "withdrawn, the newest returned to the composer as a draft, was cleared, and never reached the desk"
            .to_owned(),
        format!(
            "steered, {} went into the running turn and the desk holds it once",
            quoted(STEER)
        ),
        control,
    ])
}

fn send_while_away(journey: &mut AndroidJourney) -> Proved {
    journey.launch_fresh()?;
    pair_by_code(journey, "cabin")?;
    open_agent(journey, "backups")?;

    journey.request(stop_daemon("cabin"), None)?;
    journey.wait(
        |drawn| labelled(drawn, "out of reach"),
        "cabin out of reach",
    )?;

    journey.type_text("chat.field", BACKUP)?;
    journey.wait(
        |drawn| kept_unsent(drawn, BACKUP),
        "the message kept unsent",
    )?;
    reopen(journey, "backups", |drawn| kept_unsent(drawn, BACKUP))?;
    journey.screen("away", &[])?;

    journey.request(restart_daemon("cabin"), None)?;
    journey.wait(
        |drawn| drawn.value("chat.field") == BACKUP && drawn.enabled("chat.send"),
        "Send enabled again with the draft kept",
    )?;
    journey.tap("chat.send")?;
    journey.wait(
        |drawn| labelled(drawn, BACKUP_DONE) && drawn.has("chat.row.turn-end"),
        "the reply",
    )?;
    let heard = journey.wait_chat(
        "cabin",
        "backups",
        |chat| phase(chat) == "IDLE" && prompts(chat, BACKUP).len() == 1,
        "sent",
    )?;
    reflected_once(&heard, BACKUP)?;
    let control = negative_control(never_sent(&heard, BACKUP))?;

    reopen(journey, "backups", |drawn| {
        labelled(drawn, BACKUP_DONE) && drawn.has("chat.row.turn-end")
    })?;
    journey.screen("sent", &["chat.row.turn-end"])?;

    Ok(vec![
        "with cabin out of reach, Send was disabled and the draft stayed unsent across leaving the chat"
            .to_owned(),
        format!(
            "with cabin back, Send delivered {} once and cabin answered",
            quoted(BACKUP)
        ),
        control,
    ])
}

const LIMIT: &str = "5-hour limit reached · resets ";
const SIGN_IN_CMD: &str = "Run claude and sign in with /login on desk.";
const SIGN_IN_RETRY: &str = "Signed in elsewhere? Send a message to try again.";

fn composer_limits(journey: &mut AndroidJourney) -> Proved {
    journey.launch_fresh()?;
    pair_by_code(journey, "desk")?;

    // Usage limit banner/placeholder over composer.
    let limit_named = |drawn: &Drawn| drawn.label("chat.field").starts_with(LIMIT);
    open_agent(journey, "limited")?;
    journey.wait(limit_named, "the usage limit named in the composer")?;
    reopen(journey, "limited", limit_named)?;
    journey.screen("limit", &["chat.field"])?;

    send(journey, "Try again.")?;
    journey.wait(
        |drawn| labelled(drawn, "Done after the limit.") && drawn.has("chat.row.turn-end"),
        "the reply past the limit",
    )?;
    let past = journey.wait_chat(
        "desk",
        "limited",
        |chat| phase(chat) == "IDLE" && prompts(chat, "Try again.").len() == 1,
        "limited-sent",
    )?;
    reflected_once(&past, "Try again.")?;
    let control = negative_control(reflected_once(&past, "Try again. (a wrong prompt)"))?;

    // Refused credential puts sign-in notice over a composer that still sends.
    leave_chat(journey)?;
    open_agent(journey, "expired")?;
    send(journey, "Check the links.")?;
    let asked = |drawn: &Drawn| {
        let notice = drawn.label("chat.foot.sign-in");
        notice.contains("Claude needs you to sign in")
            && notice.contains(SIGN_IN_CMD)
            && notice.contains(SIGN_IN_RETRY)
            && drawn.has("chat.field")
    };
    journey.wait(asked, "the sign-in notice over the composer")?;
    let refused = journey.wait_chat("desk", "expired", |chat| phase(chat) == "IDLE", "refused")?;
    reflected_once(&refused, "Check the links.")?;
    reopen(journey, "expired", asked)?;
    journey.screen("sign-in", &["chat.row.turn-end"])?;

    send(journey, "Try them again.")?;
    journey.wait(
        |drawn| labelled(drawn, "Links checked.") && !drawn.has("chat.foot.sign-in"),
        "the answer clearing the sign-in notice",
    )?;
    let retried = journey.wait_chat(
        "desk",
        "expired",
        |chat| phase(chat) == "IDLE" && prompts(chat, "Try them again.").len() == 1,
        "retried",
    )?;
    reflected_once(&retried, "Try them again.")?;

    Ok(vec![
        "at its usage limit the composer named the 5-hour window and reset time while still sending"
            .to_owned(),
        control,
        "refused its credential, Claude showed the sign-in notice naming desk; a subsequent reply cleared it"
            .to_owned(),
    ])
}

fn keep_authority(journey: &mut AndroidJourney) -> Proved {
    let asked = "Are you there?";
    let unsent = "Still there?";
    journey.launch_fresh()?;

    connect(journey, "ada")?;
    pair_through_the_relay(journey, "desk")?;
    journey.tap("tab.agents")?;
    journey.wait_for(&["home.row.guarded"])?;

    // Second account opens its own empty profile without the desk.
    journey.app(json!({
        "kind": "addAccount",
        "user": "bob",
        "token": "refresh-bob",
    }))?;
    journey.wait(
        |drawn| !drawn.has("home.row.guarded") && drawn.has("home"),
        "bob's empty fleet",
    )?;
    journey.tap("tab.hosts")?;
    let drawn = journey.wait(|drawn| drawn.has("hosts"), "bob's hosts")?;
    if drawn.has("hosts.row.desk") {
        return Err("bob's profile knows the desk".to_owned());
    }
    journey.screen("other-account", &[])?;

    // Switch back to ada via the account switcher sheet.
    journey.tap("tab.agents")?;
    journey.wait_for(&["home.account-menu"])?;
    journey.tap("home.account-menu")?;
    journey.wait_for(&["accounts.sheet", "accounts.row.ada@example.com"])?;
    journey.tap("accounts.row.ada@example.com")?;
    journey.wait_for(&["home.row.guarded"])?;

    // Through the relay the desk's agent answers.
    journey.tap("home.row.guarded")?;
    journey.wait_for(&["chat.field"])?;
    send(journey, asked)?;
    journey.wait(
        |drawn| labelled(drawn, "Only this account reaches me."),
        "the first reply",
    )?;
    let reached = journey.wait_chat("desk", "guarded", |chat| phase(chat) == "IDLE", "reached")?;
    reflected_once(&reached, asked)?;
    leave_chat(journey)?;

    // Sign out from Settings.
    open_settings(journey)?;
    journey.wait_for(&["settings.account.sign-out"])?;
    journey.tap("settings.account.sign-out")?;
    journey.wait(
        |drawn| {
            drawn
                .label("settings.account.row.ada@example.com")
                .contains("Signed out")
        },
        "ada signed out in Settings",
    )?;
    close_settings(journey)?;

    // Hosts tab & chat reflect signed-out state; draft stays unsent.
    journey.tap("tab.hosts")?;
    journey.wait(
        |drawn| labelled(drawn, "signed out") || labelled(drawn, "Signed out"),
        "Hosts tab naming the sign-out",
    )?;
    journey.screen("blocked-hosts", &[])?;

    journey.tap("tab.agents")?;
    journey.tap("home.row.guarded")?;
    journey.wait(
        |drawn| drawn.has("chat.field") && labelled(drawn, "This phone is signed out"),
        "the chat naming the sign-out",
    )?;
    journey.type_text("chat.field", unsent)?;
    journey.wait(
        |drawn| kept_unsent(drawn, unsent),
        "the message kept unsent while signed out",
    )?;
    std::thread::sleep(Duration::from_secs(2));
    reopen(journey, "guarded", |drawn| kept_unsent(drawn, unsent))?;
    journey.screen("blocked", &["chat.row.turn-end"])?;

    let blocked = journey.chat("desk", "guarded", "while-signed-out")?;
    never_received(&blocked, unsent)?;
    let control = negative_control(never_received(&blocked, asked))?;

    // Sign back in: draft stays in composer and Send delivers it once.
    connect(journey, "ada")?;
    journey.wait(
        |drawn| {
            !labelled(drawn, "This phone is signed out")
                && drawn.value("chat.field") == unsent
                && drawn.enabled("chat.send")
        },
        "Send enabled again with the draft kept",
    )?;
    journey.tap("chat.send")?;
    journey.wait(
        |drawn| labelled(drawn, "The relay carries us again."),
        "the reply after signing back in",
    )?;
    let after = journey.wait_chat(
        "desk",
        "guarded",
        |chat| phase(chat) == "IDLE" && prompts(chat, unsent).len() == 1,
        "sent-after-sign-in",
    )?;
    reflected_once(&after, asked)?;
    reflected_once(&after, unsent)?;
    reopen(journey, "guarded", |drawn| {
        labelled(drawn, "The relay carries us again.") && drawn.has("chat.row.turn-end")
    })?;
    journey.screen("recovered", &["chat.row.turn-end"])?;

    Ok(vec![
        "a second account opened on its own empty profile with no desk".to_owned(),
        format!(
            "through the relay the desk received {} once and answered",
            quoted(asked)
        ),
        "signed out, the phone marked the host away and kept the draft unsent; the desk never received it"
            .to_owned(),
        control,
        format!(
            "signed back in, {} stayed in the composer and Send delivered it once",
            quoted(unsent)
        ),
    ])
}

const FIRST: &str = "The recovery state is safely stored.";
const SECOND: &str = "A second turn arrived while you were away.";
const THIRD: &str = "Back after the restart.";
const DRAFT: &str = "Pick up where we left off.";

fn leave_and_recover(journey: &mut AndroidJourney) -> Proved {
    journey.launch_fresh()?;
    pair_by_code(journey, "desk")?;
    open_agent(journey, "keeper")?;
    journey.wait(
        |drawn| labelled(drawn, FIRST) && drawn.has("chat.row.turn-end"),
        "the first turn",
    )?;
    journey.quit()?;

    // Desk moves on while app is closed, then stops.
    journey.request(
        Control::Send {
            agent: "keeper".to_owned(),
            text: "Carry on.".to_owned(),
        },
        None,
    )?;
    journey.wait_chat(
        "desk",
        "keeper",
        |chat| phase(chat) == "IDLE" && super::holds(chat, SECOND),
        "desk-moved-on",
    )?;
    journey.request(stop_daemon("desk"), None)?;

    // Relaunch without clearing data: cached transcript renders offline with draft kept.
    journey.relaunch()?;
    open_agent(journey, "keeper")?;
    let drawn = journey.wait(
        |drawn| labelled(drawn, FIRST) && labelled(drawn, "out of reach"),
        "the cached chat out of reach",
    )?;
    if labelled(&drawn, SECOND) {
        return Err("the cached chat shows a turn the phone never received".to_owned());
    }
    journey.type_text("chat.field", DRAFT)?;
    let drawn = journey.wait(
        |drawn| kept_unsent(drawn, DRAFT),
        "the draft kept unsent offline",
    )?;
    if composer_current(&drawn) {
        return Err("the chat reads current with the desk out of reach".to_owned());
    }
    let detached = reopen(journey, "keeper", |drawn| kept_unsent(drawn, DRAFT))?;
    if !labelled(&detached, "out of reach") || labelled(&detached, SECOND) {
        return Err(
            "reopened offline chat lost out-of-reach state or fabricated missed turn".to_owned(),
        );
    }
    journey.screen("cached-offline", &["chat.row.turn-end"])?;

    // Desk returns: missed turn appends once and chat becomes current with draft kept.
    journey.request(restart_daemon("desk"), None)?;
    let drawn = journey.wait(
        |drawn| {
            labelled(drawn, SECOND)
                && drawn.value("chat.field") == DRAFT
                && drawn.enabled("chat.send")
        },
        "the missed turn arrived and draft ready to send",
    )?;
    if replies(&drawn, SECOND) != 1 {
        return Err("the missed turn did not append exactly once".to_owned());
    }
    journey.tap("chat.send")?;
    journey.wait(
        |drawn| labelled(drawn, THIRD),
        "the reply to the kept draft",
    )?;
    let after = journey.wait_chat(
        "desk",
        "keeper",
        |chat| phase(chat) == "IDLE" && super::holds(chat, THIRD),
        "sent-after-return",
    )?;
    reflected_once(&after, DRAFT)?;
    let control = negative_control(reflected_once(&after, &format!("{DRAFT} (a wrong draft)")))?;
    reopen(journey, "keeper", |drawn| {
        labelled(drawn, THIRD) && drawn.has("chat.row.turn-end")
    })?;
    journey.screen("reconciled", &["chat.row.turn-end"])?;

    // Desk checkpoints and rewinds -> Reset swaps in exited chat without dropping rows.
    journey.request(
        Control::Checkpoint {
            host: "desk".to_owned(),
        },
        None,
    )?;
    journey.request(
        Control::Rewind {
            host: "desk".to_owned(),
            cuts: Vec::new(),
        },
        None,
    )?;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let drawn = journey.elements()?;
        if !(labelled(&drawn, FIRST) && labelled(&drawn, THIRD)) {
            return Err("rows left the screen during Reset".to_owned());
        }
        if drawn.has("chat.exited") {
            break;
        }
        if Instant::now() > deadline {
            return Err("the rebuilt chat never swapped in after Reset".to_owned());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    reopen(journey, "keeper", |drawn| {
        labelled(drawn, THIRD) && drawn.has("chat.exited")
    })?;
    journey.screen("after-reset", &["chat.row.turn-end"])?;

    Ok(vec![
        "relaunched offline, the app painted the cached chat without the unseen turn and kept the draft"
            .to_owned(),
        "when desk returned, the missed turn appended once and the kept draft was sent once"
            .to_owned(),
        control,
        "rows stayed on screen through desk rewind and Reset until the rebuilt exited chat swapped in"
            .to_owned(),
    ])
}

const REFUSED_SIGN_IN: &str = "amux.sh could not sign this account in";
const RELAYED: &str = "Are you there?";
const THROUGH_RELAY: &str = "Reached through the relay.";

fn account_sign_in(journey: &mut AndroidJourney) -> Proved {
    journey.launch_fresh()?;
    open_settings(journey)?;
    journey.wait_for(&["settings.accounts.sign-in"])?;
    journey.screen("signed-out", &[])?;

    // Cancelled browser leaves Sign In offered with no error dialog.
    relay_cloud(journey, json!({"signIn": "cancelled"}))?;
    journey.tap("settings.accounts.sign-in")?;
    asked_once(journey, "signIn select")?;
    let drawn = journey.wait_for(&["settings.accounts.sign-in"])?;
    if drawn.has("account.sign-in-result.dialog") {
        return Err("cancelled sign-in raised an error dialog".to_owned());
    }

    // Refused browser shows error dialog in the account service's words, and Try Again retries.
    relay_cloud(
        journey,
        json!({"signIn": "refused", "reason": REFUSED_SIGN_IN}),
    )?;
    journey.tap("settings.accounts.sign-in")?;
    journey.wait(
        |drawn| {
            drawn.has("account.sign-in-result.dialog")
                && drawn
                    .label("account.sign-in-result.body")
                    .contains(REFUSED_SIGN_IN)
        },
        "the sign-in refusal dialog",
    )?;
    journey.screen("refused", &[])?;

    relay_cloud(journey, json!({}))?;
    journey.tap("account.sign-in-result.try-again")?;
    journey.wait(
        |drawn| {
            !drawn.has("account.sign-in-result.dialog")
                && (drawn.has("settings.account.row.ada@example.com")
                    || drawn.label("home.account-menu").contains("AD"))
        },
        "ada signed in",
    )?;
    if !journey.elements()?.has("screen.settings") {
        open_settings(journey)?;
    }
    journey.wait(
        |drawn| {
            drawn.has("settings.account.row.ada@example.com")
                && !drawn
                    .label("settings.account.row.ada@example.com")
                    .contains("Signed out")
        },
        "ada signed in in Settings",
    )?;
    journey.screen("signed-in", &[])?;
    close_settings(journey)?;

    let asked = cloud_calls(journey, "account-service-calls")?;
    let selects = asked.iter().filter(|call| *call == "signIn select").count();
    if selects != 3 || !asked.iter().any(|call| call == "handOver ada") {
        return Err(format!(
            "expected 3 signIn select calls and handOver ada, got: {asked:?}"
        ));
    }

    // Pair and chat through the relay.
    pair_through_the_relay(journey, "desk")?;
    journey.tap("tab.hosts")?;
    journey.wait_for(&["hosts.row.desk"])?;
    journey.screen("hosts", &[])?;
    open_agent(journey, "desk-work")?;
    send(journey, RELAYED)?;
    journey.wait(
        |drawn| labelled(drawn, THROUGH_RELAY) && drawn.has("chat.row.turn-end"),
        "relayed reply on the phone",
    )?;
    let heard = journey.wait_chat(
        "desk",
        "desk-work",
        |chat| phase(chat) == "IDLE" && prompts(chat, RELAYED).len() == 1,
        "relayed-heard",
    )?;
    reflected_once(&heard, RELAYED)?;
    reopen(journey, "desk-work", |drawn| {
        labelled(drawn, THROUGH_RELAY) && drawn.has("chat.row.turn-end")
    })?;
    journey.screen("reached", &["chat.row.turn-end"])?;
    leave_chat(journey)?;

    // Sign out, sign back in as ada, browser returns grace -> mismatch dialog -> Cancel discards grace.
    open_settings(journey)?;
    journey.wait_for(&["settings.account.sign-out"])?;
    journey.tap("settings.account.sign-out")?;
    journey.wait(
        |drawn| {
            drawn
                .label("settings.account.row.ada@example.com")
                .contains("Signed out")
        },
        "ada signed out",
    )?;
    relay_cloud(journey, json!({"account": "grace"}))?;
    journey.tap("settings.account.sign-in-again.ada@example.com")?;
    journey.wait(
        |drawn| {
            drawn.has("account.sign-in-result.dialog")
                && drawn
                    .label("account.sign-in-result.body")
                    .contains("grace@example.com")
                && drawn
                    .label("account.sign-in-result.body")
                    .contains("ada@example.com")
        },
        "the mismatched account dialog",
    )?;
    journey.screen("another-account", &[])?;
    journey.tap("account.sign-in-result.dismiss")?;
    journey.wait(
        |drawn| {
            !drawn.has("account.sign-in-result.dialog")
                && drawn
                    .label("settings.account.row.ada@example.com")
                    .contains("Signed out")
                && !drawn.has("settings.account.row.grace@example.com")
        },
        "mismatch cancelled without keeping grace",
    )?;
    asked_once(journey, "forgetSession grace")?;
    let after = cloud_calls(journey, "account-service-calls-after-mismatch")?;
    for expected in ["signIn hint ada@example.com", "forgetSession grace"] {
        if !after.iter().any(|call| call == expected) {
            return Err(format!("missing cloud call {expected:?}: {after:?}"));
        }
    }
    if after.iter().any(|call| call == "handOver grace") {
        return Err(format!("grace was handed over despite cancel: {after:?}"));
    }
    close_settings(journey)?;

    Ok(vec![
        "cancelled sign-in showed no alert; refused sign-in showed the service's words and Try Again succeeded"
            .to_owned(),
        format!(
            "relayed pairing reached desk and delivered {} once",
            quoted(RELAYED)
        ),
        "mismatched re-sign-in named both accounts and Cancel discarded grace's session".to_owned(),
    ])
}

fn one_tap_sign_in(journey: &mut AndroidJourney) -> Proved {
    journey.launch_fresh()?;
    pair_by_code(journey, "desk")?;
    journey.screen("paired", &[])?;

    journey.request(
        Control::OfferSignIn {
            host: "desk".to_owned(),
            to: None,
        },
        Some("offer"),
    )?;
    journey.wait(
        |drawn| {
            drawn.has("account.sign-in-offer.sheet")
                && drawn
                    .label("account.sign-in-offer.title")
                    .contains("ada@example.com")
                && drawn.label("account.sign-in-offer.detail").contains("desk")
        },
        "the desk's sign-in offer sheet",
    )?;
    journey.screen("offered", &[])?;

    journey.tap("account.sign-in-offer.accept")?;
    journey.wait(
        |drawn| !drawn.has("account.sign-in-offer.sheet"),
        "the sign-in offer sheet dismissed",
    )?;

    open_settings(journey)?;
    journey.wait(
        |drawn| {
            drawn.has("settings.account.row.ada@example.com")
                && !drawn
                    .label("settings.account.row.ada@example.com")
                    .contains("Signed out")
        },
        "ada signed in in Settings",
    )?;
    journey.screen("signed-in", &[])?;
    close_settings(journey)?;

    journey.tap("tab.hosts")?;
    journey.wait_for(&["hosts.row.desk"])?;

    Ok(vec![
        "unauthenticated phone paired with signed-in desk over LAN PIN".to_owned(),
        "desk's account offer raised SignInOfferSheet naming ada@example.com and desk".to_owned(),
        "one tap on Accept signed the phone in as ada@example.com and preserved desk in Computers"
            .to_owned(),
    ])
}

const ACCESSIBLE_FONT_SCALE: f64 = 1.5;

fn accessibility(journey: &mut AndroidJourney) -> Proved {
    journey.launch_fresh()?;
    journey.app(json!({"kind": "fontScale", "scale": ACCESSIBLE_FONT_SCALE}))?;
    let reported = journey.query()?["fontScale"].as_f64().unwrap_or_default();
    if (reported - ACCESSIBLE_FONT_SCALE).abs() > 0.05 {
        return Err(format!(
            "expected fontScale ~{ACCESSIBLE_FONT_SCALE}, got {reported}"
        ));
    }

    pair_by_code(journey, "desk")?;
    open_agent(journey, "decision-sdk")?;
    journey.screen("chat", &[])?;

    send(journey, PROMPT)?;
    let card = journey.wait(
        |drawn| drawn.has("ask") && labelled(drawn, "deploy --check") && labelled(drawn, "Allow"),
        "the permission card at large text",
    )?;
    journey.screen("permission", &[])?;

    journey.tap(&choice(&card, "Allow")?)?;
    journey.wait(
        |drawn| !drawn.has("ask") && labelled(drawn, REPLY),
        "the settled turn at large text",
    )?;
    let settled = journey.wait_chat(
        "desk",
        "decision-sdk",
        |chat| phase(chat) == "IDLE" && super::holds(chat, REPLY),
        "turn-settled",
    )?;
    reflected_once(&settled, PROMPT)?;
    let lines = journey.provider_input("decision-sdk", "provider-input")?;
    if lines
        .iter()
        .filter(|line| line.to_lowercase().contains("allow"))
        .count()
        != 1
    {
        return Err(format!("the provider did not receive one allow: {lines:?}"));
    }

    reopen(journey, "decision-sdk", |drawn| {
        labelled(drawn, REPLY) && drawn.has("chat.row.turn-end")
    })?;
    journey.screen("settled", &["chat.row.turn-end"])?;

    // Hosts and Settings at 1.5x font scale: every control inside viewport & appearance choices clear.
    leave_chat(journey)?;
    journey.tap("tab.hosts")?;
    let hosts = journey.wait_for(&["hosts.row.desk"])?;
    across_the_screen(&hosts, "hosts.")?;
    journey.screen("hosts", &[])?;

    open_settings(journey)?;
    let choices = [
        "settings.appearance.light",
        "settings.appearance.dark",
        "settings.appearance.system",
    ];
    let settings = journey.wait_for(&choices)?;
    across_the_screen(&settings, "settings.")?;
    apart(&settings, &choices)?;
    let control = negative_control(apart(&settings, &["settings.appearance", choices[0]]))?;
    journey.screen("settings", &[])?;

    Ok(vec![
        format!("app drew at fontScale={ACCESSIBLE_FONT_SCALE}"),
        "at 1.5x text scale, desk paired, permission ask allowed once, and desk finished the turn"
            .to_owned(),
        "every control on Hosts and Settings lies cleanly inside the viewport and Appearance choices are apart"
            .to_owned(),
        control,
    ])
}

const REPORT_NOTE: &str = "The desk's agent is listed twice.";
const REFUSED_UPLOAD: &str = "amux.sh could not take this report";

fn report(journey: &mut AndroidJourney) -> Proved {
    journey.launch_fresh()?;
    connect(journey, "ada")?;
    pair_through_the_relay(journey, "desk")?;
    let agent_id = journey.agent_id("desk-work");
    journey.tap("tab.agents")?;
    journey.wait_for(&["home.row.desk-work"])?;

    journey.request(
        Control::RefuseReports {
            reason: Some(REFUSED_UPLOAD.to_owned()),
        },
        None,
    )?;

    // Screenshot callback raises the non-blocking Report offer banner.
    journey.system_screenshot()?;
    journey.wait_for(&["report.prompt", "report.prompt.open"])?;
    journey.screen("offer", &[])?;
    journey.tap("report.prompt.open")?;
    journey.wait(
        |drawn| {
            drawn.has("report.screen")
                && drawn.has("report.note")
                && !drawn.has("report.frame.preparing")
                && drawn.enabled("report.send")
        },
        "the report overlay with frozen frame ready",
    )?;
    journey.type_text("report.note", REPORT_NOTE)?;
    journey.wait(
        |drawn| drawn.value("report.note") == REPORT_NOTE,
        "the report note written",
    )?;

    // Refused upload preserves the written note and surfaces the service's words.
    journey.app(json!({"kind": "settle"}))?;
    journey.tap("report.send")?;
    let refused = journey.wait_for(&["report.refusal"])?;
    if !refused.label("report.refusal").contains(REFUSED_UPLOAD) {
        return Err(format!(
            "the refusal says {:?}",
            refused.get("report.refusal")
        ));
    }
    if journey.elements()?.value("report.note") != REPORT_NOTE {
        return Err("the refusal lost the note".to_owned());
    }
    journey.screen("refused", &[])?;

    // Retry hands over the exact same sealed report and receives receipt report-1.
    journey.request(Control::RefuseReports { reason: None }, None)?;
    journey.tap("report.send")?;
    let sent = journey.wait_for(&["report.sent", "report.receipt.id"])?;
    if sent.label("report.receipt.id") != "report-1" {
        return Err(format!(
            "the receipt reads {:?}",
            sent.get("report.receipt.id")
        ));
    }
    journey.screen("sent", &[])?;

    // Inspect what the account service received on the wire.
    let received = journey.output.join("received");
    let listing = journey.request(
        Control::Reports {
            dir: Some(received.clone()),
        },
        Some("received-reports"),
    )?;
    let reports = listing["reports"].as_array().cloned().unwrap_or_default();
    let [first, retry] = reports.as_slice() else {
        return Err(format!(
            "the account service was handed {} reports",
            reports.len()
        ));
    };
    if first["refused"] != REFUSED_UPLOAD || !retry["refused"].is_null() {
        return Err(format!(
            "the refusals read {} and {}",
            first["refused"], retry["refused"]
        ));
    }
    if first["report_json"] != retry["report_json"] || first["files"] != retry["files"] {
        return Err("the retry did not hand over the same report".to_owned());
    }
    let files = strings(&retry["files"]);
    if files.is_empty() {
        return Err("the report carries no files".to_owned());
    }
    for name in &files {
        let first_bytes = std::fs::read(received.join("1").join(name))
            .map_err(|error| format!("reading first report file {name}: {error}"))?;
        let retry_bytes = std::fs::read(received.join("2").join(name))
            .map_err(|error| format!("reading retry report file {name}: {error}"))?;
        if first_bytes != retry_bytes {
            return Err(format!(
                "retry uploaded different bytes for {name} ({} vs {} bytes)",
                first_bytes.len(),
                retry_bytes.len()
            ));
        }
    }
    let header = &retry["report_json"];
    if retry["account"] != "ada" || header["kind"] != "report" {
        return Err(format!("the report reads {retry}"));
    }
    if header["note"] != REPORT_NOTE {
        return Err(format!("the report does not carry the note: {header}"));
    }
    let screen = received.join("2/frame.png");
    if std::fs::metadata(&screen).map_or(true, |metadata| metadata.len() == 0) {
        return Err(format!(
            "the report carries no picture of the screen: {files:?}"
        ));
    }
    std::fs::copy(&screen, journey.output.join("uploaded-frame.png"))
        .map_err(|error| error.to_string())?;
    let dumped = strings(&retry["dump_files"]);
    for part in ["core/checkpoint", "core/store.sqlite"] {
        if !dumped.iter().any(|file| file == part) {
            return Err(format!("the report's dump holds no {part}: {dumped:?}"));
        }
    }
    journey.wait_inventory(
        "desk",
        |agents| agents.iter().any(|item| item["id"] == agent_id.as_str()),
        "desk-lists-the-reported-agent",
    )?;
    if journey.elements()?.has("report.done") {
        journey.tap("report.done")?;
    }

    Ok(vec![
        "ScreenCaptureCallback offered Report over the frozen frame; the report opened and took a note"
            .to_owned(),
        format!(
            "amux.sh refused the report ({}) without losing the note",
            quoted(REFUSED_UPLOAD)
        ),
        format!(
            "Retry uploaded the identical sealed report ({}) and showed receipt report-1",
            files.join(", ")
        ),
        "server received the note, non-empty frame.png, and Android core dump".to_owned(),
    ])
}

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
                "identifier": "settings.phone.fingerprint",
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
                "settings.phone.fingerprint | <volatile>",
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
