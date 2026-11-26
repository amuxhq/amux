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
use std::time::Instant;

use serde_json::{Value, json};
use ui_state::OpenAsk;
use wire::{AgentSettings, codex_answer, codex_ask};

use super::asks::{Answer, settled};
use super::{Install, Scenario, TURN, Verdict, expect_outcome};

// out of scope on Windows: attach. The stop's first two steps go from a
// second client on the agent's Codex socket, which a Windows host doesn't
// open: it runs Codex over stdio.
#[cfg(unix)]
mod stop;

/// The probe's tool server that logs what each call carries (see
/// `codex-probe __meta-server`), and its name in Codex's config.
const META_SERVER: &str = "meta";

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

    /// A child thread runs a command and its parent waits for its answer.
    pub(super) async fn child_command(&self) -> Result<Verdict, String> {
        let scenario = Scenario::ChildCommand;
        let (agent, follow) = match self
            .ready_agent(scenario, Some(AgentSettings::default()))
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
                Some(AgentSettings {
                    permission: Some("read-only".into()),
                    ..AgentSettings::default()
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
            .ready_agent(scenario, Some(AgentSettings::default()))
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
            .ready_agent(scenario, Some(AgentSettings::default()))
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
            .ready_agent(scenario, Some(AgentSettings::default()))
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(scenario, prompt).await?;
        self.settle(&agent, &follow, "the work to finish", |ask| match ask {
            OpenAsk::Codex(codex) if matches!(codex.body, Some(codex_ask::Body::McpTool(_))) => {
                Ok(Answer::Codex(codex_answer::Of::Approve(
                    wire::CodexDecision::Approve as i32,
                )))
            }
            _ => self.by_the_way(ask),
        })
        .await?;
        self.judged(scenario, &follow)
    }
}

/// The second turn's prompt to a parent with one child.
const FOLLOW_UP: &str = "Send the agent you started a follow-up: ask it to reply with exactly \
     SECOND_ANSWER. Wait for it to finish and reply with what it answered.";
