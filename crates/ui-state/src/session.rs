//! One open chat's state: the agent's entry and snapshot, the transcript
//! window, and this client's inputs.

mod history;

use std::collections::{BTreeMap, BTreeSet};

pub use model::{Activity, ActivityKind, BlobStatus, Composer, Connection, PhaseView, Waiting};
use wire::{Agent, HostEntry, Input, Item, Kind, QueuedInput, SessionEvent, session_event};

use crate::Key;
use crate::body::{AgentState, ItemClass, OpenAsk};
use crate::inputs::{
    InputId, InputOutcome, InputState, InputWhat, Inputs, RefusalReason, SentInput,
};
use crate::transcript::{Appended, Changed, Held, Transcript};

/// Everything the driver forwards to a session.
#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    Event(SessionEvent),
    /// An older page, requested under `epoch`. A page requested before a
    /// Reset swap describes a transcript that is gone and is dropped.
    Page {
        items: Vec<Item>,
        exhausted: bool,
        epoch: u64,
    },
    Connection(Connection),
    /// This client is sending an input: the optimistic row exists from now.
    Send(Input),
    /// What became of it.
    Sent(InputId, InputOutcome),
    /// The person discarded an input that was not confirmed.
    Discard(InputId),
    /// The agent's inventory row: lifecycle and the fleet's phase.
    Entry(Agent),
    /// The agent's host, for the header.
    Host(HostEntry),
    /// An attachment's bytes changed state in the driver's blob cache.
    Blob {
        hash: Vec<u8>,
        status: BlobStatus,
    },
    /// The reader left the newest row (false) or returned to it (true).
    /// Only the client knows where its reader is; this is how the window
    /// learns whether to trim its top or to hold what arrives.
    Following(bool),
    /// The driver reopened the stream for the reload a return asked for:
    /// what follows is a fresh tail, built apart and swapped in at CaughtUp.
    Reloading,
    /// The window's cap changed: a chat opened on screen widens it, and a
    /// session the fleet holds off screen narrows to a few rows, trimmed at
    /// once while the reader follows.
    Window(usize),
    /// A catalogue the driver fetched or held for the agent, with the hash
    /// it was fetched by.
    Catalogue {
        hash: Vec<u8>,
        catalogue: wire::Catalogue,
    },
}

/// What one update did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Every key whose row may differ, including a run's members when its
    /// attributes moved, and nothing else.
    pub changed: Vec<Key>,
    /// An Append to a held item at another revision than its base: the
    /// driver answers with Get and feeds the item back. An Append for a key
    /// the client holds nothing for is dropped instead.
    pub need_get: Option<Key>,
    /// A Reset's transcript was swapped in: every row id may be new.
    pub reloaded: bool,
    /// Something outside the rows moved: the snapshot, entry, host, inputs,
    /// connection or caught-up state.
    pub session: bool,
    /// The reader returned after the head moved on past what the session
    /// holds: the driver reopens the stream with a fresh tail and says so
    /// with [`Msg::Reloading`].
    pub reload: bool,
    /// Held inputs the host can be reached for again: each is `Sent` from
    /// now, and the driver sends it under its own id.
    pub resend: Vec<Input>,
    /// The transcript failed its own check at a Reset or a CaughtUp, saying
    /// how: the driver asks for a dump.
    pub broken: Option<String>,
}

/// A queued prompt as the composer draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct QueueRow<'a> {
    pub entry: &'a QueuedInput,
    /// Sent now into the running turn, by the queue's word or by this
    /// client's Send now in flight; reads "steered" until its reflection.
    pub steered: bool,
}

