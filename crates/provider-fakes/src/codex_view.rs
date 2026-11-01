//! `fake-codex resume <thread> --remote unix://PATH`: the Codex terminal
//! view an attach opens on an agent's thread.
//!
//! The real one is Codex's TUI repainting the thread from its rollout. The
//! fake draws plain lines a test can look for: the thread it opened, its
//! size at start and after every resize, and each line typed into it. It
//! ends on Ctrl-C or Ctrl-D, at the end of its input, or when its terminal
//! hangs up or it is asked to terminate.
//!
//! It is a client of the app server on the `--remote` socket, as Codex's own
//! app is: it resumes the thread there, or says why it could not and exits
//! with 1. A typed line starts a turn, or steers the one running; `y` or `n`
//! answers the oldest approval waiting, which it draws as
//! `approval <id>: <what>` until the server says it was resolved. It also
//! draws each prompt and reply as it completes and each turn's end.

use std::io::{Read, Write};
use std::sync::mpsc;

use serde_json::{Value, json};

enum Event {
    Bytes(Vec<u8>),
    Resized,
    Ended,
    /// The app server took the resume, with its answer.
    Joined(Value),
    /// The app server could not be reached or refused the resume.
    Refused(String),
    Server(Value),
    Disconnected,
}

/// The thread a `resume` command line names, if it is one. Values of
/// Codex's global options are skipped.
pub fn resumed_thread(args: &[String]) -> Option<&str> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" | "-c" | "--model" | "-m" | "--profile" | "-p" | "--sandbox" | "-s"
            | "--ask-for-approval" | "-a" | "--cd" | "-C" | "--remote" => {
                args.next();
            }
            "resume" => return args.next().map(String::as_str),
            flag if flag.starts_with('-') => {}
            _ => return None,
        }
    }
    None
}

/// The server a `--remote` names, if it does.
pub fn remote(args: &[String]) -> Option<&str> {
    let at = args.iter().position(|arg| arg == "--remote")?;
    args.get(at + 1).map(String::as_str)
}

pub fn run(thread: &str, remote: &str) -> i32 {
    crate::pty::raw_mode();
    let (tx, events) = mpsc::channel();
    signals(tx.clone());
    let server = connect(remote, thread, tx.clone());
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buffer = [0u8; 1024];
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(Event::Ended);
                    return;
                }
                Ok(read) => {
                    if tx.send(Event::Bytes(buffer[..read].to_vec())).is_err() {
                        return;
                    }
                }
            }
        }
    });
    let mut cwd = std::env::current_dir()
        .map(|dir| dir.display().to_string())
        .unwrap_or_default();
    // Keys typed before the server answers wait for it.
    let mut early = Vec::new();
    let mut client = loop {
        match events.recv() {
            Ok(Event::Joined(answer)) => {
                if let Some(at) = answer["result"]["cwd"].as_str() {
                    cwd = at.to_owned();
                }
                break Remote::new(thread, server);
            }
            Ok(Event::Refused(why)) => {
                draw(&format!("fake codex: {why}"));
                return 1;
            }
            Ok(Event::Ended) | Err(_) => return 0,
            Ok(event) => early.push(event),
        }
    };
    draw(&format!("fake codex resume {thread} in {cwd}"));
    draw(&crate::pty::size_line());
    let mut line = Vec::new();
    for event in early
        .into_iter()
        .map(Ok)
        .chain(std::iter::from_fn(|| Some(events.recv())))
    {
        let Ok(event) = event else { break };
        match event {
            Event::Bytes(bytes) => {
                for byte in bytes {
                    match byte {
                        0x03 | 0x04 => return 0,
                        b'\r' | b'\n' => {
                            let typed = String::from_utf8_lossy(&line).into_owned();
                            draw(&format!("> {typed}"));
                            client.typed(&typed);
                            line.clear();
                        }
                        byte => line.push(byte),
                    }
                }
            }
            Event::Resized => draw(&crate::pty::size_line()),
            Event::Ended => return 0,
            Event::Server(frame) => client.heard(frame),
            Event::Disconnected => {
                draw("fake codex: the app server hung up");
                return 1;
            }
            Event::Joined(_) | Event::Refused(_) => {}
        }
    }
    0
}

type Outbox = tokio::sync::mpsc::UnboundedSender<Value>;

/// The view's side of its app server once it has joined the thread.
struct Remote {
    thread: String,
    out: Outbox,
    next_id: u64,
    /// The running turn, as the server last reported it.
    turn: Option<String>,
    /// Approvals waiting for an answer, oldest first: id and method.
    waiting: Vec<(Value, String)>,
}

