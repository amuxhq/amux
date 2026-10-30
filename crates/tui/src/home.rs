//! Home: a top line, the list of agents, and one line of hints. No frame
//! and no composer; a new agent starts as an empty chat whose composer
//! names what it will be.
//!
//! The list is the shared fleet view's sections: the families that need
//! you, those live, and those exited, each placed by its loudest member
//! and ordered by when it last changed state. Order moves only when an
//! agent's state changes — a send, a turn starting or ending, an ask — never
//! while it streams, so nothing moves under the cursor or the mouse. Hover and
//! selection are one highlight: moving the mouse over a row selects it,
//! and keys take over until the mouse moves again.

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame as Paint;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use settings::ChatInSetting;
use ui_state::{ActivityKind, AgentKey, Attention, Connection, FleetState};
use ui_view::{
    ActivityLine, AskSubject, AskSummary, ExitCause, FleetRow, FleetSection, FleetView, Offline,
    Reach, SecondLine, SectionKind, SessionLine, StuckReason,
};
use wire::{Kind, Lifecycle, SignInState};

use crate::chat::composer::editor_lines;
use crate::editor::Editor;
use crate::fleet::FleetEffect;
use crate::hosts;
use crate::setup::{Form, FormHit, FormOutcome, Item as Setting, Pick, Picker, Setup};
use crate::text::{self, pad_to, push};
use crate::theme::Theme;

/// Blank columns at each side of the screen.
const MARGIN: usize = 2;
/// Where section headings start, and the `›` of a highlight without a tint.
const HEAD_COL: usize = MARGIN;
/// Where a row's mark sits, and the text after it.
const MARK_COL: usize = MARGIN + 2;
const NAME_COL: usize = MARK_COL + 2;
/// From this height on, rows keep a blank line between them.
const ROOMY: u16 = 30;
/// The composer takes at most this share of the draft screen's height.
const COMPOSER_SHARE: usize = 2;
/// The highlighted row's action: stop, or delete once exited. Brackets mark
/// what can be clicked.
const CLOSE: &str = "[x]";
/// The fewest columns a folder is cut to before the host gives way.
const FOLDER_LEAST: usize = 8;

/// Something the list can highlight.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    New,
    Agent(AgentKey),
    /// A section's heading, which folds and unfolds it.
    Section(SectionKind),
}

fn section_words(section: SectionKind) -> &'static str {
    match section {
        SectionKind::NeedsYou => "Needs you",
        SectionKind::Live => "Live",
        SectionKind::Exited => "Exited",
    }
}

/// What a click at a place does.
#[derive(Clone, Debug)]
enum Hit {
    /// A whole row: hovering highlights it, clicking also opens it.
    Row(Target),
    /// The highlighted row's `×`: stop, or delete once exited.
    Close(AgentKey),
    /// A family's fold marker.
    Fold(AgentKey),
    /// A hint: the key it names.
    Key(KeyEvent),
    /// One of the draft's settings on the composer's edge.
    Setting(Setting),
}

/// A clickable span of one screen row.
#[derive(Clone, Debug)]
struct Spot {
    y: u16,
    x: (u16, u16),
    hit: Hit,
}

/// One laid-out line and what clicking along it does. `spots` hold
/// column ranges; a `None` range spans the whole line.
#[derive(Default)]
struct Laid {
    line: Line<'static>,
    spots: Vec<(Option<(usize, usize)>, Hit)>,
}

impl Laid {
    fn plain(line: Line<'static>) -> Self {
        Laid {
            line,
            spots: Vec::new(),
        }
    }

    fn row(line: Line<'static>, target: Target) -> Self {
        Laid {
            line,
            spots: vec![(None, Hit::Row(target))],
        }
    }
}

/// A new agent before it exists: the first prompt and what will run it.
#[derive(Debug, Default)]
pub struct Draft {
    pub editor: Editor,
    /// What will run it and where; filled from the defaults when it first
    /// shows.
    pub setup: Option<Setup>,
    /// A setting being chosen, in a flyover over its place on the edge.
    picker: Option<Picker>,
    /// For an agent used in its own terminal, the form that starts it in
    /// place of the composer.
    form: Option<Form>,
    /// The flyover as last drawn, for clicks.
    flyover: crate::setup::Flyover,
    /// The modal's click targets as last drawn: (row, columns, hit).
    modal: Vec<(u16, (u16, u16), FormHit)>,
    /// Ctrl+S was pressed: the next letter names a setting.
    prefix: bool,
    /// The edge item under the pointer.
    hover: Option<Setting>,
    /// On screen. Leaving keeps the text for the next time.
    open: bool,
    /// The create call is in flight.
    starting: bool,
    /// Why the last start failed, shown in the form until the next start.
    error: Option<String>,
}

#[derive(Debug)]
enum Overlay {
    Hosts,
    Rename {
        agent: AgentKey,
        editor: Editor,
    },
    Confirm {
        agent: AgentKey,
        name: String,
        delete: bool,
        /// The button Enter presses; the arrows and Tab move it.
        focus: Confirmed,
    },
}

/// What home remembers between frames. Nothing here outlives the process.
#[derive(Debug, Default)]
pub struct Home {
    selected: Option<Target>,
    /// Where the highlight last stood in the list, so a row that leaves
    /// hands it to the row that takes its place, or the new last row.
    selected_at: Option<usize>,
    expanded: HashSet<Vec<u8>>,
    /// Sections the person folded or unfolded against their default.
    toggled: HashSet<SectionKind>,
    /// The filter's text while one is typed or kept.
    filter: Option<Editor>,
    /// The top line has the keys.
    filtering: bool,
    overlay: Option<Overlay>,
    pub draft: Draft,
    /// The first list line drawn.
    top: usize,
    /// A key moved the highlight, so the list scrolls to keep it in view.
    /// Hover and the wheel leave the list where it is.
    reveal: bool,
    /// Clickable places on the last frame, the most specific first.
    spots: Vec<Spot>,
    /// A modal's buttons on the last frame: (row, columns, what it does).
    overlay_spots: Vec<(u16, (u16, u16), Confirmed)>,
    /// Renaming: the name field's cursor as (column, list line) on the last
    /// frame, when its row is in view.
    rename_at: Option<(usize, usize)>,
}

/// A modal's two answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirmed {
    Yes,
    No,
}

/// One block of the list.
enum Item {
    New,
    /// A section's heading: its families, and whether it is folded.
    Heading(SectionKind, usize, bool),
    /// A blank line, unless the list already ends in one.
    Gap,
    /// A blank line, always.
    Space,
    Agent(Box<FleetRow>),
    Note(&'static str),
}

/// Which agent it is and where it works, as faint words after its name:
/// "Claude · ~/src/amux · laptop", the host only when not this machine.
/// Short of room, the folder gives way first, cut from the left down to
/// its last name, then the host, then the agent; the name and the age
/// beside them always stay.
fn where_it_runs(kind: Kind, cwd: &str, elsewhere: Option<&str>, room: usize) -> String {
    let who = ui_view::kind_name(kind);
    let folder = text::tilde(cwd.trim_end_matches('/'));
    let folder = if folder.is_empty() {
        cwd.to_owned()
    } else {
        folder
    };
    // The folder from whole to its last name: "~/a/b/c", "…/b/c", "c".
    let names: Vec<&str> = folder.split('/').collect();
    let mut folders = vec![folder.clone()];
    for from in 2..names.len() {
        folders.push(format!("…/{}", names[from..].join("/")));
    }
    if let Some(last) = names
        .last()
        .filter(|last| !last.is_empty() && **last != folder)
    {
        folders.push((*last).to_owned());
    }
    let join = |parts: &[&str]| {
        parts
            .iter()
            .filter(|part| !part.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" · ")
    };
    for folder in &folders {
        let line = join(&[who, folder, elsewhere.unwrap_or("")]);
        if text::str_width(&line) <= room {
            return line;
        }
    }
    let last = folders.last().map_or("", String::as_str);
    // A last name too long to fit (a folder with no separators) is cut in
    // the middle before the host or the agent gives way to it.
    let fixed = text::str_width(&join(&[who, "x", elsewhere.unwrap_or("")])) - 1;
    if room >= fixed + FOLDER_LEAST {
        let folder = text::ellipsize_middle(last, room - fixed);
        return join(&[who, &folder, elsewhere.unwrap_or("")]);
    }
    for parts in [[who, last], ["", last]] {
        let line = join(&parts);
        if text::str_width(&line) <= room {
            return line;
        }
    }
    text::ellipsize(last, room)
}

fn plain_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

impl Home {
    pub fn select(&mut self, agent: AgentKey) {
        self.selected = Some(Target::Agent(agent));
        self.reveal = true;
    }

    /// The create call came back: the draft is spent.
    pub fn started(&mut self) {
        // The next agent keeps these settings but not this one's name.
        let mut setup = self.draft.setup.take();
        if let Some(setup) = &mut setup {
            setup.name = None;
        }
        self.draft = Draft {
            setup,
            ..Draft::default()
        };
    }

    /// The create call failed: the draft is the person's again.
    /// Opens the new agent's screen with `setup`: a chat's own settings,
    /// copied to start a sibling.
    pub fn new_agent(&mut self, setup: Setup) {
        self.draft.setup = Some(setup);
        self.draft.picker = None;
        self.draft.form = None;
        self.draft.prefix = false;
        self.open_draft();
    }

    /// Onto the new agent's screen, with no error from an earlier visit.
    fn open_draft(&mut self) {
        self.draft.open = true;
        self.draft.error = None;
    }

    /// Back home from the new agent's screen. Words typed stay for the next
    /// visit with their settings; a draft left empty is abandoned, so the
    /// next new agent opens with the defaults again.
    fn leave_draft(&mut self) {
        self.draft.open = false;
        self.draft.picker = None;
        if self.draft.editor.is_empty() {
            self.draft.setup = None;
            self.draft.form = None;
        }
    }

