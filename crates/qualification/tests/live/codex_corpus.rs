//! The Codex corpus: sessions amux drives so that what Codex says in them
//! is recorded, joined to codex-specs's live captures (`codex-probe join`),
//! and replayed by the interpreter's goldens, the views' goldens, the
//! fakes' conformance run and the strict decode tests.
//!
//! Each is a thing a person does through amux (a prompt whose work makes a
//! row of its own, an approval decided, a message sent to the agent idle or
//! busy, a compaction), or the same under a setting of the person's own
//! Codex config that amux runs beneath (a tool server, a provider, web
//! search, an approval policy amux names no permission for, no login). Each
//! is judged as the asks are: by the real effect where there is one, and by
//! the shape of its own traffic replayed through the interpreter against
//! what the daemon committed.

use std::path::PathBuf;

use ui_state::{ItemBody, OpenAsk};
use wire::codex_item::Kind as Codex;
use wire::{
    AgentRef, CodexCreateConfig, Kind, MessageBody, PromptInput, PutBlobRequest,
    ResolveAgentRequest, SendMessageRequest, codex_ask, work,
};

use super::asks::{Answer, settled};
use super::corpus::SQUARE_PNG;
use super::{Install, Log, Scenario, TURN, Verdict, expect_outcome};

/// The tool the form-asking tool server offers (`codex-probe` run as one,
/// see its `__tool-server` mode), and the server's name in Codex's config.
const TOOL: &str = "ask_the_operator";
const TOOL_SERVER: &str = "spec";

fn codex_items(log: &Log) -> Vec<Codex> {
    log.ordered()
        .into_iter()
        .filter_map(|item| match ItemBody::decode(Kind::Codex, &item.body) {
            ItemBody::Codex(body) => Some(body),
            _ => None,
        })
        .collect()
}

fn works(log: &Log) -> Vec<wire::Work> {
    codex_items(log)
        .into_iter()
        .filter_map(|item| match item {
            Codex::Work(work) => Some(work),
            _ => None,
        })
        .collect()
}

fn commands(log: &Log) -> Vec<(wire::CommandWork, wire::ToolState)> {
    works(log)
        .into_iter()
        .filter_map(|work| {
            let state = work.state();
            match work.of {
                Some(work::Of::Command(command)) => Some((command, state)),
                _ => None,
            }
        })
        .collect()
}

fn file_changes(log: &Log) -> Vec<wire::FileChange> {
    works(log)
        .into_iter()
        .filter_map(|work| match work.of {
            Some(work::Of::FileChange(change)) => Some(change.changes),
            _ => None,
        })
        .flatten()
        .collect()
}

/// The decisions the session's work was decided with, in order.
fn decisions(log: &Log) -> Vec<wire::DecisionOutcome> {
    works(log)
        .iter()
        .filter_map(|work| work.decision.as_ref().map(|decision| decision.outcome()))
        .collect()
}

fn decide(decision: wire::Decision) -> Answer {
    Answer::Given(wire::answer::Of::Approve(decision as i32))
}

/// What a scenario changed in Codex's home while it runs: settings in its
/// config file, or the login moved away. Undone when dropped, before the
/// next scenario starts its agent.
struct HomeChange {
    home: PathBuf,
    signed_out: bool,
}

impl Drop for HomeChange {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.home.join("config.toml"));
        if self.signed_out {
            let _ = std::fs::rename(
                self.home.join("auth.json.away"),
                self.home.join("auth.json"),
            );
        }
    }
}

impl Install {
    fn home(&self) -> Result<&PathBuf, String> {
        self.codex_home
            .as_ref()
            .ok_or_else(|| "this run has no Codex home".to_owned())
    }

    /// Codex's config file holds `toml` until the returned change drops.
    fn configured(&self, toml: &str) -> Result<HomeChange, String> {
        let home = self.home()?.clone();
        std::fs::write(home.join("config.toml"), toml).map_err(|error| error.to_string())?;
        Ok(HomeChange {
            home,
            signed_out: false,
        })
    }

