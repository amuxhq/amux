//! `fake-codex`: `codex app-server` speaking JSON-RPC over stdio or a Unix socket.
//!
//! Modelled on codex-cli 0.157.0: the host initializes, starts or resumes a
//! thread and starts turns; a turn reports its items as started, streamed
//! and completed notifications and ends with `turn/completed`. Asks are
//! server requests (command, file-change and access approvals, questions,
//! tool-server forms and links) that block the turn until the host
//! answers. `thread/inject_items` is acknowledged at once and reports
//! nothing: written while a turn runs, the turn drains it and goes on;
//! written to an idle thread, it waits for the next `turn/start`.
//! `turn/steer` adds the host's input to the running turn as a user
//! message; `turn/interrupt` ends the turn as interrupted.
//!
//! On a Unix socket (`--listen unix://PATH`) it serves any number of
//! clients, as Codex does. A client joins the thread by starting or
//! resuming it, and from then on gets every notification and server
//! request; any joined client may answer a request, and all are told when
//! it is resolved. A client that leaves takes nothing with it: the turn
//! goes on and its pending request stays open for the others, and one that
//! joins later is sent the requests still pending. A second client's resume
//! must find the thread: a plain resume once it has been named or has run a
//! turn, and Codex's own app, which resumes without the turns
//! (`excludeTurns`) and pages them from the history on disk, only once that
//! history is written. Codex writes it at the first turn, or when a client
//! reads the loaded thread with its turns (`thread/read` with
//! `includeTurns`) or resumes it with them. Asked to finish (SIGTERM), it
//! finishes the running turn and exits, as a stdio server does at the end of
//! its input; a script's `exit_ms` holds it that much longer first.
//!
//! A server that starts or resumes a thread is that thread's writer for as
//! long as it runs, across processes, as Codex is: it holds a lock on the
//! thread under its Codex home (`CODEX_HOME`, or a folder in the system's
//! temporary directory, never the working directory, whose changes a chat's
//! review shows). Another server's resume of the thread while the writer lives
//! is refused with Codex's own error, `thread <id> already has an active
//! writer` (codex-cli 0.161.0, recorded in a QA run).

use std::collections::{BTreeMap, VecDeque};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::clients::{ClientId, Event, Listen, Writer};
use crate::playback::{Channel, Process};
use crate::script::{Ask, OfferedCommand, OfferedModel, Question, Script, Step, Tool};
use crate::{DRIFT_EXIT, Mode};

/// The asks Codex can raise.
pub const RAISES: &[&str] = &["permission", "question", "plan", "form", "link", "grant"];
/// The provider-specific steps Codex can play.
pub const PLAYS: &[&str] = &["usage", "auth_failed"];

/// The Codex version the fake reports: the newest the corpus shows.
pub const VERSION: &str = "0.157.0";

pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--version") {
        println!("codex-cli {}", crate::scripted_version(VERSION));
        return 0;
    }
    if let Some(thread) = crate::codex_view::resumed_thread(&args) {
        let Some(remote) = crate::codex_view::remote(&args) else {
            eprintln!("fake-codex: resume needs --remote");
            return DRIFT_EXIT;
        };
        return crate::codex_view::run(thread, remote);
    }
    let mode = match crate::mode_from_env() {
        Ok(mode) => mode,
        Err(error) => {
            eprintln!("fake-codex: {error}");
            return DRIFT_EXIT;
        }
    };
    let listen = match Listen::from_args(&args) {
        Ok(listen) => listen,
        Err(error) => {
            eprintln!("fake-codex: {error}");
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
                let played = match crate::clients::serve(&listen).await {
                    Ok(events) => play(&process, events).await,
                    Err(error) => Err(error.to_string()),
                };
                match played {
                    Ok(code) => code.unwrap_or(0),
                    Err(error) => {
                        eprintln!("fake-codex: {error}");
                        DRIFT_EXIT
                    }
                }
            }
            Mode::Script(script) => {
                if let Err(error) = script.check("Codex", RAISES, PLAYS) {
                    eprintln!("fake-codex: {error}");
                    return DRIFT_EXIT;
                }
                if reaches_a_limit(&script.steps) {
                    // No recording shows how Codex reports a reached limit.
                    eprintln!("fake-codex: Codex can be scripted near a usage limit, not past it");
                    return DRIFT_EXIT;
                }
                match crate::clients::serve(&listen).await {
                    Ok(events) => Engine::new(script, listen, events).run().await,
                    Err(error) => {
                        eprintln!("fake-codex: {error}");
                        DRIFT_EXIT
                    }
                }
            }
        }
    });
    // The stdin reader blocks in a thread the runtime would wait for on
    // drop, so a script's exit would wait for the host to close stdin.
    runtime.shutdown_background();
    code
}

/// Play one recorded process to the first client to connect: write each
/// recorded output, and require each recorded input from that client, byte
/// for byte. After the last event the client must leave without writing
/// more; a recorded exit instead ends the process there with its code.
async fn play(
    process: &Process,
    mut events: mpsc::UnboundedReceiver<Event>,
) -> Result<Option<i32>, String> {
    let (client, mut writer) = loop {
        match events.recv().await {
            Some(Event::Opened(id, writer)) => break (id, writer),
            Some(Event::Terminated) | None => return Err("no client connected".into()),
            Some(_) => {}
        }
    };
    for (index, event) in process.events.iter().enumerate() {
        match event.channel {
            Channel::Output => writer
                .write(&String::from_utf8_lossy(&event.bytes))
                .await
                .map_err(|error| format!("writing event {index}: {error}"))?,
            Channel::Input => {
                let line = next_frame(&mut events, client)
                    .await
                    .ok_or_else(|| format!("input closed before event {index}"))?;
                if line.as_bytes() != event.bytes {
                    return Err(format!(
                        "event {index}: host wrote\n  {line}\nrecording has\n  {}",
                        String::from_utf8_lossy(&event.bytes)
                    ));
                }
            }
            Channel::Exit => return Ok(Some(crate::playback::exit_code(event))),
            Channel::Transcript | Channel::Hook => {
                return Err(format!(
                    "event {index}: Codex has no {:?} channel",
                    event.channel
                ));
            }
        }
    }
    match next_frame(&mut events, client).await {
        None => Ok(None),
        Some(line) => Err(format!("host wrote past the recording's end: {line}")),
    }
}

