//! Frame layout and rendering for an open chat, its in-flight pane, composer
//! box, pinned prompt and overlays.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame as Paint;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ui_state::{Composer, Key, PhaseView, SessionState, Waiting};
use ui_view::{
    AskBody, CardState, Changeable, Comparison, FamilyHeader, Reach, RowKind, ToolRows,
    chat_rows_for, composer as composer_view,
};

use super::composer::{COMPOSER_LINES, edge_row, editor_lines};
use super::layout::{Anchor, Laid};
use super::selection::ScreenCells;
use super::{
    CHAT_MIN, ChatView, ComposerSpot, HeaderControl, HeaderSpots, Hint, LOADING_HINT_MS, PANE_MAX,
    PANE_MIN, PanelEntry, QueuedSpot, Selection, ask, commands, composer, edge_title, ended_box,
    ends_with_exit, exit_words, feed, key_event, keys, layout, native_box, native_hint, pane,
    problem_words, quiet_activity, rows, settled, sign_in_notice, turn_hint,
};
use crate::editor::Editor;
use crate::text::{self, push};
use crate::theme::Theme;

impl ChatView {
    /// Lays the chat out in `area` and paints it. Returns the page to ask
    /// for, if the reader is within a page of the oldest held row.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        paint: &mut Paint<'_>,
        area: Rect,
        state: &SessionState,
        family: Option<&FamilyHeader>,
        footer: Option<Line<'static>>,
        now_ms: i64,
        theme: Theme,
    ) -> Option<u32> {
        if state.epoch() != self.epoch {
            // A Reset swapped the transcript in: scroll to the newest, the
            // one whole-list reload.
            self.epoch = state.epoch();
            self.follow();
            self.focus = None;
            self.page_asked = None;
        }
        if self.review_open
            && let Some(page) = &mut self.review
        {
            // Its header names the agent and where it works, as the chat's.
            let name = ui_state::shown_name(state.agent()).to_owned();
            let cwd = &state.agent().cwd;
            let place = if self.local {
                text::tilde(cwd)
            } else {
                cwd.clone()
            };
            page.set_owner(&name, &place);
            let in_screen = matches!(self.selection, Some(Selection::Screen(_)));
            let footer = footer.or_else(|| {
                in_screen.then(|| {
                    let mut pairs = Vec::new();
                    if !self.copy_on_select && !self.copied {
                        pairs.push(("c", "copy"));
                    }
                    pairs.push(("esc", "clear"));
                    let hint = if self.copied {
                        Hint::Noted("copied", keys(&pairs))
                    } else {
                        Hint::Keys(keys(&pairs))
                    };
                    hint_line(&hint, usize::from(area.width), theme).0
                })
            });
            page.draw(paint, area, footer, theme);
            let cells = ScreenCells::capture(
                paint.buffer_mut(),
                page.stream_area(),
                page.stream_gutters().to_vec(),
            );
            self.keep_screen(paint, cells, theme);
            return None;
        }
        let page = self.draw_turns(paint, area, state, family, footer, now_ms, theme);
        if let Some(ask) = &self.stop_ask {
            stop_window(paint, area, ask, theme);
        }
        page
    }
}

/// The question before one background job is stopped: which job, and the
/// button Enter presses.
#[derive(Clone, Debug)]
pub(super) struct StopAsk {
    pub(super) key: String,
    /// The job's command, or the subagent's name.
    pub(super) command: String,
    /// Stop has the focus, else Cancel.
    pub(super) stop: bool,
    /// It stops the subagent whose chat this is, not one of its jobs.
    pub(super) subagent: bool,
}

/// The Stop window, centred over the chat: "Stop cargo test? The agent
/// keeps running.", then Stop and Cancel.
fn stop_window(paint: &mut Paint<'_>, area: Rect, ask: &StopAsk, theme: Theme) {
    let width = usize::from(area.width);
    let (buttons, _) = crate::panel::buttons(
        &[("Stop", true), ("Cancel", false)],
        usize::from(!ask.stop),
        theme,
    );
    let legend = crate::panel::legend(
        &[("←→", "choose"), ("enter", "press"), ("esc", "cancel")],
        theme,
    );
    // A subagent's stop ends it, and the agent that started it goes on.
    let rest = if ask.subagent {
        "? Its parent carries on."
    } else {
        "? The agent keeps running."
    };
    let inner = (text::str_width("Stop ") + text::str_width(&ask.command) + text::str_width(rest))
        .max(crate::panel::content_width(std::slice::from_ref(&legend)))
        .clamp(36, 60)
        .min(width.saturating_sub(8));
    let room = inner.saturating_sub(text::str_width("Stop ") + text::str_width(rest));
    let sentence = Line::from(vec![
        Span::styled("Stop ", theme.text()),
        Span::styled(text::ellipsize(&ask.command, room.max(8)), theme.code()),
        Span::styled(rest, theme.text()),
    ]);
    let rows = vec![Line::default(), sentence, Line::default(), buttons, legend];
    let lines = crate::panel::bordered("Stop", rows, inner, &[], None, theme);
    crate::panel::centre(paint, lines, area);
}

