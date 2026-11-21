//! An ACP agent in the terminal: its asks answered with exactly what it
//! offers, its form and link declined with words instead, and a new agent
//! started as whichever agent the host offers.

use super::*;

/// The ask a recorded ACP session ends up holding, and its time.
fn asking(fixture: &str, pick: impl Fn(&AskBody) -> bool) -> (SessionState, i64) {
    fixtures::frame_where(Kind::Acp, fixture, |state| {
        ask_card(state).is_some_and(|card| card.state == CardState::Open && pick(&card.body))
    })
}

/// The ACP answer an input carries.
fn acp_answer(input: &wire::Input) -> wire::acp_answer::Of {
    let Some(wire::KindInput::Acp(wire::AcpInput {
        of: Some(wire::acp_input::Of::Answer(answer)),
    })) = input.decode_body()
    else {
        panic!("not an ACP answer: {input:?}");
    };
    answer.of.expect("the answer says what it is")
}

/// The box lists the agent's own options, its always-allow named as the
/// agent names it, and the one picked goes back as that option's id.
#[test]
fn an_acp_permission_box_offers_the_agents_options_and_answers_with_one() {
    let (state, at) = asking("opencode-allow_always", |body| {
        matches!(body, AskBody::Command { .. })
    });
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    let (buffer, _) = draw(&mut view, &state, at, W, H, theme());
    let screen = text(&buffer);
    for row in ["1. Yes", "2. Always allow", "3. No"] {
        assert!(screen.contains(row), "{row} missing: {screen}");
    }
    let effects = view.key(&state, key(KeyCode::Char('2')), theme());
    let [ChatEffect::Answer(input)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert_eq!(
        acp_answer(input),
        wire::acp_answer::Of::Option("always".into())
    );
}

/// The refusal takes a note, which goes to the agent as what to do
/// instead.
#[test]
fn an_acp_permission_refused_with_a_note_replies_instead() {
    let (state, at) = asking("opencode-allow_always", |body| {
        matches!(body, AskBody::Command { .. })
    });
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    view.key(&state, key(KeyCode::Esc), theme());
    view.key(&state, key(KeyCode::Tab), theme());
    typed(&mut view, &state, "list only the src folder");
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    let [ChatEffect::Answer(input)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    let wire::acp_answer::Of::Reply(reply) = acp_answer(input) else {
        panic!("{input:?}");
    };
    assert_eq!(reply.text, "list only the src folder");
}

/// A form the agent asks itself is named for the agent; Tab on its Decline
/// opens the words to send instead, which go as the reply.
#[test]
fn an_acp_form_is_the_agents_and_declines_with_words() {
    let (state, at) = asking("agy-question", |body| matches!(body, AskBody::Form { .. }));
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    let (buffer, _) = draw(&mut view, &state, at, W, H, theme());
    let screen = text(&buffer);
    assert!(screen.contains("Antigravity needs details"), "{screen}");
    // Esc reaches the way out.
    view.key(&state, key(KeyCode::Esc), theme());
    let (buffer, _) = draw(&mut view, &state, at, W, H, theme());
    let screen = text(&buffer);
    assert!(
        screen.contains("Decline  esc · tab to say what to do instead"),
        "{screen}"
    );
    view.key(&state, key(KeyCode::Tab), theme());
    typed(&mut view, &state, "ask me in chat");
    let (buffer, _) = draw(&mut view, &state, at, W, H, theme());
    assert!(
        text(&buffer).contains("Decline: ask me in chat"),
        "{}",
        text(&buffer)
    );
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    let [ChatEffect::Answer(input)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    let wire::acp_answer::Of::Reply(reply) = acp_answer(input) else {
        panic!("{input:?}");
    };
    assert_eq!(reply.text, "ask me in chat");
}

/// A link the agent asks to have opened declines with words the same way.
#[test]
fn an_acp_link_declines_with_words() {
    let (state, at) = asking("authored-added_agent", |body| {
        matches!(body, AskBody::Link { .. })
    });
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    view.key(&state, key(KeyCode::Esc), theme());
    view.key(&state, key(KeyCode::Tab), theme());
    typed(&mut view, &state, "use the token in .env");
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    let [ChatEffect::Answer(input)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    let wire::acp_answer::Of::Reply(reply) = acp_answer(input) else {
        panic!("{input:?}");
    };
    assert_eq!(reply.text, "use the token in .env");
}

/// What an ACP agent offers a new agent: models and two modes, neither of
/// them its normal one.
fn acp_offer() -> wire::Catalogue {
    let mode = |value: &str| wire::OfferedMode {
        value: value.into(),
        display_name: value.into(),
        settable: true,
        ..Default::default()
    };
    wire::Catalogue {
        models: vec![wire::OfferedModel {
            value: "big-pickle".into(),
            display_name: "Big Pickle".into(),
            ..Default::default()
        }],
        modes: vec![mode("build"), mode("plan")],
        ..Default::default()
    }
}

/// The host's ACP agents are listed by name beside Claude and Codex, one
/// not signed in with its own words for signing in; picked, it starts as
/// that agent, on its own mode unless the person picks one.
#[test]
fn a_new_agent_can_be_any_agent_the_host_offers() {
    use crate::setup::Item;
    let mut fleet = home_fleet();
    let mut studio = host(b"a", "studio", Reads::Online);
    if let wire::inventory_event::Of::Host(entry) = &mut studio {
        entry.providers.push(crate::sessions::hinted(
            offered("opencode", "OpenCode", Kind::Acp),
            "Run `opencode auth login` in the terminal",
        ));
        entry.providers.push(wire::ProviderOnHost {
            signed_in: false,
            ..crate::sessions::hinted(
                offered("antigravity", "Antigravity", Kind::Acp),
                "Log in with your Google account",
            )
        });
    }
    inventory(&mut fleet, studio);
    let mut home = new_home();
    home.key(&fleet, key(KeyCode::Char('n')));
    home_screen(&mut home, &fleet, theme());
    assert_eq!(
        choice_words(&mut home, &fleet, Item::Kind),
        [
            "Claude ✓",
            "Codex",
            "OpenCode",
            "Antigravity · not signed in · Log in with your Google account"
        ]
    );
    let setup = home.open_setup().unwrap();
    setup.pick(Item::Kind, "opencode");
    setup.offer(b"a", "opencode", &[3; 32], &acp_offer());
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(screen.contains("OpenCode"), "{screen}");
    for c in "tidy up".chars() {
        home.key(&fleet, key(KeyCode::Char(c)));
    }
    let effects = home.key(&fleet, KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
    let [FleetEffect::Start { setup, .. }] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert_eq!(setup.host_kind, Kind::Acp);
    let Some(wire::create_agent_request::Agent::Acp(config)) =
        setup.request(b"id".to_vec(), None).agent
    else {
        panic!("not an ACP create");
    };
    assert_eq!(config.agent, "opencode");
    assert_eq!(config.mode, None, "no mode picked, so the agent's own");

    // Shift+Tab picks a mode, which then goes.
    let mut home = new_home();
    home.key(&fleet, key(KeyCode::Char('n')));
    let setup = home.open_setup().unwrap();
    setup.pick(Item::Kind, "opencode");
    setup.offer(b"a", "opencode", &[3; 32], &acp_offer());
    home.key(&fleet, key(KeyCode::BackTab));
    let mode = home.open_setup().unwrap().mode.clone();
    let Some(wire::create_agent_request::Agent::Acp(config)) = home
        .open_setup()
        .unwrap()
        .request(b"id".to_vec(), None)
        .agent
    else {
        panic!("not an ACP create");
    };
    assert!(mode.is_some(), "Shift+Tab picked a mode");
    assert_eq!(config.mode, mode);
}

/// The form for an agent used in its own terminal lists every agent the
/// host offers, one with no terminal of its own among them: started, it
/// opens in its chat.
#[test]
fn the_own_terminal_form_lists_every_agent_the_host_offers() {
    let mut fleet = home_fleet();
    let mut studio = host(b"a", "studio", Reads::Online);
    if let wire::inventory_event::Of::Host(entry) = &mut studio {
        entry
            .providers
            .push(offered("opencode", "OpenCode", Kind::Acp));
    }
    inventory(&mut fleet, studio);
    let mut home = crate::home::Home::new(crate::home::Place {
        chat_in: settings::ChatInSetting::Native,
        ..home_place()
    });
    home.key(&fleet, key(KeyCode::Char('n')));
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(screen.contains(" Claude   Codex   OpenCode "), "{screen}");
}

/// An image pasted for an agent that takes none is refused at once, in
/// words naming the agent, and nothing is stored.
#[test]
fn an_image_pasted_for_an_agent_that_takes_none_is_refused_at_once() {
    let (mut state, _) = fixtures::frame_where(Kind::Acp, "opencode-prompt", |_| true);
    // The host says this agent takes no images.
    let mut entry = crate::sessions::host_entry(vec![offered("opencode", "OpenCode", Kind::Acp)]);
    entry.providers[0].capabilities = Some(wire::AgentCapabilities::acp(true, false, false));
    state.update(Msg::Host(entry));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shot.png");
    std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
    assert_eq!(
        view.paste(&state, ClipboardContent::Path(path)),
        Err("OpenCode can't take images".to_owned())
    );
    assert!(view.attaching().is_empty());
}

/// A new agent's first message with an image, for an agent its host says
/// takes none, is refused before anything is created or stored, whichever
/// agent was picked after the paste; the draft stays.
#[test]
fn a_new_agents_image_is_refused_before_the_agent_is_made() {
    use crate::setup::Item;
    let mut fleet = home_fleet();
    let mut studio = host(b"a", "studio", Reads::Online);
    if let wire::inventory_event::Of::Host(entry) = &mut studio {
        entry.providers.push(wire::ProviderOnHost {
            capabilities: Some(wire::AgentCapabilities::acp(true, false, false)),
            ..offered("opencode", "OpenCode", Kind::Acp)
        });
    }
    inventory(&mut fleet, studio);
    let mut home = new_home();
    home.key(&fleet, key(KeyCode::Char('n')));
    home_screen(&mut home, &fleet, theme());
    for c in "what is this".chars() {
        home.key(&fleet, key(KeyCode::Char(c)));
    }
    home.draft.editor.attach(wire::Attachment {
        of: Some(wire::attachment::Of::Image(wire::BlobRef {
            hash: vec![7; 32],
            name: "shot.png".into(),
            mime: "image/png".into(),
            size: 8,
        })),
        ..Default::default()
    });
    // Picked after the paste.
    home.open_setup().unwrap().pick(Item::Kind, "opencode");
    let effects = home.key(&fleet, key(KeyCode::Enter));
    assert!(effects.is_empty(), "{effects:?}");
    assert_eq!(home.draft.editor.attachments().len(), 1);
    assert!(home.draft.editor.text().starts_with("what is this"));
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(screen.contains("OpenCode can't take images"), "{screen}");

    // Claude takes images: the same draft starts.
    home.open_setup().unwrap().pick(Item::Kind, "claude");
    let effects = home.key(&fleet, key(KeyCode::Enter));
    assert!(
        matches!(effects.as_slice(), [FleetEffect::Start { .. }]),
        "{effects:?}"
    );
}

/// An ACP agent says how much it may do only through its modes, so what
/// asks for its permission picks its mode: Ctrl+S p in its chat opens its
/// modes, and the one picked goes to the agent as its mode.
#[test]
fn an_acp_agents_permission_key_picks_its_mode() {
    let (state, at) = fixtures::frame_where(Kind::Acp, "agy-prompt", |state| {
        state.phase() == ui_state::PhaseView::Idle
            && ui_view::settings(state).changeable.mode == ui_view::Changeable::Pick
    });
    let mut view = ChatView::new(b"agent".to_vec(), at, false);
    view.key(
        &state,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        theme(),
    );
    view.key(&state, key(KeyCode::Char('p')), theme());
    let (buffer, _) = draw(&mut view, &state, at, W, H, theme());
    let screen = text(&buffer);
    assert!(screen.contains("╭─ Mode"), "{screen}");
    let plan = screen
        .lines()
        .find_map(|line| {
            let at = line.find(". plan")?;
            line[..at].chars().next_back().filter(char::is_ascii_digit)
        })
        .unwrap_or_else(|| panic!("no plan to pick: {screen}"));
    let effects = view.key(&state, key(KeyCode::Char(plan)), theme());
    let [ChatEffect::Answer(input)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    let Some(wire::KindInput::Acp(wire::AcpInput {
        of: Some(wire::acp_input::Of::Mode(mode)),
    })) = input.decode_body()
    else {
        panic!("not an ACP mode: {input:?}");
    };
    assert_eq!(mode.value, "plan");
}

/// New Agent for an ACP agent: Ctrl+S p opens the agent's modes, which
/// are what it has in place of permissions.
#[test]
fn a_new_acp_agents_permission_key_picks_its_mode() {
    use crate::setup::Item;
    let mut fleet = home_fleet();
    let mut studio = host(b"a", "studio", Reads::Online);
    if let wire::inventory_event::Of::Host(entry) = &mut studio {
        entry
            .providers
            .push(offered("opencode", "OpenCode", Kind::Acp));
    }
    inventory(&mut fleet, studio);
    let mut home = new_home();
    home.key(&fleet, key(KeyCode::Char('n')));
    let setup = home.open_setup().unwrap();
    setup.pick(Item::Kind, "opencode");
    setup.offer(b"a", "opencode", &[3; 32], &acp_offer());
    home.key(
        &fleet,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
    );
    home.key(&fleet, key(KeyCode::Char('p')));
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(screen.contains("╭─ Mode"), "{screen}");
    assert!(screen.contains("plan"), "{screen}");
}

/// New Agent on a host that knows no models for the agent yet (an ACP
/// agent before its first conversation there): Ctrl+S offers no model, and
/// pressing m opens no list, rather than an empty one to type into.
#[test]
fn a_new_agent_with_no_models_known_offers_no_model_list() {
    use crate::setup::Item;
    let mut fleet = home_fleet();
    let mut studio = host(b"a", "studio", Reads::Online);
    if let wire::inventory_event::Of::Host(entry) = &mut studio {
        entry
            .providers
            .push(offered("antigravity", "Antigravity", Kind::Acp));
    }
    inventory(&mut fleet, studio);
    let mut home = new_home();
    home.key(&fleet, key(KeyCode::Char('n')));
    let setup = home.open_setup().unwrap();
    setup.pick(Item::Kind, "antigravity");
    setup.offer(b"a", "antigravity", &[3; 32], &wire::Catalogue::default());
    home.key(
        &fleet,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
    );
    let screen = home_screen(&mut home, &fleet, theme());
    let keys = screen
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap();
    assert!(
        keys.contains("n name") && keys.contains("d folder"),
        "{screen}"
    );
    assert!(
        !keys.contains("m model") && !keys.contains("e effort"),
        "{screen}"
    );
    home.key(&fleet, key(KeyCode::Char('m')));
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(!screen.contains("╭─ Model"), "{screen}");
    assert!(!screen.contains("type a name"), "{screen}");
}

/// An agent with nothing for Shift+Tab to step (Grok: models, no modes and
/// no permissions) is not offered Shift+Tab in New Agent, nor p.
#[test]
fn a_new_agent_with_nothing_to_step_offers_no_shift_tab() {
    use crate::setup::Item;
    let mut fleet = home_fleet();
    let mut studio = host(b"a", "studio", Reads::Online);
    if let wire::inventory_event::Of::Host(entry) = &mut studio {
        entry.providers.push(offered("grok", "Grok", Kind::Acp));
    }
    inventory(&mut fleet, studio);
    let mut home = new_home();
    home.key(&fleet, key(KeyCode::Char('n')));
    let setup = home.open_setup().unwrap();
    setup.pick(Item::Kind, "grok");
    let models_only = wire::Catalogue {
        modes: Vec::new(),
        ..acp_offer()
    };
    setup.offer(b"a", "grok", &[3; 32], &models_only);
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(screen.contains("enter start"), "{screen}");
    assert!(!screen.contains("shift+tab"), "{screen}");
    home.key(
        &fleet,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
    );
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(screen.contains("m model"), "{screen}");
    assert!(
        !screen.contains("p permission") && !screen.contains("p mode"),
        "{screen}"
    );

    // With modes, Shift+Tab steps them and p says so.
    let mut home = new_home();
    home.key(&fleet, key(KeyCode::Char('n')));
    let setup = home.open_setup().unwrap();
    setup.pick(Item::Kind, "grok");
    setup.offer(b"a", "grok", &[3; 32], &acp_offer());
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(screen.contains("shift+tab mode"), "{screen}");
    home.key(
        &fleet,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
    );
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(screen.contains("p mode"), "{screen}");
}

/// An ACP agent has no terminal of its own: home's keys offer no attach on
/// its row, and a chat's leader panel lists none, while a terminal Claude
/// on this machine keeps both.
#[test]
fn an_agent_with_no_terminal_is_offered_no_attach() {
    let mut fleet = home_fleet();
    let mut studio = host(b"a", "studio", Reads::Online);
    if let wire::inventory_event::Of::Host(entry) = &mut studio {
        entry.providers.push(offered("grok", "Grok", Kind::Acp));
    }
    inventory(&mut fleet, studio);
    let mut row = home_agent(b"g", "grok-agent", Phase::Idle, None, 0);
    if let wire::inventory_event::Of::Agent(agent) = &mut row {
        agent.kind = Kind::Acp as i32;
        agent.provider = "grok".into();
    }
    inventory(&mut fleet, row);
    let mut home = new_home();
    let grok = fleet
        .agents()
        .find(|agent| agent.name == "grok-agent")
        .map(ui_state::agent_key)
        .unwrap();
    home.select(grok);
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(screen.contains("enter open"), "{screen}");
    assert!(!screen.contains("a attach"), "{screen}");
    let planner = fleet
        .agents()
        .find(|agent| agent.name == "planner")
        .map(ui_state::agent_key)
        .unwrap();
    home.select(planner);
    let screen = home_screen(&mut home, &fleet, theme());
    assert!(screen.contains("a attach"), "{screen}");

    let chords = |attach| -> Vec<char> {
        crate::app::panel_entries(attach)
            .iter()
            .map(|entry| entry.chord)
            .collect()
    };
    assert!(chords(true).contains(&'a'));
    assert!(!chords(false).contains(&'a'));
}

/// The newest frame of a recorded ACP session, drawn as a chat.
fn last_screen(fixture: &str) -> String {
    let (_, state, _) = fixtures::frames(Kind::Acp, fixture).pop().unwrap();
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    feed(&mut view, &state).0
}

/// A command the person denied folds as denied, not as a command run.
#[test]
fn a_denied_command_folds_as_denied() {
    let screen = last_screen("agy-deny");
    assert!(screen.contains("Denied 1 step"), "{screen}");
    assert!(!screen.contains("Ran 1 command"), "{screen}");
}

/// An answered form names what was sent by the form's own titles:
/// Antigravity's question, not its field's name.
#[test]
fn an_answered_form_names_its_fields_by_title() {
    let screen = last_screen("agy-question");
    assert!(screen.contains("Sent the form to Antigravity"), "{screen}");
    assert!(screen.contains("Do you prefer tea or coffee?"), "{screen}");
    assert!(!screen.contains("q1"), "{screen}");
}

/// Antigravity sends only the text it replaces, with no line: its edit ask
/// names the file and claims no line.
#[test]
fn an_edit_with_no_place_in_the_file_claims_no_line() {
    let (state, now) = asking("agy-edit", |body| matches!(body, AskBody::Edit { .. }));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let (buffer, _) = draw(&mut view, &state, now, W, H, theme());
    let screen = text(&buffer);
    assert!(screen.contains("Wants to edit"), "{screen}");
    assert!(!screen.contains("· line"), "{screen}");
}

/// An ACP agent's landed edit is coloured as its file's code over the
/// diff's tints, as Claude's and Codex's are: its row names the file and its
/// patch is hunks like theirs, so the shared drawing reads it the same way.
#[test]
fn an_acp_agents_landed_edit_is_coloured_as_its_files_code() {
    use prost::Message as _;
    use ui_view::TokenKind;
    crate::code::highlighter().preload();
    let agent = wire::Agent {
        cwd: "/work".into(),
        provider: "opencode".into(),
        ..fixtures::agent(Kind::Acp)
    };
    let mut state = SessionState::new(agent, crate::chat::layout::CAP as usize);
    state.update(Msg::Connection(ui_state::Connection::Live));
    state.update(snapshot(Phase::Idle, vec![], vec![]));
    let edit = wire::AcpTool {
        title: "Edit src/ready.rs".into(),
        kind: wire::AcpToolKind::Edit as i32,
        state: wire::ToolState::Succeeded as i32,
        class: wire::ToolClass::Consequential as i32,
        diffs: vec![wire::AcpDiff {
            path: "/work/src/ready.rs".into(),
            old_text: Some("// Ready.\nfn ready() -> bool { false }\n".into()),
            new_text: "// Ready.\nfn ready() -> bool { true }\n".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    state.update(event(session_event::Of::Item(wire::Item {
        key: "tool:edit".into(),
        order: 1,
        revision: 1,
        kind: Kind::Acp as i32,
        body: wire::AcpItem {
            kind: Some(wire::acp_item::Kind::Tool(edit)),
        }
        .encode_to_vec(),
        ..Default::default()
    })));
    state.update(caught_up(0));

    let rows = ui_view::chat_rows_for(
        &state,
        &["tool:edit".to_owned()],
        ui_view::ToolRows::ShowAll,
    );
    let Some(ui_view::RowKind::FileChange { files, .. }) = rows.first().map(|row| &row.kind) else {
        panic!("a file change row: {rows:?}");
    };
    assert_eq!(files[0].path, "src/ready.rs");
    let head = ui_view::patch_head(&state, &"tool:edit".to_owned(), usize::MAX).expect("a head");
    let theme = crate::theme::Theme::default();
    let lines = crate::chat::rows::patch_lines(&head, &files[0].path, true, 'b', 80, theme);
    let added = theme.diff_added();
    let keyword = lines[1]
        .spans
        .iter()
        .find(|span| span.content == "fn")
        .map(|span| span.style);
    assert_eq!(
        keyword,
        Some(theme.syntax_on_diff(added, TokenKind::Keyword)),
        "{lines:?}"
    );
}
