//! The live provider compatibility lane: a real daemon started from the
//! built amux binary, hosting the real provider under the operator's own
//! login, driven through the CLI verbs, and judged by what the daemon
//! committed to the profile store as a client subscribed to it reads it.
//!
//! Each target runs one provider entry point through its scenarios:
//! initialization and capabilities, one response, one native decision,
//! interrupt and resume; a plan decided, questions skipped and replied to
//! instead (asks.rs); usage windows read, and what the agent and the host
//! offer (offers.rs). Codex adds a permission change, a mode change and
//! attach: its own app co-driving the agent beside amux's client, captured
//! terminal by terminal (attach.rs). A scenario asserts protocol structure
//! and real effects, never generated prose: the item classes, asks,
//! decisions, phase changes and turn ends the interpreter records have the
//! shape it records for the matching probe recording, replayed from the
//! interpreter fixture that points at the recording in claude-specs or
//! codex-specs. A scenario with no such recording is judged against its own
//! provider traffic replayed through the interpreter.
//!
//! The provider runs behind the probes' capture proxies, so every
//! scenario's traffic is recorded where it can join the corpus
//! (capture.rs).
//!
//! Every result is pass, fail, unavailable (no login, no binary, or a
//! provider older than the corpus) or not_run. With no scenario selected a
//! target reports not_run for each one and starts nothing.

mod asks;
mod attach;
mod capture;
mod offers;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use interpret::Replayed;
use tokio::process::{Child, Command};
use tonic::transport::{Channel, Endpoint};
use ui_state::{AgentState, ItemBody, OpenAsk};
use wire::installation_service_client::InstallationServiceClient;
use wire::{
    ClaudeAnswer, ClaudePtyInput, ClaudeSdkInput, CodexCreateConfig, CodexInput,
    CreateAgentRequest, Input, Item, Kind, Phase, SendInputRequest, Snapshot,
    SubscribeSessionRequest, claude_answer, claude_pty_input, claude_sdk_input, codex_input,
    permission_answer, send_input_response, session_event,
};

/// Claude runs on its cheapest model; the lane judges structure, not wit.
const CLAUDE_MODEL: &str = "haiku";
/// Claude must not update itself under a run: the version a run reports
/// is the version it judged.
const UPDATE_GUARDS: &[(&str, &str)] = &[
    ("DISABLE_AUTOUPDATER", "1"),
    ("DISABLE_UPDATES", "1"),
    ("DISABLE_INSTALLATION_CHECKS", "1"),
];
/// How long a new agent may take to accept input.
const READY: Duration = Duration::from_secs(120);
/// How long a start may take to finish reporting after it takes input.
const SETTLE: Duration = Duration::from_secs(15);
/// How long one turn may take.
const TURN: Duration = Duration::from_secs(240);
/// How long a whole scenario may take before it is a hang.
const SCENARIO: Duration = Duration::from_secs(600);
/// How long one CLI verb may take.
const VERB: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    Initialize,
    Respond,
    Decide,
    Interrupt,
    Resume,
    Plan,
    Questions,
    Usage,
    Catalogue,
    HostCatalogue,
    Permission,
    Mode,
    Attach,
}

/// Every scenario, in the order a run takes them.
pub const SCENARIOS: [Scenario; 13] = [
    Scenario::Initialize,
    Scenario::Respond,
    Scenario::Decide,
    Scenario::Interrupt,
    Scenario::Resume,
    Scenario::Plan,
    Scenario::Questions,
    Scenario::Usage,
    Scenario::Catalogue,
    Scenario::HostCatalogue,
    Scenario::Permission,
    Scenario::Mode,
    Scenario::Attach,
];

impl Scenario {
    pub fn name(self) -> &'static str {
        match self {
            Scenario::Initialize => "initialize",
            Scenario::Respond => "respond",
            Scenario::Decide => "decide",
            Scenario::Interrupt => "interrupt",
            Scenario::Resume => "resume",
            Scenario::Plan => "plan",
            Scenario::Questions => "questions",
            Scenario::Usage => "usage",
            Scenario::Catalogue => "catalogue",
            Scenario::HostCatalogue => "host_catalogue",
            Scenario::Permission => "permission",
            Scenario::Mode => "mode",
            Scenario::Attach => "attach",
        }
    }

    /// Whether `kind` has what the scenario exercises: terminal Claude
    /// reports no usage; only Codex has amux-set permissions and modes and
    /// an app of its own to attach.
    fn applies(self, kind: Kind) -> bool {
        match self {
            Scenario::Usage => kind != Kind::ClaudePty,
            Scenario::Permission | Scenario::Mode | Scenario::Attach => kind == Kind::Codex,
            _ => true,
        }
    }
}

/// One provider entry point.
pub struct Driver {
    pub kind: Kind,
    /// The provider command, found on the PATH as the daemon finds it.
    pub command: &'static str,
    /// The interpreter fixture replaying each scenario's probe recording,
    /// by name under crates/interpret/fixtures/<kind>; None for a scenario
    /// judged against its own traffic.
    pub recording: fn(Scenario) -> Option<&'static str>,
    pub replay: fn(&Path) -> Result<Vec<Replayed>, String>,
}

impl Driver {
    fn scenarios(&self) -> Vec<Scenario> {
        SCENARIOS
            .into_iter()
            .filter(|scenario| scenario.applies(self.kind))
            .collect()
    }

    fn tag(&self) -> &'static str {
        match self.kind {
            Kind::ClaudePty => "claude_pty",
            Kind::ClaudeSdk => "claude_sdk",
            Kind::Codex => "codex",
            Kind::Unspecified => "unknown",
        }
    }

    fn fixture(&self, scenario: Scenario) -> Option<PathBuf> {
        (self.recording)(scenario).map(|recording| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../interpret/fixtures")
                .join(self.tag())
                .join(format!("{recording}.json"))
        })
    }
}

pub enum Verdict {
    Pass,
    Fail(String),
    Unavailable(String),
    NotRun(String),
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Verdict::Pass => write!(f, "pass"),
            Verdict::Fail(why) => write!(f, "fail: {why}"),
            Verdict::Unavailable(why) => write!(f, "unavailable: {why}"),
            Verdict::NotRun(why) => write!(f, "not_run: {why}"),
        }
    }
}

struct Report {
    kind: &'static str,
    provider: String,
    corpus: String,
    model: String,
    results: Vec<(Scenario, Verdict)>,
}

impl Report {
    fn print(&self) {
        println!("live {}", self.kind);
        println!("provider  {}", self.provider);
        println!("corpus    {}", self.corpus);
        println!("model     {}", self.model);
        for (scenario, verdict) in &self.results {
            println!("{:<15} {verdict}", scenario.name());
        }
    }

    fn failed(&self) -> bool {
        self.results
            .iter()
            .any(|(_, verdict)| matches!(verdict, Verdict::Fail(_)))
    }
}

