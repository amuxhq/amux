//! The headless Claude corpus: sessions amux drives so that what headless
//! Claude says in them is recorded, joined to claude-specs's live captures
//! (`claude-probe join`), and replayed by the interpreter's goldens, the
//! views' goldens, the fakes' conformance run and the strict decode tests.
//!
//! Each is a thing a person does through amux (a conversation compacted or
//! cleared, a model, permission or effort changed, a question or a tool
//! server's form answered, a message sent into the running turn) and is
//! judged as the asks are: by the real effect where there is one, and by
//! the shape of its own traffic replayed through the interpreter against
//! what the daemon committed.

use ui_state::{ItemBody, OpenAsk};
use wire::claude_item::Kind as Claude;
use wire::{
    AgentInput, Input, Kind, PromptInput, PutBlobRequest, SendInputRequest, agent_input, answer,
    send_input_response,
};

use super::asks::{Answer, questions, settled};
use super::{Follow, Install, Log, Scenario, TURN, Verdict, expect_outcome, judge};

/// A 32×32 solid red PNG, attached to a prompt and left in the project
/// for the provider to read back.
pub(super) const SQUARE_PNG: &[u8] = include_bytes!("../../../claude-specs/assets/square.png");

/// The tool the form-asking tool server offers (`claude-probe` run as
/// one, see its `__tool-server` mode).
const TOOL: &str = "mcp__external__ask_the_operator";

/// Two Bash calls give the running turn a boundary after the first, where
/// a message sent into it can join.
const TWO_COMMANDS: &str = "Use Bash to run exactly: python3 -c 'import time; time.sleep(15)'; \
     echo S1. Then use Bash to run exactly: python3 -c 'import time; time.sleep(5)'; echo S2. \
     Then reply exactly DONE plus any extra words the user asked for.";

/// The input carrying one arm.
fn arm(of: agent_input::Of) -> wire::AgentInput {
    AgentInput { of: Some(of) }
}

fn sdk_items(log: &Log) -> Vec<Claude> {
    log.ordered()
        .into_iter()
        .filter_map(|item| match ItemBody::decode(Kind::ClaudeSdk, &item.body) {
            ItemBody::Claude(body) => Some(body),
            _ => None,
        })
        .collect()
}

fn boundaries(log: &Log, kind: wire::BoundaryKind) -> usize {
    sdk_items(log)
        .iter()
        .filter(|item| matches!(item, Claude::Boundary(boundary) if boundary.kind() == kind))
        .count()
}

fn tools(log: &Log) -> Vec<wire::ToolCall> {
    sdk_items(log)
        .into_iter()
        .filter_map(|item| match item {
            Claude::Tool(call) => Some(call),
            _ => None,
        })
        .collect()
}

/// How each tool-server form and link was closed, in order.
fn forms(log: &Log) -> Vec<wire::AskOutcome> {
    sdk_items(log)
        .into_iter()
        .filter_map(|item| match item {
            Claude::Ask(ask) => ask.closed.map(|closed| closed.outcome()),
            _ => None,
        })
        .collect()
}

fn allow() -> Answer {
    Answer::Given(answer::Of::Permission(wire::PermissionAnswer {
        of: Some(wire::permission_answer::Of::Allow(wire::PermissionAllow {
            scope: None,
        })),
    }))
}

/// The positions of the options `labels` names in a question.
fn picked(question: &wire::Question, labels: &[&str]) -> Vec<u32> {
    question
        .options
        .iter()
        .enumerate()
        .filter(|(_, option)| labels.contains(&option.label.as_str()))
        .map(|(at, _)| at as u32)
        .collect()
}

fn question_answer(answers: Vec<wire::QuestionResponse>) -> Answer {
    Answer::Given(answer::Of::Question(wire::QuestionAnswer { answers }))
}

