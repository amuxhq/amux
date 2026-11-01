//! Asks answered in Claude's own terminal rather than through amux.
//!
//! amux answers terminal Claude's asks on the hooks that wait for them, but
//! Claude's dialog stays usable in its terminal while a hook waits, and a
//! person there may answer first. Each scenario attaches Claude's own
//! interface (`amux attach --in native`) in a real terminal before the
//! prompt, sends the prompt from amux, and answers the dialog that opens by
//! typing Claude's own keys into that terminal: 1 for Yes on a permission,
//! Escape to refuse one, a digit to pick a question's option. What the
//! interpreter infers from the call's rows must match what its own traffic
//! replays to.
//!
//! Each terminal's bytes are kept as in attach.rs, under
//! `target/live/claude-terminal/<scenario>/`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use wire::Phase;

use super::asks::{question_records, settled};
use super::attach::{DETACH, DRAW, Screen, capture_dir};
use super::{Follow, Install, Scenario, TURN, Verdict};

/// What Claude's permission dialog asks.
const PROCEED: &str = "Do you want to proceed?";

fn screen_dir(scenario: Scenario) -> Result<PathBuf, String> {
    let dir = capture_dir()
        .parent()
        .map(|live| live.join("claude-terminal").join(scenario.name()))
        .ok_or("the live capture folder has no parent")?;
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

/// Claude's own terminal, attached through amux.
struct Terminal {
    screen: Screen,
    clock: Instant,
}

impl Terminal {
    /// Reads the terminal until `done` holds of it and the session.
    async fn until(
        &mut self,
        follow: &Follow,
        what: &str,
        patience: Duration,
        done: impl Fn(&Screen, &super::Log) -> bool,
    ) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + patience;
        loop {
            self.screen.pump(self.clock).await?;
            if done(&self.screen, &follow.snapshot()) {
                return Ok(());
            }
            if tokio::time::Instant::now() > deadline {
                return Err(format!(
                    "timed out after {patience:?} waiting for {what}; Claude's terminal shows:\n{}",
                    self.screen.contents()
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Reads the terminal until it has drawn nothing new for a while.
    async fn still(&mut self) -> Result<(), String> {
        let mut last = String::new();
        let mut still = 0;
        let deadline = tokio::time::Instant::now() + DRAW;
        while still < 10 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
            self.screen.pump(self.clock).await?;
            let now = self.screen.contents();
            still = if now == last { still + 1 } else { 0 };
            last = now;
        }
        Ok(())
    }

    async fn keys(&mut self, keys: &[u8]) -> Result<(), String> {
        self.screen.type_keys(self.clock, keys).await
    }

    /// Leaves Claude's terminal for the shell, the agent untouched.
    async fn detach(mut self) -> Result<(), String> {
        self.keys(DETACH).await?;
        if !self.screen.exits(self.clock).await? {
            return Err("the attached terminal exited with a failure".into());
        }
        Ok(())
    }
}

impl Install {
    /// Claude's own terminal attached to the scenario's agent, drawn.
    async fn attach_terminal(
        &self,
        scenario: Scenario,
        follow: &Follow,
    ) -> Result<Terminal, String> {
        let dir = screen_dir(scenario)?;
        let project = self.project(scenario)?;
        let name = scenario.agent();
        let mut terminal = Terminal {
            screen: Screen::open(
                "claude",
                &dir,
                self,
                &["attach", &name, "--in", "native"],
                &project,
            )?,
            clock: Instant::now(),
        };
        terminal
            .until(follow, "Claude's terminal to draw", DRAW, |screen, _| {
                !screen.contents().trim().is_empty()
            })
            .await?;
        terminal.still().await?;
        Ok(terminal)
    }

    /// A terminal Claude agent with its own terminal attached, `prompt`
    /// sent from amux, and the dialog it leads to drawn: the terminal shows
    /// `drawn` while the ask is open in the session.
    async fn asked_at_the_terminal(
        &self,
        scenario: Scenario,
        prompt: &str,
        drawn: &str,
    ) -> Result<Result<(Follow, Terminal), Verdict>, String> {
        let (_, follow) = match self
            .claude_agent(scenario, super::CLAUDE_MODEL, &[], &[])
            .await?
        {
            Ok(ready) => ready,
            Err(verdict) => return Ok(Err(verdict)),
        };
        let mut terminal = self.attach_terminal(scenario, &follow).await?;
        self.send(scenario, prompt).await?;
        let kind = self.kind;
        terminal
            .until(
                &follow,
                "the dialog in Claude's terminal",
                TURN,
                |screen, log| !log.state(kind).asks.is_empty() && screen.shows(drawn),
            )
            .await?;
        terminal.still().await?;
        Ok(Ok((follow, terminal)))
    }

    /// A command asked about, allowed by typing 1 in Claude's own
    /// terminal: the command runs, the turn completes and the file exists.
    pub(super) async fn terminal_allow(&self) -> Result<Verdict, String> {
        let scenario = Scenario::TerminalAllow;
        let effect = self.project(scenario)?.join("allowed.txt");
        let (follow, mut terminal) = match self
            .asked_at_the_terminal(
                scenario,
                "Use the Bash tool to run exactly: printf allowed > allowed.txt. Then reply \
                 with the single word done.",
                PROCEED,
            )
            .await?
        {
            Ok(asked) => asked,
            Err(verdict) => return Ok(verdict),
        };
        terminal.keys(b"1").await?;
        let kind = self.kind;
        terminal
            .until(&follow, "the turn to end", TURN, |_, log| {
                settled(kind, log)
            })
            .await?;
        terminal.detach().await?;
        super::expect_outcome(
            follow.snapshot().turns(kind)[0],
            wire::TurnOutcome::Completed,
        )?;
        if !effect.exists() {
            return Err(format!(
                "allowed in Claude's terminal, but {} does not exist",
                effect.display()
            ));
        }
        self.judged(scenario, &follow)
    }

    /// A command asked about, refused with Escape in Claude's own terminal:
    /// the turn ends, nothing is left asked and the file is never made.
    pub(super) async fn terminal_escape(&self) -> Result<Verdict, String> {
        let scenario = Scenario::TerminalEscape;
        let effect = self.project(scenario)?.join("escaped.txt");
        let (follow, mut terminal) = match self
            .asked_at_the_terminal(
                scenario,
                "Use the Bash tool to run exactly: printf escaped > escaped.txt. Then reply \
                 with the single word done.",
                PROCEED,
            )
            .await?
        {
            Ok(asked) => asked,
            Err(verdict) => return Ok(verdict),
        };
        terminal.keys(b"\x1b").await?;
        let kind = self.kind;
        terminal
            .until(&follow, "the turn to end", TURN, |_, log| {
                settled(kind, log)
            })
            .await?;
        terminal.detach().await?;
        if effect.exists() {
            return Err(format!(
                "refused in Claude's terminal, but {} exists",
                effect.display()
            ));
        }
        self.judged(scenario, &follow)
    }

    /// A question whose second option is picked by typing 2 in Claude's
    /// own terminal, submitted there with Enter where Claude asks for it:
    /// the question reads answered with that option.
    pub(super) async fn terminal_question(&self) -> Result<Verdict, String> {
        let scenario = Scenario::TerminalQuestion;
        let (follow, mut terminal) = match self
            .asked_at_the_terminal(
                scenario,
                "Use AskUserQuestion to ask exactly one single-select question with header \
                 Color, question 'Which color do you prefer?', and options Red and Blue. Then \
                 repeat my answer.",
                "Blue",
            )
            .await?
        {
            Ok(asked) => asked,
            Err(verdict) => return Ok(verdict),
        };
        terminal.keys(b"2").await?;
        let kind = self.kind;
        // Claude may put the answers up for review before they go.
        terminal
            .until(
                &follow,
                "the answer to go or its review",
                DRAW,
                |screen, log| log.state(kind).asks.is_empty() || screen.shows("Submit"),
            )
            .await?;
        if !follow.snapshot().state(kind).asks.is_empty() {
            terminal.still().await?;
            terminal.keys(b"\r").await?;
        }
        terminal
            .until(&follow, "the turn to end", TURN, |_, log| {
                settled(kind, log)
            })
            .await?;
        terminal.detach().await?;
        let records = question_records(kind, &follow.snapshot());
        if !records.iter().any(|closed| {
            closed.outcome() == wire::AskOutcome::Answered
                && closed
                    .answers
                    .first()
                    .is_some_and(|answer| answer.picked == ["Blue"])
        }) {
            return Err(format!("no question reads answered Blue: {records:?}"));
        }
        if follow.snapshot().phase() != Phase::Idle {
            return Err("the agent did not take input again".into());
        }
        self.judged(scenario, &follow)
    }
}
