//! A scripted ACP client: just enough JSON-RPC to drive an agent through a
//! scenario, kept on `serde_json::Value` so it records what the agent
//! actually sent rather than what a typed model of the protocol expects.
//!
//! It speaks to the agent through `acp-probe`, which records every line,
//! so the client keeps only what a scenario needs to decide its next step
//! and to say what it saw.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

/// How a permission request is answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    AllowOnce,
    AllowAlways,
    RejectOnce,
}

impl Permission {
    /// The option kinds this answer picks, best first. A one-time answer
    /// never widens to an "always" one: under the owner's real login that
    /// would leave a rule behind no scenario asked for. Where the agent
    /// offers no such option the request is cancelled instead.
    fn kinds(self) -> &'static [&'static str] {
        match self {
            Permission::AllowOnce => &["allow_once"],
            Permission::AllowAlways => &["allow_always", "allow_once"],
            Permission::RejectOnce => &["reject_once"],
        }
    }
}

/// One agent process, with its probe in front of it.
pub struct Conn {
    child: Child,
    /// The probe's process group, which is its own pid. Kept from the
    /// start: once the child is reaped tokio no longer knows its pid, and
    /// the agent it started may still be running in the group.
    #[cfg_attr(not(unix), allow(dead_code))]
    group: Option<u32>,
    stdin: Option<ChildStdin>,
    incoming: mpsc::UnboundedReceiver<Value>,
    next_id: u64,
    responses: HashMap<u64, Value>,
    /// Every message the agent sent, in order.
    pub seen: Vec<Value>,
    pub permission: Permission,
    /// Whether a form elicitation is accepted (else declined).
    pub accept_forms: bool,
    /// When the process started, for the startup times a scenario reports.
    pub started: Instant,
}

/// The client's side of `initialize`: no file-system or terminal
/// capabilities (amux does neither for an agent, which does its own I/O),
/// and both kinds of elicitation, which amux shows as asks.
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": 1,
        "clientCapabilities": {"elicitation": {"form": {}, "url": {}}},
        "clientInfo": {"name": "amux", "title": "amux", "version": env!("CARGO_PKG_VERSION")},
    })
}

