//! The transcript window and its run index.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::RangeInclusive;

use wire::{Append, Item, Kind};

use crate::Key;
use crate::body::{Fold, ItemBody, ItemClass, named_kind};

/// One held item, decoded once per revision.
#[derive(Clone, Debug, PartialEq)]
pub struct Held {
    pub item: Item,
    pub body: ItemBody,
    pub class: ItemClass,
}

impl Held {
    pub(crate) fn new(kind: Kind, item: Item) -> Held {
        let kind = named_kind(item.kind).unwrap_or(kind);
        let body = ItemBody::decode(kind, &item.body);
        let class = body.class();
        Held { item, body, class }
    }
}

/// A contiguous window of items by order: every item the client holds with
/// order in `[oldest_held, head]`. A row above the head appends; inside the
/// window it upserts by key if newer, else is ignored, which absorbs the
/// overlap between a store read and the broadcast; below `oldest_held` it is
/// dropped, since a page brings the current version if it is ever scrolled
/// to. Pages only extend the low edge. The session trims the oldest rows
/// while the reader follows the newest, so the window never grows past its
/// cap then; the rows trimmed stay in the store, a page away.
#[derive(Clone, Debug, PartialEq)]
pub struct Transcript {
    kind: Kind,
    items: BTreeMap<u64, Held>,
    /// Ordered, like every map a session holds, so two equal sessions
    /// print alike: the client model tells states apart by their print.
    by_key: BTreeMap<Key, u64>,
    by_input: BTreeMap<Vec<u8>, Key>,
    exhausted: bool,
    runs: RunIndex,
}

/// What an append did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Appended {
    Applied,
    /// Nothing to apply: the held revision already covers it, or nothing is
    /// held for the key. An item not held is below the window or let go of,
    /// and every stream of appends ends with the item whole, so whoever
    /// reads that row later fetches it whole.
    Dropped,
    /// A held item is at another revision than the base; the driver answers
    /// with Get.
    NeedGet,
}

impl Transcript {
    pub fn new(kind: Kind) -> Transcript {
        Transcript {
            kind,
            items: BTreeMap::new(),
            by_key: BTreeMap::new(),
            by_input: BTreeMap::new(),
            exhausted: false,
            runs: RunIndex::default(),
        }
    }

    /// A transcript holding `items` as they are, with its indexes rebuilt
    /// from them: how a dump's rows become a window again. Nothing is
    /// checked or repaired, so a window a dump caught broken stays broken.
    pub(crate) fn restore(kind: Kind, items: Vec<Item>, exhausted: bool) -> Transcript {
        let mut transcript = Transcript::new(kind);
        transcript.exhausted = exhausted;
        for item in items {
            let held = Held::new(kind, item);
            transcript
                .by_key
                .insert(held.item.key.clone(), held.item.order);
            if !held.item.input_id.is_empty() {
                transcript
                    .by_input
                    .insert(held.item.input_id.clone(), held.item.key.clone());
            }
            transcript.items.insert(held.item.order, held);
        }
        transcript.runs = RunIndex::rebuild(&transcript);
        transcript
    }

    /// Whether the oldest row held is known to be the agent's first.
    pub(crate) fn exhausted(&self) -> bool {
        self.exhausted
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// The oldest held order: the page-older cursor.
    pub fn oldest_held(&self) -> Option<u64> {
        self.items.keys().next().copied()
    }

    pub fn head(&self) -> Option<u64> {
        self.items.keys().next_back().copied()
    }

    /// Whether history older than the window exists. Orders start at one and
    /// are assigned densely, so a window reaching order one has everything; a
    /// page that came back exhausted says the rest is gone.
    pub fn has_older(&self) -> bool {
        !self.exhausted && self.oldest_held().is_some_and(|oldest| oldest > 1)
    }

    pub fn get(&self, key: &str) -> Option<&Held> {
        self.by_key.get(key).and_then(|order| self.items.get(order))
    }

    pub fn at(&self, order: u64) -> Option<&Held> {
        self.items.get(&order)
    }

    /// The key of the item carrying this input id, a prompt's reflection.
    pub fn key_for_input(&self, input_id: &[u8]) -> Option<&Key> {
        self.by_input.get(input_id)
    }

    /// Held items, oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Held> {
        self.items.values()
    }

