//! Executable specifications for Claude Code's interactive PTY boundary.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use claude::launch::Launch;
use claude_protocol::hooks;
use claude_protocol::stream::PermissionMode;
use replay_support::{IoDirection, IoEvent, Manifest, ReplayReport, SpecEntry, StrictReplay};
use tokio::sync::{mpsc, watch};

use super::{ALLOWED_MODELS, HAIKU, SpecFailure};
use crate::driver::pty::{
    AskAnswer, AskFacts, AskKind, Intent, PermissionAnswer, PlanAnswer, PtyEvent, QuestionAnswer,
    QuestionResponse, RelinkReason,
};
use crate::terminal::keymap::KeymapSources;

const DEFAULT_WAIT: Duration = Duration::from_secs(600);
const WAIT_ENV: &str = "CLAUDE_PTY_SPEC_TIMEOUT_SECS";
const STALL_DIAGNOSTIC_ENV: &str = "CLAUDE_PTY_STALL_DIAGNOSTIC";
const SONNET: &str = "claude-sonnet-5";
const SONNET_FALLBACK_SPECS: &[&str] = &[
    "plan_approve",
    "plan_auto",
    "plan_request_changes",
    "question_mixed",
    "question_every_shape",
];

type SpecFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

struct PtySpecDef {
    entry: SpecEntry,
    args: &'static [&'static str],
    /// Added to Claude's environment for this specification only.
    env: &'static [(&'static str, &'static str)],
    run: for<'a> fn(&'a mut PtySpecSession) -> SpecFuture<'a>,
}

const fn entry(name: &'static str) -> SpecEntry {
    SpecEntry {
        name,
        recording: name,
        allowed_models: ALLOWED_MODELS,
    }
}

macro_rules! definition {
    ($name:ident, $args:expr, $run:ident) => {
        definition!($name, $args, &[], $run)
    };
    ($name:ident, $args:expr, $env:expr, $run:ident) => {
        PtySpecDef {
            entry: entry(stringify!($name)),
            args: $args,
            env: $env,
            run: |session| Box::pin($run(session)),
        }
    };
}

static DEFINITIONS: &[PtySpecDef] = &[
    definition!(prompt, &[], prompt),
    definition!(prompt_multiline, &[], prompt_multiline),
    definition!(prompt_long, &[], prompt_long),
    definition!(tools, &["--dangerously-skip-permissions"], tools),
    definition!(permission_allow_once, &[], permission_allow_once),
    definition!(permission_allow_scoped, &[], permission_allow_scoped),
    definition!(permission_deny_feedback, &[], permission_deny_feedback),
    definition!(plan_approve, &["--permission-mode", "plan"], plan_approve),
    definition!(plan_auto, &["--permission-mode", "plan"], plan_auto),
    definition!(
        plan_request_changes,
        &["--permission-mode", "plan"],
        plan_request_changes
    ),
    definition!(question_single, &[], question_single),
    definition!(question_multi_other, &[], question_multi_other),
    definition!(question_mixed, &[], question_mixed),
    definition!(question_tabs, &[], question_tabs),
    definition!(question_other_single, &[], question_other_single),
    definition!(interrupt, &["--dangerously-skip-permissions"], interrupt),
    definition!(mode_cycle, &[], mode_cycle),
    definition!(compact_relink, &[], compact_relink),
    definition!(clear_relink, &[], clear_relink),
    definition!(question_every_shape, &[], question_every_shape),
    definition!(question_cancelled, &[], question_cancelled),
    definition!(question_skip_reply, &[], question_skip_reply),
    definition!(subagent, &["--dangerously-skip-permissions"], subagent),
    definition!(
        background_shell,
        &["--dangerously-skip-permissions"],
        background_shell
    ),
    definition!(task_list, &["--dangerously-skip-permissions"], task_list),
    definition!(
        file_changes_and_failure,
        &["--dangerously-skip-permissions"],
        file_changes_and_failure
    ),
    definition!(image, &["--dangerously-skip-permissions"], image),
    definition!(thinking, &[], thinking),
    // Nothing listens on the discard port, so every request fails to
    // connect; two retries keep the recording short.
    definition!(
        api_error,
        &[],
        &[
            ("ANTHROPIC_BASE_URL", "http://127.0.0.1:9"),
            ("CLAUDE_CODE_MAX_RETRIES", "2"),
        ],
        api_error
    ),
    // A key the API rejects takes precedence over the owner's own sign-in.
    definition!(
        sign_in_problem,
        &[],
        &[
            ("ANTHROPIC_API_KEY", "sk-ant-api03-amux-spec-invalid"),
            ("CLAUDE_CODE_MAX_RETRIES", "2"),
        ],
        sign_in_problem
    ),
    definition!(
        steer_queued,
        &["--dangerously-skip-permissions"],
        steer_queued
    ),
    definition!(
        steer_send_now,
        &["--dangerously-skip-permissions"],
        steer_send_now
    ),
];

static REGISTRY: [SpecEntry; 32] = [
    entry("prompt"),
    entry("prompt_multiline"),
    entry("prompt_long"),
    entry("tools"),
    entry("permission_allow_once"),
    entry("permission_allow_scoped"),
    entry("permission_deny_feedback"),
    entry("plan_approve"),
    entry("plan_auto"),
    entry("plan_request_changes"),
    entry("question_single"),
    entry("question_multi_other"),
    entry("question_mixed"),
    entry("question_tabs"),
    entry("question_other_single"),
    entry("interrupt"),
    entry("mode_cycle"),
    entry("compact_relink"),
    entry("clear_relink"),
    entry("question_every_shape"),
    entry("question_cancelled"),
    entry("question_skip_reply"),
    entry("subagent"),
    entry("background_shell"),
    entry("task_list"),
    entry("file_changes_and_failure"),
    entry("image"),
    entry("thinking"),
    entry("api_error"),
    entry("sign_in_problem"),
    entry("steer_queued"),
    entry("steer_send_now"),
];

pub fn registry() -> &'static [SpecEntry] {
    &REGISTRY
}

pub fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/pty")
}

/// Apply the launch arguments owned by a PTY specification.
pub fn prepare_launch(entry: &SpecEntry, launch: &mut Launch) -> Result<(), SpecFailure> {
    let definition = definition_for(entry)?;
    launch
        .args
        .extend(definition.args.iter().map(|arg| (*arg).to_owned()));
    // A capture records Claude, not the machine it ran on: the owner's user
    // settings (plugins, skills, agents, permission rules, default mode) and
    // the tool servers their account connects stay out, as for headless
    // captures. The spec's own servers still come in on `--mcp-config`.
    if !launch.args.iter().any(|arg| arg == "--setting-sources") {
        launch
            .args
            .extend(["--setting-sources".to_owned(), "project,local".to_owned()]);
    }
    if !launch.args.iter().any(|arg| arg == "--strict-mcp-config") {
        launch.args.push("--strict-mcp-config".to_owned());
    }
    if !launch.args.iter().any(|arg| arg == "--model") {
        let model = if SONNET_FALLBACK_SPECS.contains(&entry.name) {
            SONNET
        } else {
            HAIKU
        };
        launch.args.extend(["--model".to_owned(), model.to_owned()]);
    }
    Ok(())
}

pub enum Source {
    Live {
        launch: Launch,
        keymaps: KeymapSources,
        size: pty_host::PtySize,
    },
    Recorded {
        replay: StrictReplay,
        manifest: Box<Manifest>,
        keymaps: KeymapSources,
    },
}

#[derive(Debug)]
pub struct RunReport {
    pub provider_version: semver::Version,
    pub model: String,
    pub session_id: String,
    pub io: Vec<IoEvent>,
    pub replay: Option<ReplayReport>,
}

