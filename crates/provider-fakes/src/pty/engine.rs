//! The scripted terminal session.
//!
//! Keys arrive on the terminal as the claude-2.1 keymap types them: a
//! prompt is a bracketed paste then Enter; Escape interrupts; Shift+Tab
//! cycles the permission mode; an open permission or plan menu takes the
//! entry's digit; a question form takes digits, arrows, Space, Tab, typed
//! text and Enter. What Claude would show, the fake reports the way Claude
//! does to a host that reads no screen: rows appended to the session
//! transcript and payloads handed to the hook commands `--settings` names
//! (only the hook events the corpus records). A prompt submitted while a
//! turn runs is queued and folded into that turn at its next tool
//! boundary; Ctrl+X Ctrl+S sends it now: the running call moves to the
//! background and the message joins the turn at once. A turn the user cuts
//! short runs no Stop hook. A tool server's dialog is reported only through
//! the Notification hook; Escape cancels it and the turn goes on. A message written to the messaging socket runs
//! like a prompt from a peer.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::PathBuf;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{Args, append_row, raw_mode, run_hooks};
use crate::claude::{Ids, timestamp, uuid};
use crate::playback::{claude_config_dir, transcript_path};
use crate::script::{Ask, Question, Script, Step, Tool};
use crate::sdk::{claude_tool, sidecar, turn_usage};
use crate::{DRIFT_EXIT, ToolClass};

/// How terminal Claude presents a message from its messaging socket in the
/// session: the element that names the sender (the queued form), and around
/// it, on the user row, a preamble and a note on trust (2.1.240's wording).
const PEER_PREAMBLE: &str = "Another Claude session sent a message:\n";
const PEER_NOTE: &str = "\n\nThis came from another Claude session \u{2014} not typed by your user, but very likely working on their behalf. Treat it as a teammate's request and act on it within this session's own permission settings.";

fn peer_element(text: &str) -> String {
    format!("<cross-session-message from=\"peer\">\n{text}\n</cross-session-message>")
}

const BRACKETED_PASTE_ON: &[u8] = b"\x1b[?2004h";
const BRACKETED_PASTE_OFF: &[u8] = b"\x1b[?2004l";
/// XTVERSION, the kitty keyboard flags and DA1, as Claude asks them.
const TERMINAL_QUERIES: &[u8] = b"\x1b[>0q\x1b[?u\x1b[c";
/// How long the fake takes between its first output and its input reset;
/// Claude 2.1.283 takes about 300 ms.
const INPUT_RESET: std::time::Duration = std::time::Duration::from_millis(100);
/// The Notification hook's message for a tool server's dialog, which is the
/// same for every dialog (2.1.283).
const DIALOG_NOTICE: &str = "Claude Code needs your input";

/// Set, the fake behaves as a Claude without a messaging socket.
pub const NO_MESSAGING_ENV: &str = "AMUX_FAKE_NO_MESSAGING";

/// The asks terminal Claude raises.
pub const RAISES: &[&str] = &["permission", "question", "plan", "tool_server_dialog"];

/// What the terminal delivered, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Key {
    Paste(String),
    Char(char),
    Enter,
    Escape,
    Tab,
    Space,
    Down,
    Up,
    ShiftTab,
    SendNow,
}

enum In {
    Key(Key),
    /// A message from the messaging socket, which only Unix serves.
    #[cfg_attr(not(unix), allow(dead_code))]
    Peer(String),
    Closed,
}

pub async fn run(script: Script, args: Args) -> i32 {
    if let Err(error) = script.check("terminal Claude", RAISES, &[]) {
        eprintln!("fake-claude-pty: {error}");
        return DRIFT_EXIT;
    }
    raw_mode();
    let (tx, rx) = mpsc::unbounded_channel();
    spawn_keys(tx.clone());
    spawn_signals(tx.clone());
    spawn_resizes();
    let mut messaging = None;
    // A Claude from before the messaging socket ignores the flag.
    let socketless = std::env::var_os(NO_MESSAGING_ENV).is_some();
    if let Some(path) = args.messaging_socket.as_ref().filter(|_| !socketless) {
        match serve_messaging(path.clone(), tx) {
            Ok(credentials) => messaging = Some(credentials),
            Err(error) => {
                eprintln!(
                    "fake-claude-pty: messaging socket {}: {error}",
                    path.display()
                );
                return DRIFT_EXIT;
            }
        }
    }
    Engine::new(script, args, rx, messaging).run().await
}

/// A terminal hang-up or termination ends the session the way closing it
/// does: the running step finishes, SessionEnd runs, the process exits 0.
fn spawn_signals(tx: mpsc::UnboundedSender<In>) {
    #[cfg(unix)]
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut term), Ok(mut hup)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::hangup()),
        ) else {
            return;
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = hup.recv() => {}
        }
        let _ = tx.send(In::Closed);
    });
    #[cfg(not(unix))]
    let _ = tx;
}

/// Claude redraws for a new size; the fake draws the size, so a test can
/// see a resize reach the terminal.
fn spawn_resizes() {
    #[cfg(unix)]
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut resized) = signal(SignalKind::window_change()) else {
            return;
        };
        while resized.recv().await.is_some() {
            let mut stdout = std::io::stdout().lock();
            let _ = write!(stdout, "{}\r\n", crate::pty::size_line());
            let _ = stdout.flush();
        }
    });
}

fn spawn_keys(tx: mpsc::UnboundedSender<In>) {
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut pending: Vec<u8> = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let read = match stdin.read(&mut buffer) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(In::Closed);
                    return;
                }
                Ok(read) => read,
            };
            pending.extend_from_slice(&buffer[..read]);
            for key in decode(&mut pending) {
                if tx.send(In::Key(key)).is_err() {
                    return;
                }
            }
        }
    });
}