    pub fn range(&self, range: RangeInclusive<u64>) -> impl DoubleEndedIterator<Item = &Held> {
        self.items.range(range).map(|(_, held)| held)
    }

    pub fn keys(&self) -> impl Iterator<Item = &Key> {
        self.items.values().map(|held| &held.item.key)
    }

    pub fn runs(&self) -> &RunIndex {
        &self.runs
    }

    /// The run an order belongs to, if it is in one.
    pub fn run_at(&self, order: u64) -> Option<Run> {
        self.runs.run_at(order, self)
    }

    /// The held orders of the run the item at `order` sits in, from its
    /// oldest step to its newest, without reading the rows.
    pub fn run_span(&self, order: u64) -> Option<RangeInclusive<u64>> {
        self.runs
            .containing(order)
            .map(|(&start, segment)| start..=segment.end)
    }

    /// Whether the turn the item at `order` belongs to has ended: the
    /// first prompt, steer or turn end held after it is a turn end.
    pub fn turn_ended_after(&self, order: u64) -> bool {
        self.items
            .range(order + 1..)
            .find(|(_, held)| bounds_turn(&held.class))
            .is_some_and(|(_, held)| held.class == ItemClass::Turn)
    }

    /// Applies one full item under the window rules and returns the keys whose
    /// rows may differ.
    pub(crate) fn upsert(&mut self, item: Item, changed: &mut Changed) {
        let order = item.order;
        match self.by_key.get(&item.key) {
            Some(&held_order) => {
                let held = &self.items[&held_order];
                if held.item.revision >= item.revision {
                    return;
                }
                // Order is assigned once per key; trust the held position.
                let mut item = item;
                item.order = held_order;
                self.replace(held_order, item, changed);
            }
            None => {
                let below = self.oldest_held().is_some_and(|oldest| order < oldest);
                if below {
                    return;
                }
                if self.items.contains_key(&order) {
                    // One key per order; a second key at a held order is not
                    // this agent's history.
                    return;
                }
                self.insert(item, changed);
            }
        }
    }

    /// Extends the low edge with an older page. Rows inside the window upsert
    /// by key if newer; rows below it insert; nothing lands above the head.
    pub(crate) fn page(&mut self, items: Vec<Item>, exhausted: bool, changed: &mut Changed) {
        let head = self.head();
        let mut items = items;
        // Newest first from the store; merge downward so each row lands below
        // the one merged before it.
        items.sort_by_key(|item| std::cmp::Reverse(item.order));
        for item in items {
            if head.is_some_and(|head| item.order > head) {
                continue;
            }
            match self.by_key.get(&item.key) {
                Some(&held_order) => {
                    if self.items[&held_order].item.revision < item.revision {
                        let mut item = item;
                        item.order = held_order;
                        self.replace(held_order, item, changed);
                    }
                }
                None if !self.items.contains_key(&item.order) => self.insert(item, changed),
                None => {}
            }
        }
        if exhausted && !self.exhausted {
            // Ruling out older history clears the open-below mark of the run
            // at the low edge.
            let before = self.runs.first(self);
            self.exhausted = true;
            let after = self.runs.first(self);
            if before != after
                && let Some(run) = after
            {
                self.runs.members(&run, self, changed);
            }
        }
    }

    pub(crate) fn append(&mut self, append: &Append, changed: &mut Changed) -> Appended {
        let Some(&order) = self.by_key.get(&append.key) else {
            return Appended::Dropped;
        };
        let held = &self.items[&order];
        if held.item.revision >= append.revision {
            return Appended::Dropped;
        }
        if held.item.revision != append.base_revision {
            return Appended::NeedGet;
        }
        let mut item = held.item.clone();
        item.text.push_str(&append.text);
        item.revision = append.revision;
        self.replace(order, item, changed);
        Appended::Applied
    }

