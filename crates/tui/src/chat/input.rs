//! Keyboard handling for an open chat: composer, slash commands, ask cards,
//! settings picker, in-flight pane and selection keys.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::layout::Position;
use ui_state::{Composer, Key, PhaseView, SessionState};
use ui_view::{AskBody, AskCard, Changeable, ClientCommand, Slash};

use super::ask::AskAction;
use super::composer::QueueEntry;
use super::layout::Anchor;
use super::{
    ASK_ARMS_MS, ChatEffect, ChatView, Hint, Selection, StopAsk, commands, composer, has_draft,
    keys, modes_stand_for_permissions, next_control, pane, refused_command_words, review,
    review_counter, unchangeable_words, withdraw_effect,
};
use crate::text;
use crate::theme::Theme;

impl ChatView {
    /// One key with no time of its own: every ask open has long been on
    /// screen.
    pub fn key(&mut self, state: &SessionState, key: KeyEvent, theme: Theme) -> Vec<ChatEffect> {
        self.timed_key(state, key, theme, None)
    }

    /// One key, pressed at `now_ms`. Ctrl+C and the leader are the app's
    /// and never reach here.
    pub fn key_at(
        &mut self,
        state: &SessionState,
        key: KeyEvent,
        theme: Theme,
        now_ms: i64,
    ) -> Vec<ChatEffect> {
        self.timed_key(state, key, theme, Some(now_ms))
    }

