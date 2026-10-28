//! The ask card, docked where the composer was. One anatomy for every
//! kind: what it wants and "1 of 3", the subject verbatim, then choices
//! stated as outcomes, the likely one first. Stop is always in the menu:
//! it is the interrupt, which ends the turn and leaves the agent live.
//!
//! [`AskUi`] is this client's state for the head ask (the highlighted
//! choice, a note, question picks, form fields); it resets whenever the
//! head ask changes, so nothing is carried from one ask to another.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ui_view::{
    AskBody, AskCard, CardState, Choice, ChoiceOutcome, EditLine, FieldProblem, FileChangeView,
    FileRow, FormField as Field, FormFieldKind as FieldKind, FormValue, LineKind, PermissionGrant,
    QuestionResponse, QuestionView, Scope, answer_input, form_answer, form_problems,
    question_answer, reply_answer,
};

use crate::editor::Editor;
use crate::text::{self, push, push_right};
use crate::theme::Theme;

/// What a key on the card asks the chat to do.
#[derive(Clone, Debug, PartialEq)]
pub enum AskAction {
    None,
    /// Send this answer.
    Answer(Box<wire::Input>),
    /// The answer was not confirmed: send it again, or forget it.
    Resend,
    Discard,
    /// Hand the terminal to the agent's own interface.
    Attach,
    /// Open a link in the person's browser; the ask stays.
    OpenUrl(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
struct QuestionPick {
    selected: Vec<u32>,
    other: Option<String>,
    /// Left unanswered on purpose: the person chose Skip.
    skipped: bool,
    /// A multi-select confirmed with nothing ticked: an empty pick is its
    /// answer, as the provider's own Submit sends one.
    none: bool,
    /// The person's note on this question.
    note: String,
}

impl QuestionPick {
    fn answered(&self) -> bool {
        self.none || !self.selected.is_empty() || self.other.as_ref().is_some_and(|o| !o.is_empty())
    }

    /// Answered or skipped: nothing more to ask of it.
    fn done(&self) -> bool {
        self.answered() || self.skipped
    }

    fn response(&self) -> QuestionResponse {
        QuestionResponse {
            picked: self.selected.clone(),
            other: self.other.clone().filter(|other| !other.is_empty()),
            note: self.note.trim().to_owned(),
        }
    }
}

/// This client's state for the head ask.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AskUi {
    key: String,
    selected: usize,
    note: Editor,
    step: usize,
    picks: Vec<QuestionPick>,
    other: Editor,
    fields: Vec<Field>,
    /// Whether the person has the ask's keys, or has handed them to the
    /// composer for a moment to keep drafting.
    pub drafting: bool,
    /// The boxed ask's command, arguments or diff shown whole.
    show_all: bool,
    /// How far an opened diff taller than the box can hold is scrolled.
    diff_scroll: usize,
    /// The choice last sent, for the line the box shows until the agent
    /// confirms it.
    sent: Option<usize>,
    /// The boxed ask's deny note is open for typing.
    noting: bool,
    /// The terminal's rows, so a question's preview leaves the feed room.
    room: usize,
    /// The agent's own terminal can be attached, for what this client
    /// cannot answer.
    attach: bool,
    /// Why the open form field's value was refused, until it changes.
    invalid: Option<FieldProblem>,
    /// Per question, what was typed in its field but not answered with:
    /// kept when Tab moves on, so coming back shows it.
    drafts: Vec<String>,
    /// The current question's note is open for typing, in `note`.
    annotating: bool,
    /// "Reply instead" is open for typing, in `reply`.
    replying: bool,
    reply: Editor,
}

fn scope_words(scope: &Scope) -> String {
    match scope {
        Scope::Session => "for this session".into(),
        Scope::Project => "in this project".into(),
        Scope::ProjectShared => "in this project, for everyone".into(),
        Scope::User => "in every project".into(),
        Scope::Other(other) => other.clone(),
    }
}

/// What a decided permission allowed from then on, in words: "always
/// allowed cargo test in this project", "allowed commands starting with
/// curl".
pub(crate) fn grant_words(granted: &PermissionGrant) -> String {
    match granted {
        PermissionGrant::Claude {
            subjects,
            directories,
            mode,
            mode_name,
            destination,
        } => {
            if !subjects.is_empty() {
                format!(
                    "always allowed {} {}",
                    subjects.join(", "),
                    scope_words(destination)
                )
            } else if !directories.is_empty() {
                format!(
                    "allowed access to {} {}",
                    directories.join(", "),
                    scope_words(destination)
                )
            } else if !mode.is_empty() {
                format!("switched to {}", crate::words::named(mode_name, mode))
            } else {
                format!("always allowed {}", scope_words(destination))
            }
        }
        PermissionGrant::Session => "allowed for this session".into(),
        PermissionGrant::Always => "always allowed in every project".into(),
        PermissionGrant::CommandPrefix { words } => {
            format!("allowed commands starting with {}", words.join(" "))
        }
        PermissionGrant::NetworkHosts { hosts } if hosts.is_empty() => {
            "allowed network access".into()
        }
        PermissionGrant::NetworkHosts { hosts } => {
            format!("allowed network access to {}", hosts.join(", "))
        }
    }
}

/// A choice in words: what happens, never rule syntax.
pub fn choice_label(choice: &Choice) -> String {
    let label = match &choice.outcome {
        ChoiceOutcome::AllowOnce => "Allow once".to_owned(),
        ChoiceOutcome::AllowAlways {
            subjects,
            directories,
            mode,
            mode_name,
            scope,
        } => {
            if !subjects.is_empty() {
                format!(
                    "Always allow {} {}",
                    subjects.join(", "),
                    scope_words(scope)
                )
            } else if !directories.is_empty() {
                format!(
                    "Allow access to {} {}",
                    directories.join(", "),
                    scope_words(scope)
                )
            } else if !mode.is_empty() {
                format!("Switch to {}", crate::words::named(mode_name, mode))
            } else {
                format!("Always allow {}", scope_words(scope))
            }
        }
        ChoiceOutcome::AllowForSession => "Allow for this session".to_owned(),
        ChoiceOutcome::AllowSimilar { prefix } => {
            format!("Allow commands starting with {}", prefix.join(" "))
        }
        ChoiceOutcome::AllowNetwork { hosts } if hosts.is_empty() => {
            "Allow network access".to_owned()
        }
        ChoiceOutcome::AllowNetwork { hosts } => {
            format!("Allow network access to {}", hosts.join(", "))
        }
        ChoiceOutcome::Deny { stops: true } => "Deny and stop".to_owned(),
        ChoiceOutcome::Deny { stops: false } => "Deny".to_owned(),
        ChoiceOutcome::DenyAndStop => "Deny and stop".to_owned(),
        ChoiceOutcome::ApprovePlan { mode, mode_name } if !mode.is_empty() => {
            format!(
                "Approve and switch to {}",
                crate::words::named(mode_name, mode)
            )
        }
        ChoiceOutcome::ApprovePlan { .. } => "Approve".to_owned(),
        ChoiceOutcome::SendBack => "Send back".to_owned(),
        ChoiceOutcome::Submit => "Submit".to_owned(),
        ChoiceOutcome::Decline => "Decline".to_owned(),
        ChoiceOutcome::OpenLink => "I opened it".to_owned(),
        ChoiceOutcome::GrantForTurn => "Grant for this turn".to_owned(),
        ChoiceOutcome::GrantForSession => "Grant for this session".to_owned(),
    };
    if choice.takes_note {
        format!("{label}…")
    } else {
        label
    }
}

impl AskUi {
    /// Keeps this state if it belongs to `card`'s ask, else starts fresh.
    pub fn sync(&mut self, card: &AskCard) {
        if self.key == card.key {
            return;
        }
        *self = AskUi {
            key: card.key.clone(),
            // An ask that must not be approved by one stray key opens on
            // the way out, the refusal.
            selected: if card.default_to_no {
                box_choices(card).len().saturating_sub(1)
            } else {
                0
            },
            ..AskUi::default()
        };
        match &card.body {
            AskBody::Question(questions) => {
                self.picks = vec![QuestionPick::default(); questions.len()];
                self.other.secret = questions.first().is_some_and(|q| q.secret);
                self.noting = questions
                    .first()
                    .is_some_and(|q| q.options.is_empty() && q.allow_other);
            }
            AskBody::Form { fields, .. } => {
                // Each field starts where the shared view says: its default,
                // a toggle off.
                self.fields = fields.clone();
                self.picks = fields
                    .iter()
                    .map(|field| form_pick(field, &field.initial))
                    .collect();
                self.noting = form_questions(&self.fields)
                    .first()
                    .is_some_and(|q| q.options.is_empty() && q.allow_other);
            }
            _ => {}
        }
    }
}

/// Lines of a command or arguments shown before "… [Show All]".
const SUBJECT_LINES: usize = 6;
/// Diff lines shown before "[Full Diff]".
const DIFF_LINES: usize = 7;

/// What a click in the boxed ask reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxSpot {
    /// A choice, by its place in the box's list.
    Choice(usize),
    /// A question's tab, or the Review tab after the last.
    Tab(usize),
    ShowAll,
    FullDiff,
}

/// A place in the boxed ask a click reaches: its line, columns and what.
pub type Spot = (usize, (usize, usize), BoxSpot);

/// The boxed ask drawn: its lines inside the box, where the cursor sits
/// when the deny note has it, and what clicks reach.
pub struct BoxLines {
    pub lines: Vec<Line<'static>>,
    pub cursor: Option<(usize, usize)>,
    pub spots: Vec<Spot>,
}

/// A typed field in the box: its words and their ink, where a click on it
/// lands, whether the cursor sits after the words, and what follows them.
struct Typed<'a> {
    typed: &'a str,
    ink: Style,
    spot: Option<BoxSpot>,
    cursor: bool,
    after: Option<(String, Style)>,
}

/// How many of a typed field's last lines the box shows.
const FIELD_LINES: usize = 4;

impl BoxLines {
    /// Pushes `row`, which ends where a typed field starts, with the
    /// field's words: each of their lines on a row of its own, later rows
    /// set in to the field's column, so what is drawn before Enter is what
    /// is sent. Only the last [`FIELD_LINES`] show, the first of them
    /// marked "…" when earlier ones are cut. A click on any of its rows
    /// reaches `spot`; with `cursor` the cursor sits after the words, and
    /// `after` follows them.
    fn push_field(&mut self, mut row: Line<'static>, width: usize, field: Typed<'_>) {
        let Typed {
            typed,
            ink,
            spot,
            cursor,
            after,
        } = field;
        let col = text::line_width(&row);
        let room = width.saturating_sub(col + 1);
        let lines: Vec<&str> = typed.split('\n').collect();
        let cut = lines.len().saturating_sub(FIELD_LINES);
        let mut end = 0;
        for (i, part) in lines[cut..].iter().enumerate() {
            if i > 0 {
                self.spots
                    .extend(spot.map(|spot| (self.lines.len(), (0, width), spot)));
                self.lines.push(std::mem::take(&mut row));
                push(&mut row, " ".repeat(col), ink, width);
            }
            let shown = if i == 0 && cut > 0 {
                text_tail(&format!("…{part}"), room)
            } else {
                text_tail(part, room)
            };
            end = text::str_width(&shown);
            push(&mut row, shown, ink, width);
        }
        if cursor {
            self.cursor = Some((self.lines.len(), col + end));
        }
        if let Some((words, style)) = after {
            push(&mut row, words, style, width);
        }
        self.spots
            .extend(spot.map(|spot| (self.lines.len(), (0, width), spot)));
        self.lines.push(row);
    }
}

/// Whether the chat draws `card` in the composer's box: from the moment it
/// opens until the agent confirms the answer.
pub fn boxed(card: &AskCard) -> bool {
    !matches!(card.state, CardState::Dismissed)
}

/// Whether a choice is the box's one way to refuse: a denial, or sending a
/// plan back.
fn refuses(outcome: &ChoiceOutcome) -> bool {
    matches!(
        outcome,
        ChoiceOutcome::Deny { .. } | ChoiceOutcome::SendBack
    )
}

/// The box's choices, as indices into the card's: the ways to allow in the
/// agent's order, then the one way to refuse, last. Stopping is Ctrl+X's,
/// so "deny and stop" is not offered where denying lets the agent carry on.
fn box_choices(card: &AskCard) -> Vec<usize> {
    let mut list = Vec::new();
    let mut deny = None;
    for (i, choice) in card.choices.iter().enumerate() {
        match choice.outcome {
            ChoiceOutcome::Deny { .. } | ChoiceOutcome::SendBack => {
                deny.get_or_insert(i);
            }
            ChoiceOutcome::DenyAndStop => {}
            _ => list.push(i),
        }
    }
    list.extend(deny);
    list
}