impl Remote {
    fn new(thread: &str, out: Outbox) -> Self {
        Self {
            thread: thread.to_owned(),
            out,
            next_id: 0,
            turn: None,
            waiting: Vec::new(),
        }
    }

    fn call(&mut self, method: &str, params: Value) {
        self.next_id += 1;
        let id = format!("view-{}", self.next_id);
        let _ = self
            .out
            .send(json!({ "id": id, "method": method, "params": params }));
    }

    fn typed(&mut self, line: &str) {
        if let ("y" | "n", false) = (line, self.waiting.is_empty()) {
            let (id, method) = self.waiting.remove(0);
            let yes = line == "y";
            let result = match method.as_str() {
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                    json!({ "decision": if yes { "accept" } else { "decline" } })
                }
                "mcpServer/elicitation/request" => json!({
                    "action": if yes { "accept" } else { "decline" },
                    "content": if yes { json!({}) } else { Value::Null },
                }),
                other => {
                    draw(&format!("fake codex cannot answer {other}"));
                    return;
                }
            };
            let _ = self.out.send(json!({ "id": id, "result": result }));
            return;
        }
        let input = json!([{ "type": "text", "text": line, "text_elements": [] }]);
        let thread = self.thread.clone();
        // Codex's app names each message it sends, as amux does.
        let message = format!("view-message-{}", self.next_id + 1);
        match self.turn.clone() {
            Some(turn) => self.call(
                "turn/steer",
                json!({ "threadId": thread, "input": input, "expectedTurnId": turn,
                        "clientUserMessageId": message }),
            ),
            None => self.call(
                "turn/start",
                json!({ "threadId": thread, "input": input, "clientUserMessageId": message }),
            ),
        }
    }

    fn heard(&mut self, frame: Value) {
        let params = &frame["params"];
        match (frame["method"].as_str(), frame.get("id")) {
            (Some(method), Some(id)) => {
                let what = params["command"]
                    .as_str()
                    .or_else(|| params["message"].as_str())
                    .unwrap_or(method);
                draw(&format!("approval {id}: {what}"));
                self.waiting.push((id.clone(), method.to_owned()));
            }
            (Some("serverRequest/resolved"), None) => {
                let id = &params["requestId"];
                self.waiting.retain(|(waiting, _)| waiting != id);
                draw(&format!("resolved {id}"));
            }
            (Some("turn/started"), None) => {
                self.turn = params["turn"]["id"].as_str().map(str::to_owned);
            }
            (Some("turn/completed"), None) => {
                self.turn = None;
                let status = params["turn"]["status"].as_str().unwrap_or_default();
                draw(&format!("turn {status}"));
            }
            (Some("item/completed"), None) => {
                let item = &params["item"];
                match item["type"].as_str() {
                    Some("userMessage") => {
                        let text: Vec<&str> = item["content"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|part| part["text"].as_str())
                            .collect();
                        draw(&format!("user: {}", text.join(" ")));
                    }
                    Some("agentMessage") => {
                        draw(&format!(
                            "codex: {}",
                            item["text"].as_str().unwrap_or_default()
                        ));
                    }
                    _ => {}
                }
            }
            (None, Some(_)) if frame.get("error").is_some() => {
                let message = frame["error"]["message"].as_str().unwrap_or_default();
                draw(&format!("fake codex: {message}"));
            }
            _ => {}
        }
    }
}

/// Reaches the app server at `url` on its own thread, joins `thread`
/// there, and from then on passes along what the server sends; returns
/// where to put what the view sends.
fn connect(url: &str, thread: &str, events: mpsc::Sender<Event>) -> Outbox {
    let (out, mut outgoing) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let url = url.to_owned();
    let thread = thread.to_owned();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a tokio runtime");
        runtime.block_on(async move {
            let mut server = match join(&url, &thread).await {
                Ok((server, answer)) => {
                    let _ = events.send(Event::Joined(answer));
                    server
                }
                Err(why) => {
                    let _ = events.send(Event::Refused(why));
                    return;
                }
            };
            loop {
                tokio::select! {
                    frame = server.next() => match frame {
                        Some(frame) => {
                            if events.send(Event::Server(frame)).is_err() {
                                return;
                            }
                        }
                        None => {
                            let _ = events.send(Event::Disconnected);
                            return;
                        }
                    },
                    frame = outgoing.recv() => match frame {
                        Some(frame) => {
                            if server.send(&frame).await.is_err() {
                                let _ = events.send(Event::Disconnected);
                                return;
                            }
                        }
                        None => return,
                    },
                }
            }
        });
    });
    out
}