/// One open chat. No I/O, no clock, no handles, no view state; never
/// persisted, because the runtime can re-serve every record it was built
/// from.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionState {
    agent: Agent,
    host: Option<HostEntry>,
    state: AgentState,
    /// The newest snapshot as it arrived: `state` is decoded from it, and a
    /// dump carries it so a replay decodes the same.
    snapshot: Option<wire::Snapshot>,
    /// The catalogue last fetched for the agent, with the hash it was
    /// fetched by; what it offers shows while
    /// the newest snapshot names its hash.
    catalogue: Option<(Vec<u8>, wire::Catalogue)>,
    transcript: Transcript,
    /// A Reset's fresh transcript, or a re-tail that does not meet the held
    /// window, swapped in at the next CaughtUp.
    pending: Option<Transcript>,
    /// The stream was reopened; a tail that leaves a gap above the head
    /// builds a pending transcript instead of appending.
    retail: bool,
    /// The origin generation the window's rows belong to, or the pending
    /// transcript's while one builds: from the first stream's Opening, then
    /// from each Reset, which names the block it replaces the rows with.
    generation: Option<u64>,
    inputs: Inputs,
    caught_up: bool,
    detached: bool,
    connection: Connection,
    /// The incarnation a Send was rejected as exited under.
    exited_under: Option<u32>,
    /// Words that the host can be reached: each time its row reads online,
    /// and each CaughtUp. A held input goes at the first one after it left.
    reach: u64,
    epoch: u64,
    blobs: BTreeMap<Vec<u8>, BlobStatus>,
    /// The most rows the window keeps while the reader follows.
    cap: usize,
    /// The reader is at the newest row, as the client last said.
    following: bool,
    /// What arrived above the head while the reader was in history.
    arrivals: Arrivals,
    /// The reader returned after the head moved on: a reload is owed, and
    /// the driver has not reopened the stream for it yet.
    reload_owed: bool,
}

/// Rows that arrived above the window's head while the reader was in
/// history, held apart so the window the reader is in does not move. They
/// continue the window without a gap, at most the window's cap of them;
/// past that, or past a gap, the session holds nothing more and remembers
/// only that the head moved on.
#[derive(Clone, Debug, Default, PartialEq)]
struct Arrivals {
    rows: BTreeMap<u64, Held>,
    by_key: BTreeMap<Key, u64>,
    moved_on: bool,
    /// Counts every change, so an update can say the activity line and the
    /// affordance, which read these rows, may have moved.
    changes: u64,
}

impl Arrivals {
    fn clear(&mut self) {
        let changes = self.changes;
        *self = Arrivals::default();
        self.changes = changes + 1;
    }

    fn get(&self, key: &str) -> Option<&Held> {
        self.by_key.get(key).and_then(|order| self.rows.get(order))
    }

    fn key_for_input(&self, input_id: &[u8]) -> bool {
        self.rows
            .values()
            .any(|held| held.item.input_id == input_id)
    }

    /// Holds a row, or gives up holding when it would leave a gap or pass
    /// the cap.
    fn hold(&mut self, kind: Kind, item: Item, head: u64, cap: usize) {
        if self.moved_on {
            return;
        }
        if let Some(&order) = self.by_key.get(&item.key) {
            if self.rows[&order].item.revision < item.revision {
                let mut item = item;
                item.order = order;
                self.rows.insert(order, Held::new(kind, item));
                self.changes += 1;
            }
            return;
        }
        let next = self.rows.keys().next_back().copied().unwrap_or(head) + 1;
        if item.order < next {
            // One key per order: another key at a held order is not this
            // agent's history.
            return;
        }
        if item.order > next || self.rows.len() >= cap {
            self.clear();
            self.moved_on = true;
            return;
        }
        self.by_key.insert(item.key.clone(), item.order);
        self.rows.insert(item.order, Held::new(kind, item));
        self.changes += 1;
    }

    fn append(&mut self, kind: Kind, append: &wire::Append) -> Appended {
        let Some(&order) = self.by_key.get(&append.key) else {
            return Appended::Dropped;
        };
        let held = &self.rows[&order];
        if held.item.revision >= append.revision {
            return Appended::Dropped;
        }
        if held.item.revision != append.base_revision {
            return Appended::NeedGet;
        }
        let mut item = held.item.clone();
        item.text.push_str(&append.text);
        item.revision = append.revision;
        self.rows.insert(order, Held::new(kind, item));
        self.changes += 1;
        Appended::Applied
    }
}

impl SessionState {
    /// A session following the newest row, whose window keeps at most `cap`
    /// rows while it does.
    pub fn new(agent: Agent, cap: usize) -> SessionState {
        let kind = agent.kind();
        SessionState {
            // Until a snapshot arrives the phase is the entry's (see
            // `phase`), so this one is never read.
            state: AgentState {
                kind,
                ..AgentState::default()
            },
            agent,
            host: None,
            snapshot: None,
            catalogue: None,
            transcript: Transcript::new(kind),
            pending: None,
            retail: false,
            generation: None,
            inputs: Inputs::default(),
            caught_up: false,
            detached: false,
            connection: Connection::Connecting,
            exited_under: None,
            reach: 0,
            epoch: 0,
            blobs: BTreeMap::new(),
            cap: cap.max(1),
            following: true,
            arrivals: Arrivals::default(),
            reload_owed: false,
        }
    }