/// The next message `client` writes; None once it has left.
async fn next_frame(
    events: &mut mpsc::UnboundedReceiver<Event>,
    client: ClientId,
) -> Option<String> {
    loop {
        match events.recv().await? {
            Event::Frame(id, text) if id == client => return Some(text),
            Event::Closed(id) if id == client => return None,
            Event::Terminated => return None,
            _ => {}
        }
    }
}

/// Why a server could not become a thread's writer.
enum WriterError {
    /// Another live server is the thread's writer.
    Held,
    Io(std::io::Error),
}

/// One connected client, and whether it has joined the thread.
struct Client {
    writer: Writer,
    joined: bool,
}

struct Engine {
    listen: Listen,
    events: mpsc::UnboundedReceiver<Event>,
    clients: BTreeMap<ClientId, Client>,
    eof: bool,
    steps: VecDeque<Step>,
    model: String,
    /// What `model/list` and `skills/list` answer.
    models: Vec<OfferedModel>,
    skills: Vec<OfferedCommand>,
    cwd: String,
    thread: Option<String>,
    /// Whether a plain resume by another client finds the thread.
    materialized: bool,
    /// Whether the thread's history is on disk, which a resume without its
    /// turns needs.
    persisted: bool,
    name: Option<String>,
    /// The running turn's id.
    turn: Option<String>,
    /// `turn/start` requests waiting to run: who asked, the id and input.
    starts: VecDeque<(ClientId, Value, Value)>,
    interrupted: bool,
    /// Our server requests still waiting for an answer, as sent, by id.
    pending: BTreeMap<String, String>,
    /// Responses to our server requests, by id.
    answers: BTreeMap<String, Value>,
    next_request: u64,
    next_item: u64,
    last_message: Option<Value>,
    /// The tool servers the launch configured.
    servers: crate::mcp::ToolServers,
    edit_files: bool,
    chunk_ms: u64,
    /// A form schema the next frame carries, as the script wrote it.
    schema: Option<crate::script::Schema>,
    /// `account/read` answers no account.
    signed_out: bool,
    /// How long a server asked to finish lingers before it exits.
    exit_ms: u64,
    /// Asked to finish by SIGTERM.
    terminated: bool,
    /// The lock that makes this process its thread's writer, held until it
    /// exits.
    writer: Option<std::fs::File>,
    /// Commands run in the background that still run, as (turn, item),
    /// each carrying its process id.
    terminals: Vec<(String, Value)>,
}

impl Engine {
    fn new(script: Script, listen: Listen, events: mpsc::UnboundedReceiver<Event>) -> Self {
        Self {
            listen,
            events,
            clients: BTreeMap::new(),
            eof: false,
            steps: script.steps.into(),
            models: OfferedModel::offered(
                &script.models,
                script.model.as_deref().unwrap_or("gpt-fake"),
            ),
            skills: script.commands,
            model: script.model.unwrap_or_else(|| "gpt-fake".into()),
            cwd: std::env::current_dir()
                .map(|dir| dir.display().to_string())
                .unwrap_or_default(),
            thread: None,
            materialized: false,
            persisted: false,
            name: None,
            turn: None,
            starts: VecDeque::new(),
            interrupted: false,
            pending: BTreeMap::new(),
            answers: BTreeMap::new(),
            next_request: 0,
            next_item: 0,
            last_message: None,
            servers: crate::mcp::ToolServers::from_codex(
                &std::env::args().skip(1).collect::<Vec<_>>(),
            ),
            edit_files: script.edit_files,
            chunk_ms: script.chunk_ms,
            schema: None,
            signed_out: script.signed_out,
            exit_ms: script.exit_ms,
            terminated: false,
            writer: None,
            terminals: Vec::new(),
        }
    }

