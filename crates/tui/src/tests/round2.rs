//! Failing tests from the second adversarial QA run's triage: the terminal
//! client's own layer, each showing what a tester met. Every test reads
//! what a person would see or what the client would send.

use ui_view::AskBody;

use super::*;

/// The whole screen drawn for a chat, one string per row.
fn screen_of(
    view: &mut ChatView,
    state: &SessionState,
    at: i64,
    width: u16,
    height: u16,
) -> String {
    let (buffer, _) = draw(view, state, at, width, height, theme());
    text(&buffer)
}

/// A finished agent reply of exactly `words`.
fn said(order: u64, words: &str) -> Item {
    use wire::claude_sdk_item::Kind as K;
    Item {
        key: format!("r{order}"),
        order,
        revision: order,
        text: words.into(),
        kind: Kind::ClaudeSdk as i32,
        body: wire::ClaudeSdkItem {
            kind: Some(K::Message(wire::Text { complete: true })),
        }
        .encode_to_vec(),
        at_ms: order as i64 * 1_000,
        ..Item::default()
    }
}

/// A prompt the person sent, of exactly `words`.
fn asked(order: u64, words: &str) -> Item {
    use wire::claude_sdk_item::Kind as K;
    Item {
        key: format!("p{order}"),
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

/// A Claude question with a 300-character sentence and four options of
/// about sixty characters, each with a description, as a tester's agent
/// asked it.
fn long_question() -> SessionState {
    let option = |label: &str| wire::QuestionOption {
        label: format!("{label} with a label that runs to about sixty characters"),
        description: "One sentence that explains what choosing this option means for the team."
            .into(),
        ..Default::default()
    };
    let body = wire::ClaudeSdkSnapshot {
        asks: vec![wire::Ask {
            key: "q-1".into(),
            item_key: "call-q".into(),
            body: Some(wire::ask::Body::Question(wire::QuestionAsk {
                questions: vec![wire::Question {
                    header: "Database".into(),
                    question: "Our small team of five needs to pick a database for an internal tool \
                               that stores a few hundred thousand rows, runs on one modest server, \
                               must be backed up nightly, and should not need a dedicated person to \
                               look after it; which of these should we choose for the first year?"
                        .into(),
                    options: vec![
                        option("Managed PostgreSQL"),
                        option("SQLite embedded"),
                        option("MongoDB Atlas"),
                        option("Self-hosted MySQL"),
                    ],
                    allow_other: true,
                    ..Default::default()
                }],
                ..Default::default()
            })),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut state = chat(replies(1, 3));
    state.update(snapshot(Phase::NeedsYou, body.encode_to_vec(), vec![]));
    state
}

/// A question's free-text row, its reply-instead row and its key line stay
/// on a short screen, as round one made a permission ask's choices stay:
/// at 80x24 four long options pushed all three off the bottom, and the
/// highlight could move onto rows that were not drawn.
#[test]
fn a_long_questions_way_out_and_keys_stay_on_a_short_screen() {
    let state = long_question();
    assert!(
        matches!(
            ask_card(&state).map(|card| card.body),
            Some(AskBody::Question(_))
        ),
        "the question is open"
    );
    for (width, height) in [(80, 24), (60, 18)] {
        let mut view = ChatView::new(b"agent".to_vec(), 0, false);
        let screen = screen_of(&mut view, &state, 0, width, height);
        for words in ["Something else", "Reply instead", "enter"] {
            assert!(
                screen.contains(words),
                "at {width}x{height} {words:?} is off the screen:\n{screen}"
            );
        }
        // The highlight walks to the last row; it stays on the screen.
        for _ in 0..6 {
            view.key(&state, key(KeyCode::Down), theme());
        }
        let screen = screen_of(&mut view, &state, 0, width, height);
        assert!(
            screen.contains('›'),
            "at {width}x{height} the highlight moved off the screen:\n{screen}"
        );
    }
}

/// Paging through a long answer shows every line: the pinned prompt at the
/// top covered the first row after each page step, so at 80x24 every
/// fourteenth line was never on any page.
#[test]
fn paging_down_a_long_answer_skips_no_line() {
    let body: Vec<String> = (1..=120).map(|n| format!("line {n:03}")).collect();
    let state = chat(vec![
        asked(1, "Print the numbers one per line."),
        said(2, &body.join("\n")),
    ]);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    screen_of(&mut view, &state, 0, 80, 24);
    view.key(
        &state,
        KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL),
        theme(),
    );
    let mut seen = HashSet::new();
    for _ in 0..40 {
        let screen = screen_of(&mut view, &state, 0, 80, 24);
        for line in screen.lines() {
            if let Some(at) = line.find("line ")
                && let Ok(n) = line[at + 5..].trim().get(..3).unwrap_or("").parse::<u32>()
            {
                seen.insert(n);
            }
        }
        view.key(&state, key(KeyCode::PageDown), theme());
    }
    let missed: Vec<u32> = (1..=120).filter(|n| !seen.contains(n)).collect();
    assert!(
        missed.is_empty(),
        "these lines were on no page while paging down: {missed:?}"
    );
}

/// A combining accent typed into the composer is drawn there, as the sent
/// message draws it: the composer drew "cafe" for "café" and the cursor
/// stepped over the invisible mark.
#[test]
fn the_composer_draws_a_combining_accent() {
    let state = chat(replies(1, 2));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    typed(&mut view, &state, "cafe\u{301} ok");
    assert_eq!(
        view.editor.text(),
        "cafe\u{301} ok",
        "the draft holds the accent"
    );
    let screen = screen_of(&mut view, &state, 0, 80, 24);
    assert!(
        screen.contains("cafe\u{301} ok"),
        "the composer drew the accent away:\n{screen}"
    );
}

/// Backspace removes the character the person sees, not one code point of
/// it: an emoji with a skin tone took two presses, the second invisible.
#[test]
fn backspace_removes_a_whole_emoji() {
    let state = chat(replies(1, 2));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    typed(&mut view, &state, "a\u{1F44D}\u{1F3FD}");
    view.key(&state, key(KeyCode::Backspace), theme());
    assert_eq!(
        view.editor.text(),
        "a",
        "one Backspace left part of the emoji behind"
    );
}

/// A tab in an agent's code block keeps its words apart: it was drawn as
/// nothing, so "tab\there" read "tabhere" (a Makefile or Go code loses its
/// indentation the same way).
#[test]
fn a_tab_in_an_agents_code_block_keeps_the_words_apart() {
    let state = chat(vec![said(1, "Here:\n\n```\ntab\there\n  two spaces\n```")]);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let screen = screen_of(&mut view, &state, 0, 80, 24);
    assert!(
        !screen.contains("tabhere") && screen.contains("tab") && screen.contains("here"),
        "the tab glued the words together:\n{screen}"
    );
}

/// A prompt of only spaces is not sent: it spent a turn on an empty
/// message ("Your message came through empty").
#[test]
fn a_prompt_of_only_spaces_is_not_sent() {
    let state = chat(replies(1, 2));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    typed(&mut view, &state, "   ");
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, ChatEffect::Prompt { .. })),
        "a prompt of only spaces was sent: {effects:?}"
    );
}

/// The settings key (Ctrl+S) takes m, e or p; any other key goes on to the
/// draft. It swallowed the next key, so the "/" of a "/model sonnet" typed
/// after reading its hint was lost and the words went to the model.
#[test]
fn a_key_after_the_settings_key_that_is_no_setting_reaches_the_draft() {
    let state = chat(replies(1, 2));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.key(&state, ctrl('s'), theme());
    typed(&mut view, &state, "/model sonnet");
    assert_eq!(
        view.editor.text(),
        "/model sonnet",
        "the key after the settings key was swallowed"
    );
}

/// While an ask holds the composer's box, the settings key opens nothing
/// that is not drawn: a model picker opened under the ask took the arrows
/// and Enter, and changed the model blindly.
#[test]
fn the_settings_key_under_an_ask_opens_no_hidden_picker() {
    let (state, at) = fixtures::Named::ClaudePermissionAsk.state();
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    screen_of(&mut view, &state, at, W, H);
    view.key(&state, ctrl('s'), theme());
    view.key(&state, key(KeyCode::Char('m')), theme());
    let screen = screen_of(&mut view, &state, at, W, H);
    view.key(&state, key(KeyCode::Down), theme());
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    let changed_model = matches!(
        sent(&effects),
        Some(wire::KindInput::ClaudeSdk(wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Model(_)),
        }))
    );
    assert!(
        !changed_model,
        "keys under the ask changed the model through a picker not on the screen:\n{screen}"
    );
}