impl ChatView {
    /// The chat: a top line naming the agent and where it
    /// stands, the feed as turns, and the composer boxed with the model and
    /// mode on its bottom edge. Returns the page to ask for, as `draw` does.
    #[allow(clippy::too_many_arguments)]
    fn draw_turns(
        &mut self,
        paint: &mut Paint<'_>,
        area: Rect,
        state: &SessionState,
        family: Option<&FamilyHeader>,
        footer: Option<Line<'static>>,
        now_ms: i64,
        theme: Theme,
    ) -> Option<u32> {
        // The in-flight pane beside the chat narrows everything under the
        // header, unless the chat would be left too narrow: then it lies
        // over the feed's right side instead.
        let full = usize::from(area.width);
        let side = self.pane_open;
        let pane_width = (full / 3).clamp(PANE_MIN, PANE_MAX).min(full);
        let beside = side && full.saturating_sub(pane_width) >= CHAT_MIN;
        let width = if beside { full - pane_width } else { full };
        // Lying over the feed, the pane leaves the feed the columns to its
        // left, and the feed's words wrap there rather than hide under it.
        let feed_width = if side { full - pane_width } else { full };
        let name = ui_state::shown_name(state.agent()).to_owned();
        let host = state
            .host()
            .map(|host| host.name().to_owned())
            .unwrap_or_else(|| "its computer".into());

        let overview = self.overview(state);
        let sign_in = ui_view::sign_in(state);
        let summary = ui_view::controls(state);
        let items = pane::Contents {
            overview: &overview,
            folded: &self.pane_folds,
        }
        .items();
        let pane_keys = self.pane_open && self.pane_keys;
        if pane_keys {
            self.pane_focus = self.pane_at(&items).map(|at| items[at].clone());
        }
        // The item drawn highlighted, in the pane and, for a job, at its
        // step in the feed: the one under the mouse, else the one the
        // pane's keys are on.
        let lit = self
            .pane_hover
            .clone()
            .filter(|hovered| items.contains(hovered))
            .or(self.pane_focus.clone().filter(|_| pane_keys));
        let (header, controls) = self.top_line(state, &name, full, theme);
        // A blank line above the header keeps it off the terminal's edge.
        self.header_spots = controls
            .into_iter()
            .map(|((from, to), control)| {
                (
                    area.y + 1,
                    (area.x + from as u16, area.x + to as u16),
                    control,
                )
            })
            .collect();
        let mut top: Vec<Line<'static>> = vec![Line::default(), header];
        if let Some(family) = family {
            let mut line = family_line(family, width, theme);
            for span in &mut line.spans {
                if span.style == theme.muted() {
                    span.style = theme.faint();
                }
            }
            top.push(line);
        }
        top.push(Line::default());

        let mut bottom: Vec<Line<'static>> = Vec::new();
        let view = composer_view(state, now_ms);
        self.note_sent(state, now_ms);
        // The running turn's live end, after its newest row and scrolled
        // with it: a prompt on its way while the agent was idle, then what
        // the agent is doing, where "Worked 6m" stands once the turn ends.
        let mut tail: Vec<Line<'static>> = Vec::new();
        for prompt in ui_view::prompts_underway(state) {
            if prompt.lands != ui_view::Lands::Feed {
                continue;
            }
            let ui_view::Underway::Sending { waiting } = prompt.underway else {
                continue;
            };
            let Some(&at_ms) = self.sending.get(&prompt.input_id) else {
                continue;
            };
            let when = if waiting {
                format!("waiting for {host}…")
            } else {
                feed::clock(at_ms)
            };
            tail.extend(feed::pending_prompt(&prompt.text, &when, feed_width, theme));
        }
        if let Some(activity) = &view.activity {
            tail.push(quiet_activity(
                activity,
                &self.agents.live,
                feed_width,
                theme,
            ));
            tail.push(Line::default());
        }
        // An exited agent's feed ends saying so, like a session boundary,
        // unless the agent wrote that boundary itself.
        if let PhaseView::Exited { cause } = state.phase()
            && !ends_with_exit(state)
        {
            tail.push(rows::rule(
                &exit_words(&cause, settled(state)),
                feed_width,
                theme,
            ));
            tail.push(Line::default());
        }
        self.feed_tail = tail;
        let mut row_at = None;
        // Prompts queued behind the running turn wait just above the
        // composer, one line each in the order they will run. At most
        // QUEUED_SHOWN show, the newest unless walking up through them
        // brings earlier ones in; one line says how many more wait, on the
        // side they are on, so the block's height never changes as it moves.
        const QUEUED_SHOWN: usize = 3;
        // The agent's account needs signing in: above everything that rests
        // on the composer, a notice says how. The composer still sends,
        // since sending is how someone who signed in elsewhere tries again;
        // a provider subagent's takes no messages, so it is left alone.
        if let Some(sign_in) = &sign_in
            && !matches!(state.composer(), Composer::Native { .. } | Composer::Ended)
        {
            bottom.extend(sign_in_notice(&host, sign_in, width, theme));
            bottom.push(Line::default());
        }
        let entries = self.queue_entries(state, &host);
        if let Some(selected) = self.tray {
            if entries.is_empty() {
                self.tray = None;
            } else if selected >= entries.len() {
                self.tray = Some(entries.len() - 1);
            }
        }
        let mut queued_at: Vec<(usize, usize, feed::Queued)> = Vec::new();
        if !entries.is_empty() {
            let latest = entries.len().saturating_sub(QUEUED_SHOWN);
            let mut from = self.queued_from.min(latest);
            match self.tray {
                Some(i) if i < from => from = i,
                Some(i) if i >= from + QUEUED_SHOWN => from = i + 1 - QUEUED_SHOWN,
                Some(_) => {}
                None => from = latest,
            }
            self.queued_from = from;
            let more = entries.len().saturating_sub(QUEUED_SHOWN);
            let more_line = || {
                Line::from(Span::styled(
                    format!("{}+{more} more queued", " ".repeat(feed::WORDS)),
                    theme.faint(),
                ))
            };
            // Earlier ones hidden: the line goes above; only later ones
            // hidden, it goes below.
            let more_above = more > 0 && from > 0;
            if more_above {
                bottom.push(more_line());
            }
            for (i, entry) in entries.iter().enumerate().skip(from).take(QUEUED_SHOWN) {
                let lit = self.tray == Some(i) || self.queued_hover == Some(i);
                let drawn = feed::queued_line(entry, lit, width, theme);
                bottom.push(drawn.line.clone());
                queued_at.push((bottom.len() - 1, i, drawn));
            }
            if more > 0 && !more_above {
                bottom.push(more_line());
            }
        }
        // The row rests on the composer's box; a blank line sets it apart
        // from whatever is above. It is the pane folded: while the pane is
        // open it takes the row's place.
        let running = overview.jobs.len();
        if !side
            && let Some(line) = edge_row(
                &overview,
                running,
                !self.editor.is_empty(),
                now_ms,
                width,
                theme,
            )
        {
            if !bottom.is_empty() {
                bottom.push(Line::default());
            }
            row_at = Some((bottom.len(), text::line_width(&line)));
            bottom.push(line);
        }

        let mut cursor = None;
        let card = self.card(state);
        match &card {
            Some(card)
                if self
                    .ask_drawn
                    .as_ref()
                    .is_none_or(|(key, _)| *key != card.key) =>
            {
                self.ask_drawn = Some((card.key.clone(), now_ms));
            }
            Some(_) => {}
            None => self.ask_drawn = None,
        }
        // A plan stays in the feed, as the agent's text, while the box asks.
        self.asking = card
            .as_ref()
            .filter(|card| ask::boxed(card) && !matches!(card.body, AskBody::Plan { .. }))
            .map(|card| card.item_key.clone());
        // A plan arriving while the reader follows opens at its first line,
        // to be read from the top; one that fits leaves the feed following.
        let plan_arrived = match card.as_ref() {
            Some(card)
                if matches!(card.body, AskBody::Plan { .. })
                    && card.state == CardState::Open
                    && self.plan_seen.as_ref() != Some(&card.item_key) =>
            {
                self.plan_seen = Some(card.item_key.clone());
                if self.anchor == Anchor::Bottom {
                    self.anchor = Anchor::Top {
                        key: card.item_key.clone(),
                        offset: 0,
                    };
                    self.revealed = true;
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        // Where the composer's box sits among the bottom lines, once drawn.
        let mut boxed_at: Option<(usize, Boxed)> = None;
        let mut composer_box = |bottom: &mut Vec<Line<'static>>,
                                cursor: &mut Option<(usize, usize)>,
                                editor: &Editor,
                                takes_keys: bool| {
            // Kept from sending, or exited, the box's edge says why; the
            // empty field still invites the draft.
            let invite = format!("Message {name}");
            let boxed = boxed_composer(editor, &invite, &summary, !pane_keys, width, theme);
            if takes_keys {
                *cursor = Some((bottom.len() + boxed.cursor.0, boxed.cursor.1));
            }
            let at = bottom.len();
            bottom.extend(boxed.lines.iter().cloned());
            boxed_at = Some((at, boxed));
        };
        let legend = || {
            if pane_keys {
                let stoppable = matches!(
                    &self.pane_focus,
                    Some(pane::PaneItem::Job(job)) if pane::stop_key(job).is_some()
                );
                Hint::Keys(if stoppable {
                    keys(&[
                        ("j/k", "move"),
                        ("enter", "open"),
                        ("x", "stop"),
                        ("esc", "back"),
                        ("ctrl+o", "close"),
                    ])
                } else {
                    keys(&[
                        ("j/k", "move"),
                        ("enter", "open"),
                        ("esc", "back"),
                        ("ctrl+o", "close"),
                    ])
                })
            } else {
                turn_hint(state, &self.editor, self.reach, self.leader)
            }
        };
        // The app's footer, when it has one, takes the line over.
        let mut hint;
        // Where the plain composer's box starts, to mark its top edge.
        let mut plain_box: Option<usize> = None;
        // A permission ask takes over the composer's box: its edge in the
        // attention ink, the draft kept behind it.
        let mut ask_box: Option<(usize, Vec<ask::Spot>)> = None;
        let mut ask_mode: Option<(usize, (usize, usize))> = None;
        match &card {
            Some(card) if ask::boxed(card) => {
                const MARGIN: usize = 2;
                let inner = width.saturating_sub(2 * MARGIN + 4).max(1);
                self.ask.set_room(usize::from(area.height), self.attach);
                let drawn = self.ask.box_lines(card, inner, theme);
                let (lines, mode, _) = framed(
                    drawn.lines,
                    theme.attention(),
                    &ROUNDED,
                    &summary,
                    width,
                    theme,
                );
                let at = bottom.len();
                if let Some((row, col)) = drawn.cursor {
                    cursor = Some((at + 1 + row, MARGIN + 2 + col));
                }
                if let Some(cols) = mode {
                    ask_mode = Some((at + lines.len() - 1, cols));
                }
                ask_box = Some((at, drawn.spots));
                bottom.extend(lines);
                // In a subagent's chat Ctrl+X stops the subagent, where it
                // can be stopped; it has no turn of its own to interrupt.
                let stop: &[(&str, &str)] = match state.composer() {
                    Composer::Native { stop } if stop.key().is_some() => &[("ctrl+x", "stop")],
                    Composer::Native { .. } => &[],
                    _ if card.stops_turn => &[("ctrl+x", "interrupt")],
                    _ => &[],
                };
                let more = format!("ctrl+{}", self.leader);
                let more = &[(more.as_str(), "more")];
                hint = Hint::Keys(match self.ask.question_hint(card, self.leader) {
                    Some(question) => question,
                    None if self.ask.in_box_note(card) => {
                        keys(&[&[("enter", "send"), ("esc", "clear")], stop].concat())
                    }
                    None if !matches!(card.state, CardState::Open | CardState::Rejected(_)) => {
                        keys(&[stop, more].concat())
                    }
                    None if self.ask.on_noted_deny(card) => {
                        keys(&[&[("enter", "choose"), ("tab", "note")], stop, more].concat())
                    }
                    None => keys(&[&[("enter", "choose")], stop, more].concat()),
                });
            }
            _ if let Composer::Native { stop } = state.composer() => {
                bottom.extend(native_box(
                    &state.provider_name(),
                    &stop,
                    &summary,
                    width,
                    theme,
                ));
                hint = native_hint(&stop, self.leader);
            }
            _ if state.composer() == Composer::Ended => {
                bottom.extend(ended_box(&state.provider_name(), &summary, width, theme));
                let more = format!("ctrl+{}", self.leader);
                hint = Hint::Keys(keys(&[(more.as_str(), "more")]));
            }
            _ => {
                plain_box = Some(bottom.len());
                composer_box(
                    &mut bottom,
                    &mut cursor,
                    &self.editor,
                    self.tray.is_none() && self.picker.is_none(),
                );
                hint = match self.tray.and_then(|i| entries.get(i)) {
                    Some(entry) => Hint::Keys(keys(entry.hint())),
                    None => legend(),
                };
            }
        }
        // Ctrl+S: its legend, at once; a picker: its keys.
        if let Some(picker) = &self.picker {
            hint = Hint::Keys(keys(&picker.hints()));
            if footer.is_none() {
                cursor = None;
            }
        } else if self.setting_prefix {
            let view = ui_view::settings(state);
            let mut pairs = Vec::new();
            for (setting, pair) in [
                (&view.changeable.model, ("m", "model")),
                (&view.changeable.effort, ("e", "effort")),
                (&view.changeable.permission, ("p", "permission")),
            ] {
                if *setting == Changeable::Pick {
                    pairs.push(pair);
                }
            }
            hint = if pairs.is_empty() {
                Hint::Sentence(
                    crate::words::by_typing_words(&view.changeable.model, &view.changeable.effort)
                        .unwrap_or_else(|| "Nothing here can change from amux".to_owned()),
                )
            } else {
                pairs.push(("esc", "back"));
                Hint::Keys(keys(&pairs))
            };
        }
        let offered = if plain_box.is_some() {
            self.offered_commands(state)
        } else {
            Vec::new()
        };
        let lit_command = self.menu.selected(self.editor.text(), &offered);
        if let Some(lit) = offered.get(lit_command) {
            hint = Hint::Keys(keys(&commands::hints(&lit.command)));
        }
        if let Some(selecting) = self.selection_hint(state) {
            hint = selecting;
        }
        // The plain composer's top edge says what stands in the way of
        // sending, most pressing first: the last prompt refused (error
        // ink), Claude's own view on another conversation (warning ink),
        // the link to the host down, an exited agent that Enter resumes, a
        // usage limit reached (warning ink; sending stays open).
        if let Some(at) = plain_box {
            let words = edge_title(
                self.not_sent.as_deref(),
                ui_view::switched_away(state),
                &state.composer(),
                &host,
                self.reach,
                composer::limit_reached(&overview, now_ms),
                theme,
            );
            if let Some((words, ink)) = words
                && let Some(top) = bottom.get_mut(at)
            {
                let edge = if unguarded(&summary) {
                    theme.warning()
                } else {
                    theme.muted()
                };
                *top = marked_edge_in(&words, ink, edge, box_glyphs(&summary), width);
            }
        }
        // A blank line lets the composer's box breathe above the keys.
        bottom.push(Line::default());
        let (hint, hint_keys) = match footer {
            Some(footer) => (footer, Vec::new()),
            None => hint_line(&hint, width, theme),
        };
        let hint_row = bottom.len();
        bottom.push(hint);

        let height = usize::from(area.height);
        let feed_height = height.saturating_sub(top.len() + bottom.len());
        // Wrapped again at another width, a selection's lines are other
        // lines.
        if self.feed.0 != feed_width {
            self.clear_selection();
            self.press = None;
            self.edge = None;
        }
        self.feed = (feed_width, feed_height);
        self.feed_origin = (area.x, area.y + top.len() as u16);
        let feed_top = top.len();
        let mut lines = top;
        // The feed lines a thumbnail may be drawn on: the pinned prompt and
        // the jump control sit over the rest.
        let mut thumb_lines = 0..feed_height;
        self.pin_spot = None;
        self.jump_spot = None;
        self.panel_spots.clear();
        if state.transcript().is_empty() {
            lines.extend(empty_feed(
                state,
                self.reach,
                feed_height,
                now_ms - self.opened_at_ms,
                width,
                theme,
            ));
            self.laid = Laid::default();
        } else {
            // A run grown since the last frame is held by its newest step
            // again, so trimming its oldest steps never folds it.
            ui_view::keep_open_runs(state, &mut self.open_runs);
            let mut laid = self.frame(theme).layout(state);
            // A step revealed within the last screenful cannot reach the
            // top; the feed simply shows the newest row.
            if std::mem::take(&mut self.revealed) && laid.at_bottom {
                self.anchor = Anchor::Bottom;
            }
            // Held at a line from which the rest no longer fills the feed
            // (a fold just closed, say), the feed shows the newest row: it
            // follows again.
            if laid.at_bottom {
                self.anchor = Anchor::Bottom;
            }
            // The pinned prompt would cover the plan's first lines, or the
            // lines a page down brought to the top: they start just below
            // it instead.
            if (plan_arrived || std::mem::take(&mut self.paged))
                && !laid.at_bottom
                && let Some((pinned, _)) = pinned(state, &laid, feed_height, feed_width, theme)
            {
                self.anchor = self
                    .frame(theme)
                    .scrolled(state, &laid, -(pinned.len() as isize));
                laid = self.frame(theme).layout(state);
            }
            let mut feed: Vec<Line<'static>> = laid.lines.clone();
            text::drop_cut_padding(&mut feed);
            if let Some(Selection::Feed(selection)) = &self.selection {
                for (row, place) in line_places(&laid, feed.len()).into_iter().enumerate() {
                    let Some((block, line)) = place else {
                        continue;
                    };
                    let block = &laid.blocks[block];
                    if let Some((from, to)) = selection.columns(block.order, line, block.text[line])
                    {
                        text::highlight(&mut feed[row], from, to, theme.selection());
                    }
                }
            }
            if let Some((pinned, key)) = pinned(state, &laid, feed_height, feed_width, theme) {
                let rows = pinned.len();
                thumb_lines.start = rows;
                // The blank line under the block is not the pin's to click.
                let block_rows = rows.saturating_sub(1);
                for (at, line) in pinned.into_iter().enumerate() {
                    if let Some(slot) = feed.get_mut(at) {
                        *slot = line;
                    }
                }
                let y = area.y + feed_top as u16;
                self.pin_spot = Some(((y, y + block_rows as u16), key));
                // The line under the pin is the feed's new edge: a cut
                // block's padding there would read as a stray band.
                if let Some(rest) = feed.get_mut(rows..) {
                    text::drop_cut_padding(rest);
                }
            }
            // The lit job's step, when the feed shows it, carries the same
            // card, so the pane's line and the feed's line read as one.
            if let Some(pane::PaneItem::Job(job)) = lit.as_ref()
                && let Some(block) = laid
                    .blocks
                    .iter()
                    .position(|block| job.step.as_ref() == Some(&block.key))
            {
                let owners = line_owners(&laid, feed.len());
                // Exactly the step's own lines, flat: a block may end in
                // the blank line that sets it apart, which is not the step's.
                let rows: Vec<usize> = owners
                    .iter()
                    .enumerate()
                    .filter(|(at, owner)| {
                        **owner == Some(block) && text::line_width(&feed[*at]) > 0
                    })
                    .map(|(at, _)| at)
                    .collect();
                if let (Some(first), Some(last)) = (rows.first(), rows.last()) {
                    pane::card_highlight(
                        &mut feed,
                        *first..*last + 1,
                        2,
                        width.saturating_sub(2),
                        false,
                        theme,
                    );
                }
            }
            if self.anchor != Anchor::Bottom
                && let Some(last) = feed.len().checked_sub(1)
            {
                // The control sits in the blank line that sets the feed
                // apart from what is below it, never over history.
                let (control, from) = jump_control(state, width, theme);
                let control_width = text::line_width(&control);
                feed[last] = text::overlay(&Line::default(), from, control, width);
                thumb_lines.end = last;
                text::drop_cut_padding(&mut feed[..last]);
                self.jump_spot = Some((
                    area.y + (feed_top + last) as u16,
                    (area.x + from as u16, area.x + (from + control_width) as u16),
                ));
            }
            lines.extend(feed);
            self.laid = laid;
        }
        let bottom_start = lines.len();
        let bottom_y = |row: usize| area.y + (bottom_start + row) as u16;
        self.hint_spots = hint_keys
            .into_iter()
            .map(|((from, to), key)| {
                (
                    bottom_y(hint_row),
                    (area.x + from as u16, area.x + to as u16),
                    key,
                )
            })
            .collect();
        self.queued_spots = queued_at
            .into_iter()
            .map(|(at, index, drawn)| QueuedSpot {
                row: bottom_y(at),
                index,
                controls: drawn
                    .controls
                    .iter()
                    .map(|(from, to)| (area.x + *from as u16, area.x + *to as u16))
                    .collect(),
            })
            .collect();
        self.row_spot = match row_at {
            Some((at, row_width)) if !self.pane_open => {
                Some((bottom_y(at), (area.x + 2, area.x + row_width as u16)))
            }
            _ => None,
        };
        self.pane_spots.clear();
        self.pane_rect = None;
        self.mode_spot = None;
        self.composer_spot = None;
        self.ask_spots = match ask_box {
            Some((at, spots)) => spots
                .into_iter()
                .map(|(row, (from, to), spot)| {
                    (
                        bottom_y(at + 1 + row),
                        (area.x + (4 + from) as u16, area.x + (4 + to) as u16),
                        spot,
                    )
                })
                .collect(),
            None => Vec::new(),
        };
        if let Some((row, (from, to))) = ask_mode {
            self.mode_spot = Some((bottom_y(row), (area.x + from as u16, area.x + to as u16)));
        }
        if let Some((at, boxed)) = &boxed_at {
            if let Some((from, to)) = boxed.mode {
                self.mode_spot = Some((
                    bottom_y(at + boxed.lines.len() - 1),
                    (area.x + from as u16, area.x + to as u16),
                ));
            }
            self.composer_spot = Some(ComposerSpot {
                x: area.x + boxed.text_x as u16,
                y: bottom_y(at + 1),
                rows: boxed.lines.len().saturating_sub(2) as u16,
                skip: boxed.skip,
                wrap: boxed.wrap,
            });
        }
        lines.extend(bottom);
        paint.render_widget(Paragraph::new(lines), area);
        let feed_rect = Rect {
            x: area.x,
            y: area.y + feed_top as u16,
            width: feed_width as u16,
            height: feed_height as u16,
        };
        let covered = self.covered
            || self.panel.is_some()
            || self.picker.is_some()
            || !offered.is_empty()
            || self.stop_ask.is_some();
        self.thumbs.draw(
            paint,
            feed_rect,
            &self.laid.hits,
            thumb_lines,
            |hash| state.blob(hash),
            covered,
            theme,
        );
        if side {
            // Beside the chat from the feed's first line down to the line
            // above the keys; over the feed alone when it lies on top.
            let x = area.x + (full - pane_width) as u16;
            let y = area.y + feed_top as u16;
            let bottom_edge = if beside {
                area.y + area.height.saturating_sub(1)
            } else {
                y + feed_height as u16
            };
            let rect = Rect {
                x,
                y,
                width: pane_width as u16,
                height: bottom_edge.saturating_sub(y),
            };
            if !beside {
                // Lying over the feed, it clears a column more than it
                // draws, so the feed's cut text never touches the rule.
                let cleared = Rect {
                    x: rect.x.saturating_sub(1),
                    width: rect.width + 1,
                    ..rect
                };
                paint.render_widget(ratatui::widgets::Clear, cleared);
            }
            self.pane_rect = Some(rect);
            self.draw_side_pane(paint, rect, &overview, lit.as_ref(), now_ms, theme);
            let body = self.pane_body.unwrap_or_default();
            let cells = ScreenCells::capture(paint.buffer_mut(), body, Vec::new());
            self.keep_screen(paint, cells, theme);
        }
        // A pane that closed takes its selection with it.
        if !side && matches!(self.selection, Some(Selection::Screen(_))) {
            self.clear_selection();
        }
        // A model or effort being chosen: its flyover rises from the model's
        // words on the composer's edge, over the box (and the pane).
        let mut flyover_cursor = None;
        if let (Some(picker), Some((at, boxed))) = (&self.picker, &boxed_at) {
            let anchor = area.x + boxed.label.unwrap_or(4) as u16;
            let above = (area.y as usize + bottom_start + at) as u16;
            self.flyover = crate::setup::draw_flyover(paint, picker, anchor, above, area, theme);
            flyover_cursor = self.flyover.cursor;
        }
        if let Some(at) = flyover_cursor {
            paint.set_cursor_position(at);
        }
        // The draft is one "/word" (or ends in a Codex "$word"): the
        // agent's commands it starts rise from the box, lined up under the
        // draft's words.
        self.menu_flyover = None;
        if let (false, Some((at, boxed))) = (offered.is_empty(), &boxed_at) {
            let all = ui_view::settings(state).commands;
            let room = usize::from(area.width).saturating_sub(4);
            let (lines, rows) = commands::panel(&all, &offered, lit_command, room, theme);
            let anchor = (area.x + boxed.text_x as u16).saturating_sub(2);
            let above = (area.y as usize + bottom_start + at) as u16;
            if let Some((rect, skip)) = crate::panel::rise(paint, lines, anchor, above, area) {
                self.menu_flyover = Some(crate::setup::Flyover {
                    rect,
                    choices: rows
                        .into_iter()
                        .enumerate()
                        .skip(skip)
                        .filter_map(|(line, at)| at.map(|at| (rect.y + (line - skip) as u16, at)))
                        .collect(),
                    cursor: None,
                });
            }
        }
        // The leader held: its keys rise from the hint line's `ctrl+a
        // more`, in a flyover like the settings', over the feed.
        if let Some(entries) = self.panel.clone() {
            let leader = KeyEvent::new(KeyCode::Char(self.leader), KeyModifiers::CONTROL);
            let (anchor, above) = self
                .hint_spots
                .iter()
                .find(|(_, _, key)| *key == leader)
                .map_or(
                    (area.x + 2, area.y + area.height.saturating_sub(1)),
                    |(y, (from, _), _)| (*from, *y),
                );
            let (lines, chords) = which_key_panel(&entries, self.leader, width, theme);
            if let Some((rect, skip)) = crate::panel::rise(paint, lines, anchor, above, area) {
                for (i, chord) in chords.into_iter().enumerate().skip(skip) {
                    if let Some(chord) = chord {
                        self.panel_spots.push((
                            rect.y + (i - skip) as u16,
                            (rect.x, rect.x + rect.width),
                            chord,
                        ));
                    }
                }
            }
        }
        // While the pane has the keys, the composer shows no cursor.
        if let Some((row, col)) = cursor.filter(|_| !pane_keys && self.picker.is_none()) {
            let y = area.y as usize + bottom_start + row;
            let x = area.x as usize + col.min(width.saturating_sub(1));
            if y < (area.y + area.height) as usize {
                paint.set_cursor_position(Position::new(x as u16, y as u16));
            }
        }
        let page = if state.transcript().is_empty() {
            state.transcript().has_older().then_some(layout::PAGE)
        } else {
            self.laid.page
        };
        match page {
            Some(n) if self.page_asked != state.oldest_order() || self.page_asked.is_none() => {
                Some(n)
            }
            _ => None,
        }
    }

    /// The pane beside the chat: a hairline rule down its left edge, then
    /// its lines past the margin, cut at the rect's foot.
    #[allow(clippy::too_many_arguments)]
    fn draw_side_pane(
        &mut self,
        paint: &mut Paint<'_>,
        rect: Rect,
        overview: &ui_view::Overview,
        lit: Option<&pane::PaneItem>,
        now_ms: i64,
        theme: Theme,
    ) {
        // The rule, a blank column, then the pane's lines out to the outer
        // margin: a highlighted item's tint spans them, its words sit two
        // columns further in, as on home.
        const LEFT: u16 = 2;
        const RIGHT: u16 = 2;
        // The title line and the blank under it stay; the rest scrolls.
        const TITLE: u16 = 2;
        let inner = rect.width.saturating_sub(LEFT + RIGHT);
        let focused = self.pane_keys;
        let content = pane::pane_lines(
            &pane::Contents {
                overview,
                folded: &self.pane_folds,
            },
            lit,
            focused,
            now_ms,
            usize::from(inner),
            theme,
        );
        // Which area has the keys shows on the frame, never the content:
        // the rule turns grey while the pane has them.
        let rule_ink = if focused {
            theme.muted()
        } else {
            theme.hairline()
        };
        let rule: Vec<Line<'static>> = (0..rect.height)
            .map(|_| Line::from(Span::styled("│", rule_ink)))
            .collect();
        paint.render_widget(
            Paragraph::new(rule),
            Rect {
                width: 1.min(rect.width),
                ..rect
            },
        );
        let x = rect.x + LEFT;
        for ((from, to), hit) in content.title_hits.iter().cloned() {
            self.pane_spots
                .push((rect.y, (x + from as u16, x + to as u16), hit));
        }
        paint.render_widget(
            Paragraph::new(vec![content.title.clone()]),
            Rect {
                x,
                width: inner,
                height: 1.min(rect.height),
                ..rect
            },
        );

        // The body: one list, scrolled so the keys' item keeps a line of
        // room from each edge when they move it.
        let height = usize::from(rect.height.saturating_sub(TITLE));
        self.pane_page = height;
        self.pane_body = Some(Rect {
            x: rect.x + LEFT,
            y: rect.y + TITLE,
            width: inner,
            height: height as u16,
        });
        let len = content.body.len();
        let max = len.saturating_sub(height);
        if self.pane_follow {
            self.pane_follow = false;
            let focus_line = self.pane_focus.as_ref().and_then(|focus| {
                content
                    .item_lines
                    .iter()
                    .find(|(item, _)| item == focus)
                    .map(|(_, line)| *line)
            });
            if let Some(line) = focus_line.filter(|_| self.pane_keys) {
                // One line of room, one more for an edge's count and, at
                // the top, one more for a pinned directory.
                const ROOM: usize = 2;
                const TOP_ROOM: usize = ROOM + 1;
                if line < self.pane_scroll + TOP_ROOM {
                    self.pane_scroll = line.saturating_sub(TOP_ROOM);
                } else if line + ROOM + 1 > self.pane_scroll + height {
                    self.pane_scroll = (line + ROOM + 1).saturating_sub(height);
                }
                // Scrolled only past a heading, the list starts at its top,
                // as long as the item still clears the bottom edge's line.
                if pane::hidden(&content, self.pane_scroll, height, 0)
                    .0
                    .is_none()
                    && line + 2 <= height
                {
                    self.pane_scroll = 0;
                }
            }
        }
        self.pane_scroll = self.pane_scroll.min(max);
        let top = self.pane_scroll;
        let (mut above, below) = pane::hidden(&content, top, height, 0);
        // Scrolled into a group, its directory stays pinned on the first
        // line under the top edge, over the row there, so nothing moves.
        let pin_row = top + usize::from(above.is_some());
        let pin = (pin_row < top + height)
            .then(|| content.pin(pin_row))
            .flatten();
        if pin.is_some() {
            above = pane::hidden(&content, top, height, 1).0.or(above);
        }
        let mut shown: Vec<Line<'static>> = content
            .body
            .iter()
            .skip(top)
            .take(height)
            .cloned()
            .collect();
        let body_y = rect.y + TITLE;
        for (row, (from, to), hit) in content.hits {
            if (top..top + height).contains(&row) {
                self.pane_spots.push((
                    body_y + (row - top) as u16,
                    (x + from as u16, x + to as u16),
                    hit,
                ));
            }
        }
        // Cut content says how much lies past each edge, on the edge's own
        // line; clicking it turns a page.
        if let Some(above) = above
            && !shown.is_empty()
        {
            shown[0] = pane::more_line(true, above, theme);
            self.pane_spots.retain(|(row, _, _)| *row != body_y);
            self.pane_spots
                .push((body_y, (x, x + inner), pane::PaneHit::Page { up: true }));
        }
        if let Some(dir) = pin
            && let Some(slot) = shown.get_mut(pin_row - top)
        {
            *slot = content.body[dir].clone();
            let row = body_y + (pin_row - top) as u16;
            self.pane_spots.retain(|(each, _, _)| *each != row);
        }
        if let Some(below) = below
            && let Some(last) = shown.len().checked_sub(1)
        {
            shown[last] = pane::more_line(false, below, theme);
            let row = body_y + last as u16;
            self.pane_spots.retain(|(each, _, _)| *each != row);
            self.pane_spots
                .push((row, (x, x + inner), pane::PaneHit::Page { up: false }));
        }
        paint.render_widget(
            Paragraph::new(shown),
            Rect {
                x,
                y: body_y,
                width: inner,
                height: height as u16,
            },
        );
    }

    /// The header. At the left the agent's name, then faint where it runs:
    /// its project, and its host when that is not this machine. At the
    /// right how much of its context is used, the working tree's `[Diff]`
    /// and `[Home]`. Where the chat stands is not repeated here, since the
    /// feed shows work and asks; only a problem is (host away,
    /// reconnecting, catching up, exited). Returns the line and the
    /// controls' column ranges.
    fn top_line(
        &self,
        state: &SessionState,
        name: &str,
        width: usize,
        theme: Theme,
    ) -> (Line<'static>, HeaderSpots) {
        let host = state
            .host()
            .map(|host| host.name().to_owned())
            .unwrap_or_default();
        // The right side, built first so the left knows its room.
        let mut right: Vec<Span<'static>> = Vec::new();
        // Groups on both sides are split by the same faint bar.
        let bar = || Span::styled(" │ ", theme.faint());
        let gap = |right: &mut Vec<Span<'static>>| {
            if !right.is_empty() {
                right.push(bar());
            }
        };
        if let Some((words, style)) = problem_words(state, self.reach, &host, theme) {
            right.push(Span::styled(words, style));
        }
        if let Some(context) = ui_view::context(state) {
            gap(&mut right);
            let used = text::tokens(context.used_tokens);
            let words = match context.window_tokens {
                Some(window) => format!("{used} / {}", text::tokens(window)),
                None => used,
            };
            let style = if context.near_full {
                theme.warning()
            } else {
                theme.faint()
            };
            right.push(Span::styled(words, style));
        }
        let diff_hovered = self.hover == Some(HeaderControl::Diff);
        let home_hovered = self.hover == Some(HeaderControl::Home);
        let home_style = if home_hovered {
            theme.emphasis()
        } else {
            theme.muted()
        };
        let mut controls = Vec::new();
        // A provider subagent works in its parent's folder, so its changes
        // are the parent's to review: it has no [Diff] of its own.
        let diff = (!state.native()).then(|| {
            (
                diff_spans(state, self.comparison, diff_hovered, theme),
                HeaderControl::Diff,
            )
        });
        for (spans, control) in diff.into_iter().chain([(
            vec![Span::styled("[Home]", home_style)],
            HeaderControl::Home,
        )]) {
            gap(&mut right);
            let columns = spans
                .iter()
                .map(|span| text::str_width(&span.content))
                .sum();
            // A control may be several spans; its columns start at the first.
            controls.push((right.len(), columns, control));
            right.extend(spans);
        }
        let right_width: usize = right
            .iter()
            .map(|span| text::str_width(&span.content))
            .sum();
        let end = width.saturating_sub(2);
        let right_at = end.saturating_sub(right_width);

        let mut line = Line::from(Span::raw("  "));
        let room = right_at.saturating_sub(2);
        push(&mut line, name, theme.bright(), room);
        // Where it works: its branch and its path as the person would write
        // it (`main ~/source/amux`), and its host when that is not this
        // machine.
        let cwd = &state.agent().cwd;
        let mut place = if self.local {
            text::tilde(cwd)
        } else {
            cwd.clone()
        };
        if let Some(branch) = state
            .agent()
            .git
            .as_ref()
            .and_then(|git| git.branch.as_deref())
        {
            place = format!("{branch} {place}");
        }
        if !self.local && !host.is_empty() {
            place = format!("{place} · {host}");
        }
        if !place.is_empty() && text::line_width(&line) + 6 < room {
            line.spans.push(bar());
            push(&mut line, place, theme.faint(), room);
        }
        let mut spots = Vec::new();
        if text::line_width(&line) + 2 <= right_at {
            text::pad_to(&mut line, right_at);
            let mut at = right_at;
            for (i, span) in right.into_iter().enumerate() {
                if let Some((_, span_width, control)) =
                    controls.iter().find(|(index, _, _)| *index == i)
                {
                    spots.push(((at, at + span_width), *control));
                }
                at += text::str_width(&span.content);
                line.spans.push(span);
            }
        }
        (line, spots)
    }
}

/// The row's change totals for `comparison`, as of the last turn end.
pub(super) fn change_totals(
    state: &SessionState,
    comparison: Comparison,
) -> Option<wire::ChangeTotals> {
    let git = state.agent().git.as_ref()?;
    match comparison {
        Comparison::Uncommitted => git.uncommitted,
        Comparison::OnBranch => git.on_branch,
    }
}

/// The header's way to the review page, with the change totals of the
/// comparison shown: "[Diff +42 −7]" for uncommitted changes, "[Diff vs
/// main +120 −30]" for everything on the branch, a bare "[Diff]" with
/// nothing to count. Each count wears what it counts, unless hovered, when
/// the whole control lights as one.
fn diff_spans(
    state: &SessionState,
    comparison: Comparison,
    hovered: bool,
    theme: Theme,
) -> Vec<Span<'static>> {
    let against = match comparison {
        Comparison::Uncommitted => None,
        Comparison::OnBranch => state
            .agent()
            .git
            .as_ref()
            .and_then(|git| git.base_branch.as_deref()),
    };
    let control = if hovered {
        theme.emphasis()
    } else {
        theme.muted()
    };
    let mut spans = Vec::new();
    let mut words = "[Diff".to_owned();
    if let Some(base) = against {
        words.push_str(&format!(" vs {base}"));
    }
    if let Some(totals) = change_totals(state, comparison)
        .filter(|totals| totals.added + totals.removed > 0 || totals.files > 0)
    {
        let count = |words: String, changed: u64, ink: ratatui::style::Style| {
            let ink = if hovered {
                control
            } else if changed > 0 {
                ink
            } else {
                theme.faint()
            };
            Span::styled(words, ink)
        };
        spans.push(Span::styled(words, control));
        spans.push(count(
            format!(" +{}", totals.added),
            totals.added.into(),
            theme.ok(),
        ));
        spans.push(count(
            format!(" −{}", totals.removed),
            totals.removed.into(),
            theme.error(),
        ));
        words = String::new();
    }
    words.push(']');
    spans.push(Span::styled(words, control));
    spans
}

/// What the composer's bottom edge says, in the words a person reads: the
/// model by its name, its effort, and the permission and mode.
/// The composer in its box, as drawn: its lines, the cursor's (line,
/// column), the mode's columns on the bottom edge, where the draft's words
/// start, how many wrapped lines are scrolled off above, and the width the
/// draft wraps at.
pub(super) struct Boxed {
    pub(super) lines: Vec<Line<'static>>,
    cursor: (usize, usize),
    mode: Option<(usize, usize)>,
    /// Where the model's words start on the bottom edge: a model or effort
    /// flyover rises from there.
    label: Option<usize>,
    text_x: usize,
    skip: usize,
    wrap: usize,
}

/// The composer in its box, the model, effort and mode on the bottom edge.
pub(super) fn boxed_composer(
    editor: &Editor,
    placeholder: &str,
    controls: &ui_view::ControlsSummary,
    focused: bool,
    width: usize,
    theme: Theme,
) -> Boxed {
    const MARGIN: usize = 2;
    let inner = width.saturating_sub(2 * MARGIN + 4).max(1);
    let (mut body, (row, col)) = editor_lines(editor, placeholder, inner, theme);
    // The box already says where to type, so the edge mark becomes a prompt.
    for (i, line) in body.iter_mut().enumerate() {
        if let Some(first) = line.spans.first_mut() {
            *first = Span::styled(if i == 0 { "› " } else { "  " }, theme.muted());
        }
    }
    let skip = (row + 1).saturating_sub(COMPOSER_LINES);
    // Holding the keys, the box's edge is grey; otherwise it recedes to a
    // hairline. An agent that acts without asking keeps it in the warning ink
    // either way: that is true of the chat, not of where the keys are. The
    // words on its edge keep their own inks. Its line is heavy as well (see
    // `box_glyphs`).
    let edge = if unguarded(controls) {
        theme.warning()
    } else if focused {
        theme.muted()
    } else {
        theme.hairline()
    };
    let body = body.into_iter().skip(skip).take(COMPOSER_LINES).collect();
    let glyphs = box_glyphs(controls);
    let (lines, mode, label) = framed(body, edge, glyphs, controls, width, theme);
    Boxed {
        lines,
        cursor: (1 + row - skip, MARGIN + 2 + col),
        mode,
        label,
        text_x: MARGIN + 4,
        skip,
        wrap: inner,
    }
}

/// The characters of the composer's box.
pub(super) struct BoxGlyphs {
    top_left: &'static str,
    top_right: &'static str,
    bottom_left: &'static str,
    bottom_right: &'static str,
    across: &'static str,
    down: &'static str,
}

pub(super) const ROUNDED: BoxGlyphs = BoxGlyphs {
    top_left: "╭",
    top_right: "╮",
    bottom_left: "╰",
    bottom_right: "╯",
    across: "─",
    down: "│",
};

const HEAVY: BoxGlyphs = BoxGlyphs {
    top_left: "┏",
    top_right: "┓",
    bottom_left: "┗",
    bottom_right: "┛",
    across: "━",
    down: "┃",
};

/// The box for this agent: heavy when it acts without asking, because the
/// warning ink is the terminal's yellow and some schemes' yellow is pale
/// enough to pass for the ordinary edge. The shape says it in any scheme.
fn box_glyphs(controls: &ui_view::ControlsSummary) -> &'static BoxGlyphs {
    if unguarded(controls) {
        &HEAVY
    } else {
        &ROUNDED
    }
}

