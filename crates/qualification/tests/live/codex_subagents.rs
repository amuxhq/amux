//! Codex's own subagents: child threads a parent starts with its
//! `spawn_agent` tool, recorded so the interpreter can learn to give each
//! one a chat of its own. Codex sends a child's events and asks on the
//! parent's app-server connection, naming the child's thread.
//!
//! A child running a command, its approval answered from amux, a follow-up
//! the parent sends it, the same follow-up after the parent is resumed,
//! amux's tools and another tool server called from it, and a stop in three
//! steps. Codex refuses direct turns to a child and amux has no input that
//! addresses one, so the stop's first two steps (interrupt the child's turn,
//! end the commands it still runs) go from a second client on the agent's
//! Codex socket, as Codex's own app would send them; the capture keeps that
//! client's lines out of the recording, so what they caused reaches amux as
//! what Codex tells it. The third step, the note that the person stopped
//! it, is a prompt amux steers into the parent's running turn. What each
//! step did is written beside the recording as `steps.txt`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::net::UnixStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use ui_state::OpenAsk;
use wire::{CodexCreateConfig, CodexInput, PromptInput, codex_answer, codex_ask, codex_input};

use super::asks::{Answer, settled};
use super::{Install, Scenario, TURN, Verdict, expect_outcome};

/// The probe's tool server that logs what each call carries (see
/// `codex-probe __meta-server`), and its name in Codex's config.
const META_SERVER: &str = "meta";

/// The command a stopped child runs: long enough to outlast the stop, and
/// unique enough to find among the machine's processes.
const STOPPED_COMMAND: &str = "sleep 97; echo LATE";
const STOPPED_PATTERN: &str = "sleep 97";

/// Starts one child with exactly `task` and waits for it.
fn spawn_prompt(task: &str) -> String {
    format!(
        "Call your spawn_agent tool directly, without reading any file or using any skill, to \
         start exactly one agent whose task is: {task} Then wait for it to finish and reply \
         with what it answered."
    )
}

/// What amux's own client heard from Codex in this run, as JSON messages,
/// with the time the proxy recorded each.
fn heard(root: &Path) -> Vec<(u64, Value)> {
    std::fs::read_to_string(root.join("capture").join("io.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|row| row["transport_id"] == "amux" && row["dir"] == "stdout")
        .filter_map(|row| {
            let at = row["us"].as_u64().unwrap_or_default();
            serde_json::from_str::<Value>(row["line"].as_str()?)
                .ok()
                .map(|message| (at, message))
        })
        .collect()
}

/// The threads `parent` started as subagents, in order.
fn children(heard: &[(u64, Value)], parent: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (_, message) in heard {
        let params = &message["params"];
        let item = &params["item"];
        if message["method"] == "item/started"
            && params["threadId"] == parent
            && item["type"] == "subAgentActivity"
            && item["kind"] == "started"
            && let Some(child) = item["agentThreadId"].as_str()
            && !found.iter().any(|known| known == child)
        {
            found.push(child.to_owned());
        }
    }
    found
}

/// The `method` notifications about `thread`.
fn about<'a>(heard: &'a [(u64, Value)], thread: &str, method: &str) -> Vec<&'a (u64, Value)> {
    heard
        .iter()
        .filter(|(_, message)| {
            message["method"] == method && message["params"]["threadId"] == thread
        })
        .collect()
}

/// The items of `kind` that started or completed on `thread`.
fn items<'a>(heard: &'a [(u64, Value)], thread: &str, method: &str, kind: &str) -> Vec<&'a Value> {
    about(heard, thread, method)
        .into_iter()
        .map(|(_, message)| &message["params"]["item"])
        .filter(|item| item["type"] == kind)
        .collect()
}

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

/// What a scenario saw, step by step, kept beside its recording.
struct Steps {
    started: Instant,
    lines: Vec<String>,
    path: PathBuf,
}

impl Steps {
    fn new(scenario: Scenario) -> Steps {
        let dir = super::capture::recordings_dir("codex").join(scenario.name());
        let _ = std::fs::create_dir_all(&dir);
        Steps {
            started: Instant::now(),
            lines: Vec::new(),
            path: dir.join("steps.txt"),
        }
    }