// --- home ----------------------------------------------------------------

fn home_at(home: &mut crate::home::Home, fleet: &FleetState, width: u16, height: u16) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
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
                &HOME_PLACE,
            );
        })
        .unwrap();
    text(terminal.backend().buffer())
}

/// After a filter narrows home to one agent, Enter opens that agent: the
/// highlight stayed on "+ New Agent" above the match, so `/ name Enter
/// Enter` started a new agent.
#[test]
fn enter_after_a_filter_opens_the_agent_it_found() {
    let fleet = home_fleet();
    let mut home = crate::home::Home::default();
    home_at(&mut home, &fleet, W, H);
    home.key(&fleet, key(KeyCode::Home), false);
    home.key(&fleet, key(KeyCode::Char('/')), false);
    for c in "alpha".chars() {
        home.key(&fleet, key(KeyCode::Char(c)), false);
    }
    home.key(&fleet, key(KeyCode::Enter), false);
    let effects = home.key(&fleet, key(KeyCode::Enter), false);
    let screen = home_at(&mut home, &fleet, W, H);
    assert!(
        matches!(effects.as_slice(), [FleetEffect::Open(agent)] if agent.agent == b"w1"),
        "Enter after filtering to alpha did {effects:?}:\n{screen}"
    );
}

/// The stop window's two buttons answer the keys that move between them:
/// Right then Enter stopped (or deleted) the agent although the person had
/// moved to [Cancel].
#[test]
fn moving_to_cancel_in_the_stop_window_cancels() {
    let fleet = home_fleet();
    let mut home = crate::home::Home::default();
    home_at(&mut home, &fleet, W, H);
    home.key(&fleet, key(KeyCode::Char('/')), false);
    for c in "alpha".chars() {
        home.key(&fleet, key(KeyCode::Char(c)), false);
    }
    home.key(&fleet, key(KeyCode::Enter), false);
    home.key(&fleet, key(KeyCode::Down), false);
    home.key(&fleet, key(KeyCode::Char('s')), false);
    let screen = home_at(&mut home, &fleet, W, H);
    assert!(
        screen.contains("Cancel"),
        "the stop window is open:\n{screen}"
    );
    home.key(&fleet, key(KeyCode::Right), false);
    let effects = home.key(&fleet, key(KeyCode::Enter), false);
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, FleetEffect::Stop(_))),
        "Enter on [Cancel] stopped the agent:\n{screen}"
    );
}

