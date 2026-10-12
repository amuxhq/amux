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
/// moved to [Cancel]. `x` opens the stop window on a live agent.
#[test]
fn moving_to_cancel_in_the_stop_window_cancels() {
    let fleet = home_fleet();
    let mut home = crate::home::Home::default();
    home_at(&mut home, &fleet, W, H);
    focus(&mut home, &fleet, "alpha");
    home.key(&fleet, key(KeyCode::Char('x')), false);
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
            .any(|effect| matches!(effect, FleetEffect::Stop(_) | FleetEffect::Delete(_))),
        "Enter on [Cancel] did {effects:?}:\n{screen}"
    );
}

/// Focuses `name`'s row through the filter.
fn focus(home: &mut crate::home::Home, fleet: &FleetState, name: &str) {
    home.key(fleet, key(KeyCode::Char('/')), false);
    for c in name.chars() {
        home.key(fleet, key(KeyCode::Char(c)), false);
    }
    home.key(fleet, key(KeyCode::Enter), false);
    home.key(fleet, key(KeyCode::Down), false);
}

/// Decided: `x` is home's one key for ending an agent. On a live agent it
/// stops it (the agent goes to Exited, resumable); only an exited agent is
/// deleted. A live agent is never offered deletion: today `x` on a live
/// agent asked "Delete … and its history?".
#[test]
fn x_stops_a_live_agent_and_deletes_only_an_exited_one() {
    let fleet = home_fleet();
    let mut home = crate::home::Home::default();
    home_at(&mut home, &fleet, W, H);
    focus(&mut home, &fleet, "alpha");
    home.key(&fleet, key(KeyCode::Char('x')), false);
    let screen = home_at(&mut home, &fleet, W, H);
    let effects = home.key(&fleet, key(KeyCode::Enter), false);
    assert!(
        matches!(effects.as_slice(), [FleetEffect::Stop(agent)] if agent.agent == b"w1")
            && !screen.contains("Delete"),
        "x on a live agent did {effects:?}:\n{screen}"
    );
    let mut home = crate::home::Home::default();
    home_at(&mut home, &fleet, W, H);
    focus(&mut home, &fleet, "archive");
    home.key(&fleet, key(KeyCode::Char('x')), false);
    let effects = home.key(&fleet, key(KeyCode::Enter), false);
    assert!(
        matches!(effects.as_slice(), [FleetEffect::Delete(agent)] if agent.agent == b"old"),
        "x on an exited agent did {effects:?}"
    );
}

/// Decided: the stop window names what a stop will cut. An agent that is
/// working says so, and one with sub-agents counts them (background jobs
/// join the sentence once the fleet row carries them), as in "alpha is
/// working … Stop anyway?"; today every stop reads "Stop alpha? It can be
/// resumed later."
#[test]
fn the_stop_window_names_the_turn_and_the_sub_agents_a_stop_cuts() {
    let fleet = home_fleet();
    for (name, words) in [("alpha", "working"), ("planner", "1 sub-agent")] {
        let mut home = crate::home::Home::default();
        home_at(&mut home, &fleet, W, H);
        focus(&mut home, &fleet, name);
        home.key(&fleet, key(KeyCode::Char('x')), false);
        let screen = home_at(&mut home, &fleet, W, H);
        assert!(
            screen.contains("Stop") && screen.contains(words),
            "stopping {name} does not say it cuts {words:?}:\n{screen}"
        );
    }
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

/// Decided: home has no `s` key; `x` (with its confirm) is the one way to
/// end an agent from home. Words typed into a chat that closed under the
/// person ran home's letters: "so sure, yes" was `s` (the stop window)
/// then `y` (confirm), and a healthy agent was stopped by a sentence.
#[test]
fn a_sentence_typed_on_home_stops_nothing() {
    let fleet = home_fleet();
    let mut home = crate::home::Home::default();
    home_at(&mut home, &fleet, W, H);
    focus(&mut home, &fleet, "alpha");
    let mut effects = Vec::new();
    for c in "so sure, yes".chars() {
        effects.extend(home.key(&fleet, key(KeyCode::Char(c)), false));
    }
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, FleetEffect::Stop(_) | FleetEffect::Delete(_))),
        "typing \"so sure, yes\" on home did {effects:?}"
    );
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

