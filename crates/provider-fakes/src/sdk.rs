//! `fake-claude-sdk`: headless Claude (`claude -p`) speaking stream-JSON.
//!
//! Modelled on what Claude Code 2.1.282 and 2.1.283 do on the wire: a user
//! message's `uuid` is echoed on its `isReplay` reflection (with
//! `--replay-user-messages`) and names its `command_lifecycle` frames
//! (queued, started, completed or cancelled); a message written while a turn
//! runs is queued, and at default priority folded into that turn at its next
//! tool result, so one `result` closes the turn for all of them; `later`
//! waits for its own turn; `now` cuts the running turn short and runs next.
//! `cancel_async_message` withdraws a message still queued. Asks are control
//! requests (`can_use_tool` for permissions, questions and plans,
//! `elicitation` for a tool server's form) that block the turn until the
//! host's control response.

use std::collections::{BTreeMap, VecDeque};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader, Stdout};
use tokio::sync::mpsc;

use crate::claude::{Args, Ids, timestamp, uuid};
use crate::lines::Out;
use crate::script::{Ask, OfferedCommand, OfferedModel, Question, Script, Step, Tool};
use crate::{DRIFT_EXIT, Mode};

/// The asks headless Claude can raise.
pub const RAISES: &[&str] = &["permission", "question", "plan", "form"];
/// The provider-specific steps headless Claude can play.
pub const PLAYS: &[&str] = &["usage", "auth_failed"];

pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if crate::claude::answered_version(&args) {
        return 0;
    }
    let mode = match crate::mode_from_env() {
        Ok(mode) => mode,
        Err(error) => {
            eprintln!("fake-claude-sdk: {error}");
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
                match crate::lines::play(
                    &process,
                    BufReader::new(tokio::io::stdin()),
                    tokio::io::stdout(),
                )
                .await
                {
                    Ok(code) => code.unwrap_or(0),
                    Err(error) => {
                        eprintln!("fake-claude-sdk: {error}");
                        DRIFT_EXIT
                    }
                }
            }
            Mode::Script(script) => {
                if let Err(error) = script.check("headless Claude", RAISES, PLAYS) {
                    eprintln!("fake-claude-sdk: {error}");
                    return DRIFT_EXIT;
                }
                Engine::new(script, Args::parse(&args)).run().await
            }
        }
    });
    // The stdin reader blocks in a thread the runtime would wait for on
    // drop, so a script's exit would wait for the host to close stdin.
    runtime.shutdown_background();
    code
}

/// A user message Claude has taken but not yet run.
#[derive(Clone, Debug)]
struct Queued {
    /// The host's uuid, when it sent one; lifecycle frames need it.
    uuid: Option<String>,
    message: Value,
}

enum Priority {
    Now,
    Next,
    Later,
}

/// Why a turn stopped before its steps did.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cut {
    Interrupted,
    Preempted,
}

struct Engine {
    out: Out<Stdout>,
    input: mpsc::UnboundedReceiver<Value>,
    eof: bool,
    steps: VecDeque<Step>,
    args: Args,
    /// The tool servers the launch configured.
    servers: crate::mcp::ToolServers,
    session: String,
    model: String,
    /// The effort this session chose, at launch or since; the model's
    /// default otherwise.
    effort: Option<String>,
    /// What the initialize answer offers.
    models: Vec<OfferedModel>,
    commands: Vec<OfferedCommand>,
    /// Every model takes auto mode, as the scripted Claude's terminal
    /// offers it.
    offers_auto_mode: bool,
    mode: String,
    cwd: String,
    ids: Ids,
    /// Messages that start turns of their own, in order.
    queue: VecDeque<Queued>,
    /// Default-priority messages written mid-turn, folded at the next tool
    /// result.
    folding: Vec<Queued>,
    /// Whether a turn is running.
    busy: bool,
    /// The command uuid that started the running turn.
    running: Option<String>,
    /// Command uuids folded into the running turn.
    absorbed: Vec<String>,
    cut: Option<Cut>,
    /// The interrupt refused a call whose permission it cancelled.
    refused_call: bool,
    answers: BTreeMap<String, Value>,
    turns: u32,
    last_text: String,
    /// Content blocks the current model response has sent: Claude numbers
    /// a response's streamed blocks from zero, in the order its assistant
    /// frames carry them.
    blocks: u32,
    /// The tool servers each turn's init reports.
    server_states: Vec<crate::script::ServerState>,
    /// The context in use each message reports, when scripted.
    context_tokens: Option<u64>,
    edit_files: bool,
    chunk_ms: u64,
    turn_ms: u64,
    /// The turn ended on a refused credential, with Claude's message.
    auth_failed: Option<String>,
    /// A form schema the next frame carries, as the script wrote it.
    schema: Option<crate::script::Schema>,
    /// The tasks the session's task tools made, by id, with their status.
    tasks: Vec<(String, String)>,
    /// Commands started in the background that have not finished.
    jobs: Vec<Job>,
    /// Initialize names no account.
    signed_out: bool,
}

/// A command Claude runs in the background: it outlives its call and ends
/// when its gate file exists, or at once without one.
struct Job {
    task: String,
    call: String,
    description: String,
    gate: Option<std::path::PathBuf>,
}

