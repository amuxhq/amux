//! Dragging over text in a chat, what that copies, replying with it, and how
//! the composer draws a quote and inline markdown. Each test presses the
//! mouse where a drawn word is, so what it asserts is what a person gets.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::style::Modifier;

use super::*;
use crate::chat::composer::{cursor_at, editor_lines, folded_quote_above};
use crate::editor::Editor;

const WIDE: u16 = 44;
const TALL: u16 = 24;
/// A line of the patch the review tests open.
const REVIEW_CODE: &str = "let c = 4;";

fn agent_says(order: u64, words: &str) -> Item {
    use wire::claude_item::Kind as K;
    Item {
        key: format!("r{order}"),
        order,
        revision: order,
        text: words.into(),
        kind: Kind::ClaudeSdk as i32,
        body: wire::ClaudeItem {
            kind: Some(K::Message(wire::Text { complete: true })),
        }
        .encode_to_vec(),
        at_ms: order as i64 * 1_000,
        ..Item::default()
    }
}

fn rows(view: &mut ChatView, state: &SessionState) -> Vec<String> {
    let (buffer, _) = draw(view, state, 0, WIDE, TALL, theme());
    text(&buffer).lines().map(str::to_owned).collect()
}

/// The column and row where `word` is drawn.
fn find(rows: &[String], word: &str) -> (u16, u16) {
    for (y, row) in rows.iter().enumerate() {
        if let Some(byte) = row.find(word) {
            return (row[..byte].chars().count() as u16, y as u16);
        }
    }
    panic!("{word} is not on\n{}", rows.join("\n"));
}

fn mouse(kind: MouseEventKind, at: (u16, u16)) -> MouseEvent {
    MouseEvent {
        kind,
        column: at.0,
        row: at.1,
        modifiers: KeyModifiers::NONE,
    }
}

/// Presses at `from`, drags to `to`, draws, and releases.
fn drag(
    view: &mut ChatView,
    state: &SessionState,
    from: (u16, u16),
    to: (u16, u16),
) -> Vec<ChatEffect> {
    let left = MouseButton::Left;
    view.mouse(state, mouse(MouseEventKind::Down(left), from), theme());
    view.mouse(state, mouse(MouseEventKind::Drag(left), to), theme());
    draw(view, state, 0, WIDE, TALL, theme());
    view.mouse(state, mouse(MouseEventKind::Up(left), to), theme())
}

fn copied(effects: &[ChatEffect]) -> String {
    match effects {
        [ChatEffect::Copy(words)] => words.clone(),
        other => panic!("expected one copy, got {other:?}"),
    }
}

const PARAGRAPH: &str = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima";

#[test]
fn a_drag_over_wrapped_words_copies_them_as_one_line_without_the_margin() {
    let state = chat(vec![agent_says(1, PARAGRAPH)]);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let drawn = rows(&mut view, &state);
    let start = find(&drawn, "alpha");
    let end = find(&drawn, "lima");
    assert_ne!(start.1, end.1, "the paragraph should wrap");

    // From the margin left of the first word, to the last letter.
    let effects = drag(&mut view, &state, (0, start.1), (end.0 + 3, end.1));
    assert_eq!(copied(&effects), PARAGRAPH);
    let drawn = rows(&mut view, &state);
    assert!(drawn.last().is_some_and(|row| row.contains("copied")));
    let keys = keys_of(&drawn.join("\n"));
    assert!(
        keys.contains("r reply") && keys.contains("esc clear"),
        "{keys}"
    );
}

#[test]
fn a_drag_inside_a_line_copies_just_the_cells_it_covers() {
    let state = chat(vec![agent_says(1, PARAGRAPH)]);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let drawn = rows(&mut view, &state);
    let start = find(&drawn, "bravo");
    let effects = drag(&mut view, &state, start, (start.0 + 4, start.1));
    assert_eq!(copied(&effects), "bravo");
}

#[test]
fn a_code_block_keeps_its_indentation_and_paragraphs_keep_a_blank_between() {
    let source = "first\n\n```\n    indented\nplain\n```";
    let state = chat(vec![agent_says(1, source)]);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let drawn = rows(&mut view, &state);
    let start = find(&drawn, "first");
    let end = find(&drawn, "plain");
    let effects = drag(&mut view, &state, start, (end.0 + 4, end.1));
    assert_eq!(copied(&effects), "first\n\n    indented\nplain");
}

#[test]
fn your_message_is_copied_without_its_time() {
    let state = chat(vec![prompt_item(1, "what did the build say")]);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let drawn = rows(&mut view, &state);
    let start = find(&drawn, "what");
    let effects = drag(&mut view, &state, (0, start.1), (WIDE - 1, start.1));
    assert_eq!(copied(&effects), "what did the build say");
}