// --- decided behaviour: sends while the host is away ----------------------

/// Decided: a prompt typed while the host can't be reached is handed to the
/// client library, which holds it as pending (visible, with Cancel) and
/// sends it when the host is back. Today the composer keeps it as a draft
/// under "Draft kept · sending waits" and nothing ever sends it.
#[test]
fn a_prompt_sent_while_the_host_is_away_is_handed_on_to_be_held() {
    let mut state = chat(replies(1, 5));
    state.update(Msg::Host(wire::HostEntry {
        host_id: b"host".to_vec(),
        name: "studio".into(),
        ..Default::default()
    }));
    state.update(event(session_event::Of::Detached(wire::Detached {})));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    typed(&mut view, &state, "send this when studio is back");
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            ChatEffect::Prompt { text, .. } if text == "send this when studio is back"
        )),
        "Enter while studio is away held nothing to send: {effects:?}, draft {:?}",
        view.editor.text()
    );
}

// --- decided behaviour: commands the client handles -----------------------

/// A headless Claude chat whose catalogue lists models and efforts.
fn claude_with_models() -> SessionState {
    offering(Kind::ClaudeSdk, "recorded_controls", |state| {
        ui_view::settings(state).models.len() > 1
    })
}

fn no_prompt(effects: &[ChatEffect]) -> bool {
    !effects
        .iter()
        .any(|effect| matches!(effect, ChatEffect::Prompt { .. }))
}

/// Decided: "/model <value>" is amux's own command wherever the catalogue
/// lists models: it sets the model through the typed model input and is
/// never sent to the agent. Today it goes to Claude as a prompt.
#[test]
fn slash_model_with_a_value_sets_the_model_and_sends_no_prompt() {
    let state = claude_with_models();
    let other = ui_view::settings(&state)
        .models
        .into_iter()
        .find(|model| !model.current)
        .expect("a model other than the current one");
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    typed(&mut view, &state, &format!("/model {}", other.value));
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    let set = matches!(
        sent(&effects),
        Some(wire::KindInput::ClaudeSdk(wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Model(ref model)),
        })) if model.model.as_deref() == Some(other.value.as_str())
    );
    assert!(
        set && no_prompt(&effects),
        "/model {} did {effects:?}",
        other.value
    );
}

/// Decided: a bare "/model" or "/effort" opens amux's own picker and sends
/// nothing; "/effort <level>" sets the effort through the typed input.
#[test]
fn bare_slash_model_opens_amuxs_picker_and_slash_effort_sets_the_effort() {
    let state = claude_with_models();
    let settings = ui_view::settings(&state);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    typed(&mut view, &state, "/model");
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    let screen = screen_of(&mut view, &state, 0, W, H);
    let listed = settings
        .models
        .iter()
        .filter(|model| screen.contains(&model.display_name))
        .count();
    assert!(
        no_prompt(&effects) && listed > 1,
        "a bare /model did {effects:?} and drew no picker:\n{screen}"
    );
    let Some(effort) = settings.efforts.iter().find(|effort| !effort.current) else {
        return;
    };
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    typed(&mut view, &state, &format!("/effort {}", effort.value));
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    assert!(
        no_prompt(&effects) && sent(&effects).is_some(),
        "/effort {} did {effects:?}",
        effort.value
    );
}

/// Decided: "/rename <name>" is amux's rename, never a prompt.
#[test]
fn slash_rename_is_amuxs_rename_not_a_prompt() {
    let state = claude_with_models();
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    typed(&mut view, &state, "/rename release-notes");
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    assert!(
        no_prompt(&effects),
        "/rename went to the agent: {effects:?}"
    );
}

