//! The folder a session runs in: the net it started, the log of every
//! command, and the commands that start and stop the net and run the real
//! CLI against a host.

use std::io::{Read as _, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::door;

/// What a command did: the words to print, the raw value for `--json`, and
/// any files it wrote, for the session log.
pub struct Outcome {
    pub text: String,
    pub value: Value,
    pub files: Vec<PathBuf>,
}

impl Outcome {
    pub fn new(text: impl Into<String>, value: Value) -> Self {
        Self {
            text: text.into(),
            value,
            files: Vec::new(),
        }
    }

    pub fn with_files(mut self, files: Vec<PathBuf>) -> Self {
        self.files = files;
        self
    }
}

/// What `ctl up` records for the commands after it.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct Record {
    pub pid: u32,
    pub control: SocketAddr,
    pub root: PathBuf,
    pub scope: String,
    pub readiness: Value,
}

/// The session's folder, and where the command was run from.
pub struct Folder {
    pub dir: PathBuf,
    /// The current folder, against which a relative file a command reads
    /// is found first.
    cwd: PathBuf,
}

/// Names the session's folder whatever the current folder is: a QA
/// tester's own, set by the launcher, so a tester that changed folder still
/// reaches its net.
pub const FOLDER_ENV: &str = "TESTNET_FOLDER";

/// The net's own files, inside the session folder.
const NET_DIR: &str = ".testnet";
/// The size of every agent's facts ring on a session's net: large enough
/// that a ring does not rotate during a session, so a dump holds each
/// agent's whole life.
const FACTS_RING_MIB: u64 = 64;

impl Folder {
    /// The folder [`FOLDER_ENV`] names, or else the current one.
    pub fn here() -> Result<Self, String> {
        let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        let dir = match std::env::var_os(FOLDER_ENV).filter(|dir| !dir.is_empty()) {
            Some(dir) => {
                let dir = cwd.join(dir);
                if !dir.is_dir() {
                    return Err(format!(
                        "{FOLDER_ENV} names {}, which is not a folder",
                        dir.display()
                    ));
                }
                dir
            }
            None => cwd.clone(),
        };
        Ok(Self { dir, cwd })
    }

    /// A session in `dir`, run from there.
    #[cfg(test)]
    pub(crate) fn at(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            cwd: dir.to_path_buf(),
        }
    }

    /// A file a command reads: as given when absolute; else where the
    /// command was run from, when it is there; else in the session folder.
    pub(crate) fn input(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            return path.to_path_buf();
        }
        let here = self.cwd.join(path);
        if here.exists() {
            here
        } else {
            self.dir.join(path)
        }
    }

    pub(crate) fn net_dir(&self) -> PathBuf {
        self.dir.join(NET_DIR)
    }

    pub(crate) fn record(&self) -> Result<Record, String> {
        let path = self.net_dir().join("net.json");
        let text = std::fs::read_to_string(&path).map_err(|_| {
            format!(
                "no net in {}: start one with `testnet ctl up`, and run ctl from the folder you \
                 started it in (or name that folder in {FOLDER_ENV})",
                self.dir.display()
            )
        })?;
        serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
    }

    /// One door request with the usual patience.
    pub(crate) fn call(&self, request: Value) -> Result<Value, String> {
        door::call(self.record()?.control, &request, super::DOOR_PATIENCE)
    }

    /// One door request with the patience it needs.
    pub(crate) fn call_within(&self, request: Value, patience: Duration) -> Result<Value, String> {
        door::call(self.record()?.control, &request, patience)
    }

    /// One door request that may take as long as a real provider does.
    pub(crate) fn call_long(&self, request: Value) -> Result<Value, String> {
        door::call(self.record()?.control, &request, Duration::from_secs(600))
    }

    /// The net as it stands now.
    pub(crate) fn status(&self) -> Result<Value, String> {
        self.call(json!({"Status": {}}))
    }

    /// A host's entry in the status.
    pub(crate) fn host(&self, name: &str) -> Result<Value, String> {
        let status = self.status()?;
        status["hosts"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|host| host["name"] == name)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "no host {name:?}; the net has {}",
                    names(&status["hosts"]).join(", ")
                )
            })
    }

    /// Appends the command and what came of it to `session.jsonl`, when
    /// this folder holds a net or the command started one.
    pub fn log(&self, arguments: &[String], outcome: &Result<Outcome, String>) {
        if !self.net_dir().exists() {
            return;
        }
        let line = match outcome {
            Ok(done) => json!({
                "time": chrono::Utc::now().to_rfc3339(),
                "args": arguments,
                "ok": true,
                "reply": done.text,
                "files": done.files,
            }),
            Err(error) => json!({
                "time": chrono::Utc::now().to_rfc3339(),
                "args": arguments,
                "ok": false,
                "reply": error,
                "files": [],
            }),
        };
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("session.jsonl"))
        {
            let _ = writeln!(file, "{line}");
        }
    }
}