/// A choice in the box, worded as what happens: "Yes", "Yes, and always
/// allow cargo test in this project", "No, and stop".
fn box_label(choice: &Choice, kind: wire::Kind) -> String {
    match &choice.outcome {
        ChoiceOutcome::ApprovePlan { mode, .. } if mode.is_empty() && kind == wire::Kind::Codex => {
            "Yes, implement this plan".to_owned()
        }
        ChoiceOutcome::AllowOnce => "Yes".to_owned(),
        ChoiceOutcome::AllowForSession => "Yes, and don't ask again this session".to_owned(),
        ChoiceOutcome::AllowAlways {
            mode, mode_name, ..
        } if !mode.is_empty() => {
            format!(
                "Yes, and switch to {}",
                crate::words::named(mode_name, mode)
            )
        }
        ChoiceOutcome::Deny { stops: true } | ChoiceOutcome::DenyAndStop => {
            "No, and stop".to_owned()
        }
        ChoiceOutcome::Deny { stops: false } => "No".to_owned(),
        ChoiceOutcome::ApprovePlan { mode, mode_name } if !mode.is_empty() => {
            format!(
                "Yes, and switch to {}",
                crate::words::named(mode_name, mode)
            )
        }
        ChoiceOutcome::ApprovePlan { .. } => "Yes, start building".to_owned(),
        ChoiceOutcome::SendBack => "No, keep planning".to_owned(),
        ChoiceOutcome::GrantForTurn => "Allow for this turn".to_owned(),
        ChoiceOutcome::GrantForSession => "Allow for this session".to_owned(),
        _ => {
            let words = choice_label(choice);
            let mut chars = words.chars();
            let first = chars
                .next()
                .map(|c| c.to_lowercase().to_string())
                .unwrap_or_default();
            format!("Yes, and {first}{}", chars.as_str())
        }
    }
}

/// A file of a patch of several, where its lines start: its path and what
/// the patch does to it.
fn file_words(file: &FileRow) -> String {
    match &file.change {
        FileChangeView::Created { .. } | FileChangeView::Writing { .. } => {
            format!("{} (new file)", file.path)
        }
        FileChangeView::Deleted => format!("{} (deleted)", file.path),
        FileChangeView::Moved { to } => format!("{} → {to}", file.path),
        FileChangeView::Edited => file.path.clone(),
    }
}

/// "cargo test -p ui-runtime…": the subject short enough for one line.
fn short_subject(card: &AskCard) -> String {
    let subject = match &card.body {
        AskBody::Command { command, .. } => text::first_line(command).to_owned(),
        AskBody::Edit { path, .. } => path.clone(),
        AskBody::Tool { server, tool, .. } => format!("{server} {tool}").trim().to_owned(),
        AskBody::Access {
            read,
            write,
            network,
            hosts,
            ..
        } => access_words(read, write, *network, hosts),
        _ => String::new(),
    };
    text::ellipsize(&subject, 48)
}

/// The step's asking verb.
fn asking_verb(card: &AskCard) -> &'static str {
    match &card.body {
        AskBody::Command { .. } => "Wants to run",
        AskBody::Edit {
            created: Some(true),
            ..
        } => "Wants to create",
        AskBody::Edit { created: None, .. } => "Wants to write",
        AskBody::Edit { .. } => "Wants to edit",
        AskBody::Plan { .. } => "Plan ready",
        AskBody::Access { .. } => "Wants access to",
        _ => "Wants to use",
    }
}

impl AskUi {
    /// Whether the boxed ask's note holds something, for Ctrl+C.
    pub fn box_note_text(&self, card: &AskCard) -> bool {
        self.in_box_note(card)
            && !(self.note.is_empty() && self.other.is_empty() && self.reply.is_empty())
    }

    /// Clears the boxed ask's note, as a kill.
    pub fn kill_box_note(&mut self) -> bool {
        let note = self.note.kill_all();
        let other = self.other.kill_all();
        let reply = self.reply.kill_all();
        note || other || reply
    }

    /// A paste into the boxed ask's note, when it has the keys.
    pub fn paste_box_note(&mut self, card: &AskCard, text: &str) {
        if !self.in_box_note(card) {
            return;
        }
        let questions = matches!(card.body, AskBody::Question(_) | AskBody::Form { .. });
        if self.replying {
            self.reply.insert_str(text);
        } else if questions && !self.annotating {
            self.other.insert_str(text);
        } else {
            self.note.insert_str(text);
        }
    }

    /// Whether the highlight is on a refusal that can carry a note.
    pub fn on_noted_deny(&self, card: &AskCard) -> bool {
        let list = box_choices(card);
        list.len().checked_sub(1) == Some(self.selected)
            && list
                .last()
                .and_then(|i| card.choices.get(*i))
                .is_some_and(|choice| refuses(&choice.outcome) && choice.takes_note)
    }

    /// Whether the deny note is open, with the keys.
    fn box_note(&self, card: &AskCard) -> bool {
        self.noting && self.on_noted_deny(card)
    }

    /// Whether the boxed ask's deny note, or a question's "Something else",
    /// has the keys.
    pub fn in_box_note(&self, card: &AskCard) -> bool {
        matches!(card.state, CardState::Open | CardState::Rejected(_))
            && match &card.body {
                AskBody::Question(questions) => {
                    self.annotating || self.replying || self.on_something_else(questions)
                }
                AskBody::Form { .. } => self.on_something_else(&form_questions(&self.fields)),
                _ => self.box_note(card),
            }
    }

    /// Sends the box's choice at `at`, the deny with its note.
    fn box_send(&mut self, card: &AskCard, at: usize) -> AskAction {
        let Some(&index) = box_choices(card).get(at) else {
            return AskAction::None;
        };
        let choice = &card.choices[index];
        let note = if choice.takes_note {
            self.note.text().trim().to_owned()
        } else {
            String::new()
        };
        match answer_input(card, &choice.answer, &note) {
            Some(input) => {
                self.sent = Some(index);
                AskAction::Answer(Box::new(input))
            }
            None => AskAction::None,
        }
    }

