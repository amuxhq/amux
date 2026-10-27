//! Raw attach on pty.sock, against the fake providers: terminal Claude's
//! one terminal read from its files by two clients at once, and Codex's
//! views, one `codex resume` per connection on the agent's one thread.
//! Unix only: Windows hosts neither, terminal Claude by choice (see
//! docs/ARCHITECTURE.md, "Windows, as a stated cost") and Codex's own app
//! because it attaches only on macOS and Linux.
#![cfg(unix)]

mod support;

use std::time::Duration;

use agent::ExitCause;
use agent::attach::Attached;
use support::*;
use wire::{Kind, PtyMode};

const PATIENCE: Duration = Duration::from_secs(30);

/// A client and everything its terminal has drawn so far.
struct Screen {
    attached: Attached,
    drawn: String,
}

impl Screen {
    async fn attach(agent: &Agent) -> Self {
        let attached = Attached::connect(&agent.dir)
            .await
            .expect("pty.sock takes a client");
        Self {
            attached,
            drawn: String::new(),
        }
    }

    /// Reads until the terminal has drawn `text`.
    async fn until(&mut self, text: &str) {
        let waited = tokio::time::timeout(PATIENCE, async {
            while !self.drawn.contains(text) {
                match self.attached.next().await.expect("the terminal reads") {
                    Some(bytes) => self.drawn.push_str(&String::from_utf8_lossy(&bytes)),
                    None => panic!(
                        "the connection ended ({:?}) before {text:?}; drawn:\n{}",
                        self.attached.closed(),
                        self.drawn
                    ),
                }
            }
        })
        .await;
        assert!(
            waited.is_ok(),
            "timed out waiting for {text:?}; drawn:\n{}",
            self.drawn
        );
    }

    /// Reads until the agent ends the connection.
    async fn until_closed(&mut self) {
        tokio::time::timeout(PATIENCE, async {
            while let Some(bytes) = self.attached.next().await.expect("the terminal reads") {
                self.drawn.push_str(&String::from_utf8_lossy(&bytes));
            }
        })
        .await
        .expect("the connection ends");
    }
}

#[test]
fn two_clients_read_terminal_claude_from_its_files_and_type_into_it() {
    terminal_test(async {
        let agent = Agent::start(Setup {
            kind: Kind::ClaudePty,
            steps: vec![
                provider_fakes::Step::Text {
                    chunks: vec!["answered at the terminal".into()],
                },
                provider_fakes::Step::TurnEnd,
            ],
            ..Setup::sdk()
        })
        .await;
        let mut daemon = agent.dial().await;
        agent.ready().await;

        let mut first = Screen::attach(&agent).await;
        assert_eq!(first.attached.mode(), PtyMode::Files);
        first.until("Claude Code (scripted)").await;
        let mut second = Screen::attach(&agent).await;
        second.until("Claude Code (scripted)").await;

        first
            .attached
            .keys(b"typed at the terminal\r")
            .await
            .unwrap();
        first.until("answered at the terminal").await;
        second.until("answered at the terminal").await;
        agent
            .wait("the typed prompt's turn ends", |log| {
                log.turn_ends() == 1 && log.has_text("typed at the terminal")
            })
            .await;

        second.attached.resize(40, 120).await.unwrap();
        first.until("size 40x120").await;
        second.until("size 40x120").await;

        drop(first);
        daemon.stop().await;
        assert_eq!(agent.exit().await, ExitCause::Stopped);
        second.until_closed().await;
    });
}

/// `/clear` typed at terminal Claude starts a new session: the next
/// incarnation resumes that one, not the conversation the clear left.
#[test]
fn a_clear_typed_at_the_terminal_is_the_session_the_next_incarnation_resumes() {
    terminal_test(async {
        let agent = Agent::start(Setup {
            kind: Kind::ClaudePty,
            steps: vec![
                provider_fakes::Step::Text {
                    chunks: vec!["before the clear".into()],
                },
                provider_fakes::Step::TurnEnd,
            ],
            ..Setup::sdk()
        })
        .await;
        let mut daemon = agent.dial().await;
        agent.ready().await;
        let mut screen = Screen::attach(&agent).await;
        screen.until("Claude Code (scripted)").await;
        screen.attached.keys(b"say hello\r").await.unwrap();
        agent
            .wait("the first turn ends", |log| log.turn_ends() == 1)
            .await;
        let before = agent.provider_session();

        screen.attached.keys(b"/clear\r").await.unwrap();
        agent
            .wait("the clear", |log| {
                log.boundaries().iter().any(|kind| kind.contains("CLEARED"))
            })
            .await;
        let after = agent.provider_session();
        assert_ne!(after, before, "the clear's session is the one kept");
        drop(screen);
        daemon.kill().await;
        assert_eq!(agent.exit().await, ExitCause::Killed);

        agent.resume();
        let mut daemon = agent.dial().await;
        agent.ready().await;
        assert_eq!(agent.provider_session(), after, "resumed, not replaced");
        daemon.stop().await;
        assert_eq!(agent.exit().await, ExitCause::Stopped);
    });
}

#[test]
fn each_codex_client_gets_its_own_view_on_the_agents_thread() {
    terminal_test(async {
        let agent = Agent::start(Setup {
            kind: Kind::Codex,
            ..Setup::sdk()
        })
        .await;
        let mut daemon = agent.dial().await;
        agent.ready().await;
        let thread = agent.provider_session();
        let view = format!("resume {thread}");

        let mut first = Screen::attach(&agent).await;
        assert_eq!(first.attached.mode(), PtyMode::Stream);
        first.until(&format!("fake codex resume {thread}")).await;
        let mut second = Screen::attach(&agent).await;
        second.until(&format!("fake codex resume {thread}")).await;

        first.attached.keys(b"from the first\r").await.unwrap();
        first.until("> from the first").await;
        second.attached.keys(b"from the second\r").await.unwrap();
        second.until("> from the second").await;
        // The thread's items reach both views ("user: from the first"); the
        // line typed into one is that view's own.
        assert!(
            !second.drawn.contains("> from the first"),
            "two views are two processes:\n{}",
            second.drawn
        );

        first.attached.resize(30, 100).await.unwrap();
        first.until("size 30x100").await;

        // Quitting a view ends its connection and nothing else.
        first.attached.keys(b"\x03").await.unwrap();
        first.until_closed().await;
        assert_eq!(
            first.attached.closed(),
            Some("codex resume exited with code 0")
        );
        second.attached.keys(b"still here\r").await.unwrap();
        second.until("> still here").await;

        // Leaving ends the view with the connection.
        drop(second);
        agent
            .until("the second view's process is gone", || !running(&view))
            .await;
        assert!(!agent.finished(), "the agent outlives its views");

        daemon.stop().await;
        assert_eq!(agent.exit().await, ExitCause::Stopped);
    });
}
