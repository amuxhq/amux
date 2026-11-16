//! The turn fold: a finished turn read answer first. A client that takes it
//! ([`ToolRows::Turns`]) draws each finished turn as its prompt, one line
//! saying how long the agent worked, then the answer it ended on, with the
//! work between folded under that line until the reader opens it. What the
//! reader must see whatever the fold stays in view: an error, a boundary, a
//! compaction, and a failed step the turn never fixed. The turn under way
//! is drawn as the run fold draws it, its work showing as it happens.
//!
//! A turn here is what a prompt or a steer opens and a turn's end closes,
//! both held: until the window holds what opened it, a turn draws unfolded,
//! since there is nothing for its line to hang under. A stretch a steer
//! cuts short never ends, so it stays unfolded too. A turn that was stopped
//! does not fold: the reader looks at where it stopped.
//!
//! The answer is the turn's last stretch of the agent's text, wherever it
//! falls; anything else in the turn is its work, folded in its order. Each
//! finished turn is held by the key of its end, which never moves, so a
//! client's set of open turns needs no upkeep as the window moves.
//!
//! [`ToolRows::Turns`]: crate::ToolRows::Turns

use std::collections::HashSet;
use std::ops::RangeInclusive;

use schemars::JsonSchema;
use serde::Serialize;
use ui_state::{Held, ItemBody, ItemClass, Key, SessionState, Transcript};

use crate::rows::{FileRow, RowKind, kind_of};

/// Where a row stands in a finished turn, stated on every row of one when
/// the client folds turns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TurnPlace {
    /// The turn's end, which names it: what opens and closes it.
    pub end: Key,
    /// Whether the reader opened its work.
    pub open: bool,
    pub part: TurnPart,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum TurnPart {
    /// What opened the turn. The line over its work goes under it; None
    /// when the turn did nothing but answer.
    Prompt { worked: Option<Worked> },
    /// Folded under the line until the turn is opened.
    Work,
    /// Work drawn whatever the fold: an error, a boundary, a compaction, or
    /// a failed step the turn never fixed.
    Kept,
    /// The agent's text the turn ended on.
    Answer,
    /// The turn's end, drawn as the foot under its answer: the answer's rows,
    /// for copying it, and each file the turn changed, once, with what all
    /// its edits in the turn added and removed.
    End {
        answer: Vec<Key>,
        files: Vec<FileRow>,
    },
}

/// The line over a finished turn's work: "Worked for 3m 14s".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Worked {
    pub duration_ms: Option<i64>,
    pub failed: bool,
}

/// A finished turn, read once for every row of it a call places.
struct Finished {
    span: RangeInclusive<u64>,
    end: Key,
    worked: Option<Worked>,
    answer: Vec<u64>,
    answer_keys: Vec<Key>,
    files: Vec<FileRow>,
    open: bool,
}

/// The finished turns one read of rows has met, so a turn is walked once
/// however many of its rows the read places.
#[derive(Default)]
pub(crate) struct Turns {
    met: Vec<Finished>,
}

impl Turns {
    /// The place of the item `held` in its finished turn, when it is in one
    /// the window holds whole; `kept` is whether its own row stays in view
    /// whatever the fold.
    pub(crate) fn place(
        &mut self,
        state: &SessionState,
        held: &Held,
        kind: &RowKind,
        kept: bool,
        open: &HashSet<Key>,
    ) -> Option<TurnPlace> {
        let order = held.item.order;
        let index = match self.met.iter().position(|turn| turn.span.contains(&order)) {
            Some(index) => index,
            None => {
                let span = finished_span(state.transcript(), order)?;
                let turn = finished(state, span, open)?;
                self.met.push(turn);
                self.met.len() - 1
            }
        };
        let turn = &self.met[index];
        let part = if order == *turn.span.start() {
            TurnPart::Prompt {
                worked: turn.worked,
            }
        } else if order == *turn.span.end() {
            TurnPart::End {
                answer: turn.answer_keys.clone(),
                files: turn.files.clone(),
            }
        } else if turn.answer.contains(&order) {
            TurnPart::Answer
        } else if kept || keeps(kind) {
            TurnPart::Kept
        } else {
            TurnPart::Work
        };
        Some(TurnPlace {
            end: turn.end.clone(),
            open: turn.open,
            part,
        })
    }
}