    /// One key on a boxed ask. ↑/↓ and j/k move, 1–9 answer at once,
    /// Enter answers with the highlighted choice. On a refusal that can
    /// carry a note, Tab opens the note; in it, Enter sends and Esc clears
    /// it and closes it. Esc at the list does nothing. Ctrl+X is the chat's.
    pub fn box_key(&mut self, card: &AskCard, key: KeyEvent) -> AskAction {
        match card.state {
            CardState::Open | CardState::Rejected(_) => {}
            CardState::NotConfirmed => {
                return match key.code {
                    KeyCode::Char('r') => AskAction::Resend,
                    KeyCode::Char('d') => AskAction::Discard,
                    _ => AskAction::None,
                };
            }
            // Cancel takes the held answer back, and the card is open again.
            CardState::Held => {
                return match key.code {
                    KeyCode::Char('c') => AskAction::Discard,
                    _ => AskAction::None,
                };
            }
            CardState::Sending | CardState::Dismissed => return AskAction::None,
        }
        match &card.body {
            AskBody::Question(questions) => return self.question_key_boxed(card, questions, key),
            AskBody::Form { .. } => {
                let questions = form_questions(&self.fields);
                return self.question_key_boxed(card, &questions, key);
            }
            AskBody::Link { .. } => return self.link_key(card, key),
            AskBody::Unanswerable { .. } => return self.unanswerable_key(key),
            _ => {}
        }
        let count = box_choices(card).len();
        if self.box_note(card) {
            match key.code {
                KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                    return self.box_send(card, self.selected);
                }
                KeyCode::Esc => {
                    self.note = Editor::default();
                    self.noting = false;
                }
                _ => {
                    self.note.key(key);
                }
            }
            return AskAction::None;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(count.saturating_sub(1))
            }
            KeyCode::Char(c @ '1'..='9') if !card.default_to_no => {
                let at = c as usize - '1' as usize;
                if at < count {
                    self.selected = at;
                    return self.box_send(card, at);
                }
            }
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                return self.box_send(card, self.selected);
            }
            KeyCode::Tab if self.on_noted_deny(card) => self.noting = true,
            // Esc only points at the way out, the refusal; Enter takes it.
            KeyCode::Esc => self.selected = count.saturating_sub(1),
            // F shows the whole command, arguments or diff in the box, and
            // cuts it again. The plan is in the feed, whole.
            KeyCode::Char('f') if !matches!(card.body, AskBody::Plan { .. }) => {
                self.show_all = !self.show_all;
                self.diff_scroll = 0;
            }
            _ => {}
        }
        AskAction::None
    }

    /// A click on a boxed ask.
    pub fn box_click(&mut self, card: &AskCard, spot: BoxSpot) -> AskAction {
        if !matches!(card.state, CardState::Open | CardState::Rejected(_)) {
            return AskAction::None;
        }
        match &card.body {
            AskBody::Question(questions) => return self.question_click(card, questions, spot),
            AskBody::Form { .. } => {
                let questions = form_questions(&self.fields);
                return self.question_click(card, &questions, spot);
            }
            AskBody::Link { .. } => return self.link_click(card, spot),
            AskBody::Unanswerable { .. } => {
                return if self.attach && matches!(spot, BoxSpot::Choice(0)) {
                    AskAction::Attach
                } else {
                    AskAction::None
                };
            }
            _ => {}
        }
        match spot {
            BoxSpot::Tab(_) => AskAction::None,
            BoxSpot::Choice(at) => {
                self.selected = at;
                self.box_send(card, at)
            }
            BoxSpot::ShowAll => {
                self.show_all = !self.show_all;
                AskAction::None
            }
            BoxSpot::FullDiff => {
                self.show_all = !self.show_all;
                self.diff_scroll = 0;
                AskAction::None
            }
        }
    }

    /// Whether the choices' long labels wrap: when the screen has room for
    /// them beside the box's own lines and one line of subject. On a short
    /// screen each choice keeps one line, so none is pushed off it.
    fn wraps_choices(&self, card: &AskCard, width: usize) -> bool {
        self.room == 0 || self.room >= BOX_CHROME + 3 + wrapped_choice_rows(card, width)
    }

    /// How many lines the box's choices take at `width`.
    fn choice_rows(&self, card: &AskCard, width: usize) -> usize {
        if self.wraps_choices(card, width) {
            wrapped_choice_rows(card, width)
        } else {
            box_choices(card).len()
        }
    }

    /// The boxed ask's lines, `width` columns wide: what it wants, the
    /// subject verbatim, the agent's reason, then the choices.
    pub fn box_lines(&self, card: &AskCard, width: usize, theme: Theme) -> BoxLines {
        let mut out = BoxLines {
            lines: Vec::new(),
            cursor: None,
            spots: Vec::new(),
        };
        let line = |words: String, style: Style| {
            let mut line = Line::default();
            push(&mut line, words, style, width);
            line
        };
        match &card.state {
            // Answered: one line until the agent confirms.
            CardState::Sending => {
                let chose = self
                    .sent
                    .and_then(|i| card.choices.get(i))
                    .map(|choice| &choice.outcome);
                let words = match chose {
                    _ if matches!(
                        card.body,
                        AskBody::Question(_) | AskBody::Form { .. } | AskBody::Link { .. }
                    ) =>
                    {
                        "Sending…".to_owned()
                    }
                    Some(ChoiceOutcome::Deny { .. } | ChoiceOutcome::DenyAndStop) => {
                        "Denying…".to_owned()
                    }
                    Some(ChoiceOutcome::SendBack) => "Sending the plan back…".to_owned(),
                    Some(ChoiceOutcome::ApprovePlan { .. }) => "Approving the plan…".to_owned(),
                    _ => format!("Allowing {}…", short_subject(card)),
                };
                out.lines.push(line(words, theme.faint()));
                return out;
            }
            // Why, wrapped inside the box: the when is the point.
            CardState::NotConfirmed => {
                for part in text::wrap(
                    "Your answer was not confirmed: the connection dropped before the agent replied.",
                    width.max(1),
                ) {
                    out.lines.push(line(part, theme.warning()));
                }
                out.lines
                    .push(line("r resend · d discard".into(), theme.faint()));
                return out;
            }
            CardState::Held => {
                for part in text::wrap(
                    "Your answer is held: the agent's host can't be reached. It sends when the host is back.",
                    width.max(1),
                ) {
                    out.lines.push(line(part, theme.warning()));
                }
                out.lines.push(line("c cancel".into(), theme.faint()));
                return out;
            }
            _ => {}
        }
        match &card.body {
            AskBody::Question(questions) => {
                return self.question_lines(card, questions, width, theme);
            }
            AskBody::Form { .. } => {
                let questions = form_questions(&self.fields);
                return self.question_lines(card, &questions, width, theme);
            }
            AskBody::Link {
                server,
                message,
                url,
            } => {
                return self.link_lines(server, message, url, width, theme);
            }
            AskBody::Unanswerable { reason } => {
                return self.unanswerable_lines(reason, width, theme);
            }
            _ => {}
        }
        let mut head = Line::from(Span::styled("● ", theme.attention()));
        push(&mut head, asking_verb(card), theme.text(), width);
        if card.count > 1 {
            push_right(
                &mut head,
                &format!("{} of {}", card.position, card.count),
                theme.faint(),
                width,
            );
        }
        out.lines.push(head);
        if let CardState::Rejected(reason) = &card.state {
            out.lines.push(line(
                format!("Not sent: {}", super::refusal_words(reason)),
                theme.error(),
            ));
        }

        // What a short terminal leaves the subject and the reason, once the
        // chat's header, the box's own lines and every choice have theirs:
        // the choices are what the ask is for, so the words give way.
        let words_room = if self.room == 0 {
            usize::MAX
        } else {
            self.room
                .saturating_sub(BOX_CHROME + out.lines.len() + 1 + self.choice_rows(card, width))
                .max(1)
        };
        let subject_lines = SUBJECT_LINES.min(words_room);
        // The subject, verbatim in the code colour: never cut short
        // without saying so.
        let capped = |out: &mut BoxLines, words: &str, show_all: bool| {
            let wrapped: Vec<String> = words
                .lines()
                .flat_map(|l| text::wrap(l, width.max(1)))
                .collect();
            let cut = !show_all && wrapped.len() > subject_lines;
            let take = if cut {
                subject_lines - 1
            } else {
                wrapped.len()
            };
            for part in wrapped.iter().take(take) {
                out.lines.push(line(part.clone(), theme.code()));
            }
            if cut {
                let mut more = Line::default();
                let hidden = wrapped.len() - take;
                let s = if hidden == 1 { "" } else { "s" };
                push(
                    &mut more,
                    format!("… {hidden} more line{s} "),
                    theme.faint(),
                    width,
                );
                let from = text::line_width(&more);
                push(&mut more, "[Show All]", theme.muted(), width);
                out.spots
                    .push((out.lines.len(), (from, from + 10), BoxSpot::ShowAll));
                out.lines.push(more);
            }
        };
        let reason = match &card.body {
            AskBody::Command {
                command,
                reason,
                description,
                ..
            } => {
                capped(&mut out, command, self.show_all);
                if reason.is_empty() {
                    description.clone()
                } else {
                    reason.clone()
                }
            }
            AskBody::Tool {
                server,
                tool,
                arguments,
            } => {
                let mut name = Line::default();
                if !server.is_empty() {
                    push(&mut name, format!("{server} "), theme.code(), width);
                }
                push(&mut name, tool.clone(), theme.code(), width);
                out.lines.push(name);
                if !arguments.is_empty() {
                    capped(&mut out, arguments, self.show_all);
                }
                String::new()
            }
            AskBody::Access {
                reason,
                read,
                write,
                network,
                hosts,
            } => {
                for part in text::wrap(&access_words(read, write, *network, hosts), width.max(1)) {
                    out.lines.push(line(part, theme.code()));
                }
                reason.clone()
            }
            AskBody::Edit {
                path,
                files,
                lines: patch,
                line: first,
                reason,
                ..
            } => {
                // The path and where in the file, then why, then the patch.
                // A path too long for the line loses its folders from the
                // front: the file's name is what the approval is about.
                let more = (*files > 1).then(|| {
                    let others = files - 1;
                    let s = if others == 1 { "" } else { "s" };
                    format!(" and {others} more file{s}")
                });
                let at = first.map(|line| format!(" · line {line}"));
                let after = more
                    .iter()
                    .chain(&at)
                    .map(|s| text::str_width(s))
                    .sum::<usize>();
                let mut name = Line::default();
                push(
                    &mut name,
                    text_tail(path, width.saturating_sub(after).max(1)),
                    theme.code(),
                    width,
                );
                for words in more.into_iter().chain(at) {
                    push(&mut name, words, theme.faint(), width);
                }
                out.lines.push(name);
                for part in text::wrap(reason, width.max(1)) {
                    out.lines.push(line(part, theme.faint()));
                }
                // The patch's first lines as the review page draws them; the
                // line beside the path says where its change starts, so the
                // hunks' starts are not drawn. A patch of several files names
                // each file where its lines start.
                let lines: Vec<(char, Style, String)> = patch
                    .iter()
                    .filter_map(|line| match line {
                        EditLine::Line(line) => Some(match line.kind {
                            LineKind::Added => ('+', theme.diff_added(), line.text.clone()),
                            LineKind::Removed => ('-', theme.diff_removed(), line.text.clone()),
                            LineKind::Context => (' ', theme.diff_context(), line.text.clone()),
                        }),
                        EditLine::File(file) => Some((' ', theme.code(), file_words(file))),
                        EditLine::Hunk { .. } => None,
                    })
                    .collect();
                // Opened ([Full Diff] or f), the whole patch shows in the
                // box, which grows to fill the chat and scrolls (wheel,
                // PgUp/PgDn) when even that is too little.
                // On a short screen the patch gives way to the choices, the
                // frame and the keys: it keeps what rows they leave, down to
                // the one saying how much more there is.
                let small = if self.room == 0 {
                    DIFF_LINES
                } else {
                    self.room
                        .saturating_sub(
                            BOX_CHROME + out.lines.len() + 1 + self.choice_rows(card, width),
                        )
                        .clamp(1, DIFF_LINES)
                };
                let opened = self.show_all && lines.len() > small;
                let cut = lines.len() > small && !opened;
                let rows = if self.room == 0 { 36 } else { self.room };
                let fits = rows
                    .saturating_sub(DIFF_CHROME + self.choice_rows(card, width))
                    .max(small.saturating_sub(1).max(1));
                let scroll = if opened {
                    self.diff_scroll.min(lines.len().saturating_sub(fits))
                } else {
                    0
                };
                let take = if cut {
                    small - 1
                } else if opened {
                    fits.min(lines.len())
                } else {
                    lines.len()
                };
                for (mark, style, body) in lines.iter().skip(scroll).take(take) {
                    let (mark, style) = (*mark, *style);
                    let mut row = Line::default();
                    push(
                        &mut row,
                        format!("{mark} {}", text::ellipsize(body, width.saturating_sub(2))),
                        style,
                        width,
                    );
                    text::fill(&mut row, style, width);
                    out.lines.push(row);
                }
                if cut {
                    let hidden = lines.len() - take;
                    let s = if hidden == 1 { "" } else { "s" };
                    let mut more = Line::default();
                    push(
                        &mut more,
                        format!("… {hidden} more line{s} "),
                        theme.faint(),
                        width,
                    );
                    let from = text::line_width(&more);
                    push(&mut more, "[Full Diff]", theme.muted(), width);
                    out.spots
                        .push((out.lines.len(), (from, from + 11), BoxSpot::FullDiff));
                    out.lines.push(more);
                } else if opened {
                    let mut less = Line::default();
                    if take < lines.len() {
                        push(
                            &mut less,
                            format!(
                                "lines {}–{} of {} · wheel or pgup/pgdn  ",
                                scroll + 1,
                                scroll + take,
                                lines.len()
                            ),
                            theme.faint(),
                            width,
                        );
                    }
                    let from = text::line_width(&less);
                    push(&mut less, "[Show Less]", theme.muted(), width);
                    out.spots
                        .push((out.lines.len(), (from, from + 11), BoxSpot::FullDiff));
                    out.lines.push(less);
                }
                String::new()
            }
            _ => String::new(),
        };
        if !reason.is_empty() {
            // Whatever room the subject left, the last line that fits
            // saying with "…" that there is more.
            let used = out.lines.len();
            let room = (self.room != 0).then(|| {
                self.room
                    .saturating_sub(BOX_CHROME + used + 1 + self.choice_rows(card, width))
            });
            let mut parts = text::wrap(&reason, width.max(1));
            if let Some(room) = room
                && parts.len() > room
            {
                parts.truncate(room);
                if let Some(last) = parts.last_mut() {
                    *last = text::ellipsize(&format!("{last}…"), width.max(1));
                }
            }
            for part in parts {
                out.lines.push(line(part, theme.faint()));
            }
        }
        out.lines.push(Line::default());

        // The choices, numbered, the highlighted one bright. The refusal is
        // last, and where it can carry a note its line is the note's field.
        let list = box_choices(card);
        let wraps = self.wraps_choices(card, width);
        for (at, index) in list.iter().enumerate() {
            let choice = &card.choices[*index];
            let lit = at == self.selected;
            let ink = if lit { theme.bright() } else { theme.text() };
            let mut row = Line::default();
            push(
                &mut row,
                if lit { "› " } else { "  " },
                theme.attention(),
                width,
            );
            push(&mut row, format!("{}. ", at + 1), ink, width);
            let deny = refuses(&choice.outcome);
            let said = box_label(choice, card.kind);
            let typed = self.note.text();
            if deny && choice.takes_note && (self.noting || !typed.is_empty()) {
                // "No: <note>", the note as typed.
                push(&mut row, format!("{said}: "), ink, width);
                let cursor = lit && self.noting;
                out.push_field(
                    row,
                    width,
                    Typed {
                        typed,
                        ink,
                        spot: Some(BoxSpot::Choice(at)),
                        cursor,
                        after: None,
                    },
                );
                continue;
            } else if deny {
                push(&mut row, said, ink, width);
                push(&mut row, "  esc", theme.faint(), width);
                if choice.takes_note {
                    push(&mut row, " · tab to add a note", theme.faint(), width);
                }
            } else if !wraps {
                push(&mut row, said, ink, width);
            } else {
                // A long label (a Codex rule naming a whole command) wraps
                // under its own start rather than losing its end, which is
                // what says what the rule covers.
                let lead = text::line_width(&row);
                let mut parts = label_lines(&said, lead, width).into_iter();
                push(&mut row, parts.next().unwrap_or_default(), ink, width);
                for part in parts {
                    out.spots
                        .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
                    out.lines.push(row);
                    row = Line::default();
                    push(&mut row, " ".repeat(lead), ink, width);
                    push(&mut row, part, ink, width);
                }
            }
            out.spots
                .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
            out.lines.push(row);
        }
        out
    }
}

/// A choice's label in lines of `width` columns, each after `lead` columns
/// (its marker and number on the first, as much blank on the rest).
fn label_lines(label: &str, lead: usize, width: usize) -> Vec<String> {
    let room = width.saturating_sub(lead).max(1);
    let parts = text::wrap(label, room);
    if parts.is_empty() {
        vec![String::new()]
    } else {
        parts
    }
}

/// How many lines the box's choices take at `width` with their labels
/// wrapped: one each, and more for a label that wraps.
fn wrapped_choice_rows(card: &AskCard, width: usize) -> usize {
    box_choices(card)
        .iter()
        .enumerate()
        .map(|(at, index)| {
            let choice = &card.choices[*index];
            if refuses(&choice.outcome) {
                return 1;
            }
            let lead = 2 + format!("{}. ", at + 1).len();
            label_lines(&box_label(choice, card.kind), lead, width).len()
        })
        .sum()
}

/// `source` with its inline code (`` `like this` ``) unmarked, and where
/// that code stands in what is left, by byte range. A backtick with no
/// partner is kept as it is.
fn inline_code(source: &str) -> (String, Vec<std::ops::Range<usize>>) {
    let mut plain = String::with_capacity(source.len());
    let mut code = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find('`') {
        plain.push_str(&rest[..at]);
        let fence_len = rest[at..].len() - rest[at..].trim_start_matches('`').len();
        let fence = &rest[at..at + fence_len];
        let after = &rest[at + fence_len..];
        match after.find(fence).filter(|&end| end > 0) {
            Some(end) => {
                let start = plain.len();
                plain.push_str(&after[..end]);
                code.push(start..plain.len());
                rest = &after[end + fence_len..];
            }
            None => {
                plain.push_str(fence);
                rest = after;
            }
        }
    }
    plain.push_str(rest);
    (plain, code)
}

/// The piece `from..to` of `plain` as runs, the code in `code`'s style and
/// the rest in `ink`.
fn styled_runs(
    plain: &str,
    ranges: &[std::ops::Range<usize>],
    from: usize,
    to: usize,
    ink: Style,
    code: Style,
) -> Vec<(String, Style)> {
    let mut runs: Vec<(String, Style)> = Vec::new();
    let mut at = from;
    while at < to {
        let inside = ranges.iter().find(|range| range.contains(&at));
        let (end, style) = match inside {
            Some(range) => (range.end.min(to), code),
            None => (
                ranges
                    .iter()
                    .map(|range| range.start)
                    .filter(|&start| start > at)
                    .min()
                    .unwrap_or(to)
                    .min(to),
                ink,
            ),
        };
        runs.push((plain[at..end].to_owned(), style));
        at = end;
    }
    runs
}

