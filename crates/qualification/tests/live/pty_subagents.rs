//! Terminal Claude's own subagents (its Agent tool), recorded so what
//! Claude writes for each one, its `<session>/subagents/agent-<id>.jsonl`
//! and the `meta.json` beside it, and the hooks it raises for them, are in
//! the corpus. The capture proxy tails those files, registers the
//! `SubagentStart` hook amux does not, and records each tool server's
//! traffic, so a call's `_meta` is in the recording too.
//!
//! The scenarios have the names of headless Claude's (subagents.rs).
//! Claude starts subagents in the background, so the parent's first turn
//! ends while they run and each one's report starts a turn of its own:
//! each scenario waits until the parent has heard from every subagent.

use std::time::Duration;

use ui_state::OpenAsk;
use wire::{Kind, Phase, claude_answer};

use super::asks::Answer;
use super::{Follow, Install, Log, Scenario, TURN, Verdict};

/// What the subagents read: one word a file, so each answer is known.
const WORDS: [(&str, &str); 3] = [("a.txt", "ALPHA"), ("b.txt", "BRAVO"), ("c.txt", "CHARLIE")];

/// How long the files Claude writes are given to settle once the parent
/// has heard from every subagent.
const TRAILING: Duration = Duration::from_secs(3);

/// The tool the form-asking tool server offers (`claude-probe` run as
/// one, see its `__tool-server` mode).
const ASK_TOOL: &str = "mcp__external__ask_the_operator";

/// How long the session must hold still before the parent has heard from
/// every subagent: the parent's background list does not show a subagent
/// a subagent started, and a report to a subagent continues it after it
/// stopped.
const QUIET: Duration = Duration::from_secs(15);

/// The session is idle with nothing asked and nothing left in the
/// background, after at least `turns` turns, and has held still for
/// [`QUIET`].
fn heard_from_all(kind: Kind, turns: usize) -> impl Fn(&Log) -> bool {
    let last = std::sync::Mutex::new((0, 0, std::time::Instant::now()));
    move |log: &Log| {
        let state = log.state(kind);
        let settled = log.phase() == Phase::Idle
            && state.asks.is_empty()
            && state.background.jobs.is_empty()
            && !super::asks::subagent_call_running(kind, log)
            && log.turns(kind).len() >= turns;
        let mut last = last.lock().unwrap();
        let seen = (log.ordered().len(), log.snapshots.len());
        if !settled || (last.0, last.1) != seen {
            *last = (seen.0, seen.1, std::time::Instant::now());
            return false;
        }
        last.2.elapsed() >= QUIET
    }
}

/// A subagent that runs one long command.
fn sleeper(seconds: u32) -> String {
    serde_json::json!({
        "sleeper": {
            "description": "Waits a long time, then reports.",
            // Claude refuses a long `sleep` in the foreground.
            "prompt": format!(
                "Use Bash, in the foreground, to run exactly: python3 -c 'import select; \
                 select.select([], [], [], {seconds})'; echo WOKE. Then reply with exactly what \
                 it printed."
            ),
            "tools": ["Bash"],
            "model": "haiku",
        },
    })
    .to_string()
}