/// Whether this agent acts without asking first.
fn unguarded(controls: &ui_view::ControlsSummary) -> bool {
    controls
        .permission
        .as_ref()
        .is_some_and(|permission| permission.never_asks)
}

/// The composer box's top edge with `words` on it in `ink`, after a short
/// run of the edge.
pub(super) fn marked_edge_in(
    words: &str,
    ink: ratatui::style::Style,
    edge: ratatui::style::Style,
    glyphs: &BoxGlyphs,
    width: usize,
) -> Line<'static> {
    const MARGIN: usize = 2;
    let span = width.saturating_sub(2 * MARGIN + 2);
    let mut top = Line::from(Span::raw(" ".repeat(MARGIN)));
    push(
        &mut top,
        format!("{}{} ", glyphs.top_left, glyphs.across),
        edge,
        width,
    );
    push(&mut top, words, ink, width);
    push(&mut top, " ", edge, width);
    let used = text::line_width(&top) - MARGIN - 1;
    push(
        &mut top,
        glyphs.across.repeat(span.saturating_sub(used)),
        edge,
        width,
    );
    push(&mut top, glyphs.top_right, edge, width);
    top
}

/// `body` in the composer's box at the margin, its edge in `edge`, with
/// the model, effort and mode on the bottom edge. Returns the lines, the
/// mode's columns on the last line, for clicks, and where the edge's words
/// start.
pub(super) fn framed(
    body: Vec<Line<'static>>,
    edge: ratatui::style::Style,
    glyphs: &BoxGlyphs,
    controls: &ui_view::ControlsSummary,
    width: usize,
    theme: Theme,
) -> (Vec<Line<'static>>, Option<(usize, usize)>, Option<usize>) {
    const MARGIN: usize = 2;
    let span = width.saturating_sub(2 * MARGIN + 2);
    let mut out = Vec::new();
    let mut top = Line::from(Span::raw(" ".repeat(MARGIN)));
    push(&mut top, glyphs.top_left, edge, width);
    push(&mut top, glyphs.across.repeat(span), edge, width);
    push(&mut top, glyphs.top_right, edge, width);
    out.push(top);
    for line in body {
        let mut boxed = Line::from(Span::raw(" ".repeat(MARGIN)));
        push(&mut boxed, format!("{} ", glyphs.down), edge, width);
        boxed.spans.extend(line.spans);
        text::pad_to(&mut boxed, width - MARGIN - 1);
        push(&mut boxed, glyphs.down, edge, width);
        out.push(boxed);
    }
    // "Opus (high) · accept edits": the model faint by its name, its
    // effort beside it, and the permission and mode, which Shift+Tab (or a
    // click) changes, in the reading ink; normal ones are left unsaid.
    let known = |fact: &Option<String>| fact.clone().filter(|fact| !fact.is_empty());
    let mut label: Vec<Span<'static>> = Vec::new();
    match (known(&controls.model), known(&controls.effort)) {
        (Some(model), Some(effort)) => {
            label.push(Span::styled(format!("{model} ({effort})"), theme.faint()))
        }
        (Some(model), None) => label.push(Span::styled(model, theme.faint())),
        (None, Some(effort)) => label.push(Span::styled(format!("({effort})"), theme.faint())),
        (None, None) => {}
    }
    let mode_words = crate::words::control_words(controls).filter(|words| !words.is_empty());
    if let Some(mode) = &mode_words {
        if !label.is_empty() {
            label.push(Span::styled(" · ", theme.faint()));
        }
        // A permission under which the agent acts without asking is the one
        // setting worth noticing at a glance.
        let unguarded = unguarded(controls);
        match mode.split_once(" · ").filter(|_| unguarded) {
            // The permission leads; a mode after it is not the risk.
            Some((permission, rest)) => {
                label.push(Span::styled(permission.to_owned(), theme.warning()));
                label.push(Span::styled(format!(" · {rest}"), theme.text()));
            }
            None if unguarded => label.push(Span::styled(mode.clone(), theme.warning())),
            None => label.push(Span::styled(mode.clone(), theme.text())),
        }
    }
    let label_width: usize = label
        .iter()
        .map(|part| text::str_width(&part.content))
        .sum();
    let mut bottom = Line::from(Span::raw(" ".repeat(MARGIN)));
    push(&mut bottom, glyphs.bottom_left, edge, width);
    let mut mode = None;
    let mut label_at = None;
    if label.is_empty() || label_width + 6 > span {
        push(&mut bottom, glyphs.across.repeat(span), edge, width);
    } else {
        let rule = span.saturating_sub(label_width + 3).max(1);
        push(&mut bottom, glyphs.across.repeat(rule), edge, width);
        push(&mut bottom, " ", edge, width);
        label_at = Some(text::line_width(&bottom));
        bottom.spans.extend(label);
        if let Some(words) = &mode_words {
            let to = text::line_width(&bottom);
            mode = Some((to - text::str_width(words), to));
        }
        push(&mut bottom, format!(" {}", glyphs.across), edge, width);
    }
    text::pad_to(&mut bottom, width - MARGIN - 1);
    push(&mut bottom, glyphs.bottom_right, edge, width);
    out.push(bottom);
    (out, mode, label_at)
}

