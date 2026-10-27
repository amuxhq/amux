//! A daemon on a temporary installation, running real agent processes
//! (`amux agent`) on the scripted fake providers.

#![allow(dead_code, unused_imports)]

pub mod synthetic;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use node::{Daemon, Launch, ProfileId, ProfileRuntime, StartOptions};
pub use patience::{PATIENCE, Stuck, holds_for, until};
use provider_fakes::script::{SCRIPT_ENV, Script, Step};
use wire::{
    AgentRef, ClaudeSdkInput, CreateAgentRequest, Input, Kind, PromptInput, PromptSubmission,
    claude_sdk_input,
};

/// The amux binary and the fake providers, built once per test run.
pub fn binaries() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let status = provider_fakes::cargo::command()
            .args([
                "build",
                "--locked",
                "-p",
                "amux",
                "-p",
                "provider-fakes",
                "--bins",
            ])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status()
            .expect("cargo runs");
        assert!(
            status.success(),
            "building amux and the fake providers failed"
        );
        // target/<profile>/deps/<this test> -> target/<profile>
        let exe = std::env::current_exe().expect("the test binary's path");
        exe.parent()
            .and_then(Path::parent)
            .expect("a target directory")
            .to_owned()
    })
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
            step_budget: daemon_core::StepBudget::TEST,
            // An agent's core gets the daemon's test budget, for the same
            // reason: a loaded debug-build run stretches ordinary steps.
            agent_step_budget_micros: daemon_core::StepBudget::TEST.micros,
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
        launch.stop_deadline_ms = 15_000;
        launch
    }

    pub fn options(&self, boot_id: &str, launch: Launch) -> StartOptions {
        StartOptions {
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

    /// The profile store, opened beside the daemon's own connection.
    fn store(&self) -> store::Sqlite {
        store::Sqlite::open(&self.profile_dir().join(node::STORE), [0; 16])
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
        let Ok(bundles) = std::fs::read_dir(self.data_dir.join(node::REPORTS)) else {
            return Vec::new();
        };
        bundles
            .filter_map(|bundle| {
                let manifest = std::fs::read(bundle.ok()?.path().join(node::MANIFEST)).ok()?;
                let manifest: serde_json::Value = serde_json::from_slice(&manifest).ok()?;
                let reason = manifest["reason"].as_str()?;
                (reason.starts_with(node::CORE_DUMP) || reason.starts_with(node::AGENT_DUMP))
                    .then(|| reason.to_owned())
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
pub fn create(cwd: &Path, name: &str, prompt: Option<&str>) -> CreateAgentRequest {
    CreateAgentRequest {
        agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
        host: None,
        name: Some(name.to_owned()),
        parent: None,
        cwd: cwd.to_string_lossy().into_owned(),
        agent: wire::create_agent_request::Agent::defaults(Kind::ClaudeSdk),
        initial_prompt: prompt.map(|text| submission(b"p0", text)),
        new_worktree: false,
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
