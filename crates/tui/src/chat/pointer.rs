//! Mouse, scroll, text selection and click-hit handling for an open chat.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame as Paint;
use ratatui::layout::Position;
use ui_state::{Key, SessionState};

use super::feed::FeedHit;
use super::layout::Anchor;
use super::selection::{FeedSelection, Point, ScreenCells, ScreenSelection};
use super::{
    ChatEffect, ChatView, HeaderControl, Press, Pressed, Selection, composer, line_places, pane,
    review, wheel_lines,
};
use crate::theme::Theme;
use crate::wheel::Direction;

impl ChatView {
    pub(super) fn scroll(&mut self, state: &SessionState, delta: isize, theme: Theme) {
        // Following the newest, there is nothing further down.
        if self.anchor == Anchor::Bottom && delta >= 0 {
            return;
        }
        self.anchor = self.frame(theme).scrolled(state, &self.laid, delta);
    }

    pub fn mouse(
        &mut self,
        state: &SessionState,
        event: MouseEvent,
        theme: Theme,
    ) -> Vec<ChatEffect> {
        match event.kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                self.drag(event);
                return vec![];
            }
            MouseEventKind::Up(MouseButton::Left) => return self.release(state, theme),
            _ => {}
        }
        let shown = match &mut self.round {
            Some((_, page)) => Some(page),
            None if self.review_page_open => self.review_page.as_mut(),
            None => None,
        };
        if let Some(page) = shown {
            match event.kind {
                MouseEventKind::ScrollUp => {
                    self.selection = None;
                    page.scroll_by(-wheel_lines(Direction::Up));
                }
                MouseEventKind::ScrollDown => {
                    self.selection = None;
                    page.scroll_by(wheel_lines(Direction::Down));
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    self.selection = None;
                    self.copied = false;
                    page.click(event.column, event.row);
                    let rect = page.stream_area();
                    if rect.contains(Position::new(event.column, event.row)) {
                        self.press = Some(Press::new(event, Pressed::Stream(rect)));
                    }
                }
                _ => {}
            }
            return vec![];
        }
        // The commands' flyover: a click on a command fills it in; one
        // elsewhere goes on as usual.
        if event.kind == MouseEventKind::Down(MouseButton::Left)
            && let Some(flyover) = &self.menu_flyover
            && flyover.covers(event.column, event.row)
        {
            if let Some(at) = flyover.choice_at(event.column, event.row)
                && let Some(found) = self.offered_commands(state).get(at)
            {
                let found = found.clone();
                self.fill_command(&found);
            }
            return vec![];
        }
        // A flyover is open: a click on a choice picks it, anywhere else
        // closes it.
        if self.picker.is_some() && event.kind == MouseEventKind::Down(MouseButton::Left) {
            let (x, y) = (event.column, event.row);
            let pick = match self.flyover.choice_at(x, y) {
                Some(at) => self
                    .picker
                    .as_mut()
                    .map_or(crate::setup::Pick::None, |picker| picker.click(at)),
                None if self.flyover.covers(x, y) => crate::setup::Pick::None,
                None => crate::setup::Pick::Close,
            };
            return self.picked(state, pick);
        }
        let control = self
            .header_spots
            .iter()
            .find(|(y, (from, to), _)| *y == event.row && (*from..*to).contains(&event.column))
            .map(|(_, _, control)| *control);
        match event.kind {
            MouseEventKind::Moved => {
                self.hover = control;
                self.queued_hover = self
                    .queued_spots
                    .iter()
                    .find(|spot| spot.row == event.row)
                    .map(|spot| spot.index);
                self.pane_hover = self
                    .pane_spots
                    .iter()
                    .find(|(row, (from, to), _)| {
                        *row == event.row && (*from..*to).contains(&event.column)
                    })
                    .and_then(|(_, _, hit)| match hit {
                        pane::PaneHit::Item(item) => Some(item.clone()),
                        pane::PaneHit::Close | pane::PaneHit::Page { .. } => None,
                    });
            }
            // The wheel scrolls what is under the pointer; over the pane it
            // leaves the keys where they are.
            MouseEventKind::ScrollUp if self.over_pane(event) => {
                self.selection = None;
                self.pane_scroll = self
                    .pane_scroll
                    .saturating_sub(crate::wheel::lines(Direction::Up));
                self.pane_hover = None;
            }
            MouseEventKind::ScrollDown if self.over_pane(event) => {
                self.selection = None;
                self.pane_scroll += crate::wheel::lines(Direction::Down);
                self.pane_hover = None;
            }
            // Under the feed, an opened diff in the ask's box takes the
            // wheel.
            MouseEventKind::ScrollUp
                if self.below_feed(event)
                    && self.scroll_box_diff(state, -wheel_lines(Direction::Up)) => {}
            MouseEventKind::ScrollDown
                if self.below_feed(event)
                    && self.scroll_box_diff(state, wheel_lines(Direction::Down)) => {}
            MouseEventKind::ScrollUp => self.scroll(state, -wheel_lines(Direction::Up), theme),
            MouseEventKind::ScrollDown => self.scroll(state, wheel_lines(Direction::Down), theme),
            MouseEventKind::Down(MouseButton::Left) => {
                // The hint line's keys act on the selection; anywhere else a
                // click lets go of it.
                if !self.hint_spots.iter().any(|(row, (from, to), _)| {
                    *row == event.row && (*from..*to).contains(&event.column)
                }) {
                    self.selection = None;
                    self.copied = false;
                }
                match control {
                    Some(HeaderControl::Diff) => return vec![ChatEffect::Review],
                    Some(HeaderControl::Home) => return vec![ChatEffect::Home],
                    None => {}
                }
                let (x, y) = (event.column, event.row);
                let on = |row: u16, (from, to): (u16, u16)| row == y && (from..to).contains(&x);
                // The review chip: its ✕ asks to discard the drafts, the rest
                // of it opens the review.
                if let Some((row, chip, close)) = self.chip_spot
                    && let Some(found) = self.review_chip(state)
                {
                    if on(row, close) {
                        self.discard_ask = Some(review::Asking::new(review::Confirm::Discard {
                            drafts: found.comments,
                        }));
                        return vec![];
                    }
                    if on(row, chip) {
                        return vec![ChatEffect::Review];
                    }
                }
                if let Some(spot) = self
                    .ask_spots
                    .iter()
                    .find(|(row, cols, _)| on(*row, *cols))
                    .map(|(_, _, spot)| *spot)
                    && let Some(card) = self.card_takes_keys(state)
                {
                    self.ask.sync(&card);
                    let action = self.ask.box_click(&card, spot);
                    return self.ask_effects(state, &card, action);
                }
                if let Some((_, _, hit)) = self
                    .pane_spots
                    .iter()
                    .find(|(row, cols, _)| on(*row, *cols))
                    .cloned()
                {
                    match hit {
                        pane::PaneHit::Close => self.close_pane(),
                        pane::PaneHit::Page { up } => {
                            let page = self.pane_page.saturating_sub(2).max(1);
                            self.pane_scroll = if up {
                                self.pane_scroll.saturating_sub(page)
                            } else {
                                self.pane_scroll + page
                            };
                        }
                        // Opens on the release, as a drag from here selects.
                        pane::PaneHit::Item(item) => {
                            self.press = Some(Press::new(event, Pressed::Pane(Some(item))));
                        }
                    }
                    return vec![];
                }
                // A click anywhere else in the pane gives it the keys.
                if self
                    .pane_rect
                    .is_some_and(|rect| rect.contains(Position::new(x, y)))
                {
                    self.press = Some(Press::new(event, Pressed::Pane(None)));
                    return vec![];
                }
                if self.row_spot.is_some_and(|(row, cols)| on(row, cols)) {
                    self.open_pane();
                    return vec![];
                }
                if let Some((_, _, chord)) = self
                    .panel_spots
                    .iter()
                    .find(|(row, cols, _)| on(*row, *cols))
                {
                    return vec![ChatEffect::Chord(*chord)];
                }
                if let Some((_, _, key)) = self
                    .hint_spots
                    .iter()
                    .find(|(row, cols, _)| on(*row, *cols))
                {
                    return vec![ChatEffect::Press(*key)];
                }
                if self.jump_spot.is_some_and(|(row, cols)| on(row, cols)) {
                    self.follow();
                    return vec![];
                }
                if let Some(((from, to), key)) = &self.pin_spot
                    && (*from..*to).contains(&y)
                {
                    self.anchor = Anchor::Top {
                        key: key.clone(),
                        offset: 0,
                    };
                    return vec![];
                }
                if self.mode_spot.is_some_and(|(row, cols)| on(row, cols)) {
                    return vec![ChatEffect::Press(KeyEvent::new(
                        KeyCode::BackTab,
                        KeyModifiers::SHIFT,
                    ))];
                }
                // A queued prompt's controls act; a click elsewhere on it
                // highlights it.
                if let Some(spot) = self.queued_spots.iter().find(|spot| spot.row == y).cloned() {
                    let entries = self.queue_entries(state, "");
                    let Some(entry) = entries.get(spot.index) else {
                        return vec![];
                    };
                    if let Some(control) = spot.controls.iter().position(|cols| on(spot.row, *cols))
                        && let Some(effect) = self.queue_act(state, entry, control)
                    {
                        self.tray = None;
                        return vec![effect];
                    }
                    self.tray = Some(spot.index);
                    return vec![];
                }

                if let Some(spot) = self.composer_spot
                    && self.tray.is_none()
                    && (spot.y..spot.y + spot.rows).contains(&y)
                    && x >= spot.x.saturating_sub(2)
                {
                    self.pane_keys = false;
                    let row = usize::from(y - spot.y) + spot.skip;
                    let col = usize::from(x.saturating_sub(spot.x));
                    let at = composer::cursor_at(&self.editor, spot.wrap, row, col);
                    self.editor.set_cursor_chars(at);
                    return vec![];
                }
                // A link or fold acts on the release, as a drag from here
                // selects.
                let hit = self.hit_at(x, y);
                let anchor = self.feed_point(x, y);
                if hit.is_some() || anchor.is_some() {
                    self.press = Some(Press::new(event, Pressed::Feed { anchor, hit }));
                }
            }
            _ => {}
        }
        vec![]
    }

    /// The feed's place under a screen cell, when a row is drawn there.
    fn feed_point(&self, x: u16, y: u16) -> Option<Point> {
        let (origin_x, origin_y) = self.feed_origin;
        let (width, height) = self.feed;
        let row = usize::from(y.checked_sub(origin_y)?);
        let col = usize::from(x.checked_sub(origin_x)?);
        if col >= width {
            return None;
        }
        let (block, line) = line_places(&self.laid, height)
            .get(row)
            .copied()
            .flatten()?;
        Some(Point {
            order: self.laid.blocks.get(block)?.order,
            line,
            col,
        })
    }

    /// Where a drag at a screen cell reaches: the feed's place there, or
    /// off the rows drawn, the nearest end of them.
    fn feed_head(&self, x: u16, y: u16) -> Option<Point> {
        let (origin_x, origin_y) = self.feed_origin;
        let height = self.feed.1;
        let places = line_places(&self.laid, height);
        let row = i64::from(y) - i64::from(origin_y);
        let at = usize::try_from(row).ok().filter(|row| *row < height);
        if let Some(place) = at.and_then(|row| places[row]) {
            let block = self.laid.blocks.get(place.0)?;
            return Some(Point {
                order: block.order,
                line: place.1,
                col: usize::from(x.saturating_sub(origin_x)),
            });
        }
        let first = places.iter().position(Option::is_some)?;
        let last = places.iter().rposition(Option::is_some)?;
        let (place, col) = if at.map_or(row < 0, |row| row < first) {
            (places[first]?, 0)
        } else {
            (places[last]?, usize::MAX)
        };
        Some(Point {
            order: self.laid.blocks.get(place.0)?.order,
            line: place.1,
            col,
        })
    }

    fn drag(&mut self, event: MouseEvent) {
        let (x, y) = (event.column, event.row);
        let Some(press) = &mut self.press else {
            return;
        };
        if !press.dragged && (x, y) == press.at {
            return;
        }
        press.dragged = true;
        press.pointer = (x, y);
        let at = press.at;
        match &press.pressed {
            Pressed::Feed { anchor, .. } => {
                let Some(anchor) = *anchor else {
                    return;
                };
                let (top, bottom) = (self.feed_origin.1, self.feed_origin.1 + self.feed.1 as u16);
                self.edge = (y < top).then_some(-1).or((y >= bottom).then_some(1));
                if let Some(head) = self.feed_head(x, y) {
                    self.selection = Some(Selection::Feed(FeedSelection { anchor, head }));
                }
            }
            Pressed::Pane(_) | Pressed::Stream(_) => {
                let rect = match &press.pressed {
                    Pressed::Stream(rect) => Some(*rect),
                    _ => self.pane_body,
                };
                let Some(rect) = rect.filter(|rect| rect.contains(Position::new(at.0, at.1)))
                else {
                    return;
                };
                let mut selection = match self.selection.take() {
                    Some(Selection::Screen(selection)) => selection,
                    _ => ScreenSelection::new(rect, at),
                };
                selection.drag_to(x, y);
                self.selection = Some(Selection::Screen(selection));
            }
        }
    }

    /// The button came up: a press that never moved is a click, a drag has
    /// selected, and goes to the clipboard if the person wants that.
    fn release(&mut self, state: &SessionState, theme: Theme) -> Vec<ChatEffect> {
        self.edge = None;
        let Some(press) = self.press.take() else {
            return vec![];
        };
        if !press.dragged {
            return match press.pressed {
                Pressed::Feed { hit: Some(hit), .. } => self.feed_hit(state, hit),
                Pressed::Pane(Some(item)) => {
                    self.pane_keys = true;
                    self.pane_focus = Some(item.clone());
                    self.open_item(state, item)
                }
                Pressed::Pane(None) => {
                    self.focus_pane();
                    vec![]
                }
                Pressed::Feed { .. } | Pressed::Stream(_) => vec![],
            };
        }
        let words = self.selected_words(state, theme);
        if words.is_empty() {
            self.selection = None;
            return vec![];
        }
        if self.copy_on_select {
            self.copied = true;
            return vec![ChatEffect::Copy(words)];
        }
        vec![]
    }

    /// Keeps what a surface just drew, and lights its selection. Laid out
    /// anew in another place, the surface's selection and any press on it
    /// no longer name what is there.
    pub(super) fn keep_screen(&mut self, paint: &mut Paint<'_>, cells: ScreenCells, theme: Theme) {
        if matches!(&self.selection, Some(Selection::Screen(selection)) if selection.rect != cells.rect)
        {
            self.clear_selection();
        }
        if matches!(&self.press, Some(Press { pressed: Pressed::Stream(rect), .. }) if *rect != cells.rect)
        {
            self.press = None;
        }
        if let Some(Selection::Screen(selection)) = &self.selection {
            selection.paint(paint.buffer_mut(), &cells, theme.selection());
        }
        self.screen_cells = Some(cells);
    }

    pub(super) fn selected_words(&self, state: &SessionState, theme: Theme) -> String {
        match &self.selection {
            Some(Selection::Feed(selection)) => self.frame(theme).selected_text(state, selection),
            Some(Selection::Screen(selection)) => self
                .screen_cells
                .as_ref()
                .filter(|cells| cells.rect == selection.rect)
                .map_or_else(String::new, |cells| selection.text(cells)),
            None => String::new(),
        }
    }

    /// A drag held past the feed's edge scrolls another line.
    pub fn selecting_past_edge(&self) -> bool {
        self.edge.is_some()
    }

    pub fn edge_tick(&mut self, state: &SessionState, theme: Theme) {
        let Some(delta) = self.edge else {
            return;
        };
        self.scroll(state, delta, theme);
        self.laid = self.frame(theme).layout(state);
        let Some(pointer) = self.press.as_ref().map(|press| press.pointer) else {
            return;
        };
        let head = self.feed_head(pointer.0, pointer.1);
        if let (Some(Selection::Feed(selection)), Some(head)) = (&mut self.selection, head) {
            selection.head = head;
        }
    }

    /// What the feed line under a click does there.
    fn hit_at(&self, column: u16, row: u16) -> Option<FeedHit> {
        let (x, y) = self.feed_origin;
        let line = usize::from(row.checked_sub(y)?);
        let column = usize::from(column.checked_sub(x)?);
        self.laid
            .hits
            .get(line)?
            .iter()
            .find(|(span, _)| span.is_none_or(|(from, to)| (from..to).contains(&column)))
            .map(|(_, hit)| hit.clone())
    }

    pub(super) fn feed_hit(&mut self, state: &SessionState, hit: FeedHit) -> Vec<ChatEffect> {
        // Scrolled up, opening or folding something keeps what is on screen
        // where it is: the feed holds its top line, so an opened fold grows
        // downward from the line that was clicked. Following the newest row,
        // it keeps following.
        if matches!(hit, FeedHit::Run(_) | FeedHit::Step(_) | FeedHit::Group(_))
            && self.anchor != Anchor::Bottom
            && let Some(top) = self.laid.blocks.first()
        {
            self.anchor = Anchor::Top {
                key: top.key.clone(),
                offset: self.laid.top_offset,
            };
        }
        match hit {
            FeedHit::Link(url) => return vec![ChatEffect::OpenUrl(url)],
            FeedHit::Attachment(blob) => return vec![ChatEffect::OpenAttachment(blob)],
            FeedHit::Round { review, text } => {
                return vec![ChatEffect::ReviewRound { review, text }];
            }
            FeedHit::Thumbnail { blob, .. } => {
                return vec![match state.blob(&blob.hash) {
                    ui_state::BlobStatus::Failed(_) => ChatEffect::RetryBlob(blob.hash),
                    _ => ChatEffect::OpenAttachment(blob),
                }];
            }
            FeedHit::Agent(agent) => return vec![ChatEffect::OpenAgent(agent)],
            // A group is held open in the set that holds open runs.
            FeedHit::Group(member) => ui_view::toggle_group(state, &member, &mut self.open_runs),
            FeedHit::Run(member) => ui_view::toggle_run(state, &member, &mut self.open_runs),
            FeedHit::Step(key) => {
                // A run opens from any member and closes as a whole.
                let transcript = state.transcript();
                let run = transcript
                    .get(&key)
                    .and_then(|held| transcript.run_at(held.item.order));
                let members: Vec<Key> = match run {
                    Some(run) => transcript
                        .range(run.oldest..=run.newest)
                        .map(|held| held.item.key.clone())
                        .collect(),
                    None => vec![key.clone()],
                };
                if members.iter().any(|member| self.expanded.contains(member)) {
                    for member in &members {
                        self.expanded.remove(member);
                    }
                } else {
                    self.expanded.insert(key);
                }
            }
        }
        vec![]
    }
}