/// Where each drawn key pair sits on its line, and the key it presses.
type KeySpots = Vec<((usize, usize), KeyEvent)>;

/// The hint line in home's style (see [`text::push_hints`]); a sentence
/// reads faint. Returns the line and, for each pair drawn that names a
/// key, its columns and the key, so a click can press it.
pub(super) fn hint_line(hint: &Hint, width: usize, theme: Theme) -> (Line<'static>, KeySpots) {
    let mut line = Line::from(Span::raw("  "));
    let pairs = match hint {
        Hint::Sentence(words) => {
            push(&mut line, words, theme.faint(), width);
            return (line, Vec::new());
        }
        Hint::Noted(words, pairs) => {
            push(&mut line, format!("{words}   "), theme.faint(), width);
            pairs
        }
        Hint::Keys(pairs) => pairs,
    };
    let borrowed: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(key, action)| (key.as_str(), action.as_str()))
        .collect();
    let room = width.saturating_sub(text::line_width(&line));
    let spots = text::push_hints(&mut line, &borrowed, room, theme)
        .into_iter()
        .filter_map(|(at, columns)| Some((columns, key_event(borrowed[at].0)?)))
        .collect();
    (line, spots)
}

/// The prompt to pin under the header, and its key: the prompt of the turn
/// that owns the feed's first line under the pin. Best effort: nothing when
/// that turn's prompt is not held (older than the window; a page will bring
/// it), when the line belongs to no turn, or when the feed is too short to
/// spare the rows. Faint while the next turn's prompt is about to take the
/// pin.
fn pinned(
    state: &SessionState,
    laid: &Laid,
    height: usize,
    width: usize,
    theme: Theme,
) -> Option<(Vec<Line<'static>>, Key)> {
    // The pin's one line and the blank line under it.
    const ROWS: usize = 2;
    // The next prompt within this many lines of the pin fades it.
    const FADE: usize = 3;
    if height < ROWS + 6 {
        return None;
    }
    let owners = line_owners(laid, height);
    let owner = (*owners.get(ROWS)?)?;
    let order = laid.blocks.get(owner)?.order;
    let transcript = state.transcript();
    let oldest = transcript.oldest_held()?;
    let mut prompt = None;
    for held in transcript.range(oldest..=order).rev() {
        match held.class {
            ui_state::ItemClass::Prompt => {
                prompt = Some(held.item.key.clone());
                break;
            }
            // Another turn's end: the line belongs to no turn.
            ui_state::ItemClass::Turn if held.item.order != order => return None,
            _ => {}
        }
    }
    let key = prompt?;
    // A prompt whose first line of words is still on screen is its own
    // landmark: pinning it too would draw it over itself. Only the block's
    // top padding line may have scrolled off.
    if let Some(first) = owners
        .iter()
        .position(|block| block.is_some_and(|block| laid.blocks[block].key == key))
        && (first > 0 || laid.top_offset <= 1)
    {
        return None;
    }
    let row = chat_rows_for(state, std::slice::from_ref(&key), ToolRows::ShowAll).pop()?;
    let next = owners
        .iter()
        .enumerate()
        .skip(ROWS + 1)
        .find(|(_, block)| {
            block.is_some_and(|block| {
                block != owner && matches!(laid.blocks[block].row.kind, RowKind::Prompt { .. })
            })
        })
        .map(|(at, _)| at);
    let faint = next.is_some_and(|at| at - ROWS <= FADE);
    let mut lines = feed::pinned_prompt(&row, faint, width, theme);
    if lines.is_empty() {
        return None;
    }
    lines.push(Line::default());
    Some((lines, key))
}

