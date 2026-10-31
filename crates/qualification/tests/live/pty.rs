//! What only terminal Claude does, recorded for its corpus as corpus.rs
//! records what both Claude kinds do: its permission-mode cycle, a prompt
//! long enough that Claude would collapse a paste of it, a provider that
//! cannot be reached, and a permission allowed with a scope or denied with a
//! note. Each is judged against its own traffic replayed through the
//! interpreter, and by its effect where it has one.

use wire::claude_item::Kind as Claude;
use wire::{AgentInput, agent_input, answer};

use super::asks::{Answer, settled};
use super::corpus::{claude_items, tools};
use super::{Install, Scenario, TURN, Verdict};

/// Far past the length Claude collapses a paste at, over several lines: it
/// must reach the model as the person's own words.
fn long_prompt() -> String {
    let padding = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima \
                   mike november oscar papa quebec romeo sierra tango uniform victor whiskey \
                   xray yankee zulu. ";
    format!(
        "Reply with exactly LONG_OK and nothing else. Padding follows.\n{}\nThat is the end \
         of the padding.",
        padding.repeat(16).trim_end()
    )
}

impl Install {
    /// The permission mode cycled from amux, then a turn: the session runs
    /// in the mode the cycle moved to.
    pub(super) async fn mode_cycle(&self) -> Result<Verdict, String> {
        let scenario = Scenario::ModeCycle;
        let (agent, follow) = match self
            .claude_agent(scenario, super::CLAUDE_MODEL, &[], &[])
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.input(
            &agent,
            AgentInput {
                of: Some(agent_input::Of::CyclePermissionMode(
                    wire::CyclePermissionMode {},
                )),
            },
        )
        .await?;
        self.turn(
            scenario,
            &follow,
            "Reply with the single word pong and nothing else.",
            1,
        )
        .await?;
        let permission = follow.snapshot().state(self.kind).permission;
        if permission.as_deref().is_none_or(|value| value == "default") {
            return Err(format!("the cycled permission reads {permission:?}"));
        }
        self.judged(scenario, &follow)
    }

    /// A prompt of many lines, far past the length Claude collapses a paste
    /// at: its row reads exactly what was sent.
    pub(super) async fn long_prompt(&self) -> Result<Verdict, String> {
        let scenario = Scenario::LongPrompt;
        let (_, follow) = match self
            .claude_agent(scenario, super::CLAUDE_MODEL, &[], &[])
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        let text = long_prompt();
        self.turn(scenario, &follow, &text, 1).await?;
        let prompts: Vec<String> = follow
            .snapshot()
            .ordered()
            .into_iter()
            .filter(|item| super::item_token(self.kind, item).as_deref() == Some("prompt"))
            .map(|item| item.text.clone())
            .collect();
        if prompts != [text] {
            return Err(format!("the prompt rows read {prompts:?}"));
        }
        self.judged(scenario, &follow)
    }

    /// A provider that cannot be reached, set through the agent's
    /// `--settings`: nothing listens on the discard port, so every request
    /// fails to connect, two retries keep it short, and the turn ends on an
    /// error row.
    pub(super) async fn api_error(&self) -> Result<Verdict, String> {
        let scenario = Scenario::ApiError;
        let settings = serde_json::json!({
            "env": {
                "ANTHROPIC_BASE_URL": "http://127.0.0.1:9",
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
            .until(TURN, "the agent to take input again", |log| {
                settled(kind, log)
            })
            .await?;
        if !claude_items(&follow.snapshot())
            .iter()
            .any(|item| matches!(item, Claude::ApiError(_)))
        {
            return Err("the turn ended with no error row".into());
        }
        self.judged(scenario, &follow)
    }

    /// A command asked about and answered from amux by `answer`, given the
    /// ask: the call ends decided as `decided`.
    async fn permission_answered(
        &self,
        scenario: Scenario,
        command: &str,
        answer: impl Fn(&wire::PermissionAsk) -> Result<wire::permission_answer::Of, String>,
        decided: wire::DecisionOutcome,
    ) -> Result<Verdict, String> {
        let (agent, follow) = match self
            .claude_agent(scenario, super::CLAUDE_MODEL, &[], &[])
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(verdict),
        };
        self.send(
            scenario,
            &format!("Use the Bash tool to run exactly: {command}. Then stop."),
        )
        .await?;
        let kind = self.kind;
        let mut asked = 0;
        self.drive(
            &agent,
            &follow,
            "the command to be decided and the turn to end",
            |ask| match ask {
                ui_state::OpenAsk::Claude(claude) => match &claude.body {
                    Some(wire::ask::Body::Permission(permission)) => {
                        asked += 1;
                        Ok(Answer::Given(answer::Of::Permission(
                            wire::PermissionAnswer {
                                of: Some(answer(permission)?),
                            },
                        )))
                    }
                    _ => self.by_the_way(ask),
                },
                _ => self.by_the_way(ask),
            },
            |log| settled(kind, log),
        )
        .await?;
        if asked == 0 {
            return Err("the command was not asked about".into());
        }
        let outcomes: Vec<_> = tools(&follow.snapshot())
            .iter()
            .filter_map(|call| call.decision.as_ref().map(|decision| decision.outcome()))
            .collect();
        if !outcomes.contains(&decided) {
            return Err(format!(
                "no call reads {}: {outcomes:?}",
                decided.as_str_name()
            ));
        }
        self.judged(scenario, &follow)
    }

    /// A command allowed with the first scope Claude offered: the call
    /// runs and the file exists.
    pub(super) async fn permission_scoped(&self) -> Result<Verdict, String> {
        let scenario = Scenario::PermissionScoped;
        let effect = self.project(scenario)?.join("allow-scoped.txt");
        let verdict = self
            .permission_answered(
                scenario,
                "printf allow-scoped > allow-scoped.txt; printf allow-scoped",
                |ask| {
                    if ask.scopes.is_empty() {
                        return Err("the ask offered no scope".into());
                    }
                    Ok(wire::permission_answer::Of::Allow(wire::PermissionAllow {
                        scope: Some(0),
                    }))
                },
                wire::DecisionOutcome::Allowed,
            )
            .await?;
        if matches!(verdict, Verdict::Pass) && !effect.exists() {
            return Err(format!("allowed, but {} does not exist", effect.display()));
        }
        Ok(verdict)
    }

    /// A command denied with a note: the call reads denied, the note
    /// reaches Claude and the file is never made.
    pub(super) async fn permission_denied(&self) -> Result<Verdict, String> {
        let scenario = Scenario::PermissionDenied;
        let effect = self.project(scenario)?.join("denied.txt");
        let verdict = self
            .permission_answered(
                scenario,
                "printf denied > denied.txt",
                |_| {
                    Ok(wire::permission_answer::Of::Deny(wire::PermissionDeny {
                        note: "Use a read-only command instead".into(),
                        stop: false,
                    }))
                },
                wire::DecisionOutcome::Denied,
            )
            .await?;
        if effect.exists() {
            return Err(format!("denied, but {} exists", effect.display()));
        }
        Ok(verdict)
    }
}