pub async fn run(entry: &SpecEntry, source: Source) -> Result<RunReport, SpecFailure> {
    let definition = definition_for(entry)?;
    let wait = claim_wait(entry)?;
    let (session, capture, controller, provider_version, model, session_id) = match source {
        Source::Live {
            mut launch,
            keymaps,
            size,
        } => {
            prepare_launch(entry, &mut launch)?;
            let model = launch
                .args
                .windows(2)
                .find_map(|pair| (pair[0] == "--model").then(|| pair[1].clone()))
                .ok_or_else(|| failure(entry, "PTY specification launch omitted --model"))?;
            if !entry.allowed_models.contains(&model.as_str()) {
                return Err(failure(entry, format!("model {model} is not allowed")));
            }
            let provider_version = claude::version::probe_version(&launch.binary)
                .await
                .map_err(|error| failure(entry, error.to_string()))?
                .0;
            let session_id = launch.session_id.to_string();
            let env = definition
                .env
                .iter()
                .map(|(name, value)| ((*name).into(), (*value).into()))
                .collect::<Vec<_>>();
            let session = crate::driver::pty::spawn(&launch, &keymaps, size, &env)
                .await
                .map_err(|error| failure(entry, error.to_string()))?;
            (
                session,
                Some(Capture::new()),
                None,
                provider_version,
                model,
                session_id,
            )
        }
        Source::Recorded {
            mut replay,
            manifest,
            keymaps,
        } => {
            let manifest = *manifest;
            let controller = replay.controller.clone();
            let session = crate::driver::pty::from_recording(&mut replay, &manifest, &keymaps)
                .map_err(|error| failure(entry, error.to_string()))?;
            if !replay.transports.is_empty() {
                return Err(failure(entry, "recording has undeclared extra transports"));
            }
            (
                session,
                None,
                Some(controller),
                manifest.recorded.version,
                manifest.recorded.model,
                manifest
                    .session_ids
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "recorded-session".to_owned()),
            )
        }
    };

    let mut session = PtySpecSession::new(session, capture.clone());
    // Keep the driver in this future so failed or cancelled specifications
    // cannot leave a detached replay task waiting for a write.
    let (claim, driver_result) = {
        let driver = async {
            if let Some(controller) = &controller {
                controller.drive().await;
            }
        };
        tokio::pin!(driver);
        let mut driver_finished = false;
        let claim = {
            let claim = tokio::time::timeout(wait, async {
                session.prepare_for_prompt().await?;
                (definition.run)(&mut session).await
            });
            tokio::pin!(claim);
            tokio::select! {
                result = &mut claim => result,
                () = &mut driver => {
                    driver_finished = true;
                    claim.await
                }
            }
        };
        let driver_result = if matches!(claim, Ok(Ok(()))) && !driver_finished {
            // Trailing recorded events can fill the session channels after the
            // last assertion. Drain them until replay completion, not until an
            // arbitrary period of wall-clock silence.
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    tokio::select! {
                        () = &mut driver => break,
                        Some(_) = session.events.recv() => {}
                    }
                }
            })
            .await
            .map_err(|_| failure(entry, "strict replay driver did not finish"))
        } else {
            Ok(())
        };
        (claim, driver_result)
    };
    if capture.is_some() && matches!(claim, Ok(Ok(()))) {
        session.drain_quiet().await;
    }
    if capture.is_some() && claim.is_err() {
        // The timed-out future is gone, so drain anything already queued before
        // killing the provider and freezing the diagnostic capture.
        session.drain_quiet().await;
    }
    let timeout_tail = claim.is_err().then(|| session.screen_tail());
    let close_result = if let Some(controller) = &controller {
        controller
            .close_reads()
            .await
            .map_err(|error| failure(entry, format!("closing recorded streams failed: {error}")))
    } else {
        Ok(())
    };
    let shutdown = tokio::time::timeout(Duration::from_secs(5), async {
        let stop = session.control.stop(pty_host::Terminate::Kill);
        tokio::pin!(stop);
        loop {
            tokio::select! {
                status = &mut stop => break status,
                event = session.events.recv() => {
                    if event.is_none() {
                        break stop.await;
                    }
                }
            }
        }
    })
    .await;
    let stall_diagnostic = if claim.is_err() {
        capture
            .as_ref()
            .map(|capture| persist_stall_diagnostic(entry, &capture.events()))
    } else {
        None
    };
    claim
        .map_err(|_| {
            let diagnostic = match stall_diagnostic {
                Some(Ok(path)) => format!("; diagnostic={}", path.display()),
                Some(Err(error)) => format!("; diagnostic write failed: {error}"),
                None => "; diagnostic unavailable for recorded source".to_owned(),
            };
            failure(
                entry,
                format!(
                    "stalled after {wait:?}{diagnostic}; terminal tail={:?}",
                    timeout_tail.unwrap_or_default()
                ),
            )
        })?
        .map_err(|claim| failure(entry, claim))?;
    driver_result?;
    close_result?;
    shutdown.map_err(|_| failure(entry, "PTY session did not exit during cleanup"))?;
    let replay =
        controller.map(|controller| controller.finish().unwrap_or_else(|error| error.report));
    if replay.as_ref().is_some_and(|report| !report.is_complete()) {
        return Err(failure(
            entry,
            format!("strict replay incomplete: {replay:?}"),
        ));
    }
    Ok(RunReport {
        provider_version,
        model,
        session_id,
        io: capture.map_or_else(Vec::new, |capture| capture.events()),
        replay,
    })
}

fn claim_wait(entry: &SpecEntry) -> Result<Duration, SpecFailure> {
    let Some(raw) = std::env::var_os(WAIT_ENV) else {
        return Ok(DEFAULT_WAIT);
    };
    let raw = raw
        .to_str()
        .ok_or_else(|| failure(entry, format!("{WAIT_ENV} is not UTF-8")))?;
    let seconds = raw.parse::<u64>().map_err(|error| {
        failure(
            entry,
            format!("{WAIT_ENV} must be a positive integer: {error}"),
        )
    })?;
    if seconds == 0 {
        return Err(failure(
            entry,
            format!("{WAIT_ENV} must be a positive integer"),
        ));
    }
    Ok(Duration::from_secs(seconds))
}

fn persist_stall_diagnostic(entry: &SpecEntry, events: &[IoEvent]) -> std::io::Result<PathBuf> {
    let path = std::env::var_os(STALL_DIAGNOSTIC_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "claude-pty-{}-stall-{}.jsonl",
                entry.name,
                std::process::id()
            ))
        });
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut output = String::new();
    for event in events {
        let mut row = serde_json::json!({
            "us": event.us,
            "dir": match event.direction {
                IoDirection::Write => "stdin",
                IoDirection::Read => "stdout",
            },
            "line": event.line,
        });
        if let Some(transport_id) = &event.transport_id {
            row["transport_id"] = transport_id.clone().into();
        }
        if let Some(session_id) = &event.session_id {
            row["session_id"] = session_id.clone().into();
        }
        output.push_str(&serde_json::to_string(&row).map_err(std::io::Error::other)?);
        output.push('\n');
    }
    std::fs::write(&path, output)?;
    Ok(path)
}

fn definition_for(entry: &SpecEntry) -> Result<&'static PtySpecDef, SpecFailure> {
    DEFINITIONS
        .iter()
        .find(|definition| definition.entry == *entry)
        .ok_or_else(|| failure(entry, "specification is not in the Claude PTY registry"))
}

fn failure(entry: &SpecEntry, claim: impl Into<String>) -> SpecFailure {
    SpecFailure {
        spec: entry.name.to_owned(),
        claim: claim.into(),
    }
}

#[derive(Clone)]
struct Capture {
    start: Instant,
    events: Arc<Mutex<Vec<IoEvent>>>,
}

impl Capture {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn push(&self, transport: &str, direction: IoDirection, line: String) {
        self.events
            .lock()
            .expect("capture mutex poisoned")
            .push(IoEvent {
                us: self.start.elapsed().as_micros() as u64,
                direction,
                line,
                transport_id: Some(transport.to_owned()),
                session_id: None,
            });
    }

