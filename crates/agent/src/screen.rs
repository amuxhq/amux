//! Terminal Claude's screen, kept as Claude draws it, for what only the
//! screen says yet.
//!
//! Two things Claude shows before any hook or transcript row reports them:
//! the permission its footer names (Shift+Tab changes it at once on screen,
//! but Claude records the change only with the next prompt), and what its
//! plan approval menu's first entry switches to (auto mode where Claude
//! offers it, accept edits otherwise). Claude redraws only the cells that
//! change, so a phrase can be read only off the whole screen, never off
//! one chunk of output.

/// Columns kept at least, whatever the terminal's width. Claude wraps its
/// own lines to the width it was told, so a wider screen loses nothing,
/// and a recording replayed into a narrower terminal still lands where it
/// was drawn.
const MIN_COLS: u16 = 240;

/// What a read of the screen found that it had not reported before.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Seen {
    /// The footer names this permission, in Claude's own spelling.
    Permission(&'static str),
    /// A plan approval menu is open, and its first entry switches to this
    /// permission.
    PlanSwitch(&'static str),
}

/// The footer's words for each permission, after its mode glyph.
const FOOTER: &[(&str, &str)] = &[
    ("accept edits on", "acceptEdits"),
    ("plan mode on", "plan"),
    ("auto mode on", "auto"),
    ("bypass permissions on", "bypassPermissions"),
    ("manual mode on", "default"),
];

/// The glyphs a footer's mode starts with.
const FOOTER_GLYPHS: &[&str] = &["\u{23f5}\u{23f5}", "\u{23f8}", "\u{23f5}"];

/// The plan menu's first entry, and the permission it switches to.
const PLAN_SWITCH: &[(&str, &str)] = &[
    ("1. Yes, and use auto mode", "auto"),
    ("1. Yes, auto-accept edits", "acceptEdits"),
];

pub(crate) struct Screen {
    parser: vt100::Parser,
    permission: Option<&'static str>,
    plan_switch: Option<&'static str>,
}

impl Screen {
    pub(crate) fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows.max(1), cols.max(MIN_COLS), 0),
            permission: None,
            plan_switch: None,
        }
    }

    pub(crate) fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.set_size(rows.max(1), cols.max(MIN_COLS));
    }

    /// Draws `bytes`, and returns what the screen now says that differs
    /// from what it said when last read. A plan menu is reported each time
    /// one opens.
    pub(crate) fn watch(&mut self, bytes: &[u8]) -> Vec<Seen> {
        self.parser.process(bytes);
        let (_, cols) = self.parser.screen().size();
        let rows: Vec<String> = self.parser.screen().rows(0, cols).collect();
        let mut seen = Vec::new();
        let permission = rows.iter().rev().find_map(|row| footer(row));
        if permission.is_some() && permission != self.permission {
            self.permission = permission;
            seen.extend(permission.map(Seen::Permission));
        }
        let plan_switch = rows.iter().rev().find_map(|row| plan_switch(row));
        if plan_switch != self.plan_switch {
            self.plan_switch = plan_switch;
            seen.extend(plan_switch.map(Seen::PlanSwitch));
        }
        seen
    }
}

/// The permission a footer row names, when `row` is one.
fn footer(row: &str) -> Option<&'static str> {
    let row = row.trim_start();
    let rest = FOOTER_GLYPHS
        .iter()
        .find_map(|glyph| row.strip_prefix(glyph))?
        .trim_start();
    FOOTER
        .iter()
        .find(|(words, _)| rest.starts_with(words))
        .map(|(_, permission)| *permission)
}

/// The permission a plan menu's first entry switches to, when `row` is
/// that entry. The selection marker may stand before it.
fn plan_switch(row: &str) -> Option<&'static str> {
    let row = row.trim_start().trim_start_matches('\u{276f}').trim_start();
    PLAN_SWITCH
        .iter()
        .find(|(entry, _)| row.starts_with(entry))
        .map(|(_, permission)| *permission)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claude 2.1.251's footer turning from `manual mode on` to `accept
    /// edits on` after Shift+Tab, as the mode cycle recording drew it: only
    /// the changed cells are written.
    #[test]
    fn a_footer_redrawn_cell_by_cell_is_read_whole() {
        let mut screen = Screen::new(24, 80);
        let first = screen.watch(b"\r\n\r\n\r\n\r\n\x1b[2C\xe2\x8f\xb8 manual mode on \xc2\xb7 \xe2\x86\x90 for agents\r\n\x1b[2C\x1b[1A");
        assert_eq!(first, vec![Seen::Permission("default")]);
        let cycled = screen.watch(b"\r\x1b[2C\xe2\x8f\xb5\xe2\x8f\xb5 \x1b[7Gccept e\x1b[15Gits on\x1b[22G(shift+tab to\x1b[36Gcycle)");
        assert_eq!(cycled, vec![Seen::Permission("acceptEdits")]);
        assert!(screen.watch(b"").is_empty(), "reported once");
    }

    #[test]
    fn prose_naming_a_mode_is_not_a_footer() {
        let mut screen = Screen::new(24, 80);
        assert!(
            screen
                .watch(b"I turned plan mode on for you.\r\n")
                .is_empty()
        );
    }

    /// Claude 2.1.251 on Sonnet 5, offering auto mode, as the plan
    /// approval recording drew its menu.
    #[test]
    fn the_plan_menu_names_its_switch() {
        let mut screen = Screen::new(24, 80);
        let drawn = b"\x1b[2G\xe2\x9d\xaf\x1b[4G1.\x1b[7GYes,\x1b[12Gand\x1b[16Guse\x1b[20Gauto\x1b[25Gmode\r\r\n\x1b[4G2.\x1b[7GYes,\x1b[12Gmanually\x1b[21Gapprove\x1b[29Gedits\r\r\n";
        assert_eq!(screen.watch(drawn), vec![Seen::PlanSwitch("auto")]);
        assert!(screen.watch(b"\x1b[2G").is_empty(), "reported once");
        let mut screen = Screen::new(24, 80);
        assert_eq!(
            screen.watch(b"1. Yes, auto-accept edits\r\n2. Yes, manually approve edits\r\n"),
            vec![Seen::PlanSwitch("acceptEdits")]
        );
    }
}
