//! The terminal client over sessions built from authored records and the
//! interpreter's recorded fixtures: layout from the anchor and paging,
//! runs keyed by item keys, every ask body, the composer's gates and
//! controls, Detached and Reset, the hosts overlay and the fleet.

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use prost::Message as _;
use ui_state::{AgentKey, FleetMsg, FleetState, InputOutcome, Msg, SessionState};
use ui_view::{AskBody, CardState, ask_card, queue_rows};
use wire::{Item, Kind, Phase, SessionEvent, ToolClass, ToolState, session_event};

use crate::chat::layout::{Anchor, PAGE};
use crate::chat::{ChatEffect, ChatView};
use crate::clipboard::ClipboardContent;
use crate::fixtures::{self, caught_up, draw, text};
use crate::fleet::{FleetEffect, FleetView};
use crate::theme::Theme;

const W: u16 = 100;
const H: u16 = 30;

mod narrow;
mod round2;
mod text_layout;

fn theme() -> Theme {
    Theme::default()
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn event(of: session_event::Of) -> Msg {
    Msg::Event(SessionEvent { of: Some(of) })
}

fn typed(view: &mut ChatView, state: &SessionState, words: &str) {
    for c in words.chars() {
        view.key(state, key(KeyCode::Char(c)), theme());
    }
}

/// A Claude SDK item: a reply, or an exploration Read of `path`.
fn item(order: u64, read: Option<&str>) -> Item {
    use wire::claude_sdk_item::Kind as K;
    let body = match read {
        Some(path) => wire::ClaudeSdkItem {
            kind: Some(K::Tool(wire::ToolCall {
                name: "Read".into(),
                state: ToolState::Succeeded as i32,
                class: ToolClass::Read as i32,
                input_json: format!(r#"{{"file_path":"{path}"}}"#).into_bytes(),
                ..Default::default()
            })),
        },
        None => wire::ClaudeSdkItem {
            kind: Some(K::Message(wire::Text { complete: true })),
        },
    };
    Item {
        key: format!("k{order}"),
        order,
        revision: order,
        text: if read.is_none() {
            format!("reply number {order}")
        } else {
            String::new()
        },
        kind: Kind::ClaudeSdk as i32,
        body: body.encode_to_vec(),
        at_ms: order as i64 * 1_000,
        ..Item::default()
    }
}

fn snapshot(phase: Phase, body: Vec<u8>, queue: Vec<wire::QueuedInput>) -> Msg {
    event(session_event::Of::Snapshot(wire::Snapshot {
        kind: Kind::ClaudeSdk as i32,
        body,
        phase: phase as i32,
        queue,
        ..wire::Snapshot::default()
    }))
}

/// A live Claude SDK chat holding `items`, caught up.
fn chat(items: Vec<Item>) -> SessionState {
    let mut state = SessionState::new(
        fixtures::agent(Kind::ClaudeSdk),
        crate::chat::layout::CAP as usize,
    );
    state.update(Msg::Connection(ui_state::Connection::Live));
    state.update(snapshot(Phase::Idle, vec![], vec![]));
    for item in items {
        state.update(event(session_event::Of::Item(item)));
    }
    state.update(caught_up(0));
    state
}

/// Tells the session what the view says about following, as the app does
/// after every key and frame.
fn tell(view: &mut ChatView, state: &mut SessionState) -> Option<bool> {
    let moved = view.following_moved();
    if let Some(following) = moved {
        state.update(Msg::Following(following));
    }
    moved
}

/// Replies at orders `from..=to`: a window with older history when `from`
/// is above one.
fn replies(from: u64, to: u64) -> Vec<Item> {
    (from..=to).map(|order| item(order, None)).collect()
}

fn feed(view: &mut ChatView, state: &SessionState) -> (String, Option<u32>) {
    let (buffer, page) = draw(view, state, 0, W, H, theme());
    (text(&buffer), page)
}

// --- layout from the anchor ------------------------------------------------

#[test]
fn the_feed_follows_the_newest_row() {
    let state = chat(replies(1, 60));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (screen, page) = feed(&mut view, &state);
    assert!(screen.contains("reply number 60"), "{screen}");
    assert!(!screen.contains("reply number 1\n"), "{screen}");
    assert_eq!(page, None, "order one is held: nothing older to fetch");
}

#[test]
fn fewer_than_a_page_of_held_rows_above_the_screen_asks_for_an_older_page() {
    // Orders 51..=70 held, older history exists.
    let state = chat(replies(51, 70));
    assert!(state.transcript().has_older());
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (_, page) = feed(&mut view, &state);
    assert_eq!(page, Some(PAGE));
    // Once asked, it waits for the window to grow before asking again.
    view.page_sent(&state);
    let (_, page) = feed(&mut view, &state);
    assert_eq!(page, None);

    // A page's worth above the screen: no fetch.
    let state = chat(replies(51, 200));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (_, page) = feed(&mut view, &state);
    assert_eq!(page, None);
}

#[test]
fn an_open_collapsed_run_at_the_top_asks_for_a_larger_page() {
    // A run of 300 reads from the oldest held row, then two replies: the
    // reads paged in while the reader was in history, where the window
    // keeps every row it pages.
    let mut state = chat(replies(401, 402));
    state.update(Msg::Following(false));
    let epoch = state.epoch();
    state.update(Msg::Page {
        items: (101..=400)
            .map(|order| item(order, Some("src/lib.rs")))
            .collect(),
        exhausted: false,
        epoch,
    });
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (screen, page) = feed(&mut view, &state);
    assert!(screen.contains("300+ reads"), "{screen}");
    assert_eq!(page, Some(300), "the run's count, under the cap");
}

#[test]
fn a_scrolled_reader_keeps_its_place_as_rows_arrive() {
    let mut state = chat(replies(1, 60));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    feed(&mut view, &state);
    view.key(&state, key(KeyCode::PageUp), theme());
    assert!(matches!(view.anchor, Anchor::Top { .. }));
    assert_eq!(tell(&mut view, &mut state), Some(false));
    let (before, _) = feed(&mut view, &state);
    assert!(before.contains("↓ Jump to Bottom"), "{before}");
    for order in 61..=65 {
        state.update(event(session_event::Of::Item(item(order, None))));
    }
    assert_eq!(
        state.transcript().head(),
        Some(60),
        "arrivals are held apart"
    );
    let (after, _) = feed(&mut view, &state);
    let top = |screen: &str| {
        screen
            .lines()
            .skip(2)
            .take(10)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(top(&before), top(&after));
    assert!(after.contains("↓ 5 new"), "{after}");
    view.key(
        &state,
        KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL),
        theme(),
    );
    assert_eq!(view.anchor, Anchor::Bottom);
    assert_eq!(tell(&mut view, &mut state), Some(true));
    let (screen, _) = feed(&mut view, &state);
    assert!(screen.contains("reply number 65"), "{screen}");
}

#[test]
fn the_view_tells_the_session_once_when_the_reader_leaves_and_once_when_it_returns() {
    let mut state = chat(replies(1, 60));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    feed(&mut view, &state);
    assert_eq!(tell(&mut view, &mut state), None, "a chat opens following");
    view.key(&state, key(KeyCode::PageUp), theme());
    assert_eq!(tell(&mut view, &mut state), Some(false));
    view.key(&state, key(KeyCode::PageUp), theme());
    assert_eq!(tell(&mut view, &mut state), None);
    assert!(!state.following());
    // Sending a prompt is a return to the newest row.
    typed(&mut view, &state, "go on");
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    assert!(matches!(effects.as_slice(), [ChatEffect::Prompt { .. }]));
    assert_eq!(tell(&mut view, &mut state), Some(true));
    assert!(state.following());
}

#[test]
fn a_following_chat_under_live_rows_issues_no_page() {
    // A long chat opened on its 40-row tail pages ahead once, then holds.
    let mut state = chat(replies(961, 1000));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (_, page) = feed(&mut view, &state);
    assert_eq!(page, Some(PAGE));
    view.page_sent(&state);
    let epoch = state.epoch();
    state.update(Msg::Page {
        items: replies(921, 960),
        exhausted: false,
        epoch,
    });
    // Replies and runs of reads flood in; the window trims at its cap and
    // the view never asks for a page it would trim again.
    for order in 1001..=1600 {
        // Long runs collapse to a row each, so a full window can draw fewer
        // rows than a page above the screen.
        let read = order % 160 >= 10;
        let row = item(order, read.then_some("src/lib.rs"));
        state.update(event(session_event::Of::Item(row)));
        let (_, page) = feed(&mut view, &state);
        assert_eq!(page, None, "a page asked while following at {order}");
        assert!(state.transcript().len() <= crate::chat::layout::CAP as usize);
    }
    assert_eq!(state.transcript().head(), Some(1600));
}

#[test]
fn scrolling_down_past_the_newest_row_follows_again() {
    let state = chat(replies(1, 60));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    feed(&mut view, &state);
    view.key(&state, key(KeyCode::PageUp), theme());
    feed(&mut view, &state);
    for _ in 0..3 {
        view.key(&state, key(KeyCode::PageDown), theme());
        feed(&mut view, &state);
    }
    assert_eq!(view.anchor, Anchor::Bottom);
}

// --- permissions and modes ------------------------------------------------

/// A fixture's last frame where the agent's catalogue is held and its
/// snapshot names `permission`, with a fresh view drawn over it.
fn offering(kind: Kind, name: &str, pred: impl Fn(&SessionState) -> bool) -> SessionState {
    fixtures::frame_where(kind, name, |state| {
        !state.agent_state().permissions.is_empty() && pred(state)
    })
    .0
}

/// The same chat with its Codex snapshot replaced: the settings given, the
/// catalogue kept.
fn codex_settings(mut state: SessionState, settings: wire::CodexSnapshot) -> SessionState {
    let agent = state.agent_state();
    state.update(event(session_event::Of::Snapshot(wire::Snapshot {
        kind: Kind::Codex as i32,
        body: wire::CodexSnapshot {
            model: agent.model.clone(),
            model_name: agent.model_name.clone(),
            ..settings
        }
        .encode_to_vec(),
        phase: Phase::Idle as i32,
        revision: agent.revision + 1,
        catalogue: agent.catalogue.clone(),
        ..wire::Snapshot::default()
    })));
    state
}

/// The settings input a key sent, if it sent one.
fn sent(effects: &[ChatEffect]) -> Option<wire::KindInput> {
    match effects {
        [ChatEffect::Answer(input)] => input.decode_body(),
        _ => None,
    }
}

/// Headless Claude has no modes: Shift+Tab moves it to the next settable
/// permission that still asks before acting, the composer names the key
/// and the edge names a permission other than its normal one.
#[test]
fn shift_tab_moves_claude_to_the_next_permission_that_still_asks() {
    let state = offering(Kind::ClaudeSdk, "recorded_controls", |state| {
        state.agent_state().permission.as_deref() == Some("acceptEdits")
    });
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (screen, _) = feed(&mut view, &state);
    assert!(screen.contains("shift+tab permission"), "{screen}");
    assert!(screen.contains(" accept edits ─╯"), "{screen}");
    let effects = view.key(&state, key(KeyCode::BackTab), theme());
    let Some(wire::KindInput::ClaudeSdk(wire::ClaudeSdkInput {
        of: Some(wire::claude_sdk_input::Of::Permission(permission)),
    })) = sent(&effects)
    else {
        panic!("{effects:?}");
    };
    assert_eq!(permission.value, "plan", "the one after accept edits");
}

/// Codex offers modes, so Shift+Tab moves through them; its permission
/// changes from the settings key, Ctrl+S then p.
#[test]
fn codex_moves_mode_with_shift_tab_and_permission_from_the_settings_key() {
    let state = offering(Kind::Codex, "offered", |_| true);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (screen, _) = feed(&mut view, &state);
    assert!(screen.contains("shift+tab mode"), "{screen}");
    let effects = view.key(&state, key(KeyCode::BackTab), theme());
    let Some(wire::KindInput::Codex(wire::CodexInput {
        of: Some(wire::codex_input::Of::Mode(mode)),
    })) = sent(&effects)
    else {
        panic!("{effects:?}");
    };
    assert_eq!(
        mode.value, "plan",
        "from the normal mode, unsaid until Codex says"
    );

    view.key(&state, ctrl('s'), theme());
    let (screen, _) = feed(&mut view, &state);
    assert!(screen.contains("p permission"), "{screen}");
    view.key(&state, key(KeyCode::Char('p')), theme());
    let (screen, _) = feed(&mut view, &state);
    for words in [
        "read only",
        "default",
        "auto",
        "full access",
        "acts without asking",
    ] {
        assert!(screen.contains(words), "{words}: {screen}");
    }
    view.key(&state, key(KeyCode::Up), theme());
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    let Some(wire::KindInput::Codex(wire::CodexInput {
        of: Some(wire::codex_input::Of::Permission(permission)),
    })) = sent(&effects)
    else {
        panic!("{effects:?}");
    };
    assert_eq!(permission.value, "read-only");

    let planning = codex_settings(
        state.clone(),
        wire::CodexSnapshot {
            permission: Some("full-access".into()),
            mode: Some("plan".into()),
            approval_policy: Some("never".into()),
            sandbox: Some("danger-full-access".into()),
            ..Default::default()
        },
    );
    let (screen, _) = feed(&mut ChatView::new(b"agent".to_vec(), 0, false), &planning);
    assert!(screen.contains("· full access · plan"), "{screen}");
}

/// Codex settings that match no named permission read custom, and Shift+Tab
/// still moves the mode.
#[test]
fn codex_settings_outside_every_permission_read_custom() {
    let state = codex_settings(
        offering(Kind::Codex, "offered", |_| true),
        wire::CodexSnapshot {
            approval_policy: Some("untrusted".into()),
            sandbox: Some("workspace-write".into()),
            ..Default::default()
        },
    );
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (screen, _) = feed(&mut view, &state);
    assert!(screen.contains("· custom"), "{screen}");
    assert!(screen.contains("shift+tab mode"), "{screen}");
}

/// Terminal Claude's permissions are reached only by its own cycle key:
/// Shift+Tab presses it, and Ctrl+S then p says so.
#[test]
fn terminal_claude_keeps_its_cycle_key() {
    let state = offering(Kind::ClaudePty, "recorded_mode_cycle", |state| {
        state.agent_state().permission.is_some()
    });
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (screen, _) = feed(&mut view, &state);
    assert!(screen.contains("shift+tab permission"), "{screen}");
    let effects = view.key(&state, key(KeyCode::BackTab), theme());
    let Some(wire::KindInput::ClaudePty(wire::ClaudePtyInput {
        of: Some(wire::claude_pty_input::Of::CyclePermissionMode(_)),
    })) = sent(&effects)
    else {
        panic!("{effects:?}");
    };
    view.key(&state, ctrl('s'), theme());
    let effects = view.key(&state, key(KeyCode::Char('p')), theme());
    assert!(
        matches!(effects.as_slice(), [ChatEffect::Notice(words)] if words.contains("cycling")),
        "{effects:?}"
    );
}

// --- asks ------------------------------------------------------------------

fn body_name(body: &AskBody) -> &'static str {
    match body {
        AskBody::Command { .. } => "command",
        AskBody::Edit { .. } => "edit",
        AskBody::Tool { .. } => "tool",
        AskBody::Question(_) => "question",
        AskBody::Plan { .. } => "plan",
        AskBody::Form { .. } => "form",
        AskBody::Link { .. } => "link",
        AskBody::Access { .. } => "access",
        AskBody::Unanswerable { .. } => "unanswerable",
    }
}

/// A Claude SDK chat asking something this build cannot read.
fn unanswerable() -> SessionState {
    let body = wire::ClaudeSdkSnapshot {
        asks: vec![wire::Ask {
            key: "menu-1".into(),
            body: Some(wire::ask::Body::Unanswerable(wire::UnanswerableAsk {
                reason: String::new(),
            })),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut state = chat(replies(1, 3));
    state.update(snapshot(Phase::NeedsYou, body.encode_to_vec(), vec![]));
    state
}

/// Every kind of ask takes the composer's box, and the keys under it say
/// how to stop the turn when one is running (not under a Codex plan,
/// proposed as its turn ended).
#[test]
fn every_ask_body_draws_in_the_box_with_the_way_to_stop() {
    let mut seen = HashSet::new();
    let mut check = |state: &SessionState, at: i64, label: &str| {
        let Some(card) = ask_card(state) else {
            return;
        };
        if card.state != CardState::Open {
            return;
        }
        seen.insert(body_name(&card.body));
        let mut view = ChatView::new(b"agent".to_vec(), at, false);
        let (buffer, _) = draw(&mut view, state, at, 120, 60, theme());
        let screen = text(&buffer);
        assert!(screen.contains("╭"), "{label}: {screen}");
        assert_eq!(
            screen.contains("ctrl+x interrupt"),
            card.stops_turn,
            "{label}: {screen}"
        );
    };
    for (kind, dir) in [
        (Kind::ClaudePty, "claude_pty"),
        (Kind::ClaudeSdk, "claude_sdk"),
        (Kind::Codex, "codex"),
    ] {
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../interpret/fixtures")
            .join(dir);
        let mut names: Vec<String> = std::fs::read_dir(&fixtures)
            .unwrap()
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                (path.extension()? == "json")
                    .then(|| path.file_stem().unwrap().to_string_lossy().into_owned())
            })
            .collect();
        names.sort();
        for name in names {
            if name.starts_with("inject_") {
                continue;
            }
            for (label, state, at) in fixtures::frames(kind, &name) {
                check(&state, at, &format!("{dir}/{name} {label}"));
            }
        }
    }
    check(&unanswerable(), 0, "authored unanswerable");
    let mut seen: Vec<_> = seen.into_iter().collect();
    seen.sort_unstable();
    assert_eq!(
        seen,
        [
            "access",
            "command",
            "edit",
            "form",
            "link",
            "plan",
            "question",
            "tool",
            "unanswerable"
        ]
    );
}

#[test]
fn ctrl_x_stops_the_turn_while_an_ask_waits() {
    let (state, at) = fixtures::Named::CodexApproval.state();
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    draw(&mut view, &state, at, W, H, theme());
    assert_eq!(
        view.key(&state, ctrl('x'), theme()),
        vec![ChatEffect::Interrupt]
    );
}

#[test]
fn the_likely_choice_answers_in_the_kinds_own_arm() {
    let (state, at) = fixtures::Named::CodexApproval.state();
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    match effects.as_slice() {
        [ChatEffect::Answer(input)] => {
            assert!(
                matches!(input.decode_body(), Some(wire::KindInput::Codex(_))),
                "{input:?}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_unanswerable_ask_offers_the_terminal_and_stop() {
    let state = unanswerable();
    let mut view = ChatView::new(b"agent".to_vec(), 0, true);
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(screen.contains("Can't answer this here"), "{screen}");
    assert!(screen.contains("Open Claude's terminal"), "{screen}");
    assert!(screen.contains("ctrl+x interrupt"), "{screen}");
    assert_eq!(
        view.key(&state, key(KeyCode::Enter), theme()),
        vec![ChatEffect::RawAttach]
    );
    assert_eq!(
        view.key(&state, ctrl('x'), theme()),
        vec![ChatEffect::Interrupt]
    );
}

#[test]
fn a_denial_takes_a_note_that_goes_back_to_the_agent() {
    let (state, at) = fixtures::Named::ClaudePermissionAsk.state();
    let card = ask_card(&state).unwrap();
    assert!(
        card.choices.iter().any(|choice| choice.takes_note),
        "Claude's deny takes a note"
    );
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    // Esc reaches the way out; Tab opens its note.
    assert!(view.key(&state, key(KeyCode::Esc), theme()).is_empty());
    assert!(view.key(&state, key(KeyCode::Tab), theme()).is_empty());
    typed(&mut view, &state, "use cargo clean");
    let (buffer, _) = draw(&mut view, &state, at, W, H, theme());
    assert!(
        text(&buffer).contains("No: use cargo clean"),
        "{}",
        text(&buffer)
    );
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    let [ChatEffect::Answer(input)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    let Some(wire::KindInput::ClaudeSdk(wire::ClaudeSdkInput {
        of: Some(wire::claude_sdk_input::Of::Answer(answer)),
    })) = input.decode_body()
    else {
        panic!("{input:?}");
    };
    let decoded = answer;
    let Some(wire::claude_answer::Of::Permission(wire::PermissionAnswer {
        of: Some(wire::permission_answer::Of::Deny(deny)),
    })) = decoded.of
    else {
        panic!("{decoded:?}");
    };
    assert_eq!(deny.note, "use cargo clean");
}

/// docs/TERMINAL.md: when the agent waits on you "the ask takes over the
/// composer's box ... the draft waits behind it and comes back after".
#[test]
fn a_draft_typed_before_an_ask_waits_behind_it_and_comes_back_after() {
    let idle = chat(vec![]);
    let (asking, at) = fixtures::Named::ClaudePermissionAsk.state();
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    typed(&mut view, &idle, "I have ");
    // The ask opens; its first choice is answered by number.
    let effects = view.key(&asking, key(KeyCode::Char('1')), theme());
    assert!(
        matches!(effects.as_slice(), [ChatEffect::Answer(_)]),
        "{effects:?}"
    );
    assert_eq!(
        view.editor.text(),
        "I have ",
        "the draft waited behind the ask"
    );
}

#[test]
fn questions_answer_through_steps_and_a_review() {
    let (state, at) = fixtures::frame_where(
        Kind::ClaudeSdk,
        "recorded_question_every_shape",
        |state| matches!(ask_card(state).map(|card| card.body), Some(AskBody::Question(questions)) if questions.len() > 1),
    );
    let Some(AskBody::Question(questions)) = ask_card(&state).map(|card| card.body) else {
        unreachable!()
    };
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    let mut sent = None;
    for question in &questions {
        if question.multi_select {
            view.key(&state, key(KeyCode::Char(' ')), theme());
        }
        let effects = view.key(&state, key(KeyCode::Enter), theme());
        assert!(effects.is_empty(), "{effects:?}");
    }
    let (buffer, _) = draw(&mut view, &state, at, 120, 60, theme());
    assert!(text(&buffer).contains("Send answers"), "{}", text(&buffer));
    for effect in view.key(&state, key(KeyCode::Enter), theme()) {
        if let ChatEffect::Answer(input) = effect {
            sent = Some(input);
        }
    }
    let input = sent.expect("the review sends the answers");
    let Some(wire::KindInput::ClaudeSdk(wire::ClaudeSdkInput {
        of: Some(wire::claude_sdk_input::Of::Answer(answer)),
    })) = input.decode_body()
    else {
        panic!("{input:?}");
    };
    let decoded = answer;
    let Some(wire::claude_answer::Of::Question(answers)) = decoded.of else {
        panic!("{decoded:?}");
    };
    assert_eq!(answers.answers.len(), questions.len());
}

#[test]
fn a_form_submits_what_was_typed() {
    let (state, at) =
        fixtures::frame_where(Kind::ClaudeSdk, "recorded_elicitation_accepted", |state| {
            matches!(
                ask_card(state).map(|card| card.body),
                Some(AskBody::Form { .. })
            )
        });
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    // The one field takes the typing; Enter submits it.
    typed(&mut view, &state, "jlw/amux");
    let (buffer, _) = draw(&mut view, &state, at, 120, 60, theme());
    assert!(text(&buffer).contains("› jlw/amux"), "{}", text(&buffer));
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    let [ChatEffect::Answer(input)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    let Some(wire::KindInput::ClaudeSdk(wire::ClaudeSdkInput {
        of: Some(wire::claude_sdk_input::Of::Answer(answer)),
    })) = input.decode_body()
    else {
        panic!("{input:?}");
    };
    let decoded = answer;
    let Some(wire::claude_answer::Of::Form(form)) = decoded.of else {
        panic!("{decoded:?}");
    };
    let content: serde_json::Value = serde_json::from_slice(&form.content_json).unwrap();
    assert!(content.to_string().contains("jlw/amux"), "{content}");
}

#[test]
fn an_unconfirmed_answer_offers_resend_and_discard() {
    let (mut state, at) = fixtures::Named::CodexApproval.state();
    let card = ask_card(&state).unwrap();
    let mut input = ui_view::answer_input(&card, &card.choices[0].answer, "").unwrap();
    input.input_id = b"answer-1".to_vec();
    state.update(Msg::Send(input));
    state.update(Msg::Sent(b"answer-1".to_vec(), InputOutcome::Lost));
    assert_eq!(ask_card(&state).unwrap().state, CardState::NotConfirmed);
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    let (buffer, _) = draw(&mut view, &state, at, W, H, theme());
    assert!(text(&buffer).contains("r resend · d discard"));
    assert_eq!(
        view.key(&state, key(KeyCode::Char('r')), theme()),
        vec![ChatEffect::Resend {
            id: b"answer-1".to_vec()
        }]
    );
    assert_eq!(
        view.key(&state, key(KeyCode::Char('d')), theme()),
        vec![ChatEffect::Discard {
            id: b"answer-1".to_vec()
        }]
    );
}

/// A held answer says why it is held and when it sends, whole, at 80
/// columns: the box wraps the sentence rather than cutting it short.
#[test]
fn a_held_answer_says_when_it_sends_whole_at_80_columns() {
    let (mut state, at) = fixtures::Named::CodexApproval.state();
    let card = ask_card(&state).unwrap();
    let mut input = ui_view::answer_input(&card, &card.choices[0].answer, "").unwrap();
    input.input_id = b"answer-1".to_vec();
    state.update(Msg::Send(input));
    state.update(Msg::Sent(b"answer-1".to_vec(), InputOutcome::Held));
    assert_eq!(ask_card(&state).unwrap().state, CardState::Held);
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    let (buffer, _) = draw(&mut view, &state, at, 80, 24, theme());
    let screen = text(&buffer);
    let words: String = screen
        .lines()
        .map(|line| line.trim_matches(|c: char| c == ' ' || c == '│'))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        !screen.contains('…') && words.contains("It sends when the host is back."),
        "the held answer's explanation is cut short:\n{screen}"
    );
    assert!(screen.contains("c cancel"), "{screen}");
}

// --- the composer ----------------------------------------------------------

/// A draft is taken at any time, and Enter always hands it on: while the
/// chat is not yet current the client library holds it, pending, and sends
/// it once the chat catches up.
#[test]
fn a_draft_is_taken_any_time_and_held_until_the_chat_is_current() {
    let mut state = SessionState::new(
        fixtures::agent(Kind::ClaudeSdk),
        crate::chat::layout::CAP as usize,
    );
    state.update(Msg::Connection(ui_state::Connection::Live));
    state.update(snapshot(Phase::Idle, vec![], vec![]));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    typed(&mut view, &state, "hello");
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    assert!(text(&buffer).contains("catching up"), "{}", text(&buffer));
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    assert_eq!(
        effects,
        vec![ChatEffect::Prompt {
            text: "hello".into(),
            attachments: vec![]
        }]
    );
    assert!(view.editor.is_empty());
    // The library holds it, pending, while the chat catches up.
    let held = wire::Input::new(
        b"held-1".to_vec(),
        wire::KindInput::ClaudeSdk(wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
                text: "hello".into(),
                attachments: vec![],
            })),
        }),
    );
    state.update(Msg::Send(held));
    assert_eq!(
        state.input_state(b"held-1"),
        Some(ui_state::InputState::Held)
    );
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    assert!(
        text(&buffer).contains("sends when"),
        "the held prompt shows pending: {}",
        text(&buffer)
    );
    let outcome = state.update(caught_up(0));
    assert_eq!(outcome.resend.len(), 1, "caught up, it goes");
}

#[test]
fn an_exited_entry_offers_resume_with_the_draft() {
    let mut state = chat(replies(1, 3));
    let mut exited = fixtures::agent(Kind::ClaudeSdk);
    exited.lifecycle = wire::Lifecycle::Exited as i32;
    exited.exit_cause = wire::ExitCause::Finished as i32;
    state.update(Msg::Entry(exited));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(screen.contains("Enter resumes"), "{screen}");
    // A one-shot agent that ended after its turn finished.
    assert!(screen.contains("finished"), "{screen}");
    assert!(!screen.contains("exited · finished"), "{screen}");
    typed(&mut view, &state, "carry on");
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    assert!(text(&buffer).contains("enter resume"));
    assert_eq!(
        view.key(&state, key(KeyCode::Enter), theme()),
        vec![ChatEffect::Resume {
            text: "carry on".into(),
            attachments: vec![]
        }]
    );
}

fn prompt(id: &[u8], words: &str) -> wire::Input {
    let mut input = ui_runtime::inputs::prompt(Kind::ClaudeSdk, words, vec![]).unwrap();
    input.input_id = id.to_vec();
    input
}

#[test]
fn an_unconfirmed_prompt_offers_resend_and_discard() {
    let mut state = chat(replies(1, 3));
    state.update(Msg::Send(prompt(b"p1", "did this land")));
    state.update(Msg::Sent(b"p1".to_vec(), InputOutcome::Lost));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(screen.contains("did this land"), "{screen}");
    assert!(screen.contains("may not have arrived"), "{screen}");
    view.key(&state, key(KeyCode::Up), theme());
    assert_eq!(view.tray, Some(0));
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(screen.contains("[Resend] [Discard]"), "{screen}");
    assert!(
        screen.contains("enter resend   backspace discard"),
        "{screen}"
    );
    assert_eq!(
        view.key(&state, key(KeyCode::Enter), theme()),
        vec![ChatEffect::Resend { id: b"p1".to_vec() }]
    );
    view.key(&state, key(KeyCode::Up), theme());
    assert_eq!(
        view.key(&state, key(KeyCode::Backspace), theme()),
        vec![ChatEffect::Discard { id: b"p1".to_vec() }]
    );
}

#[test]
fn a_queued_prompt_can_be_withdrawn_or_sent_now() {
    let queued = wire::QueuedInput {
        input_id: b"q1".to_vec(),
        text: "and then the docs".into(),
        ..Default::default()
    };
    let mut state = chat(replies(1, 3));
    state.update(snapshot(Phase::Working, vec![], vec![queued]));
    assert_eq!(queue_rows(&state).len(), 1);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(
        screen.contains("and then the docs") && screen.contains("queued"),
        "{screen}"
    );
    view.key(&state, key(KeyCode::Up), theme());
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(screen.contains("[Send Now] [Withdraw]"), "{screen}");
    assert!(
        screen.contains("enter send now   backspace withdraw"),
        "{screen}"
    );
    assert_eq!(
        view.key(&state, key(KeyCode::Enter), theme()),
        vec![ChatEffect::SendNow { id: b"q1".to_vec() }]
    );
    view.tray = Some(0);
    assert_eq!(
        view.key(&state, key(KeyCode::Backspace), theme()),
        vec![ChatEffect::Withdraw {
            id: b"q1".to_vec(),
            text: "and then the docs".into(),
            attachments: vec![]
        }]
    );
}

#[test]
fn a_steered_prompt_reads_sending_into_this_turn_until_its_reflection() {
    let queued = wire::QueuedInput {
        input_id: b"q1".to_vec(),
        text: "use the other file".into(),
        steer: true,
        ..Default::default()
    };
    let mut state = chat(replies(1, 3));
    state.update(snapshot(Phase::Working, vec![], vec![queued]));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(
        screen.contains("use the other file") && screen.contains("sending into this turn"),
        "{screen}"
    );
}

#[test]
fn ctrl_v_attaches_a_file_through_put_blob() {
    let state = chat(replies(1, 3));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    assert_eq!(
        view.key(&state, ctrl('v'), theme()),
        vec![ChatEffect::Paste]
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shot.png");
    std::fs::write(&path, b"png bytes").unwrap();
    let effect = view.paste(ClipboardContent::Path(path)).unwrap();
    assert_eq!(
        effect,
        Some(ChatEffect::Attach {
            name: "shot.png".into(),
            mime: "image/png".into(),
            bytes: b"png bytes".to_vec()
        })
    );
    typed(&mut view, &state, "see ");
    view.attach_blob(
        &state,
        wire::BlobRef {
            hash: vec![7; 32],
            name: "shot.png".into(),
            mime: "image/png".into(),
            size: 9,
        },
    );
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    assert!(
        text(&buffer).contains("see [shot.png · 9 B]"),
        "{}",
        text(&buffer)
    );
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    let [ChatEffect::Prompt { text, attachments }] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert_eq!(text, &format!("see {}", attachments::PLACEHOLDER));
    assert!(matches!(
        attachments[0].of,
        Some(wire::attachment::Of::Image(_))
    ));
}

// --- Detached and Reset ----------------------------------------------------

/// A chat whose host is away keeps its rows and says the host is away and
/// the chat not current, and keeps what is being typed.
#[test]
fn detached_keeps_the_rows_and_the_draft() {
    let mut state = chat(replies(1, 5));
    state.update(Msg::Host(wire::HostEntry {
        host_id: b"host".to_vec(),
        name: "studio".into(),
        ..Default::default()
    }));
    state.update(event(session_event::Of::Detached(wire::Detached {})));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    typed(&mut view, &state, "still typing");
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(screen.contains("studio away · not current"), "{screen}");
    assert!(screen.contains("studio is away"), "{screen}");
    assert!(screen.contains("reply number 5"), "{screen}");
    // What Enter does here is decided elsewhere: the prompt is held to send
    // when the host is back (round2::a_prompt_sent_while_the_host_is_away_…).
    assert_eq!(view.editor.text(), "still typing");
}

/// Signed out, this machine names itself as the reason a host is out of reach:
/// the header, the placeholder, the keys under the composer and both rows
/// of the hosts overlay, never a claim about the host.
#[test]
fn a_host_out_of_reach_while_this_machine_is_signed_out_names_this_machines_sign_out() {
    let mut fleet = FleetState::new();
    let mut laptop = host(
        b"laptop",
        "laptop",
        wire::Trust::Trusted,
        wire::Presence::Online,
    );
    if let wire::inventory_event::Of::Host(entry) = &mut laptop {
        entry.via = wire::RouteKind::Unspecified as i32;
        entry.account_signed_in = Some(false);
    }
    inventory(&mut fleet, laptop);
    let mut desk = host(
        b"host",
        "desk",
        wire::Trust::Trusted,
        wire::Presence::Offline,
    );
    if let wire::inventory_event::Of::Host(entry) = &mut desk {
        entry.account_signed_in = Some(true);
    }
    inventory(&mut fleet, desk);

    let mut state = chat(replies(1, 5));
    state.update(Msg::Host(fleet.host(b"host").unwrap().clone()));
    state.update(event(session_event::Of::Detached(wire::Detached {})));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.reach = ui_view::reach(&fleet, b"laptop", b"host");
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(
        screen.contains("desk away · this machine is signed out"),
        "{screen}"
    );
    assert!(
        screen.contains("desk is away · this machine is signed out"),
        "{screen}"
    );
    typed(&mut view, &state, "still there?");
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(
        screen.contains("it sends once this machine signs in"),
        "{screen}"
    );

    let screen: Vec<String> = crate::hosts::modal_rows(&fleet, b"laptop", theme())
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    let screen = screen.join("\n");
    assert!(
        screen.contains("offline · this machine is signed out"),
        "{screen}"
    );
    assert!(!screen.contains("not signed in"), "{screen}");
    // Home's top line says how to sign in again.
    let mut fleet_view = FleetView {
        local_host: b"laptop".to_vec(),
        ..FleetView::default()
    };
    let screen = fleet_screen(&mut fleet_view, &fleet);
    assert!(screen.contains("signed out · amux login"), "{screen}");

    // Signed in again, the words are what they always were.
    let mut signed_in = host(
        b"laptop",
        "laptop",
        wire::Trust::Trusted,
        wire::Presence::Online,
    );
    if let wire::inventory_event::Of::Host(entry) = &mut signed_in {
        entry.via = wire::RouteKind::Unspecified as i32;
        entry.account_signed_in = Some(true);
    }
    inventory(&mut fleet, signed_in);
    view.reach = ui_view::reach(&fleet, b"laptop", b"host");
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(screen.contains("desk away · not current"), "{screen}");
    assert!(!screen.contains("signed out"), "{screen}");
}

/// A host the relay refused while the account is on the free plan is
/// away because the free plan relays nothing between hosts, and every
/// place that says it is away says so: the hosts overlay put a stale
/// direct-dial error in its place, and the chat said only "away".
#[test]
fn a_host_away_on_the_free_plan_names_the_plan() {
    let mut fleet = FleetState::new();
    inventory(
        &mut fleet,
        host(
            b"laptop",
            "laptop",
            wire::Trust::Trusted,
            wire::Presence::Online,
        ),
    );
    let mut desk = host(b"host", "desk", wire::Trust::Trusted, wire::Presence::Away);
    if let wire::inventory_event::Of::Host(entry) = &mut desk {
        entry.via = wire::RouteKind::Relay as i32;
        entry.account_signed_in = Some(true);
        entry.last_dial_error = Some("127.0.0.1:61393: the QUIC handshake timed out".into());
    }
    inventory(&mut fleet, desk);

    let mut state = chat(replies(1, 5));
    state.update(Msg::Host(fleet.host(b"host").unwrap().clone()));
    state.update(event(session_event::Of::Detached(wire::Detached {})));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.reach = ui_view::reach(&fleet, b"laptop", b"host");
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(
        screen.contains("desk away · the free plan does not relay between hosts"),
        "{screen}"
    );
    assert!(
        screen.contains("desk is away · the free plan does not relay between hosts"),
        "{screen}"
    );

    let rows: Vec<String> = crate::hosts::modal_rows(&fleet, b"laptop", theme())
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    let rows = rows.join("\n");
    assert!(
        rows.contains("away · the free plan does not relay between hosts"),
        "{rows}"
    );
}

/// An empty chat caught up from its host has nothing yet, even while the
/// fleet, on its own stream, still has the host away; once its source
/// is lost the chat says the host is away.
#[test]
fn an_empty_caught_up_chat_is_not_away_by_a_lagging_fleet() {
    let mut state = chat(vec![]);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.reach = ui_view::Reach::Offline(ui_view::Offline::Plain);
    let screen = text(&draw(&mut view, &state, 0, W, H, theme()).0);
    assert!(screen.contains("Nothing here yet."), "{screen}");
    assert!(!screen.contains("host is away"), "{screen}");

    state.update(event(session_event::Of::Detached(wire::Detached {})));
    let screen = text(&draw(&mut view, &state, 0, W, H, theme()).0);
    assert!(
        screen.contains("Its host is away, and nothing of this chat is held here yet."),
        "{screen}"
    );
}

/// A host that closed its link saying it no longer trusts this machine is
/// captioned and headed with that, never with its own old sign-in.
#[test]
fn a_host_that_revoked_trust_says_so_instead_of_not_signed_in() {
    let mut fleet = FleetState::new();
    inventory(
        &mut fleet,
        host(
            b"laptop",
            "laptop",
            wire::Trust::Trusted,
            wire::Presence::Online,
        ),
    );
    let mut desk = host(
        b"host",
        "desk",
        wire::Trust::Trusted,
        wire::Presence::Offline,
    );
    if let wire::inventory_event::Of::Host(entry) = &mut desk {
        entry.via = wire::RouteKind::Unspecified as i32;
        entry.account_signed_in = Some(false);
        entry.revoked = Some(true);
    }
    inventory(&mut fleet, desk);

    let screen: Vec<String> = crate::hosts::modal_rows(&fleet, b"laptop", theme())
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    let screen = screen.join("\n");
    assert!(
        screen.contains("offline · no longer trusts this machine"),
        "{screen}"
    );
    assert!(!screen.contains("not signed in"), "{screen}");

    let mut state = chat(replies(1, 5));
    state.update(Msg::Host(fleet.host(b"host").unwrap().clone()));
    state.update(event(session_event::Of::Detached(wire::Detached {})));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.reach = ui_view::reach(&fleet, b"laptop", b"host");
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(
        screen.contains("desk no longer trusts this machine"),
        "{screen}"
    );
    assert!(screen.contains("it sends once you pair again"), "{screen}");
}

#[test]
fn a_reset_keeps_the_rows_until_caught_up_then_shows_the_newest() {
    let mut state = chat(replies(1, 60));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    feed(&mut view, &state);
    view.key(&state, key(KeyCode::PageUp), theme());
    let (before, _) = feed(&mut view, &state);

    state.update(event(session_event::Of::Reset(wire::Reset {
        generation: 1,
    })));
    state.update(snapshot(Phase::Idle, vec![], vec![]));
    for order in 200..=210 {
        let mut fresh = item(order, None);
        fresh.key = format!("fresh{order}");
        state.update(event(session_event::Of::Item(fresh)));
    }
    let (during, _) = feed(&mut view, &state);
    assert_eq!(
        before.lines().nth(5),
        during.lines().nth(5),
        "rows stay on screen"
    );
    assert!(
        during.contains("refreshing") || during.contains("catching up"),
        "{during}"
    );

    state.update(caught_up(0));
    let (after, _) = feed(&mut view, &state);
    assert_eq!(view.anchor, Anchor::Bottom);
    assert!(after.contains("reply number 210"), "{after}");
}

// --- hosts and the fleet ---------------------------------------------------

fn inventory(fleet: &mut FleetState, of: wire::inventory_event::Of) {
    fleet.update(FleetMsg::Event(Box::new(wire::InventoryEvent {
        of: Some(of),
    })));
}

fn host(
    id: &[u8],
    name: &str,
    trust: wire::Trust,
    presence: wire::Presence,
) -> wire::inventory_event::Of {
    wire::inventory_event::Of::Host(wire::HostEntry {
        host_id: id.to_vec(),
        name: name.into(),
        trust: trust as i32,
        presence: presence as i32,
        via: wire::RouteKind::Direct as i32,
        ..Default::default()
    })
}

#[test]
fn the_hosts_overlay_draws_trusted_hosts_and_candidates() {
    let mut fleet = FleetState::new();
    inventory(
        &mut fleet,
        host(b"a", "studio", wire::Trust::Trusted, wire::Presence::Online),
    );
    inventory(
        &mut fleet,
        host(
            b"b",
            "laptop",
            wire::Trust::Trusted,
            wire::Presence::Offline,
        ),
    );
    inventory(
        &mut fleet,
        host(
            b"c",
            "den mac",
            wire::Trust::Candidate,
            wire::Presence::Online,
        ),
    );
    let lines = crate::hosts::modal_rows(&fleet, b"a", theme());
    let screen: Vec<String> = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    let screen = screen.join("\n");
    let row = |name: &str| {
        screen
            .lines()
            .find(|line| line.trim_start().starts_with(name))
            .unwrap_or_else(|| panic!("{name}\n{screen}"))
            .to_owned()
    };
    assert!(row("laptop").ends_with("offline"), "{screen}");
    assert!(row("studio").ends_with("this machine"), "{screen}");
    assert!(
        row("den mac").ends_with("found nearby · amux pair 'den mac'"),
        "{screen}"
    );
    let studio = screen.find("studio").unwrap();
    let candidate = screen.find("den mac").unwrap();
    assert!(studio < candidate, "trusted hosts come first");
}

fn agent_row(
    id: &[u8],
    name: &str,
    phase: Phase,
    parent: Option<&[u8]>,
) -> wire::inventory_event::Of {
    wire::inventory_event::Of::Agent(wire::Agent {
        agent_id: id.to_vec(),
        host_id: b"a".to_vec(),
        kind: Kind::ClaudePty as i32,
        name: name.into(),
        lifecycle: wire::Lifecycle::Live as i32,
        phase: phase as i32,
        parent: parent.map(|parent| wire::AgentRef {
            host_id: b"a".to_vec(),
            agent_id: parent.to_vec(),
        }),
        ..Default::default()
    })
}

fn fleet_screen(view: &mut FleetView, fleet: &FleetState) -> String {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(W, H)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            view.draw(frame, area, fleet, &HashMap::new(), None, 0, theme());
        })
        .unwrap();
    text(terminal.backend().buffer())
}

#[test]
fn the_fleet_says_restart_to_update_when_the_daemon_runs_another_build() {
    let mut fleet = FleetState::new();
    let mut studio = host(b"a", "studio", wire::Trust::Trusted, wire::Presence::Online);
    if let wire::inventory_event::Of::Host(entry) = &mut studio {
        entry.version = Some("0.8.0".into());
    }
    inventory(&mut fleet, studio);
    inventory(
        &mut fleet,
        wire::inventory_event::Of::CaughtUp(wire::CaughtUp { revision: 0 }),
    );
    let mut view = FleetView {
        version: "0.8.0".into(),
        local_host: b"a".to_vec(),
        ..FleetView::default()
    };
    let screen = fleet_screen(&mut view, &fleet);
    assert!(!screen.contains("restart to update"), "{screen}");
    view.version = "0.7.0".into();
    let screen = fleet_screen(&mut view, &fleet);
    assert!(
        screen.contains("amux 0.8.0 running · restart to update"),
        "{screen}"
    );
}

#[test]
fn raw_attach_is_only_for_terminals_on_this_machine() {
    let config = crate::TuiConfig {
        working_dir: ".".into(),
        leader: 'a',
        theme: theme(),
        initial_chat: None,
        attach: true,
        version: "0.7.0".into(),
        local_host: b"a".to_vec(),
        layout: None,
        reports: None,
        chat_in: crate::setup::ChatIn::Amux,
        defaults: defaults(),
    };
    let agent = |host: &[u8], kind: Kind| wire::Agent {
        host_id: host.to_vec(),
        kind: kind as i32,
        ..Default::default()
    };
    let refusal = |agent: &wire::Agent| crate::app::terminal_refusal(agent, &config);
    assert_eq!(refusal(&agent(b"a", Kind::ClaudePty)), None);
    assert_eq!(refusal(&agent(b"a", Kind::Codex)), None);
    assert!(refusal(&agent(b"a", Kind::ClaudeSdk)).is_some_and(|why| why.contains("no terminal")));
    // Every agent on another host is refused with the same reason: raw
    // attach reads the agent's directory on this machine. Home adds that
    // Enter opens its chat; a chat does not, since Enter there sends.
    for kind in [Kind::ClaudePty, Kind::Codex, Kind::ClaudeSdk] {
        assert_eq!(
            refusal(&agent(b"b", kind)),
            Some("its terminal is on another machine"),
        );
    }
}

#[test]
fn an_answered_question_row_reads_the_question_and_what_was_picked() {
    for (kind, name, row) in [
        (
            Kind::ClaudePty,
            "recorded_question_single",
            "Which color do you prefer?\n  │   → Red",
        ),
        (
            Kind::ClaudePty,
            "recorded_question_other_single",
            "Which color do you prefer?\n  │   → a warm ochre",
        ),
        (
            Kind::ClaudeSdk,
            "recorded_question_every_shape",
            "Answered 4 questions",
        ),
    ] {
        let (_, state, at) = fixtures::frames(kind, name).pop().unwrap();
        let mut view = ChatView::new(b"agent".to_vec(), at, false);
        let (buffer, _) = draw(&mut view, &state, at, W, H, theme());
        let screen = text(&buffer);
        assert!(screen.contains(row), "{name}:\n{screen}");
        if kind == Kind::ClaudeSdk {
            assert!(screen.contains("→ Hammer, Drill"), "{screen}");
            assert!(screen.contains("→ Dried mango"), "{screen}");
        } else {
            assert!(screen.contains("Answered a question"), "{screen}");
        }
    }
}

// --- the review page -------------------------------------------------------

const PATCH: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,3 +10,4 @@ fn main() {
 let a = 1;
-let b = 2;
+let b = 3;
+let c = 4;
diff --git a/notes.md b/notes.md
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/notes.md
@@ -0,0 +1 @@
+hello
";

fn working_tree_diff() -> wire::Diff {
    wire::Diff {
        patch: Some(wire::BlobRef {
            hash: vec![9; 32],
            name: "working-tree.diff".into(),
            mime: "text/x-diff".into(),
            size: PATCH.len() as u64,
        }),
        base: Some(wire::DiffBase {
            base: Some(wire::diff_base::Base::Head(wire::Empty {})),
        }),
        head: "3f2a1c9e0000".into(),
        merge_base: None,
        files: vec![
            wire::DiffFile {
                path: "src/lib.rs".into(),
                added: 2,
                removed: 1,
                change: wire::DiffFileChange::Changed as i32,
                binary: false,
            },
            wire::DiffFile {
                path: "notes.md".into(),
                added: 1,
                removed: 0,
                change: wire::DiffFileChange::Created as i32,
                binary: false,
            },
        ],
    }
}

fn review_screen(view: &mut ChatView, state: &SessionState) -> (String, ratatui::buffer::Buffer) {
    let (buffer, _) = draw(view, state, 0, W, H, theme());
    (text(&buffer), buffer)
}

fn review_token(view: &ChatView) -> Option<wire::Review> {
    view.editor.attachments().iter().find_map(|a| match &a.of {
        Some(wire::attachment::Of::Review(review)) => Some(review.clone()),
        _ => None,
    })
}

#[test]
fn the_review_page_lists_files_and_styles_hunks() {
    let state = chat(replies(1, 3));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.open_review_at(working_tree_diff(), PATCH.into(), None);
    let (screen, buffer) = review_screen(&mut view, &state);
    assert!(
        screen.contains("review · working tree at 3f2a1c9"),
        "{screen}"
    );
    assert!(screen.contains("2 files · +3 −1"), "{screen}");
    // The list beside the stream, files under their directories.
    assert!(screen.contains("notes.md           +1  │"), "{screen}");
    assert!(screen.contains("src/"), "{screen}");
    assert!(screen.contains("lib.rs        +2 −1  │"), "{screen}");
    // No hunk headers: one column of numbers, a removed line by its old
    // number and every other by its new one.
    assert!(!screen.contains("@@"), "{screen}");
    // The page opens on the first changed line.
    assert!(screen.contains("│▌   1 + hello"), "{screen}");
    assert!(screen.contains("│   11 - let b = 2;"), "{screen}");
    assert!(screen.contains("│   11 + let b = 3;"), "{screen}");

    // Added and removed lines carry the diff tints across the row.
    let row_of = |needle: &str| {
        screen
            .lines()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle}: {screen}")) as u16
    };
    let added = row_of("+ let b = 3;");
    let removed = row_of("- let b = 2;");
    let context = row_of("let a = 1;");
    assert_eq!(buffer[(W - 1, added)].bg, theme().diff_added().bg.unwrap());
    assert_eq!(
        buffer[(W - 1, removed)].bg,
        theme().diff_removed().bg.unwrap()
    );
    assert_ne!(
        buffer[(W - 1, context)].bg,
        theme().diff_added().bg.unwrap()
    );
}

#[test]
fn a_hunk_after_left_out_lines_opens_on_a_break_naming_where_it_is() {
    let patch = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n\
                 @@ -1,2 +1,2 @@\n a\n-b\n+c\n@@ -20,2 +20,2 @@ fn main() {\n d\n-e\n+f\n\
                 @@ -40,2 +40,2 @@\n g\n-h\n+i\n";
    let state = chat(replies(1, 3));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.open_review_at(working_tree_diff(), patch.into(), None);
    let (screen, _) = review_screen(&mut view, &state);
    // The page beside the file list.
    let lines: Vec<&str> = screen
        .lines()
        .map(|line| line.rsplit_once('│').map_or(line, |(_, page)| page))
        .collect();
    let at = |needle: &str| {
        lines
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle} in {screen}"))
    };
    // The first hunk starts at the top, so nothing is left out before it.
    assert!(lines[at("1   a") - 1].trim().is_empty(), "{screen}");
    // The next opens on its scope; git's ranges are not shown.
    let named = at("⋯   fn main() { ──");
    assert!(lines[named - 1].trim().is_empty(), "{screen}");
    assert!(lines[named + 1].contains("20   d"), "{screen}");
    assert!(!screen.contains("@@"), "{screen}");
    // A hunk git names nothing for still opens on the break.
    let bare = at("40   g") - 1;
    assert!(lines[bare].trim_start().starts_with("⋯   ──"), "{screen}");
}

#[test]
fn the_first_saved_comment_puts_a_review_token_in_the_draft_at_the_cursor() {
    let state = chat(replies(1, 3));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    typed(&mut view, &state, "look at this ");
    view.open_review_at(working_tree_diff(), PATCH.into(), None);

    // Down past notes.md to src/lib.rs's first added line, and comment.
    for _ in 0..4 {
        view.key(&state, key(KeyCode::Char('j')), theme());
    }
    view.key(&state, key(KeyCode::Char('c')), theme());
    typed(&mut view, &state, "why three?");
    assert!(review_token(&view).is_none(), "nothing until it is saved");
    view.key(&state, key(KeyCode::Enter), theme());
    let (screen, _) = review_screen(&mut view, &state);
    assert!(screen.contains("│ why three?"), "{screen}");
    assert!(screen.contains("2 files · +3 −1 · 1 comment"), "{screen}");

    let review = review_token(&view).expect("the token is in the draft");
    assert_eq!(review.diff, Some(working_tree_diff()));
    assert_eq!(
        review.comments,
        vec![wire::ReviewComment {
            path: "src/lib.rs".into(),
            line: 11,
            old_line: 0,
            text: "why three?".into(),
        }]
    );
    assert_eq!(
        view.editor.text(),
        format!("look at this {} ", attachments::PLACEHOLDER)
    );

    // A comment on the removed line lands on its old-side number, and the
    // same token updates where it sits.
    view.key(&state, key(KeyCode::Char('k')), theme());
    view.key(&state, key(KeyCode::Char('c')), theme());
    typed(&mut view, &state, "was two");
    view.key(&state, key(KeyCode::Enter), theme());
    assert_eq!(view.editor.attachments().len(), 1);
    let review = review_token(&view).unwrap();
    assert_eq!(review.comments.len(), 2);
    assert_eq!(
        (review.comments[1].line, review.comments[1].old_line),
        (0, 11)
    );

    // q returns to the chat with the draft kept and the token drawn.
    view.key(&state, key(KeyCode::Char('q')), theme());
    assert!(!view.review_open);
    let (screen, _) = review_screen(&mut view, &state);
    assert!(
        screen.contains("look at this [review · 2 comments]"),
        "{screen}"
    );
    // <leader> r goes back to the same page while its token is held.
    assert!(view.resume_review());
    let (screen, _) = review_screen(&mut view, &state);
    assert!(screen.contains("│ was two"), "{screen}");
}

#[test]
fn deleting_comments_and_the_token_drops_the_draft_review() {
    let state = chat(replies(1, 3));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.open_review_at(working_tree_diff(), PATCH.into(), None);
    view.key(&state, key(KeyCode::Char('c')), theme());
    typed(&mut view, &state, "one");
    view.key(&state, key(KeyCode::Enter), theme());
    // Enter on a commented line edits its comment.
    view.key(&state, key(KeyCode::Enter), theme());
    typed(&mut view, &state, " more");
    view.key(&state, key(KeyCode::Enter), theme());
    assert_eq!(review_token(&view).unwrap().comments[0].text, "one more");
    // d deletes it, and with no comments left the token goes too.
    view.key(&state, key(KeyCode::Char('d')), theme());
    assert!(review_token(&view).is_none());
    assert!(view.editor.is_empty());
    view.key(&state, key(KeyCode::Esc), theme());
    assert!(!view.resume_review(), "no token: a fresh diff is wanted");

    // A token backspaced out of the draft drops the review the same way.
    view.open_review_at(working_tree_diff(), PATCH.into(), None);
    view.key(&state, key(KeyCode::Char('c')), theme());
    typed(&mut view, &state, "two");
    view.key(&state, key(KeyCode::Enter), theme());
    view.key(&state, key(KeyCode::Char('q')), theme());
    // The space the token keeps after itself, then the token.
    view.key(&state, key(KeyCode::Backspace), theme());
    view.key(&state, key(KeyCode::Backspace), theme());
    assert!(view.editor.is_empty());
    assert!(!view.resume_review());
}

// --- wording and keys found in use ------------------------------------------

#[test]
fn a_question_mark_types_into_a_review_comment() {
    let state = chat(replies(1, 3));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let question = key(KeyCode::Char('?'));
    assert!(
        view.opens_help(&state, question),
        "the empty composer's key"
    );
    view.open_review_at(working_tree_diff(), PATCH.into(), None);
    view.key(&state, key(KeyCode::Char('j')), theme());
    view.key(&state, key(KeyCode::Char('c')), theme());
    assert!(!view.opens_help(&state, question), "the comment's key");
    typed(&mut view, &state, "why?");
    typed(&mut view, &state, " ok");
    view.key(&state, key(KeyCode::Enter), theme());
    let review = review_token(&view).expect("the comment is saved");
    assert_eq!(review.comments[0].text, "why? ok");
}

/// A step as the feed draws it outside any run, opened or not.
fn row_text(row: &ui_view::Row, open: bool) -> String {
    crate::chat::feed::row_lines(
        row,
        &crate::chat::feed::Placement::Plain,
        open,
        &Default::default(),
        100,
        theme(),
    )
    .map(|drawn| drawn.lines)
    .unwrap_or_default()
    .iter()
    .map(|line| {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    })
    .collect::<Vec<_>>()
    .join("\n")
}

fn call_row(kind: ui_view::RowKind) -> ui_view::Row {
    ui_view::Row {
        id: "k".into(),
        order: 1,
        at_ms: 0,
        kind,
        run: None,
        collapsed: false,
        decision: None,
        attention: false,
        parent: None,
    }
}

/// A long command whose output lost its start says so above what is left,
/// so the first lines shown are not taken for the command's first.
#[test]
fn a_trimmed_commands_output_says_its_start_was_dropped() {
    use ui_view::{CallPhase, RowKind};
    let command = |output_trimmed| {
        call_row(RowKind::Command {
            command: "cargo test".into(),
            state: CallPhase::Running,
            exit_code: None,
            output_head: vec!["test b ... ok".into()],
            more_lines: 0,
            output_trimmed,
            output_tail: vec!["test b ... ok".into()],
            duration_ms: None,
        })
    };
    let screen = row_text(&command(true), true);
    let notice = screen.find("earlier output trimmed").expect(&screen);
    assert!(notice < screen.find("test b ... ok").unwrap(), "{screen}");
    assert!(!row_text(&command(false), true).contains("trimmed"));
}

#[test]
fn a_call_row_leads_with_what_happened_to_it() {
    use ui_view::{CallPhase, Decision, DecisionView, RowKind};
    let command = |state| {
        call_row(RowKind::Command {
            command: "rm -rf target".into(),
            state,
            exit_code: None,
            output_head: vec![],
            more_lines: 0,
            output_trimmed: false,
            output_tail: vec![],
            duration_ms: None,
        })
    };
    let denied = |mut row: ui_view::Row| {
        row.decision = Some(Decision {
            outcome: DecisionView::Denied,
            granted: None,
            note: Some("Use cargo clean instead".into()),
            elsewhere: false,
        });
        row
    };

    let mut asking = command(CallPhase::Asking);
    asking.attention = true;
    let screen = row_text(&asking, false);
    assert!(screen.contains("Wants to run rm -rf target"), "{screen}");
    assert!(!screen.contains("running"), "{screen}");

    // Announced and not started, it asks nobody: it reads as under way.
    for phase in [CallPhase::Pending, CallPhase::Running] {
        let screen = row_text(&command(phase), false);
        assert!(screen.contains("Running rm -rf target"), "{screen}");
        assert!(
            !screen.contains("running") && !screen.contains("Wants"),
            "{screen}"
        );
    }

    let screen = row_text(&denied(command(CallPhase::Denied)), false);
    assert!(screen.contains("Denied rm -rf target"), "{screen}");
    assert!(screen.contains("Use cargo clean instead"), "{screen}");
    assert!(
        !screen.contains("Ran") && !screen.contains("denied"),
        "{screen}"
    );

    let screen = row_text(
        &denied(call_row(RowKind::ToolCall {
            server: "linear".into(),
            tool: "delete_issue".into(),
            fact: "FOX-12".into(),
            state: CallPhase::Denied,
            result: String::new(),
        })),
        false,
    );
    assert!(screen.contains("Denied linear delete_issue"), "{screen}");
    assert!(
        !screen.contains("Used") && !screen.contains("denied"),
        "{screen}"
    );

    let screen = row_text(&command(CallPhase::Succeeded), false);
    assert!(screen.contains("Ran rm -rf target"), "{screen}");
}

#[test]
fn an_exited_agent_on_an_away_host_names_why_it_is_away() {
    let mut state = chat(replies(1, 3));
    let mut exited = fixtures::agent(Kind::ClaudeSdk);
    exited.lifecycle = wire::Lifecycle::Exited as i32;
    exited.exit_cause = wire::ExitCause::Stopped as i32;
    state.update(Msg::Entry(exited));
    state.update(Msg::Host(wire::HostEntry {
        host_id: b"host".to_vec(),
        name: "desk".into(),
        trust: wire::Trust::Trusted as i32,
        presence: wire::Presence::Offline as i32,
        ..Default::default()
    }));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.reach = ui_view::Reach::Offline(ui_view::Offline::SignedOut);
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(
        screen.contains("exited · desk away · this machine is signed out"),
        "{screen}"
    );
    assert!(screen.contains("Enter resumes"), "{screen}");

    // Back online, the header says how it ended.
    state.update(Msg::Host(wire::HostEntry {
        host_id: b"host".to_vec(),
        name: "desk".into(),
        trust: wire::Trust::Trusted as i32,
        presence: wire::Presence::Online as i32,
        ..Default::default()
    }));
    view.reach = ui_view::Reach::Online(wire::RouteKind::Direct);
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    // Stopped is an ordinary end: the header says exited, and no more.
    let screen = text(&buffer);
    assert!(screen.contains("exited"), "{screen}");
    assert!(!screen.contains("exited · "), "{screen}");
}

#[test]
fn an_agent_that_exited_while_the_daemon_was_away_says_exited_once() {
    let mut state = chat(replies(1, 3));
    let mut exited = fixtures::agent(Kind::ClaudeSdk);
    exited.lifecycle = wire::Lifecycle::Exited as i32;
    // The daemon's cause for an agent it found gone when it came back.
    exited.exit_cause = wire::ExitCause::EndedWhileAway as i32;
    state.update(Msg::Entry(exited.clone()));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (buffer, _) = draw(&mut view, &state, 0, W, H, theme());
    let screen = text(&buffer);
    assert!(
        screen.contains("exited · while the daemon was away"),
        "{screen}"
    );
    assert!(!screen.contains("exited · exited"), "{screen}");

    let mut fleet = FleetState::new();
    inventory(
        &mut fleet,
        host(
            b"host",
            "desk",
            wire::Trust::Trusted,
            wire::Presence::Online,
        ),
    );
    inventory(&mut fleet, wire::inventory_event::Of::Agent(exited));
    // Home lists it under Exited, which starts folded.
    let mut fleet_view = FleetView::default();
    fleet_view.key(&fleet, key(KeyCode::Char('G')));
    fleet_view.key(&fleet, key(KeyCode::Enter));
    let screen = fleet_screen(&mut fleet_view, &fleet);
    let row = screen
        .lines()
        .position(|line| line.contains("worker"))
        .expect(&screen);
    let rows = screen
        .lines()
        .skip(row)
        .take(2)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rows.contains("while the daemon was away"), "{screen}");
    assert!(!rows.contains("exited · exited"), "{screen}");
}

