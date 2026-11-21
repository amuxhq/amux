//! `fake-acp`: an agent speaking the Agent Client Protocol (version 1),
//! JSON-RPC over stdio, one message per line.
//!
//! Every frame it writes is composed from what Antigravity, OpenCode and
//! Grok wrote in the recordings under `crates/interpret/fixtures/acp/live/`
//! (see [`crate::shape`]). The host initializes, then opens a session with
//! `session/new`, `session/load` or `session/resume`, and prompts it; a
//! prompt plays the script's steps up to its next `turn_end` (or `stop`)
//! and is answered with the stop reason. Tool calls are `session/update`s
//! announcing the call and its progress; asks are the agent's requests
//! (`session/request_permission`, `elicitation/create`, and Grok's own
//! `_x.ai/ask_user_question`) that hold the turn until the host answers.
//!
//! Where the recorded agents differ, the script's [`AcpAgent`] chooses:
//! what the agent offers at `initialize` (reopening a session, images),
//! what a prompt sent mid-turn does, how many chunks still stream after a
//! cancel, whether a reopened session keeps its settings.
//!
//! Sessions are kept as files in the folder [`SESSIONS_ENV`] names (a
//! folder in the system's temporary directory otherwise), so a second
//! process can reopen one the first left: `session/load` streams its
//! history back before answering, `session/resume` answers at once.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::lines::Out;
use crate::script::{
    AcpAgent, Ask, Call, MidTurn, Offer, OfferedMode, OfferedModel, Outcome, SCHEMA_SLOT, Schema,
    Script, Step, Tool, ToolServerCall,
};
use crate::{DRIFT_EXIT, Mode};

/// The asks an ACP agent raises: a permission request, the agent's own
/// question as a form (Antigravity's, `agy-question`), and its questions
/// through Grok's own request (`grok-question`).
pub const RAISES: &[&str] = &["permission", "form", "question"];
/// The steps only an ACP agent plays.
pub const PLAYS: &[&str] = &["call", "tool_server", "context", "title", "stop"];
/// The version `agentInfo` reports when the script names none.
pub const VERSION: &str = "1.0.0";
/// Names the folder sessions are kept in, so a test that stops the fake
/// can start another that reopens its session.
pub const SESSIONS_ENV: &str = "AMUX_FAKE_ACP_SESSIONS";

/// JSON-RPC's and ACP's error codes the fake answers with.
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const AUTH_REQUIRED: i64 = -32000;
const RESOURCE_NOT_FOUND: i64 = -32002;

pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--version") {
        println!("fake-acp {}", crate::scripted_version(VERSION));
        return 0;
    }
    // Every other argument is the agent's own way of being asked to speak
    // ACP (`acp`, `agent stdio`), which this fake always does.
    let mode = match crate::mode_from_env() {
        Ok(mode) => mode,
        Err(error) => {
            eprintln!("fake-acp: {error}");
            return DRIFT_EXIT;
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    let code = runtime.block_on(async move {
        match mode {
            Mode::Playback(process) => {
                let input = BufReader::new(tokio::io::stdin());
                match crate::lines::play(&process, input, tokio::io::stdout()).await {
                    Ok(code) => code.unwrap_or(0),
                    Err(error) => {
                        eprintln!("fake-acp: {error}");
                        DRIFT_EXIT
                    }
                }
            }
            Mode::Script(script) => {
                if let Err(error) = script.check("an ACP agent", RAISES, PLAYS) {
                    eprintln!("fake-acp: {error}");
                    return DRIFT_EXIT;
                }
                Engine::new(script, sessions_dir()).run().await
            }
        }
    });
    // The stdin reader blocks in a thread the runtime would wait for on
    // drop, so an exit step would wait for the host to close stdin.
    runtime.shutdown_background();
    code
}

/// Where sessions are kept: [`SESSIONS_ENV`], else a folder in the
/// system's temporary directory, never the working directory, whose
/// changes a chat's review would show.
fn sessions_dir() -> PathBuf {
    std::env::var_os(SESSIONS_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("fake-acp-sessions"))
}

/// A session as it is kept between processes.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Session {
    id: String,
    cwd: String,
    model: String,
    effort: Option<String>,
    mode: Option<String>,
    title: Option<String>,
    /// What was said, in order: what `session/load` streams back.
    history: Vec<Said>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Said {
    user: bool,
    text: String,
}

/// A prompt the host sent.
#[derive(Clone, Debug)]
struct Prompt {
    id: Value,
    text: String,
}

/// The turn playing.
#[derive(Debug, Default)]
struct Turn {
    /// The prompts its end answers.
    requests: Vec<Value>,
    /// Prompts sent while it ran that play straight after it, answered
    /// with it ([`MidTurn::Together`]).
    joined: VecDeque<Prompt>,
    cancelled: bool,
}

struct Engine {
    agent: AcpAgent,
    models: Vec<OfferedModel>,
    commands: Vec<crate::script::OfferedCommand>,
    chunk_ms: u64,
    edit_files: bool,
    version: String,
    signed_out: bool,
    steps: VecDeque<Step>,
    out: Out<tokio::io::Stdout>,
    input: mpsc::UnboundedReceiver<String>,
    /// The host closed the agent's input.
    closed: bool,
    store: PathBuf,
    session: Option<Session>,
    turn: Option<Turn>,
    /// Prompts waiting for the running turn to end ([`MidTurn::Queue`]).
    queued: VecDeque<Prompt>,
    /// Answers to the agent's own requests, by id.
    answers: HashMap<String, Value>,
    next_request: u64,
    next_call: u64,
    /// Calls the person allowed always, by title: OpenCode keeps the rule
    /// for its process and asks no more (`opencode-allow_always`).
    always: HashSet<String>,
    exit: Option<i32>,
}

impl Engine {
    fn new(script: Script, store: PathBuf) -> Self {
        let (lines, input) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut stdin = BufReader::new(tokio::io::stdin()).lines();
            while let Ok(Some(line)) = stdin.next_line().await {
                if lines.send(line).is_err() {
                    break;
                }
            }
        });
        let model = script.model.clone().unwrap_or_else(|| "fake-model".into());
        Self {
            models: OfferedModel::offered(&script.models, &model),
            commands: script.commands,
            chunk_ms: script.chunk_ms,
            edit_files: script.edit_files,
            version: script.version.unwrap_or_else(|| VERSION.to_owned()),
            signed_out: script.signed_out,
            agent: script.acp,
            steps: script.steps.into(),
            out: Out::new(tokio::io::stdout()),
            input,
            closed: false,
            store,
            session: None,
            turn: None,
            queued: VecDeque::new(),
            answers: HashMap::new(),
            next_request: 0,
            next_call: 0,
            always: HashSet::new(),
            exit: None,
        }
    }

    async fn run(mut self) -> i32 {
        loop {
            if let Some(code) = self.exit {
                return code;
            }
            if self.turn.is_some() {
                self.play().await;
                continue;
            }
            if let Some(prompt) = self.queued.pop_front() {
                self.start(prompt);
                continue;
            }
            if self.closed {
                return 0;
            }
            match self.input.recv().await {
                Some(line) => self.handle(&line).await,
                None => self.closed = true,
            }
        }
    }

    /// Read what the host wrote while the turn waits on something; false
    /// once the input has closed.
    async fn pump(&mut self, within: Duration) -> bool {
        if self.closed {
            tokio::time::sleep(within).await;
            return false;
        }
        match tokio::time::timeout(within, self.input.recv()).await {
            Ok(Some(line)) => self.handle(&line).await,
            Ok(None) => self.closed = true,
            Err(_) => {}
        }
        !self.closed
    }

    fn cancelled(&self) -> bool {
        self.turn.as_ref().is_some_and(|turn| turn.cancelled)
    }

    /// Wait until `until`, or until `path` exists, reading the host
    /// meanwhile; a cancel or an exit cuts it short.
    async fn wait(&mut self, until: Option<Instant>, path: Option<&Path>) {
        loop {
            if self.cancelled() || self.exit.is_some() {
                return;
            }
            if path.is_some_and(Path::exists) {
                return;
            }
            let left = match until {
                Some(until) => match until.checked_duration_since(Instant::now()) {
                    Some(left) if !left.is_zero() => left,
                    _ => return,
                },
                None => Duration::from_millis(20),
            };
            // Polled rather than watched, so it works the same on every
            // platform and filesystem.
            let within = if path.is_some() {
                left.min(Duration::from_millis(20))
            } else {
                left
            };
            self.pump(within).await;
        }
    }

    /// The host's answer to the agent's request `id`; None when the turn
    /// was cancelled first or the host went away.
    async fn answer(&mut self, id: &str) -> Option<Value> {
        loop {
            if let Some(answer) = self.answers.remove(id) {
                return Some(answer);
            }
            if self.cancelled() || self.exit.is_some() {
                return None;
            }
            if !self.pump(Duration::from_millis(100)).await {
                // Nobody is left to answer: the process ends, as a stdio
                // agent's does at the end of its input.
                self.exit = Some(0);
                return None;
            }
        }
    }

    // ---- what the host sends ----

    async fn handle(&mut self, line: &str) {
        crate::script::log_input(line);
        let Ok(frame) = serde_json::from_str::<Value>(line) else {
            eprintln!("fake-acp: not JSON: {line}");
            return;
        };
        let method = frame["method"].as_str().map(str::to_owned);
        let id = frame.get("id").cloned();
        match (method, id) {
            (Some(method), Some(id)) => self.request(&method, id, &frame["params"]).await,
            (Some(method), None) => self.notification(&method, &frame["params"]),
            (None, Some(id)) => {
                self.answers.insert(key(&id), frame);
            }
            (None, None) => eprintln!("fake-acp: neither a request nor an answer: {line}"),
        }
    }

    fn notification(&mut self, method: &str, params: &Value) {
        if method != "session/cancel" {
            return;
        }
        let ours = self
            .session
            .as_ref()
            .is_some_and(|session| params["sessionId"] == session.id.as_str());
        if let (true, Some(turn)) = (ours, self.turn.as_mut()) {
            turn.cancelled = true;
        }
    }

    async fn request(&mut self, method: &str, id: Value, params: &Value) {
        if self.agent.unanswered.as_deref() == Some(method) {
            return;
        }
        match method {
            "initialize" => {
                let answer = self.initialized();
                self.result(&id, answer).await;
            }
            "session/new" => self.new_session(id, params).await,
            "session/load" | "session/resume" => self.reopen(method, id, params).await,
            "session/prompt" => self.prompt(id, params).await,
            "session/set_config_option" => self.set_option(id, params).await,
            "session/set_mode" => self.set_mode(id, params).await,
            "session/close" => {
                self.result(&id, json!({})).await;
            }
            _ => {
                self.error(&id, METHOD_NOT_FOUND, "Method not found", method)
                    .await;
            }
        }
    }

    fn initialized(&self) -> Value {
        let mut sessions = json!({"close": {}});
        if self.agent.load {
            sessions["load"] = json!({});
        }
        if self.agent.resume {
            sessions["resume"] = json!({});
        }
        let mut answer = json!({
            "agentCapabilities": {
                "loadSession": self.agent.load,
                "promptCapabilities": {
                    "embeddedContext": self.agent.embedded_context,
                    "image": self.agent.image,
                },
                "sessionCapabilities": sessions,
            },
            "authMethods": [{
                "description": "Run `fake-acp login` in the terminal",
                "id": "fake-login",
                "name": "Log in to the fake agent",
            }],
            "protocolVersion": self.agent.protocol_version,
        });
        if !self.agent.anonymous {
            answer["agentInfo"] = json!({"name": self.agent.name, "version": self.version});
        }
        answer
    }

    async fn new_session(&mut self, id: Value, params: &Value) {
        if self.is_signed_out() {
            self.signed_out_error(&id).await;
            return;
        }
        let mut session = self.fresh(uuid::Uuid::new_v4().to_string());
        session.cwd = params["cwd"].as_str().unwrap_or_default().to_owned();
        let answer = json!({"sessionId": session.id, "configOptions": self.options(&session)});
        self.session = Some(session);
        self.save();
        self.result(&id, answer).await;
        self.offer_commands().await;
    }

    /// A session with the agent's defaults.
    fn fresh(&self, id: String) -> Session {
        let model = &self.models[0];
        Session {
            id,
            model: model.value.clone(),
            effort: default_effort(model),
            mode: self.modes().first().map(|mode| mode.value.clone()),
            ..Session::default()
        }
    }

    async fn reopen(&mut self, method: &str, id: Value, params: &Value) {
        let offered = match method {
            "session/load" => self.agent.load,
            _ => self.agent.resume,
        };
        if !offered {
            self.error(&id, METHOD_NOT_FOUND, "Method not found", method)
                .await;
            return;
        }
        if self.is_signed_out() {
            self.signed_out_error(&id).await;
            return;
        }
        let wanted = params["sessionId"].as_str().unwrap_or_default();
        let kept = (!self.agent.refuse_reopen)
            .then(|| self.load(wanted))
            .flatten();
        let Some(mut session) = kept else {
            let detail = format!("no session {wanted}");
            self.error(&id, RESOURCE_NOT_FOUND, "Resource not found", &detail)
                .await;
            return;
        };
        if self.agent.forgets_settings {
            let fresh = self.fresh(session.id.clone());
            session.model = fresh.model;
            session.effort = fresh.effort;
            session.mode = fresh.mode;
        }
        if let Some(cwd) = params["cwd"].as_str() {
            session.cwd = cwd.to_owned();
        }
        if method == "session/load" {
            for said in session.history.clone() {
                let kind = if said.user {
                    "user_message_chunk"
                } else {
                    "agent_message_chunk"
                };
                self.update_of(
                    &session.id,
                    json!({"sessionUpdate": kind, "content": {"type": "text", "text": said.text}}),
                )
                .await;
            }
        }
        let answer = json!({"configOptions": self.options(&session)});
        self.session = Some(session);
        self.save();
        self.result(&id, answer).await;
        self.offer_commands().await;
    }

    /// Signed out by the script, or until its sign-in file appears.
    fn is_signed_out(&self) -> bool {
        self.signed_out
            || self
                .agent
                .signed_in_at
                .as_ref()
                .is_some_and(|path| !path.exists())
    }

    async fn signed_out_error(&mut self, id: &Value) {
        // Grok's words for a start with no sign-in (`grok-signed_out`).
        self.error(
            id,
            AUTH_REQUIRED,
            "Authentication required",
            "no auth method id provided",
        )
        .await;
    }

    async fn prompt(&mut self, id: Value, params: &Value) {
        if !self.ours(params) {
            self.error(&id, INVALID_PARAMS, "Invalid params", "no such session")
                .await;
            return;
        }
        let blocks = params["prompt"].as_array().cloned().unwrap_or_default();
        // A client sends only what `initialize` offered; an agent that was
        // offered no images refuses one, so a host that sends one anyway
        // is caught.
        let refused = blocks
            .iter()
            .find_map(|block| match block["type"].as_str() {
                Some("image") if !self.agent.image => Some("images are not supported"),
                Some("resource") if !self.agent.embedded_context => {
                    Some("embedded resources are not supported")
                }
                _ => None,
            });
        if let Some(refused) = refused {
            self.error(&id, INVALID_PARAMS, "Invalid params", refused)
                .await;
            return;
        }
        let text: String = blocks
            .iter()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = Prompt { id, text };
        let Some(turn) = self.turn.as_mut() else {
            self.start(prompt);
            return;
        };
        match self.agent.mid_turn {
            MidTurn::Queue => self.queued.push_back(prompt),
            MidTurn::Together => turn.joined.push_back(prompt),
            MidTurn::Steer => {
                // Antigravity answers the running prompt at once and goes
                // on with the turn under the new one (`agy-mid_turn`).
                let answered = std::mem::replace(&mut turn.requests, vec![prompt.id.clone()]);
                self.said(true, &prompt.text);
                for request in answered {
                    self.result(&request, json!({"stopReason": "end_turn"}))
                        .await;
                }
            }
        }
    }

    fn start(&mut self, prompt: Prompt) {
        self.said(true, &prompt.text);
        self.turn = Some(Turn {
            requests: vec![prompt.id],
            ..Turn::default()
        });
    }

    fn ours(&self, params: &Value) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| params["sessionId"] == session.id.as_str())
    }

    async fn set_option(&mut self, id: Value, params: &Value) {
        let Some(mut session) = self.session.clone().filter(|_| self.ours(params)) else {
            self.error(&id, INVALID_PARAMS, "Invalid params", "no such session")
                .await;
            return;
        };
        let config = params["configId"].as_str().unwrap_or_default();
        let value = params["value"].as_str().unwrap_or_default().to_owned();
        let known = self
            .options(&session)
            .as_array()
            .into_iter()
            .flatten()
            .find(|option| option["id"] == config)
            .is_some_and(|option| {
                option["options"]
                    .as_array()
                    .is_some_and(|values| values.iter().any(|offered| offered["value"] == value))
            });
        if !known {
            let detail = format!("no value {value:?} for {config:?}");
            self.error(&id, INVALID_PARAMS, "Invalid params", &detail)
                .await;
            return;
        }
        match config {
            "model" => {
                let model = self.model(&value);
                if !model
                    .efforts
                    .iter()
                    .any(|effort| Some(effort) == session.effort.as_ref())
                {
                    session.effort = default_effort(model);
                }
                session.model = value;
            }
            "mode" => session.mode = Some(value),
            _ => session.effort = Some(value),
        }
        let options = self.options(&session);
        self.session = Some(session);
        self.save();
        // Antigravity tells of the change before it answers (`agy-cancel`).
        self.update(json!({"sessionUpdate": "config_option_update", "configOptions": options}))
            .await;
        self.result(&id, json!({"configOptions": options})).await;
    }

    async fn set_mode(&mut self, id: Value, params: &Value) {
        let mode = params["modeId"].as_str().unwrap_or_default().to_owned();
        let offered = self.modes().iter().any(|offered| offered.value == mode);
        let Some(mut session) = self
            .session
            .clone()
            .filter(|_| self.ours(params) && offered)
        else {
            let detail = format!("no mode {mode:?}");
            self.error(&id, INVALID_PARAMS, "Invalid params", &detail)
                .await;
            return;
        };
        session.mode = Some(mode.clone());
        let options = self.options(&session);
        self.session = Some(session);
        self.save();
        // As Antigravity does (`agy-config`).
        self.update(json!({"sessionUpdate": "current_mode_update", "currentModeId": mode}))
            .await;
        self.update(json!({"sessionUpdate": "config_option_update", "configOptions": options}))
            .await;
        self.result(&id, json!({})).await;
    }

    // ---- the session's offers ----

    fn modes(&self) -> Vec<OfferedMode> {
        self.agent.modes.clone().unwrap_or_else(|| {
            // Antigravity's (`agy-config`).
            [
                (
                    "default",
                    "Default",
                    "Ask before edits and commands, per your permission settings.",
                ),
                (
                    "accept-edits",
                    "Accept edits",
                    "Approve file edits in the workspace without asking.",
                ),
                (
                    "auto",
                    "Auto",
                    "Approve edits and safe commands; ask before irreversible actions.",
                ),
                ("plan", "Plan", "Plan carefully before executing a task."),
            ]
            .map(|(value, name, description)| OfferedMode {
                value: value.into(),
                name: name.into(),
                description: Some(description.into()),
            })
            .to_vec()
        })
    }

    fn model(&self, value: &str) -> &OfferedModel {
        self.models
            .iter()
            .find(|model| model.value == value)
            .unwrap_or(&self.models[0])
    }

    /// The session's config options, shaped as Antigravity's: the model,
    /// its thought levels where it has any, and the mode where there are
    /// modes.
    fn options(&self, session: &Session) -> Value {
        let mut options = vec![json!({
            "category": "model",
            "currentValue": session.model,
            "id": "model",
            "name": "Model",
            "options": self.models.iter().map(|model| json!({
                "name": model.display_name(),
                "value": model.value,
            })).collect::<Vec<_>>(),
            "type": "select",
        })];
        let model = self.model(&session.model);
        if let (false, Some(effort)) = (model.efforts.is_empty(), &session.effort) {
            options.push(json!({
                "category": "thought_level",
                "currentValue": effort,
                "id": self.agent.thought_level_id,
                "name": "Thinking Level",
                "options": model.efforts.iter().map(|effort| json!({
                    "name": capitalized(effort),
                    "value": effort,
                })).collect::<Vec<_>>(),
                "type": "select",
            }));
        }
        let modes = self.modes();
        if let (false, Some(mode)) = (modes.is_empty(), &session.mode) {
            options.push(json!({
                "category": "mode",
                "currentValue": mode,
                "id": "mode",
                "name": "Mode",
                "options": modes.iter().map(|mode| {
                    let mut option = json!({"name": mode.name, "value": mode.value});
                    if let Some(description) = &mode.description {
                        option["description"] = json!(description);
                    }
                    option
                }).collect::<Vec<_>>(),
                "type": "select",
            }));
        }
        Value::Array(options)
    }

    /// The commands, after the session's answer, as OpenCode sends them.
    async fn offer_commands(&mut self) {
        if self.commands.is_empty() {
            return;
        }
        let commands: Vec<Value> = self
            .commands
            .iter()
            .map(|command| {
                let mut offered = json!({"name": command.name, "description": command.description});
                // Grok's shape for a command that takes an argument
                // (`grok-allow`).
                if !command.argument_hint.is_empty() {
                    offered["input"] = json!({"hint": command.argument_hint});
                }
                offered
            })
            .collect();
        self.update(
            json!({"sessionUpdate": "available_commands_update", "availableCommands": commands}),
        )
        .await;
    }

    // ---- the turn ----

    async fn play(&mut self) {
        loop {
            if self.exit.is_some() {
                return;
            }
            if self.cancelled() {
                self.cancel().await;
                return;
            }
            let Some(step) = crate::script::next_step(&mut self.steps) else {
                // A script that has said all it has still answers.
                if self.end("end_turn").await {
                    return;
                }
                continue;
            };
            match step {
                Step::Text { chunks } => self.text(chunks.into()).await,
                Step::Thinking { text } => {
                    self.update(json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": text}}))
                        .await;
                }
                Step::Tool(tool) => {
                    let until = tool.wait_for.clone();
                    self.call(tool_call(&tool, None), until.as_deref()).await;
                }
                Step::Ask(Ask::Permission(tool)) => {
                    let until = tool.wait_for.clone();
                    self.call(tool_call(&tool, Some(Offer::Once)), until.as_deref())
                        .await;
                }
                Step::Ask(Ask::Form {
                    message, schema, ..
                }) => self.form(&message, &schema).await,
                Step::Ask(Ask::Question { questions }) => self.questions(&questions).await,
                Step::Call(call) => self.call(call, None).await,
                Step::ToolServer(call) => self.call(server_call(call), None).await,
                Step::Context { used, size } => {
                    self.update(
                        json!({"sessionUpdate": "usage_update", "used": used, "size": size}),
                    )
                    .await;
                }
                Step::Title { title } => {
                    if let Some(session) = self.session.as_mut() {
                        session.title = Some(title.clone());
                    }
                    self.save();
                    self.update(json!({"sessionUpdate": "session_info_update", "title": title}))
                        .await;
                }
                Step::WaitFor { path } => self.wait(None, Some(&path)).await,
                Step::Pause { ms } => {
                    self.wait(Some(Instant::now() + Duration::from_millis(ms)), None)
                        .await;
                }
                Step::Spawn { command } => crate::script::spawn_detached(&command),
                Step::TurnEnd => {
                    if self.end("end_turn").await {
                        return;
                    }
                }
                Step::Stop { reason } => {
                    if self.end(&reason).await {
                        return;
                    }
                }
                Step::Exit { code } => {
                    self.exit = Some(code);
                    return;
                }
                // Refused when the script loaded.
                Step::Ask(_)
                | Step::Usage(_)
                | Step::AuthFailed { .. }
                | Step::Subagents { .. }
                | Step::Repeat { .. } => {}
            }
        }
    }

    /// The turn's steps reached their end; true when its prompts were
    /// answered, false when a prompt that joined it plays on first.
    async fn end(&mut self, reason: &str) -> bool {
        let Some(turn) = self.turn.as_mut() else {
            return true;
        };
        if let Some(joined) = turn.joined.pop_front() {
            turn.requests.push(joined.id);
            self.said(true, &joined.text);
            return false;
        }
        let requests = std::mem::take(&mut turn.requests);
        self.turn = None;
        for request in requests {
            self.result(&request, json!({"stopReason": reason})).await;
        }
        true
    }

    /// Ends a cancelled turn: the chunks already on their way still
    /// stream, as every recorded agent's did (`*-cancel`), then its
    /// prompts are answered `cancelled`. The rest of its steps are
    /// dropped, through its end and the ends of prompts that joined it.
    async fn cancel(&mut self) {
        let turns = 1 + self.turn.as_ref().map_or(0, |turn| turn.joined.len());
        let mut trailing: Vec<String> = Vec::new();
        let mut ended = 0;
        while ended < turns {
            match crate::script::next_step(&mut self.steps) {
                None => break,
                Some(Step::TurnEnd | Step::Stop { .. }) => ended += 1,
                Some(Step::Text { chunks }) => trailing.extend(chunks),
                Some(_) => {}
            }
        }
        trailing.truncate(self.agent.cancel_chunks);
        for chunk in trailing {
            self.chunk(&chunk).await;
        }
        let requests = self
            .turn
            .take()
            .map(|turn| {
                turn.requests
                    .into_iter()
                    .chain(turn.joined.into_iter().map(|prompt| prompt.id))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for request in requests {
            self.result(&request, json!({"stopReason": "cancelled"}))
                .await;
        }
    }

    /// One message, chunk by chunk.
    async fn text(&mut self, mut chunks: VecDeque<String>) {
        self.said(false, "");
        while let Some(chunk) = chunks.pop_front() {
            if self.cancelled() || self.exit.is_some() {
                // What is left is still on its way when the cancel lands.
                chunks.push_front(chunk);
                self.steps.push_front(Step::Text {
                    chunks: chunks.into(),
                });
                return;
            }
            self.chunk(&chunk).await;
            if self.chunk_ms > 0 && !chunks.is_empty() {
                self.wait(
                    Some(Instant::now() + Duration::from_millis(self.chunk_ms)),
                    None,
                )
                .await;
            }
        }
    }

    async fn chunk(&mut self, chunk: &str) {
        if let Some(session) = self.session.as_mut() {
            match session.history.last_mut() {
                Some(Said { user: false, text }) => text.push_str(chunk),
                _ => session.history.push(Said {
                    user: false,
                    text: chunk.to_owned(),
                }),
            }
        }
        self.save();
        self.update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": chunk}}))
            .await;
    }

    /// Starts a message in the session's history.
    fn said(&mut self, user: bool, text: &str) {
        if let Some(session) = self.session.as_mut() {
            session.history.push(Said {
                user,
                text: text.to_owned(),
            });
        }
        self.save();
    }

    /// A tool call, shaped as Antigravity's (`agy-allow`, `agy-edit`):
    /// announced, asked about where it asks, run (until `until` exists,
    /// where it names a file), and finished.
    async fn call(&mut self, call: Call, until: Option<&Path>) {
        self.next_call += 1;
        let id = format!("call_{}", self.next_call);
        let name = call
            .name
            .clone()
            .unwrap_or_else(|| default_name(&call.kind).to_owned());
        let mut base = json!({
            "kind": call.kind,
            "name": name,
            "rawInput": call.input.clone().unwrap_or_else(|| json!({})),
            "title": call.title,
            "toolCallId": id,
        });
        if !call.locations.is_empty() {
            base["locations"] = call
                .locations
                .iter()
                .map(|path| json!({"path": path}))
                .collect();
        }
        let diff = call.diff.as_ref().map(|diff| {
            json!({"type": "diff", "path": diff.path, "oldText": diff.old_text, "newText": diff.new_text})
        });
        self.tool_update("tool_call", &base, "pending", json!({}))
            .await;
        if let Some(offer) = call.ask
            && !self.always.contains(&call.title)
        {
            let mut asked = base.clone();
            asked["sessionUpdate"] = json!("tool_call");
            asked["status"] = json!("pending");
            if let Some(diff) = &diff {
                asked["content"] = json!([diff]);
            }
            let options = offered(offer);
            let answer = self
                .ask(
                    "session/request_permission",
                    json!({"options": options, "toolCall": asked}),
                )
                .await;
            let picked = answer
                .as_ref()
                .filter(|answer| answer["result"]["outcome"]["outcome"] == "selected")
                .and_then(|answer| answer["result"]["outcome"]["optionId"].as_str())
                .and_then(|id| options.iter().find(|option| option["optionId"] == id))
                .and_then(|option| option["kind"].as_str())
                .unwrap_or("reject_once");
            if picked == "allow_always" {
                self.always.insert(call.title.clone());
            }
            if !picked.starts_with("allow") {
                // Antigravity's refused call (`agy-deny`).
                let refused = json!({
                    "content": [{"type": "content", "content": {"type": "text", "text": ""}}],
                    "rawOutput": {"output": ""},
                });
                self.tool_update("tool_call_update", &base, "failed", refused)
                    .await;
                return;
            }
        }
        if self.cancelled() || self.exit.is_some() {
            return;
        }
        self.tool_update("tool_call_update", &base, "in_progress", json!({}))
            .await;
        self.runs(&call.outcome, until).await;
        if self.cancelled() || self.exit.is_some() {
            return;
        }
        if let (true, Some(change)) = (self.edit_files, &call.diff) {
            let path = Path::new(&change.path);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(error) = std::fs::write(path, &change.new_text) {
                eprintln!("fake-acp: writing {}: {error}", change.path);
            }
        }
        let output = call.outcome.text();
        let mut content = Vec::new();
        if !output.is_empty() || diff.is_none() {
            content.push(json!({"type": "content", "content": {"type": "text", "text": output}}));
        }
        content.extend(diff);
        let status = if call.outcome.error {
            "failed"
        } else {
            "completed"
        };
        let done = json!({"content": content, "rawOutput": {"output": output}});
        self.tool_update("tool_call_update", &base, status, done)
            .await;
    }

    /// How long a call runs: its outcome's time, or until a file exists.
    async fn runs(&mut self, outcome: &Outcome, until: Option<&Path>) {
        if outcome.ms.is_some() {
            self.wait(Some(outcome.until()), None).await;
        }
        if let Some(path) = until {
            self.wait(None, Some(path)).await;
        }
    }

    async fn tool_update(&mut self, kind: &str, base: &Value, status: &str, extra: Value) {
        let mut update = base.clone();
        update["sessionUpdate"] = json!(kind);
        update["status"] = json!(status);
        if let Value::Object(extra) = extra {
            for (key, value) in extra {
                update[key] = value;
            }
        }
        self.update(update).await;
    }

    /// The agent's own question, as Antigravity asks it: a question tool's
    /// call whose form is `elicitation/create` (`agy-question`).
    async fn form(&mut self, message: &str, schema: &Schema) {
        self.next_call += 1;
        let base = json!({
            "kind": "other",
            "name": "ask_question",
            "rawInput": {"message": message},
            "title": "Run ask_question",
            "toolCallId": format!("call_{}", self.next_call),
        });
        self.tool_update("tool_call", &base, "pending", json!({}))
            .await;
        let params = json!({
            "message": message,
            "mode": "form",
            "requestedSchema": SCHEMA_SLOT,
            "toolCallId": base["toolCallId"],
        });
        let answer = self
            .ask_with_schema("elicitation/create", params, Some(schema))
            .await;
        if self.cancelled() || self.exit.is_some() {
            return;
        }
        let accepted = answer.is_some_and(|answer| answer["result"]["action"] == "accept");
        self.tool_update("tool_call_update", &base, "in_progress", json!({}))
            .await;
        let status = if accepted { "completed" } else { "failed" };
        self.tool_update("tool_call_update", &base, status, json!({}))
            .await;
    }

    /// Questions as Grok asks them: its question tool's call, whose
    /// questions go to the client in Grok's own request and whose answer
    /// is the picked labels by question (`grok-question`).
    async fn questions(&mut self, questions: &[crate::script::Question]) {
        self.next_call += 1;
        let asked: Vec<Value> = questions
            .iter()
            .map(|question| {
                let options: Vec<Value> = question
                    .options
                    .iter()
                    .map(|option| json!({"description": option.description(), "label": option.label()}))
                    .collect();
                // Grok writes null for a question taking one answer.
                let multi = question.multi_select.then_some(true);
                json!({"multiSelect": multi, "options": options, "question": question.question})
            })
            .collect();
        let base = json!({
            "kind": "other",
            "rawInput": {"questions": asked},
            "title": "ask_user_question",
            "toolCallId": format!("call-{}", self.next_call),
        });
        self.tool_update("tool_call", &base, "pending", json!({}))
            .await;
        // Grok's ids are numbers counted from 0.
        let id = json!(self.next_request);
        self.next_request += 1;
        let mut params = json!({
            "mode": "default",
            "questions": asked,
            "toolCallId": base["toolCallId"],
        });
        if let Some(session) = &self.session {
            params["sessionId"] = json!(session.id);
        }
        self.send(&json!({"id": id, "jsonrpc": "2.0", "method": acp_protocol::xai::ASK_USER_QUESTION, "params": params}))
            .await;
        let answer = self.answer(&key(&id)).await;
        if self.cancelled() || self.exit.is_some() {
            return;
        }
        let result = answer
            .map(|answer| answer["result"].clone())
            .unwrap_or_default();
        let (status, said) = if result["outcome"] == "accepted" {
            let answers: Vec<String> = result["answers"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(question, answer)| format!("{question:?}={answer}"))
                .collect();
            let said = format!(
                "User has answered your questions: {}. You can now continue with the user's answers in mind.",
                answers.join(", ")
            );
            ("completed", said)
        } else {
            ("failed", "User declined to answer questions.".to_owned())
        };
        let done =
            json!({"content": [{"type": "content", "content": {"type": "text", "text": said}}]});
        self.tool_update("tool_call_update", &base, status, done)
            .await;
    }

    async fn ask(&mut self, method: &str, params: Value) -> Option<Value> {
        self.ask_with_schema(method, params, None).await
    }

    /// Sends one of the agent's requests and waits for its answer.
    async fn ask_with_schema(
        &mut self,
        method: &str,
        mut params: Value,
        schema: Option<&Schema>,
    ) -> Option<Value> {
        // Antigravity's ids, strings counted from 1 (`agy-question`): the
        // one agent whose forms are recorded.
        self.next_request += 1;
        let id = self.next_request.to_string();
        if let Some(session) = &self.session {
            params["sessionId"] = json!(session.id);
        }
        let frame = json!({"id": id, "jsonrpc": "2.0", "method": method, "params": params});
        match schema {
            Some(schema) => self.send_with_schema(&frame, schema).await,
            None => self.send(&frame).await,
        }
        self.answer(&key(&json!(id))).await
    }

    // ---- writing ----

    async fn update(&mut self, update: Value) {
        let Some(session) = self.session.as_ref().map(|session| session.id.clone()) else {
            return;
        };
        self.update_of(&session, update).await;
    }

    async fn update_of(&mut self, session: &str, update: Value) {
        let frame = json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {"sessionId": session, "update": update},
        });
        self.send(&frame).await;
    }

    async fn result(&mut self, id: &Value, result: Value) {
        self.send(&json!({"id": id, "jsonrpc": "2.0", "result": result}))
            .await;
    }

    async fn error(&mut self, id: &Value, code: i64, message: &str, data: &str) {
        let error = json!({"code": code, "message": message, "data": data});
        self.send(&json!({"error": error, "id": id, "jsonrpc": "2.0"}))
            .await;
    }

    async fn send(&mut self, frame: &Value) {
        if let Err(error) = self.out.send(frame).await {
            // The host is gone; nothing more can reach it.
            eprintln!("fake-acp: writing: {error}");
            self.exit.get_or_insert(0);
        }
    }

    async fn send_with_schema(&mut self, frame: &Value, schema: &Schema) {
        if let Err(error) = self.out.send_with_schema(frame, schema).await {
            eprintln!("fake-acp: writing: {error}");
            self.exit.get_or_insert(0);
        }
    }

    // ---- the session store ----

    fn path(&self, id: &str) -> Option<PathBuf> {
        // A session id is a file name here; one that is not cannot have
        // been saved.
        let safe = !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        safe.then(|| self.store.join(format!("{id}.json")))
    }

    fn save(&self) {
        let Some(session) = &self.session else {
            return;
        };
        let Some(path) = self.path(&session.id) else {
            return;
        };
        let written = std::fs::create_dir_all(&self.store).and_then(|()| {
            std::fs::write(
                &path,
                serde_json::to_vec(session).expect("a session serializes"),
            )
        });
        if let Err(error) = written {
            eprintln!("fake-acp: saving {}: {error}", path.display());
        }
    }

    fn load(&self, id: &str) -> Option<Session> {
        let bytes = std::fs::read(self.path(id)?).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

/// The options a permission request offers, with the ids and words of the
/// agent whose set it is.
fn offered(offer: Offer) -> Vec<Value> {
    let options: &[(&str, &str, &str)] = match offer {
        // `agy-allow`
        Offer::Once => &[
            ("allow_once", "Allow", "allow"),
            ("reject_once", "Deny", "deny"),
        ],
        // `opencode-allow_always`
        Offer::Always => &[
            ("allow_once", "Allow once", "once"),
            ("allow_always", "Always allow", "always"),
            ("reject_once", "Reject", "reject"),
        ],
    };
    options
        .iter()
        .map(|(kind, name, id)| json!({"kind": kind, "name": name, "optionId": id}))
        .collect()
}

/// A provider-neutral tool step as an ACP call: exploration reads,
/// consequential runs a command.
fn tool_call(tool: &Tool, ask: Option<Offer>) -> Call {
    let (kind, title) = if tool.is_exploration() {
        ("read", "Read file")
    } else {
        ("execute", "Run command")
    };
    let title = tool
        .input
        .as_ref()
        .and_then(|input| {
            input["command"]
                .as_str()
                .or_else(|| input["path"].as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| title.to_owned());
    Call {
        kind: kind.into(),
        title,
        name: tool.name.clone(),
        input: tool.input.clone(),
        locations: Vec::new(),
        diff: None,
        ask,
        outcome: tool.outcome.clone(),
    }
}

/// A tool server's tool as Antigravity calls it: kind `other`, named
/// `mcp_<server>_<tool>` (`agy-elicitation`).
fn server_call(call: ToolServerCall) -> Call {
    let name = format!("mcp_{}_{}", call.server, call.tool);
    Call {
        kind: "other".into(),
        title: format!("Run {name}"),
        name: Some(name),
        input: call.input,
        locations: Vec::new(),
        diff: None,
        ask: call.ask,
        outcome: call.outcome,
    }
}

/// Antigravity's tool for a kind of call.
fn default_name(kind: &str) -> &'static str {
    match kind {
        "read" => "read_file",
        "edit" => "edit_file",
        "execute" => "run_command",
        "search" => "grep_search",
        _ => "tool",
    }
}

fn default_effort(model: &OfferedModel) -> Option<String> {
    model
        .default_effort
        .clone()
        .or_else(|| model.efforts.first().cloned())
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A JSON-RPC id as a map key: a string and a number of the same digits
/// are different ids.
fn key(id: &Value) -> String {
    id.to_string()
}