    fn timed_key(
        &mut self,
        state: &SessionState,
        key: KeyEvent,
        theme: Theme,
        now_ms: Option<i64>,
    ) -> Vec<ChatEffect> {
        let quiet_since = self.last_key_at;
        if now_ms.is_some() {
            self.last_key_at = now_ms;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(ask) = self.stop_ask.take() {
            return self.stop_ask_key(ask, key);
        }
        if let Some(effects) = self.selection_key(state, key, theme) {
            return effects;
        }
        if key.code == KeyCode::Backspace
            && key.modifiers.is_empty()
            && self.composer_has_keys(state)
            && let Some(spot) = self.composer_spot
            && let Some((from, to)) = composer::folded_quote_above(&self.editor, spot.wrap)
        {
            self.editor.remove_chars(from, to);
            return vec![];
        }
        if let Some(mut ask) = self.discard_ask.take() {
            return match ask.key(key) {
                Some(true) => vec![ChatEffect::ReviewEdit {
                    edits: vec![ask.edit()],
                    reopen: false,
                }],
                Some(false) => vec![],
                None => {
                    self.discard_ask = Some(ask);
                    vec![]
                }
            };
        }
        if let Some((_, page)) = &mut self.round {
            if page.key(key) == review::ReviewAction::Close {
                self.round = None;
            }
            return vec![];
        }
        if self.review_page_open
            && let Some(page) = &mut self.review_page
        {
            return match page.key(key) {
                review::ReviewAction::None => vec![],
                review::ReviewAction::Close => {
                    self.review_page_open = false;
                    vec![]
                }
                review::ReviewAction::Open { refresh } => vec![ChatEffect::ReviewOpen { refresh }],
                review::ReviewAction::Edit { edits, reopen } => {
                    vec![ChatEffect::ReviewEdit { edits, reopen }]
                }
                review::ReviewAction::Context(ask) => vec![ChatEffect::ReviewContext(ask)],
                review::ReviewAction::Notice(words) => vec![ChatEffect::Notice(words.into())],
            };
        }
        if key.code == KeyCode::Char('o') && ctrl {
            self.pane_toggle_key();
            return vec![];
        }
        if let Some(effects) = self.setting_key(state, key) {
            return effects;
        }
        match key.code {
            KeyCode::Char('b') if ctrl => {
                // Takes Claude's own view back to this agent's conversation.
                return if ui_view::switched_away(state) {
                    vec![ChatEffect::SwitchBack]
                } else {
                    vec![]
                };
            }
            // A provider subagent has no turn of its own to interrupt: its
            // one stop ends it, so the Stop window asks first, as for a job.
            KeyCode::Char('x') if ctrl && let Composer::Native { stop } = state.composer() => {
                if let Some(key) = stop.key() {
                    self.stop_ask = Some(StopAsk {
                        key: key.to_owned(),
                        command: ui_state::shown_name(state.agent()).to_owned(),
                        stop: true,
                        subagent: true,
                    });
                }
                return vec![];
            }
            KeyCode::Char('x') if ctrl => {
                let stops = ui_view::ask_card(state).is_some_and(|card| card.stops_turn);
                return if state.phase() == PhaseView::Working || stops {
                    vec![ChatEffect::Interrupt]
                } else {
                    vec![]
                };
            }
            KeyCode::PageUp => {
                // The pin covers the top of the page above too.
                let pin = self
                    .pin_spot
                    .as_ref()
                    .map_or(0, |((from, to), _)| usize::from(to - from) + 1);
                let page = self.feed.1.saturating_sub(2 + pin).max(1) as isize;
                if !self.scroll_box_diff(state, -page) {
                    self.scroll(state, -page, theme);
                }
                return vec![];
            }
            KeyCode::PageDown => {
                let page = self.feed.1.saturating_sub(2).max(1) as isize;
                if !self.scroll_box_diff(state, page) {
                    self.scroll(state, page, theme);
                    self.paged = true;
                }
                return vec![];
            }
            KeyCode::End if ctrl => {
                self.follow();
                return vec![];
            }
            KeyCode::Home if ctrl => {
                if let Some(oldest) = state.transcript().iter().next() {
                    self.anchor = Anchor::Top {
                        key: oldest.item.key.clone(),
                        offset: 0,
                    };
                }
                return vec![];
            }
            _ => {}
        }
        // The pane has the keys: it takes the ones it uses and ignores the
        // rest; Esc hands the keys back to the composer.
        if self.pane_open && self.pane_keys {
            return self.pane_key(state, key);
        }
        if let Some(card) = self.card_takes_keys(state) {
            if !self.arm_ask(&card, now_ms, quiet_since) {
                // Words typed as the ask appears are still the draft's; Enter
                // neither sends the draft nor answers what was not read. A
                // chat with no draft drops them: the ask was not read either.
                if key.code != KeyCode::Enter && has_draft(state) && self.editor.key(key) {
                    self.not_sent = None;
                }
                return vec![];
            }
            self.ask.sync(&card);
            // An ask takes over the composer's box, and Esc there never
            // leaves the ask.
            let action = self.ask.box_key(&card, key);
            return self.ask_effects(state, &card, action);
        }
        // A provider subagent takes no messages, nor does an agent that has
        // ended and cannot be resumed: there is no field to type in, and
        // Enter sends nothing.
        if matches!(state.composer(), Composer::Native { .. } | Composer::Ended) {
            if key.code == KeyCode::Esc {
                self.escape();
            }
            return vec![];
        }
        if let Some(selected) = self.tray {
            return self.tray_key(state, selected, key);
        }
        if let Some(effects) = self.command_key(state, key) {
            return effects;
        }
        self.composer_key(state, key)
    }

    /// The agent's commands the draft offers now: while the composer has
    /// the keys, its cursor at the draft's end, and the person has not
    /// closed them on this draft.
    pub(super) fn offered_commands(&self, state: &SessionState) -> Vec<ui_view::CommandMatch> {
        let text = self.editor.text();
        if !self.composer_has_keys(state)
            || self.editor.cursor_chars() != text.chars().count()
            || self.menu.closed(text)
        {
            return Vec::new();
        }
        ui_view::command_matches(&ui_view::settings(state).commands, text)
    }

    /// A key while the commands' flyover is open: it moves the lit command,
    /// fills it in, runs it or closes the flyover. None when the flyover is
    /// not open or the key is the draft's.
    fn command_key(&mut self, state: &SessionState, key: KeyEvent) -> Option<Vec<ChatEffect>> {
        let matches = self.offered_commands(state);
        if matches.is_empty() || key.modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }
        let text = self.editor.text().to_owned();
        let lit = &matches[self.menu.selected(&text, &matches)];
        match key.code {
            KeyCode::Up => self.menu.step(&text, &matches, -1),
            KeyCode::Down => self.menu.step(&text, &matches, 1),
            KeyCode::Tab if key.modifiers.is_empty() => self.fill_command(lit),
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                let runs = commands::enter_runs(&lit.command);
                self.fill_command(lit);
                if runs {
                    return Some(self.submit(state));
                }
            }
            KeyCode::Esc => self.menu.close(&text),
            _ => return None,
        }
        Some(vec![])
    }

    /// Puts a picked command in the draft, as the shared view says.
    pub(super) fn fill_command(&mut self, found: &ui_view::CommandMatch) {
        let attachments = self.editor.attachments().to_vec();
        self.editor.set(&found.picked, attachments);
        self.focus = None;
        self.not_sent = None;
    }

    /// Whether `card` takes a key pressed at `now_ms`, the key before it
    /// having been pressed at `last_key`: once it has been on screen for
    /// [`ASK_ARMS_MS`] with no key pressed. An ask not drawn yet has not been
    /// seen at all. Once armed it stays armed.
    fn arm_ask(&mut self, card: &AskCard, now_ms: Option<i64>, last_key: Option<i64>) -> bool {
        let Some(now_ms) = now_ms else {
            return true;
        };
        if self.ask_armed.as_deref() == Some(card.key.as_str()) {
            return true;
        }
        let armed = match &self.ask_drawn {
            Some((key, drawn)) if *key == card.key => {
                let quiet_from = last_key.map_or(*drawn, |at| at.max(*drawn));
                now_ms.saturating_sub(quiet_from) >= ASK_ARMS_MS
            }
            _ => false,
        };
        if armed {
            self.ask_armed = Some(card.key.clone());
        }
        armed
    }

    /// What the chat does for an action on the ask card.
    pub(super) fn ask_effects(
        &mut self,
        state: &SessionState,
        card: &AskCard,
        action: AskAction,
    ) -> Vec<ChatEffect> {
        match action {
            AskAction::None => vec![],
            AskAction::Attach => vec![ChatEffect::RawAttach],
            AskAction::OpenUrl(url) => vec![ChatEffect::OpenUrl(url)],
            AskAction::Answer(input) => {
                // The plan was read from its top; once it is answered, the
                // feed follows the work it sets going.
                if matches!(card.body, AskBody::Plan { .. }) {
                    self.follow();
                    self.expanded.remove(&card.item_key);
                }
                vec![ChatEffect::Answer(*input)]
            }
            AskAction::Resend => state
                .answering(&card.key)
                .map(|sent| {
                    vec![ChatEffect::Resend {
                        id: sent.id.clone(),
                    }]
                })
                .unwrap_or_default(),
            AskAction::Discard => state
                .answering(&card.key)
                .map(|sent| {
                    vec![ChatEffect::Discard {
                        id: sent.id.clone(),
                    }]
                })
                .unwrap_or_default(),
        }
    }

    /// A key for the composer, when nothing else takes it.
    fn composer_key(&mut self, state: &SessionState, key: KeyEvent) -> Vec<ChatEffect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.escape();
                vec![]
            }
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => self.submit(state),
            KeyCode::Char('v') if ctrl => vec![ChatEffect::Paste],
            KeyCode::BackTab if state.composer() == Composer::Send => {
                next_control(&ui_view::settings(state))
                    .and_then(|change| ui_view::setting_input(state.kind(), &change))
                    .map(ChatEffect::Answer)
                    .into_iter()
                    .collect()
            }
            KeyCode::Up if self.editor.is_empty() && !self.queue_entries(state, "").is_empty() => {
                self.tray = Some(self.queue_entries(state, "").len() - 1);
                vec![]
            }
            // Nothing queued: the chat's last prompt comes back to send
            // again or edit, as a shell's history does.
            KeyCode::Up if self.editor.is_empty() => {
                if let Some(last) = state.transcript().iter().rev().find(|held| {
                    matches!(
                        held.class,
                        ui_state::ItemClass::Prompt | ui_state::ItemClass::Steer
                    )
                }) {
                    let words: String = last
                        .item
                        .text
                        .chars()
                        .filter(|c| *c != attachments::PLACEHOLDER)
                        .collect();
                    self.editor.set(words.trim(), Vec::new());
                }
                vec![]
            }
            _ => {
                if self.editor.key(key) {
                    self.focus = None;
                    // Editing answers why the last one was not sent.
                    self.not_sent = None;
                }
                vec![]
            }
        }
    }

    fn tray_key(
        &mut self,
        state: &SessionState,
        selected: usize,
        key: KeyEvent,
    ) -> Vec<ChatEffect> {
        let entries = self.queue_entries(state, "");
        let Some(entry) = entries.get(selected.min(entries.len().saturating_sub(1))) else {
            self.tray = None;
            return vec![];
        };
        match key.code {
            KeyCode::Esc => self.tray = None,
            KeyCode::Up => self.tray = Some(selected.saturating_sub(1)),
            KeyCode::Down if selected + 1 >= entries.len() => self.tray = None,
            KeyCode::Down => self.tray = Some(selected + 1),
            _ => {
                let effect = match (entry, key.code) {
                    // Typing on an entry goes back to the composer.
                    (_, KeyCode::Char(_))
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.tray = None;
                        if self.editor.key(key) {
                            self.not_sent = None;
                        }
                        None
                    }
                    (_, KeyCode::Enter) => self.queue_act(state, entry, 0),
                    (_, KeyCode::Delete | KeyCode::Backspace) => self.queue_act(state, entry, 1),
                    _ => None,
                };
                if let Some(effect) = effect {
                    // Acting on an entry hands the keys back to the
                    // composer: a withdrawn prompt's words are there to
                    // edit, and a second Enter must not act on the next.
                    self.tray = None;
                    return vec![effect];
                }
            }
        }
        vec![]
    }

    /// An entry's control by place: 0 is its first ("[Send Now]",
    /// "[Resend]"), 1 its second ("[Withdraw]", "[Discard]").
    pub(super) fn queue_act(
        &self,
        state: &SessionState,
        entry: &QueueEntry,
        control: usize,
    ) -> Option<ChatEffect> {
        match (entry, control) {
            (QueueEntry::Queued(queued), 0) if queued.can_send_now => Some(ChatEffect::SendNow {
                id: queued.input_id.clone(),
            }),
            (QueueEntry::Queued(queued), 1) if queued.can_withdraw => {
                withdraw_effect(state, &queued.input_id)
            }
            (QueueEntry::Unconfirmed { input_id, .. }, 0) => Some(ChatEffect::Resend {
                id: input_id.clone(),
            }),
            (QueueEntry::Unconfirmed { input_id, .. }, 1) => Some(ChatEffect::Discard {
                id: input_id.clone(),
            }),
            _ => None,
        }
    }

    /// Enter in the composer: send when caught up and live, resume an
    /// exited agent with the draft, and otherwise keep the draft.
    pub(super) fn submit(&mut self, state: &SessionState) -> Vec<ChatEffect> {
        // Enter on an exited agent resumes it, with or without a message.
        // A draft of only blanks is no message: it would spend a turn. The
        // review chip's drafts are one.
        let reviewed = self.review_chip(state).is_some();
        let blank = self.editor.attachments().is_empty()
            && self.editor.text().trim().is_empty()
            && !reviewed;
        if blank && state.composer() != Composer::Resume {
            return vec![];
        }
        match state.composer() {
            // A provider subagent takes no messages at all, and an agent
            // that ended for good takes none again: the draft stays.
            Composer::Native { .. } | Composer::Ended => vec![],
            // Not current, nothing goes: the draft stays in the composer,
            // whose top edge says why, until it can.
            Composer::Waiting(_) => {
                self.draft_owed = true;
                vec![]
            }
            // A prompt never goes without a file still on its way into it.
            Composer::Send | Composer::Resume if !self.attaching.is_empty() => {
                self.send_when_attached = true;
                vec![ChatEffect::Notice(format!(
                    "Sends once {} is attached",
                    self.attaching.join(", ")
                ))]
            }
            Composer::Send => {
                if !reviewed && let Some(effects) = self.slash_command(state) {
                    return effects;
                }
                let (text, attachments) = self.editor.take_prompt();
                self.not_sent = None;
                self.follow();
                if reviewed {
                    self.review_sent = review_counter(state);
                    return vec![ChatEffect::PromptWithReview { text, attachments }];
                }
                vec![ChatEffect::Prompt { text, attachments }]
            }
            Composer::Resume => {
                let (text, attachments) = self.editor.take_prompt();
                self.not_sent = None;
                self.follow();
                vec![ChatEffect::Resume { text, attachments }]
            }
        }
    }

    /// A draft that is a command the client handles, or one this agent
    /// does not run: what it does instead of going as a prompt. None sends
    /// the draft as typed.
    fn slash_command(&mut self, state: &SessionState) -> Option<Vec<ChatEffect>> {
        use crate::setup::{Item, Picker};
        let text = self.editor.text().to_owned();
        if !self.editor.attachments().is_empty() {
            return None;
        }
        let effects = match ui_view::slash(state, &text) {
            Slash::Prompt => return None,
            Slash::Typed(typed) => vec![ChatEffect::Prompt {
                text: typed,
                attachments: Vec::new(),
            }],
            Slash::Set(input) => vec![ChatEffect::Answer(input)],
            Slash::Pick(command) => {
                let view = ui_view::settings(state);
                let (item, title, choices) = match command {
                    ClientCommand::Effort => {
                        (Item::Effort, "Effort", crate::setup::effort_choices(&view))
                    }
                    _ => (Item::Model, "Model", crate::setup::model_choices(&view)),
                };
                self.picker = Some(Picker::new(item, title, choices, false));
                vec![]
            }
            Slash::Rename(name) if name.is_empty() => {
                return Some(vec![ChatEffect::Notice(
                    "Name the agent after the command: /rename <name>".into(),
                )]);
            }
            // A name the rule refuses is refused here, the draft kept to
            // fix, in words that fit the key line.
            Slash::Rename(name) => match wire::agent_name_problem(&name) {
                Some(problem) => {
                    return Some(vec![ChatEffect::Notice(format!(
                        "not renamed: {}",
                        crate::words::name_problem(problem)
                    ))]);
                }
                None => vec![ChatEffect::Rename(name)],
            },
            // The draft stays, so the person can fix it.
            Slash::Refused {
                command,
                offered,
                hint,
            } => {
                return Some(vec![ChatEffect::Notice(refused_command_words(
                    state,
                    &command,
                    &offered,
                    hint.as_deref(),
                ))]);
            }
            Slash::Unknown {
                command,
                value,
                offered,
            } => {
                let what = match command {
                    ClientCommand::Effort => "effort",
                    _ => "model",
                };
                return Some(vec![ChatEffect::Notice(format!(
                    "No {what} {value} here; it offers {}",
                    offered.join(", ")
                ))]);
            }
        };
        self.editor.take_prompt();
        self.not_sent = None;
        Some(effects)
    }

    /// Ctrl+S and what follows it: a running chat's model, effort and
    /// permission, where the agent lets a client change them. None when the key is
    /// not this.
    fn setting_key(&mut self, state: &SessionState, key: KeyEvent) -> Option<Vec<ChatEffect>> {
        use crate::setup::{Item, Picker};
        // An open ask holds the composer's box, where a picker would be
        // drawn: a picker opened under it would take keys unseen, so the ask
        // wins and the settings wait until it is answered.
        if self.card_takes_keys(state).is_some() {
            self.picker = None;
            self.setting_prefix = false;
            let settings_key =
                key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL);
            return settings_key.then(|| {
                vec![ChatEffect::Notice(
                    "Answer the ask first; settings change once it is answered".into(),
                )]
            });
        }
        if let Some(picker) = &mut self.picker {
            let pick = picker.key(key);
            return Some(self.picked(state, pick));
        }
        if std::mem::take(&mut self.setting_prefix) {
            let view = ui_view::settings(state);
            let (setting, item, title) = match key.code {
                KeyCode::Char('m') => (&view.changeable.model, Item::Model, "Model"),
                KeyCode::Char('e') => (&view.changeable.effort, Item::Effort, "Effort"),
                KeyCode::Char('p') if modes_stand_for_permissions(&view) => {
                    (&view.changeable.mode, Item::Mode, "Mode")
                }
                KeyCode::Char('p') => (&view.changeable.permission, Item::Permission, "Permission"),
                // Esc takes the settings key back; any other key was never
                // meant for it and goes on as typed.
                KeyCode::Esc => return Some(vec![]),
                _ => return None,
            };
            if *setting != Changeable::Pick {
                // A setting this agent can't change from here says why
                // rather than doing nothing.
                return Some(vec![ChatEffect::Notice(unchangeable_words(
                    &view, setting, item,
                ))]);
            }
            let choices = match item {
                Item::Model => crate::setup::model_choices(&view),
                Item::Effort => crate::setup::effort_choices(&view),
                Item::Mode => crate::setup::mode_choices(&view),
                _ => crate::setup::permission_choices(&view),
            };
            self.picker = Some(Picker::new(item, title, choices, false));
            return Some(vec![]);
        }
        if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.setting_prefix = true;
            return Some(vec![]);
        }
        None
    }

    /// What a model or effort flyover's key or click did.
    pub(super) fn picked(
        &mut self,
        state: &SessionState,
        pick: crate::setup::Pick,
    ) -> Vec<ChatEffect> {
        use crate::setup::{Item, Pick};
        let Some(item) = self.picker.as_ref().map(|picker| picker.item) else {
            return vec![];
        };
        match pick {
            Pick::None => vec![],
            Pick::Close => {
                self.picker = None;
                vec![]
            }
            Pick::Value(value) => {
                self.picker = None;
                let change = match item {
                    Item::Effort => ui_view::SettingChange::Effort(value),
                    Item::Permission => ui_view::SettingChange::Permission(value),
                    Item::Mode => ui_view::SettingChange::Mode(value),
                    _ => ui_view::SettingChange::Model(value),
                };
                match ui_view::setting_input(state.kind(), &change) {
                    Some(input) => vec![ChatEffect::Answer(input)],
                    // An agent that changes it only by its own command
                    // (terminal Claude) gets that command typed.
                    None => ui_view::typed_setting(&change)
                        .map(|text| ChatEffect::Prompt {
                            text,
                            attachments: Vec::new(),
                        })
                        .into_iter()
                        .collect(),
                }
            }
        }
    }

    /// Scrolls an opened diff in the ask's box; false when there is none.
    pub(super) fn scroll_box_diff(&mut self, state: &SessionState, lines: isize) -> bool {
        match self.card_takes_keys(state) {
            Some(card) => {
                self.ask.sync(&card);
                self.ask.scroll_diff(&card, lines)
            }
            None => false,
        }
    }

    /// Whether the pointer is under the feed: on the composer or its box.
    pub(super) fn below_feed(&self, event: MouseEvent) -> bool {
        event.row >= self.feed_origin.1 + self.feed.1 as u16
    }

    /// Esc, view-only: clear focus, then follow the newest row. It never
    /// answers and never interrupts.
    fn escape(&mut self) {
        if self.focus.take().is_some() {
            return;
        }
        self.follow();
    }

    pub fn follow(&mut self) {
        self.anchor = Anchor::Bottom;
    }

    /// Ctrl+O: closed, the pane opens with the keys; open with the
    /// composer holding the keys, the pane takes them; holding them, it
    /// closes.
    pub fn pane_toggle_key(&mut self) {
        if !self.pane_open {
            self.open_pane();
        } else if !self.pane_keys {
            self.focus_pane();
        } else {
            self.close_pane();
        }
    }

    /// Opens the pane with the keys.
    pub fn open_pane(&mut self) {
        self.pane_open = true;
        self.focus_pane();
    }

    pub fn close_pane(&mut self) {
        self.pane_open = false;
        self.pane_keys = false;
        self.pane_hover = None;
    }

    /// Gives the pane the keys; the item they land on is settled when the
    /// pane is next drawn.
    pub(super) fn focus_pane(&mut self) {
        self.pane_keys = true;
        self.pane_follow = true;
    }

    /// The pane's items in its order, as this frame would draw them.
    fn pane_items(&self, state: &SessionState) -> Vec<pane::PaneItem> {
        let overview = self.overview(state);
        pane::Contents {
            overview: &overview,
            folded: &self.pane_folds,
        }
        .items()
    }

    /// Where the pane's keys are among `items`: the focused item while it
    /// is still listed, else the first job, else the first item.
    pub(super) fn pane_at(&self, items: &[pane::PaneItem]) -> Option<usize> {
        self.pane_focus
            .as_ref()
            .and_then(|focus| items.iter().position(|item| item.same(focus)))
            .or_else(|| {
                items
                    .iter()
                    .position(|item| matches!(item, pane::PaneItem::Job(_)))
            })
            .or((!items.is_empty()).then_some(0))
    }

    /// A key while the pane has the keys: j/k and the arrows move across
    /// its headings, jobs and changed files as one list; Enter folds a
    /// heading, opens a job's step in the chat or a file on the review
    /// page; Esc hands the keys back to the composer, the pane staying
    /// open. It ignores the rest.
    fn pane_key(&mut self, state: &SessionState, key: KeyEvent) -> Vec<ChatEffect> {
        let items = self.pane_items(state);
        let at = self.pane_at(&items);
        let last = items.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => self.pane_keys = false,
            KeyCode::Char('j') | KeyCode::Down if key.modifiers.is_empty() => {
                self.pane_focus = at.and_then(|at| items.get((at + 1).min(last))).cloned();
                self.pane_follow = true;
            }
            KeyCode::Char('k') | KeyCode::Up if key.modifiers.is_empty() => {
                self.pane_focus = at.and_then(|at| items.get(at.saturating_sub(1))).cloned();
                self.pane_follow = true;
            }
            KeyCode::Enter => {
                if let Some(item) = at.and_then(|at| items.get(at)).cloned() {
                    self.pane_focus = Some(item.clone());
                    return self.open_item(state, item);
                }
            }
            // A job its provider can stop alone: the Stop window asks first.
            KeyCode::Char('x') if key.modifiers.is_empty() => {
                if let Some(pane::PaneItem::Job(job)) = at.and_then(|at| items.get(at))
                    && let Some(job_key) = pane::stop_key(job)
                {
                    self.stop_ask = Some(StopAsk {
                        key: job_key.to_owned(),
                        command: text::first_line(&job.command).to_owned(),
                        stop: true,
                        subagent: false,
                    });
                }
            }
            _ => {}
        }
        vec![]
    }

    /// A key while the Stop window is open. Enter presses the focused
    /// button, never a letter; the arrows and Tab move it; Esc cancels.
    fn stop_ask_key(&mut self, mut ask: StopAsk, key: KeyEvent) -> Vec<ChatEffect> {
        match key.code {
            KeyCode::Enter if ask.stop => return vec![ChatEffect::StopJob { key: ask.key }],
            KeyCode::Enter | KeyCode::Esc => return vec![],
            KeyCode::Left => ask.stop = true,
            KeyCode::Right => ask.stop = false,
            KeyCode::Tab | KeyCode::BackTab => ask.stop = !ask.stop,
            _ => {}
        }
        self.stop_ask = Some(ask);
        vec![]
    }

    /// A heading folds or unfolds; a job opens its step in the chat; a
    /// file, the review page at it.
    pub(super) fn open_item(
        &mut self,
        state: &SessionState,
        item: pane::PaneItem,
    ) -> Vec<ChatEffect> {
        match item {
            pane::PaneItem::Heading(section) => {
                if !self.pane_folds.remove(&section) {
                    self.pane_folds.insert(section);
                }
                self.pane_follow = true;
                vec![]
            }
            pane::PaneItem::Job(job) => {
                if let Some(key) = &job.step {
                    self.reveal_step(state, key);
                }
                vec![]
            }
            pane::PaneItem::File(path) => vec![ChatEffect::ReviewAt(path)],
        }
    }

    pub(super) fn over_pane(&self, event: MouseEvent) -> bool {
        self.pane_rect
            .is_some_and(|rect| rect.contains(Position::new(event.column, event.row)))
    }

    /// Scrolls the chat to a step and opens it, and the run it sits in.
    fn reveal_step(&mut self, state: &SessionState, key: &Key) {
        let transcript = state.transcript();
        let Some(order) = transcript.get(key).map(|held| held.item.order) else {
            return;
        };
        if !ui_view::run_is_open(state, order, &self.open_runs) {
            ui_view::toggle_run(state, key, &mut self.open_runs);
        }
        self.expanded.insert(key.clone());
        self.anchor = Anchor::Top {
            key: key.clone(),
            offset: 0,
        };
        self.revealed = true;
    }

    /// Whether the reader follows the newest row, when that changed since
    /// the session was last told: the one thing about the view the session
    /// needs, to trim its window while following and hold arrivals while
    /// the reader is in history.
    pub fn following_moved(&mut self) -> Option<bool> {
        let following = self.anchor == Anchor::Bottom;
        (following != self.told_following).then(|| {
            self.told_following = following;
            following
        })
    }

    /// Whether keys typed now reach the composer.
    fn composer_has_keys(&self, state: &SessionState) -> bool {
        !self.review_page_open
            && self.round.is_none()
            && self.discard_ask.is_none()
            && self.picker.is_none()
            && !self.setting_prefix
            && self.tray.is_none()
            && !(self.pane_open && self.pane_keys)
            && self.card_takes_keys(state).is_none()
    }

    /// A key while text is selected. None sends it on as usual: a key that
    /// is not the selection's lets go of it first, except paging, which
    /// reads on with the words still held. In the feed, whatever else has
    /// the keys keeps them.
    fn selection_key(
        &mut self,
        state: &SessionState,
        key: KeyEvent,
        theme: Theme,
    ) -> Option<Vec<ChatEffect>> {
        let in_feed = match self.selection.as_ref()? {
            Selection::Feed(_) => true,
            Selection::Screen(_) => false,
        };
        if in_feed && !self.composer_has_keys(state) {
            return None;
        }
        let plain = key.modifiers.is_empty();
        match key.code {
            KeyCode::Esc => {
                self.clear_selection();
                Some(vec![])
            }
            KeyCode::Char('r') if plain && in_feed && has_draft(state) => {
                let words = self.selected_words(state, theme);
                self.editor.quote(&words);
                self.focus = None;
                self.clear_selection();
                Some(vec![])
            }
            KeyCode::Char('c') if plain && !self.copy_on_select => {
                self.copied = true;
                Some(vec![ChatEffect::Copy(self.selected_words(state, theme))])
            }
            KeyCode::PageUp | KeyCode::PageDown if in_feed => None,
            KeyCode::End if in_feed && key.modifiers.contains(KeyModifiers::CONTROL) => None,
            _ => {
                self.clear_selection();
                None
            }
        }
    }

    pub(super) fn clear_selection(&mut self) {
        self.selection = None;
        self.copied = false;
    }

    /// The hint line while text is selected.
    pub(super) fn selection_hint(&self, state: &SessionState) -> Option<Hint> {
        let in_feed = matches!(self.selection.as_ref()?, Selection::Feed(_));
        if in_feed && !self.composer_has_keys(state) {
            return None;
        }
        let mut pairs = Vec::new();
        if !self.copy_on_select && !self.copied {
            pairs.push(("c", "copy"));
        }
        if in_feed && has_draft(state) {
            pairs.push(("r", "reply"));
        }
        pairs.push(("esc", "clear"));
        Some(if self.copied {
            Hint::Noted("copied", keys(&pairs))
        } else {
            Hint::Keys(keys(&pairs))
        })
    }
}