/// Thinking is never drawn, with or without a measured time.
#[test]
fn thinking_is_never_drawn() {
    use wire::claude_sdk_item::Kind as K;
    let thinking = |order: u64, at_ms: i64| Item {
        key: format!("t{order}"),
        order,
        revision: order,
        text: "weighing it".into(),
        kind: Kind::ClaudeSdk as i32,
        body: wire::ClaudeSdkItem {
            kind: Some(K::Thinking(wire::Thinking { complete: true })),
        }
        .encode_to_vec(),
        at_ms,
        ..Item::default()
    };
    let state = chat(vec![item(1, None), thinking(2, 1_000), thinking(3, 4_000)]);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (screen, _) = feed(&mut view, &state);
    assert!(screen.contains("reply number 1"), "{screen}");
    assert!(!screen.contains("Thought"), "{screen}");
    assert!(!screen.contains("weighing it"), "{screen}");
}

/// "Something else" opens its field on Enter (letters move the list, as
/// j and k do), and what is typed then is the answer.
#[test]
fn enter_on_something_else_opens_its_answer() {
    let (state, at) = fixtures::frame_where(
        Kind::ClaudeSdk,
        "recorded_question_every_shape",
        |state| matches!(ask_card(state).map(|card| card.body), Some(AskBody::Question(questions)) if questions.len() > 1),
    );
    let Some(AskBody::Question(questions)) = ask_card(&state).map(|card| card.body) else {
        unreachable!()
    };
    assert!(questions[0].allow_other);
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    for _ in 0..questions[0].options.len() {
        view.key(&state, key(KeyCode::Down), theme());
    }
    let (buffer, _) = draw(&mut view, &state, at, 120, 60, theme());
    assert!(text(&buffer).contains("Something else · enter to type"));
    view.key(&state, key(KeyCode::Enter), theme());
    typed(&mut view, &state, "fish");
    let (buffer, _) = draw(&mut view, &state, at, 120, 60, theme());
    let screen = text(&buffer);
    assert!(screen.contains("Something else: fish"), "{screen}");
}