/// Which drawn block each of the feed's `height` lines belongs to, top
/// first; `None` for the blank lines that pad a short chat.
fn line_owners(laid: &Laid, height: usize) -> Vec<Option<usize>> {
    line_places(laid, height)
        .into_iter()
        .map(|place| place.map(|(block, _)| block))
        .collect()
}

/// For each of the feed's `height` lines, the block drawn there and which
/// of its lines; None over the padding above a short chat and the live end
/// below the newest row.
pub(super) fn line_places(laid: &Laid, height: usize) -> Vec<Option<(usize, usize)>> {
    let mut places = vec![None; laid.pad];
    places.extend(
        laid.blocks
            .iter()
            .enumerate()
            .flat_map(|(i, block)| (0..block.lines.len()).map(move |line| Some((i, line))))
            .skip(laid.top_offset),
    );
    places.resize(height, None);
    places
}

/// The control that returns to the newest row while the reader is in
/// history, centred: "↓ Jump to Bottom  ctrl+end", or how many rows arrived
/// meanwhile. Returns it and its first column.
fn jump_control(state: &SessionState, width: usize, theme: Theme) -> (Line<'static>, usize) {
    let surface = ratatui::style::Style {
        bg: theme.user_surface().bg,
        ..Default::default()
    };
    let arrived = state.held_arrivals();
    let words = if arrived > 0 {
        format!("↓ {arrived} new")
    } else if state.arrivals_held() {
        "↓ New Activity".to_owned()
    } else {
        "↓ Jump to Bottom".to_owned()
    };
    let line = Line::from(vec![
        Span::styled(format!(" {words}  "), theme.text().patch(surface)),
        Span::styled("ctrl+end ", theme.faint().patch(surface)),
    ]);
    let from = width.saturating_sub(text::line_width(&line)) / 2;
    (line, from)
}