impl Install {
    /// A headless agent on `model`, with more of amux's options and of
    /// Claude's own arguments, taking input; the verdict when it cannot run
    /// turns here.
    async fn headless(
        &self,
        scenario: Scenario,
        model: &str,
        options: &[&str],
        extra: &[&str],
    ) -> Result<Result<(Vec<u8>, Follow), Verdict>, String> {
        let agent = self.create_as(scenario, model, options, extra).await?;
        let follow = self.follow(&agent);
        follow.ready().await?;
        if let Some(why) = follow.signed_out() {
            return Ok(Err(Verdict::Unavailable(why)));
        }
        Ok(Ok((agent, follow)))
    }

    /// A prompt sent and the `n`th turn ended as it should.
    pub(super) async fn turn(
        &self,
        scenario: Scenario,
        follow: &Follow,
        text: &str,
        n: usize,
    ) -> Result<(), String> {
        self.send(scenario, text).await?;
        expect_outcome(follow.turn_end(n).await?, wire::TurnOutcome::Completed)
    }

    /// The scenario's own traffic, replayed, has the committed shape.
    pub(super) fn judged(&self, scenario: Scenario, follow: &Follow) -> Result<Verdict, String> {
        self.note_model(follow);
        judge(
            &self.captured(scenario)?.shape(self.kind),
            &follow.snapshot().shape(self.kind),
        )?;
        Ok(Verdict::Pass)
    }

    /// Asks are answered by `answer` until the session settles after a
    /// turn, and the turns that ended all completed.
    pub(super) async fn settle(
        &self,
        agent: &[u8],
        follow: &Follow,
        what: &str,
        answer: impl FnMut(&OpenAsk) -> Result<Answer, String>,
    ) -> Result<(), String> {
        let kind = self.kind;
        self.drive(agent, follow, what, answer, |log| settled(kind, log))
            .await?;
        for outcome in follow.snapshot().turns(kind) {
            expect_outcome(outcome, wire::TurnOutcome::Completed)?;
        }
        Ok(())
    }

    /// Sends an input and returns the id it was sent with.
    pub(super) async fn send_input(
        &self,
        agent: &[u8],
        of: wire::AgentInput,
    ) -> Result<Vec<u8>, String> {
        let id = uuid::Uuid::new_v4().as_bytes().to_vec();
        let response = self
            .client
            .sessions
            .clone()
            .send_input(SendInputRequest {
                agent_id: agent.to_vec(),
                input: Some(Input::new(id.clone(), of)),
            })
            .await
            .map_err(|status| status.to_string())?
            .into_inner();
        match response.of {
            Some(send_input_response::Of::Accepted(_)) => Ok(id),
            other => Err(format!("the input was not accepted: {other:?}")),
        }
    }

    /// Two prompts, two turns: the second answered from the first.
    pub(super) async fn multi_turn(&self) -> Result<Verdict, String> {
        let scenario = Scenario::MultiTurn;
        let (_, follow) = match self
            .headless(scenario, super::CLAUDE_MODEL, &[], &[])
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.turn(
            scenario,
            &follow,
            "Remember the word ALBATROSS. Reply with exactly REMEMBERED.",
            1,
        )
        .await?;
        self.turn(
            scenario,
            &follow,
            "What word did I ask you to remember? Reply with just that word.",
            2,
        )
        .await?;
        self.judged(scenario, &follow)
    }

    /// Two turns, then `/compact`: the conversation reads compacted.
    pub(super) async fn compacted(&self) -> Result<Verdict, String> {
        let scenario = Scenario::Compacted;
        let (_, follow) = match self
            .headless(scenario, super::CLAUDE_MODEL, &[], &[])
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.turn(
            scenario,
            &follow,
            "Write three short paragraphs about the history of rope-making.",
            1,
        )
        .await?;
        self.turn(
            scenario,
            &follow,
            "Now two short paragraphs about knots.",
            2,
        )
        .await?;
        self.turn(scenario, &follow, "/compact", 3).await?;
        if boundaries(&follow.snapshot(), wire::BoundaryKind::Compacted) == 0 {
            return Err("compacting left no compacted boundary".into());
        }
        self.judged(scenario, &follow)
    }