// --- keys and pastes go to the field or overlay that is open -----------------

fn screen_of(view: &mut ChatView, state: &SessionState, at: i64) -> String {
    let (buffer, _) = draw(view, state, at, 120, 60, theme());
    text(&buffer)
}

/// Esc reaches the box's way out; Tab opens the note it carries.
fn deny_with_note(view: &mut ChatView, state: &SessionState) {
    view.key(state, key(KeyCode::Esc), theme());
    view.key(state, key(KeyCode::Tab), theme());
}

fn multi_question() -> (SessionState, i64) {
    fixtures::frame_where(
        Kind::ClaudeSdk,
        "recorded_question_every_shape",
        |state| matches!(ask_card(state).map(|card| card.body), Some(AskBody::Question(questions)) if questions.len() > 1),
    )
}

fn form() -> (SessionState, i64) {
    fixtures::frame_where(Kind::ClaudeSdk, "recorded_elicitation_accepted", |state| {
        matches!(
            ask_card(state).map(|card| card.body),
            Some(AskBody::Form { .. })
        )
    })
}

#[test]
fn a_note_answered_does_not_keep_ctrl_c_from_the_composer() {
    for note in ["use cargo clean", ""] {
        let (asking, at) = fixtures::Named::ClaudePermissionAsk.state();
        let mut view = ChatView::new(b"agent".to_vec(), at, false);
        deny_with_note(&mut view, &asking);
        typed(&mut view, &asking, note);
        let effects = view.key(&asking, key(KeyCode::Enter), theme());
        let [ChatEffect::Answer(input)] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        // While the answer goes out the card has no field.
        let mut sending = asking.clone();
        sending.update(Msg::Send(input.clone()));
        assert_eq!(ask_card(&sending).unwrap().state, CardState::Sending);
        assert!(!view.field_text(&sending), "note {note:?}: sending");
        // The ask is answered and gone: Ctrl+C is the composer's again.
        let answered = chat(replies(1, 3));
        assert!(
            !view.field_text(&answered),
            "note {note:?}: nothing to clear"
        );
        typed(&mut view, &answered, "draft");
        assert!(view.field_text(&answered));
        assert!(view.kill_field(&answered));
        assert!(view.editor.is_empty());
        assert!(!view.field_text(&answered));
    }
}