    /// The new agent's setup while its screen is open, made from the
    /// defaults when it first shows.
    pub fn open_setup(&mut self, place: &Place<'_>) -> Option<&mut Setup> {
        if !self.draft.open {
            return None;
        }
        Some(self.draft.setup.get_or_insert_with(|| {
            Setup::defaults(
                place.chat_in,
                place.working_dir,
                place.local_host,
                place.defaults,
            )
        }))
    }

    /// The hosts modal, from a chat's `<leader> h`.
    pub fn open_hosts(&mut self) {
        self.draft.open = false;
        self.overlay = Some(Overlay::Hosts);
    }

    /// The create call failed: the form takes the person's settings back
    /// and says why. False when the form was left meanwhile, so the error
    /// has nowhere to show.
    pub fn start_failed(&mut self, error: String) -> bool {
        self.draft.starting = false;
        self.draft.error = self.draft.open.then_some(error);
        self.draft.open
    }

    /// Whether a text field has the keys and holds something, for Ctrl+C.
    pub fn field_text(&self) -> bool {
        if self.draft.open {
            return !self.draft.editor.is_empty();
        }
        match (&self.overlay, &self.filter) {
            (Some(Overlay::Rename { editor, .. }), _) => !editor.is_empty(),
            (None, Some(filter)) if self.filtering => !filter.is_empty(),
            _ => false,
        }
    }

    pub fn kill_field(&mut self) -> bool {
        if self.draft.open {
            return self.draft.editor.kill_all();
        }
        match (&mut self.overlay, &mut self.filter) {
            (Some(Overlay::Rename { editor, .. }), _) => editor.kill_all(),
            (None, Some(filter)) if self.filtering => filter.kill_all(),
            _ => false,
        }
    }

    /// A bracketed paste types into whichever field has the keys. A name
    /// and a filter are one line.
    pub fn paste(&mut self, pasted: &str) {
        if self.draft.open && !self.draft.starting {
            self.draft.editor.paste(pasted);
            return;
        }
        let one_line = pasted
            .split(['\r', '\n'])
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        match (&mut self.overlay, &mut self.filter) {
            (Some(Overlay::Rename { editor, .. }), _) => editor.insert_str(&one_line),
            (None, Some(filter)) if self.filtering => filter.insert_str(&one_line),
            _ => {}
        }
    }

    fn needle(&self) -> Option<String> {
        self.filter
            .as_ref()
            .map(|filter| filter.text().trim().to_lowercase())
            .filter(|needle| !needle.is_empty())
    }

    /// Home's sections from the shared fleet view, filtered by the needle.
    /// `lines` says what each agent's session knows for its second line.
    fn view(
        &self,
        fleet: &FleetState,
        local_host: &[u8],
        lines: &HashMap<AgentKey, SessionLine>,
    ) -> FleetView {
        let needle = self.needle();
        ui_view::fleet_view(fleet, local_host, lines, &self.expanded, &|agent| {
            needle
                .as_ref()
                .is_none_or(|needle| ui_view::matches(fleet, agent, needle))
        })
    }

    fn items(&self, fleet: &FleetState, view: FleetView) -> Vec<Item> {
        let mut items = vec![Item::New];
        if view.sections.is_empty() {
            items.push(Item::Gap);
            items.push(Item::Note(match (self.needle(), fleet.caught_up()) {
                (Some(_), _) => "nothing matches",
                (None, true) => "no agents yet",
                (None, false) => "loading…",
            }));
            return items;
        }
        for section in view.sections {
            // Two blank lines between sections, one under a heading.
            items.push(Item::Gap);
            items.push(Item::Space);
            let folded = self.folded(&section);
            items.push(Item::Heading(
                section.kind,
                section.families as usize,
                folded,
            ));
            if folded {
                continue;
            }
            items.push(Item::Gap);
            items.extend(
                section
                    .rows
                    .into_iter()
                    .map(|row| Item::Agent(Box::new(row))),
            );
        }
        items
    }

    /// The list's targets, which no second line changes, nor which host
    /// is this machine's.
    fn targets_of(&self, fleet: &FleetState) -> Vec<Target> {
        Self::targets(&self.items(fleet, self.view(fleet, &[], &HashMap::new())))
    }

    /// Whether a section's agents are hidden. A filter looks through every
    /// section, folded or not.
    fn folded(&self, section: &FleetSection) -> bool {
        self.needle().is_none() && section.folded_by_default != self.toggled.contains(&section.kind)
    }

    fn toggle_section(&mut self, section: SectionKind) {
        if !self.toggled.remove(&section) {
            self.toggled.insert(section);
        }
        self.reveal = true;
    }

    fn targets(items: &[Item]) -> Vec<Target> {
        items
            .iter()
            .filter_map(|item| match item {
                Item::New => Some(Target::New),
                Item::Agent(row) => Some(Target::Agent(row.card.agent.clone())),
                Item::Heading(section, ..) => Some(Target::Section(*section)),
                _ => None,
            })
            .collect()
    }

    /// The highlight, kept on something still listed: when its row left,
    /// the row now in its place (the new last row when it was last),
    /// brought into sight; with nothing highlighted yet, the first agent,
    /// else "+ New Agent".
    fn settle(&mut self, targets: &[Target]) -> Target {
        if let Some(at) = self
            .selected
            .as_ref()
            .and_then(|selected| targets.iter().position(|t| t == selected))
        {
            self.selected_at = Some(at);
            return targets[at].clone();
        }
        let in_place = self
            .selected
            .as_ref()
            .and(self.selected_at)
            .and_then(|at| targets.get(at.min(targets.len().saturating_sub(1))));
        if in_place.is_some() {
            self.reveal = true;
        }
        let fallback = in_place
            .or_else(|| {
                targets
                    .iter()
                    .find(|target| matches!(target, Target::Agent(_)))
            })
            .or(targets.first())
            .cloned()
            .unwrap_or(Target::New);
        self.selected_at = targets.iter().position(|t| *t == fallback);
        self.selected = Some(fallback.clone());
        fallback
    }

    fn step(&mut self, targets: &[Target], by: isize) {
        let current = self.settle(targets);
        let at = targets.iter().position(|t| *t == current).unwrap_or(0);
        let next = at
            .saturating_add_signed(by)
            .min(targets.len().saturating_sub(1));
        self.selected = targets.get(next).cloned();
        self.reveal = true;
    }

    fn activate(&mut self, target: Target) -> Vec<FleetEffect> {
        match target {
            Target::New => self.open_draft(),
            Target::Agent(agent) => return vec![FleetEffect::Open(agent)],
            Target::Section(section) => self.toggle_section(section),
        }
        vec![]
    }

    /// Asks before stopping a live agent or deleting an exited one: a live
    /// agent is never offered deletion.
    fn confirm_close(&mut self, fleet: &FleetState, agent: AgentKey) {
        if let Some(entry) = fleet.agent(&agent) {
            self.overlay = Some(Overlay::Confirm {
                name: entry.name.clone(),
                delete: ui_state::attention(entry) == Attention::Exited,
                agent,
                focus: Confirmed::Yes,
            });
        }
    }

    fn toggle(&mut self, agent: &AgentKey) {
        if !self.expanded.remove(&agent.agent) {
            self.expanded.insert(agent.agent.clone());
        }
    }

    pub fn key(&mut self, fleet: &FleetState, key: KeyEvent) -> Vec<FleetEffect> {
        if self.draft.open {
            return self.draft_key(fleet, key);
        }
        if let Some(overlay) = self.overlay.take() {
            return self.overlay_key(overlay, key);
        }
        let targets = self.targets_of(fleet);
        if self.filtering {
            self.filter_key(fleet, &targets, key);
            return vec![];
        }
        let current = self.settle(&targets);
        let agent = match &current {
            Target::Agent(agent) => Some(agent.clone()),
            _ => None,
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') => return vec![FleetEffect::Quit],
            KeyCode::Char('?') => return vec![FleetEffect::Help],
            KeyCode::Char('b') => return vec![FleetEffect::Report],
            KeyCode::Up | KeyCode::Char('k') => self.step(&targets, -1),
            KeyCode::Down | KeyCode::Char('j') => self.step(&targets, 1),
            KeyCode::Home | KeyCode::Char('g') => self.step(&targets, isize::MIN),
            KeyCode::End | KeyCode::Char('G') => self.step(&targets, isize::MAX),
            KeyCode::Enter | KeyCode::Char('a') if ctrl || key.code != KeyCode::Enter => {
                return agent.map(FleetEffect::Attach).into_iter().collect();
            }
            KeyCode::Enter => return self.activate(current),
            KeyCode::Char('/') => {
                self.filtering = true;
                self.filter.get_or_insert_with(Editor::default);
            }
            KeyCode::Esc => self.filter = None,
            KeyCode::Char('n') => self.open_draft(),
            KeyCode::Right => {
                if let Some(agent) = agent.filter(|agent| {
                    fleet.families().children(agent).next().is_some()
                        && !self.expanded.contains(&agent.agent)
                }) {
                    self.toggle(&agent);
                }
            }
            KeyCode::Left => {
                if let Some(agent) = agent
                    && !self.expanded.remove(&agent.agent)
                    && let Some(parent) = fleet.parent(&agent)
                {
                    let parent = ui_state::agent_key(parent);
                    self.expanded.remove(&parent.agent);
                    self.select(parent);
                }
            }
            KeyCode::Char(' ') | KeyCode::Char('z') => {
                if let Some(agent) =
                    agent.filter(|agent| fleet.families().children(agent).next().is_some())
                {
                    self.toggle(&agent);
                }
            }
            KeyCode::Char('h') => self.overlay = Some(Overlay::Hosts),
            // Detach: leave to the shell; the agents keep running.
            KeyCode::Char('d') => return vec![FleetEffect::Quit],
            KeyCode::Char('r') => {
                if let Some(agent) = agent
                    && let Some(entry) = fleet.agent(&agent)
                {
                    let mut editor = Editor::default();
                    editor.set(&entry.name, vec![]);
                    self.overlay = Some(Overlay::Rename { agent, editor });
                }
            }
            KeyCode::Char('x') => {
                if let Some(agent) = agent {
                    if fleet.stop_held(&agent) {
                        return vec![FleetEffect::CancelStop(agent)];
                    }
                    self.confirm_close(fleet, agent);
                }
            }
            _ => {}
        }
        vec![]
    }

