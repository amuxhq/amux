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
    AgentParent, ClaudeCreateConfig, ClaudeSdkInput, CreateAgentRequest, Input, Kind, PromptInput,
    StopMode, claude_sdk_input, create_agent_request,
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
}

impl Default for Install {
    fn default() -> Self {
        Self::new()
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
        host_id: None,
        name: Some(name.to_owned()),
        parent: None,
        initial_prompt: prompt.map(|text| sdk_prompt(b"p0", text)),
        cwd: cwd.to_string_lossy().into_owned(),
        kind: Kind::ClaudeSdk as i32,
        config: Some(create_agent_request::Config::Claude(
            ClaudeCreateConfig::default(),
        )),
        host_name: None,
        new_worktree: false,
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

pub fn parent_of(agent: &wire::Agent) -> Option<AgentParent> {
    agent.parent.clone()
}

/// Whether something listens on the local socket named `path`. Dialled,
/// not looked for: on Windows the socket is a named pipe and no file
/// exists at its path.
pub async fn listens(path: &Path) -> bool {
    agent_dir::local_socket::connect(path).await.is_ok()
}

/// Kills every agent the runtime is running, so none outlives its test.
pub async fn kill_all(runtime: &ProfileRuntime) {
    for id in runtime.live() {
        let _ = runtime.stop(id, StopMode::Kill).await;
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