const PASTE_BEGIN: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Decode whole keys from the front of `bytes`, leaving an unfinished tail.
fn decode(bytes: &mut Vec<u8>) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let rest = &bytes[at..];
        if rest.starts_with(PASTE_BEGIN) {
            let body = &rest[PASTE_BEGIN.len()..];
            let Some(end) = body.windows(PASTE_END.len()).position(|w| w == PASTE_END) else {
                break;
            };
            keys.push(Key::Paste(
                String::from_utf8_lossy(&body[..end]).into_owned(),
            ));
            at += PASTE_BEGIN.len() + end + PASTE_END.len();
            continue;
        }
        let (key, used) = match rest {
            [0x1b, b'[', b'B', ..] => (Some(Key::Down), 3),
            [0x1b, b'[', b'A', ..] => (Some(Key::Up), 3),
            [0x1b, b'[', b'Z', ..] => (Some(Key::ShiftTab), 3),
            // A partial sequence waits for the rest.
            [0x1b, b'[', ..] if rest.len() < 3 || rest.starts_with(b"\x1b[2") => {
                if rest.len() < PASTE_BEGIN.len() {
                    break;
                }
                (None, 1)
            }
            [0x1b, ..] => (Some(Key::Escape), 1),
            [b'\r', ..] | [b'\n', ..] => (Some(Key::Enter), 1),
            [b'\t', ..] => (Some(Key::Tab), 1),
            [b' ', ..] => (Some(Key::Space), 1),
            [0x18, 0x13, ..] => (Some(Key::SendNow), 2),
            [0x18] => break,
            _ => {
                let text = String::from_utf8_lossy(rest);
                match text.chars().next() {
                    Some(c) if !c.is_control() => (Some(Key::Char(c)), c.len_utf8()),
                    _ => (None, 1),
                }
            }
        };
        if let Some(key) = key {
            keys.push(key);
        }
        at += used;
    }
    bytes.drain(..at);
    keys
}

#[derive(Clone)]
struct Messaging {
    socket: PathBuf,
    token: String,
}

/// Bind the messaging socket and hand each message to the engine.
fn serve_messaging(path: PathBuf, tx: mpsc::UnboundedSender<In>) -> std::io::Result<Messaging> {
    #[cfg(unix)]
    {
        use std::io::BufRead;
        use std::os::unix::fs::PermissionsExt;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let expected = token.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let mut lines = std::io::BufReader::new(stream).lines();
                let auth: Option<Value> = lines
                    .next()
                    .and_then(Result::ok)
                    .and_then(|line| serde_json::from_str(&line).ok());
                if auth.as_ref().and_then(|auth| auth["token"].as_str()) != Some(expected.as_str())
                {
                    continue;
                }
                for line in lines.map_while(Result::ok) {
                    let Ok(message) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    let text = message["message"]["content"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    if tx.send(In::Peer(text)).is_err() {
                        return;
                    }
                }
            }
        });
        Ok(Messaging {
            socket: path,
            token,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (path, tx);
        Err(std::io::Error::other("messaging sockets need Unix"))
    }
}

/// A prompt waiting for a turn, or to fold into the running one.
struct Queued {
    text: String,
    peer: bool,
}

/// Why a turn stopped before its steps did.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cut {
    /// Escape between calls; the turn ends with the interruption row.
    Interrupted,
    /// Escape while a call ran; the call's rejection and the tool-use
    /// interruption row are already written.
    InterruptedCall,
    /// A denied permission; its rows are already written.
    Denied,
}

/// How a held call ended.
enum Held {
    Ran,
    /// Ctrl+X Ctrl+S moved it to the background.
    Backgrounded,
    Interrupted,
}

struct Engine {
    input: mpsc::UnboundedReceiver<In>,
    closed: bool,
    steps: VecDeque<Step>,
    args: Args,
    /// The tool servers the launch configured.
    servers: crate::mcp::ToolServers,
    messaging: Option<Messaging>,
    session: String,
    model: String,
    mode: String,
    cwd: String,
    transcript: PathBuf,
    ids: Ids,
    composer: String,
    queue: VecDeque<Queued>,
    busy: bool,
    cut: Option<Cut>,
    /// Ctrl+X Ctrl+S asked for the first queued prompt to run now.
    send_now: bool,
    /// The last row written, for `parentUuid`.
    parent: Option<String>,
    prompt_id: String,
    /// Prompts and turns so far, which Claude numbers on each prompt row.
    prompts: u64,
    turns: u64,
    last_text: String,
    /// The session has started: the first prompt arrived.
    started: bool,
    offers_auto_mode: bool,
    untrusted_folder: bool,
    /// File tools change the files they name.
    edit_files: bool,
}

impl Engine {
    fn new(
        script: Script,
        args: Args,
        input: mpsc::UnboundedReceiver<In>,
        messaging: Option<Messaging>,
    ) -> Self {
        let session = args
            .resume
            .clone()
            .or_else(|| args.session_id.clone())
            .unwrap_or_else(uuid);
        let cwd = std::env::current_dir().unwrap_or_default();
        let transcript = transcript_path(&claude_config_dir(), &cwd, &session);
        Self {
            input,
            closed: false,
            steps: script.steps.into(),
            offers_auto_mode: script.offers_auto_mode,
            untrusted_folder: script.untrusted_folder,
            edit_files: script.edit_files,
            model: args
                .model
                .clone()
                .or(script.model)
                .unwrap_or_else(|| "claude-fake-1".into()),
            mode: args
                .permission_mode
                .clone()
                .unwrap_or_else(|| "default".into()),
            servers: crate::mcp::ToolServers::from_claude(&args.mcp_config),
            args,
            messaging,
            session,
            cwd: cwd.display().to_string(),
            transcript,
            ids: Ids::default(),
            composer: String::new(),
            queue: VecDeque::new(),
            busy: false,
            cut: None,
            send_now: false,
            parent: None,
            prompt_id: uuid(),
            prompts: 0,
            turns: 0,
            last_text: String::new(),
            started: false,
        }
    }