    fn filter_key(&mut self, fleet: &FleetState, targets: &[Target], key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filter = None;
                self.filtering = false;
            }
            KeyCode::Enter => {
                self.filtering = false;
                if self.needle().is_none() {
                    self.filter = None;
                }
            }
            KeyCode::Up => self.step(targets, -1),
            KeyCode::Down => self.step(targets, 1),
            _ => {
                if let Some(filter) = &mut self.filter {
                    filter.key(key);
                }
                self.top = 0;
                // The highlight goes to the first agent the filter finds, so
                // Enter opens what was searched for.
                if let Some(found) = self
                    .targets_of(fleet)
                    .into_iter()
                    .find(|target| matches!(target, Target::Agent(_)))
                {
                    self.selected = Some(found);
                    self.reveal = true;
                }
            }
        }
    }

    fn overlay_key(&mut self, overlay: Overlay, key: KeyEvent) -> Vec<FleetEffect> {
        match overlay {
            Overlay::Hosts => {
                if !matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h')
                ) {
                    self.overlay = Some(Overlay::Hosts);
                }
                vec![]
            }
            Overlay::Rename { agent, mut editor } => match key.code {
                KeyCode::Esc => vec![],
                KeyCode::Enter => {
                    let name = editor.text().trim().to_owned();
                    if wire::agent_name_problem(&name).is_some() {
                        self.overlay = Some(Overlay::Rename { agent, editor });
                        return vec![];
                    }
                    vec![FleetEffect::Rename { agent, name }]
                }
                _ => {
                    editor.key(key);
                    self.overlay = Some(Overlay::Rename { agent, editor });
                    vec![]
                }
            },
            // Enter presses the focused button, never a letter: words
            // typed into a chat that closed under the person must not
            // answer it.
            Overlay::Confirm {
                agent,
                name,
                delete,
                focus,
            } => match key.code {
                KeyCode::Enter if focus == Confirmed::Yes && delete => {
                    vec![FleetEffect::Delete(agent)]
                }
                KeyCode::Enter if focus == Confirmed::Yes => vec![FleetEffect::Stop(agent)],
                KeyCode::Enter | KeyCode::Esc => vec![],
                code => {
                    let focus = match code {
                        KeyCode::Left => Confirmed::Yes,
                        KeyCode::Right => Confirmed::No,
                        KeyCode::Tab | KeyCode::BackTab if focus == Confirmed::Yes => Confirmed::No,
                        KeyCode::Tab | KeyCode::BackTab => Confirmed::Yes,
                        _ => focus,
                    };
                    self.overlay = Some(Overlay::Confirm {
                        agent,
                        name,
                        delete,
                        focus,
                    });
                    vec![]
                }
            },
        }
    }

    fn draft_key(&mut self, fleet: &FleetState, key: KeyEvent) -> Vec<FleetEffect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.draft.starting {
            if key.code == KeyCode::Esc {
                self.draft.open = false;
            }
            return vec![];
        }
        // An agent used in its own terminal: the form has the keys.
        if self
            .draft
            .setup
            .as_ref()
            .is_some_and(|setup| setup.chat_in == ChatInSetting::Native)
        {
            let (Some(setup), Some(form)) = (&mut self.draft.setup, &mut self.draft.form) else {
                return vec![];
            };
            return match form.key(setup, fleet, key) {
                FormOutcome::None => vec![],
                FormOutcome::Close => {
                    self.leave_draft();
                    vec![]
                }
                FormOutcome::Start => {
                    self.draft.starting = true;
                    self.draft.error = None;
                    vec![FleetEffect::Start {
                        setup: Box::new(setup.clone()),
                        text: String::new(),
                        attachments: Vec::new(),
                        open: true,
                    }]
                }
            };
        }
        if let Some(picker) = &mut self.draft.picker {
            match picker.key(key) {
                Pick::None => {}
                Pick::Close => self.draft.picker = None,
                Pick::Value(value) => {
                    let item = picker.item;
                    if let Some(setup) = &mut self.draft.setup {
                        setup.pick(item, &value);
                        setup.fill_folder(fleet);
                    }
                    self.draft.picker = None;
                }
            }
            return vec![];
        }
        // Ctrl+S, then a letter: the setting it names.
        if std::mem::take(&mut self.draft.prefix) {
            let item = match key.code {
                KeyCode::Char('n') => Some(Setting::Name),
                KeyCode::Char('m') => Some(Setting::Model),
                KeyCode::Char('e') => Some(Setting::Effort),
                KeyCode::Char('p') => Some(Setting::Permission),
                KeyCode::Char('d') => Some(Setting::Folder),
                KeyCode::Char('h') => Some(Setting::Host),
                KeyCode::Char('w') => Some(Setting::Worktree),
                KeyCode::Char('a') => Some(Setting::Kind),
                _ => None,
            };
            if let Some(item) = item {
                self.open_setting(fleet, item);
            }
            return vec![];
        }
        match key.code {
            KeyCode::Esc => self.leave_draft(),
            KeyCode::Char('s') if ctrl => self.draft.prefix = true,
            KeyCode::BackTab => {
                if let Some(setup) = &mut self.draft.setup {
                    setup.next_control();
                }
            }
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                if self.draft.editor.text().trim().is_empty() {
                    return vec![];
                }
                let Some(setup) = self.draft.setup.clone() else {
                    return vec![];
                };
                // A host with no folder known yet: one must be chosen.
                if setup.folder.is_empty() {
                    self.open_setting(fleet, Setting::Folder);
                    return vec![];
                }
                self.draft.starting = true;
                self.draft.error = None;
                return vec![FleetEffect::Start {
                    setup: Box::new(setup),
                    text: self.draft.editor.text().trim_end().to_owned(),
                    attachments: self.draft.editor.attachments().to_vec(),
                    open: !ctrl,
                }];
            }
            _ => {
                self.draft.editor.key(key);
            }
        }
        vec![]
    }

    /// A setting from the edge or Ctrl+S: its picker, or for the worktree,
    /// on or off at once.
    fn open_setting(&mut self, fleet: &FleetState, item: Setting) {
        let Some(setup) = &mut self.draft.setup else {
            return;
        };
        if item == Setting::Worktree {
            setup.pick(Setting::Worktree, "");
            return;
        }
        if item == Setting::Name {
            let name = setup.name.clone().unwrap_or_default();
            self.draft.picker = Some(Picker::text(Setting::Name, "Name", &name));
            return;
        }
        let title = match item {
            Setting::Name => "Name",
            Setting::Kind => "Agent",
            Setting::Model => "Model",
            Setting::Effort => "Effort",
            Setting::Permission => "Permission",
            Setting::Mode => "Mode",
            Setting::Folder => "Folder",
            Setting::Host => "Host",
            Setting::Worktree => "Worktree",
        };
        let choices = setup.choices(item, fleet);
        let typed = setup.takes_typed(item);
        self.draft.picker = Some(Picker::new(item, title, choices, typed));
    }

    pub fn mouse(&mut self, fleet: &FleetState, event: MouseEvent) -> Vec<FleetEffect> {
        let (x, y) = (event.column, event.row);
        let hit = |rows_only: bool| {
            self.spots
                .iter()
                .find(|spot| {
                    spot.y == y
                        && (spot.x.0..spot.x.1).contains(&x)
                        && (!rows_only || matches!(spot.hit, Hit::Row(_)))
                })
                .map(|spot| spot.hit.clone())
        };
        let listing = self.overlay.is_none() && !self.draft.open;
        // A modal asking before a stop or delete: its buttons act; the rest
        // of home under it does nothing.
        if let Some(Overlay::Confirm { agent, delete, .. }) = &self.overlay {
            let answer = match event.kind {
                MouseEventKind::Down(MouseButton::Left) => self
                    .overlay_spots
                    .iter()
                    .find(|(row, (from, to), _)| *row == y && (*from..*to).contains(&x))
                    .map(|(_, _, answer)| *answer),
                _ => None,
            };
            let effect = match answer {
                Some(Confirmed::Yes) if *delete => vec![FleetEffect::Delete(agent.clone())],
                Some(Confirmed::Yes) => vec![FleetEffect::Stop(agent.clone())],
                _ => vec![],
            };
            if answer.is_some() {
                self.overlay = None;
            }
            return effect;
        }
        if matches!(self.overlay, Some(Overlay::Hosts)) {
            return vec![];
        }
        // The new agent's modal has the pointer: its places act, the rest
        // of home under it does nothing.
        if self.draft.open && self.draft.form.is_some() {
            if let MouseEventKind::Down(MouseButton::Left) = event.kind
                && !self.draft.starting
                && let Some(hit) = self
                    .draft
                    .modal
                    .iter()
                    .find(|(row, (from, to), _)| *row == y && (*from..*to).contains(&x))
                    .map(|(_, _, hit)| hit.clone())
                && let (Some(setup), Some(form)) = (&mut self.draft.setup, &mut self.draft.form)
            {
                match form.click(setup, fleet, hit) {
                    FormOutcome::None => {}
                    FormOutcome::Close => self.leave_draft(),
                    FormOutcome::Start => {
                        self.draft.starting = true;
                        self.draft.error = None;
                        return vec![FleetEffect::Start {
                            setup: Box::new(setup.clone()),
                            text: String::new(),
                            attachments: Vec::new(),
                            open: true,
                        }];
                    }
                }
            }
            return vec![];
        }
        // A flyover is open: a click on a choice picks it, anywhere else
        // closes it.
        if self.draft.open
            && self.draft.picker.is_some()
            && let MouseEventKind::Down(MouseButton::Left) = event.kind
        {
            let pick = match self.draft.flyover.choice_at(x, y) {
                Some(at) => self.draft.picker.as_mut().map(|picker| picker.click(at)),
                None if self.draft.flyover.covers(x, y) => None,
                None => Some(Pick::Close),
            };
            match pick {
                Some(Pick::Value(value)) => {
                    let item = self.draft.picker.as_ref().map(|picker| picker.item);
                    if let (Some(setup), Some(item)) = (&mut self.draft.setup, item) {
                        setup.pick(item, &value);
                        setup.fill_folder(fleet);
                    }
                    self.draft.picker = None;
                }
                Some(Pick::Close) => self.draft.picker = None,
                _ => {}
            }
            return vec![];
        }
        match event.kind {
            MouseEventKind::Moved if self.draft.open => {
                self.draft.hover = match hit(false) {
                    Some(Hit::Setting(item)) => Some(item),
                    _ => None,
                };
                vec![]
            }
            MouseEventKind::Moved => {
                if listing && let Some(Hit::Row(target)) = hit(true) {
                    self.selected = Some(target);
                    self.reveal = false;
                }
                vec![]
            }
            MouseEventKind::ScrollUp if listing => {
                self.top = self
                    .top
                    .saturating_sub(crate::wheel::lines(crate::wheel::Direction::Up));
                self.reveal = false;
                vec![]
            }
            MouseEventKind::ScrollDown if listing => {
                self.top += crate::wheel::lines(crate::wheel::Direction::Down);
                self.reveal = false;
                vec![]
            }
            MouseEventKind::Down(MouseButton::Left) => match hit(false) {
                Some(Hit::Key(key)) => self.key(fleet, key),
                Some(Hit::Setting(item)) if !self.draft.starting => {
                    self.draft.prefix = false;
                    self.open_setting(fleet, item);
                    vec![]
                }
                Some(Hit::Close(agent)) if listing && fleet.stop_held(&agent) => {
                    vec![FleetEffect::CancelStop(agent)]
                }
                Some(Hit::Close(agent)) if listing => {
                    self.confirm_close(fleet, agent);
                    vec![]
                }
                Some(Hit::Fold(agent)) if listing => {
                    self.toggle(&agent);
                    vec![]
                }
                Some(Hit::Row(target)) if listing => {
                    self.selected = Some(target.clone());
                    self.activate(target)
                }
                _ => vec![],
            },
            _ => vec![],
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        paint: &mut Paint<'_>,
        area: Rect,
        fleet: &FleetState,
        lines: &HashMap<AgentKey, SessionLine>,
        footer: Option<Line<'static>>,
        now_ms: i64,
        theme: Theme,
        place: &Place<'_>,
    ) {
        let width = usize::from(area.width);
        let height = usize::from(area.height);
        if height < 3 || width < 2 * MARGIN + 8 {
            return;
        }
        // A blank line above the top line keeps it off the terminal's edge.
        let mut laid: Vec<Laid> = vec![Laid::default()];
        let mut cursor;
        let mut section_folded = false;
        // Where the new agent's composer box starts, for its flyover.
        let mut box_top = None;
        self.open_setup(place);
        // An agent used in its own terminal starts from a modal over home;
        // one chatted with in amux, from its own composer.
        let modal = self.draft.open
            && self
                .draft
                .setup
                .as_ref()
                .is_some_and(|setup| setup.chat_in == ChatInSetting::Native);
        if modal && self.draft.form.is_none() {
            self.draft.form = self.draft.setup.as_ref().map(Form::new);
        }
        if self.draft.open && !modal {
            laid.push(Laid::plain(self.draft_top(width, theme)));
            let (composer, at) = self.composer(fleet, place, width, height, theme);
            // As in a chat: a blank, the top line and a blank above; the
            // box, a blank to let it breathe and the keys below.
            let body = height.saturating_sub(4 + composer.len());
            laid.resize_with(2 + body, Laid::default);
            // A failed start says why just above the box, a blank between.
            if let Some(error) = &self.draft.error {
                let words = format!("Could not start the agent: {error}");
                let wrapped = text::wrap(&words, width - 2 * MARGIN);
                let from = laid.len().saturating_sub(wrapped.len() + 1).max(2);
                for (at, words) in (from..laid.len()).zip(wrapped) {
                    let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
                    push(&mut line, words, theme.warning(), width);
                    laid[at] = Laid::plain(line);
                }
            }
            box_top = Some(laid.len());
            cursor = at
                .filter(|_| self.draft.picker.is_none())
                .map(|(col, row)| (col, laid.len() + row));
            laid.extend(composer);
        } else {
            let view = self.view(fleet, place.local_host, lines);
            let (top, at) = self.top_line(fleet, &view, width, theme, place);
            laid.push(Laid::plain(top));
            cursor = at.map(|col| (col, 1));
            laid.push(Laid::default());
            // A blank, the top line and a blank above the list; a blank and
            // the hints below it.
            let room = height.saturating_sub(5);
            let list_top = laid.len();
            let items = self.items(fleet, view);
            section_folded = items.iter().any(|item| {
                matches!(item, Item::Heading(section, _, true)
                    if self.selected == Some(Target::Section(*section)))
            });
            laid.extend(self.list(&items, now_ms, width, room, area.height, theme, place));
            // Renaming: the name is a field on its own row.
            if let Some((col, row)) = self.rename_at {
                cursor = Some((col, list_top + row));
            }
        }
        laid.resize_with(height - 1, Laid::default);
        let (hint, at) = self.hint_line(footer, width, theme, section_folded);
        if let Some(col) = at {
            cursor = Some((col, height - 1));
        }
        laid.push(hint);

        self.spots.clear();
        let mut lines = Vec::with_capacity(height);
        for (row, Laid { line, spots }) in laid.into_iter().enumerate() {
            let y = area.y + row as u16;
            // Specific places first, so a click finds them before the row.
            let (whole, specific): (Vec<_>, Vec<_>) =
                spots.into_iter().partition(|(x, _)| x.is_none());
            for (x, hit) in specific.into_iter().chain(whole) {
                let (from, to) = x.unwrap_or((0, width));
                self.spots.push(Spot {
                    y,
                    x: (area.x + from as u16, area.x + to.min(width) as u16),
                    hit,
                });
            }
            lines.push(line);
        }
        self.spots
            .sort_by_key(|spot| matches!(spot.hit, Hit::Row(_)));
        paint.render_widget(Paragraph::new(lines), area);
        if let Some((x, y)) = cursor {
            paint.set_cursor_position(Position::new(
                area.x + x.min(width - 1) as u16,
                area.y + y as u16,
            ));
        }
        // A setting being chosen: its flyover rises from the setting's place
        // on the edge, over the box. Effort lives in the model's words.
        if let (Some(picker), Some(top)) = (&self.draft.picker, box_top) {
            let item = match picker.item {
                Setting::Effort => Setting::Model,
                item => item,
            };
            let anchor = self
                .spots
                .iter()
                .find(|spot| matches!(&spot.hit, Hit::Setting(at) if *at == item))
                .map_or(area.x + MARGIN as u16, |spot| spot.x.0);
            let above = area.y + top as u16;
            self.draft.flyover =
                crate::setup::draw_flyover(paint, picker, anchor, above, area, theme);
            if let Some(at) = self.draft.flyover.cursor {
                paint.set_cursor_position(at);
            }
        }
        // Hosts, and asking before a stop or delete: modals over home.
        self.overlay_spots.clear();
        match &self.overlay {
            Some(Overlay::Hosts) => {
                let rows = hosts::modal_rows(fleet, place.local_host, theme);
                let inner = crate::panel::content_width(&rows)
                    .max(40)
                    .min(width.saturating_sub(8));
                let lines = crate::panel::bordered("Hosts", rows, inner, &[], None, theme);
                crate::panel::centre(paint, lines, area);
            }
            Some(Overlay::Confirm {
                agent,
                name,
                delete,
                focus,
            }) => {
                let (sentence, verb) = if *delete {
                    (format!("Delete {name} and its history?"), "Delete")
                } else {
                    (stop_sentence(fleet, agent, name), "Stop")
                };
                let (buttons, spots) = crate::panel::buttons(
                    &[(verb, Confirmed::Yes), ("Cancel", Confirmed::No)],
                    usize::from(*focus == Confirmed::No),
                    theme,
                );
                let legend = crate::panel::legend(
                    &[("←→", "choose"), ("enter", "press"), ("esc", "cancel")],
                    theme,
                );
                // The sentence wraps rather than widen the window past the
                // screen.
                let inner = text::str_width(&sentence)
                    .max(crate::panel::content_width(std::slice::from_ref(&legend)))
                    .clamp(36, 48)
                    .min(width.saturating_sub(8));
                let said = text::wrap(&sentence, inner);
                let mut rows = vec![Line::default()];
                rows.extend(
                    said.iter()
                        .map(|words| Line::from(Span::styled(words.clone(), theme.text()))),
                );
                rows.extend([Line::default(), buttons, legend]);
                let title = if *delete { "Delete" } else { "Stop" };
                let lines = crate::panel::bordered(title, rows, inner, &[], None, theme);
                let rect = crate::panel::centre(paint, lines, area);
                // The buttons' row: the border, a blank, the sentence's
                // lines, a blank, then the buttons; two columns of border
                // and padding.
                let row = rect.y + 3 + said.len() as u16;
                self.overlay_spots = spots
                    .into_iter()
                    .map(|(from, to, hit)| {
                        (row, (rect.x + 2 + from as u16, rect.x + 2 + to as u16), hit)
                    })
                    .collect();
            }
            _ => {}
        }
        // The new agent's modal, centred over home.
        self.draft.modal.clear();
        if modal && let (Some(setup), Some(form)) = (&self.draft.setup, &self.draft.form) {
            let error = self.draft.error.as_deref();
            let (lines, hits, at) = form.modal(setup, fleet, error, width, theme);
            let w = lines.iter().map(text::line_width).max().unwrap_or(0) as u16;
            let h = (lines.len() as u16).min(area.height);
            let x = area.x + area.width.saturating_sub(w) / 2;
            // A little above the middle, where the eye already is.
            let y = area.y + area.height.saturating_sub(h) / 3;
            let rect = Rect {
                x,
                y,
                width: w.min(area.width),
                height: h,
            };
            paint.render_widget(ratatui::widgets::Clear, rect);
            paint.render_widget(Paragraph::new(lines), rect);
            self.draft.modal = hits
                .into_iter()
                .map(|(line, from, to, hit)| {
                    (y + line as u16, (x + from as u16, x + to as u16), hit)
                })
                .collect();
            if let Some((col, line)) = at.filter(|_| !self.draft.starting) {
                paint.set_cursor_position(Position::new(x + col as u16, y + line as u16));
            }
        }
    }

    /// Where you are and what needs you; the filter while one is typed or
    /// kept. Returns the cursor's column while the filter has the keys.
    fn top_line(
        &self,
        fleet: &FleetState,
        view: &FleetView,
        width: usize,
        theme: Theme,
        place: &Place<'_>,
    ) -> (Line<'static>, Option<usize>) {
        let local_host = place.local_host;
        let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
        if let Some(filter) = self
            .filter
            .as_ref()
            .filter(|_| self.filtering || self.needle().is_some())
        {
            push(&mut line, "/ ", theme.attention(), width);
            let at = text::line_width(&line);
            push(&mut line, filter.text(), theme.text(), width);
            let cursor = self.filtering.then(|| at + filter.cursor_chars());
            let right = match self.needle() {
                Some(needle) => {
                    let shown = fleet
                        .agents()
                        .filter(|agent| ui_view::matches(fleet, agent, &needle))
                        .count();
                    format!("{shown} match{}", if shown == 1 { "" } else { "es" })
                }
                None => {
                    push(&mut line, "name, project or host", theme.muted(), width);
                    String::new()
                }
            };
            place_right(
                &mut line,
                Line::from(Span::styled(right, theme.muted())),
                width,
            );
            return (line, cursor);
        }
        // Home is the whole fleet, across hosts and directories, so the top
        // line names no directory: where a new agent starts belongs to
        // starting one.
        push(&mut line, "amux", theme.emphasis(), width);
        let (need, working) = (view.need_you, view.working);
        // Every trusted host this machine does not reach now, whatever the
        // reason.
        let reaches: Vec<(&str, Reach)> = fleet
            .hosts()
            .filter(|host| host.trusted().is_some() && host.host_id() != local_host)
            .map(|host| {
                (
                    host.name(),
                    ui_view::reach(fleet, local_host, host.host_id()),
                )
            })
            .filter(|(_, reach)| !reach.online())
            .collect();
        // A host that no longer trusts this machine is not merely away:
        // only pairing again brings it back.
        let (revoked, away): (Vec<_>, Vec<_>) = reaches
            .iter()
            .partition(|(_, reach)| *reach == Reach::Offline(Offline::Revoked));
        let away: Vec<&str> = away.iter().map(|(name, _)| *name).collect();
        let mut parts: Vec<(String, Style)> = Vec::new();
        // A daemon that restarted into a newer build keeps serving this
        // older client; only the person can restart it.
        if let Some(running) = fleet
            .host(local_host)
            .and_then(wire::HostEntry::version)
            .filter(|running| !place.version.is_empty() && *running != place.version)
        {
            parts.push((
                format!("amux {running} running · restart to update"),
                theme.warning(),
            ));
        }
        // Signed out, hosts out of reach can only be reached again by
        // signing in.
        if reaches
            .iter()
            .any(|(_, reach)| *reach == Reach::Offline(Offline::SignedOut))
        {
            parts.push(("signed out · amux login".into(), theme.warning()));
        }
        match revoked.as_slice() {
            [] => {}
            [(one, _)] => parts.push((
                format!(
                    "{one} no longer trusts this machine · amux pair {}",
                    crate::hosts::shell_target(one)
                ),
                theme.warning(),
            )),
            many => parts.push((
                format!("{} hosts no longer trust this machine", many.len()),
                theme.warning(),
            )),
        }
        match fleet.connection() {
            Connection::Live => {}
            Connection::Connecting => parts.push(("connecting".into(), theme.muted())),
            Connection::Reconnecting => {
                parts.push(("reconnecting to amux".into(), theme.warning()));
            }
        }
        match away.as_slice() {
            [] => {}
            [one] => parts.push((format!("{one} away"), theme.muted())),
            many => parts.push((format!("{} hosts away", many.len()), theme.muted())),
        }
        if working > 0 {
            parts.push((format!("{working} working"), theme.muted()));
        }
        if need > 0 {
            parts.push((
                format!("{need} need{} you", if need == 1 { "s" } else { "" }),
                theme.attention(),
            ));
        }
        let mut right = Line::default();
        for (i, (words, style)) in parts.into_iter().enumerate() {
            if i > 0 {
                right.spans.push(Span::styled(" · ", theme.muted()));
            }
            right.spans.push(Span::styled(words, style));
        }
        place_right(&mut line, right, width);
        (line, None)
    }

    fn draft_top(&self, width: usize, theme: Theme) -> Line<'static> {
        let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
        push(&mut line, "amux", theme.emphasis(), width);
        push(&mut line, "  new agent", theme.muted(), width);
        line
    }

    /// The list's lines for `room` rows, scrolled to keep the highlight in
    /// view after a key moved it.
    #[allow(clippy::too_many_arguments)]
    fn list(
        &mut self,
        items: &[Item],
        now_ms: i64,
        width: usize,
        room: usize,
        height: u16,
        theme: Theme,
        place: &Place<'_>,
    ) -> Vec<Laid> {
        let targets = Self::targets(items);
        let selected = self.settle(&targets);
        let roomy = height >= ROOMY;
        let mut laid: Vec<Laid> = Vec::new();
        // The lines the highlight covers, and the lines scrolling keeps in
        // view with it: a highlighted heading brings its section along, so
        // opening one shows what it holds.
        let mut highlight = (0, 0);
        let mut in_view = (0, 0);
        let mut section_open = false;
        let mut rename_line = None;
        for item in items {
            let start = laid.len();
            match item {
                Item::New => {
                    // A control, not a card: highlighted, its words brighten
                    // rather than the row filling in, and only the words
                    // answer the mouse.
                    let chosen = selected == Target::New;
                    let (mark, words) = if chosen {
                        (theme.emphasis(), theme.emphasis())
                    } else {
                        (theme.faint(), theme.muted())
                    };
                    // Laid out like a heading: the `+` on the screen's left
                    // edge with the top line and the headings' arrows, the
                    // words on the column the headings' labels use.
                    let mut line = control_lead(chosen, theme);
                    pad_to(&mut line, HEAD_COL);
                    push(&mut line, "+", mark, width);
                    pad_to(&mut line, MARK_COL);
                    push(&mut line, "New Agent", words, width);
                    let to = text::line_width(&line);
                    laid.push(Laid {
                        line: tint(line, chosen, width, theme),
                        spots: vec![(Some((HEAD_COL, to)), Hit::Row(Target::New))],
                    });
                }
                Item::Heading(section, count, folded) => {
                    // A fold marker, the label, its count, and a faint rule
                    // to the right edge, so a section's end is visible at a
                    // glance. The label lines up with the rows' marks.
                    // Like "+ New Agent", a control: highlighted, the marker
                    // and label brighten, and only they answer the mouse.
                    let target = Target::Section(*section);
                    let chosen = selected == target;
                    let mut line = control_lead(chosen, theme);
                    pad_to(&mut line, HEAD_COL);
                    let marker = if *folded { "▸" } else { "▾" };
                    let (marker_style, label) = match (chosen, section) {
                        (true, _) => (theme.emphasis(), theme.emphasis()),
                        (false, SectionKind::NeedsYou) => (
                            theme.faint(),
                            theme.attention().add_modifier(Modifier::BOLD),
                        ),
                        (false, _) => (theme.faint(), theme.muted().add_modifier(Modifier::BOLD)),
                    };
                    push(&mut line, marker, marker_style, width);
                    pad_to(&mut line, MARK_COL);
                    push(&mut line, section_words(*section), label, width);
                    let to = text::line_width(&line);
                    push(&mut line, format!(" {count} "), theme.faint(), width);
                    let end = width.saturating_sub(MARGIN);
                    let rule = end.saturating_sub(text::line_width(&line));
                    push(&mut line, "─".repeat(rule), theme.hairline(), width);
                    laid.push(Laid {
                        line: tint(line, chosen, width, theme),
                        spots: vec![(Some((HEAD_COL, to)), Hit::Row(target))],
                    });
                }
                Item::Gap => blank(&mut laid),
                Item::Space => laid.push(Laid::default()),
                Item::Note(words) => {
                    let mut line = Line::default();
                    pad_to(&mut line, NAME_COL);
                    push(&mut line, *words, theme.muted(), width);
                    laid.push(Laid::plain(line));
                }
                Item::Agent(row) => {
                    let chosen = selected == Target::Agent(row.card.agent.clone());
                    let renaming = match &self.overlay {
                        Some(Overlay::Rename { agent, editor }) if *agent == row.card.agent => {
                            Some(editor)
                        }
                        _ => None,
                    };
                    if let Some(editor) = renaming {
                        rename_line = Some((
                            laid.len(),
                            NAME_COL + 2 * row.depth as usize + editor.cursor_chars(),
                        ));
                    }
                    laid.extend(agent_lines(
                        row, chosen, renaming, now_ms, width, theme, place,
                    ));
                    if roomy {
                        blank(&mut laid);
                    }
                }
            }
            let is_selected = match item {
                Item::New => selected == Target::New,
                Item::Heading(section, ..) => selected == Target::Section(*section),
                Item::Agent(row) => selected == Target::Agent(row.card.agent.clone()),
                _ => false,
            };
            if is_selected {
                highlight = (start, laid.len());
                in_view = highlight;
            }
            if matches!(item, Item::Agent(_)) && section_open {
                in_view.1 = laid.len();
            }
            if let Item::Heading(section, ..) = item {
                section_open = selected == Target::Section(*section);
            }
        }
        while laid
            .last()
            .is_some_and(|laid| laid.spots.is_empty() && laid.line.spans.is_empty())
        {
            laid.pop();
        }
        pad_highlight(&mut laid, highlight, width, theme);
        let highlight = in_view;
        if self.reveal {
            if highlight.0 < self.top {
                self.top = highlight.0;
            } else if highlight.1 > self.top + room {
                // Never scroll the highlight's own first line out of view.
                self.top = (highlight.1 - room).min(highlight.0);
            }
            // The first rows sit under "+ New Agent" and a heading: keep
            // those in view with them.
            if targets.iter().position(|t| *t == selected).unwrap_or(0) <= 1 {
                self.top = 0;
            }
            self.reveal = false;
        }
        self.top = self.top.min(laid.len().saturating_sub(room));
        self.rename_at = rename_line
            .filter(|(line, _)| (self.top..self.top + room).contains(line))
            .map(|(line, col)| (col, line - self.top));
        let mut shown: Vec<Laid> = laid.into_iter().skip(self.top).take(room).collect();
        // A card's padding shows only with its card.
        if let Some(first) = shown.first_mut()
            && text::is_padding(&first.line, '▀')
        {
            *first = Laid::default();
        }
        if let Some(last) = shown.last_mut()
            && text::is_padding(&last.line, '▄')
        {
            *last = Laid::default();
        }
        shown
    }

    /// The composer of a new agent's chat, boxed, with what the agent will
    /// be on its bottom edge. Returns the lines and the cursor as (column,
    /// line).
    fn composer(
        &self,
        fleet: &FleetState,
        place: &Place<'_>,
        width: usize,
        height: usize,
        theme: Theme,
    ) -> (Vec<Laid>, Option<(usize, usize)>) {
        let inner = width - 2 * MARGIN - 4;
        let (mut body, (row, col)) = editor_lines(
            &self.draft.editor,
            "What should the new agent work on?",
            inner,
            theme,
        );
        // The box already says where to type, so the composer's own edge
        // mark becomes a prompt.
        for (i, line) in body.iter_mut().enumerate() {
            if let Some(first) = line.spans.first_mut() {
                *first = Span::styled(if i == 0 { "› " } else { "  " }, theme.muted());
            }
        }
        let most = (height / COMPOSER_SHARE).saturating_sub(2).max(1);
        let skip = body.len().saturating_sub(most).min(row);
        let edge = theme.muted();
        let mut out = Vec::new();
        let mut top = Line::from(Span::raw(" ".repeat(MARGIN)));
        push(&mut top, "╭", edge, width);
        push(&mut top, "─".repeat(width - 2 * MARGIN - 2), edge, width);
        push(&mut top, "╮", edge, width);
        out.push(Laid::plain(top));
        for line in body.into_iter().skip(skip).take(most) {
            let mut boxed = Line::from(Span::raw(" ".repeat(MARGIN)));
            push(&mut boxed, "│ ", edge, width);
            boxed.spans.extend(line.spans);
            pad_to(&mut boxed, width - MARGIN - 1);
            push(&mut boxed, "│", edge, width);
            out.push(Laid::plain(boxed));
        }
        // What the agent will be, on the bottom edge: what runs it, where it
        // works, and on which machine. Each item is a click target, lit
        // as a chip under the pointer.
        let setup = self.draft.setup.clone().unwrap_or_else(|| {
            Setup::defaults(
                place.chat_in,
                place.working_dir,
                place.local_host,
                place.defaults,
            )
        });
        let mut groups = setup.edge(fleet);
        let room = (width - 2 * MARGIN - 2).saturating_sub(6);
        let measure = |groups: &Vec<Vec<(Setting, String)>>| {
            groups
                .iter()
                .map(|group| {
                    group.iter().map(|(_, w)| text::str_width(w)).sum::<usize>()
                        + 3 * group.len().saturating_sub(1)
                })
                .sum::<usize>()
                + 3 * groups.len().saturating_sub(1)
        };
        // Too long for the edge: the folder shortens in the middle first,
        // then the name at its end, since the model and the permission are
        // what the person is about to start the agent with. Still too long,
        // the mode drops, then the model, then the permission (ctrl+s
        // still reaches them), but never what Shift+Tab steps: a key whose
        // effect nothing shows is a setting changed blind.
        for shortened in [Setting::Folder, Setting::Name] {
            let over = measure(&groups).saturating_sub(room);
            if over == 0 {
                break;
            }
            for group in &mut groups {
                for (item, words) in group.iter_mut() {
                    if *item == shortened {
                        let keep = text::str_width(words).saturating_sub(over).max(12);
                        *words = if shortened == Setting::Folder {
                            text::ellipsize_middle(words, keep)
                        } else {
                            text::ellipsize(words, keep)
                        };
                    }
                }
            }
        }
        let stepped = if setup.cycles() == "mode" {
            Setting::Mode
        } else {
            Setting::Permission
        };
        for dropped in [Setting::Mode, Setting::Model, Setting::Permission] {
            if dropped != stepped && measure(&groups) > room {
                for group in &mut groups {
                    group.retain(|(item, _)| *item != dropped);
                }
                groups.retain(|group| !group.is_empty());
            }
        }
        let label = measure(&groups);
        let rule = (width - 2 * MARGIN - 2).saturating_sub(label + 3);
        let mut bottom = Line::from(Span::raw(" ".repeat(MARGIN)));
        push(&mut bottom, "╰", edge, width);
        push(&mut bottom, "─".repeat(rule.max(1)), edge, width);
        push(&mut bottom, " ", edge, width);
        let mut spots = Vec::new();
        for (g, group) in groups.into_iter().enumerate() {
            if g > 0 {
                push(&mut bottom, " │ ", edge, width);
            }
            for (i, (item, words)) in group.into_iter().enumerate() {
                if i > 0 {
                    push(&mut bottom, " · ", edge, width);
                }
                let from = text::line_width(&bottom);
                let ink = if self.draft.hover == Some(item) {
                    theme.chip()
                } else {
                    theme.text()
                };
                push(&mut bottom, words, ink, width);
                spots.push((Some((from, text::line_width(&bottom))), Hit::Setting(item)));
            }
        }
        push(&mut bottom, " ─", edge, width);
        pad_to(&mut bottom, width - MARGIN - 1);
        push(&mut bottom, "╯", edge, width);
        out.push(Laid {
            line: bottom,
            spots,
        });
        let cursor = (!self.draft.starting).then_some((MARGIN + 2 + col, 1 + row - skip));
        (out, cursor)
    }

    /// The bottom line: an open question or a notice when there is one,
    /// else the keys for what is highlighted. Returns the cursor's column
    /// while the rename field has the keys.
    /// `section_folded`: the highlighted section heading's section is
    /// folded.
    fn hint_line(
        &self,
        footer: Option<Line<'static>>,
        width: usize,
        theme: Theme,
        section_folded: bool,
    ) -> (Laid, Option<usize>) {
        let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
        let key = |code| Hit::Key(plain_key(code));
        let hints: Vec<(&str, &str, Hit)> = match &self.overlay {
            // Renaming in place: only its keys, or why the name will not do.
            Some(Overlay::Rename { editor, .. }) => {
                if let Some(problem) = wire::agent_name_problem(editor.text().trim()) {
                    // The way out stays on the line; the reason gives way,
                    // a whole word at a time.
                    let room = width.saturating_sub(MARGIN + text::str_width(" · esc cancel"));
                    push(
                        &mut line,
                        text::ellipsize_words(&crate::words::name_problem(problem), room),
                        theme.warning(),
                        width,
                    );
                    push(&mut line, " · ", theme.muted(), width);
                    vec![("esc", "cancel", key(KeyCode::Esc))]
                } else {
                    vec![
                        ("enter", "save", key(KeyCode::Enter)),
                        ("esc", "cancel", key(KeyCode::Esc)),
                    ]
                }
            }
            // A modal carries its own keys.
            Some(Overlay::Confirm { .. }) => return (Laid::plain(line), None),
            Some(Overlay::Hosts) => return (Laid::plain(line), None),
            None if footer.is_some() => {
                // The app's words start with their own margin.
                let footer = footer.unwrap_or_default();
                let mut spans = footer.spans.into_iter().peekable();
                while spans
                    .peek()
                    .is_some_and(|span| span.content.trim().is_empty())
                {
                    spans.next();
                }
                line.spans.extend(spans);
                return (Laid::plain(line), None);
            }
            None if self.draft.open && self.draft.starting => {
                push(&mut line, "starting the agent…", theme.muted(), width);
                return (Laid::plain(line), None);
            }
            // The modal carries its own keys.
            None if self.draft.open && self.draft.form.is_some() => {
                return (Laid::plain(line), None);
            }
            None if self.draft.open => match (&self.draft.picker, self.draft.prefix) {
                (Some(picker), _) => picker
                    .hints()
                    .into_iter()
                    .map(|(keys, action)| {
                        let code = match keys {
                            "enter" => KeyCode::Enter,
                            "esc" => KeyCode::Esc,
                            "tab" => KeyCode::Tab,
                            _ => KeyCode::Down,
                        };
                        (keys, action, key(code))
                    })
                    .collect(),
                // In the edge's order; the worktree, not on the edge until
                // it is on, gives way first.
                (None, true) => [
                    ("n", "name", key(KeyCode::Char('n'))),
                    ("a", "agent", key(KeyCode::Char('a'))),
                    ("m", "model", key(KeyCode::Char('m'))),
                    ("e", "effort", key(KeyCode::Char('e'))),
                    ("p", "permission", key(KeyCode::Char('p'))),
                    ("d", "folder", key(KeyCode::Char('d'))),
                    ("h", "host", key(KeyCode::Char('h'))),
                    ("w", "worktree", key(KeyCode::Char('w'))),
                    ("esc", "back", key(KeyCode::Esc)),
                ]
                .into(),
                (None, false) => vec![
                    ("enter", "start", key(KeyCode::Enter)),
                    (
                        "ctrl+s",
                        "settings",
                        Hit::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
                    ),
                    (
                        "shift+tab",
                        self.draft
                            .setup
                            .as_ref()
                            .map_or("permission", |setup| setup.cycles()),
                        key(KeyCode::BackTab),
                    ),
                    ("esc", "back", key(KeyCode::Esc)),
                ],
            },
            None if self.filtering => vec![
                ("enter", "keep", key(KeyCode::Enter)),
                ("esc", "clear", key(KeyCode::Esc)),
                ("↑↓", "move", key(KeyCode::Down)),
            ],
            None => {
                let mut hints = match &self.selected {
                    Some(Target::New) => vec![("enter", "start", key(KeyCode::Enter))],
                    Some(Target::Section(_)) if section_folded => {
                        vec![("enter", "show", key(KeyCode::Enter))]
                    }
                    Some(Target::Section(_)) => vec![("enter", "hide", key(KeyCode::Enter))],
                    _ => vec![("enter", "open", key(KeyCode::Enter))],
                };
                if matches!(self.selected, Some(Target::Agent(_))) {
                    hints.push(("a", "attach", key(KeyCode::Char('a'))));
                }
                if self.needle().is_some() {
                    hints.push(("esc", "clear filter", key(KeyCode::Esc)));
                } else {
                    hints.push(("/", "filter", key(KeyCode::Char('/'))));
                }
                if !matches!(self.selected, Some(Target::New)) {
                    hints.push(("n", "new", key(KeyCode::Char('n'))));
                }
                hints.push(("?", "keys", key(KeyCode::Char('?'))));
                hints
            }
        };
        let mut spots = Vec::new();
        let start = text::line_width(&line);
        append_hints(&mut line, &mut spots, start, &hints, width, theme);
        (Laid { line, spots }, None)
    }
}