/// An agent's words on one line, its inline code drawn as code.
fn code_runs(source: &str, ink: Style, code: Style) -> Vec<(String, Style)> {
    let (plain, ranges) = inline_code(source);
    styled_runs(&plain, &ranges, 0, plain.len(), ink, code)
}

/// An agent's words wrapped at `width`, its inline code drawn as code
/// without the backticks, as its prose is.
fn code_wrap(source: &str, width: usize, ink: Style, code: Style) -> Vec<Vec<(String, Style)>> {
    let (plain, ranges) = inline_code(source);
    let mut cursor = 0;
    text::wrap(&plain, width.max(1))
        .into_iter()
        .map(|part| match plain[cursor..].find(part.as_str()) {
            Some(found) => {
                let start = cursor + found;
                cursor = start + part.len();
                styled_runs(&plain, &ranges, start, cursor, ink, code)
            }
            None => vec![(part, ink)],
        })
        .collect()
}

/// Pushes runs onto `line`, clipped at `width`.
fn push_runs(line: &mut Line<'static>, runs: Vec<(String, Style)>, width: usize) {
    for (words, style) in runs {
        push(line, words, style, width);
    }
}

/// The end of `words` in at most `max` columns, "…" where it is cut.
fn text_tail(words: &str, max: usize) -> String {
    if text::str_width(words) <= max {
        return words.to_owned();
    }
    let keep = max.saturating_sub(1);
    let chars: Vec<char> = words.chars().collect();
    let mut out: Vec<char> = Vec::new();
    let mut used = 0;
    for c in chars.iter().rev() {
        let w = text::str_width(c.encode_utf8(&mut [0; 4]));
        if used + w > keep {
            break;
        }
        used += w;
        out.push(*c);
    }
    out.reverse();
    format!("…{}", out.into_iter().collect::<String>())
}

/// A question's rows in the box: its options, "Something else" when it
/// takes one, then Skip and the way out when the box has them.
struct QuestionRows {
    options: usize,
    other: Option<usize>,
    skip: Option<usize>,
    out: Option<usize>,
}

impl QuestionRows {
    fn of(question: &QuestionView, card: &AskCard) -> QuestionRows {
        let options = question.options.len();
        let other = question.allow_other.then_some(options);
        let numbered = options + usize::from(question.allow_other);
        let skip = skips(card).then_some(numbered);
        let out = way_out(card).then_some(numbered + usize::from(skip.is_some()));
        QuestionRows {
            options,
            other,
            skip,
            out,
        }
    }

    /// The rows digits reach: the options and "Something else".
    fn numbered(&self) -> usize {
        self.options + usize::from(self.other.is_some())
    }

    /// The last row the highlight can reach.
    fn last(&self) -> usize {
        self.out
            .or(self.skip)
            .unwrap_or(self.numbered().saturating_sub(1))
    }
}

/// Whether a boxed question or form offers a way out besides answering: a
/// tool server's form can be declined; an agent's questions replied to in
/// the person's own words.
fn way_out(card: &AskCard) -> bool {
    match card.body {
        AskBody::Form { .. } => true,
        AskBody::Question(_) => card.question_reply,
        _ => false,
    }
}

/// Whether a boxed question can be left unanswered.
fn skips(card: &AskCard) -> bool {
    matches!(card.body, AskBody::Question(_)) && card.question_skip
}

/// Whether a boxed question takes a note on each question.
fn takes_notes(card: &AskCard) -> bool {
    matches!(card.body, AskBody::Question(_)) && card.question_note
}

