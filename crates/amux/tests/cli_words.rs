//! What the CLI says when the daemon refuses: a person reads plain words,
//! never a refusal's wire name, and a failure the far side caused is never
//! blamed on this host's own daemon.

#![cfg(unix)]

use std::process::Stdio;

use provider_fakes::Step;
use support::desk::{Desk, LONG_GRACE_SECS, say};
use support::term::Term;
use support::{created_id, line_of, until};
use tokio::io::AsyncWriteExt as _;

use crate::support;

fn desk(steps: Vec<Step>) -> Desk {
    Desk::new(
        false,
        LONG_GRACE_SECS,
        node::version(),
        "http://127.0.0.1:9/",
        steps,
    )
}

/// `amux send` to an agent that has exited says so in a sentence, not the
/// refusal's name. QA: the CLI printed "worker did not take it: exited".
#[tokio::test(flavor = "multi_thread")]
async fn a_send_to_an_exited_agent_is_refused_in_plain_words() {
    let desk = desk(vec![
        Step::Text {
            chunks: vec!["done".to_owned()],
        },
        Step::TurnEnd,
    ]);
    desk.run(&["server", "start"]).await;
    let work = desk.work.to_string_lossy().into_owned();
    let created = desk
        .run(&[
            "new", "claude", "--in", "amux", "--name", "worker", "--cwd", &work, "--prompt",
            "start",
        ])
        .await;
    created_id(&created);
    desk.run(&["stop", "worker"]).await;
    until("worker to exit", || {
        let desk = &desk;
        async move {
            let listing = desk.run(&["ls"]).await;
            line_of(&listing, "worker")
                .contains("exited")
                .then_some(())
                .ok_or(listing)
        }
    })
    .await
    .unwrap();

    let refused = desk.amux(&["send", "worker", "one more thing"]).await;
    let said = String::from_utf8_lossy(&refused.stderr);
    say(format!("$ amux send worker one more thing\n  {said}"));
    assert!(
        !refused.status.success(),
        "the send to an exited agent failed"
    );
    let reason = said.trim_end().rsplit(": ").next().unwrap_or_default();
    assert!(
        reason.contains(' '),
        "the refusal ends in a bare refusal name, not a sentence: {said}"
    );
}

/// `amux pair <address>` to an address where no amux listens says the
/// other host could not be reached; this host's daemon answered and is
/// still there. QA: pairing to a wrong address said "the amux daemon went
/// away".
#[tokio::test(flavor = "multi_thread")]
async fn pairing_to_an_address_nobody_answers_does_not_blame_this_hosts_daemon() {
    let desk = desk(Vec::new());
    desk.run(&["server", "start"]).await;
    let mut pair = desk.command(&["pair", "127.0.0.1:9"]);
    pair.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = pair.spawn().expect("amux pair starts");
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"123456\n").await.unwrap();
    drop(stdin);
    let output = tokio::time::timeout(support::PATIENCE, child.wait_with_output())
        .await
        .expect("amux pair to a dead address ends")
        .unwrap();
    let said = String::from_utf8_lossy(&output.stderr);
    say(format!("$ amux pair 127.0.0.1:9\n  {said}"));
    assert!(!output.status.success(), "pairing with nobody failed");
    assert!(
        !said.contains("daemon went away"),
        "the far address's silence is blamed on this host's daemon: {said}"
    );
    assert!(
        desk.daemon_running(),
        "this host's daemon is still running after the failed pairing"
    );
}

/// The first command a person runs in a terminal that would have usage
/// counts sent prints the notice once, on standard error, and records it
/// shown: in the installation's file with no daemon running, through the
/// daemon with one. A command whose output a program captures, as the Mac
/// app's installer captures `amux server start`, neither prints nor records
/// it: nobody read it.
#[tokio::test(flavor = "multi_thread")]
async fn the_first_command_prints_the_analytics_notice_once() {
    const SENDS: [(&str, &str); 1] = [("AMUX_ANALYTICS_URL", "http://127.0.0.1:9/")];
    async fn in_a_terminal(desk: &Desk, args: &[&str]) -> String {
        Term::run_with(
            desk,
            &desk.amux_path(),
            args,
            24,
            200,
            &SENDS,
            &["DO_NOT_TRACK"],
        )
        .exits()
        .await
    }
    async fn captured(desk: &Desk, args: &[&str]) -> String {
        let mut command = desk.command(args);
        command
            .env(SENDS[0].0, SENDS[0].1)
            .env_remove("DO_NOT_TRACK");
        let output = tokio::time::timeout(support::PATIENCE, command.output())
            .await
            .expect("amux finishes")
            .expect("amux runs");
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    let alone = desk(Vec::new());
    let unread = captured(&alone, &["config", "get", "telemetry"]).await;
    assert!(!unread.contains(analytics::NOTICE), "{unread}");
    assert!(
        !node::notice_shown_in(&alone.data),
        "output a program captured is no notice shown"
    );
    let first = in_a_terminal(&alone, &["config", "get", "telemetry"]).await;
    say(format!("$ amux config get telemetry\n  {first}"));
    assert!(first.contains(analytics::NOTICE), "{first}");
    assert!(first.contains(analytics::NOTICE_URL), "{first}");
    assert!(node::notice_shown_in(&alone.data));
    let again = in_a_terminal(&alone, &["config", "get", "telemetry"]).await;
    assert!(!again.contains(analytics::NOTICE), "{again}");

    let served = desk(Vec::new());
    served.run(&["server", "start"]).await;
    assert!(
        !node::notice_shown_in(&served.data),
        "a build that sends nothing says nothing"
    );
    let first = in_a_terminal(&served, &["ls"]).await;
    assert!(first.contains(analytics::NOTICE), "{first}");
    assert!(node::notice_shown_in(&served.data), "the daemon wrote it");
    let again = in_a_terminal(&served, &["ls"]).await;
    assert!(!again.contains(analytics::NOTICE), "{again}");
}