#[test]
fn ctrl_c_in_an_ask_field_clears_that_field_and_never_the_draft() {
    let draft = |view: &mut ChatView| view.editor.set("hidden draft", vec![]);
    let kept = |view: &ChatView| assert_eq!(view.editor.text(), "hidden draft");

    // A denial's note.
    let (state, at) = fixtures::Named::ClaudePermissionAsk.state();
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    draft(&mut view);
    deny_with_note(&mut view, &state);
    assert!(
        !view.field_text(&state),
        "an empty note has nothing to clear"
    );
    assert!(!view.kill_field(&state));
    kept(&view);
    typed(&mut view, &state, "no");
    assert!(view.field_text(&state));
    assert!(view.kill_field(&state));
    kept(&view);
    let screen = screen_of(&mut view, &state, at);
    assert!(
        screen.contains("› 3. No") && !screen.contains("No: no"),
        "{screen}"
    );

    // "Something else…".
    let (state, at) = multi_question();
    let Some(AskBody::Question(questions)) = ask_card(&state).map(|card| card.body) else {
        unreachable!()
    };
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    draft(&mut view);
    for _ in 0..questions[0].options.len() {
        view.key(&state, key(KeyCode::Down), theme());
    }
    view.key(&state, key(KeyCode::Enter), theme());
    typed(&mut view, &state, "fish");
    assert!(view.field_text(&state));
    assert!(view.kill_field(&state));
    kept(&view);
    assert!(!view.kill_field(&state));
    kept(&view);

    // A form field.
    let (state, at) = form();
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    draft(&mut view);
    typed(&mut view, &state, "jlw/amux");
    assert!(view.field_text(&state));
    assert!(view.kill_field(&state));
    kept(&view);
    assert!(!view.field_text(&state));
    assert!(!view.kill_field(&state));
    kept(&view);
}