/// Rows drawn whatever the fold.
fn keeps(kind: &RowKind) -> bool {
    matches!(
        kind,
        RowKind::Error { .. }
            | RowKind::Deprecation { .. }
            | RowKind::Boundary { .. }
            | RowKind::Compaction { .. }
    )
}

/// The held items from `lo` through `hi`, none when `lo` is past `hi`,
/// where the transcript's own range would panic.
fn between(transcript: &Transcript, lo: u64, hi: u64) -> impl DoubleEndedIterator<Item = &Held> {
    (lo <= hi)
        .then(|| transcript.range(lo..=hi))
        .into_iter()
        .flatten()
}

/// A prompt, a steer or a turn's end: what says where a turn ends.
pub(crate) fn bounds_turn(class: &ItemClass) -> bool {
    matches!(
        class,
        ItemClass::Prompt | ItemClass::Steer | ItemClass::Turn
    )
}

fn opens_turn(class: &ItemClass) -> bool {
    matches!(class, ItemClass::Prompt | ItemClass::Steer)
}

/// From what opened the turn the item at `order` sits in to the turn end
/// that closed it, when the window holds both.
fn finished_span(transcript: &Transcript, order: u64) -> Option<RangeInclusive<u64>> {
    let held = transcript.at(order)?;
    let start = if opens_turn(&held.class) {
        order
    } else {
        let oldest = transcript.oldest_held()?;
        let before = between(transcript, oldest, order.checked_sub(1)?)
            .rev()
            .find(|held| bounds_turn(&held.class))?;
        if !opens_turn(&before.class) {
            return None;
        }
        before.item.order
    };
    let head = transcript.head()?;
    let end = between(transcript, start + 1, head).find(|held| bounds_turn(&held.class))?;
    (end.class == ItemClass::Turn).then_some(start..=end.item.order)
}

/// How an item takes part in the answer, read from its class alone: the
/// walk over a turn reads every item, and building a row's kind costs more.
enum Reading {
    /// The agent's text, not a working note.
    Text,
    /// Drawn by no client: it neither is the answer nor ends it.
    Nothing,
    Other,
}

fn reading(held: &Held) -> Reading {
    match &held.class {
        ItemClass::Prose { .. } => match &held.body {
            ItemBody::Codex(wire::codex_item::Kind::WorkingNote(_)) => Reading::Other,
            _ => Reading::Text,
        },
        ItemClass::Silent => Reading::Nothing,
        ItemClass::Tool(tool) if tool.hidden || tool.child => Reading::Nothing,
        _ => Reading::Other,
    }
}

/// Reads the turn over `span`; None for a turn that was stopped.
fn finished(
    state: &SessionState,
    span: RangeInclusive<u64>,
    open: &HashSet<Key>,
) -> Option<Finished> {
    let transcript = state.transcript();
    let end = transcript.at(*span.end())?;
    let (duration_ms, failed) = match kind_of(state, end).0 {
        RowKind::TurnEnd {
            duration_ms,
            failed,
        } => (duration_ms, failed),
        _ => return None,
    };
    let inside = || between(transcript, span.start() + 1, span.end().saturating_sub(1));
    // The answer: back from the end past whatever is not the agent's text,
    // then the stretch of its text, through what draws nothing.
    let mut answer = Vec::new();
    let mut started = false;
    for held in inside().rev() {
        match reading(held) {
            Reading::Text => {
                started = true;
                answer.push(held.item.order);
            }
            Reading::Nothing => {}
            Reading::Other if started => break,
            Reading::Other => {}
        }
    }
    answer.reverse();
    let mut worked = false;
    let mut files: Vec<FileRow> = Vec::new();
    for held in inside() {
        if answer.contains(&held.item.order) || matches!(reading(held), Reading::Nothing) {
            continue;
        }
        worked = true;
        if matches!(held.class, ItemClass::Tool(_))
            && let RowKind::FileChange { files: changed, .. } = kind_of(state, held).0
        {
            for file in changed {
                match files.iter_mut().find(|known| known.path == file.path) {
                    Some(known) => {
                        known.added += file.added;
                        known.removed += file.removed;
                        known.change = file.change;
                    }
                    None => files.push(file),
                }
            }
        }
    }
    let answer_keys = answer
        .iter()
        .filter_map(|order| transcript.at(*order))
        .map(|held| held.item.key.clone())
        .collect();
    Some(Finished {
        end: end.item.key.clone(),
        open: open.contains(&end.item.key),
        worked: worked.then_some(Worked {
            duration_ms,
            failed,
        }),
        answer,
        answer_keys,
        files,
        span,
    })
}