    async fn run(mut self) -> i32 {
        loop {
            if let Some((client, id, params)) = self.starts.pop_front() {
                if let Some(code) = self.run_turn(client, id, params).await {
                    return code;
                }
                continue;
            }
            if self.eof {
                if self.terminated && self.exit_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(self.exit_ms)).await;
                }
                return 0;
            }
            let event = self.events.recv().await;
            self.receive(event).await;
        }
    }

    async fn receive(&mut self, event: Option<Event>) {
        match event {
            None => self.eof = true,
            Some(Event::Opened(id, writer)) => {
                // A stdio host is the only client there will be.
                let joined = self.listen == Listen::Stdio;
                self.clients.insert(id, Client { writer, joined });
            }
            Some(Event::Frame(id, line)) => {
                crate::script::log_input(&line);
                match serde_json::from_str::<Value>(&line) {
                    Ok(frame) => self.handle(id, frame).await,
                    Err(error) => eprintln!("fake-codex: unreadable input {line}: {error}"),
                }
            }
            // A stdio host that closes its input still reads the output:
            // the process goes on to the script's end, then exits.
            Some(Event::Closed(_)) if self.listen == Listen::Stdio => self.eof = true,
            Some(Event::Closed(id)) => {
                self.clients.remove(&id);
            }
            Some(Event::Terminated) => {
                self.terminated = true;
                self.eof = true;
            }
        }
    }

    /// A frame's text. A form schema waits for the frame that carries it.
    fn compose(&mut self, frame: &Value) -> String {
        let carries = self
            .schema
            .as_ref()
            .is_some_and(|_| frame.to_string().contains(crate::script::SCHEMA_SLOT));
        match self.schema.take() {
            Some(schema) if carries => crate::lines::with_schema(frame, &schema),
            kept => {
                self.schema = kept;
                serde_json::to_string(frame).expect("JSON values serialise")
            }
        }
    }

    /// Sends a notification or server request to every joined client.
    async fn send(&mut self, frame: Value) {
        let text = self.compose(&frame);
        self.broadcast(&text).await;
    }

    async fn broadcast(&mut self, text: &str) {
        let mut gone = Vec::new();
        for (id, client) in &mut self.clients {
            if client.joined && client.writer.write(text).await.is_err() {
                gone.push(*id);
            }
        }
        self.lost(gone);
    }

    /// Sends a response to the one client that asked.
    async fn reply(&mut self, client: ClientId, frame: Value) {
        let text = self.compose(&frame);
        let Some(to) = self.clients.get_mut(&client) else {
            return;
        };
        if to.writer.write(&text).await.is_err() {
            self.lost(vec![client]);
        }
    }

    fn lost(&mut self, gone: Vec<ClientId>) {
        // A stdio host that stopped reading has gone for good.
        if !gone.is_empty() && self.listen == Listen::Stdio {
            std::process::exit(0);
        }
        for id in gone {
            self.clients.remove(&id);
        }
    }

    async fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "method": method, "params": params, "emittedAtMs": now_ms() }))
            .await;
    }

    async fn respond(&mut self, client: ClientId, id: &Value, result: Value) {
        self.reply(client, json!({ "id": id, "result": result }))
            .await;
    }

    async fn refuse(&mut self, client: ClientId, id: &Value, code: i64, message: &str) {
        self.reply(
            client,
            json!({ "id": id, "error": { "code": code, "message": message } }),
        )
        .await;
    }

    async fn drain(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(event) => self.receive(Some(event)).await,
                Err(mpsc::error::TryRecvError::Empty) => return,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.eof = true;
                    return;
                }
            }
        }
    }

    async fn pump(&mut self) {
        if self.eof {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            return;
        }
        tokio::select! {
            event = self.events.recv() => self.receive(event).await,
            () = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }
    }

    /// Becomes `thread`'s writer, unless another live server is.
    fn take_writer(&self, thread: &str) -> Result<std::fs::File, WriterError> {
        let home = std::env::var_os("CODEX_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("amux-fake-codex"));
        let dir = home.join("writers");
        std::fs::create_dir_all(&dir).map_err(WriterError::Io)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("{thread}.lock")))
            .map_err(WriterError::Io)?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => Err(WriterError::Held),
            Err(std::fs::TryLockError::Error(error)) => Err(WriterError::Io(error)),
        }
    }

    fn thread_id(&self) -> String {
        self.thread.clone().unwrap_or_default()
    }

    async fn handle(&mut self, client: ClientId, frame: Value) {
        let Some(method) = frame["method"].as_str().map(str::to_owned) else {
            // A response to one of our requests: the first answer wins.
            if let Some(id) = frame.get("id").map(key)
                && self.pending.contains_key(&id)
            {
                self.answers.entry(id).or_insert(frame);
            }
            return;
        };
        let Some(id) = frame.get("id").cloned() else {
            // `initialized` and other notifications need nothing.
            return;
        };
        let params = &frame["params"];
        match method.as_str() {
            "initialize" => {
                let result = json!({
                    "codexHome": format!("{}/.codex", self.cwd),
                    "platformFamily": "unix",
                    "platformOs": std::env::consts::OS,
                    "userAgent": format!("fake-codex/{VERSION}"),
                });
                self.respond(client, &id, result).await;
            }
            "thread/resume" if self.thread.is_some() => self.join(client, &id, params).await,
            "thread/start" | "thread/resume" => {
                if let Some(model) = params["model"].as_str() {
                    self.model = model.to_owned();
                }
                if let Some(cwd) = params["cwd"].as_str() {
                    self.cwd = cwd.to_owned();
                }
                let thread = params["threadId"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                match self.take_writer(&thread) {
                    Ok(writer) => self.writer = Some(writer),
                    Err(WriterError::Held) => {
                        let message = format!("thread {thread} already has an active writer");
                        self.refuse(client, &id, -32600, &message).await;
                        return;
                    }
                    Err(WriterError::Io(error)) => {
                        eprintln!("fake-codex: the thread's writer lock: {error}");
                    }
                }
                self.thread = Some(thread);
                // A resumed thread was read from disk.
                self.materialized = method == "thread/resume";
                self.persisted = self.materialized;
                let result = if method == "thread/start" {
                    self.thread_result(params)
                } else {
                    self.resume_result(params)
                };
                self.join_thread(client);
                self.respond(client, &id, result).await;
                if method == "thread/start" {
                    let thread = self.thread_value();
                    self.notify("thread/started", json!({ "thread": thread }))
                        .await;
                }
            }
            "turn/start" => {
                if self.turn.is_some() {
                    self.refuse(client, &id, -32600, "a turn is already running")
                        .await;
                } else {
                    self.starts.push_back((client, id, params.clone()));
                }
            }
            "turn/steer" => {
                let expected = params["expectedTurnId"].as_str();
                match self.turn.clone() {
                    Some(turn) if expected.is_none_or(|expected| expected == turn) => {
                        self.respond(client, &id, json!({ "turnId": turn })).await;
                        self.user_message(&turn, params).await;
                    }
                    _ => {
                        self.refuse(client, &id, -32600, "no active turn to steer")
                            .await
                    }
                }
            }
            "turn/interrupt" => {
                if self.turn.is_some() {
                    self.interrupted = true;
                }
                self.respond(client, &id, json!({})).await;
            }
            // Drained by a running turn, held for the next by an idle one;
            // either way nothing is reported.
            "thread/inject_items" => self.respond(client, &id, json!({})).await,
            "thread/backgroundTerminals/list" => {
                let data: Vec<Value> = self
                    .terminals
                    .iter()
                    .map(|(_, item)| {
                        json!({
                            "itemId": item["id"], "processId": item["processId"],
                            "command": item["command"], "cwd": item["cwd"],
                        })
                    })
                    .collect();
                self.respond(client, &id, json!({ "data": data, "nextCursor": null }))
                    .await;
            }
            // Codex ends the command by its process id; the command's item
            // completes as one killed does.
            "thread/backgroundTerminals/terminate" => {
                let at = self
                    .terminals
                    .iter()
                    .position(|(_, item)| item["processId"] == params["processId"]);
                self.respond(client, &id, json!({ "terminated": at.is_some() }))
                    .await;
                if let Some(at) = at {
                    let (turn, mut item) = self.terminals.remove(at);
                    item["status"] = json!("failed");
                    item["exitCode"] = json!(143);
                    item["durationMs"] = json!(0);
                    item["aggregatedOutput"] = json!("");
                    self.item(&turn, "item/completed", &item).await;
                }
            }
            // Read with its turns, the loaded thread is written to disk.
            "thread/read" if self.thread.as_deref() == params["threadId"].as_str() => {
                if params["includeTurns"] == true {
                    self.persisted = true;
                }
                let thread = self.thread_value();
                self.respond(client, &id, json!({ "thread": thread })).await;
            }
            "thread/compact/start" => {
                self.respond(client, &id, json!({})).await;
                self.compact().await;
            }
            "thread/name/set" => {
                self.respond(client, &id, json!({})).await;
                self.materialized = true;
                self.name = params["name"].as_str().map(str::to_owned);
                let thread = self.thread_id();
                self.notify(
                    "thread/name/updated",
                    json!({ "threadId": thread, "threadName": params["name"] }),
                )
                .await;
            }
            // One page: the shapes codex-cli 0.157.0 answers with, the
            // fields no host reads left out.
            "model/list" => {
                let data = self
                    .models
                    .iter()
                    .map(|model| {
                        json!({
                            "id": model.value,
                            "model": model.value,
                            "displayName": model.display_name(),
                            "description": model.description,
                            "hidden": false,
                            "supportedReasoningEfforts": model
                                .efforts
                                .iter()
                                .map(|effort| json!({ "reasoningEffort": effort, "description": "" }))
                                .collect::<Vec<_>>(),
                            // Codex always names one.
                            "defaultReasoningEffort": model
                                .default_effort
                                .as_deref()
                                .or(model.efforts.first().map(String::as_str))
                                .unwrap_or("medium"),
                            "isDefault": model.value == self.model,
                        })
                    })
                    .collect::<Vec<_>>();
                self.respond(client, &id, json!({ "data": data, "nextCursor": null }))
                    .await
            }
            "skills/list" => {
                let skills = self
                    .skills
                    .iter()
                    .map(|skill| {
                        json!({
                            "name": skill.name,
                            "description": skill.description,
                            "path": format!("{}/.codex/skills/{}/SKILL.md", self.cwd, skill.name),
                            "scope": "user",
                            "enabled": true,
                            "pluginId": null,
                        })
                    })
                    .collect::<Vec<_>>();
                let folder = json!({ "cwd": self.cwd, "skills": skills, "errors": [] });
                self.respond(client, &id, json!({ "data": [folder] })).await
            }
            "account/read" => {
                let account = if self.signed_out {
                    Value::Null
                } else {
                    json!({ "type": "chatgpt", "planType": "pro" })
                };
                self.respond(
                    client,
                    &id,
                    json!({ "account": account, "requiresOpenaiAuth": true }),
                )
                .await
            }
            other => {
                self.refuse(client, &id, -32601, &format!("method not found: {other}"))
                    .await
            }
        }
    }

    fn thread_result(&self, params: &Value) -> Value {
        json!({
            "activePermissionProfile": null,
            "approvalPolicy": params["approvalPolicy"].as_str().unwrap_or("on-request"),
            "approvalsReviewer": "user",
            "cwd": self.cwd,
            "disabledPluginIds": [],
            "instructionSources": [],
            "model": self.model,
            "modelProvider": "openai",
            "multiAgentMode": "explicitRequestOnly",
            "reasoningEffort": null,
            "runtimeWorkspaceRoots": [self.cwd],
            "sandbox": {
                "excludeSlashTmp": false,
                "excludeTmpdirEnvVar": false,
                "networkAccess": false,
                "type": "workspaceWrite",
                "writableRoots": [],
            },
            "serviceTier": null,
            "thread": self.thread_value(),
        })
    }

    /// Codex 0.160.0 answers a resume as it answers a start, with the
    /// thread's history cursors and its collaboration mode besides.
    fn resume_result(&self, params: &Value) -> Value {
        let mut result = self.thread_result(params);
        let fields = result.as_object_mut().expect("an object");
        for cursor in [
            "initialTurnsPage",
            "itemsBackwardsCursor",
            "turnsBackwardsCursor",
        ] {
            fields.insert(cursor.into(), Value::Null);
        }
        fields.insert(
            "collaborationMode".into(),
            json!({
                "mode": "default",
                "settings": {
                    "developer_instructions": null,
                    "model": self.model,
                    "reasoning_effort": null,
                },
            }),
        );
        result
    }

    fn join_thread(&mut self, client: ClientId) {
        if let Some(joining) = self.clients.get_mut(&client) {
            joining.joined = true;
        }
    }

    /// Another client resumes the loaded thread: it joins the thread as it
    /// runs and is sent the requests still waiting for an answer.
    async fn join(&mut self, client: ClientId, id: &Value, params: &Value) {
        let asked = params["threadId"].as_str().unwrap_or_default();
        if self.thread.as_deref() != Some(asked) || !self.materialized {
            let message = format!("no rollout found for thread id {asked}");
            return self.refuse(client, id, -32600, &message).await;
        }
        if params["excludeTurns"] == true && !self.persisted {
            let message =
                format!("invalid paginated history lineage for {asked}: missing source rollout");
            return self.refuse(client, id, -32600, &message).await;
        }
        // Resumed with its turns, the thread is written out to give them.
        self.persisted = true;
        let result = self.resume_result(params);
        self.join_thread(client);
        self.respond(client, id, result).await;
        let pending: Vec<String> = self.pending.values().cloned().collect();
        for request in pending {
            if let Some(to) = self.clients.get_mut(&client)
                && to.writer.write(&request).await.is_err()
            {
                self.lost(vec![client]);
            }
        }
    }

    fn thread_value(&self) -> Value {
        let now = now_ms() / 1000;
        let thread = self.thread_id();
        json!({
            "agentNickname": null,
            "agentRole": null,
            "canAcceptDirectInput": true,
            "cliVersion": VERSION,
            "createdAt": now,
            "cwd": self.cwd,
            "daybreakEnabled": null,
            "environments": [{
                "cwd": self.cwd,
                "environmentId": "local",
                "runtimeWorkspaceRoots": [self.cwd],
            }],
            "ephemeral": false,
            "extra": null,
            "forkedFromId": null,
            "gitInfo": null,
            "historyMode": "paginated",
            "id": thread,
            "model": self.model,
            "modelProvider": "openai",
            "name": self.name,
            "originator": "fake-codex",
            "parentThreadId": null,
            "path": format!("{}/.codex/sessions/{thread}.jsonl", self.cwd),
            "preview": "",
            "projectId": null,
            "reasoningEffort": null,
            "recencyAt": now,
            "section": null,
            "sectionEnteredAt": null,
            "sessionId": thread,
            "source": "vscode",
            "status": { "type": "idle" },
            "threadSource": null,
            "turns": [],
            "updatedAt": now,
        })
    }

    fn turn_value(&self, turn: &str, status: &str, items: Vec<Value>) -> Value {
        let done = status != "inProgress";
        json!({
            "completedAt": if done { json!(now_ms() / 1000) } else { Value::Null },
            "durationMs": if done { json!(1) } else { Value::Null },
            "error": null,
            "id": turn,
            "items": items,
            "itemsView": if done && status == "completed" { "summary" } else { "notLoaded" },
            "startedAt": now_ms() / 1000,
            "status": status,
        })
    }

    fn item_id(&mut self, prefix: &str) -> String {
        self.next_item += 1;
        crate::claude::numbered(prefix, self.next_item)
    }

    async fn item(&mut self, turn: &str, phase: &str, item: &Value) {
        let thread = self.thread_id();
        let mut params = json!({ "item": item, "threadId": thread, "turnId": turn });
        if phase == "item/started" {
            params["startedAtMs"] = json!(now_ms());
        } else {
            params["completedAtMs"] = json!(now_ms());
        }
        self.notify(phase, params).await;
    }

    /// The user message a turn start or steer reports, echoing the client
    /// message id it was sent with, as Codex does.
    async fn user_message(&mut self, turn: &str, params: &Value) {
        let content: Vec<Value> = params["input"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|part| {
                let mut part = part.clone();
                if part.get("text_elements").is_none() && part["type"] == "text" {
                    part["text_elements"] = json!([]);
                }
                part
            })
            .collect();
        if content.is_empty() {
            return;
        }
        let item = json!({
            "clientId": params["clientUserMessageId"].as_str(),
            "content": content,
            "id": self.item_id("user_"),
            "type": "userMessage",
        });
        self.item(turn, "item/started", &item).await;
        self.item(turn, "item/completed", &item).await;
    }

    async fn status(&mut self, active: Option<&[&str]>) {
        let status = match active {
            Some(flags) => json!({ "type": "active", "activeFlags": flags }),
            None => json!({ "type": "idle" }),
        };
        let thread = self.thread_id();
        self.notify(
            "thread/status/changed",
            json!({ "status": status, "threadId": thread }),
        )
        .await;
    }

    async fn compact(&mut self) {
        let turn = uuid::Uuid::new_v4().to_string();
        let thread = self.thread_id();
        self.status(Some(&[])).await;
        let started = self.turn_value(&turn, "inProgress", vec![]);
        self.notify(
            "turn/started",
            json!({ "threadId": thread, "turn": started }),
        )
        .await;
        let item = json!({ "id": self.item_id("compact_"), "type": "contextCompaction" });
        self.item(&turn, "item/started", &item).await;
        self.item(&turn, "item/completed", &item).await;
        self.status(None).await;
        let mut done = self.turn_value(&turn, "completed", vec![]);
        done["itemsView"] = json!("notLoaded");
        self.notify(
            "turn/completed",
            json!({ "threadId": thread, "turn": done }),
        )
        .await;
    }

    /// Run a turn. `Some(code)` ends the process.
    async fn run_turn(&mut self, client: ClientId, id: Value, params: Value) -> Option<i32> {
        let turn = uuid::Uuid::new_v4().to_string();
        let thread = self.thread_id();
        self.turn = Some(turn.clone());
        self.materialized = true;
        self.persisted = true;
        self.interrupted = false;
        self.last_message = None;
        if let Some(mode) = params
            .get("collaborationMode")
            .filter(|mode| !mode.is_null())
        {
            self.settings(mode).await;
        }
        let started = self.turn_value(&turn, "inProgress", vec![]);
        let mut response = started.clone();
        response["startedAt"] = Value::Null;
        self.respond(client, &id, json!({ "turn": response })).await;
        self.status(Some(&[])).await;
        self.notify(
            "turn/started",
            json!({ "threadId": thread, "turn": started }),
        )
        .await;
        self.user_message(&turn, &params).await;
        let exit = loop {
            self.drain().await;
            if self.interrupted {
                break None;
            }
            let Some(step) = crate::script::next_step(&mut self.steps) else {
                break None;
            };
            match step {
                Step::Text { chunks } => self.text(&turn, &chunks).await,
                Step::Thinking { text } => self.reasoning(&turn, &text).await,
                Step::Tool(tool) => self.tool(&turn, &tool, None).await,
                Step::Ask(ask) => self.ask(&turn, ask).await,
                Step::WaitFor { path } => {
                    while !path.exists() && !self.interrupted {
                        self.pump().await;
                    }
                }
                Step::Spawn { command } => crate::script::spawn_detached(&command),
                Step::Pause { ms } => {
                    let until = tokio::time::Instant::now() + std::time::Duration::from_millis(ms);
                    while tokio::time::Instant::now() < until && !self.interrupted {
                        self.pump().await;
                    }
                }
                Step::TurnEnd => break None,
                Step::Exit { code } => break Some(code),
                Step::Usage(usage) => self.rate_limits(&usage).await,
                Step::AuthFailed { message } => {
                    self.unauthorized(&turn, &message).await;
                    let mut failed = self.turn_value(&turn, "failed", vec![]);
                    failed["error"] = json!({
                        "additionalDetails": null,
                        "codexErrorInfo": "other",
                        "message": message,
                        "misalignment": null,
                    });
                    self.status(None).await;
                    self.notify(
                        "turn/completed",
                        json!({ "threadId": thread, "turn": failed }),
                    )
                    .await;
                    self.turn = None;
                    return None;
                }
                Step::Repeat { .. } => unreachable!("next_step unrolls repeats"),
            }
        };
        if exit.is_some() {
            return exit;
        }
        let status = if self.interrupted {
            while let Some(step) = crate::script::next_step(&mut self.steps) {
                if step == Step::TurnEnd {
                    break;
                }
            }
            "interrupted"
        } else {
            let usage = token_usage();
            self.notify(
                "thread/tokenUsage/updated",
                json!({ "threadId": thread, "turnId": turn, "tokenUsage": usage }),
            )
            .await;
            "completed"
        };
        self.status(None).await;
        let items = if status == "completed" {
            self.last_message.clone().into_iter().collect()
        } else {
            vec![]
        };
        let done = self.turn_value(&turn, status, items);
        self.notify(
            "turn/completed",
            json!({ "threadId": thread, "turn": done }),
        )
        .await;
        self.turn = None;
        None
    }

    /// A turn that names a collaboration mode changes the thread's settings
    /// first, and Codex reports the settings it now runs with.
    async fn settings(&mut self, mode: &Value) {
        let mut mode = mode.clone();
        if mode["settings"]["developer_instructions"].is_null() {
            mode["settings"]["developer_instructions"] = json!("# Plan Mode (scripted)\n");
        }
        let thread = self.thread_id();
        let settings = json!({
            "activePermissionProfile": null,
            "approvalPolicy": "never",
            "approvalsReviewer": "user",
            "collaborationMode": mode,
            "cwd": self.cwd,
            "disabledPluginIds": [],
            "effort": null,
            "model": self.model,
            "modelProvider": "openai",
            "multiAgentMode": "explicitRequestOnly",
            "personality": null,
            "sandboxPolicy": {
                "excludeSlashTmp": false,
                "excludeTmpdirEnvVar": false,
                "networkAccess": false,
                "type": "workspaceWrite",
                "writableRoots": [],
            },
            "serviceTier": null,
            "summary": null,
        });
        self.notify(
            "thread/settings/updated",
            json!({ "threadId": thread, "threadSettings": settings }),
        )
        .await;
    }

    /// Reasoning with its summary streamed as one part.
    async fn reasoning(&mut self, turn: &str, text: &str) {
        let id = self.item_id("rs_");
        let thread = self.thread_id();
        let mut item = json!({ "content": [], "id": id, "summary": [], "type": "reasoning" });
        self.item(turn, "item/started", &item).await;
        let at = json!({ "itemId": id, "summaryIndex": 0, "threadId": thread, "turnId": turn });
        self.notify("item/reasoning/summaryPartAdded", at.clone())
            .await;
        let mut delta = at;
        delta["delta"] = json!(text);
        self.notify("item/reasoning/summaryTextDelta", delta).await;
        item["summary"] = json!([text]);
        self.item(turn, "item/completed", &item).await;
    }

    /// The account's usage limits, as Codex reports them between items:
    /// the first window as its primary, the second as its secondary.
    async fn rate_limits(&mut self, usage: &crate::script::Usage) {
        let now = now_ms() / 1000;
        let window = |at: usize| {
            usage.windows.get(at).map_or(Value::Null, |window| {
                json!({
                    "resetsAt": now + window.resets_in_s,
                    "usedPercent": window.used_percent.round() as i64,
                    "windowDurationMins": if window.name == "five_hour" { 300 } else { 10080 },
                })
            })
        };
        let reached = (usage.status == "rejected").then_some("primary");
        self.notify(
            "account/rateLimits/updated",
            json!({ "rateLimits": {
                "credits": { "balance": "0", "hasCredits": false, "unlimited": false },
                "individualLimit": null,
                "limitId": "codex",
                "limitName": null,
                "normalModelSlug": null,
                "planType": "pro",
                "primary": window(0),
                "rateLimitReachedType": reached,
                "secondary": window(1),
                "spendControlReached": null,
            }}),
        )
        .await;
    }

    /// A refused credential, as Codex reports it: reconnects that fail with
    /// 401, the thread's system error, and the error that ends the turn.
    async fn unauthorized(&mut self, turn: &str, message: &str) {
        let thread = self.thread_id();
        for attempt in 1..=2 {
            self.notify(
                "error",
                json!({
                    "error": {
                        "additionalDetails": message,
                        "codexErrorInfo": { "responseStreamDisconnected": { "httpStatusCode": 401 } },
                        "message": format!("Reconnecting... {attempt}/5"),
                        "misalignment": null,
                    },
                    "threadId": thread,
                    "turnId": turn,
                    "willRetry": true,
                }),
            )
            .await;
        }
        self.notify(
            "thread/status/changed",
            json!({ "status": { "type": "systemError" }, "threadId": thread }),
        )
        .await;
        self.notify(
            "error",
            json!({
                "error": {
                    "additionalDetails": null,
                    "codexErrorInfo": "other",
                    "message": message,
                    "misalignment": null,
                },
                "threadId": thread,
                "turnId": turn,
                "willRetry": false,
            }),
        )
        .await;
    }

    async fn text(&mut self, turn: &str, chunks: &[String]) {
        let id = self.item_id("msg_");
        let thread = self.thread_id();
        let mut item = json!({
            "delivery": null,
            "id": id,
            "memoryCitation": null,
            "phase": "final_answer",
            "questions": null,
            "text": "",
            "type": "agentMessage",
        });
        self.item(turn, "item/started", &item).await;
        for (at, chunk) in chunks.iter().enumerate() {
            if at > 0 && self.chunk_ms > 0 {
                let until =
                    tokio::time::Instant::now() + std::time::Duration::from_millis(self.chunk_ms);
                while tokio::time::Instant::now() < until && !self.interrupted {
                    self.pump().await;
                }
            }
            self.notify(
                "item/agentMessage/delta",
                json!({ "delta": chunk, "itemId": id, "threadId": thread, "turnId": turn }),
            )
            .await;
        }
        item["text"] = json!(chunks.concat());
        self.item(turn, "item/completed", &item).await;
        self.last_message = Some(item);
    }

    /// Send a server request and wait for its answer, or the turn's end.
    async fn request(&mut self, method: &str, params: Value) -> Option<Value> {
        let id = self.next_request;
        self.next_request += 1;
        let request = self.compose(&json!({ "id": id, "method": method, "params": params }));
        self.pending.insert(id.to_string(), request.clone());
        self.broadcast(&request).await;
        let answer = loop {
            if let Some(answer) = self.answers.remove(&id.to_string()) {
                break Some(answer);
            }
            if self.interrupted || self.eof {
                break None;
            }
            self.pump().await;
        };
        self.pending.remove(&id.to_string());
        let thread = self.thread_id();
        self.notify(
            "serverRequest/resolved",
            json!({ "requestId": id, "threadId": thread }),
        )
        .await;
        answer
    }

    /// A call: a command, or a file change when named `apply_patch`. With
    /// `approval` the call waits for the host's decision first.
    async fn tool(&mut self, turn: &str, tool: &Tool, approval: Option<()>) {
        let thread = self.thread_id();
        if let Some((server, name)) = tool
            .name
            .as_deref()
            .and_then(crate::mcp::ToolServers::split)
            && self.servers.configures(server)
        {
            let (server, name) = (server.to_owned(), name.to_owned());
            return self.server_tool(turn, &server, &name, tool).await;
        }
        if tool.name.as_deref() == Some("apply_patch") {
            let id = self.item_id("patch_");
            let changes = tool.input.clone().unwrap_or_else(|| {
                json!([{
                    "diff": "added\n",
                    "kind": { "type": "add" },
                    "path": format!("{}/new.txt", self.cwd),
                }])
            });
            let mut item = json!({
                "changes": changes,
                "id": id,
                "status": "inProgress",
                "type": "fileChange",
            });
            self.item(turn, "item/started", &item).await;
            self.run_tool(tool).await;
            let mut declined = false;
            if approval.is_some() {
                self.status(Some(&["waitingOnApproval"])).await;
                let answer = self
                    .request(
                        "item/fileChange/requestApproval",
                        json!({
                            "grantRoot": null,
                            "itemId": id,
                            "reason": null,
                            "startedAtMs": now_ms(),
                            "threadId": thread,
                            "turnId": turn,
                        }),
                    )
                    .await;
                let Some(answer) = answer else { return };
                self.status(Some(&[])).await;
                declined = !accepted(&answer);
            }
            item["status"] = json!(if declined {
                "declined"
            } else if tool.outcome.error {
                "failed"
            } else {
                "completed"
            });
            if self.edit_files && !declined && !tool.outcome.error {
                add_files(&item["changes"]);
            }
            self.item(turn, "item/completed", &item).await;
            return;
        }
        let id = self.item_id("exec-");
        let command = tool
            .input
            .as_ref()
            .and_then(|input| input["command"].as_str().map(str::to_owned))
            .unwrap_or_else(|| {
                if tool.is_exploration() {
                    "cat README.md".into()
                } else {
                    "true".into()
                }
            });
        let action = if tool.is_exploration() {
            json!({ "command": command, "name": "README.md", "path": format!("{}/README.md", self.cwd), "type": "read" })
        } else {
            json!({ "command": command, "type": "unknown" })
        };
        let mut item = json!({
            "aggregatedOutput": null,
            "command": format!("/bin/zsh -lc '{command}'"),
            "commandActions": [action],
            "cwd": self.cwd,
            "durationMs": null,
            "exitCode": null,
            "id": id,
            "pluginId": null,
            "processId": null,
            "scriptPath": null,
            "source": "agent",
            "status": "inProgress",
            "type": "commandExecution",
        });
        let background = tool
            .input
            .as_ref()
            .is_some_and(|input| input["run_in_background"] == true);
        if background {
            item["processId"] = json!(format!("{}", 9000 + self.next_item));
            item["source"] = json!("unifiedExecStartup");
        }
        // Codex reports the wait for a command's approval before the call.
        if approval.is_some() {
            self.status(Some(&["waitingOnApproval"])).await;
        }
        self.item(turn, "item/started", &item).await;
        if approval.is_some() {
            let answer = self
                .request(
                    "item/commandExecution/requestApproval",
                    json!({
                        "availableDecisions": ["accept", "acceptForSession", "decline", "cancel"],
                        "command": item["command"],
                        "commandActions": item["commandActions"],
                        "cwd": self.cwd,
                        "environmentId": "local",
                        "itemId": id,
                        "kind": "command",
                        "proposedExecpolicyAmendment": command.split_whitespace().collect::<Vec<_>>(),
                        "reason": tool
                            .input
                            .as_ref()
                            .and_then(|input| input["reason"].as_str())
                            .unwrap_or("The script asks before running this."),
                        "startedAtMs": now_ms(),
                        "threadId": thread,
                        "turnId": turn,
                    }),
                )
                .await;
            let Some(answer) = answer else { return };
            if !accepted(&answer) {
                item["status"] = json!("declined");
                self.item(turn, "item/completed", &item).await;
                self.status(Some(&[])).await;
                return;
            }
            self.status(Some(&[])).await;
        }
        // A command run in the background runs on past its turn, under a
        // process id, until it is terminated.
        if background {
            self.terminals.push((turn.to_owned(), item));
            return;
        }
        // A command printing in pieces prints them while it runs; any other
        // prints its output once it is done.
        if tool.outcome.pieces.is_some() {
            for piece in tool.outcome.printed() {
                self.notify(
                    "item/commandExecution/outputDelta",
                    json!({ "delta": piece, "itemId": id, "threadId": thread, "turnId": turn }),
                )
                .await;
            }
        }
        let started = std::time::Instant::now();
        self.run_tool(tool).await;
        let output = tool.outcome.text();
        if tool.outcome.pieces.is_none() && !output.is_empty() {
            self.notify(
                "item/commandExecution/outputDelta",
                json!({ "delta": output, "itemId": id, "threadId": thread, "turnId": turn }),
            )
            .await;
        }
        item["aggregatedOutput"] = json!(output);
        item["durationMs"] = json!(started.elapsed().as_millis() as u64);
        item["exitCode"] = json!(if tool.outcome.error { 1 } else { 0 });
        item["status"] = json!(if tool.outcome.error {
            "failed"
        } else {
            "completed"
        });
        self.item(turn, "item/completed", &item).await;
    }

    async fn ask(&mut self, turn: &str, ask: Ask) {
        let thread = self.thread_id();
        match ask {
            Ask::Permission(tool) => self.tool(turn, &tool, Some(())).await,
            Ask::Question { questions } => {
                let item = self.item_id("call_");
                self.status(Some(&["waitingOnUserInput"])).await;
                let answer = self
                    .request(
                        "item/tool/requestUserInput",
                        json!({
                            "autoResolutionMs": null,
                            "isBlocking": true,
                            "itemId": item,
                            "questions": questions.iter().enumerate().map(|(index, q)| question(index, q)).collect::<Vec<_>>(),
                            "threadId": thread,
                            "turnId": turn,
                        }),
                    )
                    .await;
                if answer.is_some() {
                    self.status(Some(&[])).await;
                }
            }
            Ask::Grant { reason, paths } => {
                let item = self.item_id("exec-");
                let permissions = json!({
                    "fileSystem": if paths.is_empty() { Value::Null } else { json!({ "write": paths }) },
                    "network": if paths.is_empty() { json!({ "enabled": true }) } else { Value::Null },
                });
                self.status(Some(&["waitingOnApproval"])).await;
                let answer = self
                    .request(
                        "item/permissions/requestApproval",
                        json!({
                            "cwd": self.cwd,
                            "environmentId": "local",
                            "itemId": item,
                            "permissions": permissions,
                            "reason": reason,
                            "startedAtMs": now_ms(),
                            "threadId": thread,
                            "turnId": turn,
                        }),
                    )
                    .await;
                if answer.is_some() {
                    self.status(Some(&[])).await;
                }
            }
            Ask::Form {
                server,
                message,
                schema,
            } => {
                self.schema = Some(schema);
                let params = json!({
                    "_meta": null,
                    "message": message,
                    "mode": "form",
                    "requestedSchema": crate::script::SCHEMA_SLOT,
                    "serverName": server,
                    "threadId": thread,
                    "turnId": turn,
                });
                self.elicit(turn, &server, params).await;
            }
            Ask::Link {
                server,
                message,
                url,
            } => {
                let params = json!({
                    "_meta": null,
                    "elicitationId": self.item_id("elicit_"),
                    "message": message,
                    "mode": "url",
                    "serverName": server,
                    "threadId": thread,
                    "turnId": turn,
                    "url": url,
                });
                self.elicit(turn, &server, params).await;
            }
            // Codex proposes a plan as an item and ends the turn; the host
            // approves it by starting the next turn out of plan mode.
            Ask::Plan { markdown } => {
                let id = format!("{turn}-plan");
                let mut item = json!({ "id": id, "text": "", "type": "plan" });
                self.item(turn, "item/started", &item).await;
                self.notify(
                    "item/plan/delta",
                    json!({ "delta": markdown, "itemId": id, "threadId": thread, "turnId": turn }),
                )
                .await;
                item["text"] = json!(markdown);
                self.item(turn, "item/completed", &item).await;
            }
            Ask::ToolServerDialog { .. } => unreachable!("refused when the script loaded"),
        }
    }

    /// A call to a tool server the launch configured, answered by it.
    async fn server_tool(&mut self, turn: &str, server: &str, name: &str, tool: &Tool) {
        let arguments = tool.input.clone().unwrap_or_else(|| json!({}));
        let mut item = json!({
            "appContext": null,
            "arguments": arguments,
            "durationMs": null,
            "error": null,
            "id": self.item_id("exec-"),
            "mcpAppUi": null,
            "pluginId": null,
            "readOnlyHint": null,
            "result": null,
            "server": server,
            "status": "inProgress",
            "tool": name,
            "type": "mcpToolCall",
        });
        self.item(turn, "item/started", &item).await;
        let started = std::time::Instant::now();
        self.run_tool(tool).await;
        let answer = self
            .servers
            .call(server, name, &arguments)
            .await
            .unwrap_or_else(|| Ok(tool.outcome.text()));
        item["durationMs"] = json!(started.elapsed().as_millis() as u64);
        match answer {
            Ok(text) => {
                item["result"] = json!({
                    "_meta": null,
                    "content": [{ "text": text, "type": "text" }],
                    "structuredContent": null,
                });
                item["status"] = json!("completed");
            }
            Err(error) => {
                item["error"] = json!({ "message": error });
                item["status"] = json!("failed");
            }
        }
        self.item(turn, "item/completed", &item).await;
    }

    /// A call held until its file exists keeps taking the host's frames
    /// while it runs; an interrupt stops waiting for it.
    async fn run_tool(&mut self, tool: &Tool) {
        let until = tool.outcome.until();
        while tokio::time::Instant::now() < until && !self.interrupted {
            self.pump().await;
        }
        let Some(path) = &tool.wait_for else { return };
        while !path.exists() && !self.interrupted {
            self.pump().await;
        }
    }

    /// A tool-server call that asks the host through its server.
    async fn elicit(&mut self, turn: &str, server: &str, params: Value) {
        let mut item = json!({
            "appContext": null,
            "arguments": {},
            "durationMs": null,
            "error": null,
            "id": self.item_id("exec-"),
            "mcpAppUi": null,
            "pluginId": null,
            "readOnlyHint": null,
            "result": null,
            "server": server,
            "status": "inProgress",
            "tool": "ask",
            "type": "mcpToolCall",
        });
        self.item(turn, "item/started", &item).await;
        // Codex asks to allow the call before the server's own ask reaches
        // the host, both as elicitations.
        let thread = self.thread_id();
        let approval = json!({
            "_meta": {
                "codex_approval_kind": "mcp_tool_call",
                "persist": ["session", "always"],
                "tool_description": "Ask the operator.",
                "tool_params": {},
                "tool_params_display": [],
            },
            "message": format!("Allow the {server} MCP server to run tool \"ask\"?"),
            "mode": "form",
            "requestedSchema": { "properties": {}, "type": "object" },
            "serverName": server,
            "threadId": thread,
            "turnId": turn,
        });
        self.status(Some(&["waitingOnApproval"])).await;
        let allowed = self
            .request("mcpServer/elicitation/request", approval)
            .await;
        let Some(allowed) = allowed else { return };
        self.status(Some(&[])).await;
        if allowed["result"]["action"] != "accept" {
            item["durationMs"] = json!(0);
            item["error"] = json!({ "message": "user rejected MCP tool call" });
            item["status"] = json!("failed");
            self.item(turn, "item/completed", &item).await;
            return;
        }
        self.status(Some(&["waitingOnApproval"])).await;
        let answer = self.request("mcpServer/elicitation/request", params).await;
        let Some(answer) = answer else { return };
        self.status(Some(&[])).await;
        let action = answer["result"]["action"].as_str().unwrap_or("cancel");
        let mut said = format!("elicitation {action}");
        if let Some(content) = answer["result"].get("content") {
            said.push(' ');
            said.push_str(&content.to_string());
        }
        item["durationMs"] = json!(0);
        item["result"] = json!({
            "_meta": null,
            "content": [{ "text": said, "type": "text" }],
            "structuredContent": null,
        });
        item["status"] = json!("completed");
        self.item(turn, "item/completed", &item).await;
    }
}