/// Decided: Codex runs only the commands its catalogue lists (amux's
/// table: /compact, /clear, /review, /init, /plan, and skills); any other
/// "/..." is refused in the composer and never reaches the model, which
/// today answers "/status" with a made-up status.
#[test]
fn a_slash_command_codex_does_not_offer_is_refused_in_the_composer() {
    let state = offering(Kind::Codex, "offered", |_| true);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    typed(&mut view, &state, "/status");
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    assert!(
        no_prompt(&effects) && view.editor.text() == "/status",
        "/status was sent to Codex's model: {effects:?}"
    );
}

// --- decided behaviour: interrupt ----------------------------------------

/// Decided: Ctrl+X ends the turn and the agent stays live, so its hint
/// reads "interrupt"; "stop" now means ending the agent.
#[test]
fn ctrl_x_reads_interrupt_while_the_agent_works() {
    let (state, at) = fixtures::Named::ClaudeWorking.state();
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    let screen = screen_of(&mut view, &state, at, W, H);
    assert!(
        screen.contains("ctrl+x interrupt") && !screen.contains("ctrl+x stop"),
        "the working chat's key line:\n{screen}"
    );
}

// --- remaining terminal causes ---------------------------------------------

/// A multi-select question with nothing ticked: Enter submits the empty
/// pick, as Claude's own Submit does; it did nothing and said nothing.
#[test]
fn enter_on_a_multi_select_with_nothing_ticked_submits_an_empty_pick() {
    let body = wire::ClaudeSdkSnapshot {
        asks: vec![wire::Ask {
            key: "q-1".into(),
            item_key: "call-q".into(),
            body: Some(wire::ask::Body::Question(wire::QuestionAsk {
                questions: vec![wire::Question {
                    header: "Toppings".into(),
                    question: "Which toppings?".into(),
                    options: ["Cheese", "Basil", "Olives"]
                        .map(|label| wire::QuestionOption {
                            label: label.into(),
                            ..Default::default()
                        })
                        .into(),
                    multi_select: true,
                    ..Default::default()
                }],
                ..Default::default()
            })),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut state = chat(replies(1, 2));
    state.update(snapshot(Phase::NeedsYou, body.encode_to_vec(), vec![]));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    screen_of(&mut view, &state, 0, W, H);
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    let screen = screen_of(&mut view, &state, 0, W, H);
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, ChatEffect::Answer(_))),
        "Enter with nothing ticked did {effects:?}:\n{screen}"
    );
}

/// Up in an empty composer with nothing queued recalls this chat's last
/// sent prompt: there was no way to send an earlier prompt again without
/// retyping it.
#[test]
fn up_in_an_empty_composer_recalls_the_last_prompt() {
    let state = chat(vec![
        asked(1, "Run the whole suite and report failures."),
        said(2, "All green."),
    ]);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    screen_of(&mut view, &state, 0, W, H);
    view.key(&state, key(KeyCode::Up), theme());
    assert_eq!(
        view.editor.text(),
        "Run the whole suite and report failures.",
        "Up in the empty composer recalled nothing"
    );
}

/// `<leader> o` with no step focused opens the newest fold on screen: it
/// did nothing, so a keyboard-only person could not open a folded run.
#[test]
fn leader_o_with_nothing_focused_opens_the_newest_fold() {
    let state = chat(vec![
        asked(1, "Read the notes."),
        item(2, Some("/work/alpha-notes.md")),
        item(3, Some("/work/beta-notes.md")),
        said(4, "Read both."),
    ]);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let folded = screen_of(&mut view, &state, 0, W, H);
    assert!(
        !folded.contains("beta-notes.md"),
        "the reads start folded:\n{folded}"
    );
    view.toggle_expanded(&state);
    let screen = screen_of(&mut view, &state, 0, W, H);
    assert!(
        screen.contains("alpha-notes.md") && screen.contains("beta-notes.md"),
        "leader o with nothing focused opened nothing:\n{screen}"
    );
}

