//! The agent process's lifecycle, run against the scripted fake providers
//! with real sockets, files and child processes, and a clock the test moves
//! by hand: every deadline fires exactly when the test says so.

mod support;

use std::time::Duration;

use agent::ExitCause;
use provider_fakes::{Ask, Outcome, Step, Tool, ToolClass};
use support::*;
use wire::{Kind, Phase};

fn text(chunk: &str) -> Step {
    Step::Text {
        chunks: vec![chunk.to_owned()],
    }
}

fn permission() -> Step {
    Step::Ask(Ask::Permission(Tool {
        name: None,
        class: ToolClass::Consequential,
        input: None,
        outcome: Outcome::default(),
        wait_for: None,
    }))
}

#[tokio::test(flavor = "multi_thread")]
async fn with_no_daemon_the_agent_runs_its_first_prompt_then_drains_after_the_grace() {
    let agent = Agent::start(Setup {
        initial_prompt: Some("go"),
        steps: vec![text("done"), Step::TurnEnd],
        ..Setup::sdk()
    })
    .await;
    agent
        .wait("the first turn ends", |log| log.turn_ends() == 1)
        .await;
    assert!(!agent.finished(), "a live agent waits out the grace");

    agent.clock.armed(T0 + GRACE).await;
    agent.clock.set(T0 + GRACE);
    assert_eq!(agent.exit().await, ExitCause::DaemonLost);

    let log = agent.log();
    assert_eq!(
        log.boundaries(),
        vec![
            "STARTED".to_owned(),
            "DAEMON_LOST".to_owned(),
            "EXITED DAEMON_LOST".to_owned()
        ]
    );
    agent.assert_released();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_snapshot_carries_the_folders_git_facts_from_start_and_after_each_turn() {
    let agent = Agent::start(Setup {
        repository: true,
        steps: vec![text("done"), Step::TurnEnd],
        ..Setup::sdk()
    })
    .await;
    let mut daemon = agent.dial().await;
    agent
        .wait("git facts read at start", |log| log.git().is_some())
        .await;
    let at_start = agent.log().git().unwrap();
    assert_eq!(at_start.branch.as_deref(), Some("main"));
    assert_eq!(at_start.base_branch.as_deref(), Some("main"));
    assert_eq!(
        at_start.uncommitted,
        Some(wire::ChangeTotals::default()),
        "nothing changed yet"
    );
    assert_eq!(
        at_start.on_branch, at_start.uncommitted,
        "on its base branch, only the uncommitted work"
    );

    // The person, or the agent, changes the folder; the turn end reads it.
    std::fs::write(agent.work().join("notes.txt"), "one\ntwo\nthree\n").unwrap();
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", "go").await, Verdict::Accepted);
    agent
        .wait("git facts read at the turn end", |log| {
            log.turn_ends() == 1
                && log.git().and_then(|git| git.uncommitted)
                    == Some(wire::ChangeTotals {
                        files: 1,
                        added: 3,
                        removed: 0,
                    })
        })
        .await;
    println!("after the turn: {:?}", agent.log().git());
    drop(daemon);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_dialling_back_in_cancels_the_grace() {
    let agent = Agent::start(Setup::sdk()).await;
    agent.ready().await;
    let daemon = agent.dial().await;
    drop(daemon);
    agent.clock.armed(T0 + GRACE).await;

    let mut daemon = agent.dial().await;
    agent
        .until("the grace is no longer armed", || {
            agent.clock.sleeping().is_empty()
        })
        .await;
    agent.clock.set(T0 + GRACE + 1);
    // A window: with the grace unarmed the clock wakes nothing, and an exit
    // that must not happen leaves no mark to wait on.
    holds_for(
        "the agent to live on",
        Duration::from_millis(200),
        || async { !agent.finished() },
    )
    .await
    .unwrap();
    // Headless Claude reports its start with the first prompt; there has
    // been none, so any boundary here would be the drain's.
    assert_eq!(agent.log().boundaries(), Vec::<String>::new());

    daemon.stop().await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);
}

/// A stop ends the agent now, even one waiting on the person: the person
/// stopping it has decided not to answer, so the turn is cancelled the
/// provider's own way, the ask closes and the agent exits stopped.
#[tokio::test(flavor = "multi_thread")]
async fn a_stop_cancels_the_turn_under_an_open_ask_and_exits_stopped() {
    let agent = Agent::start(Setup {
        steps: vec![permission(), text("never"), Step::TurnEnd],
        ..Setup::sdk()
    })
    .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", "clean up").await, Verdict::Accepted);
    agent
        .wait("the ask is open", |log| {
            log.phase() == Some(Phase::NeedsYou)
        })
        .await;

    daemon.stop().await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);
    let log = agent.log();
    assert_eq!(
        log.turn_ends(),
        1,
        "the provider ended the turn its own way"
    );
    assert_eq!(
        log.boundaries().last().map(String::as_str),
        Some("EXITED STOPPED")
    );
    assert_eq!(log.open_asks(), 0);
    assert!(!log.has_text("never"));
    agent.assert_released();
}