/// The leader's flyover: every next key and what it does, in groups (what
/// is about amux, then what is about this chat), with each line's chord for
/// clicks. A legend, not a list to move through: no pointer, no highlight;
/// the key bright, its words faint with the key's letter picked out where
/// the word holds it.
fn which_key_panel(
    entries: &[PanelEntry],
    leader: char,
    width: usize,
    theme: Theme,
) -> (Vec<Line<'static>>, Vec<Option<char>>) {
    let key_width = entries
        .iter()
        .map(|entry| text::str_width(&entry.key))
        .max()
        .unwrap_or(1);
    let mut rows = Vec::new();
    let mut chords = Vec::new();
    let mut group = "";
    for entry in entries {
        if entry.group != group {
            if !group.is_empty() {
                rows.push(Line::default());
                chords.push(None);
            }
            group = entry.group;
            rows.push(Line::from(Span::styled(group.to_owned(), theme.faint())));
            chords.push(None);
        }
        let pad = key_width.saturating_sub(text::str_width(&entry.key));
        let mut line = Line::from(vec![
            Span::raw("  "),
            Span::styled(entry.key.clone(), theme.emphasis()),
            Span::raw(" ".repeat(pad + 2)),
        ]);
        let letter = entry
            .key
            .chars()
            .next()
            .filter(|_| entry.key.chars().count() == 1);
        match letter {
            Some(letter) => line
                .spans
                .extend(crate::panel::mnemonic(&entry.label, letter, theme)),
            None => line
                .spans
                .push(Span::styled(entry.label.clone(), theme.faint())),
        }
        rows.push(line);
        chords.push(Some(entry.chord));
    }
    let title = format!("ctrl+{leader}");
    let inner = crate::panel::content_width(&rows)
        .max(text::str_width(&title) + 3)
        .min(width.saturating_sub(8).max(8));
    let lines = crate::panel::bordered(&title, rows, inner, &[], None, theme);
    let chords = std::iter::once(None).chain(chords).chain([None]).collect();
    (lines, chords)
}