pub(crate) fn names(hosts: &Value) -> Vec<String> {
    hosts
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|host| host["name"].as_str().map(str::to_owned))
        .collect()
}

/// The `testnet` and `amux` binaries this session uses: beside this one.
pub(crate) fn binary(name: &str) -> PathBuf {
    let here = std::env::current_exe().unwrap_or_default();
    here.with_file_name(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

/// The scope a session's hosts advertise in: the run's, when a QA run
/// names one, else one of this folder's own, so two sessions never see
/// each other's hosts.
fn scope_for(dir: &Path) -> String {
    if let Ok(scope) = std::env::var("AMUX_QA_SCOPE")
        && !scope.is_empty()
    {
        return scope;
    }
    format!("testnet-{}", folder_hash(dir))
}

/// A short name of this folder, the same every time it is asked.
pub(crate) fn folder_hash(dir: &Path) -> String {
    let hash = dir
        .to_string_lossy()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    format!("{:08x}", hash as u32)
}

pub(crate) fn up(folder: &Folder, topology: Option<&Path>) -> Result<Outcome, String> {
    if let Ok(record) = folder.record()
        && door::call(
            record.control,
            &json!({"Status": {}}),
            Duration::from_secs(5),
        )
        .is_ok()
    {
        return Err("a net is already up in this folder; `ctl down` first".to_owned());
    }
    let net_dir = folder.net_dir();
    std::fs::create_dir_all(&net_dir).map_err(|error| error.to_string())?;
    if net_dir.join("net").exists() {
        // A net that ended without its shutdown left its root behind.
        std::fs::remove_dir_all(net_dir.join("net")).map_err(|error| error.to_string())?;
    }
    let mut declared = match topology {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|error| format!("reading {}: {error}", path.display()))?;
            let mut value: Value = serde_json::from_str(&text)
                .map_err(|error| format!("{} is not JSON: {error}", path.display()))?;
            // Script files are read relative to the topology file.
            let base = path.parent().unwrap_or(Path::new("."));
            for agent in value["agents"].as_array_mut().into_iter().flatten() {
                if let Some(file) = agent.get("script_file").and_then(Value::as_str) {
                    let absolute = base.join(file);
                    agent["script_file"] = json!(absolute);
                }
            }
            value
        }
        None => json!({"hosts": []}),
    };
    let scope = declared["scope"]
        .as_str()
        .filter(|scope| !scope.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| scope_for(&folder.dir));
    declared["scope"] = json!(scope);
    // A relay from the start, so any host can reach it once an account is
    // added, without restarting.
    if declared.get("relay").is_none_or(Value::is_null) {
        declared["relay"] = json!({"accounts": []});
    }
    let file = net_dir.join("topology.json");
    std::fs::write(
        &file,
        serde_json::to_string_pretty(&declared).unwrap_or_default(),
    )
    .map_err(|error| error.to_string())?;
    let out = net_dir.join("serve.out");
    let log = net_dir.join("testnet.log");
    let recordings = folder.dir.join("providers");
    std::fs::create_dir_all(&recordings).map_err(|error| error.to_string())?;
    let mut command = std::process::Command::new(binary("testnet"));
    // Run from inside a Claude Code session (a tester is one), the net
    // would hand that session's identity to every real Claude agent it
    // starts, which then behaves as a child of the session.
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy();
        if key.starts_with("CLAUDECODE") || key.starts_with("CLAUDE_CODE_") || key == "CLAUDE_PID" {
            command.env_remove(key.as_ref());
        }
    }
    command
        .arg("serve")
        .arg(&file)
        .arg("--root-in")
        .arg(&net_dir)
        .arg("--gated-lan")
        .arg("--facts-ring-mib")
        .arg(FACTS_RING_MIB.to_string())
        .arg("--recordings")
        .arg(&recordings)
        .env(
            "RUST_LOG",
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_owned()),
        )
        .env_remove("AMUX_LOG")
        .env_remove("AMUX_CONFIG")
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(&out).map_err(|error| error.to_string())?)
        .stderr(std::fs::File::create(&log).map_err(|error| error.to_string())?);
    #[cfg(unix)]
    {
        // Its own process group: an interrupt meant for whoever ran `ctl up`
        // must not take the net down with it.
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("starting testnet serve: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(300);
    let readiness = loop {
        let mut text = String::new();
        if let Ok(mut file) = std::fs::File::open(&out) {
            let _ = file.read_to_string(&mut text);
        }
        if let Some(line) = text.lines().next()
            && text.contains('\n')
        {
            break serde_json::from_str::<Value>(line).map_err(|error| {
                format!("testnet serve's readiness is not JSON ({error}): {line}")
            })?;
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            let said = std::fs::read_to_string(&log).unwrap_or_default();
            let tail: Vec<&str> = said.lines().rev().take(20).collect();
            return Err(format!(
                "testnet serve exited ({status}) before the net was ready:\n{}",
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            ));
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            return Err(format!(
                "the net was not ready within five minutes; see {}",
                log.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let record = Record {
        pid: child.id(),
        control: readiness["control"]
            .as_str()
            .and_then(|addr| addr.parse().ok())
            .ok_or("the readiness names no control address")?,
        root: PathBuf::from(readiness["root"].as_str().unwrap_or_default()),
        scope: scope.clone(),
        readiness: readiness.clone(),
    };
    std::fs::write(
        net_dir.join("net.json"),
        serde_json::to_string_pretty(&record).unwrap_or_default(),
    )
    .map_err(|error| error.to_string())?;
    let status = folder.status()?;
    Ok(Outcome::new(
        format!(
            "net up (discovery scope {scope}); its log is {}\n{}",
            log.display(),
            super::describe_status(&status)
        ),
        status,
    ))
}

pub(crate) fn down(folder: &Folder) -> Result<Outcome, String> {
    let record = folder.record()?;
    let mut said = Vec::new();
    super::tui::close_all(folder);
    super::phone::close(folder);
    stop_pairings(folder);
    match door::call(record.control, &json!("Shutdown"), Duration::from_secs(180)) {
        Ok(_) => said.push("net shut down".to_owned()),
        Err(error) => said.push(format!("the door did not shut the net down ({error})")),
    }
    // Wait for serve to exit; it removes the net's root on its way out.
    let net_dir = folder.net_dir();
    let deadline = Instant::now() + Duration::from_secs(60);
    while serving(record.pid, &net_dir) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    if serving(record.pid, &net_dir) {
        let _ = std::process::Command::new("kill")
            .arg("-KILL")
            .arg(record.pid.to_string())
            .status();
        said.push("testnet serve did not exit, so it was killed".to_owned());
    }
    // Agents a killed net leaves behind die with their folders' net.
    let _ = std::process::Command::new("pkill")
        .args(["-KILL", "-f"])
        .arg(&record.root)
        .status();
    let _ = std::fs::remove_file(folder.net_dir().join("net.json"));
    Ok(Outcome::new(said.join("; "), json!({"down": true})))
}

/// Whether `pid` is still this folder's `testnet serve`. A serve that
/// crashed leaves its pid in net.json, and the system may since have given
/// that pid to any other process, which must never be killed for it.
fn serving(pid: u32, net_dir: &Path) -> bool {
    std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .is_ok_and(|output| {
            let command = String::from_utf8_lossy(&output.stdout);
            command.contains(" serve ") && command.contains(&*net_dir.to_string_lossy())
        })
}

pub(crate) fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The real `amux` CLI against a host's installation config.
pub(crate) fn amux_command(config: &str) -> std::process::Command {
    let mut command = std::process::Command::new(binary("amux"));
    command
        .arg("--config")
        .arg(config)
        .env_remove("AMUX_LOG")
        .env_remove("AMUX_CONFIG")
        .env("AMUX_TEST_DISCOVERY_MODE", "disabled");
    command
}

pub(crate) fn amux(folder: &Folder, host: &str, args: &[String]) -> Result<Outcome, String> {
    let entry = folder.host(host)?;
    let config = entry["config"]
        .as_str()
        .ok_or("the host has no config")?
        .to_owned();
    let mut args = args.to_vec();
    if args.first().map(String::as_str) == Some("pair") {
        let target = args[1..].iter().position(|arg| !arg.starts_with('-'));
        match target {
            None => return pair_waiting(folder, host, &config, &args),
            Some(at) => {
                if let Some(gate) = gate_for(folder, host, &args[1 + at])? {
                    args[1 + at] = gate;
                }
            }
        }
    }
    let output = amux_command(&config)
        .args(&args)
        .stdin(std::process::Stdio::inherit())
        .output()
        .map_err(|error| format!("running amux: {error}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.status.success() {
        Ok(Outcome::new(
            text.clone(),
            json!({"output": text, "status": 0}),
        ))
    } else {
        Err(format!(
            "amux {} exited with {}:\n{}",
            args.join(" "),
            output.status.code().unwrap_or(-1),
            text.trim_end()
        ))
    }
}

/// The gate to dial in place of `target` when it names a LAN listener of
/// one of the net's hosts by address, so the pairing runs through it.
fn gate_for(folder: &Folder, from: &str, target: &str) -> Result<Option<String>, String> {
    let Some((_, port)) = target.rsplit_once(':') else {
        return Ok(None);
    };
    let Ok(port) = port.parse::<u64>() else {
        return Ok(None);
    };
    let status = folder.status()?;
    let to = status["hosts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|host| host["lan_port"].as_u64() == Some(port))
        .and_then(|host| host["name"].as_str())
        .map(str::to_owned);
    let Some(to) = to else {
        return Ok(None);
    };
    let gate = folder.call(json!({"PairGate": {"from": from, "to": to}}))?;
    Ok(gate["addr"].as_str().map(str::to_owned))
}

/// The waiting side of a pairing, `amux pair`, which holds its window open
/// for minutes: run in the background, answering once it has printed its
/// PIN and address.
fn pair_waiting(
    folder: &Folder,
    host: &str,
    config: &str,
    args: &[String],
) -> Result<Outcome, String> {
    let (pin, said) = start_pairing(folder, host, config, args)?;
    Ok(Outcome::new(
        format!(
            "{said}\n(the pairing window stays open in the background, up to five minutes; \
             join it with: echo {pin} | testnet ctl amux OTHER-HOST -- pair {host})"
        ),
        json!({"pin": pin, "output": said}),
    ))
}

/// Starts `amux pair` on a host in the background and answers its PIN and
/// what it printed.
pub(crate) fn start_pairing(
    folder: &Folder,
    host: &str,
    config: &str,
    args: &[String],
) -> Result<(String, String), String> {
    let dir = folder.net_dir().join("pairing");
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let out = dir.join(format!("{host}.out"));
    let mut command = amux_command(config);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(&out).map_err(|error| error.to_string())?)
        .stderr(std::process::Stdio::from(
            std::fs::File::options()
                .append(true)
                .open(&out)
                .map_err(|error| error.to_string())?,
        ));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("starting amux pair: {error}"))?;
    std::fs::write(dir.join(format!("{host}.pid")), child.id().to_string())
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let text = std::fs::read_to_string(&out).unwrap_or_default();
        if let Some(pin) = pin_in(&text)
            && (text.contains("listens at") || text.lines().count() >= 2)
        {
            return Ok((pin, text.trim().to_owned()));
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Err(format!("amux pair exited ({status}):\n{}", text.trim()));
        }
        if Instant::now() > deadline {
            return Err(format!(
                "amux pair printed no PIN within a minute:\n{}",
                text.trim()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The digits of the PIN `amux pair` printed ("Pairing PIN: 774 749").
fn pin_in(text: &str) -> Option<String> {
    let line = text.lines().find(|line| line.contains("PIN"))?;
    let digits: String = line
        .split_once(':')?
        .1
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    (digits.len() >= 4).then_some(digits)
}

/// Ends every pairing window this session left open.
fn stop_pairings(folder: &Folder) {
    let Ok(entries) = std::fs::read_dir(folder.net_dir().join("pairing")) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.path().extension().is_some_and(|ext| ext == "pid")
            && let Ok(pid) = std::fs::read_to_string(entry.path())
        {
            let _ = std::process::Command::new("kill")
                .arg(pid.trim())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}