    /// A turn, then the conversation cleared: the session reads cleared.
    pub(super) async fn cleared(&self) -> Result<Verdict, String> {
        let scenario = Scenario::Cleared;
        let (agent, follow) = match self
            .headless(scenario, super::CLAUDE_MODEL, &[], &[])
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.turn(
            scenario,
            &follow,
            "Reply with the single word pong and nothing else.",
            1,
        )
        .await?;
        self.input(&agent, arm(agent_input::Of::Clear(wire::Clear {})))
            .await?;
        follow
            .until(TURN, "a cleared boundary", |log| {
                boundaries(log, wire::BoundaryKind::Cleared) > 0 && log.phase() == wire::Phase::Idle
            })
            .await?;
        self.judged(scenario, &follow)
    }

    /// The permission and the model changed from amux, then a turn: the
    /// session runs under both.
    pub(super) async fn controls(&self) -> Result<Verdict, String> {
        let scenario = Scenario::Controls;
        let (agent, follow) = match self
            .headless(scenario, super::CLAUDE_MODEL, &[], &[])
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.input(
            &agent,
            arm(agent_input::Of::Permission(wire::SetPermission {
                value: "acceptEdits".into(),
            })),
        )
        .await?;
        self.input(
            &agent,
            arm(agent_input::Of::Model(wire::SetModel {
                model: Some("sonnet".into()),
            })),
        )
        .await?;
        // Accepting edits lets the file be made without an ask.
        self.turn(
            scenario,
            &follow,
            "Use your Write tool to create a file named notes.txt in the current directory \
             containing the single word pong. Then reply with the single word done.",
            1,
        )
        .await?;
        let state = follow.snapshot().state(self.kind);
        if state.permission.as_deref() != Some("acceptEdits") {
            return Err(format!("the permission reads {:?}", state.permission));
        }
        if !state.model.as_deref().is_some_and(|m| m.contains("sonnet")) {
            return Err(format!("the model reads {:?}", state.model));
        }
        if !tools(&follow.snapshot())
            .iter()
            .any(|call| call.name == "Write" && call.state() == wire::ToolState::Succeeded)
        {
            return Err("no Write call succeeded".into());
        }
        self.judged(scenario, &follow)
    }

    /// A turn at a launch effort, then the effort changed and a second
    /// turn: the session reads each in turn.
    pub(super) async fn effort(&self) -> Result<Verdict, String> {
        let scenario = Scenario::Effort;
        let (agent, follow) = match self
            .headless(scenario, "sonnet", &["--effort", "low"], &[])
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        let effort = |follow: &Follow| follow.snapshot().state(Kind::ClaudeSdk).effort;
        self.turn(
            scenario,
            &follow,
            "Reply with the single word pong and nothing else.",
            1,
        )
        .await?;
        if effort(&follow).as_deref() != Some("low") {
            return Err(format!("the launch effort reads {:?}", effort(&follow)));
        }
        self.input(
            &agent,
            arm(agent_input::Of::Effort(wire::SetEffort {
                effort: Some("high".into()),
            })),
        )
        .await?;
        self.turn(
            scenario,
            &follow,
            "Reply with the single word pong and nothing else.",
            2,
        )
        .await?;
        if effort(&follow).as_deref() != Some("high") {
            return Err(format!("the changed effort reads {:?}", effort(&follow)));
        }
        self.judged(scenario, &follow)
    }