    fn events(&self) -> Vec<IoEvent> {
        let mut events = self.events.lock().expect("capture mutex poisoned").clone();
        events.sort_by_key(|event| event.us);
        events
    }
}

struct PtySpecSession {
    events: crate::driver::pty::EventStream,
    control: crate::driver::pty::Control,
    capture: Option<Capture>,
    screen: Arc<Mutex<String>>,
    screen_changes: watch::Receiver<()>,
    pending: VecDeque<PtyEvent>,
    tool_use_ids: HashMap<String, String>,
}

impl PtySpecSession {
    fn new(session: crate::driver::pty::Session, capture: Option<Capture>) -> Self {
        if let Some(capture) = capture.clone() {
            let (write_tx, mut write_rx) = mpsc::unbounded_channel();
            session.control.observe_writes(write_tx);
            let writes = capture.clone();
            tokio::spawn(async move {
                while let Some(bytes) = write_rx.recv().await {
                    writes.push(
                        "pty",
                        IoDirection::Write,
                        crate::driver::pty::encode_recording_bytes(&bytes),
                    );
                }
            });
        }
        let screen = Arc::new(Mutex::new(String::new()));
        let (screen_changed, screen_changes) = watch::channel(());
        if let Some(mut output) = session.control.terminal_output() {
            let captured = capture.clone();
            let observed_screen = Arc::clone(&screen);
            tokio::spawn(async move {
                while let Some(bytes) = output.recv().await {
                    if let Some(capture) = &captured {
                        capture.push(
                            "pty",
                            IoDirection::Read,
                            crate::driver::pty::encode_recording_bytes(&bytes),
                        );
                    }
                    let mut screen = observed_screen.lock().expect("screen mutex poisoned");
                    screen.push_str(&String::from_utf8_lossy(&bytes));
                    if screen.len() > 256 * 1024 {
                        let cut = screen.len() - 128 * 1024;
                        screen.drain(..cut);
                    }
                    screen_changed.send_replace(());
                }
            });
        }
        Self {
            events: session.events,
            control: session.control,
            capture,
            screen,
            screen_changes,
            pending: VecDeque::new(),
            tool_use_ids: HashMap::new(),
        }
    }

    async fn prepare_for_prompt(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut trust_answered = false;
        let mut key_answered = false;
        loop {
            let screen = self.screen.lock().expect("screen mutex poisoned").clone();
            // The hint under the composer is laid out with cursor moves, so
            // its words are matched one at a time.
            let composer_up = screen.contains("for agents")
                || (screen.contains("Try")
                    && (screen.contains("shift+tab") || screen.contains("shortcuts")));
            if !trust_answered
                && screen.contains("safety")
                && screen.contains("folder")
                && !composer_up
            {
                if self.capture.is_some() {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                }
                self.control
                    .send_program(vec![
                        crate::driver::pty::PtyInput::Bytes(b"\x1b[B".to_vec()),
                        crate::driver::pty::PtyInput::Delay(300),
                        crate::driver::pty::PtyInput::Bytes(b"\r".to_vec()),
                    ])
                    .await
                    .map_err(|error| error.to_string())?;
                trust_answered = true;
                continue;
            }
            // An API key in the environment makes Claude ask whether to use
            // it, defaulting to no; a specification that sets one wants it
            // used. The dialog's words are laid out with cursor moves, so
            // match single words. Claude remembers the answer per key in
            // ~/.claude.json and asks only the first time.
            if !key_answered
                && screen.contains("Detected")
                && screen.contains("ANTHROPIC_API_KEY")
                && !composer_up
            {
                if self.capture.is_some() {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                }
                self.control
                    .send_program(vec![
                        crate::driver::pty::PtyInput::Bytes(b"\x1b[A".to_vec()),
                        crate::driver::pty::PtyInput::Delay(300),
                        crate::driver::pty::PtyInput::Bytes(b"\r".to_vec()),
                    ])
                    .await
                    .map_err(|error| error.to_string())?;
                key_answered = true;
                continue;
            }
            if composer_up {
                if self.capture.is_some() {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                }
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "Claude composer did not appear after 60s; screen: {screen}"
                ));
            }
            if self.capture.is_some() {
                tokio::time::sleep(Duration::from_millis(200)).await;
            } else {
                tokio::time::timeout(
                    deadline.saturating_duration_since(Instant::now()),
                    self.screen_changes.changed(),
                )
                .await
                .map_err(|_| "Claude composer did not appear after 60s".to_owned())?
                .map_err(|_| {
                    "recorded terminal output ended before the composer appeared".to_owned()
                })?;
            }
        }
    }

    fn screen_tail(&self) -> String {
        let screen = self.screen.lock().expect("screen mutex poisoned");
        let mut tail = screen.chars().rev().take(4_000).collect::<Vec<_>>();
        tail.reverse();
        tail.into_iter().collect()
    }

    async fn next(&mut self) -> Result<PtyEvent, String> {
        let event = if let Some(event) = self.pending.pop_front() {
            event
        } else {
            self.events
                .recv()
                .await
                .ok_or_else(|| "PTY event stream ended".to_owned())?
        };
        if let PtyEvent::Hook(hooks::Payload::PreToolUse(started)) = &event {
            self.tool_use_ids
                .insert(started.tool_name.clone(), started.tool_use_id.clone());
        }
        if let Some(capture) = &self.capture {
            match &event {
                PtyEvent::Hook(hook) => capture.push(
                    "hook",
                    IoDirection::Read,
                    String::from_utf8(hooks::encode(hook)).expect("hook JSON is UTF-8"),
                ),
                PtyEvent::Transcript { path, row } => capture.push(
                    "transcript",
                    IoDirection::Read,
                    serde_json::json!({"path": path, "row": row}).to_string(),
                ),
                _ => {}
            }
        }
        Ok(event)
    }

    fn tool_use_id(&self, tool_name: &str) -> Option<&str> {
        self.tool_use_ids.get(tool_name).map(String::as_str)
    }

    async fn send(&self, intent: Intent) -> Result<(), String> {
        self.control
            .send(intent)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    async fn wait_ask(&mut self, matches: impl Fn(&AskKind) -> bool) -> Result<AskFacts, String> {
        loop {
            match self.next().await? {
                PtyEvent::Ask(ask) if matches(&ask.kind) => return Ok(ask),
                PtyEvent::Hook(hooks::Payload::Stop(_)) => {
                    return Err("Claude stopped the turn before producing the expected ask".into());
                }
                PtyEvent::Exited(status) => {
                    return Err(format!(
                        "Claude exited before producing the expected ask: {status:?}"
                    ));
                }
                _ => {}
            }
        }
    }

    async fn wait_transcript(
        &mut self,
        matches: impl Fn(&serde_json::Value) -> bool,
    ) -> Result<serde_json::Value, String> {
        loop {
            if let PtyEvent::Transcript { row, .. } = self.next().await? {
                let row = serde_json::to_value(row).expect("a row serializes");
                if matches(&row) {
                    return Ok(row);
                }
            }
        }
    }

    /// Wait until Claude's terminal output contains every marker. Some
    /// states, such as the retries of a failing request, reach only the
    /// screen: neither the transcript nor a hook records them.
    async fn wait_screen(&mut self, markers: &[&str]) -> Result<(), String> {
        loop {
            {
                let screen = self.screen.lock().expect("screen mutex poisoned");
                if markers.iter().all(|marker| screen.contains(marker)) {
                    return Ok(());
                }
            }
            if self.capture.is_some() {
                tokio::time::sleep(Duration::from_millis(200)).await;
            } else {
                self.screen_changes.changed().await.map_err(|_| {
                    format!("recorded terminal output ended before showing {markers:?}")
                })?;
            }
        }
    }

    async fn wait_hook(
        &mut self,
        matches: impl Fn(&hooks::Payload) -> bool,
    ) -> Result<hooks::Payload, String> {
        loop {
            if let PtyEvent::Hook(hook) = self.next().await?
                && matches(&hook)
            {
                return Ok(hook);
            }
        }
    }

    async fn wait_relink(&mut self, expected: RelinkReason) -> Result<(), String> {
        loop {
            if let PtyEvent::Relink { reason, .. } = self.next().await?
                && reason == expected
            {
                return Ok(());
            }
        }
    }

    async fn drain_quiet(&mut self) {
        while let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(100), self.events.recv()).await
        {
            self.pending.push_back(event);
            while let Some(event) = self.pending.pop_front() {
                if let Some(capture) = &self.capture {
                    match &event {
                        PtyEvent::Hook(hook) => capture.push(
                            "hook",
                            IoDirection::Read,
                            String::from_utf8(hooks::encode(hook)).expect("hook JSON is UTF-8"),
                        ),
                        PtyEvent::Transcript { path, row } => capture.push(
                            "transcript",
                            IoDirection::Read,
                            serde_json::json!({"path": path, "row": row}).to_string(),
                        ),
                        _ => {}
                    }
                }
            }
        }
    }
}