/// Every key of the finished turn `end` closes, to draw again when it opens
/// or closes.
pub fn turn_keys(state: &SessionState, end: &Key) -> Vec<Key> {
    let transcript = state.transcript();
    transcript
        .get(end)
        .and_then(|held| finished_span(transcript, held.item.order))
        .map(|span| {
            transcript
                .range(span)
                .map(|held| held.item.key.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// The keys whose place in a turn moves with the rows `changed`, beyond
/// those rows themselves. What opens or ends a turn, arriving or changing,
/// moves every row of the turns either side of it; a row changing inside a
/// finished turn moves its end, whose foot sums the turn; and the window
/// losing or gaining its oldest rows moves the turn at its low edge, which
/// folds only while what opened it is held.
pub fn turn_rows_moved(state: &SessionState, changed: &[Key]) -> Vec<Key> {
    let transcript = state.transcript();
    let (Some(oldest), Some(head)) = (transcript.oldest_held(), transcript.head()) else {
        return Vec::new();
    };
    let mut spans: Vec<RangeInclusive<u64>> = Vec::new();
    let mut add = |span: RangeInclusive<u64>| {
        if !span.is_empty() && !spans.contains(&span) {
            spans.push(span);
        }
    };
    let bound_before = |order: u64| {
        order
            .checked_sub(1)
            .filter(|before| *before >= oldest)
            .and_then(|before| {
                between(transcript, oldest, before)
                    .rev()
                    .find(|held| bounds_turn(&held.class))
                    .map(|held| held.item.order)
            })
    };
    let bound_after =
        |order: u64| between(transcript, order + 1, head).find(|held| bounds_turn(&held.class));
    // A cut at the low edge unfolds the finished turn it took the prompt
    // of: the rows up to the window's first bound, when that bound is a
    // turn's end. Rows before a prompt or a steer were in no folded turn,
    // and with no bound held no turn's end is held either. A page needs no
    // such rule: a prompt it brings in is a bound, placed below.
    let mut cut = false;
    for key in changed {
        let Some(held) = transcript.get(key) else {
            cut = true;
            continue;
        };
        let order = held.item.order;
        if bounds_turn(&held.class) {
            add(bound_before(order).unwrap_or(oldest)..=order);
            if let Some(after) = bound_after(order) {
                add(order..=after.item.order);
            }
        } else if let Some(after) = bound_after(order)
            && after.class == ItemClass::Turn
        {
            add(after.item.order..=after.item.order);
        }
    }
    if cut
        && let Some(first) = transcript
            .range(oldest..=head)
            .find(|held| bounds_turn(&held.class))
        && first.class == ItemClass::Turn
    {
        add(oldest..=first.item.order);
    }
    let changed: HashSet<&Key> = changed.iter().collect();
    let mut seen: HashSet<&Key> = HashSet::new();
    let mut keys: Vec<Key> = Vec::new();
    for span in spans {
        for held in transcript.range(span) {
            let key = &held.item.key;
            if !changed.contains(key) && seen.insert(key) {
                keys.push(key.clone());
            }
        }
    }
    keys
}