#[test]
fn a_paste_goes_to_the_field_with_the_keys() {
    // The composer, when it has them.
    let idle = chat(replies(1, 3));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.paste_text(&idle, "hello");
    assert_eq!(view.editor.text(), "hello");

    // A review comment.
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.open_review_at(working_tree_diff(), PATCH.into(), None);
    view.key(&idle, key(KeyCode::Char('j')), theme());
    view.key(&idle, key(KeyCode::Char('c')), theme());
    view.paste_text(&idle, "why this?");
    view.key(&idle, key(KeyCode::Enter), theme());
    let review = review_token(&view).expect("the comment is saved");
    assert_eq!(review.comments[0].text, "why this?");
    assert_eq!(
        view.editor.text().trim_end().chars().count(),
        1,
        "only the review token"
    );

    // A denial's note; on the menu there is nothing to paste into.
    let (state, at) = fixtures::Named::ClaudePermissionAsk.state();
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    view.paste_text(&state, "ignored");
    deny_with_note(&mut view, &state);
    view.paste_text(&state, "because");
    assert!(view.editor.is_empty());
    let screen = screen_of(&mut view, &state, at);
    assert!(screen.contains("No: because"), "{screen}");

    // "Something else", once Enter has opened it.
    let (state, at) = multi_question();
    let Some(AskBody::Question(questions)) = ask_card(&state).map(|card| card.body) else {
        unreachable!()
    };
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    for _ in 0..questions[0].options.len() {
        view.key(&state, key(KeyCode::Down), theme());
    }
    view.key(&state, key(KeyCode::Enter), theme());
    view.paste_text(&state, "fish");
    view.paste_text(&state, " soup");
    assert!(view.editor.is_empty());
    let screen = screen_of(&mut view, &state, at);
    assert!(screen.contains("Something else: fish soup"), "{screen}");

    // A form field.
    let (state, at) = form();
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    view.paste_text(&state, "jlw/amux");
    assert!(view.editor.is_empty());
    let screen = screen_of(&mut view, &state, at);
    assert!(screen.contains("› jlw/amux"), "{screen}");
}

#[test]
fn a_paste_into_a_secret_answer_shows_as_bullets() {
    let token = ui_view::QuestionView {
        header: "Token".into(),
        question: "Paste the deploy token".into(),
        multi_select: false,
        options: vec![],
        allow_other: true,
        secret: true,
    };
    let card = ui_view::AskCard {
        kind: Kind::ClaudeSdk,
        key: "ask".into(),
        item_key: "k".into(),
        position: 1,
        count: 1,
        body: AskBody::Question(vec![token]),
        choices: vec![],
        question_note: true,
        question_skip: true,
        question_reply: true,
        stops_turn: true,
        state: CardState::Open,
    };
    let mut ask = crate::chat::ask::AskUi::default();
    ask.sync(&card);
    ask.paste_box_note(&card, "s3cret");
    let lines = ask.box_lines(&card, 100, theme());
    let screen: Vec<String> = lines
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    let screen = screen.join("\n");
    assert!(screen.contains("› ••••••"), "{screen}");
    assert!(!screen.contains("s3cret"), "{screen}");
}

#[test]
fn a_paste_in_the_fleet_types_into_the_rename_field() {
    let mut fleet = FleetState::new();
    inventory(
        &mut fleet,
        host(b"a", "studio", wire::Trust::Trusted, wire::Presence::Online),
    );
    inventory(&mut fleet, agent_row(b"p", "planner", Phase::Idle, None));
    let mut view = FleetView::default();
    view.paste("ignored");
    view.key(&fleet, key(KeyCode::Char('r')));
    view.key(&fleet, ctrl('u'));
    view.paste("lead\r");
    view.paste("er");
    let effects = view.key(&fleet, key(KeyCode::Enter));
    assert!(
        matches!(effects.as_slice(), [FleetEffect::Rename { name, .. }] if name == "leader"),
        "{effects:?}"
    );
}

#[test]
fn q_and_question_mark_reach_an_open_fleet_overlay_first() {
    let mut fleet = FleetState::new();
    inventory(
        &mut fleet,
        host(b"a", "studio", wire::Trust::Trusted, wire::Presence::Online),
    );
    inventory(&mut fleet, agent_row(b"p", "planner", Phase::Idle, None));
    let mut view = FleetView::default();
    let q = key(KeyCode::Char('q'));
    let help = key(KeyCode::Char('?'));
    assert_eq!(view.key(&fleet, help), vec![FleetEffect::Help]);

    // The hosts overlay: q closes it, and the next q quits.
    view.key(&fleet, key(KeyCode::Char('h')));
    assert!(fleet_screen(&mut view, &fleet).contains("studio"));
    assert_eq!(view.key(&fleet, q), vec![]);
    assert_eq!(view.key(&fleet, q), vec![FleetEffect::Quit]);

    // New and confirm keep their overlay.
    for opens in ['n', 'x'] {
        view.key(&fleet, key(KeyCode::Char(opens)));
        assert_eq!(view.key(&fleet, q), vec![], "{opens}");
        assert_eq!(view.key(&fleet, help), vec![], "{opens}");
        view.key(&fleet, key(KeyCode::Esc));
    }

    // Rename types both, even into an empty field. "q?" is no name, so
    // Enter keeps the field and says why; without the "?" it saves.
    view.key(&fleet, key(KeyCode::Char('r')));
    view.key(&fleet, ctrl('u'));
    assert_eq!(view.key(&fleet, q), vec![]);
    assert_eq!(view.key(&fleet, help), vec![]);
    assert_eq!(view.key(&fleet, key(KeyCode::Enter)), vec![]);
    assert!(fleet_screen(&mut view, &fleet).contains("a-z, 0-9 and hyphens"));
    view.key(&fleet, key(KeyCode::Backspace));
    let effects = view.key(&fleet, key(KeyCode::Enter));
    assert!(
        matches!(effects.as_slice(), [FleetEffect::Rename { name, .. }] if name == "q"),
        "{effects:?}"
    );
}

#[test]
fn left_on_a_later_question_and_the_review_goes_back() {
    let (state, at) = multi_question();
    let Some(AskBody::Question(questions)) = ask_card(&state).map(|card| card.body) else {
        unreachable!()
    };
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    let asks = |view: &mut ChatView, question: &str| {
        let screen = screen_of(view, &state, at);
        assert!(screen.contains(question), "{question}\n{screen}");
    };
    if questions[0].multi_select {
        view.key(&state, key(KeyCode::Char(' ')), theme());
    }
    view.key(&state, key(KeyCode::Enter), theme());
    asks(&mut view, &questions[1].question);
    view.key(&state, key(KeyCode::Left), theme());
    asks(&mut view, &questions[0].question);

    for question in &questions {
        if question.multi_select {
            view.key(&state, key(KeyCode::Char(' ')), theme());
        }
        view.key(&state, key(KeyCode::Enter), theme());
    }
    asks(&mut view, "Send answers");
    view.key(&state, key(KeyCode::Left), theme());
    let screen = screen_of(&mut view, &state, at);
    assert!(!screen.contains("Send answers"), "{screen}");
    asks(&mut view, &questions[questions.len() - 1].question);
}

// --- home ------------------------------------------------------------------

fn home_agent(
    id: &[u8],
    name: &str,
    phase: Phase,
    parent: Option<&[u8]>,
    activity_ms: i64,
) -> wire::inventory_event::Of {
    let mut row = agent_row(id, name, phase, parent);
    if let wire::inventory_event::Of::Agent(agent) = &mut row {
        agent.phase_since_ms = activity_ms;
        agent.cwd = "/work/amux".into();
    }
    row
}

fn now() -> i64 {
    use client::Clock as _;
    client::SystemClock.now_ms()
}

/// A fleet on host `a`: a family whose child needs you, two working
/// agents, and an idle one untouched for two days.
fn home_fleet() -> FleetState {
    let now = now();
    let mut fleet = FleetState::new();
    inventory(
        &mut fleet,
        host(b"a", "studio", wire::Trust::Trusted, wire::Presence::Online),
    );
    inventory(
        &mut fleet,
        home_agent(b"p", "planner", Phase::Idle, None, now - 60_000),
    );
    inventory(
        &mut fleet,
        home_agent(b"c", "worker", Phase::NeedsYou, Some(b"p"), now - 30_000),
    );
    inventory(
        &mut fleet,
        home_agent(b"w1", "alpha", Phase::Working, None, now - 5_000),
    );
    inventory(
        &mut fleet,
        home_agent(b"w2", "beta", Phase::Working, None, now - 9_000),
    );
    let mut archive = home_agent(b"old", "archive", Phase::Idle, None, now - 2 * 86_400_000);
    if let wire::inventory_event::Of::Agent(agent) = &mut archive {
        agent.lifecycle = wire::Lifecycle::Exited as i32;
        agent.exit_cause = wire::ExitCause::Stopped as i32;
    }
    inventory(&mut fleet, archive);
    inventory(
        &mut fleet,
        wire::inventory_event::Of::CaughtUp(wire::CaughtUp { revision: 0 }),
    );
    fleet
}

/// New-agent defaults like the shipped ones.
fn defaults() -> crate::setup::Defaults {
    let of = |model: &str, effort: &str| crate::setup::AgentDefaults {
        model: model.into(),
        effort: effort.into(),
        permission: "default".into(),
    };
    crate::setup::Defaults {
        claude: of("opus", "high"),
        codex: of("gpt-6.1-sol", "medium"),
    }
}

static HOME_PLACE: std::sync::LazyLock<crate::home::Place<'static>> =
    std::sync::LazyLock::new(|| crate::home::Place {
        local_host: b"a",
        version: "",
        working_dir: "~/work/amux",
        attach: false,
        chat_in: crate::setup::ChatIn::Amux,
        defaults: Box::leak(Box::new(defaults())),
    });

fn home_screen(home: &mut crate::home::Home, fleet: &FleetState, theme: Theme) -> String {
    home_screen_with(home, fleet, &HashMap::new(), theme)
}

/// Home with what each agent's session says for its second line.
fn home_screen_with(
    home: &mut crate::home::Home,
    fleet: &FleetState,
    lines: &HashMap<AgentKey, ui_view::SessionLine>,
    theme: Theme,
) -> String {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(W, H)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            home.draw(frame, area, fleet, lines, None, now(), theme, &HOME_PLACE);
        })
        .unwrap();
    text(terminal.backend().buffer())
}

fn row_of(screen: &str, words: &str) -> u16 {
    screen
        .lines()
        .position(|line| line.contains(words))
        .unwrap_or_else(|| panic!("{words} is not on\n{screen}")) as u16
}

#[test]
fn home_leads_with_what_needs_you_and_folds_the_exited() {
    let fleet = home_fleet();
    let mut home = crate::home::Home::default();
    let screen = home_screen(&mut home, &fleet, theme());
    // The family whose child needs you is under the heading, its child
    // named on its second line; the running follow newest first; the
    // exited are folded away under their own heading.
    let heading = row_of(&screen, "Needs you 1");
    let planner = row_of(&screen, "planner");
    assert!(heading < planner, "{screen}");
    assert!(screen.contains("↳ worker"), "{screen}");
    assert!(
        row_of(&screen, "alpha") < row_of(&screen, "beta"),
        "{screen}"
    );
    assert!(
        row_of(&screen, "Needs you 1") < row_of(&screen, "Live 2"),
        "{screen}"
    );
    assert!(screen.contains("Exited 1"), "{screen}");
    assert!(!screen.contains("archive"), "{screen}");
    assert!(!screen.contains('┌'), "home has no frame: {screen}");
}

/// Home orders agents by when each last changed state: work streaming in
/// changes only a row's second line, and a turn ending moves its agent up.
#[test]
fn home_keeps_its_order_while_agents_stream_and_moves_one_whose_turn_ended() {
    let mut fleet = home_fleet();
    let mut home = crate::home::Home::default();
    let beta = AgentKey {
        host: b"a".to_vec(),
        agent: b"w2".to_vec(),
    };
    let step = |subject: &str| ui_view::SessionLine {
        step: Some(ui_view::ActivityLine {
            activity: ui_state::Activity {
                kind: ui_state::ActivityKind::Running { key: "call".into() },
                since_ms: 0,
                elapsed_ms: 0,
                turn_elapsed_ms: 0,
            },
            step: Some(subject.to_owned()),
        }),
        ..Default::default()
    };
    // beta streams: each step it takes shows on its second line, and it
    // stays below alpha, which changed state more recently.
    for subject in ["cargo build", "cargo test -p relay"] {
        let lines = HashMap::from([(beta.clone(), step(subject))]);
        let screen = home_screen_with(&mut home, &fleet, &lines, theme());
        assert!(
            row_of(&screen, "alpha") < row_of(&screen, "beta"),
            "{screen}"
        );
        assert_eq!(row_of(&screen, subject), row_of(&screen, "beta") + 1);
    }
    // beta's turn ends: its state changed last, so it leads.
    inventory(
        &mut fleet,
        home_agent(b"w2", "beta", Phase::Idle, None, now()),
    );
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(
        row_of(&screen, "beta") < row_of(&screen, "alpha"),
        "{screen}"
    );
}

#[test]
fn hovering_highlights_a_row_and_its_close_mark_asks_first() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let fleet = home_fleet();
    let mut home = crate::home::Home::default();
    let screen = home_screen(&mut home, &fleet, theme());
    let beta = row_of(&screen, "beta");
    let mouse = |kind, column, row| MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    home.mouse(&fleet, mouse(MouseEventKind::Moved, 10, beta), false);
    // Without a known ground the highlight is a mark and weight.
    let screen = home_screen(&mut home, &fleet, theme());
    let line = screen.lines().nth(usize::from(beta)).unwrap();
    assert!(
        line.contains('›') && line.trim_end().ends_with("[x]"),
        "{screen}"
    );
    // A key takes over from where the mouse left the highlight.
    home.key(&fleet, key(KeyCode::Char('j')), false);
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(
        !screen.lines().nth(usize::from(beta)).unwrap().contains('›'),
        "{screen}"
    );
    home.key(&fleet, key(KeyCode::Char('k')), false);
    home_screen(&mut home, &fleet, theme());
    // The × stops, after asking; beta is working, so the window says the
    // stop cuts its turn, and Enter on the focused [Stop] confirms.
    let effects = home.mouse(
        &fleet,
        mouse(MouseEventKind::Down(MouseButton::Left), W - 5, beta),
        false,
    );
    assert!(effects.is_empty());
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(screen.contains("beta is working. Stop anyway?"), "{screen}");
    let effects = home.key(&fleet, key(KeyCode::Enter), false);
    assert!(matches!(effects.as_slice(), [FleetEffect::Stop(agent)] if agent.agent == b"w2"));
}

#[test]
fn a_known_ground_tints_the_highlight_instead_of_marking_it() {
    let fleet = home_fleet();
    let mut home = crate::home::Home::default();
    let colors = crate::theme::TerminalColors {
        background: (21, 21, 21),
        foreground: (208, 208, 208),
        ansi: [(128, 128, 128); 16],
    };
    let tinted = Theme::from_terminal(colors, crate::theme::ColorMode::TrueColor);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(W, H)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            home.draw(
                frame,
                area,
                &fleet,
                &HashMap::new(),
                None,
                now(),
                tinted,
                &HOME_PLACE,
            );
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let screen = text(&buffer);
    assert!(!screen.contains('›'), "{screen}");
    let planner = row_of(&screen, "planner");
    let cell = &buffer[(W / 2, planner)];
    assert!(
        matches!(cell.bg, ratatui::style::Color::Rgb(..)),
        "the highlighted row is tinted: {:?}",
        cell.bg
    );
    // Only the highlighted row: the margins and other rows keep the
    // terminal's own ground.
    assert_eq!(buffer[(0, planner)].bg, ratatui::style::Color::Reset);
    assert_eq!(buffer[(W / 2, 0)].bg, ratatui::style::Color::Reset);
}

#[test]
fn a_new_agent_starts_from_a_draft_with_its_first_prompt() {
    let fleet = home_fleet();
    let mut home = crate::home::Home::default();
    home.key(&fleet, key(KeyCode::Char('n')), false);
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(
        screen.contains("What should the new agent work on?"),
        "{screen}"
    );
    // Until the host says what Claude offers there, the settings' own
    // words; then the catalogue's.
    assert!(
        screen.contains("Claude · opus (high) · default │ ~/work/amux"),
        "{screen}"
    );
    home.open_setup(&HOME_PLACE).unwrap().offer(
        b"a",
        wire::Provider::Claude,
        &[1; 32],
        &claude_offer(),
    );
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(
        screen.contains("Claude · Opus (high) · ask │ ~/work/amux"),
        "{screen}"
    );
    // Enter on an empty draft does nothing.
    assert!(home.key(&fleet, key(KeyCode::Enter), false).is_empty());
    for c in "fix it".chars() {
        home.key(&fleet, key(KeyCode::Char(c)), false);
    }
    let effects = home.key(
        &fleet,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
        false,
    );
    assert!(
        matches!(effects.as_slice(), [FleetEffect::Start { setup, text, open: false, .. }] if text == "fix it" && setup.kind() == Kind::ClaudeSdk),
        "{effects:?}"
    );
    // While it starts, keys do not edit the draft.
    home.key(&fleet, key(KeyCode::Char('x')), false);
    assert_eq!(home.draft.editor.text(), "fix it");
    home.started();
    assert!(home.draft.editor.is_empty());
}