/// "The shell", "The fleet, The chat", "The fleet, tmux style": an answer
/// in words, the options picked then the words typed, empty when the
/// question was skipped.
fn pick_words(question: &QuestionView, pick: &QuestionPick) -> String {
    if !pick.answered() {
        return String::new();
    }
    if question.secret && pick.answered() {
        return "answered (hidden)".to_owned();
    }
    let other = pick.other.as_ref().filter(|other| !other.is_empty());
    if pick.selected.is_empty() && other.is_none() {
        return "nothing picked".to_owned();
    }
    pick.selected
        .iter()
        .filter_map(|i| question.options.get(*i as usize))
        .map(|option| option.label.clone())
        .chain(other.cloned())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A question's name on its tab and in the review: its header, else its
/// place.
fn question_name(question: &QuestionView, at: usize) -> String {
    if question.header.is_empty() {
        format!("Question {}", at + 1)
    } else {
        question.header.clone()
    }
}

impl AskUi {
    fn in_review(&self, questions: &[QuestionView]) -> bool {
        questions.len() > 1 && self.step >= questions.len()
    }

    /// The highlight is on the current question's "Something else".
    fn on_other_row(&self, questions: &[QuestionView]) -> bool {
        !self.in_review(questions)
            && questions.get(self.step).is_some_and(|question| {
                question.allow_other && question.options.len() == self.selected
            })
    }

    /// "Something else" is open as a field and has the keys: opened by Tab,
    /// or by Enter while empty, and closed by Esc, as a permission's note.
    fn on_something_else(&self, questions: &[QuestionView]) -> bool {
        self.noting && !self.annotating && !self.replying && self.on_other_row(questions)
    }

    /// Each question's response, a skip where it was not answered, with
    /// the note being typed on the current one.
    fn responses(&self) -> Vec<QuestionResponse> {
        self.picks
            .iter()
            .enumerate()
            .map(|(at, pick)| {
                let mut response = if pick.answered() {
                    pick.response()
                } else {
                    QuestionResponse {
                        note: pick.note.trim().to_owned(),
                        ..QuestionResponse::skip()
                    }
                };
                if at == self.step && !self.in_review_count(self.picks.len()) {
                    response.note = self.note.text().trim().to_owned();
                }
                response
            })
            .collect()
    }

    fn in_review_count(&self, count: usize) -> bool {
        count > 1 && self.step >= count
    }

    /// Keeps the current question's note with it, before moving off it.
    fn stash_note(&mut self) {
        if let Some(pick) = self.picks.get_mut(self.step) {
            pick.note = self.note.text().to_owned();
        }
    }

    /// To question `step`, or to the review past the last. The highlight
    /// starts on what was picked, else the first option; in the review, on
    /// "Send answers".
    fn question_goto(&mut self, questions: &[QuestionView], step: usize) {
        let count = questions.len();
        // What was typed stays with its question, unanswered, as a draft.
        self.drafts.resize(count, String::new());
        if self.step < count {
            self.drafts[self.step] = self.other.text().to_owned();
            self.stash_note();
        }
        self.annotating = false;
        self.replying = false;
        self.note = Editor::default();
        if step >= count && count > 1 {
            self.step = count;
            self.selected = count;
            return;
        }
        self.step = step.min(count.saturating_sub(1));
        let pick = &self.picks[self.step];
        let other_row = questions[self.step].options.len();
        let draft = self.drafts[self.step].clone();
        self.note.insert_str(&pick.note);
        self.selected = match (&pick.other, pick.selected.first()) {
            (Some(other), _) if !other.is_empty() => other_row,
            (_, Some(first)) => *first as usize,
            _ if !draft.is_empty() => other_row,
            _ => 0,
        };
        // A tab that is only a text field has it live from the start.
        let question = &questions[self.step];
        self.noting = question.options.is_empty() && question.allow_other;
        self.invalid = None;
        self.other = Editor::default();
        self.other.secret = questions[self.step].secret;
        match &pick.other {
            Some(other) if !other.is_empty() => self.other.insert_str(other),
            _ => self.other.insert_str(&draft),
        }
    }

    /// After a question is answered: a lone question sends; otherwise on to
    /// the next one not yet answered, or the review when none is left.
    fn question_answered(&mut self, card: &AskCard, questions: &[QuestionView]) -> AskAction {
        let count = questions.len();
        if count == 1 {
            return self.send_boxed(card, questions);
        }
        let next = (1..count)
            .map(|ahead| (self.step + ahead) % count)
            .find(|at| !self.picks[*at].done());
        self.question_goto(questions, next.unwrap_or(count));
        AskAction::None
    }

    /// Sends the answers, or submits the form; a question that must be
    /// answered and is not (a form's required field, or any question where
    /// the agent takes none unanswered) goes there instead.
    fn send_boxed(&mut self, card: &AskCard, questions: &[QuestionView]) -> AskAction {
        if let Some(missing) = self.missing(card) {
            self.question_goto(questions, missing);
            self.invalid = self.field_problem(missing);
            return AskAction::None;
        }
        if matches!(card.body, AskBody::Form { .. }) {
            return self.submit_form(card);
        }
        self.send_boxed_questions(card)
    }

    /// The first question that must be answered before sending and is
    /// not: a form's field with a problem, or with an agent that takes no
    /// question unanswered, any.
    fn missing(&self, card: &AskCard) -> Option<usize> {
        match card.body {
            AskBody::Form { .. } => self.form_problems().first().map(|p| p.field as usize),
            AskBody::Question(_) if !card.question_skip => {
                self.picks.iter().position(|pick| !pick.answered())
            }
            _ => None,
        }
    }

    /// Sends the answers, a skipped question with none.
    fn send_boxed_questions(&mut self, card: &AskCard) -> AskAction {
        let answer = question_answer(card, &self.responses());
        match answer_input(card, &answer, "") {
            Some(input) => AskAction::Answer(Box::new(input)),
            None => AskAction::None,
        }
    }

    /// Sends the person's own words instead, with what was answered so far.
    fn send_reply(&mut self, card: &AskCard) -> AskAction {
        let words = self.reply.text().trim().to_owned();
        if words.is_empty() {
            return AskAction::None;
        }
        let answer = reply_answer(card, &words, &self.responses());
        match answer_input(card, &answer, "") {
            Some(input) => AskAction::Answer(Box::new(input)),
            None => AskAction::None,
        }
    }

    /// Acts on row `at` of the current question: picks an option (toggles
    /// one of several), opens "Something else", or takes the way out.
    /// `confirm` is Enter, which also finishes a question of several picks.
    fn question_row(
        &mut self,
        card: &AskCard,
        questions: &[QuestionView],
        at: usize,
        confirm: bool,
    ) -> AskAction {
        let question = &questions[self.step];
        let rows = QuestionRows::of(question, card);
        self.selected = at;
        if at < rows.options {
            let option = at as u32;
            let pick = &mut self.picks[self.step];
            pick.skipped = false;
            if !question.multi_select {
                pick.selected = vec![option];
                pick.other = None;
                return self.question_answered(card, questions);
            }
            if confirm {
                pick.none = !pick.answered();
                return self.question_answered(card, questions);
            }
            // Ticking keeps any words typed as "Something else": a
            // multi-select sends both.
            pick.none = false;
            match pick.selected.iter().position(|s| *s == option) {
                Some(found) => {
                    pick.selected.remove(found);
                }
                None => {
                    pick.selected.push(option);
                    pick.selected.sort_unstable();
                }
            }
            return AskAction::None;
        }
        if rows.other == Some(at) {
            let typed = self.other.text().trim().to_owned();
            if confirm && typed.is_empty() || !confirm {
                // Enter on an empty field, or its digit, opens it to type.
                self.noting = true;
                return AskAction::None;
            }
            if let Some(problem) = self.typed_problem(&typed) {
                self.invalid = Some(problem);
                return AskAction::None;
            }
            if confirm && !typed.is_empty() {
                let pick = &mut self.picks[self.step];
                // A multi-select keeps its ticks beside the typed words;
                // a single choice is one or the other.
                if !question.multi_select {
                    pick.selected.clear();
                }
                pick.none = false;
                pick.other = Some(typed);
                pick.skipped = false;
                return self.question_answered(card, questions);
            }
            return AskAction::None;
        }
        if rows.skip == Some(at) {
            let pick = &mut self.picks[self.step];
            pick.selected.clear();
            pick.other = None;
            pick.skipped = true;
            pick.none = false;
            self.other = Editor::default();
            if let Some(draft) = self.drafts.get_mut(self.step) {
                draft.clear();
            }
            return self.question_answered(card, questions);
        }
        if rows.out == Some(at) {
            if matches!(card.body, AskBody::Form { .. }) {
                return Self::choose(card, &ChoiceOutcome::Decline);
            }
            // "Reply instead" opens to type the words; Enter there sends.
            self.replying = true;
            self.annotating = false;
        }
        AskAction::None
    }

    /// One key on a boxed question ask. ←/→ move between questions and the
    /// review, ↑/↓ (or j/k) between rows, 1–9 act on a row at once, Space
    /// toggles one of several picks, Enter acts on the highlighted row. On
    /// "Something else" the line is a field: typing goes there, Enter
    /// answers with it, Esc clears it.
    fn question_key_boxed(
        &mut self,
        card: &AskCard,
        questions: &[QuestionView],
        key: KeyEvent,
    ) -> AskAction {
        let count = questions.len();
        let several = count > 1;
        let enter = key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::SHIFT);
        let notes = takes_notes(card) && !self.in_review(questions);
        if self.replying {
            match key.code {
                _ if enter => return self.send_reply(card),
                KeyCode::Esc => {
                    self.reply = Editor::default();
                    self.replying = false;
                }
                _ => {
                    self.reply.key(key);
                }
            }
            return AskAction::None;
        }
        if self.annotating {
            // The note is the question's: Enter answers with the highlighted
            // row, Tab keeps it and goes back to the rows, Esc clears it.
            match key.code {
                _ if enter => {
                    self.annotating = false;
                    self.stash_note();
                    return self.question_row(card, questions, self.selected, true);
                }
                KeyCode::Tab => self.annotating = false,
                KeyCode::Esc => {
                    self.note = Editor::default();
                    self.annotating = false;
                    self.stash_note();
                }
                _ => {
                    self.note.key(key);
                }
            }
            return AskAction::None;
        }
        // Tab opens the question's note where the agent takes one, from
        // anywhere, a field's text included; otherwise Tab and Shift+Tab
        // move between tabs.
        match key.code {
            KeyCode::Tab if notes => {
                self.annotating = true;
                return AskAction::None;
            }
            KeyCode::Tab if several => {
                self.question_goto(questions, (self.step + 1).min(count));
                return AskAction::None;
            }
            KeyCode::BackTab if several => {
                self.question_goto(questions, self.step.saturating_sub(1));
                return AskAction::None;
            }
            _ => {}
        }
        if self.in_review(questions) {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    self.selected = (self.selected + 1).min(count)
                }
                KeyCode::Left => self.question_goto(questions, count - 1),
                _ if enter && self.selected < count => self.question_goto(questions, self.selected),
                _ if enter => return self.send_boxed(card, questions),
                _ => {}
            }
            return AskAction::None;
        }
        let question = &questions[self.step];
        let rows = QuestionRows::of(question, card);
        let last = rows.last();
        // A tab that is only a text field has it live; ↓ steps out to the
        // way out and ↑ back in.
        let text_only = question.options.is_empty() && rows.other.is_some();
        if self.on_something_else(questions) {
            match key.code {
                KeyCode::Esc => {
                    let empty = self.other.text().is_empty();
                    self.other = Editor::default();
                    self.picks[self.step].other = None;
                    if let Some(draft) = self.drafts.get_mut(self.step) {
                        draft.clear();
                    }
                    self.invalid = None;
                    if !text_only {
                        self.noting = false;
                    } else if empty && let Some(out) = rows.out.or(rows.skip) {
                        // An empty live field: Esc points at the way out.
                        self.noting = false;
                        self.selected = out;
                    }
                }
                KeyCode::Down if text_only && (rows.out.or(rows.skip)).is_some() => {
                    self.noting = false;
                    self.selected = last;
                }
                _ if enter => {
                    if self.other.text().trim().is_empty() {
                        return AskAction::None;
                    }
                    return self.question_row(card, questions, self.selected, true);
                }
                _ => {
                    self.invalid = None;
                    self.other.key(key);
                }
            }
            return AskAction::None;
        }
        match key.code {
            KeyCode::Left if several => self.question_goto(questions, self.step.saturating_sub(1)),
            KeyCode::Right if several => self.question_goto(questions, self.step + 1),
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                if text_only {
                    self.noting = true;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
            // The way out is not numbered: digits reach the options and
            // "Something else" only.
            KeyCode::Char(c @ '1'..='9') => {
                let at = c as usize - '1' as usize;
                if at < rows.numbered() {
                    return self.question_row(card, questions, at, false);
                }
            }
            // Esc only points at the way out; Enter takes it.
            KeyCode::Esc => {
                if let Some(out) = rows.out.or(rows.skip) {
                    self.selected = out;
                }
            }
            // F shows the previews, or a question cut on a short screen,
            // whole, and cuts them again.
            KeyCode::Char('f') => self.show_all = !self.show_all,
            KeyCode::Char(' ') if self.selected < rows.options => {
                return self.question_row(card, questions, self.selected, false);
            }
            _ if enter => return self.question_row(card, questions, self.selected, true),
            _ => {}
        }
        AskAction::None
    }

    /// A click on a boxed question: a tab moves there, a row acts as its
    /// digit would.
    fn question_click(
        &mut self,
        card: &AskCard,
        questions: &[QuestionView],
        spot: BoxSpot,
    ) -> AskAction {
        match spot {
            BoxSpot::Tab(at) => {
                self.question_goto(questions, at);
                AskAction::None
            }
            BoxSpot::Choice(at) if self.in_review(questions) => {
                if at < questions.len() {
                    self.question_goto(questions, at);
                    AskAction::None
                } else {
                    self.send_boxed(card, questions)
                }
            }
            BoxSpot::Choice(at) => self.question_row(card, questions, at, false),
            BoxSpot::ShowAll => {
                self.show_all = !self.show_all;
                AskAction::None
            }
            _ => AskAction::None,
        }
    }

    /// The hint line's words while a boxed question has the keys.
    pub fn question_hint(&self, card: &AskCard, leader: char) -> Option<String> {
        let form = form_questions(&self.fields);
        let questions: &[QuestionView] = match &card.body {
            AskBody::Question(questions) => questions,
            AskBody::Form { .. } => &form,
            AskBody::Link { .. }
                if matches!(card.state, CardState::Open | CardState::Rejected(_)) =>
            {
                let enter = if self.selected == 2 {
                    "enter decline"
                } else {
                    "enter choose"
                };
                return Some(format!("{enter} · ctrl+x interrupt · ctrl+{leader} more"));
            }
            AskBody::Unanswerable { .. } => {
                return Some(if self.attach {
                    "enter open the terminal · ctrl+x interrupt".to_owned()
                } else {
                    "ctrl+x interrupt".to_owned()
                });
            }
            _ => return None,
        };
        if !matches!(card.state, CardState::Open | CardState::Rejected(_)) {
            return None;
        }
        let several = questions.len() > 1;
        let tabs = if several { " · ←/→ questions" } else { "" };
        if self.replying {
            return Some("enter send · esc clear · ctrl+x interrupt".to_owned());
        }
        if self.annotating {
            return Some("enter choose · tab done · esc clear · ctrl+x interrupt".to_owned());
        }
        if self.in_review(questions) {
            if let Some(at) = self.missing(card) {
                let name = question_name(&questions[at], at);
                return Some(format!("enter go to {name}{tabs} · ctrl+x interrupt"));
            }
            return Some(format!(
                "enter send{tabs} · ctrl+x interrupt · ctrl+{leader} more"
            ));
        }
        let form = matches!(card.body, AskBody::Form { .. });
        if self.on_something_else(questions) {
            // Typing: ←/→ are the text's, Tab still changes tabs.
            let enter = match (several, form) {
                (true, _) => "enter answer",
                (false, true) => "enter submit",
                (false, false) => "enter send",
            };
            let tab = if takes_notes(card) {
                " · tab note"
            } else if several {
                " · tab next"
            } else {
                ""
            };
            return Some(format!("{enter}{tab} · esc clear · ctrl+x interrupt"));
        }
        if self.on_other_row(questions) {
            // A draft kept from earlier answers on Enter.
            let enter = if self.other.text().trim().is_empty() {
                "enter to type"
            } else {
                "enter answer"
            };
            return Some(format!(
                "{enter}{tabs} · ctrl+x interrupt · ctrl+{leader} more"
            ));
        }
        let rows = QuestionRows::of(&questions[self.step], card);
        if rows.skip == Some(self.selected) {
            return Some(format!(
                "enter skip{tabs} · ctrl+x interrupt · ctrl+{leader} more"
            ));
        }
        if rows.out == Some(self.selected) {
            let enter = if form {
                "enter decline"
            } else {
                "enter to type"
            };
            return Some(format!(
                "{enter}{tabs} · ctrl+x interrupt · ctrl+{leader} more"
            ));
        }
        let note = if takes_notes(card) {
            " · tab note"
        } else {
            ""
        };
        // What Enter does next: send a lone question, or move on to the next
        // unanswered one, or to the review when none is left.
        let others_open =
            (0..questions.len()).any(|at| at != self.step && !self.picks[at].answered());
        let enter = if !several {
            "enter send"
        } else if others_open {
            "enter next"
        } else {
            "enter review"
        };
        if questions[self.step].multi_select {
            // Four at most: the stop stays, the leader goes.
            Some(format!(
                "space select · {enter}{tabs}{note} · ctrl+x interrupt"
            ))
        } else if several && !note.is_empty() {
            Some(format!("enter choose{tabs}{note} · ctrl+x interrupt"))
        } else {
            Some(format!(
                "enter choose{tabs}{note} · ctrl+x interrupt · ctrl+{leader} more"
            ))
        }
    }

    /// The boxed question ask's lines: the tabs (or the lone question's
    /// name), the question, its rows; or the review.
    fn question_lines(
        &self,
        card: &AskCard,
        questions: &[QuestionView],
        width: usize,
        theme: Theme,
    ) -> BoxLines {
        let mut out = BoxLines {
            lines: Vec::new(),
            cursor: None,
            spots: Vec::new(),
        };
        let count = questions.len();
        let review = self.in_review(questions);
        let form = match &card.body {
            AskBody::Form {
                server, message, ..
            } => Some((server.as_str(), message.as_str())),
            _ => None,
        };

        // A form says who asks and why first; its fields are the tabs.
        if let Some((server, message)) = form {
            let mut top = Line::from(Span::styled("● ", theme.attention()));
            push(
                &mut top,
                format!("{server} needs details"),
                theme.text(),
                width,
            );
            out.lines.push(top);
            for part in text::wrap(message, width.max(1)) {
                let mut line = Line::default();
                push(&mut line, part, theme.text(), width);
                out.lines.push(line);
            }
            out.lines.push(Line::default());
        }

        // The head: a tab per question, then Review; or one question's name.
        let head_row = out.lines.len();
        let mut head = Line::from(Span::styled(
            if form.is_some() { "" } else { "● " },
            theme.attention(),
        ));
        if count == 1 && form.is_some() {
            // One field: no tabs, its label says it all.
        } else if count == 1 {
            push(
                &mut head,
                question_name(&questions[0], 0),
                theme.bright(),
                width,
            );
        } else {
            let names: Vec<String> = questions
                .iter()
                .enumerate()
                .map(|(at, question)| question_name(question, at))
                .chain(std::iter::once("Review".to_owned()))
                .collect();
            let current = self.step.min(count);
            // Each tab is its name with a space either side, so the current
            // one's chip fits around it; ‹ and › at the ends say ←/→ move.
            // Always drawn, so the row never shifts as the current tab does.
            let marks: usize = self.picks.iter().filter(|pick| pick.answered()).count() * 2;
            let room = width.saturating_sub(2 + 4 + marks + 3 * names.len());
            let natural: usize = names.iter().map(|name| text::str_width(name)).sum();
            let cap = if natural <= room {
                usize::MAX
            } else {
                (room / names.len()).max(4)
            };
            let chip = theme
                .row_surface()
                .unwrap_or_else(|| theme.user_surface())
                .patch(theme.bright());
            push(&mut head, "‹ ", theme.faint(), width);
            for (at, name) in names.iter().enumerate() {
                if at > 0 {
                    push(&mut head, " ", theme.faint(), width);
                }
                let answered = self.picks.get(at).is_some_and(QuestionPick::answered);
                let style = if at == current {
                    chip
                } else if answered {
                    theme.faint()
                } else {
                    theme.text()
                };
                let from = text::line_width(&head);
                let mark = if answered { "✓ " } else { "" };
                push(
                    &mut head,
                    format!(" {mark}{} ", text::ellipsize(name, cap)),
                    style,
                    width,
                );
                out.spots
                    .push((head_row, (from, text::line_width(&head)), BoxSpot::Tab(at)));
            }
            push(&mut head, " ›", theme.faint(), width);
        }
        if !(count == 1 && form.is_some()) {
            out.lines.push(head);
            out.lines.push(Line::default());
        }

        if review {
            // Each question as asked, faint, then "→ answer" under it; the
            // highlighted one's lines brighten.
            for (at, question) in questions.iter().enumerate() {
                let lit = self.selected == at;
                for (i, part) in code_wrap(
                    &question.question,
                    width.saturating_sub(2),
                    if lit { theme.text() } else { theme.faint() },
                    theme.code(),
                )
                .into_iter()
                .enumerate()
                {
                    let mut row = Line::default();
                    push(
                        &mut row,
                        if lit && i == 0 { "› " } else { "  " },
                        theme.attention(),
                        width,
                    );
                    push_runs(&mut row, part, width);
                    out.spots
                        .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
                    out.lines.push(row);
                }
                let words = pick_words(question, &self.picks[at]);
                let (words, ink) = if words.is_empty() && card.question_skip {
                    ("skipped".to_owned(), theme.faint())
                } else if words.is_empty() {
                    ("not answered".to_owned(), theme.faint())
                } else if lit {
                    (words, theme.bright())
                } else {
                    (words, theme.text())
                };
                for (i, part) in text::wrap(&words, width.saturating_sub(6).max(1))
                    .into_iter()
                    .enumerate()
                {
                    let mut row = Line::default();
                    push(
                        &mut row,
                        if i == 0 { "    → " } else { "      " },
                        theme.faint(),
                        width,
                    );
                    push(&mut row, part, ink, width);
                    out.spots
                        .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
                    out.lines.push(row);
                }
                let note = self.picks[at].note.trim();
                if !note.is_empty() {
                    let mut row = Line::default();
                    push(&mut row, "      Note: ", theme.faint(), width);
                    let shown = text::ellipsize(note, width.saturating_sub(12));
                    push(&mut row, shown, theme.muted(), width);
                    out.lines.push(row);
                }
            }
            out.lines.push(Line::default());
            let lit = self.selected >= count;
            let mut send = Line::default();
            push(
                &mut send,
                if lit { "› " } else { "  " },
                theme.attention(),
                width,
            );
            // Nothing goes with a required field empty, nor, where the
            // agent takes none unanswered, a question: the line says which,
            // and Enter goes there.
            let missing = self.missing(card);
            push(
                &mut send,
                if form.is_some() {
                    "Submit"
                } else {
                    "Send answers"
                },
                if missing.is_some() {
                    theme.faint()
                } else if lit {
                    theme.bright()
                } else {
                    theme.text()
                },
                width,
            );
            if let Some(at) = missing {
                let words = match self.field_problem(at) {
                    Some(problem) => problem_words(&problem),
                    None => "is not answered".to_owned(),
                };
                push(
                    &mut send,
                    format!(" · {} {words}", question_name(&questions[at], at)),
                    theme.faint(),
                    width,
                );
            }
            out.spots
                .push((out.lines.len(), (0, width), BoxSpot::Choice(count)));
            out.lines.push(send);
            return out;
        }

        let question = &questions[self.step];
        let rows = QuestionRows::of(question, card);
        let asked_rows = code_wrap(&question.question, width, theme.text(), theme.code());
        let asked = out.lines.len()..out.lines.len() + asked_rows.len();
        for part in asked_rows {
            let mut line = Line::default();
            push_runs(&mut line, part, width);
            out.lines.push(line);
        }
        // A form's field: required, and what it is for.
        if let Some(field) = form.and(self.fields.get(self.step)) {
            if let Some(last) = out.lines.last_mut() {
                if field.required {
                    push(last, " · required", theme.faint(), width);
                }
                // A typed field says what is wrong beside the text; one of
                // options says it here.
                if let Some(problem) = &self.invalid
                    && !question.options.is_empty()
                    && *problem != FieldProblem::Required
                {
                    push(
                        last,
                        format!(" · {}", problem_words(problem)),
                        theme.attention(),
                        width,
                    );
                }
            }
            for part in text::wrap(&field.description, width.max(1)) {
                let mut line = Line::default();
                push(&mut line, part, theme.faint(), width);
                out.lines.push(line);
            }
        }
        let select_all = question.multi_select.then_some(out.lines.len());
        if question.multi_select {
            let mut line = Line::default();
            push(&mut line, "Select all that apply", theme.faint(), width);
            out.lines.push(line);
        }
        let gap = out.lines.len();
        out.lines.push(Line::default());

        // With previews, the options and the highlighted one's preview side
        // by side where the box is wide enough, else the preview under them.
        let previews = question.options.iter().any(|o| !o.preview.is_empty());
        let side = previews && width >= SIDE_BY_SIDE;
        let left_width = if side {
            let widest = question
                .options
                .iter()
                .map(|o| text::str_width(&o.label))
                .max()
                .unwrap_or(0);
            (widest + 6).clamp(LEFT_MIN, LEFT_MAX)
        } else {
            width
        };
        let footer = if form.is_some() {
            "Decline"
        } else {
            "Reply instead"
        };
        let mut left = self.question_rows(question, &rows, left_width, !previews, footer, theme);
        // A short screen keeps the way out, the free-text row and the keys,
        // as the boxed ask keeps its choices: the options' descriptions go
        // first, then the question's words down to two lines, then the
        // options show a window around the highlight.
        if !previews && self.room != 0 {
            let budget = self.room.saturating_sub(BOX_CHROME);
            if out.lines.len() + left.lines.len() > budget {
                left = self.question_rows(question, &rows, left_width, false, footer, theme);
            }
            // Then the spacing: the blank under the question, "Select all
            // that apply" (the key line says space selects), and the blank
            // above the footer, so the options keep their rows.
            if out.lines.len() + left.lines.len() > budget {
                drop_line(&mut out, gap);
            }
            if let Some(at) = select_all
                && out.lines.len() + left.lines.len() > budget
            {
                drop_line(&mut out, at);
            }
            if out.lines.len() + left.lines.len() > budget
                && let Some(footer_at) = rows.skip.or(rows.out).and_then(|at| {
                    left.spots
                        .iter()
                        .find(|(_, _, spot)| *spot == BoxSpot::Choice(at))
                        .map(|(row, ..)| *row)
                })
                && footer_at > 0
                && text::line_width(&left.lines[footer_at - 1]) == 0
            {
                drop_line(&mut left, footer_at - 1);
            }
            let over = (out.lines.len() + left.lines.len()).saturating_sub(budget);
            // Cut, the question keeps its opening and its last line, which
            // is where it asks, and says how much is hidden between: `f`
            // or a click shows it whole.
            let cut = if self.show_all {
                0
            } else {
                over.min(asked.len().saturating_sub(QUESTION_LEAST))
            };
            if cut > 0 {
                let hidden = cut + 1;
                let from = asked.end - 1 - hidden;
                let mut more = Line::default();
                push_more(&mut more, hidden, "[Show All]", theme, width);
                let at = text::line_width(&more).saturating_sub(10);
                out.lines.splice(from..asked.end - 1, [more]);
                for spot in &mut out.spots {
                    if spot.0 > from {
                        spot.0 -= cut;
                    }
                }
                out.spots.push((from, (at, at + 10), BoxSpot::ShowAll));
            }
            let over = (out.lines.len() + left.lines.len()).saturating_sub(budget);
            if over > 0 {
                window_options(&mut left, rows.options, self.selected, over, theme);
            }
        }
        let preview = question
            .options
            .get(self.selected)
            .map(|option| option.preview.as_str())
            .filter(|preview| !preview.is_empty());
        // Tall previews are cut so the box leaves the feed room; shown whole,
        // one still stops where the box would leave the screen.
        let room = if self.room == 0 { 36 } else { self.room };
        // The rows the rest of the screen needs: the chat's header, the
        // box's edges, the hints, a little feed, and (under the options)
        // the box's own lines above the preview.
        let above = out.lines.len() + if side { 0 } else { left.lines.len() + 1 };
        let fits = room.saturating_sub(10 + above).max(3);
        let cap = if self.show_all {
            fits
        } else {
            (room / 2).saturating_sub(6).clamp(3, fits.max(3))
        };
        let control = if self.show_all {
            "[Show Less]"
        } else {
            "[Show All]"
        };
        // The preview's room is the tallest of this question's previews, so
        // moving between options never changes the box's height.
        let tallest = |width: usize| {
            question
                .options
                .iter()
                .filter(|option| !option.preview.is_empty())
                .map(|option| {
                    let (lines, more) = preview_lines(&option.preview, width, cap, theme);
                    lines.len() + usize::from(more > 0)
                })
                .max()
                .unwrap_or(0)
        };
        let base = out.lines.len();
        if side {
            let right_width = width.saturating_sub(left_width + 3).max(1);
            let (right, more) = match preview {
                Some(preview) => preview_lines(preview, right_width, cap, theme),
                None => (Vec::new(), 0),
            };
            let height = left.lines.len().max(tallest(right_width));
            for i in 0..height {
                let mut line = left.lines.get(i).cloned().unwrap_or_default();
                text::pad_to(&mut line, left_width);
                push(&mut line, " │ ", theme.hairline(), width);
                if let Some(part) = right.get(i) {
                    line.spans.extend(part.spans.iter().cloned());
                } else if more > 0 && i == right.len() {
                    push_more(&mut line, more, control, theme, width);
                    let end = text::line_width(&line);
                    out.spots
                        .push((base + i, (end - control.len(), end), BoxSpot::ShowAll));
                }
                out.lines.push(line);
            }
        } else {
            out.lines.extend(left.lines.iter().cloned());
            let preview_top = out.lines.len();
            if let Some(preview) = preview {
                out.lines.push(Line::default());
                let (lines, more) = preview_lines(preview, width, cap, theme);
                out.lines.extend(lines);
                if more > 0 {
                    let mut line = Line::default();
                    push_more(&mut line, more, control, theme, width);
                    let end = text::line_width(&line);
                    out.spots.push((
                        out.lines.len(),
                        (end - control.len(), end),
                        BoxSpot::ShowAll,
                    ));
                    out.lines.push(line);
                }
            }
            if previews {
                let height = preview_top + 1 + tallest(width);
                while out.lines.len() < height {
                    out.lines.push(Line::default());
                }
            }
        }
        for (row, cols, spot) in left.spots {
            out.spots.push((base + row, cols, spot));
        }
        if let Some((row, col)) = left.cursor {
            out.cursor = Some((base + row, col));
        }
        out
    }
}