    /// Drops the oldest rows until at most `cap` remain. Older history then
    /// exists again, and the run at the new low edge reads open below; its
    /// indexes forget the dropped rows, so a later revision of one is
    /// ignored like any row below the window.
    pub(crate) fn trim(&mut self, cap: usize, changed: &mut Changed) {
        if self.items.len() <= cap {
            return;
        }
        let first_before = self.runs.first(self);
        while self.items.len() > cap {
            let Some((_, held)) = self.items.pop_first() else {
                break;
            };
            let key = &held.item.key;
            self.by_key.remove(key);
            if self.by_input.get(&held.item.input_id) == Some(key) {
                self.by_input.remove(&held.item.input_id);
            }
            changed.key(key);
        }
        self.exhausted = false;
        let Some(oldest) = self.oldest_held() else {
            self.runs = RunIndex::default();
            return;
        };
        // A run the cut went through loses members; whatever run starts at
        // the new low edge now reads open below.
        let cut_run = self
            .runs
            .containing(oldest)
            .filter(|(start, _)| **start < oldest)
            .map(|(_, segment)| segment.end);
        let mut runs = std::mem::take(&mut self.runs);
        runs.cut_below(oldest, self);
        self.runs = runs;
        if let Some(end) = cut_run {
            for held in self.items.range(oldest..=end).map(|(_, held)| held) {
                changed.key(&held.item.key);
            }
        }
        if let Some(run) = self.runs.first(self) {
            // The same run is the one ending at the same step; any other was
            // ended below by something the cut took.
            let was_open = first_before
                .filter(|before| before.newest == run.newest)
                .is_some_and(|before| before.open_below);
            if run.open_below != was_open {
                self.runs.members(&run, self, changed);
            }
        }
    }

    /// Checks the block invariant: orders contiguous, and every index names
    /// only held rows.
    pub fn check(&self) -> Result<(), String> {
        if let (Some(oldest), Some(head)) = (self.oldest_held(), self.head())
            && head - oldest + 1 != self.items.len() as u64
        {
            return Err(format!(
                "{} rows held between {oldest} and {head}",
                self.items.len()
            ));
        }
        for (key, order) in &self.by_key {
            if self.items.get(order).map(|held| &held.item.key) != Some(key) {
                return Err(format!("key {key} indexed at {order}, which holds another"));
            }
        }
        if self.by_key.len() != self.items.len() {
            return Err("a held row is not indexed by key".into());
        }
        for key in self.by_input.values() {
            if !self.by_key.contains_key(key) {
                return Err(format!("input id indexes {key}, which is not held"));
            }
        }
        if self.runs != RunIndex::rebuild(self) {
            return Err("run index drifted from a rebuild".into());
        }
        Ok(())
    }

    fn reindex(&mut self, order: u64, inserted: bool) {
        let mut runs = std::mem::take(&mut self.runs);
        runs.reindex(order, inserted, self);
        self.runs = runs;
    }

    fn insert(&mut self, item: Item, changed: &mut Changed) {
        let order = item.order;
        let held = Held::new(self.kind, item);
        // What passes through a run, arriving at the head, moves none.
        let quiet =
            held.class.fold() == Fold::Through && self.head().is_none_or(|head| order > head);
        let turns = bounds_turn(&held.class).then(|| self.turn_steps(order));
        let before = (!quiet).then(|| self.runs.capture(order, self));
        self.by_key.insert(held.item.key.clone(), order);
        if !held.item.input_id.is_empty() {
            self.by_input
                .insert(held.item.input_id.clone(), held.item.key.clone());
        }
        changed.key(&held.item.key);
        self.items.insert(order, held);
        if let Some(before) = before {
            self.reindex(order, true);
            self.runs.compare(before, self, changed);
        }
        if let Some(turns) = turns {
            self.turn_steps_moved(turns, changed);
        }
    }

    /// The steps before `order` back to the prompt, steer or turn end before
    /// it, each with whether its turn has ended: what an item bounding
    /// turns at `order` can change.
    fn turn_steps(&self, order: u64) -> Vec<(u64, bool)> {
        let mut steps = Vec::new();
        for held in self.range(0..=order.saturating_sub(1)).rev() {
            if bounds_turn(&held.class) {
                break;
            }
            if matches!(held.class, ItemClass::Tool(_)) {
                steps.push((held.item.order, self.turn_ended_after(held.item.order)));
            }
        }
        steps
    }

    /// A turn's end settles which of its failed steps nothing redid: the
    /// steps whose turn now reads ended, or no longer does.
    fn turn_steps_moved(&self, before: Vec<(u64, bool)>, changed: &mut Changed) {
        for (order, ended) in before {
            if self.turn_ended_after(order) != ended {
                changed.key(&self.items[&order].item.key);
            }
        }
    }