/// Decided: an attachment larger than 64 MB (ATTACHMENT_SIZE_LIMIT) is
/// refused before upload in plain words naming the limit; it went up and
/// failed with "h2 protocol error".
#[test]
fn a_file_over_the_attachment_limit_is_refused_naming_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t65m.bin");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(64_000_001).unwrap();
    drop(file);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let result = view.paste(ClipboardContent::Path(path));
    let words = match &result {
        Err(words) | Ok(Some(ChatEffect::Notice(words))) => words.clone(),
        _ => String::new(),
    };
    assert!(
        !matches!(result, Ok(Some(ChatEffect::Attach { .. }))) && words.contains("64 MB"),
        "a file over the limit: {:?}",
        result.map(|effect| effect
            .map(|effect| format!("{effect:?}").chars().take(80).collect::<String>()))
    );
}

/// Decided: choosing a host that is away says why it cannot take a new
/// agent. The host picker listed the away host and ignored its number
/// and Enter without a word, so a tester picking it saw nothing happen.
#[test]
fn choosing_an_away_host_for_a_new_agent_says_why() {
    let mut fleet = home_fleet();
    inventory(
        &mut fleet,
        host(b"d", "desk", wire::Trust::Trusted, wire::Presence::Away),
    );
    let mut home = crate::home::Home::default();
    home_at(&mut home, &fleet, W, H);
    home.key(&fleet, key(KeyCode::Char('n')), false);
    home_at(&mut home, &fleet, W, H);
    home.key(&fleet, ctrl('s'), false);
    home.key(&fleet, key(KeyCode::Char('h')), false);
    let before = home_at(&mut home, &fleet, W, H);
    assert!(
        before.contains("2. desk"),
        "the away host is listed second:\n{before}"
    );
    home.key(&fleet, key(KeyCode::Char('2')), false);
    let after = home_at(&mut home, &fleet, W, H);
    let host = home.open_setup(&HOME_PLACE).map(|setup| setup.host.clone());
    assert_eq!(host, Some(b"a".to_vec()), "the agent stays on studio");
    assert_ne!(
        before, after,
        "picking the away host desk changed nothing on screen and said nothing:\n{after}"
    );
}

/// `a` on an exited agent's home row says it is stopped, as the headless
/// and far-host refusals say why, instead of trying its terminal and
/// failing with the socket's path ("attach failed: attaching to pt's
/// terminal at /Users/…").
#[test]
fn raw_attach_to_a_stopped_agent_says_it_is_stopped() {
    let config = crate::TuiConfig {
        working_dir: ".".into(),
        leader: 'a',
        theme: theme(),
        initial_chat: None,
        attach: true,
        version: String::new(),
        local_host: b"a".to_vec(),
        layout: None,
        reports: None,
        chat_in: crate::setup::ChatIn::Amux,
        defaults: defaults(),
    };
    for kind in [Kind::ClaudePty, Kind::Codex] {
        let stopped = wire::Agent {
            host_id: b"a".to_vec(),
            kind: kind as i32,
            name: "pt".into(),
            lifecycle: wire::Lifecycle::Exited as i32,
            ..Default::default()
        };
        let refusal = crate::app::terminal_refusal(&stopped, &config);
        assert!(
            refusal.is_some_and(|why| why.contains("stopped")),
            "attaching to the stopped {kind:?} agent is not refused as stopped: {refusal:?}"
        );
    }
}

/// A chat reconnecting is this machine's own amux daemon gone (the client
/// talks to no other), so it names this machine, never the agent's host:
/// with laptop's daemon killed, a chat with an agent on desk said
/// "reconnecting to desk" while desk was fine.
#[test]
fn a_chat_reconnecting_to_its_own_daemon_does_not_blame_the_agents_host() {
    let mut state = chat(replies(1, 3));
    state.update(Msg::Host(wire::HostEntry {
        host_id: b"host".to_vec(),
        name: "desk".into(),
        ..Default::default()
    }));
    state.update(Msg::Connection(ui_state::Connection::Reconnecting));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let screen = screen_of(&mut view, &state, 0, W, H);
    assert!(screen.contains("reconnecting"), "{screen}");
    assert!(
        !screen.contains("reconnecting to desk"),
        "this machine's daemon is what went away, but the chat blames desk:\n{screen}"
    );
}
