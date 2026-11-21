//! The Android journey driver: starts a served topology with `testnet serve`,
//! installs the debug APK fresh on the pinned Android emulator (`amux-android-1`),
//! forwards the debug door over `adb forward`, launches `MainActivity` with the
//! served network's configuration, drives the real app through its Compose
//! semantics tree via the NDJSON door, checks host/agent state, and captures
//! emulator screenshots into `target/journeys/android/<story>/actual/`.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead as _, BufReader};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};

use super::phone::{AppError, Drawn, Printing, ask, normalize, printed_pin, run_within};
use super::{Proved, Record, door, root};
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

type Tell = fn(&mut AndroidJourney) -> Proved;

const STORIES: &[(&str, Tell)] = &[("reach-host", reach_host)];

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

fn shell_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', "'\\''"))
}

fn rust_log() -> String {
    std::env::var("RUST_LOG").unwrap_or_else(|_| RUST_LOG.to_owned())
}

#[derive(Clone)]
struct UndoState {
    adb: PathBuf,
    serial: String,
    host_port: u16,
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
    topology: Value,
    binaries: Binaries,
    _lock: File,
    process: Option<Child>,
    ready: Readiness,
    record: Record,
    host_port: u16,
    door_token: String,
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
            topology,
            binaries,
            _lock: lock,
            process: Some(process),
            ready,
            record,
            host_port: 0,
            door_token: String::new(),
        })
    }

    fn adb(&self, args: &[&str], timeout: Duration) -> Result<String, String> {
        let mut command = Command::new(&self.adb);
        command.args(["-s", &self.serial]).args(args);
        run_within(command, timeout)
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
        if let Some(undo) = UNDO.get()
            && let Ok(mut held) = undo.lock()
        {
            *held = Some(UndoState {
                adb: self.adb.clone(),
                serial: self.serial.clone(),
                host_port: self.host_port,
                control: self.ready.control,
            });
        }

        let scope = self.topology["scope"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        self.door_token = uuid::Uuid::new_v4().to_string();
        let start_config = build_start_config(&self.ready, &scope, true);
        let start_config_json =
            serde_json::to_string(&start_config).map_err(|error| error.to_string())?;
        let launch_cmd = format!(
            "am start -W -n {ACTIVITY} --ei amux_door_port {DEVICE_DOOR_PORT} --es amux_door_token {} --es amux_start_config {}",
            shell_quote(&self.door_token),
            shell_quote(&start_config_json)
        );
        self.adb(&["shell", &launch_cmd], Duration::from_secs(60))?;
        self.record.actions.push(format!(
            "launched {ACTIVITY} with door port {}",
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

    fn app(&self, mut request: Value) -> Result<Value, AppError> {
        if let Some(obj) = request.as_object_mut() {
            obj.insert(
                "door_token".to_owned(),
                Value::String(self.door_token.clone()),
            );
        }
        ask(self.door(), &request, Duration::from_secs(90))
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

    fn host_id(&self, host: &str) -> String {
        self.ready
            .hosts
            .iter()
            .find(|ready| ready.name == host)
            .map(|ready| ready.host_id.to_string())
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

    fn capture_screen(&mut self, label: &str) -> Result<PathBuf, String> {
        self.app(json!({"kind": "settle"}))?;
        let state = self.query()?;
        let elements = state["elements"].as_array().map_or(&[][..], Vec::as_slice);
        let actual = self.output.join("actual");
        std::fs::create_dir_all(&actual).map_err(|error| error.to_string())?;

        let png_path = actual.join(format!("{label}.png"));
        self.capture_png(&png_path)?;

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
        let geometry = format_elements(elements, &ids);
        std::fs::write(actual.join(format!("{label}.elements.txt")), &geometry)
            .map_err(|error| error.to_string())?;
        self.record.actions.push(format!("captured {label}"));
        Ok(png_path)
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
        // Save logcat tail for rapid diagnosis.
        if let Ok(logcat) = self.adb(&["logcat", "-d", "-t", "200"], Duration::from_secs(15)) {
            let _ = std::fs::write(self.output.join("logcat.txt"), logcat);
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

fn build_start_config(ready: &Readiness, scope: &str, fresh_start: bool) -> Value {
    let mut obj = json!({
        "device_name": PINNED_AVD,
        "discovery_scope": scope,
        "lan": true,
        "lan_bind": "0.0.0.0:0",
        "scripted_cloud": ready.cloud_url.is_some(),
        "use_system_discovery": false,
        "fresh_start": fresh_start,
    });
    if let Some(tcp) = ready.relay_tcp {
        obj["relay_tcp"] = json!(rewrite_for_emulator(tcp));
    }
    if let Some(quic) = ready.relay_quic {
        obj["relay_quic"] = json!(rewrite_for_emulator(quic));
    }
    if let Some(root) = &ready.relay_root {
        obj["relay_root"] = json!(root);
    }
    obj
}

/// Whether an element can be acted on: matches `phone.rs::enabled`
/// (a missing `enabled` means it can, a null one that it cannot).
fn element_enabled(element: &Value) -> bool {
    match element.get("enabled") {
        None => true,
        Some(Value::Bool(enabled)) => *enabled,
        Some(Value::Null) => false,
        Some(_) => true,
    }
}

fn format_elements(elements: &[Value], ids: &BTreeMap<String, String>) -> String {
    let mut lines = Vec::new();
    for element in elements {
        let name = element["identifier"].as_str().unwrap_or_default();
        let frame = &element["frame"];
        let coords: Vec<String> = ["x", "y", "width", "height"]
            .iter()
            .map(|key| {
                (frame[key].as_f64().unwrap_or_default().round_ties_even() as i64).to_string()
            })
            .collect();
        let words = normalize(&format!(
            "{} | {}",
            element["label"].as_str().unwrap_or_default(),
            element["value"].as_str().unwrap_or_default()
        ));
        let state = if element_enabled(element) {
            ""
        } else {
            " (disabled)"
        };
        let mut line = format!("{name} | {words} | {}{state}", coords.join(","));
        for (id, friendly) in ids {
            line = line.replace(id, &format!("<{friendly}>"));
        }
        lines.push(line);
    }
    lines.join("\n") + "\n"
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

    journey.launch_fresh()?;
    journey.app(json!({"kind": "hpkeSelfCheck"}))?;
    journey
        .record
        .actions
        .push("Conscrypt HPKE self-check passed".to_owned());
    journey.wait_for(&["tab.agents", "tab.hosts"])?;

    // Switch to the Computers tab and announce the desk host via the door's
    // discovery hook pointing at the host loopback QUIC listener via 10.0.2.2.
    journey.tap("tab.hosts")?;
    journey.wait_for(&["hosts"])?;
    journey.discovered(json!([{
        "host_id": desk_id,
        "name": "desk",
        "addrs": [desk_addr],
        "scope": scope,
        "pairing_open": false,
    }]))?;
    journey.wait_for(&["hosts.found.desk"])?;
    journey.capture_screen("found-not-paired")?;

    // Open a real pairing window on the served desk daemon and read the PIN it prints.
    let mut pairing = journey.amux("desk", &["pair"])?;
    let pin = printed_pin(&mut pairing)?;
    journey.discovered(json!([{
        "host_id": desk_id,
        "name": "desk",
        "addrs": [desk_addr],
        "scope": scope,
        "pairing_open": true,
    }]))?;

    // The Android shell raises the pairing bottom sheet automatically.
    journey.wait_for(&["pairing.sheet", "pairing.digits"])?;
    journey.capture_screen("pair-offered")?;

    // Type the 6-digit PIN through the live Compose semantics field.
    journey.type_text("pairing.digits", &pin)?;
    let confirm = journey.wait_for(&["pairing.confirm", "pairing.fingerprint"])?;
    let title = confirm.label("pairing.title");
    if !title.contains("desk") {
        return Err(format!(
            "pairing confirmation title was {title:?}, expected desk"
        ));
    }
    journey.capture_screen("pair-confirm")?;

    // Confirm trust and wait for the sheet to close and the desk to appear paired.
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
    journey.capture_screen("paired-hosts")?;

    let host_label = paired_hosts.label("hosts.row.desk").to_owned();

    // Switch to the Agents tab and verify the desk's real agent appears via the QUIC link.
    journey.tap("tab.agents")?;
    let agents = journey.wait_for(&["home.row.desk-work"])?;
    let agent_label = agents.label("home.row.desk-work").to_owned();
    journey.capture_screen("paired-agents")?;

    // Tap the agent row to verify navigation and press system Back to return.
    journey.tap("home.row.desk-work")?;
    journey.wait_for(&["screen.conversation"])?;
    journey.capture_screen("conversation")?;
    journey.back()?;
    journey.wait(
        |drawn| drawn.has("home.row.desk-work") && !drawn.has("screen.conversation"),
        "returned to Agents home via back",
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
        "tapping desk-work opened the conversation route and system Back returned to Agents"
            .to_owned(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_loopback_addresses_to_emulator_host_alias() {
        let loopback: SocketAddr = "127.0.0.1:53990".parse().unwrap();
        assert_eq!(rewrite_for_emulator(loopback), "10.0.2.2:53990");
        let lan: SocketAddr = "192.168.1.40:53990".parse().unwrap();
        assert_eq!(rewrite_for_emulator(lan), "192.168.1.40:53990");
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
    fn start_config_uses_wildcard_lan_bind_for_emulator_routing() {
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
        let config = build_start_config(&ready, "phone-reach", true);
        assert_eq!(config["lan_bind"], "0.0.0.0:0");
        assert_eq!(config["relay_tcp"], "10.0.2.2:8");
        assert_eq!(config["relay_quic"], "10.0.2.2:7");
        assert_eq!(config["discovery_scope"], "phone-reach");
        assert_eq!(config["use_system_discovery"], false);
        assert_eq!(config["fresh_start"], true);
    }
}