/// The assistant row Claude writes in place of an answer once retries are
/// spent.
fn synthetic_error_reply(row: &serde_json::Value) -> bool {
    row.get("type").and_then(serde_json::Value::as_str) == Some("assistant")
        && (row
            .get("isApiErrorMessage")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            || row
                .pointer("/message/model")
                .and_then(serde_json::Value::as_str)
                == Some("<synthetic>"))
}

fn assistant_contains(row: &serde_json::Value, marker: &str) -> bool {
    row.get("type").and_then(serde_json::Value::as_str) == Some("assistant")
        && row.to_string().contains(marker)
}

fn tool_result_contains(row: &serde_json::Value, marker: &str) -> bool {
    row.get("type").and_then(serde_json::Value::as_str) == Some("user")
        && row.to_string().contains(marker)
}

fn successful_tool_result(row: &serde_json::Value, tool_use_id: &str) -> bool {
    row.get("type").and_then(serde_json::Value::as_str) == Some("user")
        && row
            .pointer("/message/content")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    block.get("type").and_then(serde_json::Value::as_str) == Some("tool_result")
                        && block.get("tool_use_id").and_then(serde_json::Value::as_str)
                            == Some(tool_use_id)
                        && block.get("is_error").and_then(serde_json::Value::as_bool) != Some(true)
                })
            })
}

/// The id of the first call to `tool_name` an assistant row makes.
fn tool_use_named(row: &serde_json::Value, tool_name: &str) -> Option<String> {
    row.pointer("/message/content")?
        .as_array()?
        .iter()
        .find(|block| {
            block.get("type").and_then(serde_json::Value::as_str) == Some("tool_use")
                && block.get("name").and_then(serde_json::Value::as_str) == Some(tool_name)
        })?
        .get("id")?
        .as_str()
        .map(str::to_owned)
}

fn structured_patch_changes(
    row: &serde_json::Value,
    tool_use_id: &str,
    old_line: &str,
    new_line: &str,
) -> bool {
    successful_tool_result(row, tool_use_id)
        && row
            .pointer("/toolUseResult/structuredPatch")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|patches| {
                patches.iter().any(|patch| {
                    patch
                        .get("lines")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|lines| {
                            lines.iter().any(|line| line.as_str() == Some(old_line))
                                && lines.iter().any(|line| line.as_str() == Some(new_line))
                        })
                })
            })
}

fn bash_stdout_equals(row: &serde_json::Value, tool_use_id: &str, expected: &str) -> bool {
    successful_tool_result(row, tool_use_id)
        && row
            .pointer("/toolUseResult/stdout")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|stdout| stdout.trim_end() == expected)
}

fn user_prompt_equals(row: &serde_json::Value, expected: &str) -> bool {
    row.get("type").and_then(serde_json::Value::as_str) == Some("user")
        && row
            .pointer("/message/content")
            .and_then(serde_json::Value::as_str)
            == Some(expected)
}

fn question_result_has_answers(row: &serde_json::Value, expected: &[(&str, &str)]) -> bool {
    row.get("type").and_then(serde_json::Value::as_str) == Some("user")
        && row
            .pointer("/toolUseResult/answers")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|answers| {
                expected.iter().all(|(question, answer)| {
                    answers.get(*question).and_then(serde_json::Value::as_str) == Some(*answer)
                })
            })
}

/// An API that cannot be reached: Claude shows each retry on screen only
/// (the transcript and hooks record none of them), then writes an error in
/// place of an answer.
async fn api_error(session: &mut PtySpecSession) -> Result<(), String> {
    session
        .send(Intent::Prompt {
            text: "Reply exactly PTY_SPEC_API_ERROR and nothing else.".to_owned(),
        })
        .await?;
    session.wait_screen(&["Retrying", "attempt 1/2"]).await?;
    session.wait_transcript(synthetic_error_reply).await?;
    Ok(())
}

/// A credential the API rejects: Claude asks whether to use the key from
/// the environment, then answers the prompt with the authentication error.
async fn sign_in_problem(session: &mut PtySpecSession) -> Result<(), String> {
    session
        .send(Intent::Prompt {
            text: "Reply exactly PTY_SPEC_SIGN_IN and nothing else.".to_owned(),
        })
        .await?;
    session
        .wait_transcript(|row| synthetic_error_reply(row) && row.to_string().contains("401"))
        .await?;
    Ok(())
}

/// Claude's record of a prompt that joined a running turn: an attachment
/// row carrying the prompt, not a user row of its own.
fn queued_command_row(row: &serde_json::Value, marker: &str) -> bool {
    row.get("type").and_then(serde_json::Value::as_str) == Some("attachment")
        && row["attachment"]["type"] == "queued_command"
        && row["attachment"]["prompt"]
            .as_str()
            .is_some_and(|prompt| prompt.contains(marker))
}

/// A prompt typed while a tool runs goes into Claude's own queue (an
/// enqueue row in the transcript) and joins the running turn at the next
/// tool boundary, recorded as a queued_command attachment.
async fn steer_queued(session: &mut PtySpecSession) -> Result<(), String> {
    session
        .send(Intent::Prompt {
            text: "Use Bash to run exactly: python3 -c 'import time; time.sleep(15)'; echo S1. Then use Bash to run exactly: python3 -c 'import time; time.sleep(5)'; echo S2. Then reply exactly PTY_STEER_QUEUED plus any extra words the user asked for.".to_owned(),
        })
        .await?;
    session
        .wait_hook(|hook| matches!(hook, hooks::Payload::PreToolUse(started) if started.tool_name == "Bash"))
        .await?;
    session
        .send(Intent::Prompt {
            text: "Also append the word BANANA.".to_owned(),
        })
        .await?;
    session
        .wait_transcript(|row| {
            row["type"] == "queue-operation"
                && row["operation"] == "enqueue"
                && row["content"] == "Also append the word BANANA."
        })
        .await?;
    session
        .wait_transcript(|row| queued_command_row(row, "BANANA"))
        .await?;
    session
        .wait_transcript(|row| assistant_contains(row, "BANANA"))
        .await?;
    Ok(())
}

