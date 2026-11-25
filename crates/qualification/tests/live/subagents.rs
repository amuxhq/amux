//! Headless Claude's own subagents: Claude's Agent tool starting subagents
//! inside the agent's Claude, recorded with the forwarding options amux's
//! initialize sets, so each subagent's whole conversation, nested ones
//! included, is in the stream.
//!
//! Each scenario is something a subagent does that amux must later draw as
//! an agent of its own: several running at once, one starting its own,
//! one asking, one stopped (in the background and while its parent waits),
//! one continued by its parent, one live when its host is stopped and
//! resumed, one calling amux's tools, and one whose tool server asks a
//! question mid-call. Like the rest of the corpus they are judged by the
//! real effect where there is one and by the shape of their own traffic
//! replayed through the interpreter; what Claude says is never judged.

use std::sync::Mutex;
use std::time::Duration;

use ui_state::OpenAsk;
use wire::{ClaudeSdkInput, Kind, Phase, claude_answer, claude_sdk_input};

use super::asks::Answer;
use super::corpus::tools;
use super::{Follow, Install, Log, READY, Scenario, Verdict};

/// How long a session must hold still before it reads finished: a
/// subagent's end starts a new parent turn a moment after the parent goes
/// idle, so one idle frame is not the end.
const QUIET: Duration = Duration::from_secs(10);
/// How long a subagent may take to get to its long command.
const UNDER_WAY: Duration = Duration::from_secs(180);

/// The tool the form-asking tool server offers (`claude-probe` run as
/// one, see its `__tool-server` mode).
const ASK_TOOL: &str = "mcp__external__ask_the_operator";

fn sdk(of: claude_sdk_input::Of) -> wire::KindInput {
    wire::KindInput::ClaudeSdk(ClaudeSdkInput { of: Some(of) })
}

/// The calls that started subagents.
fn agent_calls(log: &Log) -> Vec<wire::ClaudeToolCall> {
    tools(log)
        .into_iter()
        .filter(|call| call.name == "Agent" || call.name == "Task")
        .collect()
}

fn running(call: &wire::ClaudeToolCall) -> bool {
    call.state() == wire::ToolState::Running
}

/// The subagent jobs the agent lists as running.
fn subagent_jobs(log: &Log) -> Vec<wire::BackgroundJob> {
    log.state(Kind::ClaudeSdk)
        .background
        .jobs
        .into_iter()
        .filter(|job| job.kind() == wire::BackgroundKind::Subagent)
        .collect()
}

/// Idle with nothing asked, every subagent call `n` or more of them, all
/// ended, and no job still running.
fn finished(log: &Log, n: usize) -> bool {
    let calls = agent_calls(log);
    log.phase() == Phase::Idle
        && log.state(Kind::ClaudeSdk).asks.is_empty()
        && !log.turns(Kind::ClaudeSdk).is_empty()
        && calls.len() >= n
        && !calls.iter().any(running)
        && subagent_jobs(log).is_empty()
}

/// The tools subagents called, in the recording a scenario's fixture
/// replays.
fn subagent_calls(fixture: &std::path::Path) -> Result<Vec<String>, String> {
    let io = fixture.with_file_name("io.jsonl");
    let text =
        std::fs::read_to_string(&io).map_err(|error| format!("{}: {error}", io.display()))?;
    let mut called = Vec::new();
    for record in text.lines() {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(record) else {
            continue;
        };
        let Some(Ok(frame)) = record["line"]
            .as_str()
            .map(serde_json::from_str::<serde_json::Value>)
        else {
            continue;
        };
        if frame["type"] != "assistant" || !frame["agent_id"].is_string() {
            continue;
        }
        for block in frame["message"]["content"].as_array().into_iter().flatten() {
            if block["type"] == "tool_use"
                && let Some(name) = block["name"].as_str()
            {
                called.push(name.to_owned());
            }
        }
    }
    Ok(called)
}

fn allow() -> Answer {
    Answer::Claude(claude_answer::Of::Permission(wire::PermissionAnswer {
        of: Some(wire::permission_answer::Of::Allow(wire::PermissionAllow {
            scope: None,
        })),
    }))
}

/// A summary of the session's subagent calls, for a failure.
fn calls_said(log: &Log) -> String {
    format!(
        "{:?}",
        agent_calls(log)
            .iter()
            .map(|call| call.state().as_str_name())
            .collect::<Vec<_>>()
    )
}

impl Install {
    /// A headless Claude agent with more of Claude's own arguments; the
    /// verdict when it cannot run turns.
    async fn subagent_host(
        &self,
        scenario: Scenario,
        extra: &[&str],
    ) -> Result<Result<(Vec<u8>, Follow), Verdict>, String> {
        self.claude_agent(scenario, super::CLAUDE_MODEL, &[], extra)
            .await
    }

