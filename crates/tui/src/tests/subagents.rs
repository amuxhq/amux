//! Provider subagents in the terminal: the parent's call rows read how
//! each subagent stands from the fleet, a turn's calls fold into one
//! group, the leader's Enter opens a subagent's chat, and that chat's
//! composer takes no messages and offers Stop where the provider stops a
//! subagent.

use super::*;
use crate::fixtures::{Family, family_frames};

const SW: u16 = 110;
const SH: u16 = 34;

/// The chat of `state` drawn as the app draws it, the fleet's view of the
/// agents its rows started handed over first.
fn chat_screen(view: &mut ChatView, state: &SessionState, family: &Family) -> String {
    view.agents = ui_view::chat_agents(state, &family.fleet);
    let (buffer, _) = crate::fixtures::draw(view, state, family.at, SW, SH, theme());
    text(&buffer)
}

/// The frame of a fixture whose label starts `label`.
fn frame_at(kind: Kind, name: &str, label: &str) -> Family {
    family_frames(kind, name)
        .into_iter()
        .find(|family| family.label.starts_with(label))
        .unwrap_or_else(|| panic!("{name}: no frame {label}"))
}

/// The parallel fixture while its three subagents run, and once all three
/// are done.
fn running() -> Family {
    frame_at(
        Kind::ClaudeSdk,
        "recorded_subagents_parallel",
        "recorded 120:",
    )
}

fn finished() -> Family {
    family_frames(Kind::ClaudeSdk, "recorded_subagents_parallel")
        .pop()
        .unwrap()
}

fn parent_view(family: &Family) -> ChatView {
    ChatView::new(b"agent".to_vec(), family.at, false)
}

fn sub_view(family: &Family, n: usize) -> ChatView {
    ChatView::new(family.sub(n).row.agent_id.clone(), family.at, false)
}

