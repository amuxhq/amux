//! `amux attach` on a Codex agent runs Codex's own app as one more client
//! of the agent's app server, on its live thread: the real binary in a
//! terminal of its own, against a real daemon hosting the agent on the fake
//! Codex, whose view joins with `--remote` as Codex's app does.
//!
//! The agent has not run a turn when the app attaches. A prompt typed in
//! the app shows in amux's chat, and the approval it leads to is answered
//! from amux; then an approval is answered in the app, and amux's ask
//! closes.

#![cfg(unix)]

use provider_fakes::{Ask, Step, Tool, ToolClass};
use support::desk::{Desk, LONG_GRACE_SECS, say};
use support::term::Term;
use support::{chat, client, says, until};
use wire::{
    CodexInput, Decision, Input, Phase, ResolveAgentRequest, SendInputRequest, codex_input,
    send_input_response,
};

use crate::support;

const DETACH: &[u8] = b"\x01d";

/// A turn that asks before running a command, then says `said`.
fn asking_turn(said: &str) -> Vec<Step> {
    vec![
        Step::Ask(Ask::Permission(Tool {
            name: None,
            class: ToolClass::Consequential,
            input: None,
            outcome: Default::default(),
            wait_for: None,
        })),
        Step::Text {
            chunks: vec![said.to_owned()],
        },
        Step::TurnEnd,
    ]
}

/// The request id of the approval the app draws last, as `approval <id>:`.
fn drawn_approval(term: &Term) -> Option<String> {
    let contents = term.contents();
    let line = contents
        .lines()
        .rev()
        .find(|line| line.starts_with("approval "))?;
    Some(line["approval ".len()..].split(':').next()?.to_owned())
}

#[tokio::test]
async fn codex_app_attaches_to_a_fresh_agent_and_codrives_it_with_amux() {
    let mut steps = asking_turn("ran it for the app");
    steps.extend(asking_turn("ran it again"));
    let desk = Desk::new(
        false,
        LONG_GRACE_SECS,
        node::version(),
        "http://127.0.0.1:9/",
        steps,
    );
    desk.run(&["server", "start"]).await;
    let work = desk.work.to_string_lossy().into_owned();
    let created = desk
        .run(&["new", "codex", "--name", "coder", "--cwd", &work])
        .await;
    let coder = support::created_id(&created);
    let mut chats = client(&desk.socket).await;
    let phase = |chats: &mut wire::ClientStubs| {
        let mut chats = chats.clone();
        async move {
            chats
                .agents
                .resolve_agent(ResolveAgentRequest {
                    name: "coder".into(),
                })
                .await
                .map(|agent| agent.into_inner().phase)
                .map_err(|error| error.to_string())
        }
    };
    until("the agent to wait for its first prompt", || {
        let phase = phase(&mut chats);
        async move {
            let phase = phase.await?;
            (phase == Phase::Idle as i32)
                .then_some(())
                .ok_or(format!("phase {phase}"))
        }
    })
    .await
    .unwrap();

    say("$ amux attach coder   (no turn has run)");
    let mut app = Term::attach(&desk, "coder", 30, 100);
    app.shows("fake codex resume").await;

    say("the app: a prompt");
    app.type_keys(b"hello from the app\r").await;
    until("the app's prompt in amux's chat", || {
        let mut chats = chats.clone();
        let coder = &coder;
        async move { says(&chat(&mut chats, coder).await, "hello from the app", 1) }
    })
    .await
    .unwrap();

    say("amux: the approval the prompt led to, answered from amux");
    app.until("the app to draw the approval", |term| {
        drawn_approval(term).is_some()
    })
    .await;
    let request = drawn_approval(&app).unwrap();
    until("amux to need the person", || {
        let phase = phase(&mut chats);
        async move {
            let phase = phase.await?;
            (phase == Phase::NeedsYou as i32)
                .then_some(())
                .ok_or(format!("phase {phase}"))
        }
    })
    .await
    .unwrap();
    let answered = chats
        .sessions
        .send_input(SendInputRequest {
            agent_id: coder.clone(),
            input: Some(Input::new(
                b"approve-1".to_vec(),
                CodexInput {
                    of: Some(codex_input::Of::Answer(wire::CodexAnswer {
                        ask_key: request.clone(),
                        of: Some(wire::codex_answer::Of::Approve(Decision::Approve as i32)),
                    })),
                },
            )),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(
        matches!(answered.of, Some(send_input_response::Of::Accepted(_))),
        "{answered:?}"
    );
    app.shows(&format!("resolved {request}")).await;
    until("the approved command's turn to finish", || {
        let mut chats = chats.clone();
        let coder = &coder;
        async move { says(&chat(&mut chats, coder).await, "ran it for the app", 1) }
    })
    .await
    .unwrap();

    say("the app: another prompt, its approval answered in the app");
    app.type_keys(b"once more\r").await;
    until("amux to be asked again", || {
        let phase = phase(&mut chats);
        async move {
            let phase = phase.await?;
            (phase == Phase::NeedsYou as i32)
                .then_some(())
                .ok_or(format!("phase {phase}"))
        }
    })
    .await
    .unwrap();
    app.until("the app to draw the second approval", |term| {
        drawn_approval(term).is_some_and(|id| id != request)
    })
    .await;
    let second = drawn_approval(&app).unwrap();
    app.type_keys(b"y\r").await;
    app.shows(&format!("resolved {second}")).await;
    until("amux's ask to close and the turn to finish", || {
        let mut chats = chats.clone();
        let coder = &coder;
        let phase = phase(&mut chats);
        async move {
            says(&chat(&mut chats, coder).await, "ran it again", 1)?;
            let phase = phase.await?;
            (phase == Phase::Idle as i32)
                .then_some(())
                .ok_or(format!("phase {phase}"))
        }
    })
    .await
    .unwrap();

    app.type_keys(DETACH).await;
    let said = app.exits().await;
    assert!(said.contains("[detached from coder]"), "{said}");
    let listing = desk.run(&["ls"]).await;
    assert!(
        !support::line_of(&listing, "coder").contains("exited"),
        "{listing}"
    );
}