    fn replace(&mut self, order: u64, item: Item, changed: &mut Changed) {
        let held = Held::new(self.kind, item);
        let old = &self.items[&order];
        if old.item.input_id != held.item.input_id {
            self.by_input.remove(&old.item.input_id);
        }
        if !held.item.input_id.is_empty() {
            self.by_input
                .insert(held.item.input_id.clone(), held.item.key.clone());
        }
        let fold_moved = old.class.fold() != held.class.fold();
        let turns = (old.class != held.class
            && (bounds_turn(&old.class) || bounds_turn(&held.class)))
        .then(|| self.turn_steps(order));
        changed.key(&held.item.key);
        if fold_moved {
            let before = self.runs.capture(order, self);
            self.items.insert(order, held);
            self.reindex(order, false);
            self.runs.compare(before, self, changed);
        } else {
            self.items.insert(order, held);
        }
        if let Some(turns) = turns {
            self.turn_steps_moved(turns, changed);
        }
    }
}

/// A prompt, a steer or a turn's end: what says where a turn ends.
fn bounds_turn(class: &ItemClass) -> bool {
    matches!(
        class,
        ItemClass::Prompt | ItemClass::Steer | ItemClass::Turn
    )
}

/// Keys whose rows may differ, in first-touched order, without repeats.
#[derive(Debug, Default)]
pub(crate) struct Changed {
    keys: Vec<Key>,
    seen: BTreeSet<Key>,
}

impl Changed {
    pub(crate) fn key(&mut self, key: &str) {
        if self.seen.insert(key.to_owned()) {
            self.keys.push(key.to_owned());
        }
    }

    pub(crate) fn into_keys(self) -> Vec<Key> {
        self.keys
    }
}

/// A run: the tool steps between two pieces of what the agent or the
/// person put in the transcript, with whatever passes through between them
/// (see [`Fold`]). Every held item from its oldest step to its newest is in
/// it. `open_below` marks a run that may continue below the window: older
/// history exists and nothing below its oldest step ends it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub oldest: u64,
    pub newest: u64,
    pub oldest_key: Key,
    pub newest_key: Key,
    pub steps: u32,
    /// Something that ends a run follows its newest step.
    pub closed: bool,
    pub open_below: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Segment {
    /// The newest step.
    end: u64,
    steps: u32,
    closed: bool,
}

/// Run membership, derived from the transcript and kept current on each
/// message: one segment per run, keyed by its oldest step. A step at the
/// head extends the last open run or starts one, and anything ending a run
/// closes it, in place; a change inside the window rescans only the runs
/// around it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunIndex {
    segments: BTreeMap<u64, Segment>,
}

/// The runs around one order before a change, to compare after it.
pub(crate) struct Captured {
    runs: Vec<(u64, Option<Run>)>,
}

impl RunIndex {
    /// Rebuilds the index from a transcript: the oracle the incremental
    /// upkeep is tested against.
    pub fn rebuild(transcript: &Transcript) -> RunIndex {
        let mut index = RunIndex::default();
        if let Some(lo) = transcript.oldest_held() {
            index.scan(lo, u64::MAX, transcript);
        }
        index
    }

    pub fn run_at(&self, order: u64, transcript: &Transcript) -> Option<Run> {
        let (&start, segment) = self.containing(order)?;
        let oldest_held = transcript.oldest_held()?;
        let open_below = transcript.has_older()
            && transcript
                .items
                .range(oldest_held..start)
                .all(|(_, held)| held.class.fold() != Fold::Break);
        Some(Run {
            oldest: start,
            newest: segment.end,
            oldest_key: transcript.items[&start].item.key.clone(),
            newest_key: transcript.items[&segment.end].item.key.clone(),
            steps: segment.steps,
            closed: segment.closed,
            open_below,
        })
    }

    /// The oldest held run: the one that may read open below.
    fn first(&self, transcript: &Transcript) -> Option<Run> {
        let (&start, _) = self.segments.iter().next()?;
        self.run_at(start, transcript)
    }