/// What the stop window asks: what a stop cuts when the agent is working,
/// has background jobs or has subagents ("desk-1 is working and has 2
/// background jobs and 1 subagent. Stop anyway?"), else the plain
/// question. Subagents are the live descendants the agent's host counts,
/// or those this fleet sees when it sees more.
fn stop_sentence(fleet: &FleetState, agent: &AgentKey, name: &str) -> String {
    let Some(entry) = fleet.agent(agent) else {
        return format!("Stop {name}? It can be resumed later.");
    };
    let working = entry.phase() == wire::Phase::Working;
    let jobs = entry.background_jobs as usize;
    let seen = live_descendants(fleet, agent);
    let subs = (entry.descendants as usize).max(seen);
    let count = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    let mut has = Vec::new();
    if jobs > 0 {
        has.push(count(jobs, "background job"));
    }
    if subs > 0 {
        has.push(count(subs, "subagent"));
    }
    let has = has.join(" and ");
    match (working, has.is_empty()) {
        (false, true) => format!("Stop {name}? It can be resumed later."),
        (true, true) => format!("{name} is working. Stop anyway?"),
        (true, false) => format!("{name} is working and has {has}. Stop anyway?"),
        (false, false) => format!("{name} has {has}. Stop anyway?"),
    }
}

/// The live agents under `agent` in this fleet, at any depth.
fn live_descendants(fleet: &FleetState, agent: &AgentKey) -> usize {
    let mut count = 0;
    let mut under: Vec<&AgentKey> = fleet.families().children(agent).collect();
    while let Some(child) = under.pop() {
        if fleet
            .agent(child)
            .is_some_and(|child| child.lifecycle() == Lifecycle::Live)
        {
            count += 1;
        }
        under.extend(fleet.families().children(child));
    }
    count
}