/// docs/TERMINAL.md: "A new agent always shows the values it will start
/// with" on the composer's bottom edge. At 80 columns in a deep folder the
/// edge dropped the model and the permission (even one that never asks)
/// and kept the folder, so the picks were invisible until the agent ran.
/// The folder is what can be shortened; the model and permission are what
/// the person is about to start.
#[test]
fn a_new_agents_edge_keeps_its_model_and_permission_in_a_deep_folder_at_80_columns() {
    let fleet = home_fleet();
    let place = crate::home::Place {
        working_dir: "~/.local/state/amux-qa/20261011-171536/t6/.testnet/net/desk/work",
        ..*HOME_PLACE
    };
    let mut home = crate::home::Home::default();
    home.key(&fleet, key(KeyCode::Char('n')), false);
    let setup = home.open_setup(&place).unwrap();
    setup.offer(b"a", wire::Provider::Claude, &[1; 32], &claude_offer());
    setup.pick(crate::setup::Item::Permission, "bypassPermissions");
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, H)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            home.draw(
                frame,
                area,
                &fleet,
                &HashMap::new(),
                None,
                now(),
                theme(),
                &place,
            );
        })
        .unwrap();
    let screen = text(terminal.backend().buffer());
    let edge = screen
        .lines()
        .find(|line| line.contains("╰") && line.contains("Claude"))
        .unwrap_or_else(|| panic!("no edge on\n{screen}"));
    assert!(
        edge.contains("Opus (high)") && edge.contains("never ask"),
        "the edge names the model and the permission it will start with: {edge}"
    );
}

/// What Shift+Tab steps shows on a new agent's edge however narrow the
/// edge is: at 80 columns in a deep folder, Codex's plan mode went on with
/// nothing on the edge saying so, and the agent started planning.
#[test]
fn a_new_codex_edge_shows_the_plan_mode_shift_tab_turned_on_at_80_columns() {
    let fleet = home_fleet();
    let place = crate::home::Place {
        working_dir: "~/.local/state/amux-qa/20261013-043305/t8/.testnet/net/desk/work",
        ..*HOME_PLACE
    };
    let mut home = crate::home::Home::default();
    home.key(&fleet, key(KeyCode::Char('n')), false);
    let setup = home.open_setup(&place).unwrap();
    setup.pick(crate::setup::Item::Kind, "1");
    setup.offer(b"a", wire::Provider::Codex, &[2; 32], &codex_offer());
    setup.pick(crate::setup::Item::Permission, "read-only");
    let edge_of = |home: &mut crate::home::Home| {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, H)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                home.draw(
                    frame,
                    area,
                    &fleet,
                    &HashMap::new(),
                    None,
                    now(),
                    theme(),
                    &place,
                );
            })
            .unwrap();
        let screen = text(terminal.backend().buffer());
        screen
            .lines()
            .find(|line| line.contains("╰") && line.contains("Codex"))
            .unwrap_or_else(|| panic!("no edge on\n{screen}"))
            .to_owned()
    };
    assert!(
        !edge_of(&mut home).contains("plan"),
        "{}",
        edge_of(&mut home)
    );
    home.key(&fleet, key(KeyCode::BackTab), false);
    let edge = edge_of(&mut home);
    assert!(
        edge.contains("plan") && edge.contains("read only"),
        "the edge does not show the plan mode Shift+Tab turned on: {edge}"
    );
    home.key(&fleet, key(KeyCode::BackTab), false);
    assert!(
        !edge_of(&mut home).contains("plan"),
        "{}",
        edge_of(&mut home)
    );
}

/// What a host offers for Claude, as headless Claude says.
fn claude_offer() -> wire::Catalogue {
    let permission = |value: &str, display_name: &str| wire::OfferedPermission {
        value: value.into(),
        display_name: display_name.into(),
        settable: true,
        ..Default::default()
    };
    wire::Catalogue {
        models: vec![wire::OfferedModel {
            value: "opus".into(),
            display_name: "Opus".into(),
            efforts: vec!["low".into(), "medium".into(), "high".into()],
            ..Default::default()
        }],
        permissions: vec![
            wire::OfferedPermission {
                normal: true,
                ..permission("default", "Ask")
            },
            permission("acceptEdits", "Accept edits"),
            permission("plan", "Plan"),
            wire::OfferedPermission {
                never_asks: true,
                ..permission("bypassPermissions", "Never ask")
            },
        ],
        ..Default::default()
    }
}

/// What a host offers for Codex, as its app server says.
fn codex_offer() -> wire::Catalogue {
    let permission = |value: &str, display_name: &str| wire::OfferedPermission {
        value: value.into(),
        display_name: display_name.into(),
        settable: true,
        ..Default::default()
    };
    let mode = |value: &str, display_name: &str| wire::OfferedMode {
        value: value.into(),
        display_name: display_name.into(),
        normal: value == "default",
        settable: true,
    };
    wire::Catalogue {
        models: vec![
            wire::OfferedModel {
                value: "gpt-6.1-sol".into(),
                display_name: "GPT-6.1 Sol".into(),
                efforts: vec!["low".into(), "medium".into(), "high".into(), "xhigh".into()],
                default_effort: Some("medium".into()),
                ..Default::default()
            },
            wire::OfferedModel {
                value: "gpt-6-mini".into(),
                display_name: "GPT-6 mini".into(),
                efforts: vec!["low".into(), "high".into()],
                default_effort: Some("low".into()),
                ..Default::default()
            },
        ],
        permissions: vec![
            permission("read-only", "Read only"),
            wire::OfferedPermission {
                normal: true,
                ..permission("default", "Default")
            },
            permission("auto", "Auto"),
            wire::OfferedPermission {
                never_asks: true,
                ..permission("full-access", "Full access")
            },
        ],
        modes: vec![mode("default", "Default"), mode("plan", "Plan")],
        ..Default::default()
    }
}

/// The labels of a setting's choices, "label · detail ✓" for the current.
fn choice_words(
    home: &mut crate::home::Home,
    fleet: &FleetState,
    item: crate::setup::Item,
) -> Vec<String> {
    let setup = home.open_setup(&HOME_PLACE).unwrap();
    setup
        .choices(item, fleet)
        .into_iter()
        .map(|choice| {
            let mut words = choice.label;
            if !choice.detail.is_empty() {
                words.push_str(&format!(" · {}", choice.detail));
            }
            if choice.current {
                words.push_str(" ✓");
            }
            words
        })
        .collect()
}

/// A new Codex agent offers the models, efforts, permissions and modes its
/// host's catalogue lists, and Shift+Tab steps its mode.
#[test]
fn a_new_agent_offers_what_its_host_offers() {
    use crate::setup::Item;
    let fleet = home_fleet();
    let mut home = crate::home::Home::default();
    home.key(&fleet, key(KeyCode::Char('n')), false);
    home_screen(&mut home, &fleet, theme());
    home.open_setup(&HOME_PLACE).unwrap().pick(Item::Kind, "1");
    // Before the host says, a model can only be named.
    assert!(
        home.open_setup(&HOME_PLACE)
            .unwrap()
            .takes_typed(Item::Model)
    );
    assert_eq!(choice_words(&mut home, &fleet, Item::Effort), ["medium ✓"]);
    // What another host offers is not this one's.
    home.open_setup(&HOME_PLACE).unwrap().offer(
        b"b",
        wire::Provider::Codex,
        &[2; 32],
        &codex_offer(),
    );
    assert!(home.open_setup(&HOME_PLACE).unwrap().offered.is_none());
    home.open_setup(&HOME_PLACE).unwrap().offer(
        b"a",
        wire::Provider::Codex,
        &[2; 32],
        &codex_offer(),
    );
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(
        screen.contains("Codex · GPT-6.1 Sol (medium) · default │ ~/work/amux"),
        "{screen}"
    );
    assert!(screen.contains("shift+tab mode"), "{screen}");
    assert!(
        !home
            .open_setup(&HOME_PLACE)
            .unwrap()
            .takes_typed(Item::Model)
    );
    assert_eq!(
        choice_words(&mut home, &fleet, Item::Model),
        ["GPT-6.1 Sol ✓", "GPT-6 mini"]
    );
    assert_eq!(
        choice_words(&mut home, &fleet, Item::Effort),
        ["low", "medium · default ✓", "high", "xhigh"]
    );
    assert_eq!(
        choice_words(&mut home, &fleet, Item::Permission),
        [
            "read only",
            "default ✓",
            "auto",
            "full access · acts without asking"
        ]
    );
    assert_eq!(
        choice_words(&mut home, &fleet, Item::Mode),
        ["default ✓", "plan"]
    );

    // Another model starts at its own default effort.
    home.open_setup(&HOME_PLACE)
        .unwrap()
        .pick(Item::Model, "gpt-6-mini");
    assert_eq!(
        choice_words(&mut home, &fleet, Item::Effort),
        ["low · default ✓", "high"]
    );
    // Shift+Tab steps the mode, which the edge names once it is not the
    // normal one.
    home.key(&fleet, key(KeyCode::BackTab), false);
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(
        screen.contains("Codex · GPT-6 mini (low) · default · plan │"),
        "{screen}"
    );
    let request = home
        .open_setup(&HOME_PLACE)
        .unwrap()
        .request(b"id".to_vec(), None);
    let Some(wire::create_agent_request::Agent::Codex(config)) = request.agent else {
        panic!("a Codex agent: {request:?}");
    };
    assert_eq!(
        (config.model.as_deref(), config.effort.as_deref()),
        (Some("gpt-6-mini"), Some("low"))
    );
    assert_eq!(
        (config.permission.as_deref(), config.mode.as_deref()),
        (Some("default"), Some("plan"))
    );
    // Another agent is another provider's catalogue, asked afresh.
    home.open_setup(&HOME_PLACE).unwrap().pick(Item::Kind, "0");
    assert!(home.open_setup(&HOME_PLACE).unwrap().offered.is_none());
}

/// Claude's Shift+Tab steps the permissions that still ask, as in a chat.
#[test]
fn a_new_claude_steps_the_permissions_that_still_ask() {
    let fleet = home_fleet();
    let mut home = crate::home::Home::default();
    home.key(&fleet, key(KeyCode::Char('n')), false);
    home_screen(&mut home, &fleet, theme());
    home.open_setup(&HOME_PLACE).unwrap().offer(
        b"a",
        wire::Provider::Claude,
        &[1; 32],
        &claude_offer(),
    );
    let mut seen = Vec::new();
    for _ in 0..4 {
        home.key(&fleet, key(KeyCode::BackTab), false);
        seen.push(
            home.open_setup(&HOME_PLACE)
                .unwrap()
                .permission
                .clone()
                .unwrap(),
        );
    }
    assert_eq!(seen, ["acceptEdits", "plan", "default", "acceptEdits"]);
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(
        screen.contains("Claude · Opus (high) · accept edits │"),
        "{screen}"
    );
    assert!(screen.contains("shift+tab permission"), "{screen}");
}

/// A provider its host says is signed out is named so where it is chosen.
#[test]
fn a_new_agent_says_when_its_provider_is_not_signed_in() {
    use crate::setup::Item;
    let mut fleet = home_fleet();
    let mut studio = host(b"a", "studio", wire::Trust::Trusted, wire::Presence::Online);
    if let wire::inventory_event::Of::Host(entry) = &mut studio {
        entry.providers = vec![
            wire::ProviderOnHost {
                provider: wire::Provider::Claude as i32,
                catalogue: Some(vec![1; 32]),
                signed_in: true,
            },
            wire::ProviderOnHost {
                provider: wire::Provider::Codex as i32,
                catalogue: Some(vec![2; 32]),
                signed_in: false,
            },
        ];
    }
    inventory(&mut fleet, studio);
    let mut home = crate::home::Home::default();
    home.key(&fleet, key(KeyCode::Char('n')), false);
    home_screen(&mut home, &fleet, theme());
    assert_eq!(
        choice_words(&mut home, &fleet, Item::Kind),
        ["Claude ✓", "Codex · not signed in"]
    );
    home.open_setup(&HOME_PLACE).unwrap().pick(Item::Kind, "1");
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(
        screen.contains("Codex (not signed in) · gpt-6.1-sol"),
        "{screen}"
    );

    // The form for an agent used in its own terminal says it on the chip.
    let place = crate::home::Place {
        local_host: HOME_PLACE.local_host,
        version: HOME_PLACE.version,
        working_dir: HOME_PLACE.working_dir,
        attach: false,
        chat_in: crate::setup::ChatIn::Terminal,
        defaults: HOME_PLACE.defaults,
    };
    let mut home = crate::home::Home::default();
    home.key(&fleet, key(KeyCode::Char('n')), false);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(W, H)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            home.draw(
                frame,
                area,
                &fleet,
                &HashMap::new(),
                None,
                now(),
                theme(),
                &place,
            );
        })
        .unwrap();
    let screen = text(terminal.backend().buffer());
    assert!(
        screen.contains(" Claude   Codex · not signed in "),
        "{screen}"
    );
}

/// A new agent can start in a new worktree, from the composer's settings
/// or the own-terminal form's checkbox, and the request asks for one; a
/// start the host refuses says why in the form and keeps what was typed.
#[test]
fn a_new_agent_asks_for_a_new_worktree_and_shows_why_a_start_failed() {
    let fleet = home_fleet();
    let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
    let refused = "starting in a new worktree: the repository already has a branch named fix-it";
    let mut home = crate::home::Home::default();
    home.key(&fleet, key(KeyCode::Char('n')), false);
    home_screen(&mut home, &fleet, theme());
    home.key(&fleet, ctrl_s, false);
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(screen.contains("w worktree"), "{screen}");
    home.key(&fleet, key(KeyCode::Char('w')), false);
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(
        screen.contains("│ ~/work/amux · new worktree │"),
        "{screen}"
    );
    for c in "fix it".chars() {
        home.key(&fleet, key(KeyCode::Char(c)), false);
    }
    let effects = home.key(&fleet, key(KeyCode::Enter), false);
    let [FleetEffect::Start { setup, .. }] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert!(setup.request(b"id".to_vec(), None).new_worktree);
    assert!(home.start_failed(refused.to_owned()));
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(
        screen.contains("Could not start the agent: starting in a new worktree:"),
        "{screen}"
    );
    assert!(screen.contains("fix it"), "{screen}");
    // Started again, the old reason goes; off again, the request asks for
    // none.
    home.key(&fleet, ctrl_s, false);
    home.key(&fleet, key(KeyCode::Char('w')), false);
    let effects = home.key(&fleet, key(KeyCode::Enter), false);
    let [FleetEffect::Start { setup, .. }] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert!(!setup.request(b"id".to_vec(), None).new_worktree);
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(!screen.contains("Could not start"), "{screen}");
    // Left meanwhile, the form has nowhere to say it.
    home.key(&fleet, key(KeyCode::Esc), false);
    assert!(!home.start_failed(refused.to_owned()));

    // The form for an agent used in its own terminal: a checkbox.
    let place = crate::home::Place {
        local_host: HOME_PLACE.local_host,
        version: HOME_PLACE.version,
        working_dir: HOME_PLACE.working_dir,
        attach: false,
        chat_in: crate::setup::ChatIn::Terminal,
        defaults: HOME_PLACE.defaults,
    };
    let mut home = crate::home::Home::default();
    home.key(&fleet, key(KeyCode::Char('n')), false);
    let screen = home_screen_at(&mut home, &fleet, &place);
    assert!(screen.contains("Worktree  [ ] new worktree"), "{screen}");
    // Out of the name, down to the last row, and Space ticks it.
    home.key(&fleet, key(KeyCode::Esc), false);
    for _ in 0..4 {
        home.key(&fleet, key(KeyCode::Down), false);
    }
    home.key(&fleet, key(KeyCode::Char(' ')), false);
    let screen = home_screen_at(&mut home, &fleet, &place);
    assert!(screen.contains("Worktree  [✓] new worktree"), "{screen}");
    let effects = home.key(&fleet, key(KeyCode::Enter), false);
    let [FleetEffect::Start { setup, .. }] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert!(setup.request(b"id".to_vec(), None).new_worktree);
    assert!(home.start_failed(refused.to_owned()));
    let screen = home_screen_at(&mut home, &fleet, &place);
    assert!(
        screen.contains("Could not start the agent: starting in a new worktree:"),
        "{screen}"
    );
    assert!(screen.contains("[Start]"), "{screen}");
}

/// Home drawn for `place`.
fn home_screen_at(
    home: &mut crate::home::Home,
    fleet: &FleetState,
    place: &crate::home::Place<'_>,
) -> String {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(W, H)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            home.draw(
                frame,
                area,
                fleet,
                &HashMap::new(),
                None,
                now(),
                theme(),
                place,
            );
        })
        .unwrap();
    text(terminal.backend().buffer())
}

#[test]
fn the_filter_lives_in_the_top_line_and_narrows_the_list() {
    let fleet = home_fleet();
    let mut home = crate::home::Home::default();
    home.key(&fleet, key(KeyCode::Char('/')), false);
    for c in "arch".chars() {
        home.key(&fleet, key(KeyCode::Char(c)), false);
    }
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(
        // The top line sits under one blank line.
        screen.lines().nth(1).unwrap().contains("/ arch"),
        "{screen}"
    );
    // The filter looks through folded sections too.
    assert!(screen.contains("archive"), "{screen}");
    assert!(!screen.contains("alpha"), "{screen}");
    home.key(&fleet, key(KeyCode::Enter), false);
    home.key(&fleet, key(KeyCode::Esc), false);
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(
        screen.contains("alpha") && !screen.contains("archive"),
        "{screen}"
    );
}

// --- the feed -------------------------------------------------------------

fn click(view: &mut ChatView, state: &SessionState, screen: &str, words: &str) -> Vec<ChatEffect> {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let row = screen
        .lines()
        .position(|line| line.contains(words))
        .unwrap_or_else(|| panic!("{words} is not on\n{screen}"));
    view.mouse(
        state,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 6,
            row: row as u16,
            modifiers: KeyModifiers::NONE,
        },
        theme(),
    )
}