    /// Every run, oldest first, as (oldest step, newest step).
    pub fn spans(&self) -> impl Iterator<Item = RangeInclusive<u64>> + '_ {
        self.segments
            .iter()
            .map(|(&start, segment)| start..=segment.end)
    }

    fn containing(&self, order: u64) -> Option<(&u64, &Segment)> {
        self.segments
            .range(..=order)
            .next_back()
            .filter(|(_, segment)| segment.end >= order)
    }

    /// Where a change at `order` can move runs: from the oldest step of the
    /// run at or before it to the newest step of the run after it.
    fn span(&self, order: u64) -> RangeInclusive<u64> {
        let before = self.segments.range(..=order).next_back();
        let after = self.segments.range(order + 1..).next();
        let lo = before.map_or(order, |(&start, _)| start);
        let hi = [
            Some(order),
            before.map(|(_, segment)| segment.end),
            after.map(|(_, segment)| segment.end),
        ]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(order);
        lo..=hi
    }

    fn capture(&self, order: u64, transcript: &Transcript) -> Captured {
        let runs = transcript
            .items
            .range(self.span(order))
            .map(|(&o, _)| (o, self.run_at(o, transcript)))
            .collect();
        Captured { runs }
    }

    fn reindex(&mut self, order: u64, inserted: bool, transcript: &Transcript) {
        // Fast path: a new head item extends, starts or closes the last run.
        if inserted && transcript.head() == Some(order) {
            let last = self.segments.iter_mut().next_back();
            match (transcript.items[&order].class.fold(), last) {
                (Fold::Through, _) => {}
                (Fold::Step, Some((_, segment))) if !segment.closed => {
                    segment.end = order;
                    segment.steps += 1;
                }
                (Fold::Step, _) => {
                    let segment = Segment {
                        end: order,
                        steps: 1,
                        closed: false,
                    };
                    self.segments.insert(order, segment);
                }
                (Fold::Break, Some((_, segment))) => segment.closed = true,
                (Fold::Break, None) => {}
            }
            return;
        }
        let span = self.span(order);
        self.scan(*span.start(), *span.end(), transcript);
    }

    /// Forgets rows below `oldest`: runs that started there go, and one
    /// that reached past it is scanned again from `oldest`.
    fn cut_below(&mut self, oldest: u64, transcript: &Transcript) {
        let cut: Vec<(u64, Segment)> = self
            .segments
            .range(..oldest)
            .map(|(&start, segment)| (start, *segment))
            .collect();
        for (start, segment) in cut {
            self.segments.remove(&start);
            if segment.end >= oldest {
                self.scan(oldest, segment.end, transcript);
            }
        }
    }

    /// Scans from `lo` up to the first item at or past `through` that ends
    /// a run, or the head, and replaces every run starting in what it read.
    fn scan(&mut self, lo: u64, through: u64, transcript: &Transcript) {
        let mut found = Vec::new();
        let mut open: Option<(u64, Segment)> = None;
        let mut read = lo;
        for (&order, held) in transcript.items.range(lo..) {
            read = order;
            match held.class.fold() {
                Fold::Step => {
                    let (_, segment) = open.get_or_insert((
                        order,
                        Segment {
                            end: order,
                            steps: 0,
                            closed: false,
                        },
                    ));
                    segment.end = order;
                    segment.steps += 1;
                }
                Fold::Through => {}
                Fold::Break => {
                    if let Some((start, mut segment)) = open.take() {
                        segment.closed = true;
                        found.push((start, segment));
                    }
                    if order >= through {
                        break;
                    }
                }
            }
        }
        found.extend(open);
        let stale: Vec<u64> = self
            .segments
            .range(lo..=read)
            .map(|(&start, _)| start)
            .collect();
        for start in stale {
            self.segments.remove(&start);
        }
        self.segments.extend(found);
    }

    /// Every row whose run changed. A change at one order only touches the
    /// runs beside it, all captured before it.
    fn compare(&self, before: Captured, transcript: &Transcript, changed: &mut Changed) {
        for (order, run) in &before.runs {
            if transcript.items.contains_key(order) && self.run_at(*order, transcript) != *run {
                changed.key(&transcript.items[order].item.key);
            }
        }
    }

    fn members(&self, run: &Run, transcript: &Transcript, changed: &mut Changed) {
        for held in transcript.range(run.oldest..=run.newest) {
            changed.key(&held.item.key);
        }
    }
}