/// Home's part of the key help.
pub fn help_rows() -> Vec<(&'static str, String)> {
    vec![
        ("Home", String::new()),
        (
            "j / k, ↑ / ↓",
            "move; the mouse highlights what it is over".into(),
        ),
        ("enter, click", "open the chat".into()),
        (
            "a, ctrl+enter",
            "attach to the agent's own terminal (this machine)".into(),
        ),
        (
            "/",
            "filter by name, agent, folder or host; esc clears".into(),
        ),
        (
            "n",
            "new agent: its first prompt, then enter (or its form, for its own terminal)".into(),
        ),
        (
            "ctrl+s then a letter",
            "a new agent's name, model, effort, folder, host, agent".into(),
        ),
        ("→ / ←, space", "show or hide a family's agents".into()),
        ("r", "rename, in place".into()),
        (
            "x",
            "stop (it can be resumed), or delete once exited; it asks first".into(),
        ),
        ("h", "hosts".into()),
        (
            "b",
            "report a problem: mark the screen, write a note".into(),
        ),
        (
            "d, q",
            "detach: leave to the shell; agents keep running".into(),
        ),
    ]
}

/// What home needs from the app that is not the fleet's.
pub struct Place<'a> {
    pub local_host: &'a [u8],
    /// This build's version, compared with the daemon's.
    pub version: &'a str,
    /// Where a new agent works, as the person would write it.
    pub working_dir: &'a str,
    /// Where the person chats, which decides how a new agent starts.
    pub chat_in: ChatInSetting,
    /// What each agent starts with.
    pub defaults: &'a settings::NewAgentSettings,
}