/// A target's entry: `<target> all | <scenario>...`.
pub fn main(driver: Driver) -> ExitCode {
    // Flags are cargo's or libtest's, handed to every target; a name that
    // is no scenario is a filter that matched nothing here.
    let names: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    let selected: Vec<Scenario> = if names.iter().any(|name| name == "all") {
        driver.scenarios()
    } else {
        driver
            .scenarios()
            .iter()
            .copied()
            .filter(|scenario| names.iter().any(|name| name == scenario.name()))
            .collect()
    };
    for name in &names {
        if name != "all"
            && !driver
                .scenarios()
                .iter()
                .any(|scenario| scenario.name() == name)
        {
            eprintln!(
                "no {} scenario named {name}; known: all, {}",
                driver.tag(),
                driver
                    .scenarios()
                    .iter()
                    .map(|scenario| scenario.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    let mut report = Report {
        kind: driver.tag(),
        provider: "not probed".into(),
        corpus: corpus(&driver, &driver.scenarios()).to_string(),
        model: "none".into(),
        results: Vec::new(),
    };
    if selected.is_empty() {
        report.results = driver
            .scenarios()
            .iter()
            .map(|scenario| (*scenario, Verdict::NotRun("no scenario selected".into())))
            .collect();
        report.print();
        return ExitCode::SUCCESS;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(run(&driver, &selected, &mut report));
    for scenario in driver.scenarios() {
        if !selected.contains(&scenario) {
            report
                .results
                .push((scenario, Verdict::NotRun("not selected".into())));
        }
    }
    report
        .results
        .sort_by_key(|(scenario, _)| driver.scenarios().iter().position(|s| s == scenario));
    // The attach capture carries its verdict beside it.
    if let Some((_, verdict)) = report.results.iter().find(|(scenario, verdict)| {
        *scenario == Scenario::Attach && !matches!(verdict, Verdict::NotRun(_))
    }) {
        attach::write_verdict(verdict);
    }
    report.print();
    if report.failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

async fn run(driver: &Driver, selected: &[Scenario], report: &mut Report) {
    let unavailable = |report: &mut Report, why: String| {
        report.results = selected
            .iter()
            .map(|scenario| (*scenario, Verdict::Unavailable(why.clone())))
            .collect();
    };
    let version = match provider_version(driver.command).await {
        Ok(version) => version,
        Err(why) => return unavailable(report, why),
    };
    report.provider = format!("{} {version}", driver.command);
    let corpus = corpus(driver, selected);
    report.corpus = corpus.to_string();
    if Version::parse(&version) < corpus.version {
        return unavailable(
            report,
            format!(
                "{} {version} is older than the corpus ({})",
                driver.command, corpus.version
            ),
        );
    }
    let install = match Install::start(driver).await {
        Ok(install) => install,
        Err(why) if why.starts_with("no Codex login") => return unavailable(report, why),
        Err(why) => {
            report.results = selected
                .iter()
                .map(|scenario| (*scenario, Verdict::Fail(format!("the daemon: {why}"))))
                .collect();
            return;
        }
    };
    for scenario in selected {
        let verdict = match tokio::time::timeout(SCENARIO, install.scenario(*scenario)).await {
            Ok(Ok(verdict)) => verdict,
            Ok(Err(why)) => Verdict::Fail(why),
            Err(_) => Verdict::Fail(format!("did not finish within {SCENARIO:?}")),
        };
        eprintln!("{} {}: {verdict}", driver.tag(), scenario.name());
        // Whatever the verdict, an agent's traffic is kept for a look.
        if install.has_agent(*scenario)
            && let Err(why) = install.collect(*scenario)
        {
            eprintln!("{} {}: no recording: {why}", driver.tag(), scenario.name());
        }
        report.results.push((*scenario, verdict));
    }
    if let Some(model) = install.model.lock().unwrap().clone() {
        report.model = model;
    }
    install.shutdown().await;
}

/// The provider's version, from `<command> --version`.
async fn provider_version(command: &str) -> Result<String, String> {
    let output = Command::new(command)
        .arg("--version")
        .envs(UPDATE_GUARDS.iter().copied())
        .stdin(Stdio::null())
        .output();
    let output = tokio::time::timeout(VERB, output)
        .await
        .map_err(|_| format!("`{command} --version` did not answer"))?
        .map_err(|error| format!("no {command} on the PATH: {error}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .find(|word| word.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(str::to_owned)
        .ok_or_else(|| format!("`{command} --version` said {text:?}"))
}

/// A dotted numeric version, compared number by number.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Version(Vec<u64>);

impl Version {
    fn parse(text: &str) -> Version {
        Version(
            text.split(|c: char| !c.is_ascii_digit())
                .take_while(|part| !part.is_empty())
                .filter_map(|part| part.parse().ok())
                .collect(),
        )
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self.0.iter().map(u64::to_string).collect();
        write!(f, "{}", parts.join("."))
    }
}

/// The newest provider version the scenarios' recordings were recorded
/// against: a provider older than that is not what they describe.
struct Corpus {
    version: Version,
    recordings: Vec<String>,
}

impl fmt::Display for Corpus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.version, self.recordings.join(", "))
    }
}

fn corpus(driver: &Driver, scenarios: &[Scenario]) -> Corpus {
    let mut version = Version::default();
    let mut recordings = BTreeSet::new();
    for scenario in scenarios {
        let Some(recording) = driver
            .fixture(*scenario)
            .and_then(|fixture| recording_dir(&fixture))
        else {
            continue;
        };
        recordings.insert(
            recording
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        let Ok(manifest) = std::fs::read_to_string(recording.join("manifest.json")) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&manifest) else {
            continue;
        };
        if let Some(seen) = manifest["recorded"]["version"].as_str() {
            version = version.max(Version::parse(seen));
        }
    }
    Corpus {
        version,
        recordings: recordings.into_iter().collect(),
    }
}

/// The recording directory an interpreter fixture replays.
fn recording_dir(fixture: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(fixture).ok()?;
    let fixture_json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let path = fixture_json["recording"]["path"].as_str()?;
    Some(fixture.parent()?.join(path).parent()?.to_owned())
}

/// An installation in a temporary directory: its own state, sockets and
/// project, the operator's own provider login.
struct Install {
    kind: Kind,
    root: tempfile::TempDir,
    amux: PathBuf,
    env: Vec<(String, OsString)>,
    daemon: tokio::sync::Mutex<Child>,
    client: wire::ClientStubs,
    driver_fixture: Box<dyn Fn(Scenario) -> Option<PathBuf> + Send + Sync>,
    replay: fn(&Path) -> Result<Vec<Replayed>, String>,
    model: Mutex<Option<String>>,
    capture: capture::Capture,
    /// Each scenario's agent, as it was created.
    agents: Mutex<Vec<(Scenario, Vec<u8>)>>,
}

impl Install {
    async fn start(driver: &Driver) -> Result<Install, String> {
        let amux = testnet::Binaries::built().amux();
        // Socket paths have a short limit; the system temp dir can be long.
        // AMUX_LIVE_KEEP leaves the installation behind for a post-mortem.
        let keep = std::env::var_os("AMUX_LIVE_KEEP").is_some();
        let root = tempfile::Builder::new()
            .prefix("al-")
            .disable_cleanup(keep)
            .tempdir_in("/tmp")
            .map_err(|error| error.to_string())?;
        let path = root.path().to_owned();
        if keep {
            eprintln!("keeping the installation at {}", path.display());
        }
        let config = path.join("config.toml");
        let socket = path.join("amux.sock");
        std::fs::create_dir_all(path.join("project")).map_err(|error| error.to_string())?;
        std::fs::write(
            &config,
            format!(
                "root = {:?}\nfront_door_socket = {:?}\nhost_name = \"live\"\nsupervisor = false\n\
                 [agent]\ngrace_secs = 10\ndrain_secs = 5\n[discovery]\nscope = \"amux-live-{}\"\n",
                path.join("data").display().to_string(),
                socket.display().to_string(),
                uuid::Uuid::new_v4().simple(),
            ),
        )
        .map_err(|error| error.to_string())?;
        // A scripted local network: real mDNS would have macOS ask for Local
        // Network access, and no scenario here is about discovery.
        let mut env: Vec<(String, OsString)> = vec![
            ("AMUX_CONFIG".into(), config.into()),
            ("AMUX_TEST_DISCOVERY_MODE".into(), "disabled".into()),
        ];
        env.extend(
            UPDATE_GUARDS
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).into())),
        );
        if driver.kind == Kind::Codex {
            env.push(("CODEX_HOME".into(), seed_codex_home(&path)?.into()));
        }
        let (capture, proxied) =
            capture::Capture::install(&path, driver.kind, driver.tag(), driver.command)?;
        env.extend(proxied);

        let log = std::fs::File::create(path.join("daemon.log")).map_err(|e| e.to_string())?;
        let mut daemon = Command::new(&amux);
        daemon
            .arg("daemon")
            .envs(env.iter().map(|(key, value)| (key, value)))
            .env_remove("AMUX_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .kill_on_drop(true);
        let daemon = daemon.spawn().map_err(|error| error.to_string())?;
        let door = wait_for(Duration::from_secs(30), || async {
            channel(&socket).await.ok()
        })
        .await
        .ok_or("its front door never answered")?;
        let profile = InstallationServiceClient::new(door)
            .list_profiles(wire::ListProfilesRequest {})
            .await
            .map_err(|status| status.to_string())?
            .into_inner()
            .profiles
            .into_iter()
            .next()
            .ok_or("it has no profile")?;
        let client = wire::client_stubs(
            channel(Path::new(&profile.socket_path))
                .await
                .map_err(|error| error.to_string())?,
        );
        let fixtures: Vec<(Scenario, Option<PathBuf>)> = driver
            .scenarios()
            .into_iter()
            .map(|scenario| (scenario, driver.fixture(scenario)))
            .collect();
        Ok(Install {
            kind: driver.kind,
            root,
            amux,
            env,
            daemon: tokio::sync::Mutex::new(daemon),
            client,
            driver_fixture: Box::new(move |scenario| {
                fixtures
                    .iter()
                    .find(|(s, _)| *s == scenario)
                    .and_then(|(_, path)| path.clone())
            }),
            replay: driver.replay,
            model: Mutex::new(None),
            capture,
            agents: Mutex::new(Vec::new()),
        })
    }

    /// Kills every agent and stops the daemon, so nothing outlives the run.
    async fn shutdown(&self) {
        if let Ok(listing) = self.amux(&["ls"]).await {
            for name in SCENARIOS.map(Scenario::name) {
                if listing
                    .lines()
                    .any(|line| line.split_whitespace().next() == Some(name))
                {
                    let _ = self.amux(&["stop", name]).await;
                }
            }
        }
        let _ = self.amux(&["server", "stop"]).await;
        let mut daemon = self.daemon.lock().await;
        if tokio::time::timeout(Duration::from_secs(10), daemon.wait())
            .await
            .is_err()
        {
            let _ = daemon.kill().await;
        }
    }

    /// Runs a CLI verb to completion and returns what it printed.
    async fn amux(&self, args: &[&str]) -> Result<String, String> {
        let output = Command::new(&self.amux)
            .args(args)
            .envs(self.env.iter().map(|(key, value)| (key, value)))
            .env_remove("AMUX_LOG")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(VERB, output)
            .await
            .map_err(|_| format!("amux {} did not finish", args.join(" ")))?
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "amux {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn project(&self, scenario: Scenario) -> Result<PathBuf, String> {
        let dir = self.root.path().join("project").join(scenario.name());
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        Ok(dir)
    }

    /// `amux new`, named after the scenario, in its own project.
    async fn create(&self, scenario: Scenario) -> Result<Vec<u8>, String> {
        self.create_with(scenario, &[]).await
    }

    /// `amux new` with more of the provider's own arguments. Claude runs in
    /// a terminal for its own interface and headless for amux's.
    async fn create_with(&self, scenario: Scenario, extra: &[&str]) -> Result<Vec<u8>, String> {
        let cwd = self.project(scenario)?.to_string_lossy().into_owned();
        let agent: &[&str] = match self.kind {
            Kind::ClaudePty => &["claude", "--in", "native"],
            Kind::Codex => &["codex"],
            _ => &["claude", "--in", "amux"],
        };
        let mut args = vec!["new"];
        args.extend(agent);
        args.extend(["--name", scenario.name(), "--cwd", cwd.as_str()]);
        if self.kind != Kind::Codex {
            // Only amux's own tool server and no settings but the
            // project's (it has none): the operator's MCP servers and
            // permission rules stay out of the run.
            args.extend([
                "--model",
                CLAUDE_MODEL,
                "--",
                "--setting-sources",
                "project",
                "--strict-mcp-config",
            ]);
        }
        // Terminal Claude's interrupt recording cancels a running Bash
        // command it was allowed to run without asking.
        if self.kind == Kind::ClaudePty && scenario == Scenario::Interrupt {
            args.extend(["--allowedTools", "Bash"]);
        }
        // Terminal Claude takes the operator's default permission mode even
        // with only project settings, and an `auto` default runs the decide
        // recording's command without asking.
        if self.kind == Kind::ClaudePty && scenario == Scenario::Decide {
            args.extend(["--permission-mode", "default"]);
        }
        args.extend(extra);
        let output = self.amux(&args).await?;
        let agent = created_id(&output).ok_or_else(|| format!("amux create said {output:?}"))?;
        self.created(scenario, &agent);
        Ok(agent)
    }

    fn created(&self, scenario: Scenario, agent: &[u8]) {
        self.agents.lock().unwrap().push((scenario, agent.to_vec()));
    }

    fn has_agent(&self, scenario: Scenario) -> bool {
        self.agents
            .lock()
            .unwrap()
            .iter()
            .any(|(s, _)| *s == scenario)
    }

    /// Writes the scenario's recorded traffic beside the run and returns
    /// the fixture that replays it.
    fn collect(&self, scenario: Scenario) -> Result<PathBuf, String> {
        let agent = self
            .agents
            .lock()
            .unwrap()
            .iter()
            .find(|(s, _)| *s == scenario)
            .map(|(_, agent)| agent.clone());
        let prelude = match (self.kind, agent) {
            (Kind::ClaudePty, Some(agent)) => capture::launch_facts(self.root.path(), &agent),
            _ => Vec::new(),
        };
        self.capture
            .collect(scenario, &self.project(scenario)?, prelude)
    }

    /// What the real provider did in this scenario, as the interpreter
    /// replays the traffic the probe recorded.
    fn captured(&self, scenario: Scenario) -> Result<Log, String> {
        self.replay_log(&self.collect(scenario)?, false)
    }

    /// Codex asks before a command only under an asking approval policy,
    /// here on-request in a read-only sandbox as its decision recording ran.
    /// The CLI has no verb for either, so this create goes through the
    /// client service, as the apps issue it.
    async fn create_asking_codex(&self, scenario: Scenario) -> Result<Vec<u8>, String> {
        self.create_codex(
            scenario,
            CodexCreateConfig {
                permission: Some("read-only".into()),
                ..CodexCreateConfig::default()
            },
        )
        .await
    }

    /// A Codex agent made as the apps make one, with settings the CLI has
    /// no flag for.
    async fn create_codex(
        &self,
        scenario: Scenario,
        config: CodexCreateConfig,
    ) -> Result<Vec<u8>, String> {
        let agent = self
            .client
            .agents
            .clone()
            .create_agent(CreateAgentRequest {
                agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                name: Some(scenario.name().to_owned()),
                cwd: self.project(scenario)?.to_string_lossy().into_owned(),
                agent: Some(wire::create_agent_request::Agent::Codex(config)),
                ..CreateAgentRequest::default()
            })
            .await
            .map_err(|status| status.to_string())?
            .into_inner();
        self.created(scenario, &agent.agent_id);
        Ok(agent.agent_id)
    }

    /// Follows the agent's session as a client does, for as long as the
    /// returned log lives.
    fn follow(&self, agent: &[u8]) -> Follow {
        let log = Arc::new(Mutex::new(Log::default()));
        let task = tokio::spawn({
            let log = log.clone();
            let mut client = self.client.sessions.clone();
            let agent = agent.to_vec();
            async move {
                loop {
                    let stream = client
                        .subscribe_session(SubscribeSessionRequest {
                            agent_id: agent.clone(),
                            tail: 1000,
                        })
                        .await;
                    if let Ok(stream) = stream {
                        let mut stream = stream.into_inner();
                        while let Ok(Some(event)) = stream.message().await {
                            let mut log = log.lock().unwrap();
                            match event.of {
                                Some(session_event::Of::Item(item)) => log.item(item),
                                Some(session_event::Of::Snapshot(snapshot)) => {
                                    log.snapshots.push(snapshot)
                                }
                                _ => {}
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        });
        Follow {
            kind: self.kind,
            log,
            task,
        }
    }

    /// What the scenario's recording replays to.
    fn recorded(&self, scenario: Scenario) -> Result<Log, String> {
        self.replayed(scenario, false)
    }

    /// What the scenario's recording replays to, up to and including the
    /// first frame that takes input when `until_ready`.
    fn replayed(&self, scenario: Scenario, until_ready: bool) -> Result<Log, String> {
        let fixture = (self.driver_fixture)(scenario)
            .ok_or_else(|| format!("{} has no recording in the corpus", scenario.name()))?;
        self.replay_log(&fixture, until_ready)
    }

    fn replay_log(&self, fixture: &Path, until_ready: bool) -> Result<Log, String> {
        let frames = (self.replay)(fixture)
            .map_err(|error| format!("replaying {}: {error}", fixture.display()))?;
        let mut log = Log::default();
        for frame in frames {
            if let Some((_, catalogue)) = frame.catalogue {
                log.catalogue = Some(catalogue);
            }
            for item in frame.step.items {
                log.item(item);
            }
            if let Some(snapshot) = frame.step.snapshot {
                let ready = snapshot.phase() == Phase::Idle;
                log.snapshots.push(snapshot);
                if until_ready && ready {
                    break;
                }
            }
        }
        Ok(log)
    }

    async fn send(&self, scenario: Scenario, text: &str) -> Result<(), String> {
        self.amux(&["send", scenario.name(), text]).await.map(drop)
    }

    /// Sends an input the CLI has no verb for, as the apps send it.
    async fn input(&self, agent: &[u8], of: wire::KindInput) -> Result<(), String> {
        let response = self
            .client
            .sessions
            .clone()
            .send_input(SendInputRequest {
                agent_id: agent.to_vec(),
                input: Some(Input::new(uuid::Uuid::new_v4().as_bytes().to_vec(), of)),
            })
            .await
            .map_err(|status| status.to_string())?
            .into_inner();
        match response.of {
            Some(send_input_response::Of::Accepted(_)) => Ok(()),
            Some(send_input_response::Of::Rejected(rejected)) => Err(format!(
                "the input was refused: {:?} {}",
                rejected.refusal(),
                rejected.detail.unwrap_or_default()
            )),
            None => Err("the daemon gave no verdict".into()),
        }
    }

    fn interrupt(&self) -> wire::KindInput {
        match self.kind {
            Kind::Codex => wire::KindInput::Codex(CodexInput {
                of: Some(codex_input::Of::Interrupt(wire::Interrupt {})),
            }),
            Kind::ClaudeSdk => wire::KindInput::ClaudeSdk(ClaudeSdkInput {
                of: Some(claude_sdk_input::Of::Interrupt(wire::Interrupt {})),
            }),
            _ => wire::KindInput::ClaudePty(ClaudePtyInput {
                of: Some(claude_pty_input::Of::Interrupt(wire::Interrupt {})),
            }),
        }
    }

    /// Allows an open ask once, the way the apps' primary choice does.
    fn allow(&self, ask: &OpenAsk) -> Result<wire::KindInput, String> {
        match ask {
            OpenAsk::Claude(ask) => {
                if !matches!(ask.body, Some(wire::ask::Body::Permission(_))) {
                    return Err(format!(
                        "an unexpected ask opened: {}",
                        ask_token(&OpenAsk::Claude(ask.clone()))
                    ));
                }
                let answer = ClaudeAnswer {
                    ask_key: ask.key.clone(),
                    of: Some(claude_answer::Of::Permission(wire::PermissionAnswer {
                        of: Some(permission_answer::Of::Allow(wire::PermissionAllow {
                            scope: None,
                        })),
                    })),
                };
                Ok(match self.kind {
                    Kind::ClaudeSdk => wire::KindInput::ClaudeSdk(ClaudeSdkInput {
                        of: Some(claude_sdk_input::Of::Answer(answer)),
                    }),
                    _ => wire::KindInput::ClaudePty(ClaudePtyInput {
                        of: Some(claude_pty_input::Of::Answer(answer)),
                    }),
                })
            }
            OpenAsk::Codex(ask) => {
                if !matches!(
                    ask.body,
                    Some(wire::codex_ask::Body::Command(_) | wire::codex_ask::Body::FileChange(_))
                ) {
                    return Err(format!(
                        "an unexpected ask opened: {}",
                        ask_token(&OpenAsk::Codex(ask.clone()))
                    ));
                }
                Ok(wire::KindInput::Codex(CodexInput {
                    of: Some(codex_input::Of::Answer(wire::CodexAnswer {
                        ask_key: ask.key.clone(),
                        of: Some(wire::codex_answer::Of::Approve(
                            wire::Decision::Approve as i32,
                        )),
                    })),
                }))
            }
        }
    }

    async fn scenario(&self, scenario: Scenario) -> Result<Verdict, String> {
        match scenario {
            Scenario::Initialize => self.initialize().await,
            Scenario::Respond => self.respond().await,
            Scenario::Decide => self.decide().await,
            Scenario::Interrupt => self.interrupt_turn().await,
            Scenario::Resume => self.resume().await,
            Scenario::Plan => self.plan().await,
            Scenario::Questions => self.questions().await,
            Scenario::Usage => self.usage().await,
            Scenario::Catalogue => self.catalogue().await,
            Scenario::HostCatalogue => self.host_catalogue().await,
            Scenario::Permission => self.permission().await,
            Scenario::Mode => self.mode().await,
            Scenario::Attach => self.attach().await,
        }
    }

    /// A new agent takes input, signed in, knowing at least what the
    /// recording knew by then.
    async fn initialize(&self) -> Result<Verdict, String> {
        let agent = self.create(Scenario::Initialize).await?;
        let follow = self.follow(&agent);
        follow.ready().await?;
        if let Some(why) = follow.signed_out() {
            return Ok(Verdict::Unavailable(why));
        }
        let recorded = self.replayed(Scenario::Initialize, true)?;
        // What the provider reports at start may land just after it takes
        // input: terminal Claude's start hook races its prompt drawing.
        let kind = self.kind;
        let items = recorded.shape(kind).items;
        let _ = follow
            .until(SETTLE, "the start the recording made", |log| {
                log.shape(kind).items == items
            })
            .await;
        let live = follow.snapshot();
        judge(&recorded.shape(self.kind), &live.shape(self.kind))?;
        let recorded_known = known(&recorded.state(self.kind));
        let live_known = known(&live.state(self.kind));
        let missing: Vec<_> = recorded_known.difference(&live_known).collect();
        if !missing.is_empty() {
            return Err(format!(
                "ready without what the recording knew by then: {missing:?} (live knew {live_known:?})"
            ));
        }
        Ok(Verdict::Pass)
    }

    /// One prompt, one completed turn with an answer in it.
    async fn respond(&self) -> Result<Verdict, String> {
        let agent = self.create(Scenario::Respond).await?;
        let follow = self.follow(&agent);
        follow.ready().await?;
        if let Some(why) = follow.signed_out() {
            return Ok(Verdict::Unavailable(why));
        }
        self.send(
            Scenario::Respond,
            "Reply with the single word pong and nothing else.",
        )
        .await?;
        let outcome = follow.turn_end(1).await?;
        self.note_model(&follow);
        let live = follow.snapshot();
        expect_outcome(outcome, wire::TurnOutcome::Completed)?;
        if !live.answered(self.kind) {
            return Err("the turn ended without a complete answer item".into());
        }
        judge(
            &self.recorded(Scenario::Respond)?.shape(self.kind),
            &live.shape(self.kind),
        )?;
        Ok(Verdict::Pass)
    }

    /// The provider asks before running a command; allowing it runs the
    /// command and the file exists.
    async fn decide(&self) -> Result<Verdict, String> {
        let agent = match self.kind {
            Kind::Codex => self.create_asking_codex(Scenario::Decide).await?,
            _ => self.create(Scenario::Decide).await?,
        };
        let follow = self.follow(&agent);
        follow.ready().await?;
        if let Some(why) = follow.signed_out() {
            return Ok(Verdict::Unavailable(why));
        }
        let effect = self.project(Scenario::Decide)?.join("decided.txt");
        // Codex's sandbox is read-only, so the command needs its approval;
        // the model is told so, or it may answer without trying.
        let prompt = match self.kind {
            Kind::Codex => {
                "Run exactly this shell command once, with your shell tool, and nothing \
                 else: touch decided.txt. The sandbox is read-only, so request escalated \
                 permissions for it; I will approve. Then reply with the single word done."
            }
            _ => {
                "Run exactly this shell command once, with your shell tool, and nothing \
                 else: touch decided.txt. Then reply with the single word done."
            }
        };
        self.send(Scenario::Decide, prompt).await?;
        let mut answered = BTreeSet::new();
        let deadline = tokio::time::Instant::now() + TURN;
        loop {
            let live = follow.snapshot();
            if !live.turns(self.kind).is_empty() {
                break;
            }
            for ask in live.state(self.kind).asks {
                if answered.insert(ask.key().to_owned()) {
                    self.input(&agent, self.allow(&ask)?).await?;
                }
            }
            if tokio::time::Instant::now() > deadline {
                return Err(format!("the turn did not end within {TURN:?}"));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.note_model(&follow);
        let live = follow.snapshot();
        if answered.is_empty() {
            return Err(format!(
                "the turn ended without the provider asking: {:?}",
                live.shape(self.kind).items
            ));
        }
        expect_outcome(live.turns(self.kind)[0], wire::TurnOutcome::Completed)?;
        if !effect.exists() {
            return Err(format!("allowed, but {} does not exist", effect.display()));
        }
        judge(
            &self.recorded(Scenario::Decide)?.shape(self.kind),
            &live.shape(self.kind),
        )?;
        Ok(Verdict::Pass)
    }

    /// Interrupting a running turn ends it as interrupted and the agent
    /// takes input again.
    async fn interrupt_turn(&self) -> Result<Verdict, String> {
        let agent = self.create(Scenario::Interrupt).await?;
        let follow = self.follow(&agent);
        follow.ready().await?;
        if let Some(why) = follow.signed_out() {
            return Ok(Verdict::Unavailable(why));
        }
        // Each kind's prompt is its recording's: the terminal recording
        // interrupts a long command, the others a long reply.
        let prompt = match self.kind {
            Kind::ClaudePty => {
                "Use Bash to run exactly: python3 -c 'import select; select.select([], [], [], \
                 30)'; printf SHOULD_NOT_FINISH. Do not do anything else."
            }
            _ => "Count from 1 to 1000, one number per line, in plain text.",
        };
        self.send(Scenario::Interrupt, prompt).await?;
        // The turn is under way once the provider has said anything of its
        // own: an interrupt before that races the prompt, not the turn.
        follow
            .until(TURN, "the turn to get under way", |log| {
                log.phase() == Phase::Working && log.under_way(self.kind)
            })
            .await?;
        self.input(&agent, self.interrupt()).await?;
        let outcome = follow.turn_end(1).await?;
        self.note_model(&follow);
        expect_outcome(outcome, wire::TurnOutcome::Interrupted)?;
        follow
            .until(READY, "the agent to take input again", |log| {
                log.phase() == Phase::Idle
            })
            .await?;
        judge(
            &self.recorded(Scenario::Interrupt)?.shape(self.kind),
            &follow.snapshot().shape(self.kind),
        )?;
        Ok(Verdict::Pass)
    }

    /// Stopped and resumed, the agent continues the same provider session:
    /// the new incarnation starts with a resume boundary, the provider
    /// reports the session it had, and its turn has the one-response
    /// recording's shape.
    async fn resume(&self) -> Result<Verdict, String> {
        let name = Scenario::Resume.name();
        let agent = self.create(Scenario::Resume).await?;
        let follow = self.follow(&agent);
        follow.ready().await?;
        if let Some(why) = follow.signed_out() {
            return Ok(Verdict::Unavailable(why));
        }
        self.send(
            Scenario::Resume,
            "Reply with the single word one and nothing else.",
        )
        .await?;
        expect_outcome(follow.turn_end(1).await?, wire::TurnOutcome::Completed)?;
        let first = follow.snapshot();
        let session = first
            .state(self.kind)
            .provider_session
            .ok_or("the first incarnation reported no provider session")?;
        self.amux(&["stop", name]).await?;
        self.listed(name, "exited").await?;
        let mark = follow.snapshot().items.len();
        let before = follow.snapshot();
        // Resumed bare, then prompted once it takes input, as the recording
        // was: a prompt given to the resume would be its first input.
        self.amux(&["resume", name]).await?;
        self.listed(name, "idle").await?;
        self.send(
            Scenario::Resume,
            "Reply with the single word two and nothing else.",
        )
        .await?;
        expect_outcome(follow.turn_end(2).await?, wire::TurnOutcome::Completed)?;
        self.note_model(&follow);
        let live = follow.snapshot();
        let resumed: Vec<wire::Boundary> = live
            .ordered()
            .into_iter()
            .skip(mark)
            .filter_map(|item| boundary(self.kind, item))
            .filter(|boundary| boundary.kind() == wire::BoundaryKind::Resumed)
            .collect();
        let Some(boundary) = resumed.first() else {
            return Err("the new incarnation has no resume boundary".into());
        };
        if !boundary.provider_session.is_empty() && boundary.provider_session != session {
            return Err(format!(
                "resumed into session {} instead of {session}",
                boundary.provider_session
            ));
        }
        let now = live.state(self.kind).provider_session;
        if now.as_deref() != Some(session.as_str()) {
            return Err(format!("the session is now {now:?}, not {session}"));
        }
        // The second incarnation alone, judged against one response with
        // its start boundary a resume.
        let mut recorded = self.recorded(Scenario::Resume)?.shape(self.kind);
        for token in &mut recorded.items {
            if token == "boundary started" {
                *token = "boundary resumed".into();
            }
        }
        recorded.phases.retain(|phase| *phase != Phase::Starting);
        let mut second = live.since(mark, &before).shape(self.kind);
        second.phases.retain(|phase| *phase != Phase::Starting);
        judge(&recorded, &second)?;
        Ok(Verdict::Pass)
    }

    /// Waits until `amux ls` lists the agent in the named state.
    async fn listed(&self, name: &str, state: &str) -> Result<(), String> {
        wait_for(READY, || async {
            self.amux(&["ls"]).await.ok().filter(|listing| {
                listing.lines().any(|line| {
                    let mut words = line.split_whitespace();
                    words.next() == Some(name)
                        && words
                            .nth(2)
                            .is_some_and(|word| word.trim_end_matches(':') == state)
                })
            })
        })
        .await
        .map(drop)
        .ok_or_else(|| format!("amux ls never listed {name} {state}"))
    }

    fn note_model(&self, follow: &Follow) {
        if let Some(model) = follow.snapshot().state(self.kind).model {
            self.model.lock().unwrap().get_or_insert(model);
        }
    }
}

impl Drop for Install {
    fn drop(&mut self) {
        if let Ok(mut daemon) = self.daemon.try_lock() {
            let _ = daemon.start_kill();
        }
    }
}

/// A scratch CODEX_HOME holding only the operator's login: their config,
/// tool servers and history stay out of the run.
fn seed_codex_home(root: &Path) -> Result<PathBuf, String> {
    let source = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .ok_or("no Codex login: neither CODEX_HOME nor HOME is set")?;
    let auth = source.join("auth.json");
    if !auth.exists() {
        return Err(format!(
            "no Codex login: {} does not exist; run `codex login`",
            auth.display()
        ));
    }
    let home = root.join("codex-home");
    std::fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    std::fs::copy(&auth, home.join("auth.json")).map_err(|error| error.to_string())?;
    // Codex refuses a pristine home before it starts its app server; these
    // are its local setup sentinels, nothing of the operator's.
    for (file, text) in [
        (".personality_migration", "v1\n".to_owned()),
        (".sandbox_migration", "v1\n".to_owned()),
        ("installation_id", format!("{}\n", uuid::Uuid::new_v4())),
    ] {
        std::fs::write(home.join(file), text).map_err(|error| error.to_string())?;
    }
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    Ok(home)
}

/// One agent's session as a subscribed client has read it.
#[derive(Clone, Default)]
struct Log {
    items: BTreeMap<String, (u64, Item)>,
    snapshots: Vec<Snapshot>,
    /// The last catalogue a replay wrote.
    catalogue: Option<wire::Catalogue>,
}

impl Log {
    /// Upserts an item. A replayed item has no order yet: it takes the
    /// next, as the daemon assigns one on first commit.
    fn item(&mut self, item: Item) {
        let next = self.items.len() as u64 + 1;
        let order = match self.items.get(&item.key) {
            Some((order, _)) => *order,
            None if item.order == 0 => next,
            None => item.order,
        };
        self.items.insert(item.key.clone(), (order, item));
    }

    fn ordered(&self) -> Vec<&Item> {
        let mut items: Vec<&(u64, Item)> = self.items.values().collect();
        items.sort_by_key(|(order, _)| *order);
        items.into_iter().map(|(_, item)| item).collect()
    }

    fn phase(&self) -> Phase {
        self.snapshots
            .last()
            .map(Snapshot::phase)
            .unwrap_or(Phase::Starting)
    }

    fn state(&self, kind: Kind) -> AgentState {
        self.snapshots
            .last()
            .map(|snapshot| AgentState::from_snapshot(kind, snapshot))
            .unwrap_or_default()
    }

    /// Something of the provider's own follows the prompt.
    fn under_way(&self, kind: Kind) -> bool {
        let items = self.ordered();
        items
            .iter()
            .position(|item| item_token(kind, item).as_deref() == Some("prompt"))
            .is_some_and(|at| items.len() > at + 1)
    }

    /// The items after the first `mark` in order, and the snapshots since
    /// `before` was taken.
    fn since(&self, mark: usize, before: &Log) -> Log {
        let keep: BTreeSet<String> = self
            .ordered()
            .into_iter()
            .skip(mark)
            .map(|item| item.key.clone())
            .collect();
        Log {
            items: self
                .items
                .iter()
                .filter(|(key, _)| keep.contains(*key))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            snapshots: self.snapshots[before.snapshots.len().min(self.snapshots.len())..].to_vec(),
            catalogue: None,
        }
    }

    fn turns(&self, kind: Kind) -> Vec<wire::TurnOutcome> {
        self.ordered()
            .into_iter()
            .filter_map(|item| turn(kind, item))
            .map(|turn| turn.outcome())
            .collect()
    }

    /// A complete answer item: the provider said something back.
    fn answered(&self, kind: Kind) -> bool {
        self.items.values().any(|(_, item)| {
            matches!(
                ItemBody::decode(kind, &item.body).class(),
                ui_state::ItemClass::Prose { complete: true }
            )
        })
    }

    fn shape(&self, kind: Kind) -> Shape {
        let mut items = Vec::new();
        for item in self.ordered() {
            if let Some(token) = item_token(kind, item)
                && items.last() != Some(&token)
            {
                items.push(token);
            }
        }
        // Every agent begins starting; a client that subscribes after the
        // create may first see a later phase.
        let mut phases = vec![Phase::Starting];
        let mut asks = Vec::new();
        let mut seen = BTreeSet::new();
        for snapshot in &self.snapshots {
            if phases.last() != Some(&snapshot.phase()) {
                phases.push(snapshot.phase());
            }
            for ask in AgentState::from_snapshot(kind, snapshot).asks {
                if seen.insert(ask.key().to_owned()) {
                    let token = ask_token(&ask);
                    if asks.last() != Some(&token) {
                        asks.push(token);
                    }
                }
            }
        }
        Shape {
            items,
            phases,
            asks,
        }
    }
}

/// The structure of a session: its item classes in order (prose and
/// thinking left out, their count being the model's choice), its phase
/// changes and the asks it opened, consecutive repeats collapsed.
#[derive(Debug, PartialEq)]
struct Shape {
    items: Vec<String>,
    phases: Vec<Phase>,
    asks: Vec<String>,
}

impl Shape {
    /// Terminal Claude's capture carries none of amux's keys, so its replay
    /// reads idle from the interruption that cancels the last question until
    /// the row of the reply amux typed in its place; the session, which typed
    /// the reply the moment the interruption landed, reads working throughout.
    fn reply_typed_at_once(&mut self) {
        if let Some(at) = self
            .phases
            .iter()
            .rposition(|phase| *phase == Phase::NeedsYou)
            && self.phases[at + 1..].starts_with(&[Phase::Working, Phase::Idle, Phase::Working])
        {
            self.phases.drain(at + 2..at + 4);
        }
    }
}

/// The live shape matches the recording's. A recording that stops before
/// its turn ends (terminal Claude's permission recording ends at the tool
/// result) judges the live run only as far as it goes; the scenario checks
/// how the live turn ended itself.
fn judge(recorded: &Shape, live: &Shape) -> Result<(), String> {
    if recorded == live {
        return Ok(());
    }
    let unfinished = !recorded.items.iter().any(|item| item.starts_with("turn "));
    if unfinished
        && live.items.starts_with(&recorded.items)
        && live.phases.starts_with(&recorded.phases)
        && live.asks == recorded.asks
    {
        return Ok(());
    }
    Err(format!(
        "the committed shape differs from the recording's\n    recorded {recorded:?}\n    live     {live:?}"
    ))
}

fn expect_outcome(outcome: wire::TurnOutcome, want: wire::TurnOutcome) -> Result<(), String> {
    if outcome == want {
        Ok(())
    } else {
        Err(format!(
            "the turn ended {}, not {}",
            outcome.as_str_name(),
            want.as_str_name()
        ))
    }
}

fn item_token(kind: Kind, item: &Item) -> Option<String> {
    use wire::claude_pty_item::Kind as Pty;
    use wire::claude_sdk_item::Kind as Sdk;
    use wire::codex_item::Kind as Codex;
    if let Some(boundary) = boundary(kind, item) {
        return Some(format!(
            "boundary {}",
            short(boundary.kind().as_str_name(), "BOUNDARY_KIND_")
        ));
    }
    if let Some(turn) = turn(kind, item) {
        return Some(format!(
            "turn {}",
            short(turn.outcome().as_str_name(), "TURN_OUTCOME_")
        ));
    }
    let tool = |state: i32, decision: Option<&wire::ToolDecision>| {
        let state = wire::ToolState::try_from(state).unwrap_or_default();
        let mut token = format!("tool {}", short(state.as_str_name(), "TOOL_STATE_"));
        if let Some(decision) = decision.filter(|d| d.outcome != 0) {
            token.push(' ');
            token.push_str(&short(
                decision.outcome().as_str_name(),
                "DECISION_OUTCOME_",
            ));
        }
        token
    };
    let ask = |ask: &wire::AskItem| {
        let outcome = ask.closed.as_ref().map_or("open".to_owned(), |closed| {
            short(closed.outcome().as_str_name(), "ASK_OUTCOME_")
        });
        format!("ask {outcome}")
    };
    match ItemBody::decode(kind, &item.body) {
        ItemBody::ClaudePty(body) => match body {
            Pty::Prompt(_) => Some("prompt".into()),
            Pty::Steer(_) => Some("steer".into()),
            Pty::Tool(call) => Some(tool(call.state, call.decision.as_ref())),
            Pty::ApiError(_) => Some("error".into()),
            Pty::AgentMessage(_) => Some("agent message".into()),
            Pty::Plan(plan) => Some(plan_token(&plan)),
            _ => None,
        },
        ItemBody::ClaudeSdk(body) => match body {
            Sdk::Prompt(_) => Some("prompt".into()),
            Sdk::Steer(_) => Some("steer".into()),
            Sdk::Tool(call) => Some(tool(call.state, call.decision.as_ref())),
            Sdk::ApiError(_) => Some("error".into()),
            Sdk::AgentMessage(_) => Some("agent message".into()),
            Sdk::Ask(item) => Some(ask(&item)),
            Sdk::Plan(plan) => Some(plan_token(&plan)),
            _ => None,
        },
        ItemBody::Codex(body) => match body {
            Codex::Prompt(_) => Some("prompt".into()),
            Codex::Steer(_) => Some("steer".into()),
            Codex::Work(work) => Some(tool(work.state, work.decision.as_ref())),
            Codex::Error(_) => Some("error".into()),
            Codex::AgentMessage(_) => Some("agent message".into()),
            Codex::Ask(item) => Some(ask(&item)),
            Codex::Plan(plan) => Some(plan_token(&plan)),
            _ => None,
        },
        ItemBody::Undecodable => Some("undecodable".into()),
    }
}

fn plan_token(plan: &wire::Plan) -> String {
    format!(
        "plan {}",
        short(plan.verdict().as_str_name(), "PLAN_VERDICT_")
    )
}

fn boundary(kind: Kind, item: &Item) -> Option<wire::Boundary> {
    use wire::claude_pty_item::Kind as Pty;
    use wire::claude_sdk_item::Kind as Sdk;
    use wire::codex_item::Kind as Codex;
    match ItemBody::decode(kind, &item.body) {
        ItemBody::ClaudePty(Pty::Boundary(boundary))
        | ItemBody::ClaudeSdk(Sdk::Boundary(boundary))
        | ItemBody::Codex(Codex::Boundary(boundary)) => Some(boundary),
        _ => None,
    }
}

fn turn(kind: Kind, item: &Item) -> Option<wire::Turn> {
    use wire::claude_pty_item::Kind as Pty;
    use wire::claude_sdk_item::Kind as Sdk;
    use wire::codex_item::Kind as Codex;
    match ItemBody::decode(kind, &item.body) {
        ItemBody::ClaudePty(Pty::Turn(turn))
        | ItemBody::ClaudeSdk(Sdk::Turn(turn))
        | ItemBody::Codex(Codex::Turn(turn)) => Some(turn),
        _ => None,
    }
}

fn ask_token(ask: &OpenAsk) -> String {
    match ask {
        OpenAsk::Claude(ask) => match &ask.body {
            Some(wire::ask::Body::Permission(_)) => "permission",
            Some(wire::ask::Body::Question(_)) => "question",
            Some(wire::ask::Body::Plan(_)) => "plan",
            Some(wire::ask::Body::Form(_)) => "form",
            Some(wire::ask::Body::Link(_)) => "link",
            Some(wire::ask::Body::Unanswerable(_)) => "unanswerable",
            None => "empty",
        },
        OpenAsk::Codex(ask) => match &ask.body {
            Some(wire::codex_ask::Body::Command(_)) => "command",
            Some(wire::codex_ask::Body::FileChange(_)) => "file change",
            Some(wire::codex_ask::Body::McpForm(_)) => "form",
            Some(wire::codex_ask::Body::McpLink(_)) => "link",
            Some(wire::codex_ask::Body::Access(_)) => "access",
            Some(wire::codex_ask::Body::Question(_)) => "question",
            Some(wire::codex_ask::Body::McpTool(_)) => "tool",
            Some(wire::codex_ask::Body::Plan(_)) => "plan",
            None => "empty",
        },
    }
    .to_owned()
}

fn short(name: &str, prefix: &str) -> String {
    name.trim_start_matches(prefix).to_lowercase()
}

/// The snapshot fields the provider has told the interpreter about.
fn known(state: &AgentState) -> BTreeSet<&'static str> {
    [
        ("model", state.model.is_some()),
        ("effort", state.effort.is_some()),
        ("permission", state.permission.is_some()),
        ("mode", state.mode.is_some()),
        ("approval_policy", state.approval_policy.is_some()),
        ("session", state.provider_session.is_some()),
        ("context", state.context.known),
        ("usage", state.usage.state() != wire::UsageState::Unknown),
        ("servers", state.servers.state != 0),
        ("sign_in", state.sign_in.state != 0),
    ]
    .into_iter()
    .filter_map(|(name, known)| known.then_some(name))
    .collect()
}

struct Follow {
    kind: Kind,
    log: Arc<Mutex<Log>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Follow {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Follow {
    fn snapshot(&self) -> Log {
        self.log.lock().unwrap().clone()
    }

    async fn until(
        &self,
        patience: Duration,
        what: &str,
        done: impl Fn(&Log) -> bool,
    ) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + patience;
        while !done(&self.log.lock().unwrap()) {
            if tokio::time::Instant::now() > deadline {
                return Err(format!("timed out after {patience:?} waiting for {what}"));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }

    async fn ready(&self) -> Result<(), String> {
        self.until(READY, "the agent to take input", |log| {
            log.phase() != Phase::Starting
        })
        .await
    }

    /// Why the provider cannot run turns here, if it says it is signed out.
    fn signed_out(&self) -> Option<String> {
        let state = self.snapshot().state(self.kind);
        match state.sign_in.state() {
            wire::SignInState::SignedOut | wire::SignInState::Expired => Some(format!(
                "the provider is not signed in: {}",
                state.sign_in.message
            )),
            _ => None,
        }
    }

    /// Waits for the `n`th turn to end and returns how it ended.
    async fn turn_end(&self, n: usize) -> Result<wire::TurnOutcome, String> {
        let kind = self.kind;
        self.until(TURN, "the turn to end", |log| log.turns(kind).len() >= n)
            .await?;
        Ok(self.snapshot().turns(kind)[n - 1])
    }
}

async fn channel(path: &Path) -> std::io::Result<Channel> {
    let path = path.to_owned();
    Endpoint::from_static("http://amux.live")
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();
            async move {
                agent_dir::local_socket::connect(&path)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .map_err(std::io::Error::other)
}

async fn wait_for<T, F: std::future::Future<Output = Option<T>>>(
    patience: Duration,
    mut attempt: impl FnMut() -> F,
) -> Option<T> {
    let deadline = tokio::time::Instant::now() + patience;
    loop {
        if let Some(value) = attempt().await {
            return Some(value);
        }
        if tokio::time::Instant::now() > deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The id of the agent `amux create` just created, from its output.
fn created_id(output: &str) -> Option<Vec<u8>> {
    let start = output.find('(')? + 1;
    let end = output[start..].find(')')? + start;
    uuid::Uuid::parse_str(&output[start..end])
        .ok()
        .map(|id| id.as_bytes().to_vec())
}