    /// The most rows the window keeps while the reader follows.
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Whether the reader is at the newest row, as the client last said.
    pub fn following(&self) -> bool {
        self.following
    }

    /// Rows arrived above the window while the reader is in history: what
    /// the new-activity affordance shows from.
    pub fn arrivals_held(&self) -> bool {
        !self.following && (!self.arrivals.rows.is_empty() || self.arrivals.moved_on)
    }

    /// How many rows arrived and are held outside the window.
    pub fn held_arrivals(&self) -> usize {
        self.arrivals.rows.len()
    }

    /// Whether the head moved on past what the session holds, so a return
    /// to the newest row reloads it.
    pub fn head_moved_on(&self) -> bool {
        self.arrivals.moved_on
    }

    /// How many older rows a page may bring: while the reader follows, only
    /// what fits under the cap, so a following client never fetches rows
    /// the next live row would trim; in history, any number.
    pub fn page_room(&self) -> Option<usize> {
        self.following
            .then(|| self.cap.saturating_sub(self.transcript.len()))
    }

    pub fn kind(&self) -> Kind {
        self.agent.kind()
    }

    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    pub fn host(&self) -> Option<&HostEntry> {
        self.host.as_ref()
    }

    /// The newest snapshot, decoded, with what the agent offers when its
    /// catalogue is held.
    pub fn agent_state(&self) -> &AgentState {
        &self.state
    }

    /// A catalogue fetched for the agent under `hash`, the hash it was
    /// fetched by. What it offers shows while the newest snapshot names it.
    /// Fetches can answer out of order, so a held catalogue the newest
    /// snapshot names is never replaced by one it does not name; any other
    /// is. Returns whether it was kept.
    pub fn set_catalogue(&mut self, hash: Vec<u8>, catalogue: wire::Catalogue) -> bool {
        let named = |hash: &[u8]| self.state.catalogue.as_deref() == Some(hash);
        if self.catalogue.as_ref().is_some_and(|(held, _)| named(held)) && !named(&hash) {
            return false;
        }
        self.catalogue = Some((hash, catalogue));
        self.offer();
        true
    }

    /// The hash of the catalogue held, whether or not the newest snapshot
    /// still names it.
    pub fn held_catalogue(&self) -> Option<&[u8]> {
        self.catalogue.as_ref().map(|(hash, _)| hash.as_slice())
    }

    fn offer(&mut self) {
        let offered = match &self.catalogue {
            Some((hash, catalogue)) if self.state.catalogue.as_ref() == Some(hash) => {
                catalogue.clone()
            }
            _ => wire::Catalogue::default(),
        };
        self.state.models = offered.models;
        self.state.commands = offered.commands;
        self.state.permissions = offered.permissions;
        self.state.modes = offered.modes;
    }

    pub fn has_snapshot(&self) -> bool {
        self.snapshot.is_some()
    }

    /// The window on screen. During a Reset it is the old one, until the
    /// next CaughtUp swaps the fresh one in.
    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    pub fn reset_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn inputs(&self) -> &Inputs {
        &self.inputs
    }

    pub fn caught_up(&self) -> bool {
        self.caught_up
    }

    /// The origin generation the rows belong to, once a stream named one.
    pub fn generation(&self) -> Option<u64> {
        self.generation
    }

    pub fn connection(&self) -> Connection {
        self.connection
    }

    /// Stamped on page requests; bumped at every Reset swap.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The page-older cursor.
    pub fn oldest_order(&self) -> Option<u64> {
        self.transcript.oldest_held()
    }

    pub fn blob(&self, hash: &[u8]) -> BlobStatus {
        self.blobs.get(hash).cloned().unwrap_or_default()
    }

    pub fn input_state(&self, id: &[u8]) -> Option<InputState> {
        self.inputs.get(id).map(|sent| sent.state.clone())
    }

    fn exited(&self) -> bool {
        self.agent.lifecycle() == wire::Lifecycle::Exited || self.exited_under.is_some()
    }

    /// Caught up and the entry says live. Drafting is never gated.
    /// Whether a send goes to the runtime now. While the chat is not
    /// current a prompt or an answer is held instead (see [`Composer::Holds`]).
    pub fn can_send(&self) -> bool {
        self.caught_up && !self.exited()
    }