    /// Answers each ask as `answer` says until `done` has held of the
    /// session for `QUIET`.
    async fn quiet(
        &self,
        agent: &[u8],
        follow: &Follow,
        what: &str,
        answer: impl FnMut(&OpenAsk) -> Result<Answer, String>,
        done: impl Fn(&Log) -> bool,
    ) -> Result<(), String> {
        let since = Mutex::new(None::<tokio::time::Instant>);
        self.drive(agent, follow, what, answer, |log| {
            let mut since = since.lock().unwrap();
            if !done(log) {
                *since = None;
                return false;
            }
            since
                .get_or_insert_with(tokio::time::Instant::now)
                .elapsed()
                >= QUIET
        })
        .await
    }

    /// Three subagents started at once, each running a command.
    pub(super) async fn subagents_parallel(&self) -> Result<Verdict, String> {
        let scenario = Scenario::SubagentsParallel;
        let (agent, follow) = match self
            .subagent_host(scenario, &["--allowedTools", "Bash"])
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(
            scenario,
            "In one message, call the Agent tool three times (subagent_type general-purpose), \
             so that three subagents run at the same time. The first subagent's task: use Bash \
             to run exactly `sleep 4; echo RED` and report what it printed. The second's: use \
             Bash to run exactly `sleep 4; echo GREEN` and report what it printed. The third's: \
             use Bash to run exactly `sleep 4; echo BLUE` and report what it printed. When all \
             three have reported back, reply with one line listing the three words.",
        )
        .await?;
        self.quiet(
            &agent,
            &follow,
            "the three subagents to report",
            |ask| self.by_the_way(ask),
            |log| finished(log, 3),
        )
        .await?;
        let calls = agent_calls(&follow.snapshot());
        if calls.len() < 3 {
            return Err(format!("{} subagents started, not 3", calls.len()));
        }
        self.judged(scenario, &follow)
    }

    /// A subagent that starts a subagent of its own.
    pub(super) async fn subagent_nested(&self) -> Result<Verdict, String> {
        let scenario = Scenario::SubagentNested;
        std::fs::write(self.project(scenario)?.join("b.txt"), "beta\n")
            .map_err(|error| error.to_string())?;
        let (agent, follow) = match self
            .subagent_host(scenario, &["--allowedTools", "Bash", "Read"])
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(
            scenario,
            "Use the Agent tool (subagent_type general-purpose) to start one subagent whose \
             task is: run `ls` with the Bash tool, then itself use the Agent tool to start one \
             nested subagent (subagent_type general-purpose) that reads b.txt with the Read \
             tool and reports its contents. When the first subagent has reported back, reply \
             with a line starting 'Done:' and its answer.",
        )
        .await?;
        self.quiet(
            &agent,
            &follow,
            "the subagents to report",
            |ask| self.by_the_way(ask),
            |log| finished(log, 1),
        )
        .await?;
        self.judged(scenario, &follow)
    }