// Process groups and the tools that observe them are POSIX.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn kill_ends_the_process_group_at_once_and_writes_nothing() {
    let marker = format!("kill-{}", uuid::Uuid::new_v4().simple());
    let agent = Agent::start(Setup {
        provider_args: vec!["--model".into(), marker.clone()],
        steps: vec![
            text("busy"),
            Step::WaitFor {
                path: agent_release(),
            },
            Step::TurnEnd,
        ],
        ..Setup::sdk()
    })
    .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", "work").await, Verdict::Accepted);
    agent
        .wait("the turn is under way", |log| log.has_text("busy"))
        .await;
    assert!(running(&marker), "the provider is running");

    daemon.kill().await;
    assert_eq!(agent.exit().await, ExitCause::Killed);
    agent
        .until("the provider is gone", || !running(&marker))
        .await;
    assert_eq!(agent.log().boundaries(), vec!["STARTED".to_owned()]);
    agent.assert_released();
}

// A directory the agent may not write stands in for a full disk: both fail
// the same write, and permissions are what a test can set.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_journal_write_that_fails_ends_the_incarnation_and_the_next_writes_its_boundary() {
    use std::os::unix::fs::PermissionsExt as _;

    // One frame per segment: every append after the first creates a file.
    let agent = Agent::start(Setup {
        initial_prompt: Some("go"),
        journal_bytes: 1,
        steps: vec![
            text("recorded"),
            Step::WaitFor {
                path: agent_release(),
            },
            text("never recorded"),
            Step::TurnEnd,
        ],
        ..Setup::sdk()
    })
    .await;
    agent
        .wait("the turn is under way", |log| log.has_text("recorded"))
        .await;
    let journal = agent.dir.join(agent::JOURNAL);
    let end = agent.journal_end();
    std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o500)).unwrap();

    agent.release();
    let cause = agent.exit().await;
    std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        matches!(cause, ExitCause::WriteFailed(_)),
        "the incarnation ends on the failed write, not with the turn: {cause:?}"
    );
    agent.assert_released();
    assert_eq!(
        agent.journal_end(),
        end,
        "the journal ends at its last whole frame"
    );
    assert!(!agent.log().has_text("never recorded"));

    agent.resume();
    agent
        .wait("the failed incarnation's final boundary", |log| {
            log.boundaries()
                .iter()
                .any(|boundary| boundary == "EXITED ENDED_UNEXPECTEDLY")
        })
        .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    daemon.kill().await;
    assert_eq!(agent.exit().await, ExitCause::Killed);
}

// The transcript position is what a resumed session reads on from; an
// incarnation that cannot keep it ends the same way as one whose journal
// write failed. A read-only cursor file stands in for the full disk. Unix
// only: terminal Claude, the provider whose transcript is followed, is not
// hosted on Windows.
#[cfg(unix)]
#[test]
fn a_transcript_position_that_cannot_be_kept_ends_the_incarnation() {
    use std::os::unix::fs::PermissionsExt as _;

    terminal_test(async {
        let agent = Agent::start(Setup {
            kind: Kind::ClaudePty,
            steps: vec![
                text("recorded"),
                Step::WaitFor {
                    path: agent_release(),
                },
                text("after the cursor froze"),
                Step::TurnEnd,
            ],
            ..Setup::sdk()
        })
        .await;
        let mut daemon = agent.dial().await;
        agent.ready().await;
        assert_eq!(daemon.prompt(b"p1", "go").await, Verdict::Accepted);
        agent
            .wait("the turn is under way", |log| log.has_text("recorded"))
            .await;
        // The position is kept after every transcript row, once the row
        // has been read; the row's text reaches the journal first.
        let cursor = agent.dir.join(agent::PRIVATE).join("transcript-cursor");
        agent
            .until("the position to be kept after a row", || cursor.is_file())
            .await;
        std::fs::set_permissions(&cursor, std::fs::Permissions::from_mode(0o400)).unwrap();

        agent.release();
        let cause = agent.exit().await;
        std::fs::set_permissions(&cursor, std::fs::Permissions::from_mode(0o600)).unwrap();
        match cause {
            ExitCause::WriteFailed(why) => assert!(
                why.contains("transcript-cursor"),
                "the cause names the file that could not be written: {why}"
            ),
            other => panic!("the incarnation ends on the failed write: {other:?}"),
        }
        agent.assert_released();
    });
}

