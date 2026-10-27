//! `amux attach` as a person runs it: the real binary in a terminal of its
//! own, against a real daemon hosting agents on the fake providers.
//!
//! Terminal Claude attaches in files mode: the retained tail replays, keys
//! and a resize reach the one terminal, a second attacher sees the same
//! screen, and the leader's detach leaves the agent running. Codex attaches
//! in stream mode, and the fleet opened over it returns to the same view
//! process. Headless Claude has no terminal and is pointed at its chat.

#![cfg(unix)]

mod support;

use provider_fakes::Step;
use support::desk::{Desk, LONG_GRACE_SECS, say};
use support::term::Term;
use support::{chat, client, created_id, line_of, says, until};

/// The leader, Ctrl+A, and its two chords.
const DETACH: &[u8] = b"\x01d";
const FLEET: &[u8] = b"\x01s";

fn text(chunk: &str) -> Step {
    Step::Text {
        chunks: vec![chunk.to_owned()],
    }
}

fn desk(steps: Vec<Step>) -> Desk {
    Desk::new(
        false,
        LONG_GRACE_SECS,
        node::version(),
        "http://127.0.0.1:9/",
        steps,
    )
}

/// Starts `agent` (claude or codex) for `interface` (amux or native).
async fn create(desk: &Desk, agent: &str, interface: &str, name: &str) -> Vec<u8> {
    let work = desk.work.to_string_lossy().into_owned();
    let created = desk
        .run(&[
            "new", agent, "--in", interface, "--name", name, "--cwd", &work, "--prompt", "start",
        ])
        .await;
    created_id(&created)
}

#[tokio::test]
async fn terminal_claude_attaches_twice_takes_keys_and_a_resize_and_detaches() {
    let desk = desk(vec![
        text("hello from the agent"),
        Step::TurnEnd,
        text("second turn done"),
        Step::TurnEnd,
    ]);
    desk.run(&["server", "start"]).await;
    let scout = create(&desk, "claude", "native", "scout").await;
    let chats = client(&desk.socket).await;
    until("the first turn", || {
        let mut chats = chats.clone();
        let scout = &scout;
        async move { says(&chat(&mut chats, scout).await, "hello from the agent", 1) }
    })
    .await
    .unwrap();

    say("$ amux attach scout");
    let mut first = Term::attach(&desk, "scout", 30, 100);
    first.drawn("hello from the agent").await;
    first.resize(33, 111);
    first.drawn("size 33x111").await;

    say("$ amux attach scout  (a second terminal)");
    let mut second = Term::attach(&desk, "scout", 33, 111);
    second.drawn("hello from the agent").await;
    second.type_keys(b"second\r").await;
    for term in [&mut first, &mut second] {
        term.drawn("> second").await;
        term.drawn("second turn done").await;
    }

    say("<leader> d in the second terminal");
    second.type_keys(DETACH).await;
    let said = second.exits().await;
    assert!(said.contains("[detached from scout]"), "{said}");

    // The first is still attached, and the agent still running.
    first.resize(40, 120);
    first.drawn("size 40x120").await;
    let listing = desk.run(&["ls"]).await;
    assert!(!line_of(&listing, "scout").contains("exited"), "{listing}");
    first.type_keys(DETACH).await;
    let said = first.exits().await;
    assert!(said.contains("[detached from scout]"), "{said}");
}

#[tokio::test]
async fn codex_attaches_in_stream_mode_and_the_fleet_returns_to_the_same_view() {
    let desk = desk(vec![text("codex was here"), Step::TurnEnd]);
    desk.run(&["server", "start"]).await;
    let coder = create(&desk, "codex", "native", "coder").await;
    let chats = client(&desk.socket).await;
    until("the codex turn", || {
        let mut chats = chats.clone();
        let coder = &coder;
        async move { says(&chat(&mut chats, coder).await, "codex was here", 1) }
    })
    .await
    .unwrap();

    say("$ amux attach coder");
    let mut term = Term::attach(&desk, "coder", 30, 100);
    term.shows("fake codex resume").await;
    term.type_keys(b"one\r").await;
    term.shows("> one").await;
    // Half a line, held by the view process until Enter.
    term.type_keys(b"par").await;

    say("<leader> s: the fleet over the attached view");
    term.type_keys(FLEET).await;
    term.shows("a attach").await;
    assert!(term.contents().contains("coder"), "{}", term.contents());

    say("a: back to the same view");
    term.type_keys(b"a").await;
    term.shows("> one").await;
    term.type_keys(b"tial\r").await;
    // A new view would have started with an empty line.
    term.shows("> partial").await;

    term.type_keys(DETACH).await;
    let said = term.exits().await;
    assert!(said.contains("[detached from coder]"), "{said}");
}

/// Headless Claude has no interface of its own: asked for by name it is
/// refused with the way to its chat, and a plain `amux attach` opens that
/// chat, amux's, whatever the config prefers.
#[tokio::test]
async fn headless_claude_has_no_terminal_and_attach_opens_its_chat() {
    let desk = desk(vec![text("headless"), Step::TurnEnd]);
    desk.run(&["server", "start"]).await;
    create(&desk, "claude", "amux", "quiet").await;
    let refused = desk.amux(&["attach", "quiet", "--in", "native"]).await;
    let said = String::from_utf8_lossy(&refused.stderr);
    say(format!("$ amux attach quiet --in native\n  {said}"));
    assert!(!refused.status.success());
    assert!(said.contains("has no Claude Code interface"), "{said}");
    assert!(
        said.contains("`amux attach quiet --in amux` opens its chat"),
        "{said}"
    );

    say("$ amux attach quiet");
    let mut chat = Term::run(&desk, &desk.amux_path(), &["attach", "quiet"], 30, 100);
    chat.shows("headless").await;
}

/// Leaving a raw attach gives the shell its own screen back: the attach
/// draws the agent's terminal on the alternate screen and leaves it on the
/// way out, so the agent's screen is not left in the shell's scrollback.
/// QA: after a detach the agent's last frame stayed in the shell.
#[tokio::test]
async fn leaving_a_raw_attach_gives_the_shell_its_own_screen_back() {
    const ENTER: &str = "\x1b[?1049h";
    const LEAVE: &str = "\x1b[?1049l";
    let desk = desk(vec![text("hello from the agent"), Step::TurnEnd]);
    desk.run(&["server", "start"]).await;
    let scout = create(&desk, "claude", "native", "scout").await;
    let chats = client(&desk.socket).await;
    until("the first turn", || {
        let mut chats = chats.clone();
        let scout = &scout;
        async move { says(&chat(&mut chats, scout).await, "hello from the agent", 1) }
    })
    .await
    .unwrap();

    say("$ amux attach scout, then <leader> d");
    let mut term = Term::attach(&desk, "scout", 30, 100);
    term.drawn("hello from the agent").await;
    term.type_keys(DETACH).await;
    let said = term.exits().await;
    let entered = said.find(ENTER);
    let left = said.rfind(LEAVE);
    assert!(
        entered.is_some_and(|entered| left.is_some_and(|left| left > entered)),
        "the attach did not draw on the alternate screen and leave it on exit \
         (entered at {entered:?}, left at {left:?}): {said:?}"
    );
}