    /// Codex's home holds no login until the returned change drops.
    fn signed_out_home(&self) -> Result<HomeChange, String> {
        let home = self.home()?.clone();
        std::fs::rename(home.join("auth.json"), home.join("auth.json.away"))
            .map_err(|error| error.to_string())?;
        Ok(HomeChange {
            home,
            signed_out: true,
        })
    }

    /// A prompt to a new agent made with `config`, each ask it raises
    /// answered by `answer`, until the session settles; then `check` holds
    /// of it and its own traffic replays to the committed shape.
    async fn codex_work(
        &self,
        scenario: Scenario,
        config: CodexCreateConfig,
        prompt: &str,
        answer: impl FnMut(&OpenAsk) -> Result<Answer, String>,
        check: impl Fn(&Log) -> Result<(), String>,
    ) -> Result<Verdict, String> {
        let (agent, follow) = match self.ready_agent(scenario, Some(config)).await? {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(scenario, prompt).await?;
        self.settle(&agent, &follow, "the work to finish", answer)
            .await?;
        check(&follow.snapshot())?;
        self.judged(scenario, &follow)
    }

    /// Sends a message to the agent as a person on a client does.
    async fn message(&self, scenario: Scenario, agent: &[u8], text: &str) -> Result<(), String> {
        let host_id = self
            .client
            .agents
            .clone()
            .resolve_agent(ResolveAgentRequest {
                name: scenario.agent(),
            })
            .await
            .map_err(|status| status.to_string())?
            .into_inner()
            .host_id;
        self.client
            .agents
            .clone()
            .send_message(SendMessageRequest {
                id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                to: Some(AgentRef {
                    host_id,
                    agent_id: agent.to_vec(),
                }),
                context: None,
                body: Some(MessageBody {
                    text: text.into(),
                    attachments: Vec::new(),
                }),
            })
            .await
            .map_err(|status| status.to_string())?;
        Ok(())
    }

    /// The scenario's recorded traffic, as the probe wrote it.
    fn traffic(&self, scenario: Scenario) -> Result<String, String> {
        let fixture = self.collect(scenario)?;
        std::fs::read_to_string(fixture.with_file_name("io.jsonl")).map_err(|e| e.to_string())
    }

    fn agent_messages(log: &Log) -> usize {
        codex_items(log)
            .iter()
            .filter(|item| matches!(item, Codex::AgentMessage(_)))
            .count()
    }

    /// A command in a read-only sandbox, denied: it does not run.
    pub(super) async fn approval_deny(&self) -> Result<Verdict, String> {
        let scenario = Scenario::ApprovalDeny;
        let effect = self.project(scenario)?.join("denied.txt");
        self.codex_work(
            scenario,
            CodexCreateConfig {
                permission: Some("read-only".into()),
                ..CodexCreateConfig::default()
            },
            "Run exactly this shell command once, with your shell tool, and nothing else: touch \
             denied.txt. The sandbox is read-only, so request escalated permissions for it. If \
             it is refused, do not try anything else; reply with the single word refused.",
            |ask| match ask {
                OpenAsk::Codex(_) => Ok(decide(wire::Decision::Deny)),
                _ => self.by_the_way(ask),
            },
            |log| {
                if effect.exists() {
                    return Err(format!("denied, but {} exists", effect.display()));
                }
                if !decisions(log).contains(&wire::DecisionOutcome::Denied) {
                    return Err(format!("no work reads denied: {:?}", decisions(log)));
                }
                Ok(())
            },
        )
        .await
    }

    /// A message to an idle agent: Codex answers it in a turn of its own.
    pub(super) async fn inject_idle(&self) -> Result<Verdict, String> {
        let scenario = Scenario::InjectIdle;
        let (agent, follow) = match self
            .ready_agent(scenario, Some(CodexCreateConfig::default()))
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.message(
            scenario,
            &agent,
            "Reply with exactly INJECTED_IDLE and nothing else.",
        )
        .await?;
        expect_outcome(follow.turn_end(1).await?, wire::TurnOutcome::Completed)?;
        let kind = self.kind;
        follow
            .until(TURN, "the session to settle", |log| settled(kind, log))
            .await?;
        if Self::agent_messages(&follow.snapshot()) != 1 {
            return Err("the message reads no agent message item".into());
        }
        self.judged(scenario, &follow)
    }

    /// A message sent while a command runs: the running turn takes it in
    /// and answers it before it ends, so no turn of its own follows.
    pub(super) async fn inject_drain(&self) -> Result<Verdict, String> {
        let scenario = Scenario::InjectDrain;
        let (agent, follow) = match self
            .ready_agent(scenario, Some(CodexCreateConfig::default()))
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(
            scenario,
            "Run the shell command `sleep 8`, then reply with exactly DONE_A and nothing else.",
        )
        .await?;
        follow
            .until(TURN, "the command to run", |log| {
                commands(log)
                    .iter()
                    .any(|(_, state)| *state == wire::ToolState::Running)
            })
            .await?;
        self.message(
            scenario,
            &agent,
            "Ignore the earlier instruction about the final reply. Reply with exactly \
             INJECTED_BUSY and nothing else.",
        )
        .await?;
        let kind = self.kind;
        self.settle(&agent, &follow, "the turn to end", |ask| {
            self.by_the_way(ask)
        })
        .await?;
        let live = follow.snapshot();
        if Self::agent_messages(&live) != 1 {
            return Err("the message reads no agent message item".into());
        }
        if live.turns(kind).len() != 1 {
            return Err(format!(
                "the message ran {} turns, not the one it was sent into",
                live.turns(kind).len()
            ));
        }
        self.judged(scenario, &follow)
    }

    /// A message queued while a command runs and sent into the running
    /// turn, and another left queued: the first reads steered and joins
    /// that turn, the second runs as a turn of its own.
    pub(super) async fn codex_steer_folded(&self) -> Result<Verdict, String> {
        let scenario = Scenario::SteerFolded;
        let (agent, follow) = match self
            .ready_agent(scenario, Some(CodexCreateConfig::default()))
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(
            scenario,
            "Run the shell command `sleep 15; echo S1`, then the shell command `sleep 5; echo \
             S2`, then reply exactly DONE plus any extra words the user asked for.",
        )
        .await?;
        follow
            .until(TURN, "the first command to run", |log| {
                commands(log)
                    .iter()
                    .any(|(_, state)| *state == wire::ToolState::Running)
            })
            .await?;
        let prompt = |text: &str| wire::AgentInput {
            of: Some(wire::agent_input::Of::Prompt(PromptInput {
                text: text.into(),
                attachments: Vec::new(),
            })),
        };
        let steered = self
            .send_input(&agent, prompt("Also append the word BANANA."))
            .await?;
        self.send_input(&agent, prompt("Then reply with exactly LATER."))
            .await?;
        self.input(
            &agent,
            wire::AgentInput {
                of: Some(wire::agent_input::Of::SendNow(wire::SendQueuedNow {
                    queued_input_id: steered,
                })),
            },
        )
        .await?;
        expect_outcome(follow.turn_end(2).await?, wire::TurnOutcome::Completed)?;
        let kind = self.kind;
        follow
            .until(TURN, "the session to settle", |log| settled(kind, log))
            .await?;
        if !codex_items(&follow.snapshot())
            .iter()
            .any(|item| matches!(item, Codex::Steer(_)))
        {
            return Err("the message sent now reads no steer".into());
        }
        self.note_model(&follow);
        let mut recorded = self.captured(scenario)?.shape(kind);
        recorded.queued_ran_at_once();
        let mut live = follow.snapshot().shape(kind);
        live.queued_ran_at_once();
        super::judge(&recorded, &live)?;
        Ok(Verdict::Pass)
    }

    /// Codex's working notes and its final answer, both in one turn.
    pub(super) async fn two_assistant_messages(&self) -> Result<Verdict, String> {
        self.codex_work(
            Scenario::TwoAssistantMessages,
            CodexCreateConfig::default(),
            "Send two separate assistant messages in this turn: first exactly FIRST_NOTE in \
             commentary, then exactly SECOND_ANSWER as the final answer.",
            |ask| self.by_the_way(ask),
            |log| {
                let items = codex_items(log);
                let notes = items
                    .iter()
                    .any(|item| matches!(item, Codex::WorkingNote(_)));
                let answers = items.iter().any(|item| matches!(item, Codex::Message(_)));
                if !(notes && answers) {
                    return Err(format!(
                        "no working note beside the answer: {:?}",
                        log.shape(Kind::Codex).items
                    ));
                }
                Ok(())
            },
        )
        .await
    }

    /// At a high effort, with the person's config asking for detailed
    /// summaries, the reasoning item carries Codex's summary of it.
    pub(super) async fn reasoning_summary(&self) -> Result<Verdict, String> {
        let _home = self.configured("model_reasoning_summary = \"detailed\"\n")?;
        self.codex_work(
            Scenario::ReasoningSummary,
            CodexCreateConfig {
                effort: Some("high".into()),
                ..CodexCreateConfig::default()
            },
            "Think carefully about which is larger, 17 * 23 or 19 * 21, then reply with only the \
             larger product.",
            |ask| self.by_the_way(ask),
            |log| {
                let summarized = codex_items(log).iter().any(|item| {
                    matches!(item, Codex::Reasoning(reasoning)
                        if reasoning.summary.iter().any(|part| !part.is_empty()))
                });
                if !summarized {
                    return Err("no reasoning item carries a summary".into());
                }
                Ok(())
            },
        )
        .await
    }

    /// Reading a file, listing files and searching: Codex classes each.
    pub(super) async fn exploring(&self) -> Result<Verdict, String> {
        let scenario = Scenario::Exploring;
        let project = self.project(scenario)?;
        std::fs::write(project.join("config.txt"), "VALUE=1\n").map_err(|e| e.to_string())?;
        self.codex_work(
            scenario,
            CodexCreateConfig::default(),
            "Run these three shell commands separately, one tool call each: `cat config.txt`, \
             `ls`, and `grep -n VALUE config.txt`. Then reply DONE.",
            |ask| self.by_the_way(ask),
            |log| {
                let classes: Vec<wire::ToolClass> =
                    works(log).iter().map(wire::Work::class).collect();
                for class in [
                    wire::ToolClass::Read,
                    wire::ToolClass::List,
                    wire::ToolClass::Search,
                ] {
                    if !classes.contains(&class) {
                        return Err(format!("no {class:?} command among {classes:?}"));
                    }
                }
                Ok(())
            },
        )
        .await
    }

    /// A command that exits non-zero reads failed, with its exit code.
    pub(super) async fn codex_failing_command(&self) -> Result<Verdict, String> {
        self.codex_work(
            Scenario::FailingCommand,
            CodexCreateConfig::default(),
            "Run exactly this shell command once: ls does-not-exist. Then tell me whether it \
             failed.",
            |ask| self.by_the_way(ask),
            |log| {
                let failed = commands(log).iter().any(|(command, state)| {
                    *state == wire::ToolState::Failed
                        && command.exit_code.is_some_and(|code| code != 0)
                });
                if !failed {
                    return Err(format!("no command reads failed: {:?}", commands(log)));
                }
                Ok(())
            },
        )
        .await
    }

    /// Patches that add a file, change it and delete another: each change
    /// reads its kind.
    pub(super) async fn file_changes(&self) -> Result<Verdict, String> {
        let scenario = Scenario::FileChanges;
        std::fs::write(self.project(scenario)?.join("old.txt"), "old\n")
            .map_err(|error| error.to_string())?;
        self.codex_work(
            scenario,
            CodexCreateConfig::default(),
            "Use apply_patch for every change, one patch per step: first create notes.txt \
             containing the line one; then change that line to two; then delete old.txt. Then \
             reply DONE.",
            |ask| self.by_the_way(ask),
            |log| {
                let kinds: Vec<wire::FileChangeKind> = file_changes(log)
                    .iter()
                    .map(wire::FileChange::kind)
                    .collect();
                for kind in [
                    wire::FileChangeKind::Add,
                    wire::FileChangeKind::Update,
                    wire::FileChangeKind::Delete,
                ] {
                    if !kinds.contains(&kind) {
                        return Err(format!("no {kind:?} change among {kinds:?}"));
                    }
                }
                Ok(())
            },
        )
        .await
    }

    /// A patch that renames a file: the change names where it moved.
    pub(super) async fn file_moved(&self) -> Result<Verdict, String> {
        let scenario = Scenario::FileMoved;
        let project = self.project(scenario)?;
        std::fs::write(project.join("old.txt"), "old\n").map_err(|error| error.to_string())?;
        self.codex_work(
            scenario,
            CodexCreateConfig::default(),
            // A move with no content hunk is refused by some apply_patch
            // builds, so the move changes the line too.
            "Use apply_patch to rename old.txt to moved.txt and change its one line from old \
             to moved, in one Update File hunk with a Move to line. Do not use shell commands. \
             Then reply DONE.",
            |ask| self.by_the_way(ask),
            |log| {
                if !file_changes(log)
                    .iter()
                    .any(|change| !change.move_to.is_empty())
                {
                    return Err(format!("no change reads moved: {:?}", file_changes(log)));
                }
                if !project.join("moved.txt").exists() {
                    return Err("moved.txt does not exist".into());
                }
                Ok(())
            },
        )
        .await
    }

    /// An image attached to a prompt, and the same image looked at on
    /// disk with Codex's image viewer.
    pub(super) async fn codex_image(&self) -> Result<Verdict, String> {
        let scenario = Scenario::Image;
        std::fs::write(self.project(scenario)?.join("square.png"), SQUARE_PNG)
            .map_err(|error| error.to_string())?;
        let (agent, follow) = match self
            .ready_agent(scenario, Some(CodexCreateConfig::default()))
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        let blob = self
            .client
            .agents
            .clone()
            .put_blob(PutBlobRequest {
                agent_id: agent.clone(),
                name: "square.png".into(),
                mime: "image/png".into(),
                bytes: SQUARE_PNG.to_vec(),
            })
            .await
            .map_err(|status| format!("the image upload: {status}"))?
            .into_inner();
        self.input(
            &agent,
            wire::AgentInput {
                of: Some(wire::agent_input::Of::Prompt(PromptInput {
                    text: "Name the color of the attached image in one word. Then look at \
                           square.png in the working directory with your image viewing tool \
                           and say whether it is the same image."
                        .into(),
                    attachments: vec![wire::Attachment {
                        of: Some(wire::attachment::Of::Image(blob)),
                    }],
                })),
            },
        )
        .await?;
        self.settle(&agent, &follow, "the image to be looked at", |ask| {
            self.by_the_way(ask)
        })
        .await?;
        if !works(&follow.snapshot())
            .iter()
            .any(|work| matches!(&work.of, Some(work::Of::Image(image)) if !image.generated))
        {
            return Err("Codex did not look at the image on disk".into());
        }
        self.judged(scenario, &follow)
    }

    /// A turn, then `/compact`: the conversation reads compacted.
    pub(super) async fn compaction(&self) -> Result<Verdict, String> {
        let scenario = Scenario::Compaction;
        let (_, follow) = match self
            .ready_agent(scenario, Some(CodexCreateConfig::default()))
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.turn(
            scenario,
            &follow,
            "Reply with exactly BEFORE_COMPACT and nothing else.",
            1,
        )
        .await?;
        self.turn(scenario, &follow, "/compact", 2).await?;
        let compacted = codex_items(&follow.snapshot()).iter().any(|item| {
            matches!(item, Codex::Boundary(boundary)
                if boundary.kind() == wire::BoundaryKind::Compacted)
        });
        if !compacted {
            return Err("compacting left no compacted boundary".into());
        }
        self.judged(scenario, &follow)
    }

    /// A turn on a model the account cannot use fails with an error.
    pub(super) async fn turn_error(&self) -> Result<Verdict, String> {
        let scenario = Scenario::TurnError;
        let (agent, follow) = match self
            .ready_agent(scenario, Some(CodexCreateConfig::default()))
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.input(
            &agent,
            wire::AgentInput {
                of: Some(wire::agent_input::Of::Model(wire::SetModel {
                    model: Some("amux-live-no-such-model".into()),
                })),
            },
        )
        .await?;
        self.send(scenario, "Reply with exactly UNREACHABLE.")
            .await?;
        expect_outcome(follow.turn_end(1).await?, wire::TurnOutcome::Failed)?;
        let kind = self.kind;
        follow
            .until(TURN, "the session to settle", |log| settled(kind, log))
            .await?;
        if !codex_items(&follow.snapshot())
            .iter()
            .any(|item| matches!(item, Codex::Error(_)))
        {
            return Err("the failed turn reads no error".into());
        }
        self.judged(scenario, &follow)
    }

    /// A provider that cannot be reached, as the person's config names
    /// one: Codex retries, saying so each time, then the turn fails.
    pub(super) async fn turn_retries(&self) -> Result<Verdict, String> {
        let scenario = Scenario::TurnRetries;
        let _home = self.configured(
            "model_provider = \"unreachable\"\n\
             [features]\nunbounded_connection_retries = false\n\
             [model_providers.unreachable]\nname = \"unreachable\"\n\
             base_url = \"http://127.0.0.1:9/v1\"\nwire_api = \"responses\"\n\
             request_max_retries = 2\nstream_max_retries = 2\n",
        )?;
        let agent = self
            .create_codex(scenario, CodexCreateConfig::default())
            .await?;
        let follow = self.follow(&agent);
        follow.ready().await?;
        self.send(scenario, "Reply with exactly UNREACHABLE.")
            .await?;
        expect_outcome(follow.turn_end(1).await?, wire::TurnOutcome::Failed)?;
        let kind = self.kind;
        follow
            .until(TURN, "the session to settle", |log| settled(kind, log))
            .await?;
        // The retries fold into the one error row the turn ends with; the
        // traffic shows each.
        if !self.traffic(scenario)?.contains(r#"\"willRetry\":true"#) {
            return Err("no error said it would retry".into());
        }
        if !codex_items(&follow.snapshot())
            .iter()
            .any(|item| matches!(item, Codex::Error(_)))
        {
            return Err("the failed turn reads no error".into());
        }
        self.judged(scenario, &follow)
    }

    /// Without a login a turn fails, and the sign-in reads the failure.
    pub(super) async fn signed_out(&self) -> Result<Verdict, String> {
        let scenario = Scenario::SignedOut;
        let _home = self.signed_out_home()?;
        let agent = self
            .create_codex(scenario, CodexCreateConfig::default())
            .await?;
        let follow = self.follow(&agent);
        follow.ready().await?;
        self.send(scenario, "Reply with exactly SIGNED_OUT.")
            .await?;
        expect_outcome(follow.turn_end(1).await?, wire::TurnOutcome::Failed)?;
        let kind = self.kind;
        follow
            .until(TURN, "the sign-in to read the failure", |log| {
                settled(kind, log)
                    && !matches!(
                        log.state(kind).sign_in.state(),
                        wire::SignInState::SignedIn | wire::SignInState::Unknown
                    )
            })
            .await?;
        self.judged(scenario, &follow)
    }

    /// With live web search on in the person's config, a search runs as
    /// work of its own.
    pub(super) async fn web_search(&self) -> Result<Verdict, String> {
        let _home = self.configured("web_search = \"live\"\n")?;
        self.codex_work(
            Scenario::WebSearch,
            CodexCreateConfig::default(),
            "Use your web search tool to look up the capital city of Australia, then reply with \
             the city name only.",
            |ask| self.by_the_way(ask),
            |log| {
                if !works(log)
                    .iter()
                    .any(|work| matches!(work.of, Some(work::Of::WebSearch(_))))
                {
                    return Err("no web search ran".into());
                }
                Ok(())
            },
        )
        .await
    }

    /// Codex's own subagent: a child thread spawned and waited for.
    pub(super) async fn subagent(&self) -> Result<Verdict, String> {
        self.codex_work(
            Scenario::Subagent,
            CodexCreateConfig::default(),
            "Call your spawn_agent tool directly, without reading any file or using any skill, \
             to start exactly one agent whose task is: reply with exactly CHILD_DONE. Then wait \
             for it to finish and reply with what it answered.",
            |ask| self.by_the_way(ask),
            |log| {
                if !works(log)
                    .iter()
                    .any(|work| matches!(work.of, Some(work::Of::Collab(_))))
                {
                    return Err("no subagent work".into());
                }
                Ok(())
            },
        )
        .await
    }

    /// A command left running as a terminal session, written to and read
    /// before it ends.
    pub(super) async fn background_terminal(&self) -> Result<Verdict, String> {
        let scenario = Scenario::BackgroundTerminal;
        let verdict = self
            .codex_work(
                scenario,
                CodexCreateConfig::default(),
                "Start the shell command `read LINE; echo GOT $LINE` with a yield time of 1000 \
                 ms, so the call returns while it waits for input. Then write the text hello \
                 followed by a newline to that same running session, read its output, and reply \
                 DONE.",
                |ask| self.by_the_way(ask),
                |log| {
                    if !commands(log)
                        .iter()
                        .any(|(_, state)| *state == wire::ToolState::Succeeded)
                    {
                        return Err(format!("no command succeeded: {:?}", commands(log)));
                    }
                    Ok(())
                },
            )
            .await?;
        // Writing to the session is no row of its own: the traffic shows it.
        if matches!(verdict, Verdict::Pass)
            && !self
                .traffic(scenario)?
                .contains("item/commandExecution/terminalInteraction")
        {
            return Err("nothing was written to the running command".into());
        }
        Ok(verdict)
    }

    /// A tool server of the person's own whose tool asks through forms:
    /// Codex asks whether the tool may run, then the server asks for a
    /// word with a form, accepted, and with a link, cancelled.
    pub(super) async fn tool_server_form(&self) -> Result<Verdict, String> {
        let probe = provider_fakes::binaries::path("codex-probe");
        let _home = self.configured(&format!(
            "[mcp_servers.{TOOL_SERVER}]\ncommand = {}\nargs = [\"__tool-server\"]\n",
            serde_json::json!(probe)
        ))?;
        let mut forms = 0;
        let mut links = 0;
        let verdict = self
            .codex_work(
                Scenario::ToolServerForm,
                CodexCreateConfig::default(),
                &format!(
                    "Call the {TOOL} tool from the {TOOL_SERVER} tool server twice, one call at \
                     a time: first with word BLUE, then with word LINK. Then reply with what \
                     each call returned."
                ),
                |ask| match ask {
                    OpenAsk::Codex(codex) => match &codex.body {
                        Some(codex_ask::Body::McpForm(_)) => {
                            forms += 1;
                            Ok(Answer::Given(wire::answer::Of::Form(wire::FormAnswer {
                                action: wire::FormAction::Accept as i32,
                                content_json: br#"{"confirmed":"BLUE"}"#.to_vec(),
                            })))
                        }
                        Some(codex_ask::Body::McpLink(_)) => {
                            links += 1;
                            Ok(Answer::Given(wire::answer::Of::Link(wire::LinkAnswer {
                                action: wire::FormAction::Cancel as i32,
                            })))
                        }
                        Some(codex_ask::Body::McpTool(_)) => Ok(decide(wire::Decision::Approve)),
                        _ => self.by_the_way(ask),
                    },
                    _ => self.by_the_way(ask),
                },
                |log| {
                    if !works(log)
                        .iter()
                        .any(|work| matches!(work.of, Some(work::Of::Mcp(_))))
                    {
                        return Err("no tool-server call ran".into());
                    }
                    Ok(())
                },
            )
            .await?;
        if matches!(verdict, Verdict::Pass) && (forms == 0 || links == 0) {
            return Err(format!("{forms} forms and {links} links were asked"));
        }
        Ok(verdict)
    }

    /// Under the person's granular approval policy with Codex's
    /// permissions tool on, Codex asks for network access, granted for
    /// the turn.
    pub(super) async fn access_grant(&self) -> Result<Verdict, String> {
        let _home = self.configured(
            "[approval_policy.granular]\nsandbox_approval = true\nrules = true\n\
             skill_approval = false\nrequest_permissions = true\nmcp_elicitations = true\n\
             [features]\nrequest_permissions_tool = true\n",
        )?;
        let mut grants = 0;
        let verdict = self
            .codex_work(
                Scenario::AccessGrant,
                CodexCreateConfig::default(),
                "Use your request_permissions tool to ask me for network access. Once granted, \
                 run `curl -sI https://example.com` and reply with its status line.",
                |ask| match ask {
                    OpenAsk::Codex(codex) => match &codex.body {
                        Some(codex_ask::Body::Access(access)) => {
                            grants += 1;
                            Ok(Answer::Given(wire::answer::Of::Grant(wire::GrantAnswer {
                                read: access.read.clone(),
                                write: access.write.clone(),
                                network: access.network,
                                for_session: false,
                            })))
                        }
                        _ => self.by_the_way(ask),
                    },
                    _ => self.by_the_way(ask),
                },
                |_| Ok(()),
            )
            .await?;
        if matches!(verdict, Verdict::Pass) && grants == 0 {
            return Err("Codex never asked for access".into());
        }
        Ok(verdict)
    }

    /// With the automatic reviewer amux's `auto` permission names, a
    /// command that needs approval is decided by Codex's reviewer, not
    /// asked.
    pub(super) async fn automatic_review(&self) -> Result<Verdict, String> {
        self.codex_work(
            Scenario::AutomaticReview,
            CodexCreateConfig {
                permission: Some("auto".into()),
                ..CodexCreateConfig::default()
            },
            "Run exactly this shell command once, with your shell tool, and nothing else: curl \
             -sI https://example.com. The sandbox has no network, so request escalated \
             permissions for it. Then reply with its status line.",
            |ask| self.by_the_way(ask),
            |log| {
                if !codex_items(log)
                    .iter()
                    .any(|item| matches!(item, Codex::Verdict(_)))
                {
                    return Err("no reviewer verdict".into());
                }
                Ok(())
            },
        )
        .await
    }

    /// Approvals that outlast the request: a command allowed with the rule
    /// for similar commands, and a file change allowed for the session.
    pub(super) async fn approval_scopes(&self) -> Result<Verdict, String> {
        self.codex_work(
            Scenario::ApprovalScopes,
            CodexCreateConfig {
                permission: Some("read-only".into()),
                ..CodexCreateConfig::default()
            },
            "Do these steps in order, one tool call each: run the shell command `touch \
             first.txt`; then use apply_patch to create notes.txt containing the line one; then \
             use apply_patch to change that line to two. The sandbox is read-only, so request \
             escalated permissions where you need them; I will approve. Then reply DONE.",
            |ask| match ask {
                OpenAsk::Codex(codex) => Ok(match &codex.body {
                    Some(codex_ask::Body::Command(_))
                        if codex
                            .decisions
                            .contains(&(wire::Decision::ApproveSimilar as i32)) =>
                    {
                        decide(wire::Decision::ApproveSimilar)
                    }
                    Some(codex_ask::Body::FileChange(_))
                        if codex
                            .decisions
                            .contains(&(wire::Decision::ApproveSession as i32)) =>
                    {
                        decide(wire::Decision::ApproveSession)
                    }
                    _ => return self.by_the_way(ask),
                }),
                _ => self.by_the_way(ask),
            },
            |log| {
                let decided = decisions(log);
                if decided.len() < 2 {
                    return Err(format!("only {decided:?} were decided"));
                }
                Ok(())
            },
        )
        .await
    }
}