/// An emptied rename says why Enter does nothing: the key line kept
/// offering "enter save" while Enter refused the empty name silently.
#[test]
fn an_empty_rename_says_why_enter_does_nothing() {
    let fleet = home_fleet();
    let mut home = crate::home::Home::default();
    home_at(&mut home, &fleet, W, H);
    home.key(&fleet, key(KeyCode::Char('/')), false);
    for c in "alpha".chars() {
        home.key(&fleet, key(KeyCode::Char(c)), false);
    }
    home.key(&fleet, key(KeyCode::Enter), false);
    home.key(&fleet, key(KeyCode::Down), false);
    home.key(&fleet, key(KeyCode::Char('r')), false);
    home.key(&fleet, ctrl('u'), false);
    home.key(&fleet, key(KeyCode::Enter), false);
    let screen = home_at(&mut home, &fleet, W, H);
    assert!(
        !screen.contains("enter save") && screen.contains("empty"),
        "an empty name offers enter save and gives no reason:\n{screen}"
    );
}

/// At 80 columns the new agent's settings line keeps "a agent", the only
/// way to choose Codex, and "esc back", or marks that it cut them.
#[test]
fn the_new_agents_settings_line_keeps_its_keys_at_80_columns() {
    let fleet = home_fleet();
    let mut home = crate::home::Home::default();
    home_at(&mut home, &fleet, 80, 24);
    home.key(&fleet, key(KeyCode::Char('n')), false);
    home_at(&mut home, &fleet, 80, 24);
    home.key(&fleet, ctrl('s'), false);
    let screen = home_at(&mut home, &fleet, 80, 24);
    let line = screen
        .lines()
        .find(|line| line.contains("permission") && line.contains("folder"))
        .unwrap_or_else(|| panic!("no settings line:\n{screen}"));
    assert!(
        (line.contains("a agent") && line.contains("esc back")) || line.contains('…'),
        "the settings line drops keys silently at 80 columns: {line:?}"
    );
}