    fn note(&mut self, line: impl Into<String>) {
        let line = format!(
            "{:>7.1}s {}",
            self.started.elapsed().as_secs_f64(),
            line.into()
        );
        eprintln!("  {line}");
        self.lines.push(line);
        let _ = std::fs::write(&self.path, self.lines.join("\n") + "\n");
    }
}

impl Install {
    /// The parent's thread: the provider session amux reports.
    fn parent_thread(&self, follow: &super::Follow) -> Result<String, String> {
        follow
            .snapshot()
            .state(self.kind)
            .provider_session
            .ok_or_else(|| "the agent reported no provider session".to_owned())
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

    /// Waits until `done` holds of what amux's client heard.
    async fn heard_until(
        &self,
        patience: Duration,
        what: &str,
        done: impl Fn(&[(u64, Value)]) -> bool,
    ) -> Result<Vec<(u64, Value)>, String> {
        let deadline = tokio::time::Instant::now() + patience;
        loop {
            let heard = self::heard(self.root.path());
            if done(&heard) {
                return Ok(heard);
            }
            if tokio::time::Instant::now() > deadline {
                return Err(format!("timed out after {patience:?} waiting for {what}"));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// A child thread runs a command and its parent waits for its answer.
    pub(super) async fn child_command(&self) -> Result<Verdict, String> {
        let scenario = Scenario::ChildCommand;
        let (agent, follow) = match self
            .ready_agent(scenario, Some(CodexCreateConfig::default()))
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        let parent = self.parent_thread(&follow)?;
        self.send(
            scenario,
            &spawn_prompt(
                "run the shell command `echo CHILD_RAN` once with your shell tool and reply with \
                 its output.",
            ),
        )
        .await?;
        self.settle(&agent, &follow, "the work to finish", |ask| {
            self.by_the_way(ask)
        })
        .await?;
        let heard = self::heard(self.root.path());
        let kids = children(&heard, &parent);
        let [child] = kids.as_slice() else {
            return Err(format!("{} children started, not one", kids.len()));
        };
        let ran = items(&heard, child, "item/completed", "commandExecution");
        if !ran.iter().any(|item| {
            item["aggregatedOutput"]
                .as_str()
                .is_some_and(|out| out.contains("CHILD_RAN"))
        }) {
            return Err(format!(
                "the child ran no command printing CHILD_RAN: {ran:?}"
            ));
        }
        // The subagent's title is Codex's nickname for the child, which
        // only amux's `thread/read` of it gets.
        if !heard.iter().any(|(_, message)| {
            let thread = &message["result"]["thread"];
            thread["id"] == child.as_str()
                && thread["agentNickname"]
                    .as_str()
                    .is_some_and(|nickname| !nickname.is_empty())
        }) {
            return Err("amux's read of the child came back with no nickname".to_owned());
        }
        self.judged(scenario, &follow)
    }

    /// A child's command asks to leave the read-only sandbox, and amux
    /// approves it: the ask names the child's thread and the file exists.
    pub(super) async fn child_approval(&self) -> Result<Verdict, String> {
        let scenario = Scenario::ChildApproval;
        let effect = self.project(scenario)?.join("child_decided.txt");
        let (agent, follow) = match self
            .ready_agent(
                scenario,
                Some(CodexCreateConfig {
                    permission: Some("read-only".into()),
                    ..CodexCreateConfig::default()
                }),
            )
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        let parent = self.parent_thread(&follow)?;
        self.send(
            scenario,
            &spawn_prompt(
                "run exactly this shell command once, with your shell tool, and nothing else: \
                 touch child_decided.txt. The sandbox is read-only, so request escalated \
                 permissions for it; the user will approve. Then reply with the single word done.",
            ),
        )
        .await?;
        let mut asked = 0;
        self.settle(&agent, &follow, "the work to finish", |ask| {
            if let OpenAsk::Codex(_) = ask {
                asked += 1;
            }
            self.by_the_way(ask)
        })
        .await?;
        if asked == 0 {
            return Err("nothing was asked".into());
        }
        if !effect.exists() {
            return Err(format!("approved, but {} does not exist", effect.display()));
        }
        let heard = self::heard(self.root.path());
        let kids = children(&heard, &parent);
        let asks_from_children = heard
            .iter()
            .filter(|(_, message)| {
                message.get("id").is_some()
                    && message["method"]
                        .as_str()
                        .is_some_and(|method| method.ends_with("requestApproval"))
                    && message["params"]["threadId"]
                        .as_str()
                        .is_some_and(|thread| kids.iter().any(|kid| kid == thread))
            })
            .count();
        if asks_from_children == 0 {
            return Err(format!("no approval named a child thread of {kids:?}"));
        }
        self.judged(scenario, &follow)
    }

    /// The parent sends its child a follow-up in a second turn: the child
    /// runs a second turn of its own.
    pub(super) async fn child_continued(&self) -> Result<Verdict, String> {
        let scenario = Scenario::ChildContinued;
        let (agent, follow) = match self
            .ready_agent(scenario, Some(CodexCreateConfig::default()))
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        let parent = self.parent_thread(&follow)?;
        self.send(scenario, &spawn_prompt("reply with exactly FIRST_ANSWER."))
            .await?;
        expect_outcome(follow.turn_end(1).await?, wire::TurnOutcome::Completed)?;
        self.send(scenario, FOLLOW_UP).await?;
        self.settle(&agent, &follow, "the follow-up to finish", |ask| {
            self.by_the_way(ask)
        })
        .await?;
        let heard = self::heard(self.root.path());
        let kids = children(&heard, &parent);
        let [child] = kids.as_slice() else {
            return Err(format!("{} children started, not one", kids.len()));
        };
        let turns = about(&heard, child, "turn/completed").len();
        if turns < 2 {
            return Err(format!("the child ran {turns} turns, not two"));
        }
        self.judged(scenario, &follow)
    }

    /// The parent starts a child, is settled and resumed by amux, then
    /// sends the child a follow-up: amux resumes only the parent's thread,
    /// and the child's second turn is what it hears without asking.
    pub(super) async fn child_resume(&self) -> Result<Verdict, String> {
        let scenario = Scenario::ChildResume;
        let name = scenario.agent();
        let (_agent, follow) = match self
            .ready_agent(scenario, Some(CodexCreateConfig::default()))
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        let mut steps = Steps::new(scenario);
        let parent = self.parent_thread(&follow)?;
        self.send(scenario, &spawn_prompt("reply with exactly FIRST_ANSWER."))
            .await?;
        expect_outcome(follow.turn_end(1).await?, wire::TurnOutcome::Completed)?;
        let heard_before = self::heard(self.root.path());
        let kids = children(&heard_before, &parent);
        let [child] = kids.as_slice() else {
            return Err(format!("{} children started, not one", kids.len()));
        };
        let child = child.clone();
        steps.note(format!("parent {parent} started child {child}"));
        self.amux(&["settle", &name]).await?;
        self.listed(&name, "settled").await?;
        steps.note("parent settled: its Codex exits");
        self.amux(&["resume", &name]).await?;
        self.listed(&name, "idle").await?;
        steps.note("parent resumed");
        let mark = self::heard(self.root.path()).len();
        self.send(scenario, FOLLOW_UP).await?;
        let kind = self.kind;
        // The follow-up's turn may fail to reach the child: whatever it
        // does, it ends, and the recording shows what Codex did.
        follow
            .until(TURN * 2, "the follow-up to finish", |log| {
                settled(kind, log) && log.turns(kind).len() >= 2
            })
            .await?;
        let heard = self::heard(self.root.path());
        let after = &heard[mark.min(heard.len())..];
        let child_turns = about(after, &child, "turn/started").len();
        let child_items = after
            .iter()
            .filter(|(_, message)| {
                message["params"]["threadId"] == child.as_str()
                    && message["method"]
                        .as_str()
                        .is_some_and(|method| method.starts_with("item/"))
            })
            .count();
        let new_children = children(after, &parent);
        steps.note(format!(
            "after the resume amux heard {child_turns} turn starts and {child_items} item \
             events on the child; children started since: {new_children:?}"
        ));
        let traffic = self.traffic(scenario)?;
        let resumed_child = traffic.lines().any(|line| {
            line.contains(r#"\"method\":\"thread/resume\""#)
                && line.contains(child.as_str())
                && line.contains(r#""dir":"stdin""#)
        });
        steps.note(format!(
            "amux sent thread/resume for the child: {resumed_child}"
        ));
        for outcome in follow.snapshot().turns(kind) {
            steps.note(format!("parent turn ended {outcome:?}"));
        }
        if child_turns == 0 && new_children.is_empty() {
            return Err("after the resume no child ran the follow-up".into());
        }
        // The exit between the two Codex processes is the daemon's own row
        // and phase; no line of Codex's traffic makes it.
        self.note_model(&follow);
        let mut recorded = self.captured(scenario)?.shape(kind);
        let mut live = follow.snapshot().shape(kind);
        live.items.retain(|item| item != "boundary exited");
        for shape in [&mut recorded, &mut live] {
            shape.phases.retain(|phase| *phase != wire::Phase::Starting);
            shape.phases.dedup();
        }
        super::judge(&recorded, &live)?;
        Ok(Verdict::Pass)
    }

    /// The parent and its child each call amux's `status` tool and the
    /// probe's tool server, which logs what each call carries: whether the
    /// child starts tool servers of its own, and the `_meta` its calls
    /// carry.
    pub(super) async fn child_tools(&self) -> Result<Verdict, String> {
        let scenario = Scenario::ChildTools;
        let log = self.root.path().join("meta-server.jsonl");
        let _ = std::fs::remove_file(&log);
        let probe = provider_fakes::binaries::path("codex-probe");
        let _home = self.configured(&format!(
            "[mcp_servers.{META_SERVER}]\ncommand = {}\nargs = [\"__meta-server\", {}]\n",
            json!(probe),
            json!(log),
        ))?;
        let verdict = self
            .child_work(
                scenario,
                &format!(
                    "First call the whoami tool from the {META_SERVER} tool server yourself with \
                     caller parent, and call the status tool from the amux tool server with text \
                     parent-checking. {}",
                    spawn_prompt(&format!(
                        "call the whoami tool from the {META_SERVER} tool server once with caller \
                         child, then call the status tool from the amux tool server with text \
                         child-checking, then reply with what whoami returned."
                    ))
                ),
            )
            .await?;
        let kept = super::capture::recordings_dir("codex")
            .join(scenario.name())
            .join("meta-server.jsonl");
        let _ = std::fs::create_dir_all(kept.parent().unwrap_or(Path::new(".")));
        std::fs::copy(&log, &kept).map_err(|error| format!("{}: {error}", log.display()))?;
        let calls = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .filter_map(|row| serde_json::from_str::<Value>(row).ok())
            .filter_map(|row| serde_json::from_str::<Value>(row["line"].as_str()?).ok())
            .filter(|message| message["method"] == "tools/call")
            .count();
        if matches!(verdict, Verdict::Pass) && calls < 2 {
            return Err(format!("the tool server heard {calls} calls, not two"));
        }
        Ok(verdict)
    }

    /// A prompt to a new agent of the default config, its asks allowed,
    /// judged against its own traffic.
    async fn child_work(&self, scenario: Scenario, prompt: &str) -> Result<Verdict, String> {
        let (agent, follow) = match self
            .ready_agent(scenario, Some(CodexCreateConfig::default()))
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(scenario, prompt).await?;
        self.settle(&agent, &follow, "the work to finish", |ask| match ask {
            OpenAsk::Codex(codex) if matches!(codex.body, Some(codex_ask::Body::McpTool(_))) => Ok(
                Answer::Codex(codex_answer::Of::Approve(wire::Decision::Approve as i32)),
            ),
            _ => self.by_the_way(ask),
        })
        .await?;
        self.judged(scenario, &follow)
    }

    /// A child stopped in three steps while its command runs: its turn
    /// interrupted, its commands ended, and the parent told by a message
    /// steered into its turn. Each step's effect is noted: whether the
    /// child's turn ends, whether its command's process is gone, and
    /// whether the parent stops waiting and finishes its turn.
    pub(super) async fn child_stop(&self) -> Result<Verdict, String> {
        let scenario = Scenario::ChildStop;
        let (agent, follow) = match self
            .ready_agent(scenario, Some(CodexCreateConfig::default()))
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
        let heard = self::heard(self.root.path());
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
        let heard = self::heard(self.root.path());
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
        let heard = self::heard(self.root.path());
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
            let _ = std::process::Command::new("kill").arg(pid).status();
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

/// The second turn's prompt to a parent with one child.
const FOLLOW_UP: &str = "Send the agent you started a follow-up: ask it to reply with exactly \
     SECOND_ANSWER. Wait for it to finish and reply with what it answered.";
