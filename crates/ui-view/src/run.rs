//! The one fold for tool steps. A run is every tool step between two pieces
//! of what the agent or the person put in the transcript: a turn reads as
//! the agent's text with its work in between, and a client folds each run
//! of that work to one line, what it did, not every call. While a run is
//! under way it shows its newest step and any step still running, under a
//! line of what the steps it no longer shows did. Thinking, retry
//! notices, a subagent's own steps and items that draw nothing pass through
//! a run; anything else ends it.
//!
//! Membership is the session's run index; this module states it on each
//! row, so any client's list gets the fold with the rows. A client chooses
//! between showing every step, hiding them and folding them, and holds
//! which runs are open.
//!
//! A run's ends move with the window: a page of older history grows it
//! below, and a window following the newest row trims its oldest steps. So
//! a client holds an open run by one of its steps, not by either end: a run
//! is open when the open set holds any of its members. Opening holds its
//! newest step, which no page moves; [`keep_open_runs`] moves each hold to
//! its run's newest step as the run grows, so a trim never takes it.

use std::collections::{BTreeSet, HashSet};

use schemars::JsonSchema;
use serde::Serialize;
use ui_state::{Fold, Held, ItemClass, Key, SessionState};

use crate::rows::{CallPhase, ExploreVerb, RowKind, kind_of, subject_of};

/// How many of a run's newest steps say how many steps follow them: the
/// newest, and the one before it, which a client draws last while the
/// newest is the composer's to ask about.
const RECENT_STEPS: u32 = 2;

/// A row's place in its run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Run {
    /// The run's oldest held step, where an opened run draws its header.
    /// It moves as the window's low edge does, so it names no run: hold
    /// an open run by a member (see [`run_is_open`]).
    pub first: Key,
    /// Its newest step, where a folded run draws its one line.
    pub last: Key,
    pub steps: u32,
    /// Nothing but steps follows it yet: the agent is still at it.
    pub live: bool,
    /// It starts at the oldest held rows and older history exists, so it may
    /// continue below: its counts are a floor.
    pub open_below: bool,
    /// For one of the run's two newest steps, how many steps follow it
    /// (none for the newest); absent for the rest.
    pub recent: Option<u32>,
    /// While the run is under way this step shows: it is the newest, or it
    /// is still running. False once the run is over.
    pub live_shown: bool,
    /// On the oldest step a live run shows, what the steps it does not
    /// show did, for the line above it; absent elsewhere, and when it shows
    /// every step.
    pub hidden: Option<RunCounts>,
    /// This step failed and nothing later in its turn redid it, and the
    /// turn has ended: it stays in view when the run folds.
    pub unresolved_failure: bool,
    /// What the run's steps did, on its newest step's row only.
    pub counts: Option<RunCounts>,
    /// The reader opened it: a folding view's open set holds one of its
    /// steps. Always false for a view that does not fold.
    pub open: bool,
}

impl Run {
    /// This row is the run's newest step.
    pub fn is_last(&self) -> bool {
        self.recent == Some(0)
    }
}

/// What a run's steps did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RunCounts {
    pub commands: u32,
    /// Files changed, each counted once however often it changed.
    pub edits: u32,
    /// Files read, each counted once however often it was read.
    pub reads: u32,
    /// Searches of files or the web.
    pub searches: u32,
    /// Folders listed.
    pub lists: u32,
    /// Pages fetched.
    pub fetches: u32,
    /// Subagents run, started or stopped.
    pub subagents: u32,
    /// Tool-server calls, images and the rest.
    pub other: u32,
}

impl RunCounts {
    fn is_empty(&self) -> bool {
        *self == RunCounts::default()
    }
}

/// The run the item `held` sits in, stated for its row; `kind` and
/// `failed` are the row's own.
pub(crate) fn run_of(
    state: &SessionState,
    held: &Held,
    kind: &RowKind,
    failed: bool,
) -> Option<Run> {
    let transcript = state.transcript();
    let run = transcript.run_at(held.item.order)?;
    let order = held.item.order;
    let step = held.class.fold() == Fold::Step;
    let live = !run.closed;
    let recent = step.then(|| steps_after(state, &run, order)).flatten();
    let newest = run.newest == order;
    let live_shown = live && step && shows_live(held, &run);
    // One walk of the run serves both the newest step's counts and the
    // line above the oldest step a live run shows, which are often the
    // same step.
    let tally = (newest || live_shown).then(|| tally(state, &run, live));
    let hidden = tally
        .as_ref()
        .filter(|tally| live_shown && tally.first_shown == Some(order))
        .map(|tally| tally.hidden.clone())
        .filter(|hidden| !hidden.is_empty());
    let counts = tally.filter(|_| newest).map(|tally| tally.all);
    Some(Run {
        first: run.oldest_key.clone(),
        last: run.newest_key.clone(),
        steps: run.steps,
        live,
        open_below: run.open_below,
        recent,
        live_shown,
        hidden,
        unresolved_failure: step && (failed || failed_kind(kind)) && unresolved(state, held),
        counts,
        open: false,
    })
}

/// A live run's step shows when it is the newest or still running, so
/// calls made side by side all show while they run.
fn shows_live(held: &Held, run: &ui_state::Run) -> bool {
    held.item.order == run.newest || matches!(&held.class, ItemClass::Tool(tool) if tool.in_flight)
}

/// Whether the run the item at `order` sits in is open: `open` holds one
/// of its steps. Costs a lookup per held key, never a walk of the run.
pub fn run_is_open(state: &SessionState, order: u64, open: &HashSet<Key>) -> bool {
    let transcript = state.transcript();
    let Some(span) = transcript.run_span(order) else {
        return false;
    };
    open.iter().any(|key| {
        transcript
            .get(key)
            .is_some_and(|held| span.contains(&held.item.order))
    })
}