/// ctrl+x ctrl+s ("send queued messages now") while a tool runs moves the
/// tool to the background instead of waiting for it, so the queued prompt
/// reaches the running turn at once. The moved tool's result says so; its
/// PostToolUse hook does not always fire.
async fn steer_send_now(session: &mut PtySpecSession) -> Result<(), String> {
    session
        .send(Intent::Prompt {
            text: "I am testing how follow-up messages reach you while a command runs. Use Bash, not in the background, to run exactly: python3 -c 'import time; time.sleep(30)'; echo S1. Then reply with the word PTY_STEER_NOW, and also follow any follow-up message I send while the command runs.".to_owned(),
        })
        .await?;
    let started = session
        .wait_hook(|hook| matches!(hook, hooks::Payload::PreToolUse(started) if started.tool_name == "Bash"))
        .await?;
    // A command the model backgrounds itself leaves nothing for the chord
    // to move; the capture is then about something else.
    if let hooks::Payload::PreToolUse(started) = &started
        && started.tool_input["run_in_background"] == true
    {
        return Err("the model ran the command in the background itself".to_owned());
    }
    session
        .send(Intent::Prompt {
            text: "Also append the word DURIAN.".to_owned(),
        })
        .await?;
    session
        .wait_transcript(|row| {
            row["type"] == "queue-operation"
                && row["operation"] == "enqueue"
                && row["content"] == "Also append the word DURIAN."
        })
        .await?;
    session
        .control
        .send_program(vec![
            crate::driver::pty::PtyInput::Bytes(vec![0x18]),
            crate::driver::pty::PtyInput::Delay(200),
            crate::driver::pty::PtyInput::Bytes(vec![0x13]),
        ])
        .await
        .map_err(|error| error.to_string())?;
    session
        .wait_transcript(|row| row["toolUseResult"]["backgroundedToDeliverMessage"] == true)
        .await?;
    // The prompt then reaches the running turn either folded (a
    // queued_command attachment) or dequeued as a user row; which one
    // depends on timing Claude does not expose.
    session
        .wait_transcript(|row| {
            queued_command_row(row, "DURIAN")
                || (row["type"] == "user"
                    && row["message"]["content"] == "Also append the word DURIAN.")
        })
        .await?;
    Ok(())
}

async fn prompt(session: &mut PtySpecSession) -> Result<(), String> {
    const MARKER: &str = "PTY_SPEC_PROMPT_OK";
    session
        .send(Intent::Prompt {
            text: format!("Reply exactly {MARKER} and nothing else."),
        })
        .await?;
    session
        .wait_transcript(|row| assistant_contains(row, MARKER))
        .await?;
    Ok(())
}

async fn prompt_multiline(session: &mut PtySpecSession) -> Result<(), String> {
    const TEXT: &str = "Reply exactly PTY_SPEC_MULTILINE_OK and nothing else.\nThis second line is part of one prompt.";
    session
        .send(Intent::Prompt {
            text: TEXT.to_owned(),
        })
        .await?;
    session
        .wait_transcript(|row| user_prompt_equals(row, TEXT))
        .await?;
    session
        .wait_transcript(|row| assistant_contains(row, "PTY_SPEC_MULTILINE_OK"))
        .await?;
    Ok(())
}

/// A prompt far past the length Claude collapses a paste at: it must reach
/// the model as the person's own words, not as pasted content the model
/// declines to take instructions from.
async fn prompt_long(session: &mut PtySpecSession) -> Result<(), String> {
    const MARKER: &str = "PTY_SPEC_LONG_OK";
    let padding = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu. ";
    let text = format!(
        "Reply with exactly {MARKER} and nothing else. Padding follows.\n{}\nThat is the end of the padding.",
        padding.repeat(16).trim_end()
    );
    session.send(Intent::Prompt { text: text.clone() }).await?;
    let row = session
        .wait_transcript(|row| {
            row.get("type").and_then(serde_json::Value::as_str) == Some("user")
                && row
                    .pointer("/message/content")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|content| content.contains("Padding follows."))
        })
        .await?;
    if !user_prompt_equals(&row, &text) {
        return Err(format!(
            "the prompt did not arrive as typed: {}",
            row.pointer("/message/content")
                .unwrap_or(&serde_json::Value::Null)
        ));
    }
    session
        .wait_transcript(|row| assistant_contains(row, MARKER))
        .await?;
    Ok(())
}

async fn tools(session: &mut PtySpecSession) -> Result<(), String> {
    session
        .send(Intent::Prompt {
            text: "First use Read to inspect config.txt. Then use Edit to change its only line from VALUE=1 to VALUE=2. Then use Bash to run exactly: cat config.txt. Do all three in this turn, in that order, then stop."
                .to_owned(),
        })
        .await?;

    // Hooks and transcript rows arrive on independent streams. A result may
    // be observed before its hook, so retain both and correlate by tool ID.
    let expected_tools = ["Read", "Edit", "Bash"];
    let mut tool_ids = Vec::new();
    let mut results = Vec::new();
    loop {
        match session.next().await? {
            PtyEvent::Hook(hooks::Payload::PreToolUse(started))
                if expected_tools.contains(&started.tool_name.as_str()) =>
            {
                let tool_name = &started.tool_name;
                if expected_tools.get(tool_ids.len()).copied() != Some(tool_name.as_str()) {
                    return Err(format!(
                        "expected Read, Edit, Bash in order; observed {tool_name}"
                    ));
                }
                tool_ids.push(started.tool_use_id);
            }
            PtyEvent::Transcript { row, .. } => {
                results.push(serde_json::to_value(row).expect("a row serializes"))
            }
            PtyEvent::Exited(status) => {
                return Err(format!(
                    "Claude exited before all three tool results: {status:?}"
                ));
            }
            _ => {}
        }
        if tool_ids.len() == 3
            && results
                .iter()
                .any(|row| successful_tool_result(row, &tool_ids[0]))
            && results
                .iter()
                .any(|row| structured_patch_changes(row, &tool_ids[1], "-VALUE=1", "+VALUE=2"))
            && results
                .iter()
                .any(|row| bash_stdout_equals(row, &tool_ids[2], "VALUE=2"))
        {
            return Ok(());
        }
    }
}

async fn permission_ask(session: &mut PtySpecSession, command: &str) -> Result<AskFacts, String> {
    session
        .send(Intent::Prompt {
            text: format!("Use the Bash tool to run exactly: {command}. Then stop."),
        })
        .await?;
    session
        .wait_ask(|ask| matches!(ask, AskKind::Permission { tool_name, is_plan: false, .. } if tool_name == "Bash"))
        .await
}

async fn permission_allow_once(session: &mut PtySpecSession) -> Result<(), String> {
    let ask = permission_ask(
        session,
        "printf allow-once > allow-once.txt; printf allow-once",
    )
    .await?;
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Permission(PermissionAnswer::AllowOnce),
        })
        .await?;
    session
        .wait_transcript(|row| tool_result_contains(row, "allow-once"))
        .await?;
    Ok(())
}

async fn permission_allow_scoped(session: &mut PtySpecSession) -> Result<(), String> {
    let ask = permission_ask(
        session,
        "printf allow-scoped > allow-scoped.txt; printf allow-scoped",
    )
    .await?;
    let suggestions = match ask.kind {
        AskKind::Permission { suggestions, .. } => suggestions,
        _ => 0,
    };
    if suggestions == 0 {
        return Err("scoped permission ask carried no suggestion".to_owned());
    }
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Permission(PermissionAnswer::AllowScoped { suggestion: 0 }),
        })
        .await?;
    session
        .wait_transcript(|row| tool_result_contains(row, "allow-scoped"))
        .await?;
    Ok(())
}

async fn permission_deny_feedback(session: &mut PtySpecSession) -> Result<(), String> {
    const FEEDBACK: &str = "Use a read-only command instead";
    let ask = permission_ask(session, "printf denied > denied.txt").await?;
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Permission(PermissionAnswer::Deny {
                feedback: Some(FEEDBACK.to_owned()),
            }),
        })
        .await?;
    session
        .wait_transcript(|row| tool_result_contains(row, FEEDBACK))
        .await?;
    Ok(())
}

async fn plan_ask(session: &mut PtySpecSession) -> Result<AskFacts, String> {
    session
        .send(Intent::Prompt {
            text: "Plan changing README.md's only line from CURRENT to UPDATED, then call ExitPlanMode. Do not ask questions.".to_owned(),
        })
        .await?;
    session
        .wait_ask(|ask| matches!(ask, AskKind::Permission { is_plan: true, .. }))
        .await
}