    /// A turn with Claude's thinking shown, as its own arguments ask: the
    /// thinking item carries text. Haiku 4.5 is the model that shows it.
    pub(super) async fn configured_turn(&self) -> Result<Verdict, String> {
        let scenario = Scenario::ConfiguredTurn;
        let extra = [
            "--max-thinking-tokens",
            "1024",
            "--thinking-display",
            "summarized",
        ];
        let (_, follow) = match self
            .headless(scenario, "claude-haiku-4-5-20251001", &[], &extra)
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.turn(
            scenario,
            &follow,
            "Think it through briefly, then answer: what is the capital of France?",
            1,
        )
        .await?;
        let thought = follow.snapshot().ordered().into_iter().any(|item| {
            matches!(
                ItemBody::decode(Kind::ClaudeSdk, &item.body),
                ItemBody::Claude(Claude::Thinking(_))
            ) && !item.text.is_empty()
        });
        if !thought {
            return Err("no thinking item carries text".into());
        }
        self.judged(scenario, &follow)
    }

    /// One question, answered with an option: the record reads answered.
    pub(super) async fn question_asked(&self) -> Result<Verdict, String> {
        let scenario = Scenario::QuestionAsked;
        let (agent, follow) = match self
            .headless(scenario, super::CLAUDE_MODEL, &[], &[])
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(
            scenario,
            "Use AskUserQuestion to ask exactly one single-select question with header Color, \
             question 'Which color do you prefer?', and options Red and Blue. Then repeat my \
             answer.",
        )
        .await?;
        self.settle(
            &agent,
            &follow,
            "the question to be answered",
            |ask| match questions(ask) {
                Some(questions) => Ok(question_answer(
                    questions
                        .iter()
                        .map(|question| wire::QuestionResponse {
                            selected: picked(question, &["Blue"]),
                            ..Default::default()
                        })
                        .collect(),
                )),
                None => self.by_the_way(ask),
            },
        )
        .await?;
        self.judged(scenario, &follow)
    }

    /// Four questions in one ask: single and multiple choice, previews
    /// and an answer of the person's own.
    pub(super) async fn question_every_shape(&self) -> Result<Verdict, String> {
        let scenario = Scenario::QuestionEveryShape;
        let (agent, follow) = match self
            .headless(scenario, super::CLAUDE_MODEL, &[], &[])
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(
            scenario,
            "Use exactly one AskUserQuestion call containing exactly four questions, in this \
             order. 1: header Color, question 'Which color do you prefer?', single-select, \
             options Red and Blue. 2: header Tools, question 'Which tools do you need?', \
             multiSelect true, options Hammer, Saw and Drill. 3: header Layout, question 'Which \
             layout should the page use?', single-select, four options Sidebar, Topbar, Grid \
             and Stack; give every one of these four options a preview holding a small ASCII \
             sketch of that layout. 4: header Snack, question 'Which snack do you want?', \
             single-select, options Apple, Pretzel and Popcorn. Add nothing else. After I \
             answer, repeat all four answers.",
        )
        .await?;
        let mut shapes = None;
        self.settle(&agent, &follow, "the questions to be answered", |ask| {
            let Some(questions) = questions(ask) else {
                return self.by_the_way(ask);
            };
            shapes.get_or_insert(questions.len());
            Ok(question_answer(
                questions
                    .iter()
                    .map(|question| match question.header.as_str() {
                        "Color" => wire::QuestionResponse {
                            selected: picked(question, &["Blue"]),
                            ..Default::default()
                        },
                        "Tools" => wire::QuestionResponse {
                            selected: picked(question, &["Hammer", "Drill"]),
                            ..Default::default()
                        },
                        "Layout" => wire::QuestionResponse {
                            selected: picked(question, &["Grid"]),
                            ..Default::default()
                        },
                        _ => wire::QuestionResponse {
                            other: Some("Whatever is fresh".into()),
                            ..Default::default()
                        },
                    })
                    .collect(),
            ))
        })
        .await?;
        if shapes != Some(4) {
            return Err(format!("the ask held {shapes:?} questions, not four"));
        }
        self.judged(scenario, &follow)
    }