/// Connects and resumes `thread` the way Codex's app does, returning the
/// connection and the server's answer to the resume.
async fn join(url: &str, thread: &str) -> Result<(Server, Value), String> {
    let mut server = Server::connect(url).await?;
    server
        .call(
            "initialize",
            json!({
                "capabilities": { "experimentalApi": true },
                "clientInfo": { "name": "codex-tui", "title": null, "version": crate::codex::VERSION },
            }),
        )
        .await?;
    server.send(&json!({ "method": "initialized" })).await?;
    let answer = server
        .call(
            "thread/resume",
            json!({ "threadId": thread, "excludeTurns": true }),
        )
        .await?;
    Ok((server, answer))
}

#[cfg(unix)]
struct Server {
    socket: crate::clients::Connection,
}

#[cfg(unix)]
impl Server {
    async fn connect(url: &str) -> Result<Self, String> {
        let path = match crate::clients::Listen::from_url(url)? {
            crate::clients::Listen::Unix(path) => path,
            crate::clients::Listen::Stdio => return Err(format!("cannot attach to {url}")),
        };
        let wait = std::time::Duration::from_secs(5);
        let socket = crate::clients::connect(&path, wait)
            .await
            .map_err(|error| format!("cannot reach {url}: {error}"))?;
        Ok(Self { socket })
    }

    async fn send(&mut self, frame: &Value) -> Result<(), String> {
        use futures_util::SinkExt;
        let message = tokio_tungstenite::tungstenite::Message::text(frame.to_string());
        self.socket
            .send(message)
            .await
            .map_err(|error| error.to_string())
    }

    async fn next(&mut self) -> Option<Value> {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;
        loop {
            match self.socket.next().await? {
                Ok(Message::Text(text)) => {
                    if let Ok(frame) = serde_json::from_str(text.as_str()) {
                        return Some(frame);
                    }
                }
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }

    /// A request answered: the answer, or the server's refusal.
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = format!("view-{method}");
        self.send(&json!({ "id": id, "method": method, "params": params }))
            .await?;
        loop {
            let frame = self
                .next()
                .await
                .ok_or_else(|| "the app server hung up".to_owned())?;
            if frame["id"] == id.as_str() && frame.get("method").is_none() {
                if let Some(message) = frame["error"]["message"].as_str() {
                    return Err(message.to_owned());
                }
                return Ok(frame);
            }
        }
    }
}

/// Codex's app reaches a remote server on a Unix socket; on Windows a host
/// keeps Codex on stdio and offers no attach.
#[cfg(not(unix))]
struct Server;

#[cfg(not(unix))]
impl Server {
    async fn connect(url: &str) -> Result<Self, String> {
        Err(format!("cannot attach to {url} on Windows"))
    }

    async fn send(&mut self, _frame: &Value) -> Result<(), String> {
        Ok(())
    }

    async fn next(&mut self) -> Option<Value> {
        None
    }

    async fn call(&mut self, _method: &str, _params: Value) -> Result<Value, String> {
        Err("no server".into())
    }
}

fn draw(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = write!(stdout, "{text}\r\n");
    let _ = stdout.flush();
}

fn signals(tx: mpsc::Sender<Event>) {
    #[cfg(unix)]
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a tokio runtime");
        runtime.block_on(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let (Ok(mut resized), Ok(mut term), Ok(mut hup)) = (
                signal(SignalKind::window_change()),
                signal(SignalKind::terminate()),
                signal(SignalKind::hangup()),
            ) else {
                return;
            };
            loop {
                let event = tokio::select! {
                    _ = resized.recv() => Event::Resized,
                    _ = term.recv() => Event::Ended,
                    _ = hup.recv() => Event::Ended,
                };
                let ended = matches!(event, Event::Ended);
                if tx.send(event).is_err() || ended {
                    return;
                }
            }
        });
    });
    // A console signals nothing on a resize: the fake looks at its size
    // often enough that a test waiting for the new one sees it.
    #[cfg(windows)]
    std::thread::spawn(move || {
        let mut drawn = crate::pty::size_line();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let size = crate::pty::size_line();
            if size != drawn {
                drawn = size;
                if tx.send(Event::Resized).is_err() {
                    return;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn a_resume_command_line_names_its_thread_past_global_options() {
        assert_eq!(
            resumed_thread(&args(&["--config", "a=b", "resume", "t1"])),
            Some("t1")
        );
        assert_eq!(resumed_thread(&args(&["resume", "t2"])), Some("t2"));
        assert_eq!(
            resumed_thread(&args(&["app-server", "--listen", "stdio://"])),
            None
        );
        assert_eq!(resumed_thread(&args(&[])), None);
    }
}