#[test]
fn a_selection_stays_on_its_words_while_the_feed_moves() {
    let mut items = vec![agent_says(1, "keep this sentence")];
    items.extend(replies(2, 4));
    let state = chat(items);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let drawn = rows(&mut view, &state);
    let start = find(&drawn, "keep");
    let end = find(&drawn, "sentence");
    drag(&mut view, &state, start, (end.0 + 7, end.1));

    let mut more = vec![agent_says(1, "keep this sentence")];
    more.extend(replies(2, 8));
    let moved = chat(more);
    rows(&mut view, &moved);
    view.key(&moved, key(KeyCode::Char('r')), theme());
    assert!(view.editor.text().starts_with("> keep this sentence"));
}

#[test]
fn r_quotes_the_selection_at_the_cursor_and_leaves_the_reply_to_you() {
    let state = chat(vec![agent_says(1, PARAGRAPH)]);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let drawn = rows(&mut view, &state);
    let start = find(&drawn, "bravo");
    drag(&mut view, &state, start, (start.0 + 4, start.1));

    view.key(&state, key(KeyCode::Char('r')), theme());
    typed(&mut view, &state, "Oh really?");
    assert_eq!(view.editor.text(), "> bravo\n\nOh really?");
    assert!(!keys_of(&rows(&mut view, &state).join("\n")).contains("r reply"));
}

#[test]
fn a_quote_starts_its_own_lines_after_a_draft() {
    let mut editor = Editor::default();
    editor.set("first thought", Vec::new());
    editor.quote("one\n\ntwo");
    assert_eq!(editor.text(), "first thought\n\n> one\n>\n> two\n\n");
}

#[test]
fn another_key_lets_the_selection_go_and_reaches_the_draft() {
    let state = chat(vec![agent_says(1, PARAGRAPH)]);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let drawn = rows(&mut view, &state);
    let start = find(&drawn, "bravo");
    drag(&mut view, &state, start, (start.0 + 4, start.1));
    typed(&mut view, &state, "x");
    assert_eq!(view.editor.text(), "x");
    view.key(&state, key(KeyCode::Char('r')), theme());
    assert_eq!(
        view.editor.text(),
        "xr",
        "the selection is gone, so r is typed"
    );
}

#[test]
fn with_copying_off_the_clipboard_waits_for_c() {
    let state = chat(vec![agent_says(1, PARAGRAPH)]);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.copy_on_select = false;
    let drawn = rows(&mut view, &state);
    let start = find(&drawn, "bravo");
    assert!(drag(&mut view, &state, start, (start.0 + 4, start.1)).is_empty());
    let keys = keys_of(&rows(&mut view, &state).join("\n"));
    assert!(keys.contains("c copy"), "{keys}");
    let effects = view.key(&state, key(KeyCode::Char('c')), theme());
    assert_eq!(copied(&effects), "bravo");
}

#[test]
fn a_click_acts_on_the_release_and_a_drag_from_the_same_place_does_not() {
    let source = "see [the docs](https://example.com/docs) now";
    let state = chat(vec![agent_says(1, source)]);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let drawn = rows(&mut view, &state);
    let link = find(&drawn, "docs");
    let left = MouseButton::Left;
    assert!(
        view.mouse(&state, mouse(MouseEventKind::Down(left), link), theme())
            .is_empty()
    );
    assert!(matches!(
        view.mouse(&state, mouse(MouseEventKind::Up(left), link), theme())
            .as_slice(),
        [ChatEffect::OpenUrl(_)]
    ));

    let effects = drag(&mut view, &state, link, (link.0 + 3, link.1));
    assert_eq!(copied(&effects), "docs");
}

#[test]
fn a_drag_held_past_the_top_of_the_feed_reaches_rows_scrolled_in() {
    let state = chat(replies(1, 60));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let drawn = rows(&mut view, &state);
    let left = MouseButton::Left;
    let start = (6, drawn.len() as u16 - 8);
    view.mouse(&state, mouse(MouseEventKind::Down(left), start), theme());
    view.mouse(&state, mouse(MouseEventKind::Drag(left), (6, 0)), theme());
    assert!(view.selecting_past_edge());
    for _ in 0..30 {
        view.edge_tick(&state, theme());
        rows(&mut view, &state);
    }
    let effects = view.mouse(&state, mouse(MouseEventKind::Up(left), (6, 0)), theme());
    assert!(!view.selecting_past_edge());
    let lines = copied(&effects).lines().count();
    assert!(
        lines > usize::from(TALL),
        "only {lines} lines were selected"
    );
}