    /// A question left open and the turn interrupted: the question closes
    /// with the turn.
    pub(super) async fn question_dismissed(&self) -> Result<Verdict, String> {
        let scenario = Scenario::QuestionDismissed;
        let (agent, follow) = match self
            .headless(scenario, super::CLAUDE_MODEL, &[], &[])
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(
            scenario,
            "Use AskUserQuestion to ask one single-select question with header Color, question \
             'Which color do you prefer?', and options Red and Blue.",
        )
        .await?;
        let kind = self.kind;
        follow
            .until(TURN, "the question", |log| {
                log.state(kind)
                    .asks
                    .iter()
                    .any(|ask| questions(ask).is_some())
            })
            .await?;
        self.input(&agent, self.interrupt()).await?;
        expect_outcome(follow.turn_end(1).await?, wire::TurnOutcome::Interrupted)?;
        follow
            .until(TURN, "the question to close", |log| settled(kind, log))
            .await?;
        self.judged(scenario, &follow)
    }

    /// A plan sent back with a note, then the revised plan approved with
    /// the permission it offers to switch to: the change is made.
    pub(super) async fn plan_reviewed(&self) -> Result<Verdict, String> {
        let scenario = Scenario::PlanReviewed;
        let readme = self.project(scenario)?.join("README.md");
        std::fs::write(&readme, "CURRENT\n").map_err(|error| error.to_string())?;
        let (agent, follow) = match self
            .headless(
                scenario,
                super::CLAUDE_MODEL,
                &[],
                &["--permission-mode", "plan"],
            )
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(
            scenario,
            "Plan changing README.md's only line from CURRENT to UPDATED. It is a one-step \
             plan: do not ask me anything, and put the plan to me for approval with the \
             ExitPlanMode tool.",
        )
        .await?;
        let mut plans = 0;
        let kind = self.kind;
        self.drive(
            &agent,
            &follow,
            "the revised plan to be approved and carried out",
            |ask| {
                let OpenAsk::Claude(claude) = ask else {
                    return self.by_the_way(ask);
                };
                let Some(wire::ask::Body::Plan(plan)) = &claude.body else {
                    return self.by_the_way(ask);
                };
                plans += 1;
                let (choice, note) = match plans {
                    1 => (
                        wire::PlanChoice::KeepPlanning,
                        Some(
                            "Also say why the change is needed, then put the plan to me \
                             again with ExitPlanMode."
                                .to_owned(),
                        ),
                    ),
                    _ if !plan.switch_permission.is_empty() => {
                        (wire::PlanChoice::StartSwitching, None)
                    }
                    _ => (wire::PlanChoice::Start, None),
                };
                Ok(Answer::Given(answer::Of::Plan(wire::PlanAnswer {
                    choice: choice as i32,
                    note,
                })))
            },
            |log| {
                std::fs::read_to_string(&readme).is_ok_and(|text| text.contains("UPDATED"))
                    && settled(kind, log)
            },
        )
        .await?;
        let verdicts: Vec<wire::PlanVerdict> = sdk_items(&follow.snapshot())
            .into_iter()
            .filter_map(|item| match item {
                Claude::Plan(plan) => Some(plan.verdict()),
                _ => None,
            })
            .collect();
        if !verdicts.contains(&wire::PlanVerdict::SentBack) {
            return Err(format!("no plan reads sent back: {verdicts:?}"));
        }
        self.judged(scenario, &follow)
    }