async fn plan_approve(session: &mut PtySpecSession) -> Result<(), String> {
    let ask = plan_ask(session).await?;
    let tool_use_id = session
        .tool_use_id("ExitPlanMode")
        .ok_or_else(|| "ExitPlanMode PreToolUse hook omitted its tool-use id".to_owned())?
        .to_owned();
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Plan(PlanAnswer::ApproveManual),
        })
        .await?;
    session
        .wait_transcript(|row| successful_tool_result(row, &tool_use_id))
        .await?;
    Ok(())
}

async fn plan_auto(session: &mut PtySpecSession) -> Result<(), String> {
    let ask = plan_ask(session).await?;
    let exit_plan_tool_use_id = session
        .tool_use_id("ExitPlanMode")
        .ok_or_else(|| "ExitPlanMode PreToolUse hook omitted its tool-use id".to_owned())?
        .to_owned();
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Plan(PlanAnswer::ApproveAuto),
        })
        .await?;
    session
        .wait_transcript(|row| successful_tool_result(row, &exit_plan_tool_use_id))
        .await?;

    let mut edit_tool_use_id = None;
    loop {
        match session.next().await? {
            PtyEvent::Ask(ask) => {
                return Err(format!(
                    "ApproveAuto produced a further ask before the planned edit landed: {:?}",
                    ask.kind
                ));
            }
            PtyEvent::Hook(hooks::Payload::PreToolUse(started)) if started.tool_name == "Edit" => {
                edit_tool_use_id = session.tool_use_id("Edit").map(str::to_owned);
                if edit_tool_use_id.is_none() {
                    return Err("Edit PreToolUse hook omitted its tool-use id".to_owned());
                }
            }
            // Hooks and rows travel apart: the Edit's own row can come
            // before its PreToolUse hook, and names the call as well.
            PtyEvent::Transcript { row, .. }
                if edit_tool_use_id.is_none()
                    && tool_use_named(
                        &serde_json::to_value(&row).expect("a row serializes"),
                        "Edit",
                    )
                    .is_some() =>
            {
                edit_tool_use_id = tool_use_named(
                    &serde_json::to_value(&row).expect("a row serializes"),
                    "Edit",
                );
            }
            PtyEvent::Transcript { row, .. }
                if edit_tool_use_id.as_ref().is_some_and(|tool_use_id| {
                    structured_patch_changes(
                        &serde_json::to_value(&row).expect("a row serializes"),
                        tool_use_id,
                        "-CURRENT",
                        "+UPDATED",
                    )
                }) =>
            {
                return Ok(());
            }
            PtyEvent::Hook(hooks::Payload::Stop(_)) => {
                return Err("Claude stopped before the automatically approved edit landed".into());
            }
            PtyEvent::Exited(status) => {
                return Err(format!(
                    "Claude exited before the automatically approved edit landed: {status:?}"
                ));
            }
            _ => {}
        }
    }
}

async fn plan_request_changes(session: &mut PtySpecSession) -> Result<(), String> {
    const FEEDBACK: &str = "Also explain why the change is needed";
    let ask = plan_ask(session).await?;
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Plan(PlanAnswer::RequestChanges {
                feedback: FEEDBACK.to_owned(),
            }),
        })
        .await?;
    session
        .wait_transcript(|row| tool_result_contains(row, FEEDBACK))
        .await?;
    Ok(())
}

async fn question_ask(session: &mut PtySpecSession, prompt: &str) -> Result<AskFacts, String> {
    session
        .send(Intent::Prompt {
            text: prompt.to_owned(),
        })
        .await?;
    session
        .wait_ask(|ask| matches!(ask, AskKind::Question { .. }))
        .await
}

async fn question_single(session: &mut PtySpecSession) -> Result<(), String> {
    let ask = question_ask(session, "Use AskUserQuestion to ask one single-select question with header Color and options Red and Blue. Then repeat my answer.").await?;
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Question(QuestionResponse {
                answers: vec![QuestionAnswer {
                    selected: vec![0],
                    other: None,
                }],
            }),
        })
        .await?;
    session
        .wait_transcript(|row| tool_result_contains(row, "Red"))
        .await?;
    Ok(())
}

async fn question_multi_other(session: &mut PtySpecSession) -> Result<(), String> {
    const OTHER: &str = "Torque wrench";
    let ask = question_ask(session, "Use AskUserQuestion to ask one multi-select question with header Tools and options Hammer, Saw, and Drill. Then repeat my answer.").await?;
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Question(QuestionResponse {
                answers: vec![QuestionAnswer {
                    selected: vec![0, 1],
                    other: Some(OTHER.to_owned()),
                }],
            }),
        })
        .await?;
    session
        .wait_transcript(|row| tool_result_contains(row, OTHER))
        .await?;
    Ok(())
}

async fn question_mixed(session: &mut PtySpecSession) -> Result<(), String> {
    let ask = question_ask(session, "Use one AskUserQuestion call containing two questions: first a single-select Color question with Red and Blue; second a multi-select Tools question with Hammer, Saw, and Drill. Then repeat both answers.").await?;
    let count = match &ask.kind {
        AskKind::Question { questions } => questions.len(),
        _ => 0,
    };
    if count != 2 {
        return Err(format!("mixed question ask carried {count} questions"));
    }
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Question(QuestionResponse {
                answers: vec![
                    QuestionAnswer {
                        selected: vec![1],
                        other: None,
                    },
                    QuestionAnswer {
                        selected: vec![0, 2],
                        other: None,
                    },
                ],
            }),
        })
        .await?;
    session
        .wait_transcript(|row| tool_result_contains(row, "Blue"))
        .await?;
    Ok(())
}

async fn question_tabs(session: &mut PtySpecSession) -> Result<(), String> {
    const COLOR_QUESTION: &str = "Which color do you prefer?";
    const SIZE_QUESTION: &str = "Which size fits best?";
    let ask = question_ask(
        session,
        "Use one AskUserQuestion call containing exactly two single-select questions. First: header Color, question 'Which color do you prefer?', options Red and Blue. Second: header Size, question 'Which size fits best?', options Small and Large. Then repeat both answers.",
    )
    .await?;
    let questions = match &ask.kind {
        AskKind::Question { questions } => questions,
        _ => unreachable!("question_ask returned a question"),
    };
    if questions.len() != 2
        || questions
            .iter()
            .any(|question| question.multi_select || question.options != 2)
    {
        return Err(format!(
            "two-question single-select ask had unexpected shape: {questions:?}"
        ));
    }
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Question(QuestionResponse {
                answers: vec![
                    QuestionAnswer {
                        selected: vec![0],
                        other: None,
                    },
                    QuestionAnswer {
                        selected: vec![1],
                        other: None,
                    },
                ],
            }),
        })
        .await?;
    session
        .wait_transcript(|row| {
            question_result_has_answers(row, &[(COLOR_QUESTION, "Red"), (SIZE_QUESTION, "Large")])
        })
        .await?;
    Ok(())
}

async fn question_other_single(session: &mut PtySpecSession) -> Result<(), String> {
    const QUESTION: &str = "Which color do you prefer?";
    const OTHER: &str = "a warm ochre";
    let ask = question_ask(
        session,
        "Use AskUserQuestion to ask exactly one single-select question with header Color, question 'Which color do you prefer?', and options Red and Blue. Then repeat my answer.",
    )
    .await?;
    let questions = match &ask.kind {
        AskKind::Question { questions } => questions,
        _ => unreachable!("question_ask returned a question"),
    };
    if questions.len() != 1 || questions[0].multi_select || questions[0].options != 2 {
        return Err(format!(
            "single-select Other ask had unexpected shape: {questions:?}"
        ));
    }
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Question(QuestionResponse {
                answers: vec![QuestionAnswer {
                    selected: Vec::new(),
                    other: Some(OTHER.to_owned()),
                }],
            }),
        })
        .await?;
    session
        .wait_transcript(|row| question_result_has_answers(row, &[(QUESTION, OTHER)]))
        .await?;
    Ok(())
}