#[test]
fn the_review_page_copies_code_without_its_gutter() {
    let state = chat(replies(1, 3));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.open_review_at(working_tree_diff(), PATCH.into(), None);
    let drawn = rows(&mut view, &state);
    let row = drawn
        .iter()
        .position(|row| row.contains(REVIEW_CODE))
        .unwrap_or_else(|| panic!("{}", drawn.join("\n")));
    let end = find(&drawn, REVIEW_CODE);
    let effects = drag(
        &mut view,
        &state,
        (0, row as u16),
        (end.0 + REVIEW_CODE.len() as u16 - 1, row as u16),
    );
    assert_eq!(copied(&effects), REVIEW_CODE);
    assert!(view.review_open);
    view.key(&state, key(KeyCode::Esc), theme());
    assert!(
        view.review_open,
        "esc let go of the selection, not the page"
    );
}

fn drafted(words: &str, cursor: usize) -> Editor {
    let mut editor = Editor::default();
    editor.set(words, Vec::new());
    editor.set_cursor_chars(cursor);
    editor
}

fn shown(editor: &Editor, width: usize) -> Vec<String> {
    let (lines, _) = editor_lines(editor, "", width, theme());
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
                .chars()
                .skip(2)
                .collect()
        })
        .collect()
}

/// The style of the first drawn cell of `at`-th character on the only row.
fn style_at(editor: &Editor, at: usize) -> ratatui::style::Style {
    let (lines, _) = editor_lines(editor, "", 80, theme());
    let mut seen = 0;
    for span in lines[0].spans.iter().skip(1) {
        let count = span.content.chars().count();
        if at < seen + count {
            return span.style;
        }
        seen += count;
    }
    panic!("no character {at}");
}

#[test]
fn bold_and_italic_show_styled_with_their_marks_kept() {
    let editor = drafted("a **bold** and *slant* word", 0);
    assert_eq!(shown(&editor, 80), ["a **bold** and *slant* word"]);
    let bold = style_at(&editor, 4);
    assert!(bold.add_modifier.contains(Modifier::BOLD));
    assert!(!style_at(&editor, 2).add_modifier.contains(Modifier::BOLD));
    assert_eq!(
        style_at(&editor, 2).fg,
        theme().faint().fg,
        "a mark recedes"
    );
    assert!(
        style_at(&editor, 16)
            .add_modifier
            .contains(Modifier::ITALIC)
    );
}

#[test]
fn inline_code_is_styled_and_arithmetic_is_not() {
    let editor = drafted("run `make` now", 0);
    assert_eq!(style_at(&editor, 5).fg, theme().code().fg);
    assert_eq!(style_at(&editor, 4).fg, theme().faint().fg);
    let sums = drafted("2 * 3 * 4 and **open", 0);
    for at in 0..20 {
        assert!(!style_at(&sums, at).add_modifier.contains(Modifier::ITALIC));
        assert!(!style_at(&sums, at).add_modifier.contains(Modifier::BOLD));
    }
}

#[test]
fn a_secret_is_never_styled_or_folded() {
    let mut editor = drafted("> **a** b\n> c\n> d\n> e\n> f\n> g\n\nh", 0);
    editor.secret = true;
    let drawn = shown(&editor, 80);
    assert_eq!(drawn.len(), 8);
    assert!(drawn[0].chars().all(|c| c == '•'));
}

#[test]
fn a_quote_draws_on_a_rail_and_its_wrapped_lines_keep_it() {
    let editor = drafted("> one two three four five six\n\nreply", 35);
    assert_eq!(
        shown(&editor, 18),
        ["│ one two three ", "│ four five six", "", "reply"]
    );
}

#[test]
fn a_long_quote_folds_once_the_cursor_is_outside_it() {
    let quote = "> one\n> two\n> three\n> four\n> five\n> six";
    let words = format!("{quote}\n\nreply");
    let end = words.chars().count();
    let outside = drafted(&words, end);
    assert_eq!(
        shown(&outside, 40),
        ["│ one", "│ two", "│ … 4 more lines", "", "reply"]
    );
    assert_eq!(
        cursor_at(&outside, 40, 2, 3),
        12,
        "a click on the count opens at the first row it hides"
    );

    let inside = drafted(&words, 3);
    assert_eq!(
        shown(&inside, 40).len(),
        8,
        "the cursor is in it: all of it shows"
    );
}

#[test]
fn a_short_quote_never_folds() {
    let words = "> one\n> two\n> three\n> four\n\nreply";
    assert_eq!(shown(&drafted(words, words.len()), 40).len(), 6);
}