    /// A tool server's form or link, answered as `answer` says: the server
    /// hears the answer and the ask closes as it does.
    /// `asked`: the tool is asked about and allowed, rather than allowed at
    /// launch.
    async fn elicitation(
        &self,
        scenario: Scenario,
        asked: bool,
        word: &str,
        answer: answer::Of,
        closed: wire::AskOutcome,
    ) -> Result<Verdict, String> {
        let probe = provider_fakes::binaries::path("claude-probe");
        let servers = serde_json::json!({
            "mcpServers": {
                "external": {"command": probe, "args": ["__tool-server"]},
            },
        })
        .to_string();
        let mut extra = vec!["--mcp-config", &servers];
        if !asked {
            extra.extend(["--allowedTools", TOOL]);
        }
        let (agent, follow) = match self
            .headless(scenario, super::CLAUDE_MODEL, &[], &extra)
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(
            scenario,
            &format!(
                "Call the {TOOL} tool with word set to {word}, then reply with exactly what it \
                 returned."
            ),
        )
        .await?;
        let mut answer = Some(answer);
        self.settle(
            &agent,
            &follow,
            "the form to be answered",
            |ask| match ask {
                OpenAsk::Claude(claude)
                    if matches!(
                        claude.body,
                        Some(wire::ask::Body::Form(_) | wire::ask::Body::Link(_))
                    ) =>
                {
                    answer
                        .take()
                        .map(Answer::Given)
                        .ok_or_else(|| "a second form opened".to_owned())
                }
                _ => Ok(allow()),
            },
        )
        .await?;
        let outcomes = forms(&follow.snapshot());
        if outcomes != [closed] {
            return Err(format!("the forms closed {outcomes:?}, not [{closed:?}]"));
        }
        let permissions = tools(&follow.snapshot())
            .iter()
            .filter(|call| call.decision.is_some())
            .count();
        if (permissions > 0) != asked {
            return Err(format!("{permissions} calls were decided"));
        }
        self.judged(scenario, &follow)
    }

    pub(super) async fn elicitation_accepted(&self) -> Result<Verdict, String> {
        self.elicitation(
            Scenario::ElicitationAccepted,
            true,
            "ALBATROSS",
            answer::Of::Form(wire::FormAnswer {
                action: wire::FormAction::Accept as i32,
                content_json: br#"{"confirmed":"ALBATROSS"}"#.to_vec(),
            }),
            wire::AskOutcome::Answered,
        )
        .await
    }

    pub(super) async fn elicitation_declined(&self) -> Result<Verdict, String> {
        self.elicitation(
            Scenario::ElicitationDeclined,
            false,
            "ALBATROSS",
            answer::Of::Form(wire::FormAnswer {
                action: wire::FormAction::Decline as i32,
                content_json: Vec::new(),
            }),
            wire::AskOutcome::Declined,
        )
        .await
    }

    pub(super) async fn elicitation_cancelled(&self) -> Result<Verdict, String> {
        self.elicitation(
            Scenario::ElicitationCancelled,
            false,
            "ALBATROSS",
            answer::Of::Form(wire::FormAnswer {
                action: wire::FormAction::Cancel as i32,
                content_json: Vec::new(),
            }),
            wire::AskOutcome::Cancelled,
        )
        .await
    }

    pub(super) async fn elicitation_link_accepted(&self) -> Result<Verdict, String> {
        self.elicitation(
            Scenario::ElicitationLinkAccepted,
            false,
            "LINK",
            answer::Of::Link(wire::LinkAnswer {
                action: wire::FormAction::Accept as i32,
            }),
            wire::AskOutcome::Answered,
        )
        .await
    }

    /// A tool server whose command cannot start: its health reads failed
    /// and the session works without it.
    pub(super) async fn failed_tool_server(&self) -> Result<Verdict, String> {
        let scenario = Scenario::FailedToolServer;
        let servers = serde_json::json!({
            "mcpServers": {"broken": {"command": "amux-live-no-such-tool-server"}},
        })
        .to_string();
        let (_, follow) = match self
            .headless(
                scenario,
                super::CLAUDE_MODEL,
                &[],
                &["--mcp-config", &servers],
            )
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.turn(
            scenario,
            &follow,
            "Reply with the single word pong and nothing else.",
            1,
        )
        .await?;
        let servers = follow.snapshot().state(self.kind).servers;
        if !servers.iter().any(|server| {
            server.name == "broken" && server.status() == wire::ToolServerStatus::Failed
        }) {
            return Err(format!("no failed server named broken: {servers:?}"));
        }
        self.judged(scenario, &follow)
    }