async fn interrupt(session: &mut PtySpecSession) -> Result<(), String> {
    session
        .send(Intent::Prompt {
            text: "Use Bash to run exactly: python3 -c 'import select; select.select([], [], [], 30)'; printf SHOULD_NOT_FINISH. Do not do anything else.".to_owned(),
        })
        .await?;
    session
        .wait_hook(|hook| matches!(hook, hooks::Payload::PreToolUse(started) if started.tool_name == "Bash"))
        .await?;
    session.send(Intent::Interrupt).await?;
    session
        .wait_transcript(|row| row.to_string().to_lowercase().contains("interrupt"))
        .await?;
    Ok(())
}

async fn mode_cycle(session: &mut PtySpecSession) -> Result<(), String> {
    session.send(Intent::CyclePermissionMode).await?;
    session
        .send(Intent::Prompt {
            text: "Reply exactly MODE_CYCLED and nothing else.".to_owned(),
        })
        .await?;
    let hook = session
        .wait_hook(|hook| matches!(hook, hooks::Payload::Stop(_)))
        .await?;
    let mode = hook
        .common()
        .and_then(|common| common.permission_mode.as_ref());
    if mode == Some(&PermissionMode::Default) {
        return Err("permission mode remained default after cycle".to_owned());
    }
    Ok(())
}

async fn completed_turn(session: &mut PtySpecSession) -> Result<(), String> {
    session
        .send(Intent::Prompt {
            text: "Reply exactly READY_FOR_RELINK and nothing else.".to_owned(),
        })
        .await?;
    session
        .wait_hook(|hook| matches!(hook, hooks::Payload::Stop(_)))
        .await?;
    Ok(())
}

async fn compact_relink(session: &mut PtySpecSession) -> Result<(), String> {
    for _ in 0..4 {
        completed_turn(session).await?;
    }
    session
        .send(Intent::Prompt {
            text: "/compact".to_owned(),
        })
        .await?;
    session.wait_relink(RelinkReason::Compact).await?;
    // The compaction's own rows land after the hook that relinks: the
    // recording holds them through the command's output, its last.
    session
        .wait_transcript(|row| {
            row.pointer("/message/content")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|text| {
                    // 2.1.312 wraps the words in terminal escapes.
                    text.starts_with("<local-command-stdout>") && text.contains("Compacted")
                })
        })
        .await
        .map(|_| ())
}

async fn clear_relink(session: &mut PtySpecSession) -> Result<(), String> {
    completed_turn(session).await?;
    session
        .send(Intent::Prompt {
            text: "/clear".to_owned(),
        })
        .await?;
    session.wait_relink(RelinkReason::Clear).await
}

fn tool_names_in(row: &serde_json::Value) -> Vec<String> {
    if row.get("type").and_then(serde_json::Value::as_str) != Some("assistant") {
        return Vec::new();
    }
    row.pointer("/message/content")
        .and_then(serde_json::Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| {
                    block.get("type").and_then(serde_json::Value::as_str) == Some("tool_use")
                })
                .filter_map(|block| block.get("name").and_then(serde_json::Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Collect transcript rows until the turn has stopped and `done` holds for
/// what was seen. The Stop hook and the last transcript rows travel on
/// separate streams, so either may arrive first.
async fn collect_turn(
    session: &mut PtySpecSession,
    done: impl Fn(&[serde_json::Value]) -> bool,
) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = Vec::new();
    let mut stopped = false;
    loop {
        match session.next().await? {
            PtyEvent::Transcript { row, .. } => {
                rows.push(serde_json::to_value(row).expect("a row serializes"))
            }
            PtyEvent::Hook(hooks::Payload::Stop(_)) => stopped = true,
            PtyEvent::Exited(status) => {
                return Err(format!("Claude exited mid-turn: {status:?}"));
            }
            _ => {}
        }
        if stopped && done(&rows) {
            return Ok(rows);
        }
    }
}

fn used_tools(rows: &[serde_json::Value], expected: &[&str]) -> bool {
    let seen = rows.iter().flat_map(tool_names_in).collect::<Vec<_>>();
    expected
        .iter()
        .all(|name| seen.iter().any(|tool| tool == name))
}

async fn question_every_shape(session: &mut PtySpecSession) -> Result<(), String> {
    const COLOR: &str = "Which color do you prefer?";
    const TOOLS: &str = "Which tools do you need?";
    const LAYOUT: &str = "Which layout should the page use?";
    const SNACK: &str = "Which snack do you want?";
    const OTHER: &str = "Dried mango";
    let ask = question_ask(
        session,
        "Use exactly one AskUserQuestion call containing exactly four questions, in this order. \
         1: header Color, question 'Which color do you prefer?', single-select, options Red and Blue. \
         2: header Tools, question 'Which tools do you need?', multiSelect true, options Hammer, Saw and Drill. \
         3: header Layout, question 'Which layout should the page use?', single-select, four options Sidebar, Topbar, Grid and Stack; give every one of these four options a preview holding a small ASCII sketch of that layout. \
         4: header Snack, question 'Which snack do you want?', single-select, options Apple, Pretzel and Popcorn. \
         Add nothing else. After I answer, repeat all four answers.",
    )
    .await?;
    let shape = match &ask.kind {
        AskKind::Question { questions } => questions
            .iter()
            .map(|question| (question.options, question.multi_select))
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    if shape != [(2, false), (3, true), (4, false), (3, false)] {
        return Err(format!("four-question ask had unexpected shape: {shape:?}"));
    }
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Question(QuestionResponse {
                answers: vec![
                    QuestionAnswer {
                        selected: vec![1],
                        other: None,
                    },
                    QuestionAnswer {
                        selected: vec![0, 2],
                        other: None,
                    },
                    QuestionAnswer {
                        selected: vec![2],
                        other: None,
                    },
                    QuestionAnswer {
                        selected: Vec::new(),
                        other: Some(OTHER.to_owned()),
                    },
                ],
            }),
        })
        .await?;
    let row = session
        .wait_transcript(|row| {
            question_result_has_answers(row, &[(COLOR, "Blue"), (LAYOUT, "Grid"), (SNACK, OTHER)])
        })
        .await?;
    let tools = row
        .pointer("/toolUseResult/answers")
        .and_then(|answers| answers.get(TOOLS))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if !(tools.contains("Hammer") && tools.contains("Drill") && !tools.contains("Saw")) {
        return Err(format!("multi-select answer was {tools:?}"));
    }
    let previews = row
        .pointer("/toolUseResult/questions/2/options")
        .and_then(serde_json::Value::as_array)
        .map_or(0, |options| {
            options
                .iter()
                .filter(|option| {
                    option
                        .get("preview")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|preview| !preview.is_empty())
                })
                .count()
        });
    if previews != 4 {
        return Err(format!(
            "layout question carried {previews} previews, not 4"
        ));
    }
    Ok(())
}

async fn question_cancelled(session: &mut PtySpecSession) -> Result<(), String> {
    question_ask(
        session,
        "Use AskUserQuestion to ask one single-select question with header Color and options Red and Blue. If I decline to answer, reply exactly QUESTION_DECLINED.",
    )
    .await?;
    session.send(Intent::Interrupt).await?;
    session
        .wait_transcript(|row| {
            row.get("type").and_then(serde_json::Value::as_str) == Some("user")
                && row.to_string().contains("tool_result")
        })
        .await?;
    Ok(())
}