// Unix only: the straggler is started by a POSIX shell, and Windows does not host terminal
// Claude (ConPTY re-renders its output; see docs/ARCHITECTURE.md, "Windows, as a stated cost").
#[cfg(unix)]
#[test]
fn the_agent_exits_on_its_child_exit_while_a_straggler_holds_the_terminal() {
    let straggler = format!("sleep 29.{}", std::process::id());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let outcome = runtime.block_on(async {
        let agent = Agent::start(Setup {
            kind: Kind::ClaudePty,
            command: Some("/bin/sh".into()),
            provider_args: vec!["-c".into(), format!("{straggler} & printf started; exit 3")],
            ..Setup::sdk()
        })
        .await;
        let cause = agent.exit().await;
        (cause, agent)
    });
    // The terminal reader is still blocked on the straggler; leave it.
    runtime.shutdown_background();
    let (cause, agent) = outcome;
    let _ = std::process::Command::new("pkill")
        .args(["-f", &straggler])
        .status();
    assert_eq!(cause, ExitCause::ProviderExited(Some(3)));
    assert!(agent.terminal_bytes().contains("started"));
    assert_eq!(
        agent.log().boundaries().last().map(String::as_str),
        Some("EXITED PROVIDER_EXITED: exit code 3")
    );
    agent.assert_released();
}

/// A person's stop is always stop now: the running turn is cancelled the
/// provider's own way and the agent exits as stopped within seconds, even
/// in the middle of a command, and nothing of that command outlives it.
/// Claude's and Codex's tool shells each lead a session of their own, so a
/// kill of the provider's process group misses them: QA saw a stopped
/// headless Claude's command run on for its whole 90 seconds, writing
/// files, and the stop itself take 30 seconds to report a kill.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_stop_mid_command_ends_stopped_at_once_and_leaves_no_command_running() {
    let marker = format!("600.{}", uuid::Uuid::new_v4().as_u128() % 1_000_000_000);
    let agent = Agent::start(Setup {
        steps: vec![
            Step::Spawn {
                command: format!("exec sleep {marker}"),
            },
            Step::Tool(Tool {
                name: None,
                class: ToolClass::Consequential,
                input: None,
                outcome: Outcome::default(),
                wait_for: Some(agent_release()),
            }),
            text("never"),
            Step::TurnEnd,
        ],
        ..Setup::sdk()
    })
    .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", "build it").await, Verdict::Accepted);
    agent
        .until("the command is running", || {
            running(&format!("sleep {marker}"))
        })
        .await;

    daemon.stop().await;
    let cause = agent.exit().await;
    let left = running(&format!("sleep {marker}"));
    let _ = std::process::Command::new("pkill")
        .args(["-f", &format!("sleep {marker}")])
        .status();
    assert!(
        !left,
        "the stopped agent's command is still running: a session leader of its own escapes the provider's process group"
    );
    assert_eq!(
        (cause, agent.log().boundaries().last().cloned()),
        (ExitCause::Stopped, Some("EXITED STOPPED".to_owned())),
        "a person's stop of a working agent ends it as stopped"
    );
    assert!(!agent.log().has_text("never"));
}

/// A one-shot terminal Claude types its queued follow-up as its turn ends,
/// and until Claude records that prompt the agent reads idle with nothing
/// queued. The prompt is still in flight: the child runs it before it
/// exits, rather than ending finished with the prompt lost.
#[cfg(unix)]
#[test]
fn a_one_shot_terminal_child_runs_the_follow_up_it_typed_as_its_turn_ended() {
    terminal_test(async {
        let agent = Agent::start(Setup {
            kind: Kind::ClaudePty,
            parent: true,
            initial_prompt: Some("first"),
            steps: vec![
                text("one"),
                Step::WaitFor {
                    path: agent_release(),
                },
                Step::TurnEnd,
                text("two"),
                Step::TurnEnd,
            ],
            ..Setup::sdk()
        })
        .await;
        let mut daemon = agent.dial().await;
        agent
            .wait("the first turn is under way", |log| log.has_text("one"))
            .await;
        assert_eq!(daemon.prompt(b"p2", "second").await, Verdict::Queued);

        agent.release();
        assert_eq!(agent.exit().await, ExitCause::Finished);
        let log = agent.log();
        assert!(log.has_text("two"), "the follow-up ran before the exit");
        assert_eq!(log.turn_ends(), 2);
        agent.assert_released();
    });
}
