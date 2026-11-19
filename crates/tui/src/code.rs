//! Code in the terminal's colours: the shared view's highlighting, each
//! token kind mapped to a style by the theme, for chat code blocks and edit
//! cards.
//!
//! A result that is not settled yet (a grammar still building, a streaming
//! block with lines not yet passed over) is drawn as it is, and the frame
//! notes it so the loop draws again soon; the next draw asks afresh.
//!
//! Every piece of code the terminal draws is highlighted here, with the
//! highlighter this thread draws with: the process's shared one, unless a
//! test has handed the thread one whose background work it runs itself, so
//! what a frame shows does not hang on how fast a background thread is.

use std::cell::{Cell, RefCell};

use ratatui::style::Style;
use ui_view::{CodeRun, HighlightedCode, Highlighter, PatchHunk};

use crate::theme::Theme;

thread_local! {
    /// Whether this frame drew code that a later frame colours more of.
    static UNSETTLED: Cell<bool> = const { Cell::new(false) };
    /// What the terminal highlights with on this thread.
    static HIGHLIGHTER: RefCell<Highlighter> = RefCell::new(Highlighter::shared().clone());
}

/// Starts a frame: nothing unsettled drawn yet.
pub(crate) fn begin_frame() {
    UNSETTLED.with(|unsettled| unsettled.set(false));
}

/// Whether the frame drawn last showed code still being coloured, so the
/// loop should draw again soon.
pub(crate) fn drew_unsettled() -> bool {
    UNSETTLED.with(Cell::get)
}

/// The highlighter code on this thread is drawn with.
pub(crate) fn highlighter() -> Highlighter {
    HIGHLIGHTER.with(|highlighter| highlighter.borrow().clone())
}

/// Draws this thread's code with `highlighter` from now on: for a test,
/// one built on [`ui_view::HeldJobs`].
#[cfg(test)]
pub(crate) fn draw_with(highlighter: Highlighter) {
    HIGHLIGHTER.with(|held| *held.borrow_mut() = highlighter);
}

/// A line of code as styled runs.
pub(crate) type Runs = Vec<(String, Style)>;

/// Notes a result's state for the frame, and hands its lines on.
fn noted(code: HighlightedCode) -> Vec<Vec<CodeRun>> {
    if !code.settled {
        UNSETTLED.with(|unsettled| unsettled.set(true));
    }
    code.lines
}

/// A code block's lines as styled runs, and whether they are settled.
/// `streaming` is for the block still being written; see
/// [`ui_view::highlight_block`].
pub(crate) fn block(code: &str, info: &str, streaming: bool, theme: Theme) -> (Vec<Runs>, bool) {
    let highlighted = ui_view::highlight_block(&highlighter(), code, info, streaming);
    let settled = highlighted.settled;
    let lines = noted(highlighted)
        .into_iter()
        .map(|line| {
            line.into_iter()
                .map(|run| {
                    let style = theme.syntax(run.kind);
                    (run.text, style)
                })
                .collect()
        })
        .collect();
    (lines, settled)
}

/// A file's change's code, for an edit card or a landed edit's head: one
/// line for each of its hunks' lines, hunk after hunk; see
/// [`ui_view::highlight_patch`].
pub(crate) fn patch(path: &str, hunks: &[PatchHunk]) -> Vec<Vec<CodeRun>> {
    noted(ui_view::highlight_patch(&highlighter(), path, hunks))
}

/// A diff line's runs in the line's style, each token in its colour.
pub(crate) fn diff_runs(runs: &[CodeRun], line: Style, theme: Theme) -> Runs {
    runs.iter()
        .map(|run| (run.text.clone(), theme.syntax_on_diff(line, run.kind)))
        .collect()
}

#[cfg(test)]
mod tests {
    use ratatui::text::Line;
    use ui_view::{FilePatch, LineKind, PatchHead, PatchHunk, PatchLine, TokenKind};

    use super::*;

    fn style_of(line: &Line<'_>, text: &str) -> Option<Style> {
        line.spans
            .iter()
            .find(|span| span.content == text)
            .map(|span| span.style)
    }

