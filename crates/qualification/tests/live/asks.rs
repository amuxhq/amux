//! Deciding what the provider puts to the person: a plan approved, and
//! questions with one skipped and one replied to instead.
//!
//! Neither has a recording in the corpus that does what amux now does, so
//! each is judged against its own traffic: what the real provider did,
//! recorded through the probe and replayed through the interpreter, must
//! have the shape the daemon committed.

use std::collections::BTreeSet;
use std::time::Duration;

use ui_state::{ItemBody, OpenAsk};
use wire::{AgentInput, CodexCreateConfig, Kind, Phase, agent_input, answer};

use super::{Install, Log, Scenario, TURN, Verdict, expect_outcome, judge};

/// What the person says instead of answering the second questions.
pub const REPLY: &str = "Never mind the shape. Reply with the single word done and nothing else.";

/// How a scenario answers an ask that opened.
pub enum Answer {
    Given(answer::Of),
    /// Codex's approval of a command or file change.
    Approve,
}

impl Install {
    /// Answers each ask that opens with what `answer` says, until `done`
    /// holds of the session.
    pub(super) async fn drive(
        &self,
        agent: &[u8],
        follow: &super::Follow,
        what: &str,
        mut answer: impl FnMut(&OpenAsk) -> Result<Answer, String>,
        done: impl Fn(&Log) -> bool,
    ) -> Result<(), String> {
        let mut answered = BTreeSet::new();
        // A scenario may run two turns, with asks between.
        let deadline = tokio::time::Instant::now() + TURN * 2;
        loop {
            let live = follow.snapshot();
            if done(&live) {
                return Ok(());
            }
            for ask in live.state(self.kind).asks {
                if answered.insert(ask.key().to_owned()) {
                    let of = self.answer_input(&ask, answer(&ask)?)?;
                    self.input(agent, of).await?;
                }
            }
            if tokio::time::Instant::now() > deadline {
                return Err(format!(
                    "timed out waiting for {what}; the session reads {:?}",
                    live.shape(self.kind)
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn answer_input(&self, ask: &OpenAsk, answer: Answer) -> Result<wire::AgentInput, String> {
        let ask_key = ask.key().to_owned();
        Ok(match (self.kind, answer) {
            (_, Answer::Given(of)) => AgentInput {
                of: Some(agent_input::Of::Answer(wire::Answer {
                    ask_key,
                    of: Some(of),
                })),
            },
            (Kind::Codex, Answer::Approve) => self.allow(ask)?,
            _ => return Err(format!("no answer fits {}", super::ask_token(ask))),
        })
    }

    /// The answer for an ask a scenario did not ask for but may meet on
    /// the way: a permission or approval allowed once, a question's first
    /// option each.
    pub(super) fn by_the_way(&self, ask: &OpenAsk) -> Result<Answer, String> {
        if let Some(questions) = questions(ask) {
            let answers = questions
                .iter()
                .map(|_| wire::QuestionResponse {
                    selected: vec![0],
                    ..Default::default()
                })
                .collect();
            return Ok(self.question_answer(wire::QuestionAnswer { answers }));
        }
        match ask {
            OpenAsk::Claude(ask) if matches!(ask.body, Some(wire::ask::Body::Permission(_))) => Ok(
                Answer::Given(answer::Of::Permission(wire::PermissionAnswer {
                    of: Some(wire::permission_answer::Of::Allow(wire::PermissionAllow {
                        scope: None,
                    })),
                })),
            ),
            OpenAsk::Codex(_) if !is_plan(ask) => Ok(Answer::Approve),
            _ => Err(format!(
                "an unexpected ask opened: {}",
                super::ask_token(ask)
            )),
        }
    }

    fn question_answer(&self, answer: wire::QuestionAnswer) -> Answer {
        Answer::Given(answer::Of::Question(answer))
    }

    fn plan_answer(&self, choice: wire::PlanChoice) -> Answer {
        let answer = wire::PlanAnswer {
            choice: choice as i32,
            note: None,
        };
        Answer::Given(answer::Of::Plan(answer))
    }

    /// An agent that starts in plan mode: Claude by its own flag, Codex by
    /// amux's mode.
    async fn create_planning(&self, scenario: Scenario) -> Result<Vec<u8>, String> {
        match self.kind {
            Kind::Codex => {
                self.create_codex(
                    scenario,
                    CodexCreateConfig {
                        mode: Some("plan".into()),
                        ..CodexCreateConfig::default()
                    },
                )
                .await
            }
            _ => {
                self.create_with(scenario, &["--permission-mode", "plan"])
                    .await
            }
        }
    }

    /// A plan for a one-line change, approved from amux: the plan reads
    /// approved and the change is made.
    pub(super) async fn plan(&self) -> Result<Verdict, String> {
        let project = self.project(Scenario::Plan)?;
        let readme = project.join("README.md");
        std::fs::write(&readme, "CURRENT\n").map_err(|error| error.to_string())?;
        let agent = self.create_planning(Scenario::Plan).await?;
        let follow = self.follow(&agent);
        follow.ready().await?;
        if let Some(why) = follow.signed_out() {
            return Ok(Verdict::Unavailable(why));
        }
        // Claude, left to choose, may write its plan out instead of putting
        // it to the person with its plan tool.
        let how = match self.kind {
            Kind::Codex => "",
            _ => " with the ExitPlanMode tool",
        };
        self.send(
            Scenario::Plan,
            &format!(
                "Plan changing README.md's only line from CURRENT to UPDATED. It is a one-step \
                 plan: do not ask me anything, and put the plan to me for approval{how}."
            ),
        )
        .await?;
        let kind = self.kind;
        self.drive(
            &agent,
            &follow,
            "the plan to be approved and carried out",
            |ask| {
                if is_plan(ask) {
                    Ok(self.plan_answer(wire::PlanChoice::Start))
                } else {
                    self.by_the_way(ask)
                }
            },
            |log| {
                std::fs::read_to_string(&readme).is_ok_and(|text| text.contains("UPDATED"))
                    && settled(kind, log)
            },
        )
        .await?;
        self.note_model(&follow);
        let live = follow.snapshot();
        let verdicts = plans(kind, &live);
        if !verdicts.contains(&wire::PlanVerdict::Approved) {
            return Err(format!("no plan reads approved: {verdicts:?}"));
        }
        expect_outcome(
            *live.turns(kind).last().ok_or("no turn ended")?,
            wire::TurnOutcome::Completed,
        )?;
        judge(
            &self.captured(Scenario::Plan)?.shape(kind),
            &live.shape(kind),
        )?;
        Ok(Verdict::Pass)
    }

    /// Two questions asked together, the first skipped and the second
    /// answered, then a third replied to instead: the records read
    /// answered 1 of 2 and replied instead with the words. Terminal
    /// Claude's skip moves through its form to the review screen and
    /// submits from there; Codex asks questions only while planning.
    pub(super) async fn questions(&self) -> Result<Verdict, String> {
        let agent = match self.kind {
            Kind::Codex => self.create_planning(Scenario::Questions).await?,
            _ => self.create(Scenario::Questions).await?,
        };
        let follow = self.follow(&agent);
        follow.ready().await?;
        if let Some(why) = follow.signed_out() {
            return Ok(Verdict::Unavailable(why));
        }
        let tool = match self.kind {
            Kind::Codex => "your tool for asking the user questions",
            _ => "the AskUserQuestion tool",
        };
        self.send(
            Scenario::Questions,
            &format!(
                "Use {tool} once, asking me two questions in that one call: 'Which colour?' \
                 with the options Red and Blue, and 'Which size?' with the options Small and \
                 Large. After I answer, use it once more to ask me one question, 'Which \
                 shape?' with the options Circle and Square. Then reply with the single word \
                 done. Do nothing else."
            ),
        )
        .await?;
        let kind = self.kind;
        let mut asked = 0;
        let mut failure = None;
        self.drive(
            &agent,
            &follow,
            "both questions to be answered and the turn to end",
            |ask| {
                // Codex plans as it goes; whatever it proposes is left be.
                if is_plan(ask) {
                    return Ok(self.plan_answer(wire::PlanChoice::KeepPlanning));
                }
                let Some(questions) = questions(ask) else {
                    return self.by_the_way(ask);
                };
                asked += 1;
                let nothing = || wire::QuestionResponse::default();
                Ok(match asked {
                    1 => {
                        if questions.len() < 2 {
                            failure = Some(format!(
                                "the first ask held {} question, not two",
                                questions.len()
                            ));
                        }
                        // The first skipped, the rest given their first option.
                        let answers = questions
                            .iter()
                            .enumerate()
                            .map(|(at, _)| match at {
                                0 => nothing(),
                                _ => wire::QuestionResponse {
                                    selected: vec![0],
                                    ..Default::default()
                                },
                            })
                            .collect();
                        self.question_answer(wire::QuestionAnswer { answers })
                    }
                    _ => {
                        let reply = wire::ReplyInstead {
                            text: REPLY.to_owned(),
                            answers_so_far: questions.iter().map(|_| nothing()).collect(),
                        };
                        Answer::Given(answer::Of::Reply(reply))
                    }
                })
            },
            |log| question_records(kind, log).len() >= 2 && settled(kind, log),
        )
        .await?;
        if let Some(failure) = failure {
            return Err(failure);
        }
        self.note_model(&follow);
        let live = follow.snapshot();
        let records = question_records(kind, &live);
        let skipped = records.iter().any(|closed| {
            closed.outcome() == wire::AskOutcome::Answered
                && closed.answers.len() >= 2
                && skipped(&closed.answers[0])
                && closed.answers[1..].iter().all(|answer| !skipped(answer))
        });
        if !skipped {
            return Err(format!(
                "no record reads the first question skipped and the rest answered: {records:?}"
            ));
        }
        if !records
            .iter()
            .any(|closed| closed.outcome() == wire::AskOutcome::Replied && closed.reply == REPLY)
        {
            return Err(format!("no record reads replied instead: {records:?}"));
        }
        let mut recorded = self.captured(Scenario::Questions)?.shape(kind);
        if kind == Kind::ClaudePty {
            recorded.reply_typed_at_once();
        }
        judge(&recorded, &live.shape(kind))?;
        Ok(Verdict::Pass)
    }
}

/// The questions an ask puts, when it is a question.
pub(super) fn questions(ask: &OpenAsk) -> Option<Vec<wire::Question>> {
    match ask {
        OpenAsk::Claude(ask) => match &ask.body {
            Some(wire::ask::Body::Question(question)) if !question.provider_dialog => {
                Some(question.questions.clone())
            }
            _ => None,
        },
        OpenAsk::Codex(ask) => match &ask.body {
            Some(wire::codex_ask::Body::Question(question)) => Some(question.questions.clone()),
            _ => None,
        },
    }
}

fn is_plan(ask: &OpenAsk) -> bool {
    match ask {
        OpenAsk::Claude(ask) => matches!(ask.body, Some(wire::ask::Body::Plan(_))),
        OpenAsk::Codex(ask) => matches!(ask.body, Some(wire::codex_ask::Body::Plan(_))),
    }
}

/// Nothing picked, typed or hidden: the question was skipped.
fn skipped(answer: &wire::AnsweredQuestion) -> bool {
    answer.picked.is_empty() && answer.other.is_none() && !answer.hidden
}

/// The agent takes input again, nothing is asked and a turn has ended.
pub(super) fn settled(kind: Kind, log: &Log) -> bool {
    log.phase() == Phase::Idle && log.state(kind).asks.is_empty() && !log.turns(kind).is_empty()
}

/// Every plan's verdict, in order.
fn plans(kind: Kind, log: &Log) -> Vec<wire::PlanVerdict> {
    use wire::claude_item::Kind as Claude;
    use wire::codex_item::Kind as Codex;
    log.ordered()
        .into_iter()
        .filter_map(|item| match ItemBody::decode(kind, &item.body) {
            ItemBody::Claude(Claude::Plan(plan)) | ItemBody::Codex(Codex::Plan(plan)) => {
                Some(plan.verdict())
            }
            _ => None,
        })
        .collect()
}

/// How each closed question was closed, in order.
fn question_records(kind: Kind, log: &Log) -> Vec<wire::AskClosed> {
    use wire::claude_item::Kind as Claude;
    use wire::codex_item::Kind as Codex;
    log.ordered()
        .into_iter()
        .filter_map(|item| match ItemBody::decode(kind, &item.body) {
            ItemBody::Claude(Claude::Ask(ask)) | ItemBody::Codex(Codex::Ask(ask)) => Some(ask),
            _ => None,
        })
        .filter(|ask| matches!(ask.ask, Some(wire::ask_item::Ask::Question(_))))
        .filter_map(|ask| ask.closed)
        .collect()
}