/// `key word` pairs, laid out by [`text::fit_hints`]; each is a click
/// target for its key. `? keys` is last and the first to go.
fn append_hints(
    line: &mut Line<'static>,
    spots: &mut Vec<(Option<(usize, usize)>, Hit)>,
    start: usize,
    hints: &[(&str, &str, Hit)],
    width: usize,
    theme: Theme,
) {
    let pairs: Vec<(&str, &str)> = hints.iter().map(|(key, word, _)| (*key, *word)).collect();
    let fitted = text::fit_hints(&pairs, width.saturating_sub(MARGIN + start));
    let mut at = start;
    for (i, &index) in fitted.shown.iter().enumerate() {
        let (key, word, hit) = &hints[index];
        let size = text::str_width(key) + 1 + text::str_width(word);
        if fitted.mark == Some(i) {
            line.spans.push(Span::styled(" …", theme.faint()));
            at += 2;
        }
        let gap = if i == 0 { 0 } else { fitted.gap };
        if at + gap + size > width - MARGIN {
            break;
        }
        line.spans.push(Span::raw(" ".repeat(gap)));
        let from = at + gap;
        // The key is what you press, so it reads first; its action recedes.
        line.spans
            .push(Span::styled(key.to_string(), theme.emphasis()));
        line.spans
            .push(Span::styled(format!(" {word}"), theme.faint()));
        at = from + size;
        spots.push((Some((from, at)), hit.clone()));
    }
    if fitted.mark == Some(fitted.shown.len()) {
        line.spans.push(Span::styled(" …", theme.faint()));
    }
}