impl Install {
    /// A terminal Claude given `definitions` as its subagents, sent
    /// `prompt`, its asks answered by `answer` until it has heard from every
    /// subagent.
    async fn pty_delegated(
        &self,
        scenario: Scenario,
        definitions: serde_json::Value,
        more: &[&str],
        prompt: &str,
        answer: impl FnMut(&OpenAsk) -> Result<Answer, String>,
    ) -> Result<Result<(Vec<u8>, Follow), Verdict>, String> {
        let definitions = definitions.to_string();
        let mut extra = vec!["--agents", definitions.as_str()];
        extra.extend(more);
        let (agent, follow) = match self
            .claude_agent(scenario, super::CLAUDE_MODEL, &[], &extra)
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(Err(verdict)),
        };
        self.send(scenario, prompt).await?;
        let kind = self.kind;
        self.drive(
            &agent,
            &follow,
            "every subagent to report",
            answer,
            heard_from_all(kind, 1),
        )
        .await?;
        tokio::time::sleep(TRAILING).await;
        Ok(Ok((agent, follow)))
    }

    /// A terminal Claude given `definitions` as its subagents, with Bash
    /// allowed so a subagent's long command runs unasked.
    async fn pty_with_sleeper(
        &self,
        scenario: Scenario,
        definitions: &str,
        more: &[&str],
    ) -> Result<Result<(Vec<u8>, Follow), Verdict>, String> {
        let mut extra = vec!["--agents", definitions, "--allowedTools", "Bash"];
        extra.extend(more);
        self.claude_agent(scenario, super::CLAUDE_MODEL, &[], &extra)
            .await
    }

    /// The parent launches its sleeper in the background and goes idle
    /// with it running.
    async fn pty_launch_sleeper(&self, scenario: Scenario, follow: &Follow) -> Result<(), String> {
        let kind = self.kind;
        self.send(
            scenario,
            "Launch the `sleeper` subagent in the background, then reply with exactly \
             LAUNCHED and do nothing else.",
        )
        .await?;
        follow
            .until(TURN, "the subagent to run in the background", |log| {
                log.phase() == Phase::Idle && super::asks::subagent_call_running(kind, log)
            })
            .await
    }

    fn pty_words(&self, scenario: Scenario) -> Result<(), String> {
        let project = self.project(scenario)?;
        for (file, word) in WORDS {
            std::fs::write(project.join(file), format!("{word}\n"))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    /// Three subagents started in one message, each running tools, one of
    /// them asking before a command, which amux allows on its own chat.
    pub(super) async fn pty_subagents_parallel(&self) -> Result<Verdict, String> {
        let scenario = Scenario::PtySubagentsParallel;
        self.pty_words(scenario)?;
        let effect = self.project(scenario)?.join("c-read.txt");
        let mut asked = 0;
        let (_, follow) = match self
            .pty_delegated(
                scenario,
                serde_json::json!({
                    "reader": {
                        "description": "Reads one file and reports the word in it.",
                        "prompt": "Read the file you are given with the Read tool, then list \
                                   the .txt files in the working directory with Glob. If you \
                                   were given c.txt, also use Bash to run exactly: touch \
                                   c-read.txt. Then reply with exactly the word the file holds.",
                        "tools": ["Read", "Glob", "Bash"],
                        "model": "haiku",
                    },
                }),
                &[],
                "In a single message, launch three `reader` subagents in parallel: one for \
                 a.txt, one for b.txt and one for c.txt. When all three have reported, reply \
                 with their three words in file order, separated by spaces.",
                |ask| {
                    asked += 1;
                    self.by_the_way(ask)
                },
            )
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        if asked == 0 {
            return Err("the c.txt reader ran its command without asking".into());
        }
        if !effect.exists() {
            return Err(format!("allowed, but {} does not exist", effect.display()));
        }
        self.judged(scenario, &follow)
    }

    /// A subagent that starts a subagent of its own.
    pub(super) async fn pty_subagent_nested(&self) -> Result<Verdict, String> {
        let scenario = Scenario::PtySubagentNested;
        self.pty_words(scenario)?;
        let (_, follow) = match self
            .pty_delegated(
                scenario,
                serde_json::json!({
                    "outer": {
                        "description": "Delegates to the inner subagent and reports its answer.",
                        "prompt": "Use the Agent tool to launch the `inner` subagent, asking \
                                   it for the word in a.txt. Wait for its answer, then reply \
                                   with exactly OUTER followed by what it said.",
                        "tools": ["Agent"],
                        "model": "haiku",
                    },
                    "inner": {
                        "description": "Reads one file and reports the word in it.",
                        "prompt": "Read the file you are given with the Read tool and reply \
                                   with exactly the word it holds.",
                        "tools": ["Read"],
                        "model": "haiku",
                    },
                }),
                &[],
                "Launch the `outer` subagent and wait for it. When it has reported, reply with \
                 exactly what it said.",
                |ask| self.by_the_way(ask),
            )
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.judged(scenario, &follow)
    }

    /// A subagent asks before running a command, and amux allows it.
    pub(super) async fn pty_subagent_ask(&self) -> Result<Verdict, String> {
        let scenario = Scenario::PtySubagentAsk;
        let effect = self.project(scenario)?.join("sub-asked.txt");
        let mut asked = 0;
        let (_, follow) = match self
            .pty_delegated(
                scenario,
                serde_json::json!({
                    "toucher": {
                        "description": "Runs one command and reports.",
                        "prompt": "Use Bash to run exactly: touch sub-asked.txt. Then reply \
                                   with exactly DONE.",
                        "tools": ["Bash"],
                        "model": "haiku",
                    },
                }),
                &[],
                "Launch the `toucher` subagent and wait for it. When it has reported, reply \
                 with exactly what it said.",
                |ask| {
                    asked += 1;
                    self.by_the_way(ask)
                },
            )
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        if asked == 0 {
            return Err("the subagent ran its command without asking".into());
        }
        if !effect.exists() {
            return Err(format!("allowed, but {} does not exist", effect.display()));
        }
        self.judged(scenario, &follow)
    }

    /// A subagent in the background, stopped: the parent's next turn is
    /// interrupted from amux while it runs, then the parent is asked to
    /// stop it with its own tool. amux has no stop of its own for one.
    pub(super) async fn pty_subagent_stopped(&self) -> Result<Verdict, String> {
        let scenario = Scenario::PtySubagentStopped;
        let (agent, follow) = match self.pty_with_sleeper(scenario, &sleeper(120), &[]).await? {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        let kind = self.kind;
        self.pty_launch_sleeper(scenario, &follow).await?;
        self.send(
            scenario,
            "Use Bash to run exactly: python3 -c 'import select; select.select([], [], [], 30)'; \
             echo SECOND. Do nothing else.",
        )
        .await?;
        follow
            .until(TURN, "the second turn to get under way", |log| {
                log.phase() == Phase::Working && log.turns(kind).len() == 1
            })
            .await?;
        tokio::time::sleep(Duration::from_secs(3)).await;
        self.input(&agent, self.interrupt()).await?;
        follow.turn_end(2).await?;
        follow
            .until(TURN, "the agent to take input again", |log| {
                log.phase() == Phase::Idle
            })
            .await?;
        tokio::time::sleep(TRAILING).await;
        self.send(
            scenario,
            "Stop the `sleeper` subagent now, with your tool for stopping a background task. \
             Then reply with exactly STOPPED.",
        )
        .await?;
        self.drive(
            &agent,
            &follow,
            "the subagent to be stopped",
            |ask| self.by_the_way(ask),
            heard_from_all(kind, 3),
        )
        .await?;
        tokio::time::sleep(TRAILING).await;
        self.note_model(&follow);
        Ok(Verdict::Pass)
    }

    /// A subagent its parent waits on in the foreground, interrupted from
    /// amux while it runs. Claude 2.1.325 starts every subagent in the
    /// background, `run_in_background: false` or not, unless background
    /// tasks are turned off in its environment.
    pub(super) async fn pty_subagent_stopped_foreground(&self) -> Result<Verdict, String> {
        let scenario = Scenario::PtySubagentStoppedForeground;
        let settings = serde_json::json!({
            "env": {"CLAUDE_CODE_DISABLE_BACKGROUND_TASKS": "1"},
        })
        .to_string();
        let (agent, follow) = match self
            .pty_with_sleeper(scenario, &sleeper(120), &["--settings", &settings])
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        let kind = self.kind;
        self.send(
            scenario,
            "Launch the `sleeper` subagent in the foreground, with run_in_background set to \
             false, and wait for its answer. Then reply with exactly what it said.",
        )
        .await?;
        follow
            .until(TURN, "the subagent to get under way", |log| {
                log.phase() == Phase::Working && log.under_way(kind)
            })
            .await?;
        // Long enough for the subagent's own command to be running.
        tokio::time::sleep(Duration::from_secs(15)).await;
        self.input(&agent, self.interrupt()).await?;
        follow.turn_end(1).await?;
        follow
            .until(TURN, "the agent to take input again", |log| {
                log.phase() == Phase::Idle
            })
            .await?;
        // What Claude does with the subagent's files after the interrupt.
        tokio::time::sleep(Duration::from_secs(10)).await;
        self.note_model(&follow);
        Ok(Verdict::Pass)
    }

    /// A subagent its parent continues after it reported.
    pub(super) async fn pty_subagent_continued(&self) -> Result<Verdict, String> {
        let scenario = Scenario::PtySubagentContinued;
        let (agent, follow) = match self
            .pty_delegated(
                scenario,
                serde_json::json!({
                    "counter": {
                        "description": "Counts and reports.",
                        "prompt": "Reply with exactly ONE TWO THREE. If you are later asked \
                                   to go on, reply with exactly the next three numbers in \
                                   words.",
                        "tools": [],
                        "model": "haiku",
                    },
                }),
                &[],
                "Launch the `counter` subagent and wait for its answer. Then reply with exactly \
                 what it said.",
                |ask| self.by_the_way(ask),
            )
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        let kind = self.kind;
        let turns = follow.snapshot().turns(kind).len();
        self.send(
            scenario,
            "Continue that same `counter` subagent, the one that already answered (send it a \
             message by its agent id, do not launch a new one), asking it to go on. Wait for \
             its answer, then reply with exactly what it said.",
        )
        .await?;
        self.drive(
            &agent,
            &follow,
            "the continued subagent to report",
            |ask| self.by_the_way(ask),
            heard_from_all(kind, turns + 1),
        )
        .await?;
        tokio::time::sleep(TRAILING).await;
        self.judged(scenario, &follow)
    }

    /// The host stopped and resumed while its subagent runs in the
    /// background.
    pub(super) async fn pty_subagent_resumed(&self) -> Result<Verdict, String> {
        let scenario = Scenario::PtySubagentResumed;
        let name = scenario.agent();
        let (agent, follow) = match self.pty_with_sleeper(scenario, &sleeper(45), &[]).await? {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        let kind = self.kind;
        self.pty_launch_sleeper(scenario, &follow).await?;
        tokio::time::sleep(Duration::from_secs(5)).await;
        self.amux(&["settle", &name]).await?;
        tokio::time::sleep(Duration::from_secs(5)).await;
        self.amux(&["resume", &name]).await?;
        self.listed(&name, "idle").await?;
        let turns = follow.snapshot().turns(kind).len();
        self.send(
            scenario,
            "Is the `sleeper` subagent you launched still running? Check, and reply in one \
             short sentence.",
        )
        .await?;
        self.drive(
            &agent,
            &follow,
            "the resumed host to answer",
            |ask| self.by_the_way(ask),
            heard_from_all(kind, turns + 1),
        )
        .await?;
        // Past the time the subagent would have reported.
        tokio::time::sleep(Duration::from_secs(45)).await;
        self.note_model(&follow);
        Ok(Verdict::Pass)
    }

    /// A subagent calls amux's own tools; the tool server's traffic is
    /// recorded, so what it is told about its caller is in the corpus.
    pub(super) async fn pty_subagent_amux_tools(&self) -> Result<Verdict, String> {
        let scenario = Scenario::PtySubagentAmuxTools;
        let (_, follow) = match self
            .pty_delegated(
                scenario,
                serde_json::json!({
                    "reporter": {
                        "description": "Reports its status through amux and lists the fleet.",
                        "prompt": "Call the mcp__amux__set_working_on tool with the text 'subagent \
                                   checking in'. Then call the mcp__amux__list_agents tool. Then \
                                   reply with exactly DONE.",
                        "tools": ["mcp__amux__set_working_on", "mcp__amux__list_agents"],
                        "model": "haiku",
                    },
                }),
                &[],
                "Launch the `reporter` subagent and wait for it. When it has reported, reply \
                 with exactly what it said.",
                |ask| self.by_the_way(ask),
            )
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.judged(scenario, &follow)
    }

    /// Another tool server asks a question mid-call, from inside a
    /// subagent, and amux answers it. A terminal capture does not hold what
    /// amux answered on the hook, so its replay reads the form as
    /// dismissed: the live run is not judged against it.
    pub(super) async fn pty_subagent_elicitation(&self) -> Result<Verdict, String> {
        let scenario = Scenario::PtySubagentElicitation;
        let probe = provider_fakes::binaries::path("claude-probe");
        let servers = serde_json::json!({
            "mcpServers": {
                "external": {"command": probe, "args": ["__tool-server"]},
            },
        })
        .to_string();
        let mut answer = Some(claude_answer::Of::Form(wire::FormAnswer {
            action: wire::FormAction::Accept as i32,
            content_json: br#"{"confirmed":"ALBATROSS"}"#.to_vec(),
        }));
        let mut forms = 0;
        let (_, follow) = match self
            .pty_delegated(
                scenario,
                serde_json::json!({
                    "confirmer": {
                        "description": "Confirms a word with the operator.",
                        "prompt": format!(
                            "Call the {ASK_TOOL} tool with word set to ALBATROSS, then reply \
                             with exactly what it returned."
                        ),
                        "tools": [ASK_TOOL],
                        "model": "haiku",
                    },
                }),
                &["--mcp-config", &servers, "--allowedTools", ASK_TOOL],
                "Launch the `confirmer` subagent and wait for it. When it has reported, reply \
                 with exactly what it said.",
                |ask| match ask {
                    OpenAsk::Claude(claude)
                        if matches!(
                            claude.body,
                            Some(wire::claude_ask::Body::Form(_) | wire::claude_ask::Body::Link(_))
                        ) =>
                    {
                        forms += 1;
                        answer
                            .take()
                            .map(Answer::Claude)
                            .ok_or_else(|| "a second form opened".to_owned())
                    }
                    _ => self.by_the_way(ask),
                },
            )
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        if forms == 0 {
            return Err("no form opened".into());
        }
        self.note_model(&follow);
        Ok(Verdict::Pass)
    }
}
