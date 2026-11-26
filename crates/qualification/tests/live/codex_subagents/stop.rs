//! A child stopped in three steps while its command runs, the first two
//! from a second client on the agent's Codex socket, as Codex's own app
//! would send them (see the parent module).

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::net::UnixStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use wire::{CodexInput, PromptInput, codex_input};

use super::super::asks::settled;
use super::super::{Install, Scenario, TURN, Verdict};
use super::{Steps, about, children, items, spawn_prompt};

/// The command a stopped child runs: long enough to outlast the stop, and
/// unique enough to find among the machine's processes.
const STOPPED_COMMAND: &str = "sleep 97; echo LATE";
const STOPPED_PATTERN: &str = "sleep 97";

/// The machine's processes whose command line holds `pattern`.
fn processes(pattern: &str) -> Vec<String> {
    std::process::Command::new("pgrep")
        .args(["-f", pattern])
        .output()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// A directory named `name` somewhere under `root`.
fn find_dir(root: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        if entry.file_name() == name {
            return Some(path);
        }
        if let Some(found) = find_dir(&path, name) {
            return Some(found);
        }
    }
    None
}

/// Another client of the agent's Codex app server, as Codex's own app is
/// one: it sends what amux has no input for.
struct AppClient {
    socket: WebSocketStream<UnixStream>,
    next: u64,
}

impl AppClient {
    async fn connect(path: &Path) -> Result<AppClient, String> {
        let stream = UnixStream::connect(path)
            .await
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let (socket, _) = tokio_tungstenite::client_async("ws://localhost/", stream)
            .await
            .map_err(|error| error.to_string())?;
        let mut client = AppClient { socket, next: 0 };
        client
            .request(
                "initialize",
                json!({
                    "clientInfo": {"name": "amux-live-stop", "title": null, "version": "0"},
                    "capabilities": {"experimentalApi": true},
                }),
            )
            .await?;
        client
            .socket
            .send(Message::text(json!({"method": "initialized"}).to_string()))
            .await
            .map_err(|error| error.to_string())?;
        Ok(client)
    }

    /// Sends a request and returns Codex's answer to it, result or error.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next += 1;
        let id = format!("live-stop-{}", self.next);
        let message = json!({"id": id, "method": method, "params": params});
        self.socket
            .send(Message::text(message.to_string()))
            .await
            .map_err(|error| error.to_string())?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let next = tokio::time::timeout_at(deadline, self.socket.next())
                .await
                .map_err(|_| format!("{method} was not answered"))?;
            let Some(Ok(Message::Text(text))) = next else {
                if next.is_none() {
                    return Err(format!("the socket closed before {method} was answered"));
                }
                continue;
            };
            if let Ok(answer) = serde_json::from_str::<Value>(text.as_str())
                && answer["id"] == id.as_str()
            {
                return Ok(answer);
            }
        }
    }
}