    #[test]
    fn a_landed_edit_colours_its_code_over_the_diff_tints() {
        highlighter().preload();
        let theme = Theme::default();
        let line = |number, kind, text: &str| PatchLine {
            number: Some(number),
            kind,
            text: text.into(),
        };
        let patch = PatchHead {
            hunks: vec![PatchHunk {
                start: None,
                lines: vec![
                    line(1, LineKind::Context, "fn ready() -> bool {"),
                    line(2, LineKind::Removed, "    false"),
                    line(2, LineKind::Added, "    true"),
                    line(3, LineKind::Context, "}"),
                ],
            }],
            more: 0,
        };
        let lines = crate::chat::rows::patch_lines(&patch, "src/ready.rs", true, 'b', 80, theme);
        let keyword = |base: Style| theme.syntax_on_diff(base, TokenKind::Keyword);
        assert_eq!(
            style_of(&lines[0], "fn"),
            Some(keyword(theme.diff_context()))
        );
        assert_eq!(
            style_of(&lines[0], "ready"),
            Some(theme.syntax_on_diff(theme.diff_context(), TokenKind::Function))
        );
        let removed = theme.diff_removed();
        assert_eq!(style_of(&lines[1], "-"), Some(removed));
        assert_eq!(
            style_of(&lines[1], "false"),
            Some(theme.syntax_on_diff(removed, TokenKind::Number))
        );
        let added = theme.diff_added();
        assert_eq!(
            style_of(&lines[2], "true"),
            Some(theme.syntax_on_diff(added, TokenKind::Number))
        );
        // The token's colour sits on the line's tint.
        assert_eq!(theme.syntax_on_diff(added, TokenKind::Number).bg, added.bg);
        assert_ne!(theme.syntax_on_diff(added, TokenKind::Number).fg, added.fg);

        // A file no grammar reads stays in the diff's own inks.
        let plain = crate::chat::rows::patch_lines(&patch, "notes.txt", true, 'b', 80, theme);
        assert_eq!(style_of(&plain[2], "    true"), Some(added));
    }

    /// An edit whose code is still being coloured (here, its grammar still
    /// building) marks the frame unsettled, so the loop draws again soon;
    /// once the work is done, the next frame is settled. Both an edit card
    /// and a landed edit's patch.
    #[test]
    fn an_edit_still_being_coloured_asks_for_another_frame() {
        let held = ui_view::HeldJobs::default();
        draw_with(held.highlighter());
        let theme = Theme::default();
        let line = |number, kind, text: &str| PatchLine {
            number: Some(number),
            kind,
            text: text.into(),
        };
        let lines = vec![
            line(1, LineKind::Context, "fn ready() -> bool {"),
            line(2, LineKind::Added, "    true"),
        ];
        let hunks = vec![PatchHunk { start: None, lines }];
        let patch = PatchHead {
            hunks: hunks.clone(),
            more: 0,
        };
        let card = ui_view::AskCard {
            kind: wire::Kind::ClaudeSdk,
            key: "ask".into(),
            item_key: "item".into(),
            position: 1,
            count: 1,
            body: ui_view::AskBody::Edit {
                path: "src/ready.rs".into(),
                files: 1,
                added: 1,
                removed: 0,
                patch: vec![FilePatch { file: None, hunks }],
                line: Some(2),
                reason: String::new(),
                created: Some(false),
            },
            choices: Vec::new(),
            question_note: false,
            question_skip: false,
            question_reply: false,
            stops_turn: true,
            default_to_no: false,
            state: ui_view::CardState::Open,
        };
        let ask = crate::chat::ask::AskUi::default();
        let frames: [(&str, &dyn Fn()); 2] = [
            ("the edit card", &|| {
                ask.box_lines(&card, 80, theme);
            }),
            ("the landed edit", &|| {
                crate::chat::rows::patch_lines(&patch, "src/ready.rs", true, 'b', 80, theme);
            }),
        ];
        for (what, draw) in frames {
            begin_frame();
            draw();
            assert!(
                drew_unsettled(),
                "{what} drawn plain asks to be drawn again"
            );
        }
        held.run();
        for (what, draw) in frames {
            begin_frame();
            draw();
            assert!(!drew_unsettled(), "{what} coloured is settled");
        }
    }
}