fn key(id: &Value) -> String {
    match id {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn accepted(answer: &Value) -> bool {
    matches!(
        answer["result"]["decision"].as_str(),
        Some("accept" | "acceptForSession")
    ) || answer["result"]["decision"].is_object()
}

fn question(index: usize, question: &Question) -> Value {
    // A question with no options is answered in words alone.
    let options: Vec<Value> = question
        .options
        .iter()
        .map(|option| {
            json!({
                "description": option.description(),
                "label": option.label(),
            })
        })
        .collect();
    json!({
        "header": question.header,
        "id": format!("q{index}"),
        "isOther": question.other,
        "isSecret": question.secret,
        "options": options,
        "question": question.question,
    })
}

/// The thread's token tally.
fn token_usage() -> Value {
    let tally = json!({
        "cachedInputTokens": 0,
        "inputTokens": 1,
        "outputTokens": 1,
        "reasoningOutputTokens": 0,
        "totalTokens": 2,
    });
    json!({ "last": tally, "total": tally, "modelContextWindow": 258400 })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

/// Writes each file a patch adds, its diff being the new file's text.
fn add_files(changes: &Value) {
    for change in changes.as_array().into_iter().flatten() {
        if change["kind"]["type"] != "add" {
            continue;
        }
        let Some(path) = change["path"].as_str() else {
            continue;
        };
        let path = std::path::Path::new(path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(error) = std::fs::write(path, change["diff"].as_str().unwrap_or_default()) {
            eprintln!("fake-codex: {}: {error}", path.display());
        }
    }
}

fn reaches_a_limit(steps: &[Step]) -> bool {
    steps.iter().any(|step| match step {
        Step::Usage(usage) => usage.status == "rejected",
        Step::Repeat { steps, .. } => reaches_a_limit(steps),
        _ => false,
    })
}