/// The subagent's own job as its snapshot lists it, stopping or refused.
fn job_snapshot(sub: &crate::fixtures::Sub, stopping: bool, error: &str) -> Msg {
    let key = sub.row.native.as_ref().unwrap().provider_child_id.clone();
    event(session_event::Of::Snapshot(wire::Snapshot {
        revision: 1_000_000,
        phase: Phase::Working as i32,
        body: wire::ClaudeSnapshot {
            background_jobs: Some(wire::BackgroundJobs {
                known: true,
                jobs: vec![wire::BackgroundJob {
                    kind: wire::BackgroundKind::Subagent as i32,
                    key,
                    command: "Run sleep then echo RED".into(),
                    stopping,
                    stop_error: error.into(),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        }
        .encode_to_vec(),
        ..Default::default()
    }))
}

/// Several subagents started in one turn fold into one row that says how
/// many and where they stand, from the fleet: all running, then all done.
/// The prompt that started them stays in view above it.
#[test]
fn a_turns_subagents_fold_into_one_row_with_where_they_stand() {
    let family = running();
    let screen = chat_screen(&mut parent_view(&family), &family.state, &family);
    assert!(screen.contains("▸ 3 subagents · 3 running"), "{screen}");
    assert!(
        screen.contains("call the Agent tool three times"),
        "{screen}"
    );
    // The calls fold under the group: none draws its own row.
    assert!(!screen.contains("Running subagent"), "{screen}");
    assert!(screen.contains("3 subagents working"), "{screen}");

    let family = finished();
    let screen = chat_screen(&mut parent_view(&family), &family.state, &family);
    assert!(screen.contains("▸ 3 subagents · 3 done"), "{screen}");
    assert!(screen.contains("RED GREEN BLUE"), "{screen}");
}

/// While the agent waits on its subagents and one of them waits on the
/// person, its activity line says how they stand, as their group's row
/// does, not that they all work.
#[test]
fn the_activity_line_says_a_subagent_waits_on_you() {
    let mut family = running();
    let mut asking = family.sub(1).row.clone();
    asking.phase = Phase::NeedsYou as i32;
    family
        .fleet
        .update(ui_state::FleetMsg::Event(Box::new(wire::InventoryEvent {
            of: Some(wire::inventory_event::Of::Agent(asking)),
        })));
    let screen = chat_screen(&mut parent_view(&family), &family.state, &family);
    assert!(
        screen.contains("▸ 3 subagents · 2 running · 1 waiting on you"),
        "{screen}"
    );
    let activity = screen
        .lines()
        .rfind(|line| line.contains("subagent"))
        .unwrap_or_default();
    assert!(
        activity.contains("3 subagents · 2 running · 1 waiting on you") && !activity.contains('▸'),
        "{screen}"
    );
    assert!(!screen.contains("subagents working"), "{screen}");
}

/// Home's line for a parent that waits on its subagents says how they
/// stand once one of them waits on the person, in the words and from the
/// same source as the chat's activity line, not that they all work.
#[test]
fn home_says_a_parents_subagent_waits_on_you_as_its_chat_does() {
    let mut family = running();
    let parent = ui_state::agent_key(family.state.agent());
    inventory(&mut family.fleet, host(&parent.host, "host", Reads::Online));
    // The fixture lists the parent as it started; live, it works while it
    // waits on its subagents.
    let mut working = family.state.agent().clone();
    working.phase = Phase::Working as i32;
    inventory(&mut family.fleet, wire::inventory_event::Of::Agent(working));
    let lines = |family: &Family| {
        HashMap::from([(
            parent.clone(),
            ui_view::session_line(&family.state, family.at),
        )])
    };
    let mut home = crate::home::Home::default();
    home.select(parent.clone());
    home.key(&family.fleet, key(KeyCode::Right));
    let screen = home_screen_with(&mut home, &family.fleet, &lines(&family), theme());
    assert!(screen.contains("3 subagents working"), "{screen}");

    let mut asking = family.sub(1).row.clone();
    asking.phase = Phase::NeedsYou as i32;
    family
        .fleet
        .update(ui_state::FleetMsg::Event(Box::new(wire::InventoryEvent {
            of: Some(wire::inventory_event::Of::Agent(asking)),
        })));
    let screen = home_screen_with(&mut home, &family.fleet, &lines(&family), theme());
    assert!(
        screen.contains("3 subagents · 2 running · 1 waiting on you"),
        "{screen}"
    );
    assert!(!screen.contains("subagents working"), "{screen}");
}

/// The leader's `o` opens the group in place and closes it again; the
/// leader's Enter on one of its calls opens that subagent's chat, a
/// gesture apart. On the folded group, which stands for several agents,
/// Enter opens the group to pick one from.
#[test]
fn the_leader_opens_a_group_in_place_and_enter_opens_a_subagents_chat() {
    let family = running();
    let mut view = parent_view(&family);
    chat_screen(&mut view, &family.state, &family);
    // Nothing focused: Enter falls on the newest group, which opens.
    assert!(view.open_focused_agent(&family.state).is_empty());
    let screen = chat_screen(&mut view, &family.state, &family);
    assert!(screen.contains("▾ 3 subagents · 3 running"), "{screen}");
    for colour in ["RED", "GREEN", "BLUE"] {
        assert!(
            screen.contains(&format!("● Running subagent Run sleep then echo {colour}")),
            "{screen}"
        );
    }
    // The newest call, focused: Enter opens its subagent's chat.
    view.move_focus(&family.state, true, theme());
    chat_screen(&mut view, &family.state, &family);
    let blue = ui_state::agent_key(&family.sub(2).row);
    assert_eq!(
        view.open_focused_agent(&family.state),
        vec![ChatEffect::OpenAgent(blue)]
    );
    // The first call carries the group's line: `o` there folds the group.
    for _ in 0..2 {
        view.move_focus(&family.state, true, theme());
        chat_screen(&mut view, &family.state, &family);
    }
    view.toggle_expanded(&family.state);
    let screen = chat_screen(&mut view, &family.state, &family);
    assert!(screen.contains("▸ 3 subagents · 3 running"), "{screen}");
    // With nothing focused, `o` falls on the newest group, as on a run.
    view.focus = None;
    view.toggle_expanded(&family.state);
    let screen = chat_screen(&mut view, &family.state, &family);
    assert!(screen.contains("▾ 3 subagents · 3 running"), "{screen}");
}

/// A lone call reads its subagent's state from the fleet: waiting on you
/// while the subagent's ask is open in its own chat, which the call itself
/// knows nothing of.
#[test]
fn a_subagent_waiting_on_you_reads_so_on_its_parents_row() {
    let family = frame_at(Kind::ClaudeSdk, "recorded_subagent_ask", "recorded 57:");
    let screen = chat_screen(&mut parent_view(&family), &family.state, &family);
    assert!(
        screen.contains("● Running subagent Create asked.txt file · waiting on you"),
        "{screen}"
    );
    let ask = chat_screen(&mut sub_view(&family, 0), &family.sub(0).state, &family);
    assert!(ask.contains("Wants to run"), "{ask}");
    assert!(ask.contains("touch asked.txt"), "{ask}");
    // In the subagent's chat Ctrl+X stops the subagent: it has no turn of
    // its own to interrupt.
    assert!(ask.contains("ctrl+x stop"), "{ask}");
    assert!(!ask.contains("interrupt"), "{ask}");
}

/// A subagent's chat has no text box, so nothing typed there lands in a
/// draft nobody sees. A key pressed as its ask appears is dropped, not
/// kept for a draft: the ask was not read yet, so it does not answer it
/// either. Once the ask has been on screen half a second with no key, a
/// digit answers it. A paste goes nowhere.
#[test]
fn keys_pressed_as_a_subagents_ask_appears_never_land_in_a_hidden_draft() {
    let family = frame_at(Kind::ClaudeSdk, "recorded_subagent_ask", "recorded 57:");
    let sub = &family.sub(0).state;
    let mut view = sub_view(&family, 0);
    let at = family.at;
    crate::fixtures::draw(&mut view, sub, at, SW, SH, theme());
    let effects = view.key_at(sub, key(KeyCode::Char('1')), theme(), at + 100);
    assert!(effects.is_empty(), "{effects:?}");
    view.paste_text(sub, "pasted");
    assert_eq!(view.editor.text(), "", "a subagent's chat kept a draft");

    let now = at + 100 + crate::chat::ASK_ARMS_MS;
    crate::fixtures::draw(&mut view, sub, now, SW, SH, theme());
    let effects = view.key_at(sub, key(KeyCode::Char('1')), theme(), now);
    assert!(
        matches!(effects.as_slice(), [ChatEffect::Answer(_)]),
        "{effects:?}"
    );
    assert_eq!(view.editor.text(), "");
}

/// Words selected in a subagent's chat copy, but offer no reply: there is
/// no draft to quote them into.
#[test]
fn a_selection_in_a_subagents_chat_offers_no_reply() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let family = running();
    let sub = &family.sub(0).state;
    let mut view = sub_view(&family, 0);
    let (buffer, _) = crate::fixtures::draw(&mut view, sub, family.at, SW, SH, theme());
    let screen = text(&buffer);
    let (row, line) = screen
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("echo RED. Report"))
        .unwrap_or_else(|| panic!("{screen}"));
    let column = line[..line.find("RED").unwrap()].chars().count() as u16;
    let at = |column: u16, kind| MouseEvent {
        kind,
        column,
        row: row as u16,
        modifiers: KeyModifiers::NONE,
    };
    let left = MouseButton::Left;
    view.mouse(sub, at(column, MouseEventKind::Down(left)), theme());
    view.mouse(sub, at(column + 2, MouseEventKind::Drag(left)), theme());
    crate::fixtures::draw(&mut view, sub, family.at, SW, SH, theme());
    view.mouse(sub, at(column + 2, MouseEventKind::Up(left)), theme());
    let (buffer, _) = crate::fixtures::draw(&mut view, sub, family.at, SW, SH, theme());
    let screen = text(&buffer);
    assert!(screen.contains("esc clear"), "{screen}");
    assert!(!screen.contains("r reply"), "{screen}");
    view.key(sub, key(KeyCode::Char('r')), theme());
    assert_eq!(view.editor.text(), "", "a subagent's chat kept a quote");
}

/// A subagent's chat shows its own work: its prompt, its steps and its
/// answer, and a header with no diff of its own.
#[test]
fn a_subagents_chat_shows_its_own_steps_and_no_diff() {
    let family = finished();
    let screen = chat_screen(&mut sub_view(&family, 0), &family.sub(0).state, &family);
    assert!(screen.contains("Run sleep then echo RED"), "{screen}");
    assert!(
        screen.contains("Use the Bash tool to run exactly this command"),
        "{screen}"
    );
    assert!(screen.contains("Ran 1 command"), "{screen}");
    assert!(screen.contains("RED"), "{screen}");
    assert!(!screen.contains("[Diff"), "{screen}");
}

/// The composer of a provider subagent has no field: it says Claude runs
/// the subagent and it takes no messages, nothing typed lands and Enter
/// sends nothing. Ctrl+X asks before stopping it and Enter there sends the
/// stop to the subagent's own job.
#[test]
fn a_subagents_composer_takes_no_messages_and_stops_it_behind_a_confirm() {
    let family = running();
    let sub = family.sub(0);
    let mut view = sub_view(&family, 0);
    let screen = chat_screen(&mut view, &sub.state, &family);
    assert!(
        screen.contains("Claude runs this subagent · it takes no messages"),
        "{screen}"
    );
    assert!(screen.contains("ctrl+x stop"), "{screen}");
    assert!(!screen.contains("Message"), "{screen}");
    for key_event in [
        key(KeyCode::Char('h')),
        key(KeyCode::Char('i')),
        key(KeyCode::Enter),
    ] {
        assert!(view.key(&sub.state, key_event, theme()).is_empty());
    }
    let screen = chat_screen(&mut view, &sub.state, &family);
    assert!(!screen.contains("› hi"), "{screen}");

    assert!(view.key(&sub.state, ctrl('x'), theme()).is_empty());
    let asked = chat_screen(&mut view, &sub.state, &family);
    assert!(
        asked.contains("Stop Run sleep then echo RED? Its parent carries on."),
        "{asked}"
    );
    let ui_state::Composer::Native { stop } = sub.state.composer() else {
        panic!("a subagent's composer is native");
    };
    assert_eq!(
        view.key(&sub.state, key(KeyCode::Enter), theme()),
        vec![ChatEffect::StopJob {
            key: stop.key().unwrap().to_owned()
        }]
    );
}

/// Once a stop went, the composer says it is stopping and offers no other;
/// a refusal shows the provider's words and offers Stop again.
#[test]
fn a_subagents_stop_shows_stopping_then_the_providers_refusal() {
    let family = running();
    let mut state = family.sub(0).state.clone();
    state.update(job_snapshot(family.sub(0), true, ""));
    let mut view = sub_view(&family, 0);
    let screen = chat_screen(&mut view, &state, &family);
    assert!(screen.contains("Stopping…"), "{screen}");
    assert!(!screen.contains("ctrl+x stop"), "{screen}");
    assert!(view.key(&state, ctrl('x'), theme()).is_empty());
    assert!(!chat_screen(&mut view, &state, &family).contains("Its parent carries on"));

    let mut state = family.sub(0).state.clone();
    state.update(job_snapshot(family.sub(0), false, "no such task"));
    let screen = chat_screen(&mut view, &state, &family);
    assert!(
        screen.contains("Could not stop it: no such task"),
        "{screen}"
    );
    assert!(screen.contains("ctrl+x stop"), "{screen}");
}

/// Terminal Claude stops no single subagent, so its subagent's chat offers
/// no Stop at all, in the composer or in an ask's box.
#[test]
fn a_terminal_claude_subagent_offers_no_stop() {
    let family = frame_at(Kind::ClaudePty, "recorded_subagent_ask", "recorded 61:");
    let sub = family.sub(0);
    let mut view = sub_view(&family, 0);
    let ask = chat_screen(&mut view, &sub.state, &family);
    assert!(ask.contains("Wants to run"), "{ask}");
    assert!(!ask.contains("ctrl+x"), "{ask}");
    assert!(view.key(&sub.state, ctrl('x'), theme()).is_empty());

    let family = family_frames(Kind::ClaudePty, "recorded_subagent_ask")
        .pop()
        .unwrap();
    let sub = family.sub(0);
    let screen = chat_screen(&mut sub_view(&family, 0), &sub.state, &family);
    assert!(
        screen.contains("Claude runs this subagent · it takes no messages"),
        "{screen}"
    );
    assert!(!screen.contains("ctrl+x"), "{screen}");
}

/// Home lists subagents under their host by their titles, with no folder
/// of their own, and offers them no terminal and no delete.
#[test]
fn home_lists_subagents_under_their_host_without_a_terminal_or_delete() {
    let family = running();
    let mut home = crate::home::Home::default();
    let host = ui_state::agent_key(&family.state.agent().clone());
    home.select(host.clone());
    let fleet = &family.fleet;
    home.key(fleet, key(KeyCode::Right));
    let screen = fleet_screen(&mut home, fleet);
    for colour in ["RED", "GREEN", "BLUE"] {
        let line = screen
            .lines()
            .find(|line| line.contains(&format!("Run sleep then echo {colour}")))
            .unwrap_or_else(|| panic!("{screen}"));
        assert!(line.contains("Claude subagent"), "{screen}");
    }
    let red = ui_state::agent_key(&family.sub(0).row);
    home.select(red.clone());
    let screen = fleet_screen(&mut home, fleet);
    assert!(!screen.contains("a attach"), "{screen}");
    assert!(home.key(fleet, key(KeyCode::Char('a'))).is_empty());

    // Settled, it is not deleted on its own.
    let mut fleet = fleet.clone();
    let mut row = family.sub(0).row.clone();
    row.filing = wire::Filing::Closed as i32;
    row.lifecycle = wire::Lifecycle::Exited as i32;
    inventory(&mut fleet, wire::inventory_event::Of::Agent(row.clone()));
    assert!(home.key(&fleet, key(KeyCode::Char('x'))).is_empty());
    let screen = fleet_screen(&mut home, &fleet);
    assert!(!screen.contains("Delete"), "{screen}");

    let config = crate::TuiConfig {
        working_dir: ".".into(),
        leader: 'a',
        theme: theme(),
        initial_chat: None,
        version: "0.7.0".into(),
        local_host: b"host".to_vec(),
        layout: None,
        chat_in: settings::ChatInSetting::Amux,
        system_clipboard: false,
    };
    assert_eq!(
        crate::app::terminal_refusal(&row, &config),
        Some("a subagent has no terminal of its own")
    );
}

/// With the mouse, a click on the group's line opens it and a click on one
/// subagent's line opens that subagent's chat, as a tap does on the phone.
#[test]
fn a_click_opens_the_group_and_a_subagents_chat() {
    let family = running();
    let mut view = parent_view(&family);
    let screen = chat_screen(&mut view, &family.state, &family);
    assert!(click(&mut view, &family.state, &screen, "▸ 3 subagents").is_empty());
    let screen = chat_screen(&mut view, &family.state, &family);
    assert!(screen.contains("▾ 3 subagents"), "{screen}");
    let green = ui_state::agent_key(&family.sub(1).row);
    assert_eq!(
        click(
            &mut view,
            &family.state,
            &screen,
            "subagent Run sleep then echo GREEN"
        ),
        vec![ChatEffect::OpenAgent(green)]
    );
}