    pub fn composer(&self) -> Composer {
        if self.exited() {
            Composer::Resume
        } else if self.caught_up {
            Composer::Send
        } else if self.connection == Connection::Reconnecting {
            Composer::Holds(Waiting::Reconnecting)
        } else if self.detached {
            Composer::Holds(Waiting::Detached)
        } else {
            Composer::Holds(Waiting::CatchingUp)
        }
    }

    pub fn phase(&self) -> PhaseView {
        if self.exited() {
            return PhaseView::Exited {
                cause: crate::exit_cause(&self.agent),
            };
        }
        let phase = if self.has_snapshot() {
            self.state.phase
        } else {
            self.agent.phase()
        };
        match phase {
            wire::Phase::Starting => PhaseView::Starting,
            wire::Phase::Idle => PhaseView::Idle,
            wire::Phase::Working => PhaseView::Working,
            wire::Phase::NeedsYou => PhaseView::NeedsYou,
        }
    }

    /// The asks to draw, head first. Before CaughtUp an ask is drawn only if
    /// the entry says needs_you, so an ask answered elsewhere does not flash
    /// from a cached snapshot.
    pub fn open_asks(&self) -> &[OpenAsk] {
        if !self.caught_up && !self.exited() && self.agent.phase() != wire::Phase::NeedsYou {
            return &[];
        }
        &self.state.asks
    }

    /// An exited agent's open asks are drawn dismissed.
    pub fn asks_dismissed(&self) -> bool {
        self.exited()
    }

    /// This client's newest answer to an ask, while the card shows it.
    pub fn answering(&self, ask_key: &str) -> Option<&SentInput> {
        self.inputs
            .iter()
            .rev()
            .find(|sent| matches!(&sent.what, InputWhat::Answer { ask_key: key } if key == ask_key))
    }

