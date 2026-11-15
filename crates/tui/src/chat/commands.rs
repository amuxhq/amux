//! The agent's commands offered while a draft is one "/word", or ends in a
//! Codex "$word": a flyover rising from the composer like the settings', so
//! a command can be found without knowing its name. Which commands match,
//! and what a pick leaves in the draft, are the shared view's
//! ([`ui_view::command_matches`]); this draws them and moves among them.

use ratatui::text::{Line, Span};
use ui_view::{CommandMatch, CommandView};

use crate::text::{self, push};
use crate::theme::Theme;

/// Matches shown at once; the rest scroll.
const SHOWN: usize = 8;
/// The narrowest the flyover is drawn, inside its border.
const INNER_LEAST: usize = 24;
/// The widest it is drawn, inside its border: as wide as the composer it
/// rises from on most terminals, and no wider on a very wide one.
const INNER_MOST: usize = 116;
/// The widest the name column grows: one plugin command's long name and
/// hint would otherwise push every description to the edge. A longer one
/// loses its hint, then the end of its name.
const NAME_MOST: usize = 24;
/// Columns between a name and its description.
const GAP: usize = 3;
/// Less room than this for a description drops them all.
const DESCRIPTION_LEAST: usize = 12;

/// Which match is lit, and whether the person closed the flyover, for the
/// draft they belong to: an edit starts the choice afresh.
#[derive(Debug, Default)]
pub struct Menu {
    /// The draft `selected` and `closed` were set on.
    draft: String,
    selected: usize,
    closed: bool,
}

impl Menu {
    /// The lit match among `matches` for `draft`: the one kept since the
    /// draft last changed, else the command whose whole name was typed,
    /// else the first.
    pub fn selected(&self, draft: &str, matches: &[CommandMatch]) -> usize {
        if self.draft == draft {
            return self.selected.min(matches.len().saturating_sub(1));
        }
        let word = draft
            .rfind(['/', '$'])
            .map_or("", |at| &draft[at + 1..])
            .to_lowercase();
        matches
            .iter()
            .position(|found| found.command.name.to_lowercase() == word)
            .unwrap_or(0)
    }

    /// Closed with Esc on this very draft.
    pub fn closed(&self, draft: &str) -> bool {
        self.closed && self.draft == draft
    }

    /// Moves the lit match by `step`, within `matches`.
    pub fn step(&mut self, draft: &str, matches: &[CommandMatch], step: isize) {
        let at = self.selected(draft, matches) as isize + step;
        self.selected = at.clamp(0, matches.len().saturating_sub(1) as isize) as usize;
        self.draft = draft.to_owned();
        self.closed = false;
    }

    pub fn close(&mut self, draft: &str) {
        self.draft = draft.to_owned();
        self.closed = true;
    }
}

/// Enter runs the lit command rather than only filling it in: a "/"
/// command that needs nothing after its name. One whose hint names a
/// required argument ("<name>") is filled in to finish, and a "$" skill is
/// a mention inside a message, never sent alone.
pub fn enter_runs(command: &CommandView) -> bool {
    command.sigil == "/" && !command.argument_hint.trim_start().starts_with('<')
}

/// The keys while the flyover is open, for the line under the composer.
pub fn hints(lit: &CommandView) -> Vec<(&'static str, &'static str)> {
    if enter_runs(lit) {
        vec![
            ("enter", "run"),
            ("tab", "fill in"),
            ("↑↓", "move"),
            ("esc", "close"),
        ]
    } else {
        vec![("enter", "fill in"), ("↑↓", "move"), ("esc", "close")]
    }
}

/// "/name <hint>": a command as its column shows it.
fn name_words(command: &CommandView) -> (String, String) {
    let hint = command.argument_hint.trim();
    let hint = if hint.is_empty() {
        String::new()
    } else {
        format!(" {hint}")
    };
    (format!("{}{}", command.sigil, command.name), hint)
}

/// The flyover's lines in its border, at most `room` columns wide, and
/// which match each line shows. Its width comes from every command of the
/// sigil being typed, not just the matches, so it holds still while the
/// person types.
pub fn panel(
    commands: &[CommandView],
    matches: &[CommandMatch],
    selected: usize,
    room: usize,
    theme: Theme,
) -> (Vec<Line<'static>>, Vec<Option<usize>>) {
    let Some(first) = matches.first() else {
        return (Vec::new(), Vec::new());
    };
    let sigil = first.command.sigil.as_str();
    let family = || commands.iter().filter(|command| command.sigil == sigil);
    let name_column = family()
        .map(|command| {
            let (name, hint) = name_words(command);
            text::str_width(&name) + text::str_width(&hint)
        })
        .max()
        .unwrap_or(0)
        .min(NAME_MOST);
    let described = family()
        .map(|command| text::str_width(&command.description))
        .max()
        .unwrap_or(0);
    // The pointer's two columns, the name, the gap and the description.
    let wanted = 2 + name_column + if described > 0 { GAP + described } else { 0 };
    let inner = wanted
        .clamp(INNER_LEAST, INNER_MOST)
        .min(room.saturating_sub(4).max(8));
    let name_room = name_column.min(inner.saturating_sub(2));
    let description_room = inner.saturating_sub(2 + name_room + GAP);
    let descriptions = description_room >= DESCRIPTION_LEAST;

    let mut body = Vec::new();
    let mut rows = Vec::new();
    let selected = selected.min(matches.len() - 1);
    let from = selected.saturating_sub(SHOWN - 1);
    for (at, found) in matches.iter().enumerate().skip(from).take(SHOWN) {
        let lit = at == selected;
        let command = &found.command;
        let mut line = Line::from(Span::styled(
            if lit { "› " } else { "  " },
            theme.attention(),
        ));
        let (name, hint) = name_words(command);
        let name = text::ellipsize(&name, name_room);
        let hint_room = name_room.saturating_sub(text::str_width(&name));
        push(
            &mut line,
            name,
            if lit { theme.bright() } else { theme.text() },
            inner,
        );
        if hint_room > 0 && !hint.is_empty() {
            push(
                &mut line,
                text::ellipsize(&hint, hint_room),
                theme.faint(),
                inner,
            );
        }
        let description = command.description.trim();
        if descriptions && !description.is_empty() {
            text::pad_to(&mut line, 2 + name_room + GAP);
            push(
                &mut line,
                text::ellipsize(text::first_line(description), description_room),
                if lit { theme.muted() } else { theme.faint() },
                inner,
            );
        }
        body.push(line);
        rows.push(Some(at));
    }
    let below = matches.len().saturating_sub(from + SHOWN);
    if below > 0 {
        let mut more = Line::from(Span::raw("  "));
        push(&mut more, format!("… {below} more"), theme.faint(), inner);
        body.push(more);
        rows.push(None);
    }
    let title = if sigil == "$" { "Skills" } else { "Commands" };
    let lines = crate::panel::bordered(title, body, inner, &[], None, theme);
    let rows = std::iter::once(None).chain(rows).chain([None]).collect();
    (lines, rows)
}