    /// A subagent asks before a command and amux allows it.
    pub(super) async fn subagent_ask(&self) -> Result<Verdict, String> {
        let scenario = Scenario::SubagentAsk;
        let (agent, follow) = match self.subagent_host(scenario, &[]).await? {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(
            scenario,
            "Use the Agent tool (subagent_type general-purpose) to start one subagent whose \
             task is: run exactly this Bash command once, and nothing else: touch asked.txt. \
             Then report done. When it has reported back, reply with the single word DONE.",
        )
        .await?;
        let asked = Mutex::new(0usize);
        self.quiet(
            &agent,
            &follow,
            "the subagent's ask and its report",
            |ask| match ask {
                OpenAsk::Claude(claude)
                    if matches!(claude.body, Some(wire::claude_ask::Body::Permission(_))) =>
                {
                    *asked.lock().unwrap() += 1;
                    Ok(allow())
                }
                other => self.by_the_way(other),
            },
            |log| finished(log, 1),
        )
        .await?;
        if *asked.lock().unwrap() == 0 {
            return Err("the subagent ran its command without asking".into());
        }
        let effect = self.project(scenario)?.join("asked.txt");
        if !effect.exists() {
            return Err(format!("allowed, but {} does not exist", effect.display()));
        }
        self.judged(scenario, &follow)
    }

    /// A subagent in the background stopped from amux while its long
    /// command runs. A subagent its parent waits on is no job amux lists
    /// (Claude leaves it out of its background tasks), so it has no stop
    /// here.
    pub(super) async fn subagent_stopped(&self) -> Result<Verdict, String> {
        let scenario = Scenario::SubagentStopped;
        let (agent, follow) = match self
            .subagent_host(scenario, &["--allowedTools", "Bash"])
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(
            scenario,
            "Use the Agent tool once, with run_in_background set to true, to start one \
             general-purpose subagent whose task is exactly: run the Bash command `python3 -c \
             'import time; time.sleep(90)'; echo LATE` in the foreground and report what it \
             printed. When the subagent has reported or ended, reply with one line saying how \
             it ended.",
        )
        .await?;
        follow
            .until(UNDER_WAY, "the subagent's command to run", |log| {
                !subagent_jobs(log).is_empty()
                    && tools(log)
                        .iter()
                        .any(|call| call.name == "Bash" && running(call))
            })
            .await
            .map_err(|why| {
                let log = follow.snapshot();
                format!(
                    "{why}; jobs {:?}, calls {}",
                    log.state(Kind::ClaudeSdk).background.jobs,
                    calls_said(&log)
                )
            })?;
        // Let the command get going in its shell before it is stopped.
        tokio::time::sleep(Duration::from_secs(3)).await;
        let job = subagent_jobs(&follow.snapshot())
            .into_iter()
            .next()
            .ok_or("the subagent's job is gone")?;
        self.input(
            &agent,
            sdk(claude_sdk_input::Of::StopJob(wire::StopJob {
                key: job.key,
            })),
        )
        .await?;
        self.quiet(
            &agent,
            &follow,
            "the stopped subagent to end",
            |ask| self.by_the_way(ask),
            |log| finished(log, 1),
        )
        .await?;
        self.judged(scenario, &follow)
    }

    /// A subagent its parent sends a second task to once it reported.
    pub(super) async fn subagent_continued(&self) -> Result<Verdict, String> {
        let scenario = Scenario::SubagentContinued;
        let (agent, follow) = match self
            .subagent_host(scenario, &["--allowedTools", "Bash"])
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(
            scenario,
            "Use the Agent tool (subagent_type general-purpose) to start one subagent whose \
             task is: use Bash to run exactly `echo ALPHA` and report what it printed. When it \
             has reported back, use the SendMessage tool to send that same subagent, by its \
             agent id, this message: use Bash to run exactly `echo BETA` and report what it \
             printed. When it has reported again, reply with the single word DONE.",
        )
        .await?;
        self.quiet(
            &agent,
            &follow,
            "the subagent to report twice",
            |ask| self.by_the_way(ask),
            |log| {
                finished(log, 1)
                    && tools(log)
                        .iter()
                        .any(|call| call.name == "SendMessage" && !running(call))
            },
        )
        .await?;
        self.judged(scenario, &follow)
    }

    /// The host settled (its Claude exits) and resumed while its subagent's
    /// command runs, then asked to continue that subagent.
    pub(super) async fn subagent_resumed(&self) -> Result<Verdict, String> {
        let scenario = Scenario::SubagentResumed;
        let name = scenario.agent();
        let (agent, follow) = match self
            .subagent_host(scenario, &["--allowedTools", "Bash"])
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(
            scenario,
            "Use the Agent tool once, with run_in_background set to true, to start one \
             general-purpose subagent whose task is exactly: run the Bash command `python3 -c \
             'import time; time.sleep(90)'; echo LATE` in the foreground and report what it \
             printed. Then reply with the single word STARTED.",
        )
        .await?;
        // The subagent is an agent of its own: its command runs in its own
        // session, which the Agent call's row opens.
        follow
            .until(UNDER_WAY, "the subagent to start", |log| {
                !super::asks::opened_subagents(Kind::ClaudeSdk, log).is_empty()
            })
            .await?;
        let child = super::asks::opened_subagents(Kind::ClaudeSdk, &follow.snapshot()).remove(0);
        let subagent = self.follow(&child);
        subagent
            .until(UNDER_WAY, "the subagent's command to run", |log| {
                tools(log)
                    .iter()
                    .any(|call| call.name == "Bash" && running(call))
            })
            .await?;
        drop(subagent);
        tokio::time::sleep(Duration::from_secs(3)).await;
        self.amux(&["settle", &name]).await?;
        self.listed(&name, "settled").await?;
        self.amux(&["resume", &name]).await?;
        self.listed(&name, "idle").await?;
        let turns = follow.snapshot().turns(Kind::ClaudeSdk).len();
        self.send(
            scenario,
            "Is the subagent you started still running? Answer from what you know. Then use \
             the SendMessage tool to send it, by its agent id, this message: reply with the \
             single word BACK. When it has answered, or if it cannot be reached, reply with \
             the single word DONE.",
        )
        .await?;
        follow
            .until(READY, "the resumed turn to begin", |log| {
                log.turns(Kind::ClaudeSdk).len() > turns || log.phase() == Phase::Working
            })
            .await?;
        self.quiet(
            &agent,
            &follow,
            "the resumed host to settle",
            |ask| self.by_the_way(ask),
            |log| log.turns(Kind::ClaudeSdk).len() > turns && finished(log, 1),
        )
        .await?;
        // The exit and the resume are the agent process's, not Claude's:
        // the traffic has neither boundary, and the new process starts
        // again.
        self.note_model(&follow);
        let recorded = self.captured(scenario)?.shape(Kind::ClaudeSdk);
        let mut live = follow.snapshot().shape(Kind::ClaudeSdk);
        live.items
            .retain(|token| token != "boundary exited" && token != "boundary resumed");
        let mut phases = vec![Phase::Starting];
        for phase in live.phases.iter().skip(1) {
            if *phase != Phase::Starting && phases.last() != Some(phase) {
                phases.push(*phase);
            }
        }
        live.phases = phases;
        // The resumed Claude first reports the subagent its exit stopped and
        // answers that report with an empty turn, before the prompt arrives:
        // a Working spell of tens of milliseconds inside the process's start,
        // which a client reading the daemon's latest state can miss. Live
        // phases that only skip spells the recording holds are its phases.
        let mut recorded_phases = recorded.phases.iter();
        if live
            .phases
            .iter()
            .all(|phase| recorded_phases.any(|recorded| recorded == phase))
        {
            live.phases = recorded.phases.clone();
        }
        super::judge(&recorded, &live)?;
        Ok(Verdict::Pass)
    }

    /// A subagent calling amux's own tools.
    pub(super) async fn subagent_amux_tools(&self) -> Result<Verdict, String> {
        let scenario = Scenario::SubagentAmuxTools;
        let (agent, follow) = match self.subagent_host(scenario, &[]).await? {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(
            scenario,
            "Use the Agent tool (subagent_type general-purpose) to start one subagent whose \
             task is: first call the mcp__amux__set_working_on tool with working_on set to 'checking from a \
             subagent', then call the mcp__amux__list_agents tool, and report the agent names it \
             lists. When it has reported back, reply with the single word DONE.",
        )
        .await?;
        self.quiet(
            &agent,
            &follow,
            "the subagent to report",
            |ask| self.by_the_way(ask),
            |log| finished(log, 1),
        )
        .await?;
        // amux draws its own status call as no row, so the calls are read
        // from what Claude sent.
        let called = subagent_calls(&self.collect(scenario)?)?;
        for tool in ["mcp__amux__set_working_on", "mcp__amux__list_agents"] {
            if !called.iter().any(|name| name == tool) {
                return Err(format!("the subagent's calls were {called:?}"));
            }
        }
        self.judged(scenario, &follow)
    }

    /// A subagent's call to another tool server, which asks the person a
    /// question mid-call; the form is accepted from amux. The tool server
    /// keeps every request it was sent in the project's `tool-server.jsonl`.
    pub(super) async fn subagent_elicitation(&self) -> Result<Verdict, String> {
        let scenario = Scenario::SubagentElicitation;
        let probe = provider_fakes::binaries::path("claude-probe");
        let log_path = self.project(scenario)?.join("tool-server.jsonl");
        let servers = serde_json::json!({
            "mcpServers": {
                "external": {
                    "command": probe,
                    "args": ["__tool-server"],
                    "env": {"AMUX_LIVE_TOOL_LOG": log_path},
                },
            },
        })
        .to_string();
        let (agent, follow) = match self
            .subagent_host(
                scenario,
                &["--mcp-config", &servers, "--allowedTools", ASK_TOOL],
            )
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(
            scenario,
            &format!(
                "Use the Agent tool (subagent_type general-purpose) to start one subagent \
                 whose task is: call the {ASK_TOOL} tool with word set to ALBATROSS, then \
                 report exactly what it returned. When it has reported back, reply with \
                 exactly what it said."
            ),
        )
        .await?;
        let forms = Mutex::new(0usize);
        self.quiet(
            &agent,
            &follow,
            "the form to be answered and the subagent to report",
            |ask| match ask {
                OpenAsk::Claude(claude)
                    if matches!(claude.body, Some(wire::claude_ask::Body::Form(_))) =>
                {
                    *forms.lock().unwrap() += 1;
                    Ok(Answer::Claude(claude_answer::Of::Form(wire::FormAnswer {
                        action: wire::FormAction::Accept as i32,
                        content_json: br#"{"confirmed":"ALBATROSS"}"#.to_vec(),
                    })))
                }
                other => self.by_the_way(other),
            },
            |log| finished(log, 1),
        )
        .await?;
        if *forms.lock().unwrap() != 1 {
            return Err(format!("{} forms opened, not 1", forms.lock().unwrap()));
        }
        self.judged(scenario, &follow)
    }
}
