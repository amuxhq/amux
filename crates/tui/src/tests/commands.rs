//! The agent's commands offered over the composer as a "/word" is typed:
//! what the flyover lists, and what its keys and clicks do to the draft.

use super::*;

/// A headless Claude chat offering clear, compact, context, cost, init,
/// pr-comments, review, security-review, code-review:review and
/// release-notes, beside amux's own model, effort and rename.
fn chat() -> (ChatView, SessionState) {
    let scene = crate::vocabulary::chat_commands("");
    (scene.view, scene.state)
}

fn screen(view: &mut ChatView, state: &SessionState) -> String {
    let (buffer, _) = draw(view, state, 0, W, H, theme());
    text(&buffer)
}

fn prompts(effects: &[ChatEffect]) -> Vec<String> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            ChatEffect::Prompt { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// A bare "/" lists the agent's commands with their descriptions under a
/// title; a word narrows them to the ones it starts, a plugin's by the
/// name after its namespace, and the key line says what the keys do.
#[test]
fn a_slash_word_lists_the_commands_it_starts_over_the_composer() {
    let (mut view, state) = chat();
    assert!(!screen(&mut view, &state).contains("Commands"));
    typed(&mut view, &state, "/");
    let all = screen(&mut view, &state);
    assert!(
        all.contains("Commands")
            && all.contains("/compact [instructions]")
            && all.contains("Clear the history but keep a summary in context")
            && all.contains("more"),
        "{all}"
    );
    typed(&mut view, &state, "re");
    let narrowed = screen(&mut view, &state);
    for shown in [
        "/rename",
        "/review",
        "/code-review:review",
        "/release-notes",
    ] {
        assert!(narrowed.contains(shown), "{shown} missing:\n{narrowed}");
    }
    assert!(
        !narrowed.contains("/compact") && narrowed.contains("enter fill in"),
        "{narrowed}"
    );
}

/// Tab fills the lit command in, with a space to go on typing, and the
/// flyover goes once the draft is more than a "/word".
#[test]
fn tab_fills_the_lit_command_in() {
    let (mut view, state) = chat();
    typed(&mut view, &state, "/comp");
    let effects = view.key(&state, key(KeyCode::Tab), theme());
    assert!(effects.is_empty(), "{effects:?}");
    assert_eq!(view.editor.text(), "/compact ");
    assert!(!screen(&mut view, &state).contains("Commands"));
}

/// Enter runs the lit command when it needs nothing after its name, and
/// the arrows move which one is lit.
#[test]
fn enter_runs_the_lit_command_and_the_arrows_move_it() {
    let (mut view, state) = chat();
    typed(&mut view, &state, "/cl");
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    assert_eq!(prompts(&effects), ["/clear"]);
    assert!(view.editor.is_empty());

    let (mut view, state) = chat();
    typed(&mut view, &state, "/c");
    view.key(&state, key(KeyCode::Down), theme());
    view.key(&state, key(KeyCode::Down), theme());
    view.key(&state, key(KeyCode::Up), theme());
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    assert_eq!(prompts(&effects), ["/compact"]);
}

/// A command whose hint names an argument it needs is filled in by Enter,
/// not run without it.
#[test]
fn enter_fills_in_a_command_that_needs_an_argument() {
    let (mut view, state) = chat();
    typed(&mut view, &state, "/pr");
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    assert!(prompts(&effects).is_empty(), "{effects:?}");
    assert_eq!(view.editor.text(), "/pr-comments ");
}

/// Esc closes the flyover and keeps the draft, and Enter then sends what
/// was typed; an edit offers the commands again.
#[test]
fn esc_closes_the_flyover_until_the_draft_changes() {
    let (mut view, state) = chat();
    typed(&mut view, &state, "/co");
    view.key(&state, key(KeyCode::Esc), theme());
    assert_eq!(view.editor.text(), "/co");
    assert!(!screen(&mut view, &state).contains("Commands"));
    typed(&mut view, &state, "n");
    assert!(screen(&mut view, &state).contains("Commands"));
    view.key(&state, key(KeyCode::Esc), theme());
    let effects = view.key(&state, key(KeyCode::Enter), theme());
    assert_eq!(prompts(&effects), ["/con"]);
}

/// While the flyover is open, Up moves within it rather than bringing the
/// last prompt back.
#[test]
fn up_moves_in_the_flyover_not_through_history() {
    let (mut view, state) = chat();
    typed(&mut view, &state, "/");
    view.key(&state, key(KeyCode::Down), theme());
    view.key(&state, key(KeyCode::Up), theme());
    view.key(&state, key(KeyCode::Up), theme());
    assert_eq!(view.editor.text(), "/");
}

/// A click on a command fills it in.
#[test]
fn a_click_on_a_command_fills_it_in() {
    let (mut view, state) = chat();
    typed(&mut view, &state, "/");
    let drawn = screen(&mut view, &state);
    click(&mut view, &state, &drawn, "/context");
    assert_eq!(view.editor.text(), "/context ");
}

/// Typing a command's whole name lights it even when another listed first
/// also starts with it.
#[test]
fn the_whole_name_typed_is_the_one_lit() {
    let command = |name: &str| ui_view::CommandView {
        name: name.into(),
        description: String::new(),
        argument_hint: String::new(),
        source: String::new(),
        sigil: "/".into(),
        anywhere: false,
    };
    let commands = [command("clear-all"), command("clear")];
    let matches = ui_view::command_matches(&commands, "/clear");
    let menu = crate::chat::commands::Menu::default();
    assert_eq!(
        matches[menu.selected("/clear", &matches)].command.name,
        "clear"
    );
    assert_eq!(
        menu.selected("/cle", &ui_view::command_matches(&commands, "/cle")),
        0
    );
}
