//! A daemon on a temporary installation, running real agent processes
//! (`amux agent`) on the scripted fake providers.

#![allow(dead_code, unused_imports)]

pub mod synthetic;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use node::{Daemon, Launch, ProfileId, ProfileRuntime, StartOptions};
pub use patience::{PATIENCE, holds_for, until};
use provider_fakes::script::{SCRIPT_ENV, Script, Step};
use wire::{
    AgentRef, ClaudeSdkInput, Input, Kind, PromptInput, PromptSubmission, claude_sdk_input,
};

/// The amux binary and the fake providers.
pub fn binaries() -> &'static Path {
    provider_fakes::binaries::dir()
}

fn binary(name: &str) -> PathBuf {
    binaries().join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

/// A temporary installation with one profile.
pub struct Install {
    root: tempfile::TempDir,
    pub data_dir: PathBuf,
    pub work: PathBuf,
    pub profile: ProfileId,
    /// The test makes the daemon core ask for dumps on purpose.
    core_dumps_expected: std::sync::atomic::AtomicBool,
}

impl Install {
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("a temp dir");
        let data_dir = root.path().join("data");
        let work = root.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let profile = node::create_profile(&data_dir).expect("a profile");
        Self {
            root,
            data_dir,
            work,
            profile,
            core_dumps_expected: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// An installation holding a copy of `data_dir`, as its one profile.
    pub fn holding(data_dir: &Path) -> Self {
        let root = tempfile::tempdir().expect("a temp dir");
        let copy = root.path().join("data");
        let work = root.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        copy_files(data_dir, &copy);
        let profiles = node::profile_dir(&copy, ProfileId::nil());
        let profile = std::fs::read_dir(profiles.parent().unwrap())
            .unwrap()
            .find_map(|entry| entry.unwrap().file_name().to_str()?.parse().ok())
            .expect("the data directory holds a profile");
        Self {
            root,
            data_dir: copy,
            work,
            profile,
            core_dumps_expected: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// A path under the installation's scratch directory, for gates.
    pub fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// Writes a script the fake providers play and returns what agents
    /// start with: headless Claude on its fake, running `amux agent`.
    pub fn launch(&self, name: &str, steps: Vec<Step>) -> Launch {
        let script = self.root.path().join(format!("{name}.json"));
        let script_text = serde_json::to_string(&Script {
            steps,
            ..Script::default()
        })
        .unwrap();
        std::fs::write(&script, script_text).unwrap();
        let mut launch = Launch {
            install_path: binary("amux"),
            claude_command: binary("fake-claude-sdk").to_string_lossy().into_owned(),
            codex_command: binary("fake-codex").to_string_lossy().into_owned(),
            // Untimed, daemon and agent cores alike: a loaded debug-build
            // run stretches ordinary steps past any budget, and a dump for
            // that overrun would land in `reports/` while a test reads it.
            step_budget: daemon_core::StepBudget::UNLIMITED,
            agent_step_budget_micros: daemon_core::StepBudget::UNLIMITED.micros,
            ..Launch::default()
        };
        launch
            .provider_env
            .insert(SCRIPT_ENV.to_owned(), script.to_string_lossy().into_owned());
        launch.provider_env.insert(
            "CLAUDE_CONFIG_DIR".to_owned(),
            self.root
                .path()
                .join("claude")
                .to_string_lossy()
                .into_owned(),
        );
        // Long enough to ride out a daemon restart in a test, short enough
        // that an agent a failed test leaves behind goes away on its own.
        launch.agent.grace_secs = 20;
        launch.agent.drain_secs = 5;
        launch.core.stop_deadline_ms = 15_000;
        launch
    }

    pub fn options(&self, boot_id: &str, launch: Launch) -> StartOptions {
        StartOptions {
            // A home of its own: the person's real one holds the Claude Code
            // and Codex sessions the folder list would offer.
            settings: node::SettingsOptions {
                home: Some(self.root.path().join("home")),
                ..Default::default()
            },
            data_dir: self.data_dir.clone(),
            boot_id: Some(boot_id.to_owned()),
            launch,
            clock: Arc::new(agent_dir::SystemClock),
            push: Arc::new(node::NoopSender),
            daemon_log: None,
            supervisor_log: None,
            front_door: None,
            edge: node::EdgeOptions::default(),
            analytics: node::Telemetry::Off,
            auto_reports: node::AutoReports::off(),
        }
    }

    pub async fn start(&self, boot_id: &str, launch: Launch) -> Daemon {
        node::start(self.options(boot_id, launch), None)
            .await
            .expect("the daemon starts")
    }

    pub fn profile_dir(&self) -> PathBuf {
        node::profile_dir(&self.data_dir, self.profile)
    }

    pub fn agent_dir(&self, id: uuid::Uuid) -> PathBuf {
        self.profile_dir().join(node::AGENTS).join(id.to_string())
    }

    /// The profile store, opened beside the daemon's own connection: a
    /// test sets up what no input produces by writing through it.
    pub fn store(&self) -> store::Sqlite {
        store::Sqlite::open(
            &self.profile_dir().join(node::STORE),
            self.profile.as_bytes().to_vec(),
        )
        .expect("the profile store opens")
    }

    /// The row this profile keeps for the paired `peer`, when it has one.
    pub fn trusted_host(&self, peer: node::HostId) -> Option<store::TrustedHost> {
        self.store()
            .trusted_hosts()
            .expect("the trusted hosts read")
            .into_iter()
            .find(|row| row.host_id == peer.as_bytes())
    }

    /// Rewrites the row this profile keeps for `peer`, with no daemon
    /// running on it.
    pub fn edit_trusted_host(
        &self,
        peer: node::HostId,
        edit: impl FnOnce(&mut store::TrustedHost),
    ) {
        let mut row = self.trusted_host(peer).expect("the peer is trusted");
        edit(&mut row);
        self.store()
            .put_trusted_host(&row, store::Durability::Normal)
            .expect("the row writes");
    }
}

impl Default for Install {
    fn default() -> Self {
        Self::new()
    }
}

impl Install {
    /// Says this test makes the daemon core ask for dumps on purpose, so
    /// the installation may be left with them.
    pub fn expect_core_dumps(&self) {
        self.core_dumps_expected
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// The reason of every dump in the installation's reports that the
    /// daemon core or an agent's core asked for: an invariant it broke, or
    /// a step over its budget.
    pub fn core_dumps(&self) -> Vec<String> {
        self.bundles()
            .into_iter()
            .filter_map(|bundle| {
                let manifest = std::fs::read(bundle.join(node::MANIFEST)).ok()?;
                let manifest: serde_json::Value = serde_json::from_slice(&manifest).ok()?;
                let reason = manifest["reason"].as_str()?;
                (reason.starts_with(node::CORE_DUMP) || reason.starts_with(node::AGENT_DUMP))
                    .then(|| reason.to_owned())
            })
            .collect()
    }

    /// Every bundle sealed in the installation's reports. A dump assembles
    /// its bundle in a hidden directory, writes the manifest there last and
    /// renames it into place, so a hidden one with a manifest is a bundle
    /// about to move: a path read through it may already be gone.
    pub fn bundles(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(self.data_dir.join(node::REPORTS)) else {
            return Vec::new();
        };
        entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                (!entry.file_name().to_string_lossy().starts_with('.')).then(|| entry.path())
            })
            .collect()
    }
}

impl Drop for Install {
    /// No test leaves a dump the daemon core or an agent's core asked for
    /// because one of its invariants failed: production asserts them, so
    /// such a dump is a bug.
    /// A test that breaks one on purpose says so first. Dumps for a step
    /// over its budget are printed and tolerated: an overrun on a loaded
    /// machine is time the core thread was not running, not work inside the
    /// step (measured under the desktop flood: step CPU at most 14 ms
    /// against 100 to 833 ms wall), and a parallel debug-build run on a
    /// four-core CI runner is exactly that machine.
    fn drop(&mut self) {
        if std::thread::panicking() || *self.core_dumps_expected.get_mut() {
            return;
        }
        let (violations, slow): (Vec<_>, Vec<_>) =
            self.core_dumps().into_iter().partition(|reason| {
                reason.starts_with(node::CORE_VIOLATION_DUMP)
                    || reason.starts_with(node::AGENT_VIOLATION_DUMP)
            });
        for reason in &slow {
            println!("tolerated a dump a core asked for: {reason}");
        }
        assert!(
            violations.is_empty(),
            "a core asked for dumps for broken invariants: {violations:#?}"
        );
    }
}

pub fn runtime(daemon: &Daemon, install: &Install) -> Arc<ProfileRuntime> {
    daemon
        .profile(install.profile)
        .expect("the profile runs")
        .clone()
}

/// A headless Claude creation in `cwd`, with a first prompt when given.
pub fn create(cwd: &Path, name: &str, prompt: Option<&str>) -> wire::CreateAgentRequest {
    wire::CreateAgentRequest {
        agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
        name: Some(name.to_owned()),
        cwd: cwd.to_string_lossy().into_owned(),
        agent: wire::create_agent_request::Agent::defaults(Kind::ClaudeSdk),
        initial_prompt: prompt.map(|text| submission(b"p0", text)),
        ..wire::CreateAgentRequest::default()
    }
}

/// Copies the regular files under `from`: a socket means nothing once its
/// process is gone.
pub fn copy_files(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        // Through links: a build that stages a fixture may link it in.
        let kind = std::fs::metadata(entry.path()).unwrap().file_type();
        if kind.is_dir() {
            copy_files(&entry.path(), &to.join(entry.file_name()));
        } else if kind.is_file() {
            let copy = to.join(entry.file_name());
            std::fs::copy(entry.path(), &copy).unwrap();
            // The copy is the test's to write, whatever mode the fixture
            // was staged with (a build's inputs are read-only).
            let mut permissions = std::fs::metadata(&copy).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            std::fs::set_permissions(&copy, permissions).unwrap();
        }
    }
}

/// A first prompt for a create or a resume.
pub fn submission(id: &[u8], text: &str) -> PromptSubmission {
    PromptSubmission {
        input_id: id.to_vec(),
        prompt: Some(PromptInput {
            text: text.to_owned(),
            attachments: Vec::new(),
        }),
    }
}

pub fn sdk_prompt(id: &[u8], text: &str) -> Input {
    Input::new(
        id.to_vec(),
        ClaudeSdkInput {
            of: Some(claude_sdk_input::Of::Prompt(PromptInput {
                text: text.to_owned(),
                attachments: Vec::new(),
            })),
        },
    )
}

pub fn id_of(agent: &wire::Agent) -> uuid::Uuid {
    uuid::Uuid::from_slice(&agent.agent_id).unwrap()
}

pub fn parent_of(agent: &wire::Agent) -> Option<AgentRef> {
    agent.parent.clone()
}

/// Whether something listens on the local socket named `path`. Dialled,
/// not looked for: on Windows the socket is a named pipe and no file
/// exists at its path.
pub async fn listens(path: &Path) -> bool {
    agent_dir::local_socket::connect(path).await.is_ok()
}

/// The agent process serving the agent directory `dir`, pinned: whoever
/// serves its `private/agent.sock`, as the operating system names that
/// socket's server. For a test that freezes (`suspend`), thaws (`resume`)
/// or kills an agent from outside, as a stalled or crashed machine would.
pub async fn agent_process(dir: &Path) -> os::process::ProcessRef {
    let (stream, pid) =
        agent_dir::local_socket::connect_to_process(&agent_dir::agent_sock::path(dir))
            .await
            .expect("the agent answers on its socket");
    stream.close();
    os::process::ProcessRef::open(pid.expect("the socket names its server"))
        .expect("the agent process runs")
}

/// A gRPC channel to a local socket.
pub async fn local_channel(path: &Path) -> tonic::transport::Channel {
    let path = path.to_owned();
    tonic::transport::Endpoint::from_static("http://amux.test")
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();
            async move {
                agent_dir::local_socket::connect(&path)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .expect("the socket answers")
}

/// Kills every agent the runtime is running, so none outlives its test.
/// Waits until calls from `edge` to `host` have a route, or `patience`
/// passes.
pub async fn wait_for_route(
    edge: &node::Edge,
    host: node::HostId,
    patience: std::time::Duration,
) -> bool {
    tokio::time::timeout(patience, async {
        while edge.via(host).await == node::harness::LiveRoute::Offline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok()
}

pub async fn kill_all(runtime: &ProfileRuntime) {
    for id in runtime.live() {
        let _ = runtime.kill_agent(id).await;
    }
}

/// A step that holds the turn until `path` exists.
pub fn wait_for(path: &Path) -> Step {
    Step::WaitFor {
        path: path.to_owned(),
    }
}

pub fn text(text: &str) -> Step {
    Step::Text {
        chunks: vec![text.to_owned()],
    }
}

/// What this process logs: everything at the level the daemon logs at by
/// default, and the driver's and the link channels' debug lines, which
/// tests count. Installed once for the whole binary: a subscriber set per
/// test would race the others' callsites.
pub fn captured_log() -> &'static Arc<std::sync::Mutex<Vec<u8>>> {
    static LOG: std::sync::OnceLock<Arc<std::sync::Mutex<Vec<u8>>>> = std::sync::OnceLock::new();
    LOG.get_or_init(|| {
        use tracing_subscriber::layer::{Layer as _, SubscriberExt as _};
        use tracing_subscriber::util::SubscriberInitExt as _;

        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = log.clone();
        let filter = tracing_subscriber::filter::Targets::new()
            .with_default(tracing::Level::INFO)
            .with_target("node::driver", tracing::Level::DEBUG)
            .with_target("node::link::channels", tracing::Level::DEBUG);
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(move || Captured(writer.clone()))
                    .with_filter(filter),
            )
            .try_init()
            .expect("the only subscriber this binary installs");
        log
    })
}

struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The host services of `runtime` as `caller` reaches them in memory: a
/// person on the profile's socket, or one of this host's agents on its
/// tools socket, named as its row names it.
pub async fn served_to(
    runtime: &Arc<ProfileRuntime>,
    caller: Option<node::AgentId>,
) -> node::harness::InMemory {
    use node::harness::{Calling, Served, served_as};
    match caller {
        None => served_as(
            runtime,
            Served::Profile,
            Calling::PersonHere(analytics::Client::Terminal),
        ),
        Some(agent) => {
            // As the tools socket's entry states the agent: from its row.
            let row = runtime.agent(agent).await.ok();
            let (name, incarnation, level) = row.map_or_else(
                || (String::new(), 1, wire::PermissionLevel::Unspecified),
                |row| {
                    let level = row.settings.as_deref().map_or(
                        wire::PermissionLevel::Unspecified,
                        wire::AgentSettings::permission_level,
                    );
                    (row.name, row.incarnation, level)
                },
            );
            served_as(
                runtime,
                Served::Tools(agent),
                Calling::Agent {
                    host: runtime.host(),
                    agent,
                    name,
                    incarnation,
                    level,
                },
            )
        }
    }
}

/// Acting on a host's own agents as its clients do, through its
/// AgentService: as a person, or as the agent `caller` names.
pub trait Acts {
    /// Makes an agent here; an agent that calls is its parent.
    fn spawn(
        &self,
        request: wire::CreateAgentRequest,
        caller: Option<node::AgentId>,
    ) -> impl std::future::Future<Output = Result<wire::Agent, tonic::Status>> + Send;

    /// Sends the agent `envelope` is addressed to its message, under its
    /// id, from `caller`.
    fn send_message(
        &self,
        envelope: wire::Envelope,
        caller: Option<node::AgentId>,
    ) -> impl std::future::Future<Output = Result<wire::SendMessageResponse, tonic::Status>> + Send;
}

impl Acts for Arc<ProfileRuntime> {
    fn spawn(
        &self,
        request: wire::CreateAgentRequest,
        caller: Option<node::AgentId>,
    ) -> impl std::future::Future<Output = Result<wire::Agent, tonic::Status>> + Send {
        let runtime = self.clone();
        async move {
            let mut agents = wire::agent_service_client(served_to(&runtime, caller).await);
            let created = agents.create_agent(request).await?.into_inner();
            Ok(created.agent.expect("a created agent is answered"))
        }
    }

    fn send_message(
        &self,
        envelope: wire::Envelope,
        caller: Option<node::AgentId>,
    ) -> impl std::future::Future<Output = Result<wire::SendMessageResponse, tonic::Status>> + Send
    {
        let runtime = self.clone();
        async move {
            let message = match envelope.body {
                Some(wire::envelope::Body::Message(message)) => Some(message),
                _ => None,
            };
            let request = wire::SendMessageRequest {
                id: envelope.id,
                agent_id: envelope.to.map(|to| to.agent_id).unwrap_or_default(),
                context: envelope.context,
                message,
            };
            let mut agents = wire::agent_service_client(served_to(&runtime, caller).await);
            Ok(agents.send_message(request).await?.into_inner())
        }
    }
}

impl Install {
    /// Options for a daemon that serves its profile's socket, as the
    /// installed one does.
    pub fn serving(&self, boot_id: &str, launch: Launch) -> StartOptions {
        StartOptions {
            front_door: Some(self.path("door.sock")),
            ..self.options(boot_id, launch)
        }
    }
}

/// A channel on `runtime`'s profile socket whose calls name `authority`:
/// `amux.local` for the node itself, or a host's id for a call the node
/// carries to that host.
pub async fn addressed(runtime: &ProfileRuntime, authority: &str) -> tonic::transport::Channel {
    let path = runtime.dir().join(node::PROFILE_SOCKET);
    tonic::transport::Endpoint::from_shared(format!("http://{authority}"))
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();
            async move {
                agent_dir::local_socket::connect(&path)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .expect("the socket answers")
}