pub fn text(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

impl Conn {
    /// Starts `command` in `cwd` through the probe, recording into
    /// `capture`, with `env` added to this process's environment.
    pub fn start(
        probe: &Path,
        command: &[&str],
        cwd: &Path,
        capture: &Path,
        env: &[(&str, OsString)],
    ) -> Result<Conn, String> {
        let mut process = Command::new(probe);
        process
            .arg("--")
            .args(command)
            .current_dir(cwd)
            .env("ACP_CAPTURE_DIR", capture)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // The probe keeps the agent's stderr in the capture.
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for (name, value) in env {
            process.env(name, value);
        }
        // Its own process group, so ending a scenario ends whatever the
        // agent started too (its tool servers, its shells).
        #[cfg(unix)]
        process.process_group(0);
        let mut child = process
            .spawn()
            .map_err(|error| format!("starting {}: {error}", probe.display()))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let (tx, incoming) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // A line that is not JSON is the agent's noise on stdout;
                // the probe has it, and it answers nothing.
                if let Ok(message) = serde_json::from_str::<Value>(&line)
                    && tx.send(message).is_err()
                {
                    break;
                }
            }
        });
        Ok(Conn {
            group: child.id(),
            child,
            stdin,
            incoming,
            next_id: 0,
            responses: HashMap::new(),
            seen: Vec::new(),
            permission: Permission::AllowOnce,
            accept_forms: true,
            started: Instant::now(),
        })
    }

    pub async fn send(&mut self, message: Value) -> Result<(), String> {
        let stdin = self.stdin.as_mut().ok_or("the agent's stdin is closed")?;
        stdin
            .write_all(format!("{message}\n").as_bytes())
            .await
            .map_err(|error| format!("writing to the agent: {error}"))?;
        stdin.flush().await.map_err(|error| error.to_string())
    }

    pub async fn request(&mut self, method: &str, params: Value) -> Result<u64, String> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await?;
        Ok(id)
    }

    pub async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await
    }

    /// The agent's answer to request `id`, whole (`result` or `error`).
    pub async fn response(&mut self, id: u64, within: Duration) -> Result<Value, String> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(response) = self.responses.remove(&id) {
                return Ok(response);
            }
            self.next(deadline).await.map_err(|error| {
                format!(
                    "waiting {}s for the answer to request {id}: {error}",
                    within.as_secs()
                )
            })?;
        }
    }

    pub async fn call(
        &mut self,
        method: &str,
        params: Value,
        within: Duration,
    ) -> Result<Value, String> {
        let id = self.request(method, params).await?;
        self.response(id, within).await
    }

    /// Reads until a message `wanted` accepts arrives, and returns it.
    pub async fn until(
        &mut self,
        within: Duration,
        wanted: impl Fn(&Value) -> bool,
    ) -> Result<Value, String> {
        let deadline = Instant::now() + within;
        loop {
            let message = self.next(deadline).await?;
            if wanted(&message) {
                return Ok(message);
            }
        }
    }

    /// Reads until the agent has been quiet for `quiet`, or `cap` has
    /// passed: what trails a turn's answer (late updates, title changes).
    pub async fn settle(&mut self, quiet: Duration, cap: Duration) {
        let end = Instant::now() + cap;
        loop {
            let deadline = (Instant::now() + quiet).min(end);
            if self.next(deadline).await.is_err() || Instant::now() >= end {
                return;
            }
        }
    }

    /// The next message, with any request in it answered.
    async fn next(&mut self, deadline: Instant) -> Result<Value, String> {
        let wait = deadline.saturating_duration_since(Instant::now());
        let message = match tokio::time::timeout(wait, self.incoming.recv()).await {
            Err(_) => return Err("timed out".into()),
            Ok(None) => return Err("the agent's stdout closed".into()),
            Ok(Some(message)) => message,
        };
        self.seen.push(message.clone());
        let has_method = message.get("method").is_some();
        match message.get("id").and_then(Value::as_u64) {
            Some(id) if !has_method => {
                self.responses.insert(id, message.clone());
            }
            _ if has_method && message.get("id").is_some() => {
                let answer = self.answer(&message);
                self.send(answer).await?;
            }
            _ => {}
        }
        Ok(message)
    }

    /// What the client answers a request from the agent: permission by
    /// the scenario's policy, forms filled or declined, links accepted,
    /// Grok's questions with their first options, and anything else (the file-system and terminal methods the client
    /// did not offer) refused as unknown.
    fn answer(&self, request: &Value) -> Value {
        let id = request["id"].clone();
        let params = &request["params"];
        let result = match request["method"].as_str().unwrap_or_default() {
            "session/request_permission" => {
                let options = params["options"].as_array().cloned().unwrap_or_default();
                let chosen = self.permission.kinds().iter().find_map(|kind| {
                    options
                        .iter()
                        .find(|option| option["kind"] == *kind)
                        .map(|option| option["optionId"].clone())
                });
                match chosen {
                    Some(option) => {
                        json!({"outcome": {"outcome": "selected", "optionId": option}})
                    }
                    None => json!({"outcome": {"outcome": "cancelled"}}),
                }
            }
            "elicitation/create" if params["mode"] == "url" => json!({"action": "accept"}),
            "elicitation/create" if self.accept_forms => {
                json!({"action": "accept", "content": fill(&params["requestedSchema"])})
            }
            "elicitation/create" => json!({"action": "decline"}),
            // Grok's questions: each answered with its first option, by its
            // label under the question's text.
            "_x.ai/ask_user_question" => {
                let answers: serde_json::Map<String, Value> = params["questions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|question| {
                        let label = question["options"][0]["label"].clone();
                        Some((question["question"].as_str()?.to_owned(), label))
                    })
                    .collect();
                json!({"outcome": "accepted", "answers": answers})
            }
            method => {
                return json!({"jsonrpc": "2.0", "id": id, "error": {
                    "code": -32601, "message": format!("method not found: {method}"),
                }});
            }
        };
        json!({"jsonrpc": "2.0", "id": id, "result": result})
    }

    /// Ends the process as amux ends an agent: its stdin closed, a few
    /// seconds to go, then its whole process group killed.
    pub async fn end(mut self) {
        self.stdin = None;
        let _ = tokio::time::timeout(Duration::from_secs(8), self.child.wait()).await;
        self.kill_group();
    }

    fn kill_group(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.group {
            // SAFETY: signalling a process group this client created.
            unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
        }
        let _ = self.child.start_kill();
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.kill_group();
    }
}

/// A form's answer: every property given a plausible value of its type.
fn fill(schema: &Value) -> Value {
    let mut content = serde_json::Map::new();
    if let Some(properties) = schema["properties"].as_object() {
        for (name, property) in properties {
            let value = if let Some(first) = property["enum"].as_array().and_then(|e| e.first()) {
                first.clone()
            } else if let Some(default) = property.get("default") {
                default.clone()
            } else {
                match property["type"].as_str() {
                    Some("boolean") => json!(true),
                    Some("number" | "integer") => json!(1),
                    Some("array") => json!([]),
                    _ => json!("kiwi"),
                }
            };
            content.insert(name.clone(), value);
        }
    }
    Value::Object(content)
}