impl Engine {
    fn new(script: Script, args: Args) -> Self {
        let (tx, input) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                crate::script::log_input(&line);
                match serde_json::from_str::<Value>(&line) {
                    Ok(value) => {
                        if tx.send(value).is_err() {
                            break;
                        }
                    }
                    Err(error) => eprintln!("fake-claude-sdk: unreadable input {line}: {error}"),
                }
            }
        });
        let session = args
            .resume
            .clone()
            .or_else(|| args.session_id.clone())
            .unwrap_or_else(uuid);
        let model = args
            .model
            .clone()
            .or_else(|| script.model.clone())
            .unwrap_or_else(|| "claude-fake-1".into());
        let args_effort = args.effort.clone();
        let mode = args
            .permission_mode
            .clone()
            .unwrap_or_else(|| "default".into());
        let cwd = std::env::current_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default();
        Self {
            out: Out::new(tokio::io::stdout()),
            input,
            eof: false,
            steps: script.steps.into(),
            servers: crate::mcp::ToolServers::from_claude(&args.mcp_config),
            args,
            session,
            models: OfferedModel::offered(&script.models, &model),
            commands: script.commands,
            offers_auto_mode: script.offers_auto_mode,
            effort: args_effort,
            model,
            mode,
            cwd,
            ids: Ids::default(),
            queue: VecDeque::new(),
            folding: Vec::new(),
            busy: false,
            running: None,
            absorbed: Vec::new(),
            cut: None,
            refused_call: false,
            answers: BTreeMap::new(),
            turns: 0,
            last_text: String::new(),
            blocks: 0,
            server_states: script.servers,
            context_tokens: script.context_tokens,
            edit_files: script.edit_files,
            chunk_ms: script.chunk_ms,
            turn_ms: script.turn_ms.unwrap_or(1),
            auth_failed: None,
            schema: None,
            tasks: Vec::new(),
            jobs: Vec::new(),
            signed_out: script.signed_out,
        }
    }

    async fn run(mut self) -> i32 {
        // Claude resumes only a session whose transcript holds a
        // conversation.
        if let Some(session) = self
            .args
            .resume
            .as_ref()
            .filter(|_| !self.has_conversation())
        {
            eprintln!("No conversation found with session ID: {session}");
            return 1;
        }
        // Nor does it start a session under an id it already has.
        if let Some(session) = self
            .args
            .session_id
            .as_ref()
            .filter(|_| self.args.resume.is_none() && self.transcript().exists())
        {
            eprintln!("Error: Session ID {session} is already in use.");
            return 1;
        }
        loop {
            if let Some(next) = self.queue.pop_front() {
                if let Some(code) = self.turn(next).await {
                    return code;
                }
                continue;
            }
            if self.eof {
                return 0;
            }
            // A background command can end while no turn runs.
            if !self.jobs.is_empty() {
                self.pump().await;
                continue;
            }
            match self.input.recv().await {
                Some(frame) => self.handle(frame).await,
                None => self.eof = true,
            }
        }
    }

    async fn send(&mut self, frame: Value) {
        // A form schema waits for the frame that carries it.
        let carries = self
            .schema
            .as_ref()
            .is_some_and(|_| frame.to_string().contains(crate::script::SCHEMA_SLOT));
        let sent = match self.schema.take() {
            Some(schema) if carries => self.out.send_with_schema(&frame, &schema).await,
            kept => {
                self.schema = kept;
                self.out.send(&frame).await
            }
        };
        if sent.is_err() {
            // The host is gone; so is the reason to run.
            std::process::exit(0);
        }
    }

    /// Take whatever the host has written, without waiting.
    async fn drain(&mut self) {
        self.finish_jobs().await;
        loop {
            match self.input.try_recv() {
                Ok(frame) => self.handle(frame).await,
                Err(mpsc::error::TryRecvError::Empty) => return,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.eof = true;
                    return;
                }
            }
        }
    }

    /// Wait for the next host frame, or for a moment to pass.
    async fn pump(&mut self) {
        self.finish_jobs().await;
        if self.eof {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            return;
        }
        tokio::select! {
            frame = self.input.recv() => match frame {
                Some(frame) => self.handle(frame).await,
                None => self.eof = true,
            },
            () = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }
    }

    async fn handle(&mut self, frame: Value) {
        match frame["type"].as_str() {
            Some("control_request") => self.control(frame).await,
            Some("control_response") => {
                let response = &frame["response"];
                if let Some(id) = response["request_id"].as_str() {
                    self.answers.insert(id.to_owned(), response.clone());
                }
            }
            Some("user") => self.user(frame).await,
            other => eprintln!("fake-claude-sdk: ignoring input of type {other:?}"),
        }
    }

    async fn control(&mut self, frame: Value) {
        let id = frame["request_id"].clone();
        let request = &frame["request"];
        let body = match request["subtype"].as_str().unwrap_or_default() {
            "initialize" => {
                let response = self.initialize();
                self.send(json!({
                    "type": "control_response",
                    "response": {
                        "subtype": "success",
                        "request_id": id,
                        "response": response,
                        "pending_permission_requests": [],
                        "pending_user_dialog_requests": [],
                    },
                }))
                .await;
                return;
            }
            "set_model" => {
                if let Some(model) = request["model"].as_str() {
                    self.model = model.to_owned();
                }
                None
            }
            "apply_flag_settings" => {
                if let Some(effort) = request["settings"].get("effortLevel") {
                    self.effort = effort.as_str().map(str::to_owned);
                }
                None
            }
            "get_settings" => Some(json!({
                "applied": { "model": self.model, "effort": self.applied_effort() },
            })),
            "set_permission_mode" => {
                if let Some(mode) = request["mode"].as_str() {
                    self.mode = mode.to_owned();
                }
                Some(json!({ "mode": self.mode }))
            }
            "interrupt" => {
                if self.busy {
                    self.cut = Some(Cut::Interrupted);
                }
                Some(json!({ "still_queued": self.still_queued() }))
            }
            "cancel_async_message" => {
                let target = request["message_uuid"].as_str().unwrap_or_default();
                let cancelled = self.withdraw(target);
                if cancelled {
                    self.lifecycle(target, "cancelled").await;
                }
                Some(json!({ "cancelled": cancelled }))
            }
            other => {
                let error = format!("Unsupported control request: {other}");
                self.send(json!({
                    "type": "control_response",
                    "response": { "subtype": "error", "request_id": id, "error": error },
                }))
                .await;
                return;
            }
        };
        let mut response = json!({ "subtype": "success", "request_id": id });
        if let Some(body) = body {
            response["response"] = body;
        }
        self.send(json!({ "type": "control_response", "response": response }))
            .await;
    }

    /// The effort the session runs at: its own choice, else the current
    /// model's default.
    fn applied_effort(&self) -> Option<String> {
        self.effort.clone().or_else(|| {
            self.models
                .iter()
                .find(|model| {
                    model.value == self.model
                        || model.resolved_model.as_deref() == Some(&self.model)
                })
                .and_then(|model| model.default_effort.clone())
        })
    }

    fn still_queued(&self) -> Vec<String> {
        self.queue
            .iter()
            .chain(&self.folding)
            .filter_map(|queued| queued.uuid.clone())
            .collect()
    }

    fn withdraw(&mut self, target: &str) -> bool {
        let before = self.queue.len() + self.folding.len();
        self.queue
            .retain(|queued| queued.uuid.as_deref() != Some(target));
        self.folding
            .retain(|queued| queued.uuid.as_deref() != Some(target));
        before != self.queue.len() + self.folding.len()
    }

    async fn user(&mut self, frame: Value) {
        let queued = Queued {
            uuid: frame["uuid"].as_str().map(str::to_owned),
            message: frame["message"].clone(),
        };
        let priority = match frame["priority"].as_str() {
            Some("now") => Priority::Now,
            Some("later") => Priority::Later,
            _ => Priority::Next,
        };
        if let Some(uuid) = queued.uuid.clone() {
            self.lifecycle(&uuid, "queued").await;
        }
        if !self.busy {
            self.queue.push_back(queued);
            return;
        }
        match priority {
            Priority::Later => self.queue.push_back(queued),
            Priority::Next => self.folding.push(queued),
            Priority::Now => {
                self.cut = Some(Cut::Preempted);
                self.queue.push_front(queued);
            }
        }
    }

    async fn lifecycle(&mut self, command: &str, state: &str) {
        let frame = json!({
            "type": "command_lifecycle",
            "command_uuid": command,
            "state": state,
            "session_id": self.session,
            "uuid": uuid(),
        });
        self.send(frame).await;
    }

    fn initialize(&self) -> Value {
        json!({
            "commands": self.commands.iter().map(|command| json!({
                "name": command.name,
                "description": command.description,
                "argumentHint": command.argument_hint,
            })).collect::<Vec<_>>(),
            "agents": [],
            // Signed out, Claude leaves out every field but the backend.
            "account": if self.signed_out {
                json!({ "apiProvider": "firstParty" })
            } else {
                json!({
                    "email": "fake@example.com",
                    "subscriptionType": "Claude Max",
                    "tokenSource": "claude.ai",
                    "apiProvider": "firstParty",
                })
            },
            "models": self.models.iter().map(|model| {
                let mut offered = json!({
                    "value": model.value,
                    "displayName": model.display_name(),
                    "description": model.description,
                });
                // Claude leaves the field out for a model that takes no effort.
                if !model.efforts.is_empty() {
                    offered["supportedEffortLevels"] = json!(model.efforts);
                }
                if let Some(resolved) = &model.resolved_model {
                    offered["resolvedModel"] = json!(resolved);
                }
                if self.offers_auto_mode {
                    offered["supportsAutoMode"] = json!(true);
                }
                offered
            }).collect::<Vec<_>>(),
            "output_style": "default",
            "available_output_styles": ["default"],
            "current_permission_mode": self.mode,
            "fast_mode_state": "off",
        })
    }

    /// Where Claude keeps this session's transcript.
    fn transcript(&self) -> std::path::PathBuf {
        crate::playback::transcript_path(
            &crate::playback::claude_config_dir(),
            std::path::Path::new(&self.cwd),
            &self.session,
        )
    }

    /// Whether this session's transcript has a user or assistant row, as
    /// Claude's resume asks.
    fn has_conversation(&self) -> bool {
        std::fs::read_to_string(self.transcript()).is_ok_and(|rows| {
            rows.lines().any(|row| {
                serde_json::from_str::<Value>(row)
                    .is_ok_and(|row| row["type"] == "user" || row["type"] == "assistant")
            })
        })
    }

    /// Claude writes a session's transcript once its first turn begins,
    /// with the prompt's row.
    fn begin_transcript(&self) {
        let path = self.transcript();
        if path.exists() {
            return;
        }
        let row = json!({ "type": "user", "sessionId": self.session, "cwd": self.cwd });
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, format!("{row}\n")));
        if let Err(error) = written {
            eprintln!("fake-claude-sdk: transcript: {error}");
        }
    }

    fn init_frame(&self) -> Value {
        let mut frame = json!({
            "type": "system",
            "subtype": "init",
            "agents": [],
            "analytics_disabled": false,
            "apiKeySource": "none",
            "capabilities": ["interrupt_receipt_v1", "msg_lifecycle_v1"],
            "claude_code_version": crate::claude::VERSION,
            "cwd": self.cwd,
            "fast_mode_disabled_reason": "sdk_opt_in_required",
            "fast_mode_state": "off",
            "mcp_servers": self.server_states.iter().map(|server| json!({
                "name": server.name,
                "source": "user",
                "status": server.status,
            })).collect::<Vec<_>>(),
            "memory_paths": { "auto": format!("{}/memory", self.cwd) },
            "per_turn_effort_active": false,
            "product_feedback_disabled": false,
            "terminal_slash_commands": [],
            "view_mode": "default",
            "model": self.model,
            "output_style": "default",
            "permissionMode": self.mode,
            "plugins": [],
            "session_id": self.session,
            "skills": [],
            "slash_commands": [],
            "tools": ["Bash", "Read", "Edit", "Write", "AskUserQuestion", "ExitPlanMode"],
            "uuid": uuid(),
        });
        if let Some(socket) = &self.args.messaging_socket {
            frame["messaging_socket_path"] = json!(socket.display().to_string());
        }
        frame
    }

    /// Run one turn from its first message. `Some(code)` ends the process.
    async fn turn(&mut self, first: Queued) -> Option<i32> {
        self.busy = true;
        self.turns += 1;
        self.cut = None;
        self.running = None;
        self.absorbed.clear();
        self.last_text.clear();
        // One model response per request: a new one after each tool's
        // result, as Claude makes a new API call to continue.
        let mut request = self.ids.next("req_fake");
        let mut message = self.ids.next("msg_fake");
        self.blocks = 0;
        if let Some(uuid) = first.uuid.clone() {
            self.lifecycle(&uuid, "started").await;
            self.running = Some(uuid);
        }
        // `/clear` runs no model turn: Claude announces the reset with an id
        // it makes up, then starts a new session under another and reports
        // that one in its init.
        let clear = first.message["content"] == "/clear";
        if clear {
            let frame = json!({
                "type": "conversation_reset",
                "new_conversation_id": uuid(),
                "uuid": uuid(),
                "session_id": self.session,
                "trigger": "clear",
            });
            self.send(frame).await;
            self.session = uuid();
        }
        let init = self.init_frame();
        self.send(init).await;
        if !clear {
            self.begin_transcript();
            self.replay(&first).await;
        }
        let mut calls = 0;
        let exit = if clear {
            None
        } else {
            loop {
                self.drain().await;
                if self.cut.is_some() {
                    break None;
                }
                let Some(step) = crate::script::next_step(&mut self.steps) else {
                    break None;
                };
                match step {
                    Step::Text { chunks } => self.text(&request, &message, &chunks).await,
                    Step::Thinking { text } => {
                        let block =
                            json!({ "type": "thinking", "thinking": text, "signature": "" });
                        let frame = self.assistant(&request, &message, block);
                        self.send(frame).await;
                    }
                    Step::Tool(tool) => {
                        calls += 1;
                        let (id, input) = self.tool_use(&request, &message, &tool).await;
                        if input["run_in_background"] == json!(true) {
                            self.launch(&id, &tool, &input).await;
                        } else {
                            self.run_tool(&tool).await;
                            let outcome = self.outcome(&tool, &input).await;
                            self.tool_result(&id, &tool, &input, outcome).await;
                        }
                        self.fold().await;
                        request = self.ids.next("req_fake");
                        message = self.ids.next("msg_fake");
                        self.blocks = 0;
                    }
                    Step::Ask(ask) => {
                        calls += 1;
                        self.ask(&request, &message, ask).await;
                        if self.cut.is_none() {
                            self.fold().await;
                        }
                        request = self.ids.next("req_fake");
                        message = self.ids.next("msg_fake");
                        self.blocks = 0;
                    }
                    Step::WaitFor { path } => {
                        while !path.exists() && self.cut.is_none() {
                            self.pump().await;
                        }
                    }
                    Step::Spawn { command } => crate::script::spawn_detached(&command),
                    Step::Pause { ms } => {
                        let until =
                            tokio::time::Instant::now() + std::time::Duration::from_millis(ms);
                        while tokio::time::Instant::now() < until && self.cut.is_none() {
                            self.pump().await;
                        }
                    }
                    Step::TurnEnd => break None,
                    Step::Exit { code } => break Some(code),
                    Step::Usage(usage) => self.rate_limit(&usage).await,
                    Step::AuthFailed { message } => {
                        self.auth_retries().await;
                        let frame = self.auth_error(&request, &message);
                        self.send(frame).await;
                        self.auth_failed = Some(message);
                        break None;
                    }
                    Step::Repeat { .. } => unreachable!("next_step unrolls repeats"),
                }
            }
        };
        if let Some(code) = exit {
            return Some(code);
        }
        if let Some(message) = self.auth_failed.take() {
            self.failed(calls, &message).await;
            for command in std::mem::take(&mut self.absorbed)
                .into_iter()
                .chain(self.running.take())
            {
                self.lifecycle(&command, "completed").await;
            }
            self.busy = false;
            return None;
        }
        // Commands folded into a finished turn complete before its result,
        // the one that started it after; a preempted turn's are cancelled.
        let absorbed = std::mem::take(&mut self.absorbed);
        let commands = match self.cut {
            None => {
                for command in &absorbed {
                    self.lifecycle(command, "completed").await;
                }
                self.succeed(calls).await;
                self.running
                    .take()
                    .into_iter()
                    .map(|c| (c, "completed"))
                    .collect()
            }
            Some(Cut::Preempted) => {
                self.preempted(calls).await;
                self.running
                    .take()
                    .into_iter()
                    .chain(absorbed)
                    .map(|c| (c, "cancelled"))
                    .collect()
            }
            Some(Cut::Interrupted) => {
                self.interrupted(calls).await;
                self.running
                    .take()
                    .into_iter()
                    .chain(absorbed)
                    .map(|c| (c, "completed"))
                    .collect::<Vec<_>>()
            }
        };
        for (command, state) in commands {
            self.lifecycle(&command, state).await;
        }
        // Messages waiting to fold when the turn ended run as the next turn.
        let folding = std::mem::take(&mut self.folding);
        for (index, queued) in folding.into_iter().enumerate() {
            self.queue.insert(index, queued);
        }
        self.busy = false;
        None
    }

    async fn replay(&mut self, queued: &Queued) {
        if !self.args.replay_user_messages {
            return;
        }
        let frame = json!({
            "type": "user",
            "message": queued.message,
            "parent_tool_use_id": null,
            "session_id": self.session,
            "timestamp": timestamp(),
            "uuid": queued.uuid.clone().unwrap_or_else(uuid),
            "isReplay": true,
        });
        self.send(frame).await;
    }

    /// Fold every default-priority message written since the last boundary
    /// into the running turn: their reflections, then their starts.
    async fn fold(&mut self) {
        self.drain().await;
        let folding = std::mem::take(&mut self.folding);
        for queued in &folding {
            self.replay(queued).await;
        }
        for queued in folding {
            if let Some(uuid) = queued.uuid {
                self.lifecycle(&uuid, "started").await;
                self.absorbed.push(uuid);
            }
        }
    }

    /// A call held until its file exists keeps taking the host's frames
    /// while it runs; an interrupt stops waiting for it.
    async fn run_tool(&mut self, tool: &Tool) {
        let until = tool.outcome.until();
        while tokio::time::Instant::now() < until && self.cut != Some(Cut::Interrupted) {
            self.pump().await;
        }
        let Some(path) = &tool.wait_for else { return };
        while !path.exists() && self.cut != Some(Cut::Interrupted) {
            self.pump().await;
        }
    }

    /// Start a command in the background as Claude does: the job list
    /// gains it, its task starts, and the call's result only says it was
    /// launched. It ends in `finish_jobs`.
    async fn launch(&mut self, call: &str, tool: &Tool, input: &Value) {
        let job = Job {
            task: self.ids.next("bfake"),
            call: call.to_owned(),
            description: input["description"]
                .as_str()
                .or(input["command"].as_str())
                .unwrap_or_default()
                .to_owned(),
            gate: tool.wait_for.clone(),
        };
        let started = json!({
            "type": "system",
            "subtype": "task_started",
            "task_id": job.task,
            "tool_use_id": job.call,
            "description": job.description,
            "task_type": "local_bash",
            "is_backgrounded": true,
            "session_id": self.session,
            "uuid": uuid(),
        });
        let content = format!(
            "Command running in background with ID: {}. Output is being written to: /tmp/claude/tasks/{}.output",
            job.task, job.task
        );
        let sidecar = json!({
            "backgroundTaskId": job.task,
            "interrupted": false,
            "isImage": false,
            "noOutputExpected": false,
            "stderr": "",
            "stdout": "",
        });
        self.jobs.push(job);
        self.jobs_changed().await;
        self.send(started).await;
        self.user_result(call, json!(content), false, sidecar).await;
    }

    /// End every background command whose gate is open: the job list
    /// loses it, its task completes, and Claude says how it ended.
    async fn finish_jobs(&mut self) {
        let (done, running): (Vec<Job>, Vec<Job>) = std::mem::take(&mut self.jobs)
            .into_iter()
            .partition(|job| job.gate.as_ref().is_none_or(|gate| gate.exists()));
        self.jobs = running;
        if done.is_empty() {
            return;
        }
        self.jobs_changed().await;
        for job in done {
            let updated = json!({
                "type": "system",
                "subtype": "task_updated",
                "task_id": job.task,
                "patch": { "status": "completed", "end_time": now_s() * 1000 },
                "session_id": self.session,
                "uuid": uuid(),
            });
            self.send(updated).await;
            let notified = json!({
                "type": "system",
                "subtype": "task_notification",
                "task_id": job.task,
                "tool_use_id": job.call,
                "status": "completed",
                "output_file": format!("/tmp/claude/tasks/{}.output", job.task),
                "summary": format!(
                    "Background command \"{}\" completed (exit code 0)",
                    job.description
                ),
                "session_id": self.session,
                "uuid": uuid(),
            });
            self.send(notified).await;
        }
    }

    async fn jobs_changed(&mut self) {
        let tasks: Vec<Value> = self
            .jobs
            .iter()
            .map(|job| {
                json!({
                    "task_id": job.task,
                    "task_type": "local_bash",
                    "description": job.description,
                })
            })
            .collect();
        let frame = json!({
            "type": "system",
            "subtype": "background_tasks_changed",
            "tasks": tasks,
            "session_id": self.session,
            "uuid": uuid(),
        });
        self.send(frame).await;
    }

    fn assistant(&mut self, request: &str, message: &str, block: Value) -> Value {
        self.blocks += 1;
        json!({
            "type": "assistant",
            "message": {
                "container": null,
                "content": [block],
                "context_management": null,
                "diagnostics": null,
                "id": message,
                "input_transformations": [],
                "model": self.model,
                "role": "assistant",
                "stop_details": null,
                "stop_reason": null,
                "stop_sequence": null,
                "type": "message",
                "usage": usage(self.context_tokens),
            },
            "parent_tool_use_id": null,
            "request_id": request,
            "session_id": self.session,
            "timestamp": timestamp(),
            "uuid": uuid(),
        })
    }

    async fn text(&mut self, request: &str, message: &str, chunks: &[String]) {
        let text: String = chunks.concat();
        let index = self.blocks;
        if self.args.include_partial_messages {
            self.stream(json!({
                "type": "message_start",
                "message": {
                    "content": [],
                    "diagnostics": null,
                    "id": message,
                    "model": self.model,
                    "role": "assistant",
                    "stop_details": null,
                    "stop_reason": null,
                    "stop_sequence": null,
                    "type": "message",
                    "usage": usage(self.context_tokens),
                },
            }))
            .await;
            self.stream(json!({
                "type": "content_block_start",
                "index": index,
                "content_block": { "type": "text", "text": "" },
            }))
            .await;
            for (at, chunk) in chunks.iter().enumerate() {
                if at > 0 && self.chunk_ms > 0 {
                    self.pause(self.chunk_ms).await;
                }
                self.stream(json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": { "type": "text_delta", "text": chunk },
                }))
                .await;
            }
        }
        let frame = self.assistant(request, message, json!({ "type": "text", "text": text }));
        self.send(frame).await;
        if self.args.include_partial_messages {
            self.stream(json!({ "type": "content_block_stop", "index": index }))
                .await;
        }
        self.last_text = text;
    }

    /// Stay busy for `ms`, still taking the host's frames.
    async fn pause(&mut self, ms: u64) {
        let until = tokio::time::Instant::now() + std::time::Duration::from_millis(ms);
        while tokio::time::Instant::now() < until && self.cut.is_none() {
            self.pump().await;
        }
    }

    /// The account's usage limits, as Claude reports them between frames.
    async fn rate_limit(&mut self, usage: &crate::script::Usage) {
        let now = now_s();
        // Claude names both windows every time; one the script leaves out
        // is barely used.
        let mut windows = serde_json::Map::new();
        for (name, resets_in_s) in [("five_hour", 5 * 3600), ("seven_day", 7 * 86400)] {
            windows.insert(
                name.to_owned(),
                json!({ "resetsAt": now + resets_in_s, "utilization": 0.01 }),
            );
        }
        for window in &usage.windows {
            windows.insert(
                window.name.clone(),
                json!({
                    "resetsAt": now + window.resets_in_s,
                    "utilization": window.used_percent / 100.0,
                }),
            );
        }
        let first = usage.windows.first();
        let mut info = json!({
            "isUsingOverage": false,
            "overageDisabledReason": "org_level_disabled",
            "overageStatus": "rejected",
            "rateLimitType": first.map_or("five_hour", |window| window.name.as_str()),
            "resetsAt": now + first.map_or(0, |window| window.resets_in_s),
            "status": usage.status,
            "unifiedWindows": windows,
        });
        // Past a threshold Claude also says how much is used.
        if usage.status != "allowed" {
            let used = first.map_or(0.0, |window| window.used_percent / 100.0);
            info["utilization"] = json!(used);
            info["surpassedThreshold"] = json!(0.75);
        }
        let frame = json!({
            "type": "rate_limit_event",
            "rate_limit_info": info,
            "session_id": self.session,
            "uuid": uuid(),
        });
        self.send(frame).await;
    }

    /// Claude retries a refused credential twice before giving up.
    async fn auth_retries(&mut self) {
        for (attempt, delay) in [(1, 612), (2, 1233)] {
            let frame = json!({
                "type": "system",
                "subtype": "api_retry",
                "attempt": attempt,
                "error": "authentication_failed",
                "error_status": 401,
                "max_retries": 2,
                "retry_delay_ms": delay,
                "session_id": self.session,
                "uuid": uuid(),
            });
            self.send(frame).await;
        }
    }

    /// The error message Claude writes as the turn's reply when its
    /// credential is refused.
    fn auth_error(&mut self, request: &str, message: &str) -> Value {
        let id = self.ids.next("msg_fake");
        let mut frame = self.assistant(request, &id, json!({ "type": "text", "text": message }));
        frame["error"] = json!("authentication_failed");
        frame["is_api_error_message"] = json!(true);
        frame["message"]["model"] = json!("<synthetic>");
        frame["message"]["stop_reason"] = json!("stop_sequence");
        frame["message"]["stop_sequence"] = json!("");
        self.last_text = message.to_owned();
        frame
    }

    /// A turn that ended on a refused credential: an error result.
    async fn failed(&mut self, calls: u32, message: &str) {
        let frame = json!({
            "type": "result",
            "subtype": "success",
            "api_error_status": 401,
            "duration_api_ms": 0,
            "duration_ms": 1,
            "fast_mode_disabled_reason": "sdk_opt_in_required",
            "fast_mode_state": "off",
            // Claude 2.1.289 also times each response.
            "first_content_frame_ms": 1,
            "time_to_request_ms": 1,
            "ttft_ms": 1,
            "ttft_stream_ms": 1,
            "is_error": true,
            "modelUsage": {},
            "result_index": 0,
            "subagent_stats": subagent_stats(),
            "num_turns": calls + 1,
            "permission_denials": [],
            "queued_turn_count": self.queue.len(),
            "result": message,
            "session_id": self.session,
            "stop_reason": "stop_sequence",
            "terminal_reason": "completed",
            "total_cost_usd": 0.0,
            "usage": result_usage(),
            "uuid": uuid(),
        });
        self.send(frame).await;
    }

    async fn stream(&mut self, event: Value) {
        let mut frame = json!({
            "type": "stream_event",
            "event": event,
            "parent_tool_use_id": null,
            "session_id": self.session,
            "uuid": uuid(),
        });
        if frame["event"]["type"] == "message_start" {
            frame["ttft_ms"] = json!(1);
        }
        self.send(frame).await;
    }

    async fn tool_use(&mut self, request: &str, message: &str, tool: &Tool) -> (String, Value) {
        let (name, input) = claude_tool(tool, &self.cwd);
        let id = self.ids.next("toolu_fake");
        let block = json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
            "caller": { "type": "direct" },
        });
        let mut frame = self.assistant(request, message, block);
        frame["wire_tool_inputs"] = json!({ id.clone(): input.clone() });
        self.send(frame).await;
        (id, input)
    }

    /// What a call returns: a configured tool server's answer, or the
    /// script's output.
    async fn outcome(&mut self, tool: &Tool, input: &Value) -> Result<String, String> {
        let (name, _) = claude_tool(tool, &self.cwd);
        if let Some((server, tool_name)) = crate::mcp::ToolServers::split(&name)
            && let Some(answer) = self.servers.call(server, tool_name, input).await
        {
            return answer;
        }
        Ok(tool.outcome.text())
    }

    /// The tool's result row: its output, or the refusal or failure text.
    async fn tool_result(
        &mut self,
        id: &str,
        tool: &Tool,
        input: &Value,
        outcome: Result<String, String>,
    ) {
        let (name, _) = claude_tool(tool, &self.cwd);
        let (content, is_error, sidecar) = match outcome {
            Ok(output) if tool.outcome.error => {
                (output.clone(), true, json!(format!("Error: {output}")))
            }
            Ok(output) => {
                if self.edit_files
                    && let Err(error) = crate::claude::apply_file_tool(&name, input)
                {
                    eprintln!("fake-claude-sdk: {name}: {error}");
                }
                match self.task_tool(&name, input) {
                    Some((said, sidecar)) => (said, false, sidecar),
                    None => {
                        let sidecar = sidecar(&name, input, &output);
                        (output, false, sidecar)
                    }
                }
            }
            Err(refusal) => (refusal.clone(), true, json!(format!("Error: {refusal}"))),
        };
        self.user_result(id, json!(content), is_error, sidecar)
            .await;
    }

    async fn user_result(&mut self, id: &str, content: Value, is_error: bool, sidecar: Value) {
        let mut block = json!({ "type": "tool_result", "tool_use_id": id, "content": content });
        if is_error {
            block["is_error"] = json!(true);
        }
        let frame = json!({
            "type": "user",
            "message": { "role": "user", "content": [block] },
            "parent_tool_use_id": null,
            "session_id": self.session,
            "timestamp": timestamp(),
            "tool_use_result": sidecar,
            "uuid": uuid(),
        });
        self.send(frame).await;
    }

    /// What a task tool says and returns, as Claude's do: a created task
    /// gets the next id; an update reports the status it changed.
    fn task_tool(&mut self, name: &str, input: &Value) -> Option<(String, Value)> {
        let text = |key: &str| input[key].as_str().unwrap_or_default().to_owned();
        match name {
            "TaskCreate" => {
                let id = (self.tasks.len() + 1).to_string();
                let subject = text("subject");
                self.tasks.push((id.clone(), "pending".into()));
                Some((
                    format!("Task #{id} created successfully: {subject}"),
                    json!({ "task": { "id": id, "subject": subject } }),
                ))
            }
            "TaskUpdate" => {
                let id = text("taskId");
                let to = text("status");
                let task = self.tasks.iter_mut().find(|(known, _)| *known == id)?;
                let from = std::mem::replace(&mut task.1, to.clone());
                Some((
                    format!("Updated task #{id} status"),
                    json!({
                        "statusChange": { "from": from, "to": to },
                        "success": true,
                        "taskId": id,
                        "updatedFields": ["status"],
                    }),
                ))
            }
            _ => None,
        }
    }

    /// Send a control request and wait for its response, or for the turn to
    /// be cut short.
    /// An allow that sets the permission mode (an approved plan's way out
    /// of plan mode) switches it, and Claude says so in a status message.
    async fn apply_mode(&mut self, answer: &Value) {
        let response = &answer["response"];
        if response["behavior"] != "allow" {
            return;
        }
        let set = response["updatedPermissions"]
            .as_array()
            .into_iter()
            .flatten()
            .rev()
            .filter(|update| update["type"] == "setMode")
            .find_map(|update| update["mode"].as_str());
        let Some(mode) = set else { return };
        self.mode = mode.to_owned();
        let status = json!({
            "type": "system",
            "subtype": "status",
            "status": null,
            "permissionMode": self.mode,
            "uuid": uuid(),
            "session_id": self.session,
        });
        self.send(status).await;
    }

    async fn request(&mut self, body: Value) -> Option<Value> {
        let id = uuid();
        let call = body["tool_use_id"].as_str().map(str::to_owned);
        self.send(json!({ "type": "control_request", "request_id": id, "request": body }))
            .await;
        loop {
            if let Some(answer) = self.answers.remove(&id) {
                self.apply_mode(&answer).await;
                return Some(answer);
            }
            // An interrupt cancels an open permission request, as Claude
            // does, and refuses the call it was for.
            if self.cut == Some(Cut::Interrupted)
                && let Some(call) = &call
            {
                self.send(json!({ "type": "control_cancel_request", "request_id": id }))
                    .await;
                self.user_result(call, json!(REFUSED), true, json!("User rejected tool use"))
                    .await;
                self.refused_call = true;
                return None;
            }
            if self.cut.is_some() {
                return None;
            }
            if self.eof {
                return None;
            }
            self.pump().await;
        }
    }

    async fn ask(&mut self, request: &str, message: &str, ask: Ask) {
        match ask {
            Ask::Permission(tool) => {
                let (id, input) = self.tool_use(request, message, &tool).await;
                let (name, _) = claude_tool(&tool, &self.cwd);
                let answer = self
                    .request(json!({
                        "subtype": "can_use_tool",
                        "tool_name": name,
                        "display_name": name,
                        "input": input,
                        "tool_use_id": id,
                        "permission_suggestions": [{
                            "type": "addRules",
                            "behavior": "allow",
                            "destination": "localSettings",
                            "rules": [{ "toolName": name }],
                        }],
                    }))
                    .await;
                let Some(answer) = answer else { return };
                let outcome = match allowed(&answer) {
                    Ok(()) => {
                        self.run_tool(&tool).await;
                        Ok(tool.outcome.text())
                    }
                    Err(refusal) => Err(refusal),
                };
                self.tool_result(&id, &tool, &input, outcome).await;
            }
            Ask::Question { questions } => {
                let input =
                    json!({ "questions": questions.iter().map(question).collect::<Vec<_>>() });
                let id = self
                    .named_tool_use(request, message, "AskUserQuestion", &input)
                    .await;
                let answer = self
                    .request(json!({
                        "subtype": "can_use_tool",
                        "tool_name": "AskUserQuestion",
                        "display_name": "AskUserQuestion",
                        "input": input,
                        "tool_use_id": id,
                        "requires_user_interaction": true,
                    }))
                    .await;
                let Some(answer) = answer else { return };
                match allowed(&answer) {
                    Ok(()) => {
                        let updated = &answer["response"]["updatedInput"];
                        let answers = updated["answers"].clone();
                        let said = answers
                            .as_object()
                            .into_iter()
                            .flatten()
                            .map(|(question, answer)| {
                                format!(
                                    "\"{question}\"=\"{}\"",
                                    answer.as_str().unwrap_or_default()
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        let content = format!(
                            "Your questions have been answered: {said}. You can now continue with the user's answers in mind."
                        );
                        let sidecar =
                            json!({ "questions": input["questions"], "answers": answers });
                        self.user_result(&id, json!(content), false, sidecar).await;
                    }
                    Err(refusal) => {
                        self.user_result(
                            &id,
                            json!(refusal),
                            true,
                            json!(format!("Error: {refusal}")),
                        )
                        .await;
                    }
                }
            }
            Ask::Plan { markdown } => {
                let input = json!({ "plan": markdown });
                let id = self
                    .named_tool_use(request, message, "ExitPlanMode", &input)
                    .await;
                let answer = self
                    .request(json!({
                        "subtype": "can_use_tool",
                        "tool_name": "ExitPlanMode",
                        "display_name": "ExitPlanMode",
                        "input": input,
                        "tool_use_id": id,
                        "requires_user_interaction": true,
                    }))
                    .await;
                let Some(answer) = answer else { return };
                match allowed(&answer) {
                    Ok(()) => {
                        let content = format!(
                            "User has approved your plan. You can now start coding.\n\n## Approved Plan:\n{markdown}"
                        );
                        let sidecar = json!({ "plan": markdown, "isAgent": false });
                        self.user_result(&id, json!(content), false, sidecar).await;
                    }
                    Err(refusal) => {
                        self.user_result(
                            &id,
                            json!(refusal),
                            true,
                            json!(format!("Error: {refusal}")),
                        )
                        .await;
                    }
                }
            }
            Ask::Form {
                server,
                message: prompt,
                schema,
            } => {
                let name = format!("mcp__{server}__ask");
                let id = self
                    .named_tool_use(request, message, &name, &json!({}))
                    .await;
                self.schema = Some(schema);
                let answer = self
                    .request(json!({
                        "subtype": "elicitation",
                        "mcp_server_name": server,
                        "message": prompt,
                        "mode": "form",
                        "requested_schema": crate::script::SCHEMA_SLOT,
                    }))
                    .await;
                let Some(answer) = answer else { return };
                let action = answer["response"]["action"]
                    .as_str()
                    .unwrap_or("cancel")
                    .to_owned();
                let mut said = format!("elicitation {action}");
                if let Some(content) = answer["response"].get("content") {
                    said.push(' ');
                    said.push_str(&content.to_string());
                }
                let blocks = json!([{ "type": "text", "text": said }]);
                self.user_result(&id, blocks.clone(), false, blocks).await;
            }
            Ask::Link { .. } | Ask::Grant { .. } | Ask::ToolServerDialog { .. } => {
                unreachable!("refused when the script loaded")
            }
        }
    }

    async fn named_tool_use(
        &mut self,
        request: &str,
        message: &str,
        name: &str,
        input: &Value,
    ) -> String {
        let tool = Tool {
            name: Some(name.to_owned()),
            class: crate::ToolClass::Consequential,
            input: Some(input.clone()),
            outcome: Default::default(),
            wait_for: None,
        };
        self.tool_use(request, message, &tool).await.0
    }

    async fn succeed(&mut self, calls: u32) {
        let frame = json!({
            "type": "result",
            "subtype": "success",
            "api_error_status": null,
            "duration_api_ms": 1,
            "duration_ms": self.turn_ms,
            "fast_mode_disabled_reason": "sdk_opt_in_required",
            "fast_mode_state": "off",
            // Claude 2.1.289 also times each response.
            "first_content_frame_ms": 1,
            "time_to_request_ms": 1,
            "ttft_ms": 1,
            "ttft_stream_ms": 1,
            "is_error": false,
            "modelUsage": self.model_usage(),
            "result_index": 0,
            "subagent_stats": subagent_stats(),
            "num_turns": calls + 1,
            "permission_denials": [],
            "queued_turn_count": self.queue.len(),
            "result": self.last_text,
            "session_id": self.session,
            "stop_reason": "end_turn",
            "terminal_reason": "completed",
            "total_cost_usd": 0.0,
            "usage": result_usage(),
            "uuid": uuid(),
        });
        self.send(frame).await;
    }

    /// Per-model tallies, with the context window, when the script names a
    /// context in use.
    fn model_usage(&self) -> Value {
        match self.context_tokens {
            Some(tokens) => json!({
                self.model.clone(): {
                    "inputTokens": tokens,
                    "outputTokens": 1,
                    "cacheReadInputTokens": 0,
                    "cacheCreationInputTokens": 0,
                    "webSearchRequests": 0,
                    "costUSD": 0.0,
                    "contextWindow": 200000,
                    "maxOutputTokens": 32000,
                },
            }),
            None => json!({}),
        }
    }

    /// Drop the rest of a cut turn's steps.
    fn skip_turn(&mut self) {
        while let Some(step) = crate::script::next_step(&mut self.steps) {
            if step == Step::TurnEnd {
                break;
            }
        }
    }

    /// A `now` message ends the running turn once its call returns, as
    /// Claude does: a successful result whose terminal reason says the turn
    /// was cut, with no interruption marker.
    async fn preempted(&mut self, calls: u32) {
        self.skip_turn();
        let frame = json!({
            "type": "result",
            "subtype": "success",
            "api_error_status": null,
            "duration_api_ms": 1,
            "duration_ms": 1,
            "fast_mode_disabled_reason": "sdk_opt_in_required",
            "fast_mode_state": "off",
            // Claude 2.1.289 also times each response.
            "first_content_frame_ms": 1,
            "time_to_request_ms": 1,
            "ttft_ms": 1,
            "ttft_stream_ms": 1,
            "is_error": false,
            "modelUsage": {},
            "result_index": 0,
            "subagent_stats": subagent_stats(),
            "num_turns": calls + 1,
            "permission_denials": [],
            "queued_turn_count": 0,
            "result": self.last_text,
            "session_id": self.session,
            "stop_reason": "tool_use",
            "terminal_reason": if calls > 0 { "aborted_tools" } else { "aborted_streaming" },
            "total_cost_usd": 0.0,
            "usage": result_usage(),
            "uuid": uuid(),
        });
        self.send(frame).await;
    }

    /// An interrupt cuts the turn short the way Claude does: the
    /// interruption marker, then an error result.
    async fn interrupted(&mut self, calls: u32) {
        self.skip_turn();
        let marker = if std::mem::take(&mut self.refused_call) {
            "[Request interrupted by user for tool use]"
        } else {
            "[Request interrupted by user]"
        };
        let frame = json!({
            "type": "user",
            "message": { "role": "user", "content": [{ "type": "text", "text": marker }] },
            "parent_tool_use_id": null,
            "session_id": self.session,
            "timestamp": timestamp(),
            "uuid": uuid(),
        });
        self.send(frame).await;
        let frame = json!({
            "type": "result",
            "subtype": "error_during_execution",
            "duration_api_ms": 1,
            "duration_ms": 1,
            "errors": [],
            "fast_mode_disabled_reason": "sdk_opt_in_required",
            "fast_mode_state": "off",
            "is_error": true,
            "modelUsage": {},
            "result_index": 0,
            "subagent_stats": subagent_stats(),
            "num_turns": calls + 1,
            "permission_denials": [],
            "queued_turn_count": self.queue.len(),
            "session_id": self.session,
            "stop_reason": null,
            "terminal_reason": if calls > 0 { "aborted_tools" } else { "aborted_streaming" },
            "total_cost_usd": 0.0,
            "usage": turn_usage(),
            "uuid": uuid(),
        });
        self.send(frame).await;
    }
}

/// A message's token tally: `context` tokens in, when the script names a
/// context in use.
fn usage(context: Option<u64>) -> Value {
    json!({
        "cache_creation": { "ephemeral_1h_input_tokens": 0, "ephemeral_5m_input_tokens": 0 },
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0,
        "inference_geo": "not_available",
        "input_tokens": context.unwrap_or(1),
        "output_tokens": 1,
        "service_tier": "standard",
    })
}

/// A turn's token tally, as its result carries it.
pub(crate) fn turn_usage() -> Value {
    let mut usage = usage(None);
    usage["iterations"] = json!([]);
    usage["output_tokens_details"] = json!({ "thinking_tokens": 0 });
    usage["server_tool_use"] = json!({ "web_fetch_requests": 0, "web_search_requests": 0 });
    usage["speed"] = json!("standard");
    usage
}

/// A turn result's usage: the turn's, and the fallback credit Claude
/// 2.1.289 reports on results alone.
fn result_usage() -> Value {
    let mut usage = turn_usage();
    usage["fallback_credit"] = Value::Null;
    usage
}

fn subagent_stats() -> Value {
    json!({
        "by_type": {},
        "completed": 0,
        "failed": 0,
        "killed": { "parent": 0, "system": 0, "user": 0 },
        "max_depth": 0,
        "refused": { "budget": 0, "concurrency_limit": 0, "depth_limit": 0 },
        "requested": { "background": 0, "foreground": 0, "unset": 0 },
        "spawned": 0,
        "spawned_by_subagents": 0,
        "started_in_background": 0,
    })
}

/// What Claude answers a call the person refused.
const REFUSED: &str = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed.";

/// The allow or the refusal text of a `can_use_tool` answer.
fn allowed(answer: &Value) -> Result<(), String> {
    let response = &answer["response"];
    if answer["subtype"] == "success" && response["behavior"] == "allow" {
        return Ok(());
    }
    Err(response["message"]
        .as_str()
        .filter(|message| !message.is_empty())
        .unwrap_or(REFUSED)
        .to_owned())
}

fn question(question: &Question) -> Value {
    json!({
        "question": question.question,
        "header": question.header,
        "multiSelect": question.multi_select,
        "options": question.options.iter().map(|option| {
            let mut offered = json!({
                "label": option.label(),
                "description": option.description(),
            });
            if let Some(preview) = option.preview() {
                offered["preview"] = json!(preview);
            }
            offered
        }).collect::<Vec<_>>(),
    })
}

fn now_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

/// The Claude tool a scripted call names, with its input.
pub fn claude_tool(tool: &Tool, cwd: &str) -> (String, Value) {
    let name = tool.name.clone().unwrap_or_else(|| {
        if tool.is_exploration() {
            "Read".into()
        } else {
            "Bash".into()
        }
    });
    let input = tool.input.clone().unwrap_or_else(|| match name.as_str() {
        "Read" => json!({ "file_path": format!("{cwd}/README.md") }),
        "Bash" => json!({ "command": "true", "description": "Run a command" }),
        "Grep" => json!({ "pattern": "TODO" }),
        "Glob" => json!({ "pattern": "**/*" }),
        _ => json!({}),
    });
    (name, input)
}

/// The `tool_use_result` Claude writes beside a successful call's result.
pub fn sidecar(name: &str, input: &Value, output: &str) -> Value {
    match name {
        "Bash" => json!({
            "stdout": output,
            "stderr": "",
            "interrupted": false,
            "isImage": false,
            "noOutputExpected": false,
        }),
        "Read" => json!({
            "type": "text",
            "file": {
                "filePath": input["file_path"],
                "content": output,
                "numLines": output.lines().count(),
                "startLine": 1,
                "totalLines": output.lines().count(),
            },
        }),
        _ => json!(output),
    }
}