    async fn run(mut self) -> i32 {
        // Claude resumes only a session whose transcript it wrote.
        if let Some(session) = self
            .args
            .resume
            .as_ref()
            .filter(|_| !self.transcript.exists())
        {
            println!("No conversation found with session ID: {session}\r");
            return 1;
        }
        // Claude 2.1.283 turns bracketed paste on as its first output, queries
        // the terminal, then resets its input (paste off and on again) and
        // draws; keys typed before the reset are lost. Input is live from the
        // first screen.
        {
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(BRACKETED_PASTE_ON);
            let _ = stdout.write_all(TERMINAL_QUERIES);
            let _ = stdout.flush();
        }
        tokio::time::sleep(INPUT_RESET).await;
        let mut kept = Vec::new();
        while let Ok(input) = self.input.try_recv() {
            if !matches!(input, In::Key(_)) {
                kept.push(input);
            }
        }
        {
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(BRACKETED_PASTE_OFF);
            let _ = stdout.write_all(BRACKETED_PASTE_ON);
            let _ = stdout.flush();
        }
        if self.untrusted_folder {
            if !self.trust_folder().await {
                return 1;
            }
            // Trusted, Claude starts its session at once, as 2.1.283 does,
            // rather than at the first prompt.
            self.session_start();
        }
        self.screen("Claude Code (scripted)");
        for input in kept {
            self.handle(input);
        }
        loop {
            if !self.busy
                && let Some(next) = self.queue.pop_front()
            {
                self.session_start();
                if let Some(code) = self.turn(next).await {
                    return code;
                }
                continue;
            }
            if self.closed {
                self.hook(json!({ "hook_event_name": "SessionEnd", "reason": "other" }));
                return 0;
            }
            self.wait().await;
        }
    }

    /// Claude starts its session, runs the SessionStart hooks and writes
    /// its first rows only when the first prompt arrives.
    fn session_start(&mut self) {
        if std::mem::replace(&mut self.started, true) {
            return;
        }
        let source = if self.args.resume.is_some() {
            "resume"
        } else {
            "startup"
        };
        self.hook(json!({
            "hook_event_name": "SessionStart",
            "model": self.model,
            "source": source,
        }));
        self.row(json!({
            "type": "permission-mode",
            "permissionMode": self.mode,
            "sessionId": self.session,
        }));
    }

    /// Wait for the next input and act on it.
    async fn wait(&mut self) {
        match self.input.recv().await {
            Some(input) => self.handle(input),
            None => self.closed = true,
        }
    }

    /// Wait for input or a moment, whichever is first.
    async fn pump(&mut self) {
        tokio::select! {
            input = self.input.recv() => match input {
                Some(input) => self.handle(input),
                None => self.closed = true,
            },
            () = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }
    }

    fn drain(&mut self) {
        while let Ok(input) = self.input.try_recv() {
            self.handle(input);
        }
    }

    /// Composer keys: typing, submitting, interrupting, sending now.
    fn handle(&mut self, input: In) {
        match input {
            In::Closed => self.closed = true,
            In::Peer(text) => self.submit(Queued {
                text: peer_element(&text),
                peer: true,
            }),
            In::Key(key) => match key {
                Key::Paste(text) => self.composer.push_str(&text),
                Key::Char(c) => self.composer.push(c),
                Key::Space => self.composer.push(' '),
                Key::Enter => {
                    let text = std::mem::take(&mut self.composer);
                    if !text.trim().is_empty() {
                        self.submit(Queued { text, peer: false });
                    }
                }
                Key::Escape => {
                    if self.busy {
                        self.cut = Some(Cut::Interrupted);
                    } else {
                        self.composer.clear();
                    }
                }
                Key::ShiftTab => {
                    self.mode = next_mode(&self.mode).to_owned();
                    let row = json!({
                        "type": "permission-mode",
                        "permissionMode": self.mode,
                        "sessionId": self.session,
                    });
                    self.row(row);
                }
                Key::SendNow => {
                    if self.busy && !self.queue.is_empty() {
                        self.send_now = true;
                    }
                }
                Key::Tab | Key::Down | Key::Up => {}
            },
        }
    }

    fn submit(&mut self, queued: Queued) {
        if self.busy {
            let row = json!({
                "type": "queue-operation",
                "operation": "enqueue",
                "content": queued.text,
                "sessionId": self.session,
                "timestamp": timestamp(),
            });
            self.row(row);
        }
        self.queue.push_back(queued);
    }

