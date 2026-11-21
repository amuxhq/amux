//! A comparison's changed files as a list, under their folders as the
//! shared view groups them, each file's lines added and removed at the
//! right in the theme's green and red: the Overview's Changes section,
//! which places it and sets its width.

use ratatui::text::Line;
use ui_view::{Changes, FileStatus};

use crate::text::{self, pad_to, push};
use crate::theme::Theme;

/// The list drawn, `width` columns wide from its own left edge.
#[derive(Default)]
pub struct ChangesOut {
    pub lines: Vec<Line<'static>>,
    /// Each file's line and its path, in the list's order.
    pub files: Vec<(usize, String)>,
    /// The directory lines: a group scrolled under the top edge keeps its
    /// directory pinned.
    pub dir_lines: Vec<usize>,
}

impl ChangesOut {
    /// The directory line to pin over line `row`, the first under the top
    /// edge: the one naming the group `row` sits in, when it has scrolled
    /// out above. The next group's own line takes over as it reaches `row`.
    pub fn pin(&self, row: usize) -> Option<usize> {
        let dir = self.dir_lines.iter().copied().rfind(|line| *line <= row)?;
        (dir < row && row < self.lines.len()).then_some(dir)
    }
}

/// Files under their folder: a faint line naming it, shortened in the
/// middle so both ends survive, then each file's name two columns in, after
/// its old path when it was renamed, with its lines added and removed at
/// the right; a blank line between folders.
pub fn changes_lines(changes: &Changes, width: usize, theme: Theme) -> ChangesOut {
    let mut out = ChangesOut::default();
    for (n, folder) in changes.folders.iter().enumerate() {
        if n > 0 {
            out.lines.push(Line::default());
        }
        let indent = if folder.path.is_empty() {
            0
        } else {
            let mut line = Line::default();
            push(
                &mut line,
                shorten_middle(&folder.path, width),
                theme.faint(),
                width,
            );
            out.dir_lines.push(out.lines.len());
            out.lines.push(line);
            2
        };
        for file in &folder.files {
            // A zero side is left out: "+1", or "−5" for a pure deletion.
            let added = if file.added > 0 {
                format!("+{}", file.added)
            } else {
                String::new()
            };
            let removed = match (file.removed, added.is_empty()) {
                (0, _) => String::new(),
                (n, true) => format!("\u{2212}{n}"),
                (n, false) => format!(" \u{2212}{n}"),
            };
            let counts = format!("{added}{removed}");
            let room = width.saturating_sub(indent + text::str_width(&counts) + 2);
            // A renamed file names where it came from first, so a narrow
            // list cuts that and keeps the name.
            let label = match (&file.status, &file.old_path) {
                (FileStatus::Renamed, Some(old)) => format!("{old} \u{2192} {}", file.name),
                _ => file.name.clone(),
            };
            let mut line = Line::default();
            pad_to(&mut line, indent);
            push(&mut line, shorten_left(&label, room), theme.text(), width);
            pad_to(&mut line, width.saturating_sub(text::str_width(&counts)));
            // Green and red from the terminal's palette.
            push(&mut line, added, theme.ok(), width);
            push(&mut line, removed, theme.error(), width);
            out.files.push((out.lines.len(), file.path.clone()));
            out.lines.push(line);
        }
    }
    out
}

/// The paths in the list's order.
pub fn ordered(changes: &Changes) -> Vec<String> {
    changes
        .folders
        .iter()
        .flat_map(|folder| &folder.files)
        .map(|file| file.path.clone())
        .collect()
}

/// A directory in at most `max` columns, cut in the middle at directories
/// so its first and last parts survive: "apps/…/Sources/Chat/".
fn shorten_middle(dir: &str, max: usize) -> String {
    if text::str_width(dir) <= max {
        return dir.to_owned();
    }
    let parts: Vec<&str> = dir.trim_end_matches('/').split('/').collect();
    if let Some((first, rest)) = parts.split_first() {
        for keep in (1..rest.len()).rev() {
            let tail = rest[rest.len() - keep..].join("/");
            let shown = format!("{first}/…/{tail}/");
            if text::str_width(&shown) <= max {
                return shown;
            }
        }
    }
    shorten_left(dir, max)
}

/// `path` in at most `max` columns, cut from the left at a directory so the
/// file's name survives: "…/src/session.rs"; a name too long on its own
/// keeps its start and extension.
pub fn shorten_left(path: &str, max: usize) -> String {
    if text::str_width(path) <= max {
        return path.to_owned();
    }
    let mut at = 0;
    while let Some(slash) = path[at..].find('/') {
        at += slash + 1;
        let tail = &path[at..];
        if text::str_width(tail) + 2 <= max {
            return format!("…/{tail}");
        }
    }
    // Only the name is left: keep its start and its extension, which tell
    // names apart better than their ends ("Composer….swift").
    let name = &path[at..];
    let ext = name
        .rfind('.')
        .filter(|dot| *dot > 0)
        .map_or("", |dot| &name[dot..]);
    let head = max.saturating_sub(text::str_width(ext) + 1);
    if head >= 3 {
        let start: String = name.chars().take(head).collect();
        return format!("{start}…{ext}");
    }
    let keep = max.saturating_sub(1);
    let skip = name.chars().count().saturating_sub(keep);
    format!("…{}", name.chars().skip(skip).collect::<String>())
}