/// Choosing another host for a new agent does not keep this machine's
/// folder: an agent started on desk from the laptop's form ran in the
/// laptop's path, which on two real machines names nothing, or something
/// else.
#[test]
fn choosing_another_host_drops_this_hosts_folder() {
    let mut setup = crate::setup::Setup::defaults(
        crate::setup::ChatIn::Amux,
        "/Users/me/laptop-project",
        b"\x0a",
        &defaults(),
    );
    setup.pick(crate::setup::Item::Host, "0b");
    assert_eq!(setup.host, b"\x0b".to_vec(), "the host changed");
    assert_ne!(
        setup.folder, "/Users/me/laptop-project",
        "the new host's agent would start in this machine's folder"
    );
}

/// A new-agent form backed out of with nothing typed opens next time with
/// the defaults: a permission cycled to auto and abandoned carried into
/// the next agent.
#[test]
fn a_new_agent_form_left_empty_opens_with_the_defaults_again() {
    let fleet = home_fleet();
    let mut home = crate::home::Home::default();
    home_at(&mut home, &fleet, W, H);
    home.key(&fleet, key(KeyCode::Char('n')), false);
    home_at(&mut home, &fleet, W, H);
    let before = home
        .open_setup(&HOME_PLACE)
        .map(|setup| setup.permission.clone());
    // As Shift+Tab cycling to auto does once the host has said what it offers.
    if let Some(setup) = home.open_setup(&HOME_PLACE) {
        setup.pick(crate::setup::Item::Permission, "auto");
    }
    let cycled = home
        .open_setup(&HOME_PLACE)
        .map(|setup| setup.permission.clone());
    assert_ne!(before, cycled, "the permission moved");
    home.key(&fleet, key(KeyCode::Esc), false);
    home.key(&fleet, key(KeyCode::Char('n')), false);
    let again = home
        .open_setup(&HOME_PLACE)
        .map(|setup| setup.permission.clone());
    assert_eq!(
        again, before,
        "the abandoned form's permission carried into the next new agent"
    );
}

/// Words typed into a chat that closed under the person (its agent was
/// deleted elsewhere) never stop or delete another agent: home took "so
/// sure, yes" as `s` (stop window) then `y` (confirm), and "fix x" + Enter
/// as `x` (delete window) then Enter, so a healthy agent was stopped or
/// deleted by a sentence.
#[test]
fn a_sentence_typed_on_home_stops_and_deletes_nothing() {
    let fleet = home_fleet();
    for words in ["so sure, yes", "fix x\r"] {
        let mut home = crate::home::Home::default();
        home_at(&mut home, &fleet, W, H);
        home.key(&fleet, key(KeyCode::Char('/')), false);
        for c in "alpha".chars() {
            home.key(&fleet, key(KeyCode::Char(c)), false);
        }
        home.key(&fleet, key(KeyCode::Enter), false);
        home.key(&fleet, key(KeyCode::Down), false);
        let mut effects = Vec::new();
        for c in words.chars() {
            let code = if c == '\r' {
                KeyCode::Enter
            } else {
                KeyCode::Char(c)
            };
            effects.extend(home.key(&fleet, key(code), false));
        }
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, FleetEffect::Stop(_) | FleetEffect::Delete(_))),
            "typing {words:?} on home did {effects:?}"
        );
    }
}

// --- attaching -------------------------------------------------------------

/// Enter while a file is still attaching does not send the prompt without
/// it: a 50 MB file's prompt went as plain text, and the chip appeared a
/// moment later in the emptied composer, as if for the next message.
#[test]
fn a_prompt_sent_while_its_file_attaches_waits_for_the_file() {
    let state = chat(replies(1, 3));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big2.bin");
    std::fs::write(&path, b"bytes").unwrap();
    let effect = view.paste(ClipboardContent::Path(path)).unwrap();
    assert!(
        matches!(effect, Some(ChatEffect::Attach { .. })),
        "the file is attaching: {effect:?}"
    );
    typed(&mut view, &state, "what is in this file?");
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    let without_file = effects.iter().any(
        |effect| matches!(effect, ChatEffect::Prompt { attachments, .. } if attachments.is_empty()),
    );
    assert!(
        !without_file,
        "the prompt went while its file was still attaching, without it: {effects:?}"
    );
}