/// Half a line of the highlight's tint above and below it, drawn with half
/// blocks into the blank lines around the row, so the highlighted row reads
/// as a card with room around its words. Where rows are packed with no
/// blank line between them, the highlight stays flat.
fn pad_highlight(laid: &mut Vec<Laid>, (start, end): (usize, usize), width: usize, theme: Theme) {
    let Some(surface) = theme.row_surface().and_then(|style| style.bg) else {
        return;
    };
    if end <= start {
        return;
    }
    let is_blank = |laid: &Laid| laid.spots.is_empty() && laid.line.spans.is_empty();
    // The range may take in the blank line laid after a row.
    let mut end = end;
    while end > start && laid.get(end - 1).is_some_and(is_blank) {
        end -= 1;
    }
    // The tint's own extent: the outer margin on each side.
    let edge = |glyph: &str| {
        Line::from(vec![
            Span::raw(" ".repeat(MARGIN)),
            Span::styled(
                glyph.repeat(width.saturating_sub(2 * MARGIN)),
                Style::default().fg(surface),
            ),
        ])
    };
    if let Some(above) = start.checked_sub(1).and_then(|at| laid.get_mut(at))
        && is_blank(above)
    {
        above.line = edge("▄");
    }
    match laid.get_mut(end) {
        Some(below) if is_blank(below) => below.line = edge("▀"),
        // The list's last row: its lower edge is a line of its own.
        None => laid.push(Laid::plain(edge("▀"))),
        Some(_) => {}
    }
}

/// One blank line, unless the list already ends in one.
fn blank(laid: &mut Vec<Laid>) {
    let ends_blank = laid
        .last()
        .is_some_and(|last| last.spots.is_empty() && last.line.spans.is_empty());
    if !ends_blank {
        laid.push(Laid::default());
    }
}

/// Places `right` against the right margin, or drops it when it would
/// touch what is already on the line.
fn place_right(line: &mut Line<'static>, right: Line<'static>, width: usize) {
    let used = text::line_width(line);
    let need = text::line_width(&right);
    if need == 0 || used + 2 + need + MARGIN > width {
        return;
    }
    pad_to(line, width - MARGIN - need);
    line.spans.extend(right.spans);
}

/// A highlightable line's first cells: blank, or `›` at the heading column
/// when the terminal gave no ground to tint.
fn lead(chosen: bool, theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    if chosen && theme.row_surface().is_none() {
        pad_to(&mut line, HEAD_COL);
        line.spans.push(Span::styled("›", theme.text()));
    }
    line
}

/// A control's first cells ("+ New Agent", a section heading), which
/// highlight as an agent row does: tinted where the terminal gave a ground
/// to tint, else marked. The control's own glyph holds the heading column,
/// so its `›` sits in the margin before it.
fn control_lead(chosen: bool, theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    if chosen && theme.row_surface().is_none() {
        line.spans.push(Span::styled("›", theme.text()));
    }
    line
}

fn name_style(chosen: bool, dim: bool, theme: Theme) -> Style {
    if chosen && theme.row_surface().is_none() {
        theme.emphasis()
    } else if dim {
        theme.muted()
    } else {
        theme.bright()
    }
}

/// The highlighted row's tint, from the outer margin on one side to the
/// outer margin on the other. Every highlightable line starts with at least
/// a margin of blank padding, so the tint's start is taken from that.
fn tint(line: Line<'static>, chosen: bool, width: usize, theme: Theme) -> Line<'static> {
    let Some(surface) = theme.row_surface().filter(|_| chosen) else {
        return line;
    };
    let mut spans = line.spans.into_iter();
    let mut out = Line::from(Span::raw(" ".repeat(MARGIN)));
    if let Some(first) = spans.next() {
        let rest: String = first.content.chars().skip(MARGIN).collect();
        out.spans
            .push(Span::styled(rest, first.style.patch(surface)));
    }
    for span in spans {
        out.spans
            .push(Span::styled(span.content, span.style.patch(surface)));
    }
    text::fill(&mut out, surface, width - MARGIN);
    out
}

/// An agent's mark: empty when nothing is happening, full while it works,
/// and full in the attention ink when it needs you. Every other state is said in
/// words on the second line, so there are only three marks to learn.
fn mark(row: &FleetRow, quiet: bool, theme: Theme) -> (&'static str, Style) {
    if quiet {
        return ("○", theme.faint());
    }
    if row.loud.is_some() {
        return ("●", theme.attention());
    }
    match row.card.attention {
        Attention::NeedsYou => ("●", theme.attention()),
        Attention::Working => ("●", theme.text()),
        Attention::Starting | Attention::Idle => ("○", theme.muted()),
        Attention::Exited => ("○", theme.faint()),
    }
}

/// The provider a sign-in belongs to.
fn provider_word(kind: Kind) -> &'static str {
    match kind {
        Kind::ClaudeSdk | Kind::ClaudePty => "Claude",
        Kind::Codex => "Codex",
        Kind::Unspecified => "the provider",
    }
}