    /// The shared queue as the composer draws it. An entry this client holds
    /// as uncertain is not shown as queued: it is drawn once, as not
    /// confirmed, until CaughtUp resolves it.
    pub fn queue(&self) -> Vec<QueueRow<'_>> {
        self.state
            .queue
            .iter()
            .filter_map(|entry| {
                let own = self.inputs.get(&entry.input_id);
                if own.is_some_and(|sent| sent.state == InputState::Uncertain) {
                    return None;
                }
                let sending_now = self.inputs.iter().any(|sent| {
                    sent.state == InputState::Sent
                        && matches!(&sent.what, InputWhat::SendNow { target } if *target == entry.input_id)
                });
                Some(QueueRow { entry, steered: entry.steer || sending_now })
            })
            .collect()
    }

    /// Inputs held to send once the host is back, drawn pending with Cancel.
    pub fn held(&self) -> impl Iterator<Item = &SentInput> {
        self.inputs
            .iter()
            .filter(|sent| sent.state == InputState::Held)
    }

    /// Inputs to draw as "not confirmed", with resend and discard.
    pub fn not_confirmed(&self) -> impl Iterator<Item = &SentInput> {
        self.inputs
            .iter()
            .filter(|sent| sent.state == InputState::Uncertain)
    }

    /// Prompts to draw optimistically: sent and not yet reflected or queued.
    pub fn sending(&self) -> impl Iterator<Item = &SentInput> {
        self.inputs.iter().filter(|sent| {
            sent.state == InputState::Sent && matches!(sent.what, InputWhat::Prompt { .. })
        })
    }

    /// The activity line, derived from the newest items and the phase.
    pub fn activity(&self, now_ms: i64) -> Option<Activity> {
        if self.exited() || self.phase() != PhaseView::Working {
            return None;
        }
        let transcript = &self.transcript;
        // Arrivals held above the window are the newest rows there are.
        let newest_first = || {
            self.arrivals
                .rows
                .values()
                .rev()
                .chain(transcript.iter().rev())
        };
        let running_tasks = self
            .state
            .active_tasks
            .iter()
            .filter(|task| task.state() == wire::TaskState::Running)
            .count() as u32;
        let mut subagents = 0u32;
        let mut turn_start = None;
        for held in newest_first() {
            match &held.class {
                ItemClass::Tool(tool) if tool.subagent => subagents += 1,
                ItemClass::Prompt | ItemClass::Steer | ItemClass::AgentMessage => {
                    turn_start = Some(held.item.at_ms);
                    break;
                }
                ItemClass::Turn | ItemClass::Boundary => break,
                _ => {}
            }
        }
        let subagents = subagents.max(running_tasks);
        let turn_start = turn_start.unwrap_or(self.state.at_ms);
        let at = |since_ms: i64, kind| Activity {
            kind,
            since_ms,
            elapsed_ms: (now_ms - since_ms).max(0),
            turn_elapsed_ms: (now_ms - turn_start).max(0),
        };
        // A call whose hook announced it and whose row has not landed, or
        // landed after it: terminal Claude writes a call's row up to
        // seconds after the call starts.
        let announced = self.state.running_calls.last().filter(|call| {
            !newest_first().any(|held| {
                matches!(&held.class, ItemClass::Tool(tool) if tool.in_flight)
                    && held.item.at_ms > call.since_ms
            })
        });
        let Some(newest) = newest_first().find(|held| held.class != ItemClass::Ask) else {
            return Some(match announced {
                Some(call) => at(
                    call.since_ms,
                    ActivityKind::Running {
                        key: call.tool_use_id.clone(),
                    },
                ),
                None => at(turn_start, ActivityKind::Working),
            });
        };
        let since = newest.item.at_ms;
        Some(match &newest.class {
            ItemClass::Retrying {
                attempt,
                max_attempts,
                retry_at_ms,
            } => at(
                since,
                ActivityKind::Retrying {
                    attempt: *attempt,
                    max_attempts: *max_attempts,
                    retry_at_ms: *retry_at_ms,
                },
            ),
            ItemClass::Compacting => at(since, ActivityKind::Compacting),
            ItemClass::Thinking { complete: false } => at(since, ActivityKind::Thinking),
            _ if let Some(call) = announced => at(
                call.since_ms,
                ActivityKind::Running {
                    key: call.tool_use_id.clone(),
                },
            ),
            ItemClass::Tool(tool) if tool.in_flight && !tool.subagent => at(
                since,
                ActivityKind::Running {
                    key: newest.item.key.clone(),
                },
            ),
            _ if subagents > 0 => at(turn_start, ActivityKind::Subagents { count: subagents }),
            _ => at(turn_start, ActivityKind::Working),
        })
    }

    pub fn update(&mut self, msg: Msg) -> Outcome {
        let mut outcome = Outcome::default();
        let mut changed = Changed::default();
        match msg {
            Msg::Event(event) => self.event(event, &mut changed, &mut outcome),
            Msg::Page {
                items,
                exhausted,
                epoch,
            } => {
                if epoch == self.epoch {
                    self.transcript.page(items, exhausted, &mut changed);
                }
            }
            Msg::Connection(connection) => {
                self.connection = connection;
                if connection == Connection::Reconnecting {
                    self.caught_up = false;
                    self.reopened();
                }
                outcome.session = true;
            }
            Msg::Send(input) => {
                let id = input.input_id.clone();
                outcome.session = self.inputs.push(input);
                if outcome.session
                    && let Some(sent) = self.inputs.get_mut(&id)
                {
                    sent.left_at = self.reach;
                }
                // Not current, a prompt or an answer certainly cannot leave:
                // it is held, and goes once the chat is current again.
                let hold = !self.caught_up && !self.exited();
                if let Some(sent) = self.inputs.get_mut(&id)
                    && hold
                    && sent.state == InputState::Sent
                    && sent.what.holds()
                {
                    sent.state = InputState::Held;
                }
                // A reflection can beat the optimistic row only on a resend
                // of a known id, which the push refuses.
            }
            Msg::Sent(id, result) => {
                outcome.session = self.sent(&id, result);
                // Held by a failure that answers a send which left before
                // the newest word that the host is back: that word came
                // already, and the input goes now.
                if outcome.session && self.caught_up && self.host_online() {
                    outcome.resend = self.release_held();
                }
            }
            Msg::Discard(id) => outcome.session = self.inputs.remove(&id),
            Msg::Entry(agent) => {
                if agent.lifecycle() == wire::Lifecycle::Live
                    && self
                        .exited_under
                        .is_some_and(|incarnation| agent.incarnation > incarnation)
                {
                    self.exited_under = None;
                }
                self.agent = agent;
                outcome.session = true;
            }
            // Every word that the host is online counts, not only a return
            // from away: a send can fail as unreachable after the host's
            // return already arrived, or while its row still read online.
            Msg::Host(host) => {
                self.host = Some(host);
                if self.host_online() {
                    self.reach += 1;
                    if self.caught_up {
                        outcome.resend = self.release_held();
                    }
                }
                outcome.session = true;
            }
            Msg::Blob { hash, status } => {
                if self.blob(&hash) != status {
                    for held in self.transcript.iter() {
                        if references(&held.item, &hash) {
                            changed.key(&held.item.key);
                        }
                    }
                    self.blobs.insert(hash, status);
                }
            }
            Msg::Following(following) => self.follow(following, &mut changed, &mut outcome),
            Msg::Window(cap) => {
                self.cap = cap.max(1);
                self.trim(&mut changed);
            }
            Msg::Catalogue { hash, catalogue } => {
                outcome.session = self.set_catalogue(hash, catalogue);
            }
            Msg::Reloading => {
                // Every reopened stream re-tails; a reload builds apart.
                self.retail = true;
                if self.reload_owed {
                    self.reload_owed = false;
                    self.pending = Some(Transcript::new(self.kind()));
                    outcome.session = true;
                }
            }
        }
        outcome.changed = changed.into_keys();
        outcome
    }

    fn event(&mut self, event: SessionEvent, changed: &mut Changed, outcome: &mut Outcome) {
        let Some(event) = event.of else { return };
        let arrivals = self.arrivals.changes;
        self.apply_event(event, changed, outcome);
        if self.arrivals.changes != arrivals {
            outcome.session = true;
        }
    }

    fn apply_event(
        &mut self,
        event: session_event::Of,
        changed: &mut Changed,
        outcome: &mut Outcome,
    ) {
        use session_event::Of;
        match event {
            Of::Snapshot(snapshot) => {
                self.state = AgentState::from_snapshot(self.kind(), &snapshot);
                self.offer();
                self.snapshot = Some(snapshot);
                self.queue_moved();
                outcome.session = true;
            }
            Of::Item(item) => {
                let settles = !item.input_id.is_empty();
                let input_id = item.input_id.clone();
                let visible = self.item(item, changed);
                if settles && self.reflected(&input_id, visible) {
                    outcome.session = true;
                }
            }
            Of::Append(append) => {
                let kind = self.kind();
                let target = if let Some(pending) = &mut self.pending {
                    pending.append(&append, &mut Changed::default())
                } else if self.transcript.get(&append.key).is_some() {
                    self.transcript.append(&append, changed)
                } else if self.arrivals.get(&append.key).is_some() {
                    self.arrivals.append(kind, &append)
                } else {
                    // Nothing held for the key: the row is below the window
                    // or let go of, and comes whole when it is read again.
                    Appended::Dropped
                };
                if target == Appended::NeedGet {
                    outcome.need_get = Some(append.key);
                }
            }
            Of::CaughtUp(_) => {
                self.caught_up = true;
                self.detached = false;
                self.retail = false;
                if let Some(fresh) = self.pending.take() {
                    self.swap(fresh, changed);
                    outcome.reloaded = true;
                }
                self.trim(changed);
                self.resolve_uncertain();
                self.reach += 1;
                outcome.resend = self.release_held();
                outcome.session = true;
                outcome.broken = self.transcript.check().err();
            }
            Of::Lagged(_) => {
                self.caught_up = false;
                self.reopened();
                outcome.session = true;
            }
            Of::Reset(reset) => {
                // The transcript as it stood before the origin's history
                // changed under it.
                outcome.broken = self.transcript.check().err();
                self.caught_up = false;
                self.generation = Some(reset.generation);
                self.pending = Some(Transcript::new(self.kind()));
                outcome.session = true;
            }
            Of::Detached(_) => {
                self.caught_up = false;
                self.detached = true;
                outcome.session = true;
            }
            // A stream reopened under another generation follows a Reset
            // the gap in the stream may have carried: the rows held are of
            // a history the origin no longer has, so the tail is built
            // apart and swapped in at CaughtUp.
            Of::Opening(opening) => {
                let known = self.generation.replace(opening.generation);
                if known.is_some_and(|known| known != opening.generation) {
                    self.caught_up = false;
                    self.pending = Some(Transcript::new(self.kind()));
                    outcome.session = true;
                }
            }
        }
    }

    /// Applies an item to the transcript it belongs to; true when that is the
    /// one on screen.
    fn item(&mut self, item: Item, changed: &mut Changed) -> bool {
        if let Some(pending) = &mut self.pending {
            pending.upsert(item, &mut Changed::default());
            return false;
        }
        let kind = self.kind();
        if self.transcript.get(&item.key).is_none()
            && let Some(head) = self.transcript.head()
            && item.order > head
        {
            if self.arrivals.get(&item.key).is_some() || !self.following {
                // In history the window stays put: what arrives above it
                // is held apart.
                self.arrivals.hold(kind, item, head, self.cap);
                return false;
            }
            if self.arrivals.moved_on {
                // Back at the newest row with the reload on its way.
                return false;
            }
        }
        let gap = self.retail
            && self.transcript.get(&item.key).is_none()
            && self
                .transcript
                .head()
                .is_some_and(|head| item.order > head + 1);
        if gap {
            // A re-tail that does not meet the held window: build it apart
            // and swap it in at CaughtUp, as for a Reset.
            let mut fresh = Transcript::new(self.kind());
            fresh.upsert(item, &mut Changed::default());
            self.pending = Some(fresh);
            return false;
        }
        self.transcript.upsert(item, changed);
        self.trim(changed);
        true
    }

    /// The stream ended and the driver opens it again with a fresh tail. A
    /// transcript being built apart starts over: the tail that follows is
    /// its whole, and what it held may not meet it.
    fn reopened(&mut self) {
        self.retail = true;
        let kind = self.kind();
        if let Some(pending) = &mut self.pending {
            *pending = Transcript::new(kind);
        }
    }

    /// While the reader follows, the window keeps only the newest rows.
    fn trim(&mut self, changed: &mut Changed) {
        if self.following && self.pending.is_none() {
            self.transcript.trim(self.cap, changed);
        }
    }

    /// The reader moved to or from the newest row. Leaving holds what
    /// arrives from now; returning releases what was held when it all was
    /// and continues the window, and otherwise asks the driver for a reload.
    fn follow(&mut self, following: bool, changed: &mut Changed, outcome: &mut Outcome) {
        if self.following == following {
            return;
        }
        self.following = following;
        outcome.session = true;
        if !following {
            return;
        }
        if self.arrivals.moved_on {
            self.reload_owed = true;
            outcome.reload = true;
            return;
        }
        let held = std::mem::take(&mut self.arrivals);
        for (_, held) in held.rows {
            self.transcript.upsert(held.item, changed);
        }
        // A window grown past the cap by paging is trimmed here too, which
        // fetches nothing and keeps the block contiguous.
        self.trim(changed);
    }

    fn swap(&mut self, fresh: Transcript, changed: &mut Changed) {
        self.arrivals.clear();
        self.reload_owed = false;
        let old = std::mem::replace(&mut self.transcript, fresh);
        let new = &self.transcript;
        let keys: BTreeSet<&Key> = old.keys().chain(new.keys()).collect();
        for key in keys {
            let before = old.get(key).map(|held| (held, old.run_at(held.item.order)));
            let after = new.get(key).map(|held| (held, new.run_at(held.item.order)));
            if before != after {
                changed.key(key);
            }
        }
        self.epoch += 1;
    }

    /// A reflection carries an input id: this client's prompt landed.
    fn reflected(&mut self, input_id: &[u8], visible: bool) -> bool {
        let caught_up = self.caught_up;
        let Some(sent) = self.inputs.get_mut(input_id) else {
            return false;
        };
        let settle = match sent.state {
            InputState::Sent | InputState::Queued | InputState::Held => true,
            // An uncertain input is judged against caught-up state only.
            InputState::Uncertain => caught_up && visible,
            InputState::Settled | InputState::Rejected(_) => false,
        };
        if settle {
            sent.state = InputState::Settled;
        }
        settle
    }

    fn sent(&mut self, id: &[u8], result: InputOutcome) -> bool {
        use wire::send_input_response::Of;
        let incarnation = self.agent.incarnation;
        let reflected =
            self.transcript.key_for_input(id).is_some() || self.arrivals.key_for_input(id);
        let listed = self.state.queue.iter().any(|entry| entry.input_id == id);
        let Some(sent) = self.inputs.get_mut(id) else {
            // A late result for an input this open never sent.
            return false;
        };
        if sent.state != InputState::Sent {
            return false;
        }
        let holds = sent.what.holds();
        sent.state = match result {
            InputOutcome::Lost => InputState::Uncertain,
            InputOutcome::Held => InputState::Held,
            InputOutcome::Reply(reply) => match reply.of {
                // Queued while the queue lists it, or until it first does;
                // seen listed and gone again before this reply, it already
                // left (withdrawn or submitted).
                Some(Of::Accepted(accepted)) if accepted.queued => {
                    if listed || (!sent.seen_queued && !reflected) {
                        InputState::Queued
                    } else {
                        InputState::Settled
                    }
                }
                Some(Of::Accepted(_)) => match sent.what {
                    InputWhat::Prompt { .. } if !reflected => InputState::Sent,
                    _ => InputState::Settled,
                },
                // Refused before it was written anywhere: it never left, and
                // goes again once the host can be reached.
                Some(Of::Rejected(rejected))
                    if holds && rejected.refusal() == wire::Refusal::HostUnreachable =>
                {
                    InputState::Held
                }
                Some(Of::Rejected(rejected)) => {
                    let refusal = crate::refusal(&rejected);
                    if refusal.reason == RefusalReason::Exited {
                        self.exited_under = Some(incarnation);
                    }
                    InputState::Rejected(refusal)
                }
                None => InputState::Uncertain,
            },
        };
        true
    }

    /// A new snapshot: inputs it lists were seen queued, even before the
    /// reply accepting them arrived; a queued one seen before and gone now
    /// was submitted or withdrawn, and is settled either way.
    fn queue_moved(&mut self) {
        let queue = &self.state.queue;
        for sent in self.inputs.iter_mut() {
            let listed = queue.iter().any(|entry| entry.input_id == sent.id);
            match sent.state {
                InputState::Sent | InputState::Queued if listed => sent.seen_queued = true,
                InputState::Queued if sent.seen_queued => sent.state = InputState::Settled,
                _ => {}
            }
        }
    }

    fn host_online(&self) -> bool {
        self.host
            .as_ref()
            .is_some_and(|host| host.presence() == wire::Presence::Online)
    }

    /// The held inputs that left before the newest word that the host can
    /// be reached, sent from now: the chat is current again or the agent's
    /// host is online. One held under the newest word waits for the next,
    /// so a host that cannot take it is not asked again and again. One that
    /// already landed (its reflection or queue entry is here) is settled
    /// instead of sent twice.
    fn release_held(&mut self) -> Vec<Input> {
        let queue = &self.state.queue;
        let transcript = &self.transcript;
        let arrivals = &self.arrivals;
        let reach = self.reach;
        let mut resend = Vec::new();
        for sent in self.inputs.iter_mut() {
            if sent.state != InputState::Held || sent.left_at == reach {
                continue;
            }
            if queue.iter().any(|entry| entry.input_id == sent.id) {
                sent.state = InputState::Queued;
                sent.seen_queued = true;
            } else if transcript.key_for_input(&sent.id).is_some()
                || arrivals.key_for_input(&sent.id)
            {
                sent.state = InputState::Settled;
            } else {
                sent.state = InputState::Sent;
                sent.left_at = reach;
                resend.push(sent.input.clone());
            }
        }
        resend
    }

    /// At CaughtUp: an uncertain input found in the queue is queued, one found
    /// among the items is settled, and the rest stay for the person.
    fn resolve_uncertain(&mut self) {
        let queue = &self.state.queue;
        let transcript = &self.transcript;
        let arrivals = &self.arrivals;
        for sent in self.inputs.iter_mut() {
            if sent.state != InputState::Uncertain {
                continue;
            }
            if queue.iter().any(|entry| entry.input_id == sent.id) {
                sent.state = InputState::Queued;
                sent.seen_queued = true;
            } else if transcript.key_for_input(&sent.id).is_some()
                || arrivals.key_for_input(&sent.id)
            {
                sent.state = InputState::Settled;
            }
        }
    }
}

fn references(item: &Item, hash: &[u8]) -> bool {
    use wire::attachment::Of;
    item.attachments
        .iter()
        .any(|attachment| match &attachment.of {
            Some(Of::Image(blob) | Of::File(blob)) => blob.hash == hash,
            Some(Of::Review(review)) => review
                .diff
                .as_ref()
                .and_then(|diff| diff.patch.as_ref())
                .is_some_and(|patch| patch.hash == hash),
            Some(Of::Text(_)) | None => false,
        })
}