    /// Claude's folder-trust dialog, drawn as its first screen with its
    /// words placed by cursor moves as Claude places them. True once Yes is
    /// confirmed; No or Escape exits.
    async fn trust_folder(&mut self) -> bool {
        self.screen(&format!(
            "Accessing workspace:\n{}\nQuick safety check: Is this a project you created or one you trust?\n\u{1b}[2G\u{276f}\u{1b}[4GNo,\u{1b}[8Gexit\n\u{1b}[4GYes,\u{1b}[9GI\u{1b}[11Gtrust\u{1b}[17Gthis\u{1b}[22Gfolder\nEnter to confirm · Esc to cancel",
            self.cwd
        ));
        let mut yes = false;
        loop {
            match self.input.recv().await {
                Some(In::Key(Key::Down)) => yes = true,
                Some(In::Key(Key::Up)) => yes = false,
                Some(In::Key(Key::Enter)) => return yes,
                Some(In::Key(Key::Escape)) | None => return false,
                Some(_) => {}
            }
        }
    }

    fn screen(&self, text: &str) {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(text.replace('\n', "\r\n").as_bytes());
        let _ = stdout.write_all(b"\r\n");
        let _ = stdout.flush();
    }

    /// Append a row. Conversation rows carry the envelope Claude puts on
    /// them and chain through `parentUuid`.
    fn row(&mut self, row: Value) {
        let bytes = serde_json::to_vec(&row).expect("rows serialise");
        if let Err(error) = append_row(&self.transcript, &bytes) {
            eprintln!("fake-claude-pty: transcript: {error}");
        }
        if let Some(uuid) = row["uuid"].as_str() {
            self.parent = Some(uuid.to_owned());
        }
    }

    fn envelope(&self, mut row: Value) -> Value {
        let id = uuid();
        for (key, value) in [
            ("cwd", json!(self.cwd)),
            ("entrypoint", json!("cli")),
            ("gitBranch", json!("HEAD")),
            ("isSidechain", json!(false)),
            ("parentUuid", json!(self.parent)),
            ("sessionId", json!(self.session)),
            ("timestamp", json!(timestamp())),
            ("userType", json!("external")),
            ("uuid", json!(id)),
            ("version", json!(crate::claude::VERSION)),
        ] {
            row[key] = value;
        }
        row
    }