/// A form of two questions, the first skipped through the keymap's skip
/// step and the second answered; then a second form replied to instead: the
/// form cancelled with the interrupt key and the words typed as the next
/// prompt.
async fn question_skip_reply(session: &mut PtySpecSession) -> Result<(), String> {
    const COLOUR: &str = "Which colour?";
    const SIZE: &str = "Which size?";
    const REPLY: &str = "Skip the shape; we are done here.";
    let ask = question_ask(
        session,
        "Use the AskUserQuestion tool once, asking me two questions in that one call: \
         'Which colour?' with the options Red and Blue, and 'Which size?' with the options \
         Small and Large. After I answer, use it once more to ask me one question, \
         'Which shape?' with the options Circle and Square. Then reply with the single word \
         done. Do nothing else.",
    )
    .await?;
    let shape = match &ask.kind {
        AskKind::Question { questions } => questions
            .iter()
            .map(|question| (question.options, question.multi_select))
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    if shape != [(2, false), (2, false)] {
        return Err(format!("two-question ask had unexpected shape: {shape:?}"));
    }
    session
        .send(Intent::Answer {
            ask_id: ask.id,
            answer: AskAnswer::Question(QuestionResponse {
                answers: vec![
                    QuestionAnswer {
                        selected: Vec::new(),
                        other: None,
                    },
                    QuestionAnswer {
                        selected: vec![0],
                        other: None,
                    },
                ],
            }),
        })
        .await?;
    // The answer's row and the next form's hook travel separate channels, so
    // either may come first. Only the hook tells the next form apart: the
    // first form's own row, written after its answer, asks it again, and a
    // hook names an ask by its prompt, which both forms share.
    let mut answered = None;
    let mut asked_again = false;
    while answered.is_none() || !asked_again {
        match session.next().await? {
            PtyEvent::Transcript { row, .. } => {
                let row = serde_json::to_value(row).expect("a row serializes");
                if question_result_has_answers(&row, &[(SIZE, "Small")]) {
                    answered = Some(row);
                }
            }
            PtyEvent::Hook(hooks::Payload::PermissionRequest(request))
                if request.tool_name == "AskUserQuestion" =>
            {
                asked_again = true;
            }
            PtyEvent::Hook(hooks::Payload::Stop(_)) => {
                return Err("Claude stopped the turn before asking again".into());
            }
            PtyEvent::Exited(status) => {
                return Err(format!("Claude exited before asking again: {status:?}"));
            }
            _ => {}
        }
    }
    let row = answered.expect("the loop ends with the answer's row");
    if row
        .pointer("/toolUseResult/answers")
        .and_then(|answers| answers.get(COLOUR))
        .is_some()
    {
        return Err(format!("the skipped question was answered: {row}"));
    }
    session.send(Intent::Interrupt).await?;
    session
        .wait_transcript(|row| {
            row.get("type").and_then(serde_json::Value::as_str) == Some("user")
                && row.to_string().contains("tool_result")
        })
        .await?;
    session
        .send(Intent::Prompt {
            text: REPLY.to_owned(),
        })
        .await?;
    session
        .wait_transcript(|row| user_prompt_equals(row, REPLY))
        .await?;
    Ok(())
}

async fn subagent(session: &mut PtySpecSession) -> Result<(), String> {
    session
        .send(Intent::Prompt {
            text: "Use the Agent tool to start one general-purpose subagent with this task: read config.txt with the Read tool and report its single line. Wait for it, then tell me what it reported."
                .to_owned(),
        })
        .await?;
    collect_turn(session, |rows| {
        used_tools(rows, &["Agent"]) && rows.iter().any(|row| assistant_contains(row, "VALUE=1"))
    })
    .await?;
    Ok(())
}

async fn background_shell(session: &mut PtySpecSession) -> Result<(), String> {
    session
        .send(Intent::Prompt {
            text: "Use Bash with run_in_background set to true to run exactly: for i in 1 2 3; do echo tick $i; sleep 2; done. Then wait about eight seconds by running Bash: sleep 8. Then read the background shell's output with the tool for reading background output, and report the last line it printed."
                .to_owned(),
        })
        .await?;
    collect_turn(session, |rows| {
        rows.iter()
            .any(|row| row.pointer("/toolUseResult/backgroundTaskId").is_some())
            && rows.iter().any(|row| assistant_contains(row, "tick 3"))
    })
    .await?;
    Ok(())
}

async fn task_list(session: &mut PtySpecSession) -> Result<(), String> {
    session
        .send(Intent::Prompt {
            text: "Track this work with your task tools: create three tasks named Draft, Review and Publish. Then mark Draft in progress, then mark Draft completed. Do nothing else, then stop."
                .to_owned(),
        })
        .await?;
    collect_turn(session, |rows| {
        used_tools(rows, &["TaskCreate", "TaskUpdate"])
    })
    .await?;
    Ok(())
}

async fn file_changes_and_failure(session: &mut PtySpecSession) -> Result<(), String> {
    session
        .send(Intent::Prompt {
            text: "Do these in order, one tool call each. 1: use Write to overwrite README.md with the single line REPLACED. 2: use Bash to run exactly: mv config.txt moved.txt. 3: use Bash to run exactly: rm moved.txt. 4: use Bash to run exactly: ls missing-file.txt. The last one fails; that is expected. Then stop."
                .to_owned(),
        })
        .await?;
    collect_turn(session, |rows| {
        used_tools(rows, &["Write", "Bash"])
            && rows.iter().any(|row| {
                row.get("type").and_then(serde_json::Value::as_str) == Some("user")
                    && row.to_string().contains("\"is_error\":true")
                    && row.to_string().contains("missing-file.txt")
            })
    })
    .await?;
    Ok(())
}

async fn image(session: &mut PtySpecSession) -> Result<(), String> {
    session
        .send(Intent::Prompt {
            text: "Use the Read tool on square.png and tell me its main color in one word."
                .to_owned(),
        })
        .await?;
    collect_turn(session, |rows| {
        used_tools(rows, &["Read"])
            && rows
                .iter()
                .any(|row| row.to_string().contains("\"type\":\"image\""))
    })
    .await?;
    Ok(())
}

async fn thinking(session: &mut PtySpecSession) -> Result<(), String> {
    session
        .send(Intent::Prompt {
            text: "Think hard about this before answering: which is larger, 17 squared or 2 to the power 8? Answer with just the larger number."
                .to_owned(),
        })
        .await?;
    collect_turn(session, |rows| {
        rows.iter().any(|row| assistant_contains(row, "289"))
            && rows
                .iter()
                .any(|row| row.to_string().contains("\"type\":\"thinking\""))
    })
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_names_every_semantic_pty_claim_in_order() {
        assert_eq!(
            registry()
                .iter()
                .map(|entry| entry.name)
                .collect::<Vec<_>>(),
            vec![
                "prompt",
                "prompt_multiline",
                "prompt_long",
                "tools",
                "permission_allow_once",
                "permission_allow_scoped",
                "permission_deny_feedback",
                "plan_approve",
                "plan_auto",
                "plan_request_changes",
                "question_single",
                "question_multi_other",
                "question_mixed",
                "question_tabs",
                "question_other_single",
                "interrupt",
                "mode_cycle",
                "compact_relink",
                "clear_relink",
                "question_every_shape",
                "question_cancelled",
                "question_skip_reply",
                "subagent",
                "background_shell",
                "task_list",
                "file_changes_and_failure",
                "image",
                "thinking",
                "api_error",
                "sign_in_problem",
                "steer_queued",
                "steer_send_now",
            ]
        );
        assert_eq!(DEFINITIONS.len(), registry().len());
    }

    #[test]
    fn sonnet_fallback_is_limited_to_specs_haiku_does_not_drive_reliably() {
        assert_eq!(
            SONNET_FALLBACK_SPECS,
            &[
                "plan_approve",
                "plan_auto",
                "plan_request_changes",
                "question_mixed",
                "question_every_shape"
            ]
        );
        assert!(
            registry()
                .iter()
                .filter(|entry| !SONNET_FALLBACK_SPECS.contains(&entry.name))
                .all(|entry| !entry.name.starts_with("plan_"))
        );
    }
}