    /// A prompt whose work is one tool call, each asked about and allowed,
    /// that must leave a call in `state` named `tool`.
    async fn work(
        &self,
        scenario: Scenario,
        extra: &[&str],
        prompt: &str,
        tool: &str,
        state: wire::ToolState,
    ) -> Result<Verdict, String> {
        let (agent, follow) = match self
            .headless(scenario, super::CLAUDE_MODEL, &[], extra)
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(scenario, prompt).await?;
        self.settle(&agent, &follow, "the work to finish", |ask| {
            self.by_the_way(ask)
        })
        .await?;
        let calls = tools(&follow.snapshot());
        if !calls
            .iter()
            .any(|call| call.name == tool && call.state() == state)
        {
            return Err(format!(
                "no {tool} call reads {}: {:?}",
                state.as_str_name(),
                calls
                    .iter()
                    .map(|call| (call.name.clone(), call.state().as_str_name()))
                    .collect::<Vec<_>>()
            ));
        }
        self.judged(scenario, &follow)
    }

    pub(super) async fn failing_command(&self) -> Result<Verdict, String> {
        self.work(
            Scenario::FailingCommand,
            &[],
            "Run exactly this Bash command once: ls does-not-exist. Then tell me whether it \
             failed.",
            "Bash",
            wire::ToolState::Failed,
        )
        .await
    }

    /// Headless Claude offers its task tools only when its launch names
    /// them. Their calls are no items of their own: they make the agent's
    /// task list.
    pub(super) async fn task_list(&self) -> Result<Verdict, String> {
        let scenario = Scenario::TaskList;
        let (agent, follow) = match self
            .headless(
                scenario,
                super::CLAUDE_MODEL,
                &[],
                &["--tools", "TaskCreate,TaskUpdate,TaskList,TaskGet"],
            )
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(
            scenario,
            "Track this work with your task or todo tool: create three tasks named Draft, \
             Review and Publish. Then mark Draft in progress, then mark Draft completed. Do \
             nothing else, then stop.",
        )
        .await?;
        self.settle(&agent, &follow, "the tasks to be made", |ask| {
            self.by_the_way(ask)
        })
        .await?;
        let tasks = follow.snapshot().state(Kind::ClaudeSdk).tasks;
        let read: Vec<_> = tasks
            .entries
            .iter()
            .map(|task| (task.subject.as_str(), task.status()))
            .collect();
        use wire::TaskListStatus::{Completed, Pending};
        if read
            != [
                ("Draft", Completed),
                ("Review", Pending),
                ("Publish", Pending),
            ]
        {
            return Err(format!("the task list reads {read:?}"));
        }
        self.judged(scenario, &follow)
    }

    pub(super) async fn background_shell(&self) -> Result<Verdict, String> {
        self.work(
            Scenario::BackgroundShell,
            &[],
            "Use Bash with run_in_background set to true to run exactly: for i in 1 2 3; do echo \
             tick $i; sleep 2; done. Then wait about eight seconds by running Bash: sleep 8. \
             Then read the background shell's output with the tool for reading background \
             output, and report the last line it printed.",
            "Bash",
            wire::ToolState::Succeeded,
        )
        .await
    }

    pub(super) async fn subagent_task(&self) -> Result<Verdict, String> {
        let agents = serde_json::json!({
            "counter": {
                "description": "Counts to three and reports the result.",
                "prompt": "Reply with exactly ONE TWO THREE and nothing else.",
                "tools": [],
                "model": "haiku",
            },
        })
        .to_string();
        self.work(
            Scenario::SubagentTask,
            &["--agents", &agents],
            "Delegate to the `counter` subagent, then reply with exactly what it said.",
            "Agent",
            wire::ToolState::Succeeded,
        )
        .await
    }