#[test]
fn one_backspace_below_a_folded_quote_removes_it() {
    let quote = "> one\n> two\n> three\n> four\n> five\n> six";
    let words = format!("{quote}\n\nreply");
    let start = quote.chars().count() + 2;
    let mut editor = drafted(&words, start);
    let (from, to) = folded_quote_above(&editor, 40).expect("a folded quote above");
    editor.remove_chars(from, to);
    assert_eq!(editor.text(), "reply");

    // Mid-reply, Backspace is an ordinary Backspace.
    let editor = drafted(&words, start + 2);
    assert_eq!(folded_quote_above(&editor, 40), None);
}

#[test]
fn a_sent_quote_draws_on_a_rail() {
    let state = chat(vec![prompt_item(1, "> what you said\n\nOh really?")]);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let drawn = rows(&mut view, &state);
    let row = &drawn[usize::from(find(&drawn, "what").1)];
    assert!(row.contains("│ what you said"), "{row}");
    assert!(!drawn.join("\n").contains("> what"));
}

#[test]
fn the_selected_words_are_lit_and_nothing_else_is() {
    for theme in [theme(), Theme::dark(crate::theme::ColorMode::Ansi)] {
        let state = chat(vec![agent_says(1, PARAGRAPH)]);
        let mut view = ChatView::new(b"agent".to_vec(), 0, false);
        let (buffer, _) = draw(&mut view, &state, 0, WIDE, TALL, theme);
        let drawn: Vec<String> = text(&buffer).lines().map(str::to_owned).collect();
        let (x, y) = find(&drawn, "bravo");
        let left = MouseButton::Left;
        view.mouse(&state, mouse(MouseEventKind::Down(left), (x, y)), theme);
        view.mouse(&state, mouse(MouseEventKind::Drag(left), (x + 4, y)), theme);
        let (buffer, _) = draw(&mut view, &state, 0, WIDE, TALL, theme);
        let lit = theme.selection().bg;
        assert!(lit.is_some());
        assert_eq!(buffer[(x, y)].bg, lit.unwrap());
        assert_eq!(buffer[(x + 4, y)].bg, lit.unwrap());
        assert_ne!(buffer[(x + 5, y)].bg, lit.unwrap());
        assert_ne!(buffer[(x.saturating_sub(1), y)].bg, lit.unwrap());
    }
}

#[test]
fn a_selection_shows_on_a_palette_with_no_surfaces_to_paint() {
    let terminal = crate::theme::TerminalColors {
        background: (16, 16, 16),
        foreground: (230, 230, 230),
        ansi: [(128, 128, 128); 16],
    };
    let adopted = Theme::from_terminal(terminal, crate::theme::ColorMode::Ansi);
    assert!(
        adopted
            .selection()
            .add_modifier
            .contains(Modifier::REVERSED),
        "{:?}",
        adopted.selection()
    );
}

/// A chat with a changed file in its side pane, drawn, and where that
/// file's name is.
fn pane_with_a_file() -> (SessionState, ChatView, (u16, u16)) {
    let state = chat_in_git(replies(1, 2), Some("main"));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.open_pane();
    view.changes = Some(crate::chat::Fetched {
        key: view
            .wants_changes(&state)
            .expect("the pane wants its files")
            .0,
        diff: working_tree_diff(),
    });
    let (buffer, _) = draw(&mut view, &state, 0, 110, TALL, theme());
    let drawn: Vec<String> = text(&buffer).lines().map(str::to_owned).collect();
    let at = find(&drawn, "notes.md");
    (state, view, at)
}

#[test]
fn a_click_on_a_side_pane_item_opens_it_on_release() {
    let (state, mut view, at) = pane_with_a_file();
    let left = MouseButton::Left;
    assert!(
        view.mouse(&state, mouse(MouseEventKind::Down(left), at), theme())
            .is_empty(),
        "nothing happens on the press"
    );
    assert!(matches!(
        view.mouse(&state, mouse(MouseEventKind::Up(left), at), theme()).as_slice(),
        [ChatEffect::ReviewAt(path)] if path == "notes.md"
    ));
}

#[test]
fn a_drag_from_a_side_pane_item_selects_it_instead_of_opening_it() {
    let (state, mut view, from) = pane_with_a_file();
    let left = MouseButton::Left;
    let to = (from.0 + 7, from.1);
    view.mouse(&state, mouse(MouseEventKind::Down(left), from), theme());
    view.mouse(&state, mouse(MouseEventKind::Drag(left), to), theme());
    // No frame between the drag and the release: they arrived together.
    let effects = view.mouse(&state, mouse(MouseEventKind::Up(left), to), theme());
    assert_eq!(copied(&effects), "notes.md");
    let (buffer, _) = draw(&mut view, &state, 0, 110, TALL, theme());
    let keys = keys_of(&text(&buffer));
    assert!(keys.contains("esc clear"), "{keys}");
    assert!(
        !keys.contains("r reply"),
        "nothing in the pane to reply to: {keys}"
    );
}