impl Install {
    /// Waits until `done` holds of what amux's client heard.
    async fn heard_until(
        &self,
        patience: Duration,
        what: &str,
        done: impl Fn(&[(u64, Value)]) -> bool,
    ) -> Result<Vec<(u64, Value)>, String> {
        let deadline = tokio::time::Instant::now() + patience;
        loop {
            let heard = super::heard(self.root.path());
            if done(&heard) {
                return Ok(heard);
            }
            if tokio::time::Instant::now() > deadline {
                return Err(format!("timed out after {patience:?} waiting for {what}"));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// The agent's Codex socket, where its app server listens, by a link
    /// at the top of the run's folder: the socket's own path is longer than
    /// a socket address may be.
    fn codex_socket(&self, agent: &[u8]) -> Result<PathBuf, String> {
        let id = uuid::Uuid::from_slice(agent).map_err(|error| error.to_string())?;
        let dir = find_dir(self.root.path(), &id.to_string())
            .ok_or_else(|| format!("no directory for agent {id}"))?;
        let link = self
            .root
            .path()
            .join(format!("{}.sock", &id.simple().to_string()[..8]));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(dir.join("private").join("codex.sock"), &link)
            .map_err(|error| error.to_string())?;
        Ok(link)
    }

    /// A child stopped in three steps while its command runs: its turn
    /// interrupted, its commands ended, and the parent told by a message
    /// steered into its turn. Each step's effect is noted: whether the
    /// child's turn ends, whether its command's process is gone, and
    /// whether the parent stops waiting and finishes its turn.
    pub(super) async fn child_stop(&self) -> Result<Verdict, String> {
        let scenario = Scenario::ChildStop;
        let (agent, follow) = match self
            .ready_agent(scenario, Some(AgentSettings::default()))
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        let mut steps = Steps::new(scenario);
        if !processes(STOPPED_PATTERN).is_empty() {
            return Err(format!("`{STOPPED_PATTERN}` already runs on this machine"));
        }
        let parent = self.parent_thread(&follow)?;
        self.send(
            scenario,
            &spawn_prompt(&format!(
                "run the shell command `{STOPPED_COMMAND}` once with your shell tool, wait for \
                 it to finish, and reply with its output."
            )),
        )
        .await?;
        let heard = self
            .heard_until(TURN, "the child's command to start", |heard| {
                children(heard, &parent).iter().any(|child| {
                    !items(heard, child, "item/started", "commandExecution").is_empty()
                })
            })
            .await?;
        let child = children(&heard, &parent)
            .into_iter()
            .find(|child| !items(&heard, child, "item/started", "commandExecution").is_empty())
            .ok_or("no child")?;
        let command = items(&heard, &child, "item/started", "commandExecution")[0].clone();
        let child_turn = about(&heard, &child, "item/started")
            .into_iter()
            .find(|(_, message)| message["params"]["item"]["id"] == command["id"])
            .and_then(|(_, message)| message["params"]["turnId"].as_str())
            .ok_or("the command names no turn")?
            .to_owned();
        steps.note(format!(
            "parent {parent}, child {child}, child turn {child_turn}, command item {} \
             processId {}",
            command["id"], command["processId"]
        ));
        tokio::time::sleep(Duration::from_secs(3)).await;
        steps.note(format!(
            "before any step: `{STOPPED_PATTERN}` processes {:?}",
            processes(STOPPED_PATTERN)
        ));
        let mut app = AppClient::connect(&self.codex_socket(&agent)?).await?;

        // Step one: the child's turn interrupted.
        let answer = app
            .request(
                "turn/interrupt",
                json!({"threadId": child, "turnId": child_turn}),
            )
            .await?;
        steps.note(format!(
            "step 1 turn/interrupt on the child answered {answer}"
        ));
        let child_ended =
            |heard: &[(u64, Value)]| !about(heard, &child, "turn/completed").is_empty();
        let ended = self
            .heard_until(
                Duration::from_secs(10),
                "the child's turn to end",
                child_ended,
            )
            .await;
        // Long enough to see whether the interrupt alone ends the command.
        tokio::time::sleep(Duration::from_secs(5)).await;
        let heard = super::heard(self.root.path());
        steps.note(format!(
            "after step 1: child turn/completed {}; command item completed {}; parent \
             turn/completed {}; `{STOPPED_PATTERN}` processes {:?}",
            match &ended {
                Ok(_) => about(&heard, &child, "turn/completed")
                    .first()
                    .map(|(_, message)| message["params"]["turn"]["status"].to_string())
                    .unwrap_or_default(),
                Err(why) => why.clone(),
            },
            !items(&heard, &child, "item/completed", "commandExecution").is_empty(),
            !about(&heard, &parent, "turn/completed").is_empty(),
            processes(STOPPED_PATTERN),
        ));

        // Step two: the commands it still runs, ended.
        let listed = app
            .request(
                "thread/backgroundTerminals/list",
                json!({"threadId": child}),
            )
            .await?;
        steps.note(format!(
            "step 2 thread/backgroundTerminals/list on the child answered {listed}"
        ));
        let mut ids: Vec<String> = listed["result"]["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|terminal| terminal["processId"].as_str().map(str::to_owned))
            .collect();
        if let Some(id) = command["processId"].as_str()
            && !ids.iter().any(|known| known == id)
        {
            ids.push(id.to_owned());
        }
        for id in &ids {
            let answer = app
                .request(
                    "thread/backgroundTerminals/terminate",
                    json!({"threadId": child, "processId": id}),
                )
                .await?;
            steps.note(format!("step 2 terminate {id} answered {answer}"));
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
        let heard = super::heard(self.root.path());
        let completed = items(&heard, &child, "item/completed", "commandExecution");
        steps.note(format!(
            "after step 2: command item completed {:?}; parent turn/completed {}; \
             `{STOPPED_PATTERN}` processes {:?}",
            completed
                .iter()
                .map(|item| format!("status={} exitCode={}", item["status"], item["exitCode"]))
                .collect::<Vec<_>>(),
            !about(&heard, &parent, "turn/completed").is_empty(),
            processes(STOPPED_PATTERN),
        ));
        // Is the parent told without the third step?
        let alone = self
            .heard_until(
                Duration::from_secs(15),
                "the parent to finish alone",
                |heard| !about(heard, &parent, "turn/completed").is_empty(),
            )
            .await
            .is_ok();
        steps.note(format!(
            "15 s later, the parent finished on its own: {alone}"
        ));

        // Step three: the parent told, by a prompt steered into its turn
        // (queued, then sent now: `turn/steer`).
        if !alone {
            let queued = self
                .send_input(
                    &agent,
                    wire::KindInput::Codex(CodexInput {
                        of: Some(codex_input::Of::Prompt(PromptInput {
                            text: "I stopped the agent you started; it will not answer. Stop \
                                   waiting for it and reply with exactly STOPPED."
                                .into(),
                            attachments: Vec::new(),
                        })),
                    }),
                )
                .await?;
            self.input(
                &agent,
                wire::KindInput::Codex(CodexInput {
                    of: Some(codex_input::Of::SendNow(wire::SendQueuedNow {
                        queued_input_id: queued,
                    })),
                }),
            )
            .await?;
            steps.note("step 3 a prompt steered into the parent's running turn");
        }
        let kind = self.kind;
        let finished = follow
            .until(TURN, "the parent's turn to end", |log| settled(kind, log))
            .await;
        let heard = super::heard(self.root.path());
        steps.note(format!(
            "after step 3: parent turn {:?}; parent turn/completed {:?}; child turn/completed \
             {}; `{STOPPED_PATTERN}` processes {:?}",
            finished,
            about(&heard, &parent, "turn/completed")
                .iter()
                .map(|(_, message)| message["params"]["turn"]["status"].to_string())
                .collect::<Vec<_>>(),
            about(&heard, &child, "turn/completed").len(),
            processes(STOPPED_PATTERN),
        ));
        if finished.is_err() {
            // Left waiting: amux interrupts it so the recording ends.
            self.input(&agent, self.interrupt()).await?;
            follow
                .until(TURN, "the parent's turn to end", |log| settled(kind, log))
                .await?;
            steps.note("the parent's turn was interrupted from amux to end the recording");
        }
        let left = processes(STOPPED_PATTERN);
        for pid in &left {
            if let Some(process) = pid
                .parse()
                .ok()
                .and_then(|pid| os::process::ProcessRef::open(pid).ok())
            {
                let _ = process.kill();
            }
        }
        if !left.is_empty() {
            return Err(format!("the child's command still runs: {left:?}"));
        }
        if finished.is_err() {
            return Err("the parent did not finish its turn after the three steps".into());
        }
        self.judged(scenario, &follow)
    }
}