    /// An image attached to a prompt and the same image read from the
    /// project: the Read call carries the image it read.
    pub(super) async fn image(&self) -> Result<Verdict, String> {
        let scenario = Scenario::Image;
        std::fs::write(self.project(scenario)?.join("square.png"), SQUARE_PNG)
            .map_err(|error| error.to_string())?;
        let (agent, follow) = match self
            .headless(scenario, super::CLAUDE_MODEL, &[], &[])
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
        self.send_input(
            &agent,
            arm(agent_input::Of::Prompt(PromptInput {
                text: "Name the color of the attached image in one word. Then read square.png \
                       in the working directory with the Read tool and say whether it is the \
                       same image."
                    .into(),
                attachments: vec![wire::Attachment {
                    of: Some(wire::attachment::Of::Image(blob)),
                }],
            })),
        )
        .await?;
        self.settle(&agent, &follow, "the image to be read", |ask| {
            self.by_the_way(ask)
        })
        .await?;
        if !tools(&follow.snapshot())
            .iter()
            .any(|call| call.name == "Read" && !call.attachments.is_empty())
        {
            return Err("no Read call carries the image it read".into());
        }
        self.judged(scenario, &follow)
    }

    /// A rejected credential: the turn fails and the sign-in reads failed.
    pub(super) async fn sign_in_problem(&self) -> Result<Verdict, String> {
        let scenario = Scenario::SignInProblem;
        let settings = serde_json::json!({
            "env": {
                "ANTHROPIC_API_KEY": "sk-ant-api03-amux-live-invalid",
                "CLAUDE_CODE_MAX_RETRIES": "2",
            },
        })
        .to_string();
        let agent = self
            .create_as(
                scenario,
                super::CLAUDE_MODEL,
                &[],
                &["--settings", &settings],
            )
            .await?;
        let follow = self.follow(&agent);
        follow.ready().await?;
        self.send(
            scenario,
            "Reply with the single word pong and nothing else.",
        )
        .await?;
        follow.turn_end(1).await?;
        let kind = self.kind;
        follow
            .until(TURN, "the sign-in to read the problem", |log| {
                log.phase() == wire::Phase::Idle
                    && !matches!(
                        log.state(kind).sign_in.state(),
                        wire::SignInState::SignedIn | wire::SignInState::Unknown
                    )
            })
            .await?;
        self.judged(scenario, &follow)
    }

    /// A message queued while a command runs and sent into the running
    /// turn, and another left queued: the first reads steered and joins
    /// that turn, the second runs as a turn of its own.
    pub(super) async fn steer_folded(&self) -> Result<Verdict, String> {
        let scenario = Scenario::SteerFolded;
        let (agent, follow) = match self
            .headless(
                scenario,
                super::CLAUDE_MODEL,
                &[],
                &["--allowedTools", "Bash"],
            )
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(scenario, TWO_COMMANDS).await?;
        follow
            .until(TURN, "the first command to run", |log| {
                tools(log)
                    .iter()
                    .any(|call| call.state() == wire::ToolState::Running)
            })
            .await?;
        let prompt = |text: &str| {
            arm(agent_input::Of::Prompt(PromptInput {
                text: text.into(),
                attachments: Vec::new(),
            }))
        };
        let steered = self
            .send_input(&agent, prompt("Also append the word BANANA."))
            .await?;
        self.send_input(&agent, prompt("Then reply with exactly LATER."))
            .await?;
        self.input(
            &agent,
            arm(agent_input::Of::SendNow(wire::SendQueuedNow {
                queued_input_id: steered,
            })),
        )
        .await?;
        expect_outcome(follow.turn_end(2).await?, wire::TurnOutcome::Completed)?;
        let kind = self.kind;
        follow
            .until(TURN, "the session to settle", |log| settled(kind, log))
            .await?;
        if !sdk_items(&follow.snapshot())
            .iter()
            .any(|item| matches!(item, Claude::Steer(_)))
        {
            return Err("the message sent now reads no steer".into());
        }
        self.note_model(&follow);
        let mut recorded = self.captured(scenario)?.shape(kind);
        recorded.queued_ran_at_once();
        let mut live = follow.snapshot().shape(kind);
        live.queued_ran_at_once();
        judge(&recorded, &live)?;
        Ok(Verdict::Pass)
    }
}