/// A question cut for a short screen keeps this many lines of its words.
const QUESTION_LEAST: usize = 2;

/// Drops `over` of a question's option lines (the first `options` choices),
/// keeping the highlight in view: what is hidden above or below is marked
/// with an arrow in place of the line nearest it. The rows after the
/// options, the free-text row and the way out, always stay.
fn window_options(left: &mut BoxLines, options: usize, selected: usize, over: usize, theme: Theme) {
    let option_rows = left
        .spots
        .iter()
        .filter_map(|(row, _, spot)| match spot {
            BoxSpot::Choice(at) if *at < options => Some(*row + 1),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    // However short the screen, the window holds the highlighted option
    // with a marker either side: one row was all "↓ more", which hid every
    // option and the highlight with them.
    let show = option_rows.saturating_sub(over).max(3);
    if show >= option_rows {
        return;
    }
    let lit = left
        .spots
        .iter()
        .find(|(_, _, spot)| *spot == BoxSpot::Choice(selected))
        .map_or(option_rows, |(row, ..)| *row);
    // The highlight a little above the window's foot, or the window at the
    // options' end when the highlight is below them.
    let from = lit
        .saturating_sub(show.saturating_sub(2))
        .min(option_rows - show);
    let to = from + show;
    let mut lines: Vec<Line<'static>> = left.lines.drain(..option_rows).collect();
    let mut kept: Vec<Line<'static>> = lines.drain(from..to).collect();
    let marker = |words: &str| {
        let mut line = Line::default();
        push(
            &mut line,
            format!("  {words}"),
            theme.faint(),
            usize::MAX / 4,
        );
        line
    };
    if from > 0
        && let Some(first) = kept.first_mut()
    {
        *first = marker("↑ more");
    }
    if to < option_rows
        && let Some(last) = kept.last_mut()
    {
        *last = marker("↓ more");
    }
    let shown = kept.len();
    kept.append(&mut left.lines);
    left.lines = kept;
    // A marker answers no click.
    let marked = |row: usize| (from > 0 && row == from) || (to < option_rows && row + 1 == to);
    left.spots.retain_mut(|(row, _, _)| {
        if *row < option_rows {
            let keep = (from..to).contains(row) && !marked(*row);
            *row = row.saturating_sub(from);
            keep
        } else {
            *row = *row - option_rows + shown;
            true
        }
    });
    if let Some((row, _)) = &mut left.cursor {
        if *row >= option_rows {
            *row = *row - option_rows + shown;
        } else {
            *row = row.saturating_sub(from);
        }
    }
}

/// Removes line `at`, moving the click spots and the cursor below it up.
fn drop_line(lines: &mut BoxLines, at: usize) {
    if at >= lines.lines.len() {
        return;
    }
    lines.lines.remove(at);
    lines.spots.retain(|(row, ..)| *row != at);
    for (row, ..) in &mut lines.spots {
        if *row > at {
            *row -= 1;
        }
    }
    if let Some((row, _)) = &mut lines.cursor
        && *row > at
    {
        *row -= 1;
    }
}

/// Boxes this wide put a question's options and a preview side by side.
const SIDE_BY_SIDE: usize = 82;
/// The options' column beside a preview.
const LEFT_MIN: usize = 22;
const LEFT_MAX: usize = 34;

/// "… 12 more lines [Show All]".
fn push_more(line: &mut Line<'static>, hidden: usize, control: &str, theme: Theme, width: usize) {
    let s = if hidden == 1 { "" } else { "s" };
    push(
        line,
        format!("… {hidden} more line{s} "),
        theme.faint(),
        width,
    );
    push(line, control, theme.muted(), width);
}

/// Whether a preview is drawn rather than written: a line with box-drawing
/// characters, two spaces between words, or a leading indent. Such a
/// preview keeps its spacing, line for line, never reflowed.
fn preformatted(preview: &str) -> bool {
    !preview.contains("```")
        && preview.lines().any(|line| {
            let body = line.trim_end();
            body.chars().any(|c| ('\u{2500}'..='\u{257f}').contains(&c))
                || body.trim_start().starts_with(['+', '|'])
                || body.starts_with("  ")
                || body.trim().contains("  ")
        })
}

/// A preview's lines at `width`, at most `cap` of them, and how many more
/// there are. Drawn previews are kept as drawn, cut at the width; anything
/// else is markdown, with code coloured.
fn preview_lines(
    preview: &str,
    width: usize,
    cap: usize,
    theme: Theme,
) -> (Vec<Line<'static>>, usize) {
    let lines: Vec<Line<'static>> = if preformatted(preview) {
        preview
            .trim_end()
            .lines()
            .map(|line| {
                let mut out = Line::default();
                push(&mut out, text::ellipsize(line, width), theme.text(), width);
                out
            })
            .collect()
    } else {
        crate::markdown::markdown_rows(preview.trim_end(), width, theme)
            .into_iter()
            .map(Line::from)
            .collect()
    };
    if lines.len() <= cap {
        return (lines, 0);
    }
    let keep = cap.saturating_sub(1).max(1);
    let hidden = lines.len() - keep;
    (lines.into_iter().take(keep).collect(), hidden)
}

/// The rows around a boxed ask's own lines: the chat's header and the
/// blank under it, the box's two edges, and the blank and key lines below.
const BOX_CHROME: usize = 7;

/// The rows an opened diff leaves to the rest of the chat: the header, a
/// little feed, the box's own lines around the diff, and the hint.
const DIFF_CHROME: usize = 16;

impl AskUi {
    /// Scrolls an opened diff in the box by `lines`; false when none is
    /// open, so the feed scrolls instead.
    pub fn scroll_diff(&mut self, card: &AskCard, lines: isize) -> bool {
        if !self.show_all || !matches!(card.body, AskBody::Edit { .. }) {
            return false;
        }
        self.diff_scroll = self.diff_scroll.saturating_add_signed(lines);
        true
    }

    /// How many rows the terminal has, so previews leave the feed room.
    pub fn set_room(&mut self, rows: usize, attach: bool) {
        self.room = rows;
        self.attach = attach;
    }

    /// A question's rows at `width`: its options (with their descriptions
    /// when `descriptions`), "Something else", then the way out as a
    /// footer, when there is one.
    fn question_rows(
        &self,
        question: &QuestionView,
        rows: &QuestionRows,
        width: usize,
        descriptions: bool,
        footer: &str,
        theme: Theme,
    ) -> BoxLines {
        let mut out = BoxLines {
            lines: Vec::new(),
            cursor: None,
            spots: Vec::new(),
        };
        let pick = &self.picks[self.step];
        let row_line = |at: usize, lit: bool| {
            let mut row = Line::default();
            push(
                &mut row,
                if lit { "› " } else { "  " },
                theme.attention(),
                width,
            );
            push(
                &mut row,
                format!("{}. ", at + 1),
                if lit { theme.bright() } else { theme.text() },
                width,
            );
            row
        };
        for (at, option) in question.options.iter().enumerate() {
            let lit = self.selected == at;
            let mut row = row_line(at, lit);
            if question.multi_select {
                let on = pick.selected.contains(&(at as u32));
                push(
                    &mut row,
                    if on { "[✓] " } else { "[ ] " },
                    theme.muted(),
                    width,
                );
            }
            let indent = text::line_width(&row);
            push_runs(
                &mut row,
                code_runs(
                    &option.label,
                    if lit { theme.bright() } else { theme.text() },
                    theme.code(),
                ),
                width,
            );
            // "recommended" after the label where it fits, else under it.
            let mut under = None;
            if option.recommended {
                if text::line_width(&row) + 14 <= width {
                    push(&mut row, " · recommended", theme.faint(), width);
                } else {
                    let mut line = Line::from(Span::raw(" ".repeat(indent)));
                    push(&mut line, "recommended", theme.faint(), width);
                    under = Some(line);
                }
            }
            out.spots
                .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
            if let Some(line) = under {
                out.lines.push(row);
                out.spots
                    .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
                row = line;
            }
            // The description after the label when it fits, else under it,
            // hanging at the label.
            let description = option.description.trim();
            if descriptions && !description.is_empty() {
                let used = text::line_width(&row);
                if used + 3 + text::str_width(&inline_code(description).0) <= width {
                    push(&mut row, " · ", theme.faint(), width);
                    push_runs(
                        &mut row,
                        code_runs(description, theme.faint(), theme.code()),
                        width,
                    );
                    out.lines.push(row);
                } else {
                    out.lines.push(row);
                    for part in code_wrap(
                        description,
                        width.saturating_sub(indent),
                        theme.faint(),
                        theme.code(),
                    ) {
                        let mut more = Line::from(Span::raw(" ".repeat(indent)));
                        push_runs(&mut more, part, width);
                        out.spots
                            .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
                        out.lines.push(more);
                    }
                }
            } else {
                out.lines.push(row);
            }
        }
        if let Some(at) = rows.other.filter(|_| question.options.is_empty()) {
            // A tab that is only a text field: the field itself, live.
            let lit = self.selected == at;
            let mut row = Line::default();
            push(
                &mut row,
                if lit { "› " } else { "  " },
                theme.attention(),
                width,
            );
            let typed = if question.secret {
                "•".repeat(self.other.text().chars().count())
            } else {
                self.other.text().to_owned()
            };
            let open = lit && self.noting;
            if typed.is_empty() && !open {
                push(&mut row, "type an answer", theme.faint(), width);
                out.spots
                    .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
                out.lines.push(row);
            } else {
                let problem = self
                    .invalid
                    .as_ref()
                    .filter(|_| open)
                    .map(|problem| (format!(" · {}", problem_words(problem)), theme.faint()));
                out.push_field(
                    row,
                    width,
                    Typed {
                        typed: &typed,
                        ink: theme.bright(),
                        spot: Some(BoxSpot::Choice(at)),
                        cursor: open,
                        after: problem,
                    },
                );
            }
        } else if let Some(at) = rows.other {
            let lit = self.selected == at;
            let mut row = row_line(at, lit);
            let ink = if lit { theme.bright() } else { theme.text() };
            let name = "Something else";
            let typed = if question.secret {
                "•".repeat(self.other.text().chars().count())
            } else {
                self.other.text().to_owned()
            };
            let open = lit && self.noting;
            if open || !typed.is_empty() {
                push(&mut row, format!("{name}: "), ink, width);
                let problem = self
                    .invalid
                    .as_ref()
                    .filter(|_| open)
                    .map(|problem| (format!(" · {}", problem_words(problem)), theme.faint()));
                out.push_field(
                    row,
                    width,
                    Typed {
                        typed: &typed,
                        ink,
                        spot: Some(BoxSpot::Choice(at)),
                        cursor: open,
                        after: problem,
                    },
                );
            } else {
                push(&mut row, name, ink, width);
                if lit {
                    push(&mut row, " · enter to type", theme.faint(), width);
                }
                out.spots
                    .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
                out.lines.push(row);
            }
        }
        // The question's note, once there is one or while it is typed.
        let note = self.note.text();
        if self.annotating || !note.is_empty() {
            let mut row = Line::default();
            push(&mut row, "  Note: ", theme.faint(), width);
            out.push_field(
                row,
                width,
                Typed {
                    typed: note,
                    ink: theme.text(),
                    spot: None,
                    cursor: self.annotating,
                    after: None,
                },
            );
        }
        // Skip and the way out are a footer, not answers: apart,
        // unnumbered, faint until highlighted.
        if rows.skip.is_none() && rows.out.is_none() {
            return out;
        }
        out.lines.push(Line::default());
        if let Some(at) = rows.skip {
            let lit = self.selected == at;
            let mut row = Line::default();
            push(
                &mut row,
                if lit { "› " } else { "  " },
                theme.attention(),
                width,
            );
            push(
                &mut row,
                "Skip",
                if lit { theme.bright() } else { theme.faint() },
                width,
            );
            out.spots
                .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
            out.lines.push(row);
        }
        let Some(at) = rows.out else {
            return out;
        };
        let lit = self.selected == at;
        let mut row = Line::default();
        push(
            &mut row,
            if lit { "› " } else { "  " },
            theme.attention(),
            width,
        );
        let ink = if lit { theme.bright() } else { theme.faint() };
        let typed = self.reply.text();
        if self.replying || !typed.is_empty() {
            // "Reply instead: <words>", the words as typed.
            push(&mut row, format!("{footer}: "), ink, width);
            let replying = self.replying;
            out.push_field(
                row,
                width,
                Typed {
                    typed,
                    ink: theme.bright(),
                    spot: Some(BoxSpot::Choice(at)),
                    cursor: replying,
                    after: None,
                },
            );
            return out;
        }
        push(&mut row, footer, ink, width);
        push(&mut row, "  esc", theme.faint(), width);
        out.spots
            .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
        out.lines.push(row);
        out
    }
}

/// A form's fields asked the way questions are: a choice is single choice,
/// a yes-or-no is "Yes" / "No", several picks are checkboxes, and text or a
/// number is typed.
fn form_questions(fields: &[Field]) -> Vec<QuestionView> {
    let option = |label: &str| ui_view::OptionView {
        label: label.to_owned(),
        description: String::new(),
        preview: String::new(),
        recommended: false,
    };
    fields
        .iter()
        .map(|field| {
            let (options, multi_select): (Vec<&str>, bool) = match &field.kind {
                FieldKind::Choice { options } => {
                    (options.iter().map(String::as_str).collect(), false)
                }
                FieldKind::Many { options, .. } => {
                    (options.iter().map(String::as_str).collect(), true)
                }
                FieldKind::Toggle => (vec!["Yes", "No"], false),
                FieldKind::Text { .. } | FieldKind::Number { .. } => (Vec::new(), false),
            };
            QuestionView {
                header: field.title.clone(),
                question: field.title.clone(),
                multi_select,
                allow_other: options.is_empty(),
                options: options.into_iter().map(option).collect(),
                secret: false,
            }
        })
        .collect()
}

/// A form field's value as its question's pick: a toggle is "Yes" (the
/// first row) or "No", a choice or several its options, and text or a
/// number what is typed.
fn form_pick(field: &Field, value: &FormValue) -> QuestionPick {
    let mut pick = QuestionPick::default();
    match (&field.kind, value) {
        (FieldKind::Toggle, FormValue::Toggle(on)) => pick.selected = vec![u32::from(!*on)],
        (FieldKind::Choice { .. }, FormValue::Choice(Some(at))) => pick.selected = vec![*at],
        (FieldKind::Many { .. }, FormValue::Many(picked)) => pick.selected = picked.clone(),
        (FieldKind::Text { .. } | FieldKind::Number { .. }, FormValue::Text(typed))
            if !typed.is_empty() =>
        {
            pick.other = Some(typed.clone())
        }
        _ => {}
    }
    pick
}

/// A form field's question pick back as the value the shared view checks.
fn form_value(field: &Field, pick: &QuestionPick) -> FormValue {
    match &field.kind {
        FieldKind::Toggle => FormValue::Toggle(pick.selected.first() == Some(&0)),
        FieldKind::Choice { .. } => FormValue::Choice(pick.selected.first().copied()),
        FieldKind::Many { .. } => FormValue::Many(pick.selected.clone()),
        FieldKind::Text { .. } | FieldKind::Number { .. } => {
            FormValue::Text(pick.other.clone().unwrap_or_default())
        }
    }
}

/// What is wrong with a form field, said after its name: "is required",
/// "needs a whole number", "needs at least 3".
fn problem_words(problem: &FieldProblem) -> String {
    let plural = |n: u32, one: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {one}s")
        }
    };
    match problem {
        FieldProblem::Required => "is required".to_owned(),
        FieldProblem::NotANumber => "needs a number".to_owned(),
        FieldProblem::NotWholeNumber => "needs a whole number".to_owned(),
        FieldProblem::BelowMinimum { minimum } => format!("needs at least {minimum}"),
        FieldProblem::AboveMaximum { maximum } => format!("needs at most {maximum}"),
        FieldProblem::TooShort { min_length } => {
            format!("needs at least {}", plural(*min_length, "character"))
        }
        FieldProblem::TooLong { max_length } => {
            format!("takes at most {}", plural(*max_length, "character"))
        }
        FieldProblem::TooFew { min_items } => format!("needs at least {min_items} picked"),
        FieldProblem::TooMany { max_items } => format!("takes at most {max_items} picked"),
    }
}