/// Opens the run `member` sits in, holding its newest step, or closes it,
/// forgetting every step of it the set holds.
pub fn toggle_run(state: &SessionState, member: &Key, open: &mut HashSet<Key>) {
    let transcript = state.transcript();
    let Some(span) = transcript
        .get(member)
        .and_then(|held| transcript.run_span(held.item.order))
    else {
        return;
    };
    let held: Vec<Key> = open
        .iter()
        .filter(|key| {
            transcript
                .get(key)
                .is_some_and(|held| span.contains(&held.item.order))
        })
        .cloned()
        .collect();
    if held.is_empty() {
        if let Some(newest) = transcript.at(*span.end()) {
            open.insert(newest.item.key.clone());
        }
    } else {
        for key in held {
            open.remove(&key);
        }
    }
}

/// Moves each hold on an open run to the run's newest step, so a window
/// that trims the run's oldest steps while it grows never drops the hold.
/// A key no longer held stays: its run opens again when it pages back in.
pub fn keep_open_runs(state: &SessionState, open: &mut HashSet<Key>) {
    let transcript = state.transcript();
    let moved: Vec<(Key, Key)> = open
        .iter()
        .filter_map(|key| {
            let span = transcript.run_span(transcript.get(key)?.item.order)?;
            let newest = &transcript.at(*span.end())?.item.key;
            (newest != key).then(|| (key.clone(), newest.clone()))
        })
        .collect();
    for (old, new) in moved {
        open.remove(&old);
        open.insert(new);
    }
}

/// How many steps follow the one at `order` in `run`, when it is among the
/// run's two newest; the walk stops there, so a long run costs no more.
fn steps_after(state: &SessionState, run: &ui_state::Run, order: u64) -> Option<u32> {
    let mut after = 0;
    for held in state.transcript().range(order..=run.newest).rev() {
        if held.class.fold() != Fold::Step {
            continue;
        }
        if held.item.order == order {
            return Some(after);
        }
        after += 1;
        if after >= RECENT_STEPS {
            return None;
        }
    }
    None
}

/// What a run's steps did: all of them, and, under way, those it does not
/// show; and the oldest step it shows.
struct Tally {
    all: RunCounts,
    hidden: RunCounts,
    first_shown: Option<u64>,
}

fn tally(state: &SessionState, run: &ui_state::Run, live: bool) -> Tally {
    let mut all = Counting::default();
    let mut hidden = Counting::default();
    let mut first_shown = None;
    for held in state.transcript().range(run.oldest..=run.newest) {
        if held.class.fold() != Fold::Step {
            continue;
        }
        let kind = kind_of(state, held).0;
        all.count(&kind);
        if live && shows_live(held, run) {
            first_shown.get_or_insert(held.item.order);
        } else {
            hidden.count(&kind);
        }
    }
    Tally {
        all: all.counts,
        hidden: hidden.counts,
        first_shown,
    }
}

/// Counts with the files already counted, so a file changed or read twice
/// counts once.
#[derive(Default)]
struct Counting {
    counts: RunCounts,
    edited: BTreeSet<String>,
    read: BTreeSet<String>,
}

impl Counting {
    fn count(&mut self, kind: &RowKind) {
        let counts = &mut self.counts;
        match kind {
            RowKind::Command { .. } | RowKind::Background { .. } => counts.commands += 1,
            RowKind::FileChange { files, .. } => {
                for file in files {
                    if self.edited.insert(file.path.clone()) {
                        counts.edits += 1;
                    }
                }
            }
            RowKind::Explore { verb, subject, .. } => match verb {
                ExploreVerb::Read => {
                    if self.read.insert(subject.clone()) {
                        counts.reads += 1;
                    }
                }
                ExploreVerb::Search | ExploreVerb::WebSearch => counts.searches += 1,
                ExploreVerb::List => counts.lists += 1,
                ExploreVerb::Fetch => counts.fetches += 1,
            },
            RowKind::Subagent { .. } | RowKind::Child { .. } => counts.subagents += 1,
            // Bookkeeping and amux's own look around draw nothing, so
            // they did nothing the person would count.
            RowKind::Hidden => {}
            _ => counts.other += 1,
        }
    }
}

/// A failed step stays unresolved when its turn has ended and no later
/// step of that turn with the same subject succeeded, as a lint rerun that
/// passes. Until the turn ends the agent may still fix it.
fn unresolved(state: &SessionState, failed: &Held) -> bool {
    let transcript = state.transcript();
    if !transcript.turn_ended_after(failed.item.order) {
        return false;
    }
    let subject = subject_of(failed);
    if subject.is_empty() {
        return true;
    }
    for later in transcript.range(failed.item.order + 1..=u64::MAX) {
        if later.class == ItemClass::Turn {
            break;
        }
        if later.class.fold() != Fold::Step || subject_of(later) != subject {
            continue;
        }
        let (kind, _, did_fail) = kind_of(state, later);
        if !did_fail && !failed_kind(&kind) {
            return false;
        }
    }
    true
}

/// Whether a step's own facts say it failed: its state, or a command's
/// non-zero exit.
fn failed_kind(kind: &RowKind) -> bool {
    match kind {
        RowKind::Command {
            state, exit_code, ..
        } => *state == CallPhase::Failed || exit_code.is_some_and(|code| code != 0),
        RowKind::Explore { state, .. }
        | RowKind::ToolCall { state, .. }
        | RowKind::Child { state, .. }
        | RowKind::FileChange { state, .. } => *state == CallPhase::Failed,
        _ => false,
    }
}
