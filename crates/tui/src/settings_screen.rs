//! The settings screen: the everyday settings the daemon serves and the
//! profile's account, in the words onboarding asked them in. Every change
//! goes to the daemon at once; the screen draws what the daemon answers.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame as Paint;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::editor::Editor;
use crate::text::{self, push};
use crate::theme::Theme;

const MARGIN: usize = 2;
/// Where a row's words start: after the focus mark.
const LABEL_COL: usize = MARGIN + 2;

/// What the app does for the settings screen.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsEffect {
    Update(Box<wire::UpdateSettingsRequest>),
    SignIn,
    CancelSignIn,
    SignOut,
    /// Whether dragging over text in a chat copies it.
    CopyOnSelect(bool),
    /// Back to home.
    Close,
}

/// One row of the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Name,
    StartAtLogin,
    KeepAwake,
    Repository(usize),
    AddRepository,
    ChatIn,
    CopyOnSelect,
    Telemetry,
    AutoReports,
    Account,
}

#[derive(Debug)]
pub struct SettingsScreen {
    settings: wire::Settings,
    profile: Option<wire::Profile>,
    copy_on_select: bool,
    /// "Mac" or "computer", as "Keep this Mac awake" names it.
    machine: String,
    focus: usize,
    rename: Option<Editor>,
    adding: Option<Editor>,
    /// A browser sign-in is running: the code it shows, once it has one,
    /// and whether the browser could not be opened.
    signing_in: Option<Option<(wire::SignInCode, bool)>>,
    error: Option<String>,
    spots: Vec<(u16, (u16, u16), Hit)>,
}

#[derive(Clone, Debug, PartialEq)]
enum Hit {
    Key(KeyEvent),
    Row(usize),
}