/// Tool steps between two replies fold to one line of counts, which opens
/// to the steps; a step opens in turn.
#[test]
fn a_run_folds_to_its_counts_and_opens_to_its_steps() {
    let mut items = replies(1, 1);
    items.extend((2..=4).map(|order| item(order, Some(&format!("src/file{order}.rs")))));
    items.extend(replies(5, 5));
    let state = chat(items);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (screen, _) = feed(&mut view, &state);
    assert!(screen.contains("▸ 3 reads"), "{screen}");
    assert!(!screen.contains("src/file2.rs"), "{screen}");

    // Opened, every step has its own line: no merged "Read 3 files",
    // which would only repeat the folded line.
    assert!(click(&mut view, &state, &screen, "3 reads").is_empty());
    let (screen, _) = feed(&mut view, &state);
    assert!(screen.contains("▾ 3 reads"), "{screen}");
    assert!(!screen.contains("Read 3 files"), "{screen}");
    for order in 2..=4 {
        assert!(screen.contains(&format!("src/file{order}.rs")), "{screen}");
    }

    // And the run folds again from its line.
    click(&mut view, &state, &screen, "3 reads");
    let (screen, _) = feed(&mut view, &state);
    assert!(screen.contains("▸ 3 reads"), "{screen}");
}

/// An opened run longer than the window stays open as the reader scrolls
/// into it and its older steps page in below.
#[test]
fn an_opened_run_stays_open_as_its_older_steps_page_in() {
    let mut items: Vec<Item> = (41..=60)
        .map(|order| item(order, Some(&format!("src/file{order}.rs"))))
        .collect();
    items.extend(replies(61, 61));
    let mut state = chat(items);
    assert!(state.transcript().has_older());
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (screen, _) = feed(&mut view, &state);
    click(&mut view, &state, &screen, "20+ reads");
    let (screen, _) = feed(&mut view, &state);
    assert!(screen.contains("src/file59.rs"), "{screen}");

    let epoch = state.epoch();
    state.update(Msg::Page {
        items: (21..=40)
            .map(|order| item(order, Some(&format!("src/file{order}.rs"))))
            .collect(),
        exhausted: false,
        epoch,
    });
    let (screen, _) = feed(&mut view, &state);
    assert!(!screen.contains("▸"), "the run folded:\n{screen}");
    assert!(screen.contains("src/file59.rs"), "{screen}");
    // Scrolled up, the paged steps draw on their own lines under the
    // run's line.
    let mut screen = screen;
    for _ in 0..10 {
        if screen.contains("40+ reads") {
            break;
        }
        view.key(&state, key(KeyCode::PageUp), theme());
        screen = feed(&mut view, &state).0;
    }
    assert!(screen.contains("▾ 40+ reads"), "{screen}");
    for order in 21..=25 {
        assert!(screen.contains(&format!("src/file{order}.rs")), "{screen}");
    }
}

/// A failed command its turn left unresolved stays in view where it ran
/// when its run folds; one the agent redid folds with the rest.
#[test]
fn a_folded_run_keeps_a_failure_its_turn_left_unresolved() {
    use wire::claude_sdk_item::Kind as K;
    let command = |order: u64, line: &str, exit: i32| {
        let mut item = item(order, Some("unused"));
        item.body = wire::ClaudeSdkItem {
            kind: Some(K::Tool(wire::ToolCall {
                name: "Bash".into(),
                state: ToolState::Succeeded as i32,
                class: ToolClass::Consequential as i32,
                input_json: format!(r#"{{"command":"{line}"}}"#).into_bytes(),
                exit_code: Some(exit),
                ..Default::default()
            })),
        }
        .encode_to_vec();
        item
    };
    let turn_end = |order: u64| {
        let mut item = item(order, None);
        item.text = String::new();
        item.body = wire::ClaudeSdkItem {
            kind: Some(K::Turn(wire::Turn::default())),
        }
        .encode_to_vec();
        item
    };
    let mut items = replies(1, 1);
    items.push(command(2, "just lint", 1));
    items.push(command(3, "just lint", 0));
    items.push(command(4, "just docs-check", 1));
    items.push(item(5, Some("docs/a.md")));
    items.extend(replies(6, 6));
    let mut state = chat(items);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (screen, _) = feed(&mut view, &state);
    assert!(screen.contains("▸ 3 commands · 1 read"), "{screen}");
    assert!(
        !screen.contains("just docs-check"),
        "until the turn ends the agent may fix it\n{screen}"
    );

    state.update(event(session_event::Of::Item(turn_end(7))));
    let (screen, _) = feed(&mut view, &state);
    assert!(
        screen.contains("✗ Ran just docs-check · exit 1"),
        "{screen}"
    );
    assert!(!screen.contains("just lint"), "{screen}");
    let failure = row_of(&screen, "just docs-check");
    assert!(failure < row_of(&screen, "3 commands"), "{screen}");
}

/// `<leader> t` switches a chat's tool steps round from collapse through
/// show all and hide: show all lists every step with no fold line, and
/// hide leaves only the agent's text and a failure its turn left
/// unresolved.
#[test]
fn tool_steps_switch_between_collapse_show_all_and_hide() {
    use wire::claude_sdk_item::Kind as K;

    use crate::chat::ToolSteps;
    let command = |order: u64, line: &str, exit: i32| {
        let mut item = item(order, Some("unused"));
        item.body = wire::ClaudeSdkItem {
            kind: Some(K::Tool(wire::ToolCall {
                name: "Bash".into(),
                state: ToolState::Succeeded as i32,
                class: ToolClass::Consequential as i32,
                input_json: format!(r#"{{"command":"{line}"}}"#).into_bytes(),
                exit_code: Some(exit),
                ..Default::default()
            })),
        }
        .encode_to_vec();
        item
    };
    let mut items = replies(1, 1);
    items.push(command(2, "just lint", 1));
    items.push(command(3, "just lint", 0));
    items.push(command(4, "just docs-check", 1));
    items.push(item(5, Some("docs/a.md")));
    items.extend(replies(6, 6));
    let mut turn_end = item(7, None);
    turn_end.text = String::new();
    turn_end.body = wire::ClaudeSdkItem {
        kind: Some(K::Turn(wire::Turn::default())),
    }
    .encode_to_vec();
    items.push(turn_end);
    // A second turn still at it: five reads, no text after them yet.
    items.extend(replies(8, 8));
    items.extend((9..=13).map(|order| item(order, Some(&format!("src/file{order}.rs")))));
    let state = chat(items);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    assert_eq!(view.tools, ToolSteps::Collapse);
    let (screen, _) = feed(&mut view, &state);
    assert!(screen.contains("▸ 3 commands · 1 read"), "{screen}");
    assert!(
        screen.contains("✗ Ran just docs-check · exit 1"),
        "{screen}"
    );
    assert!(!screen.contains("just lint"), "{screen}");
    assert!(screen.contains("2 earlier steps"), "{screen}");
    assert!(!screen.contains("src/file10.rs"), "{screen}");
    assert!(screen.contains("src/file13.rs"), "{screen}");

    assert_eq!(view.switch_tools(), ToolSteps::ShowAll);
    let (screen, _) = feed(&mut view, &state);
    assert!(!screen.contains("3 commands"), "{screen}");
    assert!(!screen.contains("earlier step"), "{screen}");
    for words in ["just lint", "just docs-check", "docs/a.md"] {
        assert!(screen.contains(words), "{words}:\n{screen}");
    }
    for order in 9..=13 {
        assert!(screen.contains(&format!("src/file{order}.rs")), "{screen}");
    }

    assert_eq!(view.switch_tools(), ToolSteps::Hide);
    let (screen, _) = feed(&mut view, &state);
    assert!(
        screen.contains("✗ Ran just docs-check · exit 1"),
        "{screen}"
    );
    for words in [
        "just lint",
        "docs/a.md",
        "3 commands",
        "earlier step",
        "src/file",
    ] {
        assert!(!screen.contains(words), "{words}:\n{screen}");
    }
    for order in [1, 6, 8] {
        assert!(
            screen.contains(&format!("reply number {order}")),
            "{screen}"
        );
    }

    assert_eq!(view.switch_tools(), ToolSteps::Collapse);
    let (screen, _) = feed(&mut view, &state);
    assert!(screen.contains("▸ 3 commands · 1 read"), "{screen}");
}

/// How a chat draws its tool steps is kept for that chat between runs;
/// another chat keeps collapsing them.
#[test]
fn a_chats_tool_steps_choice_is_kept_for_that_chat() {
    use crate::chat::ToolSteps;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("layout.json");
    let mut layout = crate::app::Layout::load(Some(&path));
    assert!(layout.set_tools(b"one", ToolSteps::Hide));
    assert!(!layout.set_tools(b"one", ToolSteps::Hide));
    layout.save(Some(&path));

    let mut layout = crate::app::Layout::load(Some(&path));
    assert_eq!(layout.tools_of(b"one"), ToolSteps::Hide);
    assert_eq!(layout.tools_of(b"two"), ToolSteps::Collapse);
    // Back to the default, the chat is no longer named.
    assert!(layout.set_tools(b"one", ToolSteps::Collapse));
    assert!(layout.tools.is_empty());
}

/// The header names the agent under one blank line, and the composer is
/// boxed.
#[test]
fn the_chat_header_names_the_agent_and_offers_the_diff_and_home() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let state = chat_in_git(replies(1, 2), Some("main"));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (screen, _) = feed(&mut view, &state);
    assert!(
        screen.lines().next().unwrap_or_default().trim().is_empty(),
        "{screen}"
    );
    let first = screen.lines().nth(1).unwrap_or_default();
    // The name, then the controls; where the chat stands is left to the
    // feed while nothing is wrong.
    assert!(first.trim_start().starts_with("worker"), "{screen}");
    assert!(
        first.contains("[Diff +42 −7]") && first.contains("[Home]"),
        "{screen}"
    );
    assert!(!first.contains("idle"), "{screen}");
    // The composer is boxed and the keys sit one blank line under it.
    let lines: Vec<&str> = screen.lines().collect();
    let edge = lines
        .iter()
        .rposition(|line| line.contains('╯'))
        .expect(&screen);
    assert!(lines[edge + 1].trim().is_empty(), "{screen}");
    assert!(lines[edge + 2].contains("ctrl+a more"), "{screen}");
    // [Home] goes home.
    let column = first.find("[Home]").expect(&screen);
    let column = first[..column].chars().count() as u16 + 1;
    let effects = view.mouse(
        &state,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row: 1,
            modifiers: KeyModifiers::NONE,
        },
        theme(),
    );
    assert!(matches!(effects.as_slice(), [ChatEffect::Home]));
}

/// A chat like `chat` whose agent works on a branch with changes, with
/// `base` as the branch it left.
fn chat_in_git(items: Vec<Item>, base: Option<&str>) -> SessionState {
    let totals = |files, added, removed| wire::ChangeTotals {
        files,
        added,
        removed,
    };
    let agent = wire::Agent {
        git: Some(wire::Git {
            branch: Some("topic".into()),
            base_branch: base.map(str::to_owned),
            uncommitted: Some(totals(2, 42, 7)),
            on_branch: Some(totals(5, 120, 30)),
        }),
        ..fixtures::agent(Kind::ClaudeSdk)
    };
    let mut state = SessionState::new(agent, crate::chat::layout::CAP as usize);
    state.update(Msg::Connection(ui_state::Connection::Live));
    state.update(snapshot(Phase::Idle, vec![], vec![]));
    for item in items {
        state.update(event(session_event::Of::Item(item)));
    }
    state.update(caught_up(0));
    state
}

/// The header counts the comparison the person chose, from the agent's
/// row: uncommitted changes, or everything since the branch left its base.
#[test]
fn the_header_counts_the_comparison_chosen() {
    let state = chat_in_git(replies(1, 2), Some("main"));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let header = |view: &mut ChatView| feed(view, &state).0.lines().nth(1).unwrap().to_owned();
    assert!(header(&mut view).contains("[Diff +42 −7]"));
    assert!(view.switch_comparison(&state));
    assert!(header(&mut view).contains("[Diff vs main +120 −30]"));
    assert!(view.switch_comparison(&state));
    assert!(header(&mut view).contains("[Diff +42 −7]"));

    // With no base branch there is nothing to count the branch against.
    let state = chat_in_git(replies(1, 2), None);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    assert!(!view.switch_comparison(&state));
    assert_eq!(view.comparison, crate::chat::Comparison::Uncommitted);
}

/// The overview fetches the changed files, without a patch, only while it
/// shows: once per comparison, and again when the row's totals move.
#[test]
fn the_overview_fetches_its_changed_files_once_per_comparison_and_totals() {
    use wire::diff_base::Base;

    use crate::chat::Comparison;
    let mut state = chat_in_git(replies(1, 2), Some("main"));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    assert!(view.wants_changes(&state).is_none(), "the pane is closed");

    view.open_pane();
    let (key, base) = view.wants_changes(&state).expect("asked on opening");
    assert_eq!(key.comparison, Comparison::Uncommitted);
    assert!(matches!(base.base, Some(Base::Head(_))));
    assert!(view.wants_changes(&state).is_none(), "asked once");

    assert!(view.switch_comparison(&state));
    let (key, base) = view.wants_changes(&state).expect("asked for the branch");
    assert_eq!(key.comparison, Comparison::OnBranch);
    assert_eq!(base.base, Some(Base::Branch("main".into())));
    assert!(view.wants_changes(&state).is_none());

    // A turn end moves the branch's totals: the list is asked for again.
    let mut agent = state.agent().clone();
    if let Some(git) = agent.git.as_mut() {
        git.on_branch = Some(wire::ChangeTotals {
            files: 6,
            added: 130,
            removed: 30,
        });
    }
    state.update(Msg::Entry(agent));
    assert!(view.wants_changes(&state).is_some());
}

/// The overview lists each running background job with its command and
/// running time, the fetched files under their folders, and every usage
/// window with its name, fullness, state and reset.
#[test]
fn the_overview_lists_jobs_files_by_folder_and_usage_windows() {
    use crate::chat::{Comparison, FetchKey};
    let meter = |used_percent, state: wire::UsageState| {
        Some(wire::UsageMeter {
            used_percent,
            resets_at_ms: None,
            state: state as i32,
        })
    };
    let body = wire::ClaudeSdkSnapshot {
        background_jobs: Some(wire::BackgroundJobs {
            known: true,
            jobs: vec![wire::BackgroundJob {
                item_key: "k2".into(),
                command: "npm run dev".into(),
                started_at_ms: 0,
                ..Default::default()
            }],
        }),
        usage: Some(wire::ClaudeUsage {
            state: wire::UsageState::NearLimit as i32,
            windows: vec![
                wire::ClaudeUsageWindow {
                    limit: wire::Limit::FiveHour as i32,
                    meter: meter(91.0, wire::UsageState::NearLimit),
                    ..Default::default()
                },
                wire::ClaudeUsageWindow {
                    limit: wire::Limit::Weekly as i32,
                    model: Some("Fable".into()),
                    meter: meter(30.0, wire::UsageState::Ok),
                    ..Default::default()
                },
            ],
        }),
        ..Default::default()
    }
    .encode_to_vec();
    let mut state = chat_in_git(replies(1, 2), Some("main"));
    state.update(snapshot(Phase::Idle, body, vec![]));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.open_pane();
    let file = |path: &str| wire::DiffFile {
        path: path.into(),
        added: 1,
        ..Default::default()
    };
    view.changes_fetched(
        FetchKey {
            comparison: Comparison::Uncommitted,
            totals: None,
        },
        wire::Diff {
            files: vec![file("src/b.rs"), file("README.md"), file("src/a.rs")],
            ..Default::default()
        },
    );
    let (buffer, _) = draw(&mut view, &state, 5 * 60_000, 140, 40, theme());
    let screen = text(&buffer);
    let pane: Vec<String> = screen
        .lines()
        .filter_map(|line| line.split_once('│').map(|(_, pane)| pane.trim().to_owned()))
        .filter(|line| !line.is_empty())
        .collect();
    let at = |words: &str| {
        pane.iter()
            .position(|line| line.starts_with(words))
            .unwrap_or_else(|| panic!("{words:?} not in the pane:\n{screen}"))
    };
    assert!(pane[at("npm run dev")].ends_with("5m"), "{screen}");
    // Root files first, then each folder with its files by name.
    assert!(at("README.md") < at("src/"));
    assert!(at("src/") < at("a.rs") && at("a.rs") < at("b.rs"));
    assert!(pane[at("5-hour limit")].ends_with("91% used"), "{screen}");
    assert_eq!(pane[at("5-hour limit") + 1], "near");
    assert!(
        pane[at("Fable weekly limit")].ends_with("30% used"),
        "{screen}"
    );
    assert_eq!(pane[at("Fable weekly limit") + 1], "fine");
}