/// The type of a pasted file that is not a native image, from its name.
/// "↑ planner · 3 subagents · 1 needs you"
fn family_line(family: &FamilyHeader, width: usize, theme: Theme) -> Line<'static> {
    let mut parts = Vec::new();
    if let Some(parent) = &family.parent {
        parts.push(format!("↑ {}", parent.name));
    }
    let count = family.children.len();
    if count > 0 {
        parts.push(format!(
            "{count} subagent{}",
            if count == 1 { "" } else { "s" }
        ));
    }
    let waiting = family.need_you;
    let mut line = Line::from(Span::raw("  "));
    push(&mut line, parts.join(" · "), theme.muted(), width);
    if waiting > 0 {
        push(
            &mut line,
            format!(
                " · {waiting} need{} you",
                if waiting == 1 { "s" } else { "" }
            ),
            theme.attention(),
            width,
        );
    }
    line
}

fn empty_feed(
    state: &SessionState,
    reach: Reach,
    height: usize,
    waited_ms: i64,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::default(); height];
    let away = matches!(state.composer(), Composer::Waiting(Waiting::Detached)) || !reach.online();
    // A caught-up chat is current from its host, which loses its source
    // the moment the host does: the fleet's word on the host comes on
    // another stream and may still say away just after the host is back.
    let words = if state.has_snapshot() && state.caught_up() {
        Some("Nothing here yet.")
    } else if away {
        Some("Its computer is away, and nothing of this chat is held here yet.")
    } else if waited_ms >= LOADING_HINT_MS {
        Some("Loading…")
    } else {
        None
    };
    if let (Some(words), Some(line)) = (words, lines.get_mut(height / 2)) {
        let mut centred = Line::from(Span::raw(" ".repeat(width.saturating_sub(words.len()) / 2)));
        push(&mut centred, words, theme.muted(), width);
        *line = centred;
    }
    lines
}
