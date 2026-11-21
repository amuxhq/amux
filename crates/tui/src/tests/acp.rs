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