/// "write target/, read ~/.cargo · the network (api.github.com)": what an
/// access grant asks for.
pub(crate) fn access_words(
    read: &[String],
    write: &[String],
    network: bool,
    hosts: &[String],
) -> String {
    let mut parts = Vec::new();
    if !write.is_empty() {
        parts.push(format!("write {}", write.join(", ")));
    }
    if !read.is_empty() {
        parts.push(format!("read {}", read.join(", ")));
    }
    if network {
        parts.push(if hosts.is_empty() {
            "the network".to_owned()
        } else {
            format!("the network ({})", hosts.join(", "))
        });
    }
    if parts.is_empty() {
        "more access".to_owned()
    } else {
        parts.join(" · ")
    }
}

fn middle_cut(text: &str, max: usize) -> String {
    text::ellipsize_middle(text, max)
}

impl AskUi {
    /// The form's values as the person left them, in field order.
    fn form_values(&self) -> Vec<FormValue> {
        self.fields
            .iter()
            .zip(&self.picks)
            .map(|(field, pick)| form_value(field, pick))
            .collect()
    }

    /// Every field's problem, as the shared view checks the form.
    fn form_problems(&self) -> Vec<ui_view::FormProblem> {
        form_problems(&self.fields, &self.form_values())
    }

    /// The problem of the form's field `at`, if it has one.
    fn field_problem(&self, at: usize) -> Option<FieldProblem> {
        self.form_problems()
            .into_iter()
            .find(|problem| problem.field as usize == at)
            .map(|problem| problem.problem)
    }