/// A background command reads as code; the provider's own subagent or
/// monitor, named by a description, reads as words after a tag saying
/// which it is.
#[test]
fn the_overview_tags_a_subagent_and_a_monitor_and_shows_a_command_as_code() {
    let job = |command: &str, kind: wire::BackgroundKind| wire::BackgroundJob {
        command: command.into(),
        kind: kind as i32,
        ..Default::default()
    };
    let body = wire::ClaudeSdkSnapshot {
        background_jobs: Some(wire::BackgroundJobs {
            known: true,
            jobs: vec![
                job("npm run dev", wire::BackgroundKind::Shell),
                job("Review the auth module", wire::BackgroundKind::Subagent),
                job("build log", wire::BackgroundKind::Monitor),
            ],
        }),
        ..Default::default()
    }
    .encode_to_vec();
    let mut state = chat_in_git(replies(1, 2), Some("main"));
    state.update(snapshot(Phase::Idle, body, vec![]));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.open_pane();
    let (buffer, _) = draw(&mut view, &state, 3 * 60_000, 140, 40, theme());
    let screen = text(&buffer);
    let line = |words: &str| {
        screen
            .lines()
            .filter_map(|line| line.split_once('│').map(|(_, pane)| pane.trim()))
            .find(|pane| pane.contains(words))
            .unwrap_or_else(|| panic!("{words:?} not in the pane:\n{screen}"))
            .to_owned()
    };
    assert!(line("npm run dev").starts_with("npm run dev"), "{screen}");
    assert!(
        line("Review the auth").starts_with("subagent  Review the auth module"),
        "{screen}"
    );
    assert!(
        line("build log").starts_with("monitor  build log"),
        "{screen}"
    );
    // The command in code's style, the descriptions in the text's.
    let style_of = |words: &str| {
        let (x, y) = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .find(|&(x, y)| {
                let row: String = (x..buffer.area.width)
                    .take(words.len())
                    .map(|x| buffer[(x, y)].symbol().to_owned())
                    .collect();
                row == words
            })
            .unwrap_or_else(|| panic!("{words:?} not drawn"));
        buffer[(x, y)].style().fg
    };
    let theme = theme();
    assert_eq!(style_of("npm run dev"), theme.code().fg);
    assert_eq!(style_of("Review the auth module"), theme.text().fg);
    assert_ne!(theme.code().fg, theme.text().fg);
}

// --- the chat: keys, the pinned prompt -------------------------------------

/// Your message at `order`, as the agent reflected it.
fn prompt_item(order: u64, words: &str) -> Item {
    use wire::claude_sdk_item::Kind as K;
    Item {
        key: format!("k{order}"),
        order,
        revision: order,
        text: words.into(),
        kind: Kind::ClaudeSdk as i32,
        body: wire::ClaudeSdkItem {
            kind: Some(K::Prompt(wire::Prompt {})),
        }
        .encode_to_vec(),
        at_ms: order as i64 * 1_000,
        ..Item::default()
    }
}

/// The key line: the screen's last line with anything on it.
fn keys_of(screen: &str) -> String {
    screen
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn the_keys_under_the_composer_follow_what_is_happening() {
    let mut state = chat(replies(1, 3));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    // Idle: nothing everyone knows, and always the way to more.
    let keys = keys_of(&feed(&mut view, &state).0);
    assert!(keys.trim_end().ends_with("ctrl+a more"), "{keys}");
    assert!(!keys.contains("enter"), "{keys}");
    assert!(
        !keys.contains("attach") && !keys.contains("review"),
        "{keys}"
    );
    // Working: Enter queues, and interrupting is at hand.
    state.update(snapshot(Phase::Working, vec![], vec![]));
    let keys = keys_of(&feed(&mut view, &state).0);
    assert!(keys.contains("enter queue"), "{keys}");
    assert!(keys.contains("ctrl+x interrupt"), "{keys}");
    // Exited: Enter resumes.
    let mut exited = fixtures::agent(Kind::ClaudeSdk);
    exited.lifecycle = wire::Lifecycle::Exited as i32;
    exited.exit_cause = wire::ExitCause::Finished as i32;
    state.update(Msg::Entry(exited));
    let keys = keys_of(&feed(&mut view, &state).0);
    assert!(keys.contains("enter resume"), "{keys}");
    assert!(keys.trim_end().ends_with("ctrl+a more"), "{keys}");
}

#[test]
fn the_prompt_of_the_turn_at_the_top_is_pinned_under_the_header() {
    let mut items = vec![prompt_item(1, "first question")];
    items.extend(replies(2, 30));
    items.push(prompt_item(31, "second question"));
    items.extend(replies(32, 60));
    let state = chat(items);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let pinned = |view: &mut ChatView, state: &SessionState| {
        let (screen, _) = feed(view, state);
        // A blank line, the header, a blank line, then the pin.
        screen.lines().nth(3).unwrap_or_default().to_owned()
    };
    // Deep in the first turn: its prompt is pinned.
    view.anchor = crate::chat::layout::Anchor::Top {
        key: "k12".into(),
        offset: 0,
    };
    let line = pinned(&mut view, &state);
    assert!(line.contains("first question"), "{line}");
    // Deep in the second: the second's.
    view.anchor = crate::chat::layout::Anchor::Top {
        key: "k45".into(),
        offset: 0,
    };
    let line = pinned(&mut view, &state);
    assert!(line.contains("second question"), "{line}");
    // Clicking the pin scrolls to the prompt itself.
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    view.mouse(
        &state,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 3,
            modifiers: KeyModifiers::NONE,
        },
        theme(),
    );
    assert_eq!(
        view.anchor,
        crate::chat::layout::Anchor::Top {
            key: "k31".into(),
            offset: 0
        }
    );
    // With the prompt's own block on screen below the top, nothing is
    // pinned: it is its own landmark.
    let (screen, _) = feed(&mut view, &state);
    assert_eq!(screen.matches("second question").count(), 1, "{screen}");
}

#[test]
fn scrolled_back_offers_the_way_to_the_newest() {
    let state = chat(replies(1, 60));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.anchor = crate::chat::layout::Anchor::Top {
        key: "k20".into(),
        offset: 0,
    };
    let (screen, _) = feed(&mut view, &state);
    let row = screen
        .lines()
        .position(|line| line.contains("Jump to Bottom"))
        .expect(&screen);
    let line = screen.lines().nth(row).unwrap_or_default();
    let column = line.find("Jump").expect(&screen);
    let column = line[..column].chars().count() as u16;
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    view.mouse(
        &state,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row: row as u16,
            modifiers: KeyModifiers::NONE,
        },
        theme(),
    );
    assert_eq!(view.anchor, crate::chat::layout::Anchor::Bottom);
}

/// Two questions as headless Claude asks them: pick one, then pick several.
fn two_questions() -> ui_view::AskCard {
    let option = |label: &str| ui_view::OptionView {
        label: label.into(),
        description: String::new(),
        preview: String::new(),
        recommended: false,
    };
    let question = |header: &str, multi_select| ui_view::QuestionView {
        header: header.into(),
        question: format!("{header}?"),
        multi_select,
        options: vec![option("one"), option("two"), option("three")],
        allow_other: true,
        secret: false,
    };
    ui_view::AskCard {
        kind: Kind::ClaudeSdk,
        key: "ask".into(),
        item_key: "k".into(),
        position: 1,
        count: 1,
        body: AskBody::Question(vec![
            question("Rollout", false),
            question("Platforms", true),
        ]),
        choices: vec![],
        question_note: true,
        question_skip: true,
        question_reply: true,
        stops_turn: true,
        state: CardState::Open,
    }
}

/// Presses `keys` on the card's box; the answer the last one sent.
fn pressed(card: &ui_view::AskCard, keys: &[KeyEvent]) -> Option<wire::ClaudeAnswer> {
    let mut ask = crate::chat::ask::AskUi::default();
    ask.sync(card);
    let mut sent = None;
    for key in keys {
        if let crate::chat::ask::AskAction::Answer(input) = ask.box_key(card, *key) {
            sent = Some(input);
        }
    }
    let input = sent?;
    match input.decode_body() {
        Some(wire::KindInput::ClaudeSdk(wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Answer(answer)),
        }))
        | Some(wire::KindInput::ClaudePty(wire::ClaudePtyInput {
            of: Some(wire::claude_pty_input::Of::Answer(answer)),
        })) => Some(answer),
        other => panic!("{other:?}"),
    }
}

fn chars(words: &str) -> Vec<KeyEvent> {
    words.chars().map(|c| key(KeyCode::Char(c))).collect()
}

#[test]
fn a_question_is_noted_with_tab_and_the_next_skipped() {
    let card = two_questions();
    // Tab opens the first question's note; Enter answers with the
    // highlighted option and the note. On the second, Esc then ↑ reaches
    // Skip, which leaves it and goes to the review; Enter sends.
    let mut keys = vec![key(KeyCode::Down), key(KeyCode::Tab)];
    keys.extend(chars("behind the flag"));
    keys.extend([
        key(KeyCode::Enter),
        key(KeyCode::Esc),
        key(KeyCode::Up),
        key(KeyCode::Enter),
        key(KeyCode::Enter),
    ]);
    let answer = pressed(&card, &keys).expect("the review sends the answers");
    let Some(wire::claude_answer::Of::Question(answers)) = answer.of else {
        panic!("{answer:?}");
    };
    assert_eq!(
        answers.answers,
        vec![
            wire::QuestionResponse {
                selected: vec![1],
                other: None,
                note: Some("behind the flag".into()),
            },
            wire::QuestionResponse::default(),
        ]
    );
}

#[test]
fn a_question_is_replied_to_instead_with_what_was_answered_so_far() {
    let card = two_questions();
    // The first answered; on the second, Esc points at Reply instead and
    // Enter opens it to type the words.
    let mut keys = vec![key(KeyCode::Enter), key(KeyCode::Esc), key(KeyCode::Enter)];
    keys.extend(chars("let's talk first"));
    keys.push(key(KeyCode::Enter));
    let answer = pressed(&card, &keys).expect("the reply is sent");
    let Some(wire::claude_answer::Of::Reply(reply)) = answer.of else {
        panic!("{answer:?}");
    };
    assert_eq!(reply.text, "let's talk first");
    assert_eq!(
        reply.answers_so_far,
        vec![
            wire::QuestionResponse {
                selected: vec![0],
                ..Default::default()
            },
            wire::QuestionResponse::default(),
        ]
    );
}

/// What the composer's box shows for `card`.
fn box_screen(card: &ui_view::AskCard) -> String {
    let mut ask = crate::chat::ask::AskUi::default();
    ask.sync(card);
    ask.box_lines(card, 100, theme())
        .lines
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.content.to_string()))
        .collect()
}

#[test]
fn terminal_claude_offers_skip_only_on_a_form_of_several_questions() {
    // Claude's form asking several questions ends in its review screen,
    // which submits with one unanswered; a lone question is submitted by
    // answering it. Neither has a place for a note.
    let open = |count: usize| {
        move |state: &SessionState| {
            ask_card(state).is_some_and(|card| {
                card.state == CardState::Open
                    && matches!(&card.body, AskBody::Question(questions) if questions.len() == count)
            })
        }
    };
    let (state, _) = fixtures::frame_where(Kind::ClaudePty, "question_skip", open(3));
    let card = ask_card(&state).expect("three questions");
    assert!(card.question_skip && !card.question_note);
    let screen = box_screen(&card);
    assert!(screen.contains("Skip"), "{screen}");
    assert!(screen.contains("Reply instead"), "{screen}");
    // Blue on the first; on the second, Esc then ↑ reaches Skip; Small on
    // the third; the review sends.
    let keys = [
        key(KeyCode::Down),
        key(KeyCode::Enter),
        key(KeyCode::Esc),
        key(KeyCode::Up),
        key(KeyCode::Enter),
        key(KeyCode::Enter),
        key(KeyCode::Enter),
    ];
    let answer = pressed(&card, &keys).expect("the review sends the answers");
    let Some(wire::claude_answer::Of::Question(answers)) = answer.of else {
        panic!("{answer:?}");
    };
    let picked = |index| wire::QuestionResponse {
        selected: vec![index],
        ..Default::default()
    };
    assert_eq!(
        answers.answers,
        vec![picked(1), wire::QuestionResponse::default(), picked(0)]
    );

    let (state, _) = fixtures::frame_where(Kind::ClaudePty, "question_skip", open(1));
    let card = ask_card(&state).expect("a lone question");
    assert!(!card.question_skip && !card.question_note);
    let screen = box_screen(&card);
    assert!(!screen.contains("Skip"), "{screen}");
    assert!(screen.contains("Reply instead"), "{screen}");
}

#[test]
fn a_plan_reads_under_its_heading_and_folds_once_decided() {
    // For each kind, the plan fixture drawn while its last plan waits on
    // the person (its text in the feed, the decision in the composer's
    // box) and once it is decided (folded to its outcome).
    for (kind, name) in [
        (Kind::ClaudeSdk, "claude_sdk"),
        (Kind::ClaudePty, "claude_pty"),
        (Kind::Codex, "codex"),
    ] {
        let frames = fixtures::frames(kind, "plans");
        let plan_card = |state: &SessionState| {
            ask_card(state).filter(|card| {
                matches!(card.body, AskBody::Plan { ref title, ref body } if title.is_some() || !body.is_empty())
                    && card.state == CardState::Open
            })
        };
        // The fixture's last plan: an earlier one may have been dismissed.
        let open = frames
            .iter()
            .rposition(|(_, state, _)| plan_card(state).is_some())
            .unwrap_or_else(|| panic!("{name}: no plan waits on the person"));
        let decided = frames[open..]
            .iter()
            .position(|(_, state, _)| ask_card(state).is_none())
            .map(|after| open + after)
            .unwrap_or_else(|| panic!("{name}: the plan is never decided"));
        for (at, when) in [(open, "open"), (decided, "decided")] {
            let (_, state, now) = &frames[at];
            let mut view = ChatView::new(b"agent".to_vec(), *now, false);
            let (buffer, _) = draw(&mut view, state, *now, W, H, theme());
            let screen = text(&buffer);
            assert!(screen.contains("Plan"), "{name} {when}:\n{screen}");
            // Codex proposes its plan as its turn ends, so there is no turn
            // to interrupt; Claude asks mid-turn.
            if when == "open" {
                assert_eq!(
                    screen.contains("ctrl+x interrupt"),
                    kind != Kind::Codex,
                    "{name}:\n{screen}"
                );
            }
            fixtures::assert_frame_golden(&format!("frame_plan_{name}_{when}"), &buffer, theme());
        }
    }
}

// --- keys pressed just as an ask appears --------------------------------------

/// An ask arms only after half a second with no key pressed: every key,
/// before or after the ask is drawn, restarts the window. A person typing
/// steadily as an ask opens has not read it, so their digits and their
/// Enter stay the draft's; once they have stopped for half a second, a
/// digit answers. (Round one's window ran from the ask's first draw, so a
/// tester typing a key every 300 ms to 1.5 s answered asks with the digits
/// of a sentence.)
#[test]
fn keys_typed_steadily_as_an_ask_appears_stay_in_the_draft_until_half_a_second_of_quiet() {
    let idle = chat(vec![]);
    let (asking, at) = fixtures::Named::ClaudePermissionAsk.state();
    let mut view = ChatView::new(b"agent".to_vec(), at - 900, false);
    // Typing before the ask, one key every 300 ms.
    for (i, c) in "I h".chars().enumerate() {
        view.key_at(
            &idle,
            key(KeyCode::Char(c)),
            theme(),
            at - 900 + 300 * i as i64,
        );
    }
    draw(&mut view, &asking, at, W, H, theme());
    // Still typing across the ask's opening, every 300 ms: at its draw,
    // then 300, 600 and 900 ms after it, so the last two are past half a
    // second from the draw but never half a second after a key.
    let mut answered = Vec::new();
    for (i, c) in ['a', '2', '3', '1'].into_iter().enumerate() {
        let now = at + 300 * i as i64;
        draw(&mut view, &asking, now, W, H, theme());
        answered.extend(
            view.key_at(&asking, key(KeyCode::Char(c)), theme(), now)
                .into_iter()
                .filter(|effect| matches!(effect, ChatEffect::Answer(_))),
        );
    }
    let now = at + 1_200;
    draw(&mut view, &asking, now, W, H, theme());
    answered.extend(
        view.key_at(&asking, key(KeyCode::Enter), theme(), now)
            .into_iter()
            .filter(|effect| matches!(effect, ChatEffect::Answer(_))),
    );
    assert!(
        answered.is_empty(),
        "keys typed every 300 ms answered the ask: {answered:?}"
    );
    assert_eq!(
        view.editor.text(),
        "I ha231",
        "the keys typed across the ask's opening are the draft's"
    );
    // Half a second of quiet: the ask is armed and a digit answers.
    let now = at + 1_200 + 500;
    draw(&mut view, &asking, now, W, H, theme());
    let effects = view.key_at(&asking, key(KeyCode::Char('1')), theme(), now);
    assert!(
        matches!(effects.as_slice(), [ChatEffect::Answer(_)]),
        "after half a second with no key a digit answers: {effects:?}"
    );
}

// --- focus on home's controls --------------------------------------------------

/// Focus on "+ New Agent" or a section heading carries the same › marker as
/// a focused agent row, so a keyboard user can see where Enter will land:
/// on a control it starts an agent or folds a section.
#[test]
fn a_focused_control_on_home_carries_the_agent_rows_marker() {
    let fleet = home_fleet();
    let mut home = crate::home::Home::default();
    home_screen(&mut home, &fleet, theme());
    let focused = |screen: &str, words: &str| -> String {
        screen
            .lines()
            .find(|line| line.contains(words))
            .unwrap_or_else(|| panic!("no {words} on\n{screen}"))
            .to_owned()
    };
    home.key(&fleet, key(KeyCode::Home), false);
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(
        screen.contains("enter start"),
        "Home puts the keys on + New Agent: {screen}"
    );
    let new_agent = focused(&screen, "New Agent");
    home.key(&fleet, key(KeyCode::Down), false);
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(
        screen.contains("enter hide"),
        "Down from + New Agent lands on the first heading: {screen}"
    );
    let heading = focused(&screen, "Needs you");
    assert!(
        new_agent.contains('›') && heading.contains('›'),
        "a focused control has no marker: {new_agent:?} and {heading:?}"
    );
}