fn plain(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// The plan the relay carries this machine's connections on, as a person
/// reads it: "Pro" or "Free"; nothing while the relay link is not up, when
/// the daemon holds no word of it.
pub fn plan_words(profile: &wire::Profile) -> String {
    use wire::cloud_standing::Standing;
    use wire::relay_link::State;
    let connected = match profile
        .cloud
        .as_ref()
        .and_then(|cloud| cloud.standing.as_ref())
    {
        Some(Standing::SignedIn(wire::RelayLink {
            state: Some(State::Connected(connected)),
        })) => connected,
        _ => return String::new(),
    };
    match connected.tier() {
        wire::Tier::Pro => "Pro".to_owned(),
        wire::Tier::Free => "Free".to_owned(),
        wire::Tier::Unspecified => String::new(),
    }
}

fn signed_in(profile: &wire::Profile) -> bool {
    matches!(
        profile
            .cloud
            .as_ref()
            .and_then(|cloud| cloud.standing.as_ref()),
        Some(wire::cloud_standing::Standing::SignedIn(_))
    )
}

impl SettingsScreen {
    pub fn new(
        settings: wire::Settings,
        profile: Option<wire::Profile>,
        machine: &str,
    ) -> SettingsScreen {
        SettingsScreen {
            settings,
            profile,
            copy_on_select: true,
            machine: machine.to_owned(),
            focus: 0,
            rename: None,
            adding: None,
            signing_in: None,
            error: None,
            spots: Vec::new(),
        }
    }

    /// The person's choice for dragging over text, which this client keeps
    /// itself rather than the host.
    pub fn copying_on_select(mut self, on: bool) -> SettingsScreen {
        self.copy_on_select = on;
        self
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::Name];
        if self.settings.start_at_login.is_some() {
            rows.push(Row::StartAtLogin);
        }
        rows.push(Row::KeepAwake);
        rows.extend((0..self.settings.repositories.len()).map(Row::Repository));
        rows.extend([
            Row::AddRepository,
            Row::ChatIn,
            Row::CopyOnSelect,
            Row::Telemetry,
            Row::AutoReports,
            Row::Account,
        ]);
        rows
    }

    fn focused(&self) -> Row {
        let rows = self.rows();
        rows[self.focus.min(rows.len() - 1)]
    }

    pub fn settings_changed(&mut self, settings: wire::Settings) {
        self.settings = settings;
        self.error = None;
    }

    pub fn profile_changed(&mut self, profile: wire::Profile) {
        if signed_in(&profile) {
            self.signing_in = None;
        }
        self.profile = Some(profile);
    }

    pub fn sign_in_code(&mut self, code: wire::SignInCode, opened: bool) {
        if self.signing_in.is_some() {
            self.signing_in = Some(Some((code, !opened)));
        }
    }

    pub fn signed_in(&mut self) {
        self.signing_in = None;
    }

    pub fn failed(&mut self, why: String) {
        self.signing_in = None;
        self.error = Some(why);
    }

    /// Whether a browser sign-in is running, for closing the screen to
    /// abandon it.
    pub fn signing_in(&self) -> bool {
        self.signing_in.is_some()
    }

    pub fn field_text(&self) -> bool {
        self.rename
            .as_ref()
            .or(self.adding.as_ref())
            .is_some_and(|editor| !editor.is_empty())
    }

    pub fn kill_field(&mut self) {
        if let Some(editor) = self.rename.as_mut().or(self.adding.as_mut()) {
            editor.kill_all();
        }
    }

    pub fn paste(&mut self, pasted: &str) {
        if let Some(editor) = self.rename.as_mut().or(self.adding.as_mut()) {
            editor.insert_str(pasted.trim());
        }
    }

    fn update(&mut self, request: wire::UpdateSettingsRequest) -> Vec<SettingsEffect> {
        self.error = None;
        vec![SettingsEffect::Update(Box::new(request))]
    }

    pub fn key(&mut self, key: KeyEvent) -> Vec<SettingsEffect> {
        if let Some(editor) = &mut self.rename {
            match key.code {
                KeyCode::Esc => self.rename = None,
                KeyCode::Enter => {
                    let name = editor.text().trim().to_owned();
                    self.rename = None;
                    self.settings.host_name = name.clone();
                    return self.update(wire::UpdateSettingsRequest {
                        host_name: Some(name),
                        ..Default::default()
                    });
                }
                _ => {
                    editor.key(key);
                }
            }
            return Vec::new();
        }
        if let Some(editor) = &mut self.adding {
            match key.code {
                KeyCode::Esc => self.adding = None,
                KeyCode::Enter => {
                    let path = editor.text().trim().to_owned();
                    self.adding = None;
                    if path.is_empty() {
                        return Vec::new();
                    }
                    let mut paths = self.settings.repositories.clone();
                    paths.push(path);
                    return self.update(wire::UpdateSettingsRequest {
                        repositories: Some(wire::RepositoryList { paths }),
                        ..Default::default()
                    });
                }
                _ => {
                    editor.key(key);
                }
            }
            return Vec::new();
        }
        let rows = self.rows().len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char(',') => {
                let mut effects = Vec::new();
                if self.signing_in.take().is_some() {
                    effects.push(SettingsEffect::CancelSignIn);
                }
                effects.push(SettingsEffect::Close);
                return effects;
            }
            KeyCode::Up | KeyCode::Char('k') => self.focus = self.focus.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.focus = (self.focus + 1).min(rows - 1),
            KeyCode::Home | KeyCode::Char('g') => self.focus = 0,
            KeyCode::End | KeyCode::Char('G') => self.focus = rows - 1,
            KeyCode::Enter | KeyCode::Char(' ') => return self.change(),
            KeyCode::Char('x') | KeyCode::Backspace | KeyCode::Delete => {
                if let Row::Repository(at) = self.focused() {
                    let mut paths = self.settings.repositories.clone();
                    paths.remove(at);
                    return self.update(wire::UpdateSettingsRequest {
                        repositories: Some(wire::RepositoryList { paths }),
                        ..Default::default()
                    });
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// Enter or Space on the focused row: a switch flips, a name or a
    /// folder opens its field, the account signs in or out.
    fn change(&mut self) -> Vec<SettingsEffect> {
        let settings = &self.settings;
        let request = match self.focused() {
            Row::Name => {
                let mut editor = Editor::default();
                editor.set(&settings.host_name, Vec::new());
                self.rename = Some(editor);
                return Vec::new();
            }
            Row::AddRepository => {
                self.adding = Some(Editor::default());
                return Vec::new();
            }
            Row::Repository(_) => return Vec::new(),
            Row::CopyOnSelect => {
                self.copy_on_select = !self.copy_on_select;
                return vec![SettingsEffect::CopyOnSelect(self.copy_on_select)];
            }
            Row::StartAtLogin => wire::UpdateSettingsRequest {
                start_at_login: settings.start_at_login.map(|on| !on),
                ..Default::default()
            },
            Row::KeepAwake => wire::UpdateSettingsRequest {
                keep_awake: Some(!settings.keep_awake),
                ..Default::default()
            },
            Row::ChatIn => wire::UpdateSettingsRequest {
                chat_in: Some(match settings.chat_in() {
                    wire::ChatIn::Native => wire::ChatIn::Amux,
                    _ => wire::ChatIn::Native,
                } as i32),
                ..Default::default()
            },
            Row::Telemetry => wire::UpdateSettingsRequest {
                telemetry: Some(!settings.telemetry),
                ..Default::default()
            },
            Row::AutoReports => wire::UpdateSettingsRequest {
                auto_reports: Some(!settings.auto_reports),
                ..Default::default()
            },
            Row::Account => {
                if self.profile.as_ref().is_some_and(signed_in) {
                    return vec![SettingsEffect::SignOut];
                }
                if self.signing_in.is_some() {
                    return Vec::new();
                }
                self.error = None;
                self.signing_in = Some(None);
                return vec![SettingsEffect::SignIn];
            }
        };
        self.update(request)
    }

    pub fn mouse(&mut self, mouse: MouseEvent) -> Vec<SettingsEffect> {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return Vec::new();
        }
        let hit = self
            .spots
            .iter()
            .find(|(row, (from, to), _)| *row == mouse.row && (*from..*to).contains(&mouse.column))
            .map(|(.., hit)| hit.clone());
        match hit {
            Some(Hit::Key(key)) => self.key(key),
            Some(Hit::Row(row)) => {
                self.focus = row;
                self.change()
            }
            None => Vec::new(),
        }
    }

    fn label(&self, row: Row) -> String {
        match row {
            Row::Name => "This machine's name".to_owned(),
            Row::StartAtLogin => "Start amux when you log in".to_owned(),
            Row::KeepAwake => format!("Keep this {} awake while it's plugged in", self.machine),
            Row::Repository(at) => text::tilde(&self.settings.repositories[at]),
            Row::AddRepository => "+ Add a folder…".to_owned(),
            Row::ChatIn => "Chat with agents in".to_owned(),
            Row::CopyOnSelect => "Copy text when you select it".to_owned(),
            Row::Telemetry => "Send usage counts".to_owned(),
            Row::AutoReports => "Send reports automatically".to_owned(),
            Row::Account => match &self.profile {
                Some(profile) if signed_in(profile) => {
                    let who = [&profile.email, &profile.account_name]
                        .into_iter()
                        .find(|who| !who.is_empty())
                        .cloned()
                        .unwrap_or_else(|| "your account".to_owned());
                    format!("Signed in as {who}")
                }
                _ => "Not signed in".to_owned(),
            },
        }
    }

    fn value(&self, row: Row) -> String {
        let settings = &self.settings;
        match row {
            Row::Name => settings.host_name.clone(),
            Row::StartAtLogin => on_off(settings.start_at_login == Some(true)).to_owned(),
            Row::KeepAwake => on_off(settings.keep_awake).to_owned(),
            Row::ChatIn => match settings.chat_in() {
                wire::ChatIn::Native => "each agent's own interface",
                _ => "amux's chat",
            }
            .to_owned(),
            Row::CopyOnSelect => on_off(self.copy_on_select).to_owned(),
            Row::Telemetry => on_off(settings.telemetry).to_owned(),
            Row::AutoReports => on_off(settings.auto_reports).to_owned(),
            Row::Account => match &self.profile {
                Some(profile) if signed_in(profile) => plan_words(profile),
                _ => String::new(),
            },
            Row::Repository(_) | Row::AddRepository => String::new(),
        }
    }

    /// The section a row opens, when it is the first of one.
    fn heading(row: Row, previous: Option<Row>) -> Option<&'static str> {
        let section = |row: Row| match row {
            Row::Name | Row::StartAtLogin | Row::KeepAwake => "This machine",
            Row::Repository(_) | Row::AddRepository => "Repositories",
            Row::ChatIn | Row::CopyOnSelect => "Chats",
            Row::Telemetry | Row::AutoReports => "Help improve amux",
            Row::Account => "Account",
        };
        (previous.map(section) != Some(section(row))).then(|| section(row))
    }

    fn hints(&self) -> Vec<(&'static str, &'static str, KeyEvent)> {
        let key = plain;
        if self.rename.is_some() || self.adding.is_some() {
            let what = if self.rename.is_some() { "save" } else { "add" };
            return vec![
                ("enter", what, key(KeyCode::Enter)),
                ("esc", "cancel", key(KeyCode::Esc)),
            ];
        }
        let back = ("esc", "back", key(KeyCode::Esc));
        let account_signed_in = self.profile.as_ref().is_some_and(signed_in);
        match self.focused() {
            Row::Repository(_) => vec![("x", "remove", key(KeyCode::Char('x'))), back],
            Row::Name => vec![("enter", "rename", key(KeyCode::Enter)), back],
            Row::AddRepository => vec![("enter", "add a folder", key(KeyCode::Enter)), back],
            Row::Account if account_signed_in => {
                vec![("enter", "sign out", key(KeyCode::Enter)), back]
            }
            Row::Account if self.signing_in.is_some() => vec![back],
            Row::Account => vec![("enter", "sign in", key(KeyCode::Enter)), back],
            _ => vec![("enter", "change", key(KeyCode::Enter)), back],
        }
    }

    pub fn draw(
        &mut self,
        paint: &mut Paint<'_>,
        area: Rect,
        footer: Option<Line<'static>>,
        theme: Theme,
    ) {
        let width = usize::from(area.width);
        let height = usize::from(area.height);
        self.spots.clear();
        if height < 4 || width < 2 * MARGIN + 8 {
            return;
        }
        let rows = self.rows();
        let focus = self.focus.min(rows.len() - 1);
        let column = rows
            .iter()
            .filter(|row| !matches!(row, Row::Repository(_) | Row::AddRepository))
            .map(|row| text::str_width(&self.label(*row)))
            .max()
            .unwrap_or(0)
            + LABEL_COL
            + 3;
        let mut lines: Vec<Line<'static>> = vec![Line::default()];
        let mut top = Line::from(Span::raw(" ".repeat(MARGIN)));
        push(&mut top, "Settings", theme.emphasis(), width);
        lines.push(top);
        let mut spots: Vec<(usize, (usize, usize), Hit)> = Vec::new();
        let mut cursor = None;
        let mut focus_line = 0;
        let mut previous = None;
        for (at, row) in rows.iter().copied().enumerate() {
            if let Some(heading) = Self::heading(row, previous) {
                lines.push(Line::default());
                let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
                push(
                    &mut line,
                    heading,
                    theme.muted().add_modifier(ratatui::style::Modifier::BOLD),
                    width,
                );
                push(&mut line, " ", theme.muted(), width);
                let rule = width.saturating_sub(text::line_width(&line) + MARGIN);
                push(&mut line, "─".repeat(rule), theme.hairline(), width);
                lines.push(line);
            }
            previous = Some(row);
            let chosen = at == focus;
            if chosen {
                focus_line = lines.len();
            }
            let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
            push(
                &mut line,
                if chosen { "› " } else { "  " },
                theme.attention(),
                width,
            );
            let label_ink = match row {
                Row::AddRepository if !chosen => theme.muted(),
                _ if chosen => theme.bright(),
                _ => theme.text(),
            };
            let field = match row {
                Row::Name => self.rename.as_ref(),
                Row::AddRepository => self.adding.as_ref(),
                _ => None,
            };
            match field {
                Some(editor) => {
                    let label = if row == Row::Name {
                        self.label(row)
                    } else {
                        "Folder".to_owned()
                    };
                    push(&mut line, label, theme.muted(), width);
                    text::pad_to(&mut line, column.min(width.saturating_sub(12)));
                    push(&mut line, "▎ ", theme.attention(), width);
                    let from = text::line_width(&line);
                    push(&mut line, editor.text(), theme.text(), width);
                    let before: String =
                        editor.text().chars().take(editor.cursor_chars()).collect();
                    cursor = Some((from + text::str_width(&before), lines.len()));
                }
                None => {
                    push(&mut line, self.label(row), label_ink, width);
                    let value = self.value(row);
                    if !value.is_empty() {
                        if text::line_width(&line) + 2 < column {
                            text::pad_to(&mut line, column);
                        } else {
                            push(&mut line, "  ", theme.muted(), width);
                        }
                        let ink = if chosen {
                            theme.bright()
                        } else {
                            theme.muted()
                        };
                        push(&mut line, value, ink, width);
                    }
                }
            }
            spots.push((lines.len(), (MARGIN, width - MARGIN), Hit::Row(at)));
            lines.push(line);
            // What a row needs said under it.
            let under: Vec<(String, ratatui::style::Style)> = match row {
                Row::KeepAwake => vec![(
                    "On battery it stays awake only while an agent works.".to_owned(),
                    theme.faint(),
                )],
                Row::Telemetry => vec![("Never your code or chats.".to_owned(), theme.faint())],
                Row::Account => match &self.signing_in {
                    Some(Some((code, unopened))) => {
                        let mut said = vec![(
                            format!(
                                "Approve amux in your browser. It should show {}.",
                                code.user_code
                            ),
                            theme.text(),
                        )];
                        if *unopened {
                            said.push((
                                format!(
                                    "The browser did not open; open {}",
                                    code.verification_uri_complete
                                ),
                                theme.muted(),
                            ));
                        }
                        said
                    }
                    Some(None) => vec![("Starting sign-in…".to_owned(), theme.faint())],
                    None => Vec::new(),
                },
                _ => Vec::new(),
            };
            for (words, style) in under {
                for part in text::wrap(&words, width.saturating_sub(LABEL_COL + MARGIN)) {
                    let mut line = Line::from(Span::raw(" ".repeat(LABEL_COL)));
                    push(&mut line, part, style, width);
                    lines.push(line);
                }
            }
        }
        if let Some(error) = &self.error {
            lines.push(Line::default());
            for part in text::wrap(error, width - 2 * MARGIN) {
                let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
                push(&mut line, part, theme.warning(), width);
                lines.push(line);
            }
        }
        // Scrolled to keep the focused row in view, under the title.
        let body = height.saturating_sub(4);
        let head = 2;
        let skip = (focus_line + 2).saturating_sub(head + body);
        let mut shown: Vec<Line<'static>> = lines[..head].to_vec();
        shown.extend(lines.iter().skip(head + skip).take(body).cloned());
        shown.resize(height - 1, Line::default());
        let mut keys = Line::from(Span::raw(" ".repeat(MARGIN)));
        let mut key_spots = Vec::new();
        match footer {
            Some(footer) => keys.spans.extend(
                footer
                    .spans
                    .into_iter()
                    .skip_while(|span| span.content.trim().is_empty()),
            ),
            None => {
                let hints = self.hints();
                let pairs: Vec<(&str, &str)> =
                    hints.iter().map(|(key, word, _)| (*key, *word)).collect();
                for (at, columns) in text::push_hints(&mut keys, &pairs, width - 2 * MARGIN, theme)
                {
                    key_spots.push((height - 1, columns, Hit::Key(hints[at].2)));
                }
            }
        }
        shown.push(keys);
        let moved =
            |line: usize| (line >= head + skip && line < head + skip + body).then(|| line - skip);
        self.spots = spots
            .into_iter()
            .filter_map(|(line, columns, hit)| Some((moved(line)?, columns, hit)))
            .chain(key_spots)
            .map(|(line, (from, to), hit)| {
                (
                    area.y + line as u16,
                    (area.x + from as u16, area.x + to as u16),
                    hit,
                )
            })
            .collect();
        paint.render_widget(Paragraph::new(shown), area);
        if let Some((x, line)) = cursor
            && let Some(y) = moved(line)
        {
            paint.set_cursor_position(Position::new(
                area.x + x.min(width - 1) as u16,
                area.y + y as u16,
            ));
        }
    }
}