    /// Why the open form field cannot take `typed`, checked as the shared
    /// view checks the whole form; never for a question's own text.
    fn typed_problem(&self, typed: &str) -> Option<FieldProblem> {
        let field = self.fields.get(self.step)?;
        form_problems(
            std::slice::from_ref(field),
            &[FormValue::Text(typed.to_owned())],
        )
        .into_iter()
        .next()
        .map(|problem| problem.problem)
    }

    /// The answer of the card's choice with `outcome`.
    fn choose(card: &AskCard, outcome: &ChoiceOutcome) -> AskAction {
        card.choices
            .iter()
            .find(|choice| &choice.outcome == outcome)
            .and_then(|choice| answer_input(card, &choice.answer, ""))
            .map_or(AskAction::None, |input| AskAction::Answer(Box::new(input)))
    }

    /// Submits the form with what was given for each field, as the shared
    /// view encodes it.
    fn submit_form(&mut self, card: &AskCard) -> AskAction {
        match form_answer(card, &self.form_values()) {
            Ok(Some(answer)) => answer_input(card, &answer, "")
                .map_or(AskAction::None, |input| AskAction::Answer(Box::new(input))),
            Ok(None) | Err(_) => AskAction::None,
        }
    }

    /// A link's rows: open it, say it is done, and the Decline footer.
    fn link_act(&mut self, card: &AskCard, at: usize) -> AskAction {
        self.selected = at;
        match at {
            0 => match &card.body {
                AskBody::Link { url, .. } => AskAction::OpenUrl(url.clone()),
                _ => AskAction::None,
            },
            1 => Self::choose(card, &ChoiceOutcome::OpenLink),
            _ => Self::choose(card, &ChoiceOutcome::Decline),
        }
    }

    fn link_key(&mut self, card: &AskCard, key: KeyEvent) -> AskAction {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(2),
            KeyCode::Char(c @ '1'..='2') => return self.link_act(card, c as usize - '1' as usize),
            KeyCode::Esc => self.selected = 2,
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                return self.link_act(card, self.selected);
            }
            _ => {}
        }
        AskAction::None
    }

    fn link_click(&mut self, card: &AskCard, spot: BoxSpot) -> AskAction {
        match spot {
            BoxSpot::Choice(at) => self.link_act(card, at),
            BoxSpot::FullDiff => self.link_act(card, 0),
            _ => AskAction::None,
        }
    }

    /// A tool server's link: who wants it and why, the link itself (a
    /// click opens it), then the ways on.
    fn link_lines(
        &self,
        server: &str,
        message: &str,
        url: &str,
        width: usize,
        theme: Theme,
    ) -> BoxLines {
        let mut out = BoxLines {
            lines: Vec::new(),
            cursor: None,
            spots: Vec::new(),
        };
        let mut head = Line::from(Span::styled("● ", theme.attention()));
        push(
            &mut head,
            format!("{server} sent a link"),
            theme.text(),
            width,
        );
        out.lines.push(head);
        out.lines.push(Line::default());
        for part in text::wrap(message, width.max(1)) {
            let mut line = Line::default();
            push(&mut line, part, theme.text(), width);
            out.lines.push(line);
        }
        let mut link = Line::default();
        let shown = middle_cut(url, width);
        push(&mut link, shown.clone(), theme.code(), width);
        out.spots.push((
            out.lines.len(),
            (0, text::str_width(&shown)),
            BoxSpot::FullDiff,
        ));
        out.lines.push(link);
        out.lines.push(Line::default());
        for (at, words) in ["Open the link", "Done"].iter().enumerate() {
            let lit = self.selected == at;
            let mut row = Line::default();
            push(
                &mut row,
                if lit { "› " } else { "  " },
                theme.attention(),
                width,
            );
            let ink = if lit { theme.bright() } else { theme.text() };
            push(&mut row, format!("{}. {words}", at + 1), ink, width);
            out.spots
                .push((out.lines.len(), (0, width), BoxSpot::Choice(at)));
            out.lines.push(row);
        }
        out.lines.push(Line::default());
        let lit = self.selected == 2;
        let mut row = Line::default();
        push(
            &mut row,
            if lit { "› " } else { "  " },
            theme.attention(),
            width,
        );
        push(
            &mut row,
            "Decline",
            if lit { theme.bright() } else { theme.faint() },
            width,
        );
        push(&mut row, "  esc", theme.faint(), width);
        out.spots
            .push((out.lines.len(), (0, width), BoxSpot::Choice(2)));
        out.lines.push(row);
        out
    }

    /// What this client cannot answer: Enter opens the agent's own
    /// terminal where that is possible; otherwise only Ctrl+X ends it.
    fn unanswerable_key(&mut self, key: KeyEvent) -> AskAction {
        if self.attach && key.code == KeyCode::Enter {
            return AskAction::Attach;
        }
        AskAction::None
    }

    fn unanswerable_lines(&self, reason: &str, width: usize, theme: Theme) -> BoxLines {
        let mut out = BoxLines {
            lines: Vec::new(),
            cursor: None,
            spots: Vec::new(),
        };
        let mut head = Line::from(Span::styled("● ", theme.attention()));
        push(&mut head, "Can't answer this here", theme.text(), width);
        out.lines.push(head);
        out.lines.push(Line::default());
        let reason = if reason.is_empty() {
            "The agent is showing something this build cannot read."
        } else {
            reason
        };
        for part in text::wrap(reason, width.max(1)) {
            let mut line = Line::default();
            push(&mut line, part, theme.muted(), width);
            out.lines.push(line);
        }
        if self.attach {
            out.lines.push(Line::default());
            let mut row = Line::default();
            push(&mut row, "› ", theme.attention(), width);
            push(&mut row, "Open Claude's terminal", theme.bright(), width);
            out.spots
                .push((out.lines.len(), (0, width), BoxSpot::Choice(0)));
            out.lines.push(row);
        }
        out
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    /// An authored form card with a Submit that sends Claude's accept.
    fn form_card(schema: &[u8]) -> AskCard {
        AskCard {
            kind: wire::Kind::ClaudeSdk,
            key: "ask".into(),
            item_key: "item".into(),
            position: 1,
            count: 1,
            body: AskBody::Form {
                server: "linear".into(),
                message: "Details".into(),
                fields: ui_view::form_fields(schema),
            },
            choices: vec![Choice {
                outcome: ChoiceOutcome::Submit,
                primary: true,
                takes_note: false,
                answer: ui_view::Answer::Claude(wire::ClaudeAnswer {
                    of: Some(wire::claude_answer::Of::Form(wire::FormAnswer {
                        action: wire::FormAction::Accept as i32,
                        content_json: vec![],
                    })),
                    ..Default::default()
                }),
            }],
            question_note: false,
            question_skip: false,
            question_reply: false,
            stops_turn: true,
            default_to_no: false,
            state: CardState::Open,
        }
    }

    fn press(ui: &mut AskUi, card: &AskCard, code: KeyCode) -> AskAction {
        ui.box_key(card, KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn typed(ui: &mut AskUi, card: &AskCard, text: &str) {
        for c in text.chars() {
            press(ui, card, KeyCode::Char(c));
        }
    }

    fn screen(ui: &AskUi, card: &AskCard) -> String {
        ui.box_lines(card, 100, Theme::default())
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn content(action: AskAction) -> serde_json::Value {
        let AskAction::Answer(input) = action else {
            panic!("{action:?}");
        };
        let Some(wire::KindInput::ClaudeSdk(wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Answer(answer)),
        })) = input.decode_body()
        else {
            panic!("{input:?}");
        };
        let Some(wire::claude_answer::Of::Form(form)) = answer.of else {
            panic!("not a form");
        };
        serde_json::from_slice(&form.content_json).unwrap()
    }

    /// Fields start at the schema's defaults and a toggle off, so a form
    /// whose defaults answer it goes as it stands, the untouched toggle
    /// sent off rather than left out.
    #[test]
    fn a_form_starts_from_its_defaults_and_sends_an_untouched_toggle_off() {
        let card = form_card(
            br#"{"type":"object","properties":{
                "team":{"enum":["core","apps"],"default":"apps"},
                "estimate":{"type":"integer","default":2},
                "urgent":{"type":"boolean"}
            },"required":["team","urgent"]}"#,
        );
        let mut ui = AskUi::default();
        ui.sync(&card);
        // Every tab is answered from the start; Tab past them reaches the
        // review, and Enter there submits.
        assert!(screen(&ui, &card).contains("✓ team   ✓ estimate   ✓ urgent"));
        press(&mut ui, &card, KeyCode::Tab);
        press(&mut ui, &card, KeyCode::Tab);
        press(&mut ui, &card, KeyCode::Tab);
        let review = screen(&ui, &card);
        assert!(
            review.contains("→ apps") && review.contains("→ No"),
            "{review}"
        );
        assert_eq!(
            content(press(&mut ui, &card, KeyCode::Enter)),
            serde_json::json!({"team": "apps", "estimate": 2, "urgent": false})
        );
    }

    /// What the shared check refuses is said on the field and on the review,
    /// in the problem's own words.
    #[test]
    fn a_fields_problem_is_worded_where_the_person_is() {
        let card = form_card(
            br#"{"type":"object","properties":{
                "estimate":{"type":"integer","minimum":1,"title":"Estimate"},
                "team":{"enum":["core","apps"],"title":"Team"}
            },"required":["team"]}"#,
        );
        let mut ui = AskUi::default();
        ui.sync(&card);
        typed(&mut ui, &card, "2.5");
        assert_eq!(press(&mut ui, &card, KeyCode::Enter), AskAction::None);
        assert!(screen(&ui, &card).contains("2.5 · needs a whole number"));
        // Typing again clears it; a number under the minimum is refused too.
        press(&mut ui, &card, KeyCode::Esc);
        press(&mut ui, &card, KeyCode::Up);
        typed(&mut ui, &card, "0");
        press(&mut ui, &card, KeyCode::Enter);
        assert!(screen(&ui, &card).contains("0 · needs at least 1"));
        // The review names the first field that keeps the form back.
        press(&mut ui, &card, KeyCode::Tab);
        press(&mut ui, &card, KeyCode::Tab);
        assert!(screen(&ui, &card).contains("Submit · Team is required"));
    }
}
