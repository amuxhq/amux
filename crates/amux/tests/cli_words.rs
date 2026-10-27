//! What the CLI says when the daemon refuses: a person reads plain words,
//! never a refusal's wire name, and a failure the far side caused is never
//! blamed on this host's own daemon.

#![cfg(unix)]

mod support;

use std::process::Stdio;

use provider_fakes::Step;
use support::desk::{Desk, LONG_GRACE_SECS, say};
use support::{created_id, line_of, until};
use tokio::io::AsyncWriteExt as _;

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