/// What an ask asks, in a line: the command, the file, the question.
fn ask_words(ask: &AskSummary) -> String {
    let words = match &ask.subject {
        AskSubject::Command { command } => format!("wants to run {}", text::first_line(command)),
        AskSubject::Edit { files, .. } if *files > 1 => format!("wants to edit {files} files"),
        AskSubject::Edit { path, created, .. } => {
            let verb = match created {
                Some(true) => "create",
                Some(false) => "edit",
                None => "write",
            };
            format!("wants to {verb} {path}")
        }
        AskSubject::Tool { server, tool } => {
            let tool = [server.as_str(), tool.as_str()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            format!("wants to use {tool}")
        }
        AskSubject::Question { question, .. } => text::first_line(question).to_owned(),
        AskSubject::Plan => "has a plan for you to decide".to_owned(),
        AskSubject::Form { server, message } | AskSubject::Link { server, message } => {
            match server.as_str() {
                "" => text::first_line(message).to_owned(),
                server => format!("{server}: {}", text::first_line(message)),
            }
        }
        AskSubject::Access { reason } => format!("wants access: {}", text::first_line(reason)),
        AskSubject::Unanswerable { reason } => text::first_line(reason).to_owned(),
    };
    match ask.count {
        0 | 1 => words,
        count => format!("{words} · {} more", count - 1),
    }
}

/// The step a working agent is on, as the chat's activity line names it:
/// the step's own subject when it is running one.
fn step_words(line: &ActivityLine) -> String {
    match (&line.activity.kind, &line.step) {
        (ActivityKind::Running { .. }, Some(step)) => text::first_line(step).to_owned(),
        (kind, _) => crate::chat::activity_words(kind),
    }
}

/// A row's second line in words and ink, or None when there is nothing to
/// say (starting, or nothing known yet).
fn second_words(
    line: &SecondLine,
    row: &FleetRow,
    now_ms: i64,
    theme: Theme,
) -> Option<(String, Style)> {
    // A stop the person gave while the host was out of reach waits to be
    // sent; `x` takes it back.
    if row.card.stop_held {
        let host = match row.card.host.as_str() {
            "" => "its host",
            host => host,
        };
        return Some((
            format!("stop held · it goes when {host} is back · x cancels"),
            theme.warning(),
        ));
    }
    // Work it runs in the background, which leaves it idle, is counted
    // first, so a long line cannot cut the count off.
    let jobs = row.card.background_jobs;
    if jobs > 0
        && matches!(
            line,
            SecondLine::LastSaid(_) | SecondLine::Step(_) | SecondLine::Blank
        )
    {
        let count = format!("{jobs} background job{}", if jobs == 1 { "" } else { "s" });
        return Some(match line {
            SecondLine::LastSaid(said) => (format!("{count} · {said}"), theme.muted()),
            SecondLine::Step(step) => (format!("{count} · {}", step_words(step)), theme.muted()),
            _ => (format!("{count} running"), theme.muted()),
        });
    }
    Some(match line {
        // What it asks is the most important line on home.
        SecondLine::Ask(ask) => (ask_words(ask), theme.text()),
        SecondLine::Step(step) => (step_words(step), theme.muted()),
        SecondLine::LastSaid(said) => (said.clone(), theme.muted()),
        SecondLine::Stuck(StuckReason::SignedOut { state, .. }) => {
            let provider = provider_word(row.card.kind);
            let words = match state {
                SignInState::Expired => format!("{provider} sign-in expired"),
                SignInState::Failed => format!("{provider} sign-in failed"),
                _ => format!("signed out of {provider}"),
            };
            (words, theme.warning())
        }
        SecondLine::Stuck(StuckReason::UsageLimit { resets_at_ms }) => {
            let mut words = "usage limit reached".to_owned();
            if let Some(at) = resets_at_ms.filter(|at| *at > now_ms) {
                words.push_str(&format!(" · resets {}", crate::chat::resets(at, now_ms)));
            }
            (words, theme.warning())
        }
        SecondLine::Exited(ExitCause::Finished) => ("finished".to_owned(), theme.faint()),
        SecondLine::Exited(ExitCause::Ended) => ("exited".to_owned(), theme.faint()),
        SecondLine::Exited(ExitCause::Failed(cause)) => {
            (format!("exited · {cause}"), theme.error())
        }
        SecondLine::HostRevoked => {
            let host = match row.card.host.as_str() {
                "" => "its host",
                host => host,
            };
            (
                format!("{host} no longer trusts this machine"),
                theme.warning(),
            )
        }
        SecondLine::Blank => return None,
    })
}

/// An agent's lines. The first: its mark, its name, its branch, and faint
/// what runs it and where (see `where_it_runs`), with how long it has been
/// in its state at the right or, on the highlighted row, `[x]`. The second
/// says what its state calls for (see `ui_view::SecondLine`), or for a
/// folded family, what its member that needs you asks; never its state
/// again in words. Ink follows importance: the name and an ask read
/// brightest, the branch and second line grey, where and when faint.
fn agent_lines(
    row: &FleetRow,
    chosen: bool,
    renaming: Option<&Editor>,
    now_ms: i64,
    width: usize,
    theme: Theme,
    place: &Place<'_>,
) -> Vec<Laid> {
    let card = &row.card;
    let target = Target::Agent(card.agent.clone());
    let quiet = card.attention == Attention::Exited;
    // A live agent on a host out of reach is only as it last said.
    let unreached = !card.host_reach.online();
    let indent = 2 * row.depth as usize;
    // Text sits a margin inside the card's edge, on the right as on the left.
    let end = width - 2 * MARGIN;

    let mut first = lead(chosen, theme);
    let mut spots = vec![(None, Hit::Row(target.clone()))];
    pad_to(&mut first, MARK_COL + indent);
    let (glyph, glyph_style) = mark(row, unreached, theme);
    first.spans.push(Span::styled(glyph, glyph_style));
    pad_to(&mut first, NAME_COL + indent);
    // The right edge: the age, or on the highlighted row its `[x]`.
    let right = if chosen {
        CLOSE.to_owned()
    } else {
        text::age(now_ms, card.phase_since_ms)
    };
    let right_at = end.saturating_sub(text::str_width(&right));
    let fold = (card.children > 0).then(|| {
        format!(
            " {} {}",
            if row.expanded { "▾" } else { "▸" },
            card.children
        )
    });
    let elsewhere = (card.agent.host != place.local_host && !card.host.is_empty())
        .then_some(card.host.as_str());
    let room = right_at.saturating_sub(NAME_COL + indent + 2);
    let fold_width = fold.as_deref().map_or(0, text::str_width);
    // Renaming, the name is a field in its own place, underlined.
    match renaming {
        Some(editor) => first.spans.push(Span::styled(
            text::ellipsize(editor.text(), room.max(1)),
            theme.bright().add_modifier(Modifier::UNDERLINED),
        )),
        None => {
            let name = text::ellipsize(&card.name, room.saturating_sub(fold_width).max(1));
            first.spans.push(Span::styled(
                name,
                name_style(chosen, quiet || unreached, theme),
            ));
        }
    }
    if let Some(fold) = fold {
        let from = text::line_width(&first);
        let style = if row.loud.is_some() {
            theme.attention()
        } else {
            theme.faint()
        };
        first.spans.push(Span::styled(fold, style));
        spots.push((
            Some((from, text::line_width(&first))),
            Hit::Fold(card.agent.clone()),
        ));
    }
    // The branch keeps up to half of what is left; where it runs gives way
    // before it.
    if let Some(branch) = &card.branch {
        let used = text::line_width(&first) + 2;
        let room = right_at.saturating_sub(used + 1) / 2;
        if room > 0 {
            pad_to(&mut first, used);
            first
                .spans
                .push(Span::styled(text::ellipsize(branch, room), theme.muted()));
        }
    }
    let used = text::line_width(&first) + 2;
    if right_at > used + 1 {
        pad_to(&mut first, used);
        let meta = where_it_runs(card.kind, &card.cwd, elsewhere, right_at - used - 1);
        first.spans.push(Span::styled(meta, theme.faint()));
    }
    pad_to(&mut first, right_at);
    if chosen {
        first.spans.push(Span::styled(right, theme.text()));
        spots.push((Some((right_at, end)), Hit::Close(card.agent.clone())));
    } else {
        first.spans.push(Span::styled(right, theme.faint()));
    }

    let first = Laid {
        line: tint(first, chosen, width, theme),
        spots,
    };
    let mut second = Line::default();
    pad_to(&mut second, NAME_COL + indent);
    let room = end.saturating_sub(NAME_COL + indent);
    if let Some(loud) = &row.loud {
        let name = text::ellipsize(&loud.name, room / 2);
        push(&mut second, "↳ ", theme.faint(), end);
        push(&mut second, name, theme.text(), end);
        let waiting = second_words(&loud.second_line, row, now_ms, theme)
            .map_or_else(|| "needs you".to_owned(), |(words, _)| words);
        push(&mut second, format!(" · {waiting}"), theme.text(), end);
    } else {
        let Some((words, style)) = second_words(&row.second_line, row, now_ms, theme) else {
            return vec![first];
        };
        let words = text::first_line(&words).to_owned();
        push(&mut second, text::ellipsize(&words, room), style, end);
    }
    vec![first, Laid::row(tint(second, chosen, width, theme), target)]
}