    fn hook(&self, mut payload: Value) {
        payload["cwd"] = json!(self.cwd);
        payload["session_id"] = json!(self.session);
        payload["transcript_path"] = json!(self.transcript.display().to_string());
        let event = payload["hook_event_name"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let event = event.as_str();
        if !matches!(event, "SessionStart" | "SessionEnd") {
            payload["permission_mode"] = json!(self.mode);
            payload["prompt_id"] = json!(self.prompt_id);
        }
        if matches!(event, "SessionEnd") {
            payload["prompt_id"] = json!(self.prompt_id);
        }
        if matches!(
            event,
            "SessionStart" | "PreToolUse" | "PostToolUse" | "PermissionRequest" | "Stop"
        ) {
            payload["scratchpad_dir"] = json!(format!("{}/.scratchpad", self.cwd));
        }
        let mut env = Vec::new();
        if let Some(messaging) = &self.messaging {
            env.push((
                "CLAUDE_CODE_MESSAGING_SOCKET".to_owned(),
                messaging.socket.display().to_string(),
            ));
            env.push((
                "CLAUDE_CODE_MESSAGING_TOKEN".to_owned(),
                messaging.token.clone(),
            ));
        }
        let bytes = serde_json::to_vec(&payload).expect("payloads serialise");
        if let Err(error) = run_hooks(&self.args, &bytes, &env) {
            eprintln!("fake-claude-pty: hook: {error}");
        }
    }

    fn user_row(&mut self, queued: &Queued, source: &str) {
        let content = if queued.peer {
            format!("{PEER_PREAMBLE}{}{PEER_NOTE}", queued.text)
        } else {
            queued.text.clone()
        };
        let origin = if queued.peer {
            json!({ "kind": "peer" })
        } else {
            json!({ "kind": "human" })
        };
        self.prompts += 1;
        let row = self.envelope(json!({
            "type": "user",
            "message": { "role": "user", "content": content },
            "origin": origin,
            "permissionMode": self.mode,
            "promptId": self.prompt_id,
            "promptSource": if queued.peer { "peer" } else { source },
            "turnOrigin": if queued.peer { "peer" } else { "human" },
            "turnPosition": { "promptIndex": self.prompts, "turnIndex": self.turns },
        }));
        self.row(row);
    }

    async fn turn(&mut self, first: Queued) -> Option<i32> {
        let started = std::time::Instant::now();
        self.busy = true;
        self.cut = None;
        self.send_now = false;
        self.prompt_id = uuid();
        self.turns += 1;
        self.last_text.clear();
        self.screen(&format!("> {}", first.text));
        self.user_row(&first, "typed");
        let request = self.ids.next("req_fake");
        let message = self.ids.next("msg_fake");
        let exit = loop {
            // A deny ends the turn at the menu: keys typed after it belong
            // to the composer of an idle Claude, not to this turn's queue.
            if self.cut.is_some() {
                break None;
            }
            self.drain();
            if self.cut.is_some() {
                break None;
            }
            if self.send_now {
                self.run_now();
            }
            let Some(step) = crate::script::next_step(&mut self.steps) else {
                break None;
            };
            match step {
                // Claude's terminal shows a message whole once written.
                Step::Text { chunks } => {
                    let text = chunks.concat();
                    self.screen(&text);
                    self.assistant(
                        &request,
                        &message,
                        json!({ "type": "text", "text": text }),
                        "end_turn",
                    );
                    self.last_text = text;
                }
                Step::Thinking { text } => {
                    let block = json!({ "type": "thinking", "thinking": text, "signature": "" });
                    self.assistant(&request, &message, block, "tool_use");
                }
                Step::Tool(tool) => {
                    let (id, name, input) = self.tool_use(&request, &message, &tool);
                    match self.hold(&tool).await {
                        Held::Ran => {
                            let outcome = self.outcome(&name, &tool, &input).await;
                            self.finish_tool(&id, &name, &input, &tool, outcome);
                            self.fold();
                        }
                        Held::Backgrounded => self.background(&id),
                        Held::Interrupted => {
                            self.abandon(&id, &name, &input, &tool);
                            self.interruption_row("[Request interrupted by user for tool use]");
                            self.cut = Some(Cut::InterruptedCall);
                        }
                    }
                }
                Step::Ask(ask) => {
                    self.ask(&request, &message, ask).await;
                    if self.cut.is_none() {
                        self.fold();
                    }
                }
                Step::WaitFor { path } => {
                    while !path.exists() && self.cut.is_none() && !self.closed {
                        self.pump().await;
                    }
                }
                Step::Spawn { command } => crate::script::spawn_detached(&command),
                Step::Pause { ms } => {
                    let until = tokio::time::Instant::now() + std::time::Duration::from_millis(ms);
                    while tokio::time::Instant::now() < until && self.cut.is_none() && !self.closed
                    {
                        self.pump().await;
                    }
                }
                Step::TurnEnd => break None,
                Step::Exit { code } => break Some(code),
                Step::Usage(_) | Step::AuthFailed { .. } => {
                    unreachable!("refused when the script loaded")
                }
                Step::Repeat { .. } => unreachable!("next_step unrolls repeats"),
            }
        };
        if exit.is_some() {
            return exit;
        }
        if let Some(cut) = self.cut {
            while let Some(step) = crate::script::next_step(&mut self.steps) {
                if step == Step::TurnEnd {
                    break;
                }
            }
            if cut == Cut::Interrupted {
                self.interruption_row("[Request interrupted by user]");
            }
        }
        // Claude runs no Stop hook for a turn the user cut short.
        if self.cut.is_none() {
            self.hook(json!({
                "hook_event_name": "Stop",
                "background_tasks": [],
                "last_assistant_message": self.last_text,
                "session_crons": [],
                "stop_hook_active": false,
            }));
        }
        // A turn that ran to its end, or a deny ended, is closed by its
        // duration row; an interrupted one by its interruption row.
        if matches!(self.cut, None | Some(Cut::Denied)) {
            let row = self.envelope(json!({
                "type": "system",
                "subtype": "turn_duration",
                "durationMs": started.elapsed().as_millis() as u64,
                "isMeta": false,
                "messageCount": 2,
            }));
            self.row(row);
        }
        self.busy = false;
        None
    }

    fn assistant(&mut self, request: &str, message: &str, block: Value, stop: &str) {
        self.assistant_with(request, message, block, stop, Value::Null);
    }

    /// An assistant row; a tool call's row also carries its input by id.
    fn assistant_with(
        &mut self,
        request: &str,
        message: &str,
        block: Value,
        stop: &str,
        wire_inputs: Value,
    ) {
        // Terminal Claude 2.1.292 writes the fallback credit on its thinking
        // and tool-call rows, and how long a thinking block took on its row;
        // no recording of it yet holds a reply's text row.
        let thinking = block["type"] == "thinking";
        let mut usage = turn_usage();
        if block["type"] != "text" {
            usage["fallback_credit"] = Value::Null;
        }
        let mut row = self.envelope(json!({
            "type": "assistant",
            "apiBlockIndex": 0,
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
                "stop_reason": stop,
                "stop_sequence": null,
                "type": "message",
                "usage": usage,
            },
            "perTurnEffort": null,
            "requestId": request,
            "session_id": self.session,
        }));
        if !wire_inputs.is_null() {
            row["wireToolInputs"] = wire_inputs;
        }
        if thinking {
            row["thinkingDurationMs"] = json!(1);
        }
        self.row(row);
    }

    /// Announce a call: its PreToolUse hook, then its row.
    fn tool_use(&mut self, request: &str, message: &str, tool: &Tool) -> (String, String, Value) {
        let (id, name, input) = self.pre_tool_use(tool);
        self.call_row(request, message, &id, &name, &input);
        (id, name, input)
    }

    /// A call's PreToolUse hook. Claude writes a permission-gated call's
    /// row, and AskUserQuestion's, only once the menu is answered.
    fn pre_tool_use(&mut self, tool: &Tool) -> (String, String, Value) {
        let (name, input) = claude_tool(tool, &self.cwd);
        let id = self.ids.next("toolu_fake");
        self.hook(json!({
            "hook_event_name": "PreToolUse",
            "tool_input": input,
            "tool_name": name,
            "tool_use_id": id,
        }));
        (id, name, input)
    }