#[test]
fn a_shorter_terminal_lets_a_review_selection_go_without_a_crash() {
    let state = chat(replies(1, 3));
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    view.open_review_at(working_tree_diff(), PATCH.into(), None);
    let drawn = rows(&mut view, &state);
    let (x, y) = find(&drawn, REVIEW_CODE);
    drag(&mut view, &state, (x, y), (x + 4, y));
    draw(&mut view, &state, 0, WIDE, 8, theme());
    let (buffer, _) = draw(&mut view, &state, 0, WIDE, 8, theme());
    assert!(
        !keys_of(&text(&buffer)).contains("esc clear"),
        "{}",
        text(&buffer)
    );
    let effects = view.key(&state, key(KeyCode::Char('c')), theme());
    assert!(effects.is_empty(), "the selection is gone: {effects:?}");
}

#[test]
fn the_settings_page_turns_copying_off_and_on() {
    use crate::settings_screen::{SettingsEffect, SettingsScreen};
    let settings = wire::Settings {
        ..Default::default()
    };
    let mut page = SettingsScreen::new(settings, None, "Mac").copying_on_select(true);
    let copying = (0..12).find(|_| {
        let hit = page.key(key(KeyCode::Enter));
        if matches!(hit.as_slice(), [SettingsEffect::CopyOnSelect(false)]) {
            return true;
        }
        page.key(key(KeyCode::Esc));
        page.key(key(KeyCode::Down));
        false
    });
    assert!(copying.is_some(), "no row turned copying off");
    assert_eq!(
        page.key(key(KeyCode::Enter)),
        [SettingsEffect::CopyOnSelect(true)]
    );
}

#[test]
fn a_click_past_a_wrapped_row_lands_after_its_last_character() {
    let editor = drafted("abcdefghijklmnop", 0);
    assert_eq!(cursor_at(&editor, 12, 0, 11), 10);
}

#[test]
fn a_sent_message_reads_as_markdown_and_opens_its_links() {
    let state = chat(vec![prompt_item(
        1,
        "a **bold** word, [docs](https://example.com)",
    )]);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let drawn = rows(&mut view, &state);
    assert!(!drawn.join("\n").contains("**"), "{}", drawn.join("\n"));
    let (x, y) = find(&drawn, "bold");
    let (buffer, _) = draw(&mut view, &state, 0, WIDE, TALL, theme());
    assert!(buffer[(x, y)].modifier.contains(Modifier::BOLD));
    let link = find(&drawn, "docs");
    let left = MouseButton::Left;
    view.mouse(&state, mouse(MouseEventKind::Down(left), link), theme());
    assert!(matches!(
        view.mouse(&state, mouse(MouseEventKind::Up(left), link), theme())
            .as_slice(),
        [ChatEffect::OpenUrl(_)]
    ));
}

#[test]
fn a_drag_over_an_opened_edit_copies_its_code_and_not_its_gutter() {
    use wire::claude_item::Kind as K;
    let mut write = item(2, Some("unused"));
    write.body = wire::ClaudeItem {
        kind: Some(K::Tool(wire::ToolCall {
            name: "Write".into(),
            state: ToolState::Succeeded as i32,
            class: ToolClass::Consequential as i32,
            input: Some(interpret::tool_input(
                "Write",
                "",
                &serde_json::json!({
                    "file_path": "src/a.rs",
                    "content": "let one = 1;\n\nlet two = 2;",
                }),
                "",
            )),
            ..Default::default()
        })),
    }
    .encode_to_vec();
    let state = chat(vec![write]);
    let mut view = ChatView::new(b"agent".to_vec(), 0, false);
    let heading = find(&rows(&mut view, &state), "a.rs");
    let left = MouseButton::Left;
    view.mouse(&state, mouse(MouseEventKind::Down(left), heading), theme());
    view.mouse(&state, mouse(MouseEventKind::Up(left), heading), theme());
    let drawn = rows(&mut view, &state);
    let (_, top) = find(&drawn, "let one");
    let (x, bottom) = find(&drawn, "let two");
    let end = x + "let two = 2;".len() as u16 - 1;
    let effects = drag(&mut view, &state, (0, top), (end, bottom));
    assert_eq!(copied(&effects), "let one = 1;\n\nlet two = 2;");
}