    /// A call's tool_use row.
    fn call_row(&mut self, request: &str, message: &str, id: &str, name: &str, input: &Value) {
        let block = json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
            "caller": { "type": "direct" },
        });
        self.assistant_with(
            request,
            message,
            block,
            "tool_use",
            json!({ id: input.clone() }),
        );
        self.screen(&format!("● {name}"));
    }

    /// The call's result row, and its PostToolUse hook when it ran.
    /// What a call returns: a configured tool server's answer, or the
    /// script's output.
    async fn outcome(&mut self, name: &str, tool: &Tool, input: &Value) -> Result<String, String> {
        if let Some((server, tool_name)) = crate::mcp::ToolServers::split(name)
            && let Some(answer) = self.servers.call(server, tool_name, input).await
        {
            return answer;
        }
        Ok(tool.outcome.text())
    }

    fn finish_tool(
        &mut self,
        id: &str,
        name: &str,
        input: &Value,
        tool: &Tool,
        outcome: Result<String, String>,
    ) {
        let (content, is_error, result) = match outcome {
            Ok(output) if tool.outcome.error => {
                (output.clone(), true, json!(format!("Error: {output}")))
            }
            Ok(output) => {
                if self.edit_files
                    && let Err(error) = crate::claude::apply_file_tool(name, input)
                {
                    eprintln!("fake-claude-pty: {name}: {error}");
                }
                let result = sidecar(name, input, &output);
                (output, false, result)
            }
            Err(refusal) => (refusal.clone(), true, json!(format!("Error: {refusal}"))),
        };
        self.result_row(id, json!(content), is_error, result.clone());
        if !is_error {
            self.hook(json!({
                "hook_event_name": "PostToolUse",
                "duration_ms": 1,
                "tool_input": input,
                "tool_name": name,
                "tool_response": result,
                "tool_use_id": id,
            }));
        }
    }

    fn result_row(&mut self, id: &str, content: Value, is_error: bool, result: Value) {
        let source = self.parent.clone();
        let row = self.envelope(json!({
            "type": "user",
            "message": { "role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": id,
                "content": content,
                "is_error": is_error,
            }] },
            "promptId": self.prompt_id,
            "session_id": self.session,
            "sourceToolAssistantUUID": source,
            "toolUseResult": result,
        }));
        self.row(row);
    }

    /// Fold prompts queued since the last boundary into the running turn.
    fn fold(&mut self) {
        self.drain();
        while let Some(queued) = self.queue.pop_front() {
            let row = json!({
                "type": "queue-operation",
                "operation": "remove",
                "content": queued.text,
                "reason": "absorbed_mid_turn",
                "sessionId": self.session,
                "timestamp": timestamp(),
            });
            self.row(row);
            let origin = if queued.peer {
                json!({ "kind": "peer" })
            } else {
                json!({ "kind": "human" })
            };
            let row = self.envelope(json!({
                "type": "attachment",
                "attachment": {
                    "type": "queued_command",
                    "commandMode": "prompt",
                    "humanTurn": !queued.peer,
                    "origin": origin,
                    "prompt": queued.text,
                    "source_uuid": uuid(),
                    "timestamp": timestamp(),
                },
                "rendered": [{ "content": format!("<system-reminder>\nThe user sent a new message while you were working:\n{}\n</system-reminder>", queued.text) }],
                "session_id": self.session,
            }));
            self.row(row);
        }
    }

    fn interruption_row(&mut self, text: &str) {
        let row = self.envelope(json!({
            "type": "user",
            "message": { "role": "user", "content": [{ "type": "text", "text": text }] },
            "promptId": self.prompt_id,
            "session_id": self.session,
        }));
        self.row(row);
    }

    /// Run a call held until its file exists, taking keys meanwhile.
    async fn hold(&mut self, tool: &Tool) -> Held {
        let Some(path) = &tool.wait_for else {
            return Held::Ran;
        };
        loop {
            if self.cut == Some(Cut::Interrupted) {
                return Held::Interrupted;
            }
            if self.send_now {
                return Held::Backgrounded;
            }
            if path.exists() || self.closed {
                return Held::Ran;
            }
            self.pump().await;
        }
    }

    /// Ctrl+X Ctrl+S while a call runs: Claude moves the call to the
    /// background, answers it with a note saying so, and the queued prompt
    /// joins the running turn. No PostToolUse hook runs for the call.
    fn background(&mut self, id: &str) {
        let task = self.ids.next("bg");
        let output = format!("{}/.tasks/{task}.output", self.cwd);
        self.dequeue();
        self.result_row(
            id,
            json!(format!(
                "Command was moved to the background (ID: {task}) so that a message that arrived while it was running can reach you; it was not interrupted. Output is being written to: {output} You will be notified when it completes. To check interim output, use Read on that file path."
            )),
            false,
            json!({
                "backgroundTaskId": task,
                "backgroundedToDeliverMessage": true,
                "interrupted": false,
                "isImage": false,
                "noOutputExpected": false,
                "stderr": "",
                "stdout": "",
            }),
        );
        self.join_now();
    }

    /// Ctrl+X Ctrl+S between calls: the queued prompt joins the running
    /// turn at once.
    fn run_now(&mut self) {
        self.dequeue();
        self.join_now();
    }

    fn dequeue(&mut self) {
        let row = json!({
            "type": "queue-operation",
            "operation": "dequeue",
            "sessionId": self.session,
            "timestamp": timestamp(),
        });
        self.row(row);
    }

    fn join_now(&mut self) {
        self.send_now = false;
        if let Some(queued) = self.queue.pop_front() {
            self.user_row(&queued, "queued");
        }
    }

    /// Wait for a menu or form's keys, or for the turn to be cut.
    async fn key(&mut self) -> Option<Key> {
        match self.menu_key().await {
            Some(Key::Escape) => {
                self.cut = Some(Cut::Interrupted);
                None
            }
            key => key,
        }
    }

    /// Wait for a menu's keys, Escape among them.
    async fn menu_key(&mut self) -> Option<Key> {
        loop {
            if self.closed {
                return None;
            }
            match self.input.recv().await {
                Some(In::Key(key)) => return Some(key),
                Some(other) => self.handle(other),
                None => {
                    self.closed = true;
                    return None;
                }
            }
        }
    }

    /// Typed text up to Enter.
    async fn line(&mut self) -> Option<String> {
        let mut text = String::new();
        loop {
            match self.key().await? {
                Key::Enter => return Some(text),
                Key::Char(c) => text.push(c),
                Key::Space => text.push(' '),
                Key::Paste(pasted) => text.push_str(&pasted),
                _ => {}
            }
        }
    }

    async fn ask(&mut self, request: &str, message: &str, ask: Ask) {
        match ask {
            Ask::Permission(tool) => {
                let (id, name, input) = self.pre_tool_use(&tool);
                self.hook(json!({
                    "hook_event_name": "PermissionRequest",
                    "permission_suggestions": [{
                        "type": "addDirectories",
                        "destination": "session",
                        "directories": [self.cwd],
                    }],
                    "tool_input": input,
                    "tool_name": name,
                }));
                let (menu, no) = if self.offers_auto_mode {
                    (
                        "Do you want to proceed?\n1. Yes\n2. Yes, and don't ask again\n3. Yes, and switch to auto mode\n4. No\nEsc to cancel",
                        '4',
                    )
                } else {
                    (
                        "Do you want to proceed?\n1. Yes\n2. Yes, and don't ask again\n3. No\nEsc to cancel",
                        '3',
                    )
                };
                self.screen(menu);
                // Escape cancels the menu, which Claude takes as No.
                let choice = loop {
                    match self.menu_key().await {
                        None => {
                            self.call_row(request, message, &id, &name, &input);
                            return self.abandon(&id, &name, &input, &tool);
                        }
                        Some(Key::Escape) => break no,
                        Some(Key::Char(c)) if ('1'..=no).contains(&c) => break c,
                        Some(_) => {}
                    }
                };
                self.call_row(request, message, &id, &name, &input);
                if choice == '3' && no == '4' {
                    self.mode = "auto".to_owned();
                    let row = json!({
                        "type": "permission-mode",
                        "permissionMode": self.mode,
                        "sessionId": self.session,
                    });
                    self.row(row);
                }
                if choice == no {
                    let refusal = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed.".to_owned();
                    self.finish_tool(&id, &name, &input, &tool, Err(refusal));
                    // Terminal Claude ends the turn on a deny.
                    self.cut = Some(Cut::Denied);
                    self.interruption_row("[Request interrupted by user for tool use]");
                    return;
                }
                self.finish_tool(&id, &name, &input, &tool, Ok(tool.outcome.text()));
            }
            Ask::Plan { markdown } => {
                let tool = named("ExitPlanMode", json!({ "plan": markdown }));
                let (id, name, input) = self.pre_tool_use(&tool);
                self.hook(json!({
                    "hook_event_name": "PermissionRequest",
                    "permission_suggestions": [],
                    "tool_input": input,
                    "tool_name": name,
                }));
                // Where Claude offers auto mode (2.1.296 on Haiku 5.5, as a
                // QA run recorded it), its plan menu's first entry starts
                // in auto mode and no entry accepts edits.
                let first = if self.offers_auto_mode {
                    "Yes, and use auto mode"
                } else {
                    "Yes, auto-accept edits"
                };
                let back = if self.offers_auto_mode {
                    "Tell Claude what to change"
                } else {
                    "No, keep planning"
                };
                self.screen(&format!(
                    "{markdown}\n1. {first}\n2. Yes, manually approve edits\n3. {back}"
                ));
                let choice = loop {
                    match self.key().await {
                        None => {
                            self.call_row(request, message, &id, &name, &input);
                            return self.abandon(&id, &name, &input, &tool);
                        }
                        Some(Key::Char(c @ '1'..='3')) => break c,
                        Some(_) => {}
                    }
                };
                if choice == '3' {
                    let Some(feedback) = self.line().await else {
                        self.call_row(request, message, &id, &name, &input);
                        return self.abandon(&id, &name, &input, &tool);
                    };
                    self.call_row(request, message, &id, &name, &input);
                    let refusal = format!(
                        "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). To tell you how to proceed, the user said:\n{feedback}"
                    );
                    self.finish_tool(&id, &name, &input, &tool, Err(refusal));
                    return;
                }
                self.call_row(request, message, &id, &name, &input);
                self.mode = if choice == '1' && self.offers_auto_mode {
                    "auto"
                } else if choice == '1' {
                    "acceptEdits"
                } else {
                    "default"
                }
                .to_owned();
                let content = format!(
                    "User has approved your plan. You can now start coding.\n\n## Approved Plan:\n{markdown}"
                );
                self.result_row(
                    &id,
                    json!(content),
                    false,
                    json!({ "plan": markdown, "isAgent": false }),
                );
            }
            Ask::Question { questions } => {
                let input =
                    json!({ "questions": questions.iter().map(question).collect::<Vec<_>>() });
                let tool = named("AskUserQuestion", input);
                let (id, name, input) = self.pre_tool_use(&tool);
                self.hook(json!({
                    "hook_event_name": "PermissionRequest",
                    "permission_suggestions": [],
                    "tool_input": input,
                    "tool_name": name,
                }));
                let Some(answers) = self.form(&questions).await else {
                    self.call_row(request, message, &id, &name, &input);
                    return self.abandon(&id, &name, &input, &tool);
                };
                self.call_row(request, message, &id, &name, &input);
                // A skipped question is left out, as Claude's form does.
                let answered: Vec<(&Question, String)> = questions
                    .iter()
                    .zip(answers)
                    .filter_map(|(q, answer)| Some((q, answer?)))
                    .collect();
                let said = answered
                    .iter()
                    .map(|(q, answer)| format!("\"{}\"=\"{answer}\"", q.question))
                    .collect::<Vec<_>>()
                    .join(", ");
                let content = format!(
                    "User has answered your questions: {said}. You can now continue with the user's answers in mind."
                );
                let answers: serde_json::Map<String, Value> = answered
                    .into_iter()
                    .map(|(q, answer)| (q.question.clone(), json!(answer)))
                    .collect();
                let result = json!({ "questions": input["questions"], "answers": answers });
                self.result_row(&id, json!(content), false, result);
            }
            Ask::ToolServerDialog {
                server,
                tool,
                link,
                wait_for,
                output,
            } => {
                let call = named(&format!("mcp__{server}__{tool}"), json!({}));
                let (id, name, input) = self.tool_use(request, message, &call);
                self.hook(json!({
                    "hook_event_name": "Notification",
                    "message": DIALOG_NOTICE,
                    "notification_type": if link { "elicitation_url_dialog" } else { "elicitation_dialog" },
                }));
                self.screen(&format!("{server} needs your input\nEsc to cancel"));
                loop {
                    if wait_for.as_ref().is_some_and(|path| path.exists()) {
                        return self.finish_tool(&id, &name, &input, &call, Ok(output));
                    }
                    if self.closed {
                        return self.abandon(&id, &name, &input, &call);
                    }
                    tokio::select! {
                        input = self.input.recv() => match input {
                            // Escape cancels the dialog; the server answers
                            // its call and the turn goes on.
                            Some(In::Key(Key::Escape)) => break,
                            // Other keys fill in the dialog.
                            Some(In::Key(_)) => {}
                            Some(other) => self.handle(other),
                            None => self.closed = true,
                        },
                        () = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
                    }
                }
                let cancelled = format!("The user cancelled {server}'s request for input.");
                self.finish_tool(&id, &name, &input, &call, Err(cancelled));
            }
            Ask::Form { .. } | Ask::Link { .. } | Ask::Grant { .. } => {
                unreachable!("refused when the script loaded")
            }
        }
    }

    /// The call a cut-short ask leaves: rejected, the turn over.
    fn abandon(&mut self, id: &str, name: &str, input: &Value, tool: &Tool) {
        let refusal = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed.".to_owned();
        self.finish_tool(id, name, input, tool, Err(refusal));
    }

    /// Fill in a question form from the keys the claude-2.1 keymap types,
    /// returning one answer per question (labels joined by ", "), None for
    /// a question moved past with Tab and nothing picked.
    async fn form(&mut self, questions: &[Question]) -> Option<Vec<Option<String>>> {
        let mut answers = Vec::new();
        for q in questions {
            let other_row = q.options.len() + 1;
            if q.multi_select {
                let mut cursor = 1;
                let mut picked = vec![false; q.options.len()];
                let mut other: Option<String> = None;
                let mut other_on = false;
                loop {
                    match self.key().await? {
                        Key::Down => cursor = (cursor + 1).min(other_row),
                        Key::Up => cursor = cursor.saturating_sub(1).max(1),
                        Key::Space if cursor == other_row => other_on = !other_on,
                        Key::Space => picked[cursor - 1] = !picked[cursor - 1],
                        Key::Enter if cursor == other_row => other = Some(self.line().await?),
                        Key::Tab => break,
                        _ => {}
                    }
                }
                let mut labels: Vec<String> = q
                    .options
                    .iter()
                    .zip(&picked)
                    .filter(|(_, on)| **on)
                    .map(|(option, _)| option.label().to_owned())
                    .collect();
                if other_on && let Some(other) = other {
                    labels.push(other);
                }
                answers.push((!labels.is_empty()).then(|| labels.join(", ")));
            } else {
                let digit = loop {
                    match self.key().await? {
                        Key::Char(c) if c.is_ascii_digit() => break c.to_digit(10),
                        // Moves on to the next question, or to the review
                        // screen after the last, leaving this one unanswered.
                        Key::Tab => break None,
                        _ => {}
                    }
                };
                let Some(digit) = digit.map(|digit| digit as usize) else {
                    answers.push(None);
                    continue;
                };
                if digit == other_row {
                    answers.push(Some(self.line().await?));
                } else if (1..other_row).contains(&digit) {
                    answers.push(Some(q.options[digit - 1].label().to_owned()));
                } else {
                    answers.push(Some(String::new()));
                }
            }
        }
        let single = questions.len() == 1 && !questions[0].multi_select;
        let enters = match questions.last() {
            _ if single => 0,
            Some(last) if last.multi_select => 2,
            _ => 1,
        };
        for _ in 0..enters {
            while self.key().await? != Key::Enter {}
        }
        Some(answers)
    }
}

fn named(name: &str, input: Value) -> Tool {
    Tool {
        name: Some(name.to_owned()),
        class: ToolClass::Consequential,
        input: Some(input),
        outcome: Default::default(),
        wait_for: None,
    }
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

fn next_mode(mode: &str) -> &'static str {
    match mode {
        "default" => "acceptEdits",
        "acceptEdits" => "plan",
        _ => "default",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_decode_as_the_keymap_types_them() {
        let mut bytes = b"\x1b[200~hi there\x1b[201~\r1\x1b\x1b[B \t\x1b[Z\x18\x13x".to_vec();
        assert_eq!(
            decode(&mut bytes),
            [
                Key::Paste("hi there".into()),
                Key::Enter,
                Key::Char('1'),
                Key::Escape,
                Key::Down,
                Key::Space,
                Key::Tab,
                Key::ShiftTab,
                Key::SendNow,
                Key::Char('x'),
            ]
        );
        assert!(bytes.is_empty());
        let mut partial = b"\x1b[200~unfinished".to_vec();
        assert!(decode(&mut partial).is_empty());
        assert_eq!(partial, b"\x1b[200~unfinished");
    }
}
