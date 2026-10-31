//! One open chat on a client: the session state both clients reduce their
//! streams into, fed by one agent's daemon through a subscription.
//!
//! The client is the real `ui_state::SessionState`, updated with the
//! messages its driver forwards. The world around it is the environment:
//! the daemon writing the agent's items (a new item, an append to a text
//! item, a full revision, a snapshot), its stream carrying each write in
//! order, the stream falling behind (Lagged) and the driver reopening it
//! with a fresh tail, the origin's generation changing under a replica
//! (Reset, a fresh tail and the newest item gone), the client asking for an
//! older page and the answer arriving later, and this client's one input:
//! sent, reaching the daemon or not, answered accepted or lost on the way,
//! and discarded by the person while it is unconfirmed. An append whose
//! base the client does not hold is answered with Get before the next
//! event, as the driver does.
//!
//! Beside the client the state keeps a client that received every event
//! uninterrupted (the whole), which is what a catch-up plus the live
//! stream must read as, and the session as its stream last opened with the
//! messages since, as a client's dump records them, which must replay to
//! the live session.

use std::collections::VecDeque;
use std::fmt;
use std::hash::{Hash, Hasher};

use prost::Message as _;
use ui_state::{InputOutcome, InputState, Msg, SessionState};
use wire::{
    Agent, Append, ClaudeItem, Item, Kind, Lifecycle, Phase, SendInputResponse, SessionEvent,
    Snapshot, claude_item, session_event,
};

use crate::{Machine, Property};

const KIND: Kind = Kind::ClaudeSdk;
const AGENT_ID: &[u8] = b"agent-0000000000";
/// The one input this client sends.
const INPUT: &[u8] = b"input-1";
/// The items the daemon holds when the chat opens.
const OPENING_ITEMS: u64 = 3;

/// One open chat, within its bounds.
#[derive(Clone, Debug)]
pub struct ClientModel {
    /// Items, appends, revisions and snapshots the daemon writes, all told.
    pub writes: u8,
    /// Times the stream falls behind and is reopened.
    pub lags: u8,
    /// Times the origin's generation changes under the daemon.
    pub resets: u8,
    /// Older pages the client asks for.
    pub pages: u8,
    /// How many of the newest items a subscription's tail carries.
    pub tail: usize,
    /// How many items an older page carries.
    pub page: usize,
}

impl ClientModel {
    /// The bounds `just test` runs: two writes, one lag, one reset and two
    /// pages over a three-item opening, a tail of two and pages of one.
    pub const fn ci() -> Self {
        Self {
            writes: 2,
            lags: 1,
            resets: 1,
            pages: 2,
            tail: 2,
            page: 1,
        }
    }

    /// The nightly bounds: a third write.
    pub const fn nightly() -> Self {
        Self {
            writes: 3,
            ..Self::ci()
        }
    }
}

/// The world's and the client's moves.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// The daemon writes a new text item.
    NewItem,
    /// The daemon appends to the text item at this order.
    Append(u64),
    /// The daemon revises the item at this order whole.
    Revise(u64),
    /// The daemon writes a snapshot with the other phase.
    Snapshot,
    /// The next message on the stream reaches the client: an item, an
    /// append, a snapshot or a Reset.
    Event,
    /// The next message on the stream is the CaughtUp marker.
    CaughtUp,
    /// The stream falls behind: what it held is dropped and Lagged closes
    /// it.
    Lagged,
    /// The origin's generation changes: the daemon's newest item is gone
    /// and every other is revised; the stream carries what a replica
    /// publishes for the new block: Reset naming its generation, a snapshot,
    /// the fresh tail and CaughtUp.
    Reset,
    /// The client asks for the page below its oldest held order.
    AskPage,
    /// The page asked for arrives.
    Page,
    /// The person sends the prompt.
    Send,
    /// The prompt reaches the daemon, which writes the item reflecting it.
    Arrive,
    /// The send's reply: accepted (only once it arrived) or lost.
    InputVerdict(bool),
    /// The person discards the unconfirmed prompt.
    Withdraw,
}

/// What the daemon holds for the agent.
#[derive(Clone, Debug, PartialEq)]
struct Daemon {
    items: Vec<Item>,
    revision: u64,
    phase: Phase,
    generation: u64,
    /// The revision of the newest snapshot written, which is what an
    /// opening carries: later writes revise items, not the snapshot.
    newest_snapshot: u64,
}

impl Daemon {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            revision: self.newest_snapshot,
            kind: KIND as i32,
            phase: self.phase as i32,
            ..Snapshot::default()
        }
    }

    /// What a subscription opens with: the Opening naming the generation,
    /// the snapshot, the newest `tail` items by order, then CaughtUp.
    fn opening(&self, tail: usize) -> Vec<Msg> {
        let mut msgs = vec![
            event(session_event::Of::Opening(wire::Opening {
                generation: self.generation,
            })),
            event(session_event::Of::Snapshot(self.snapshot())),
        ];
        let from = self.items.len().saturating_sub(tail);
        msgs.extend(
            self.items[from..]
                .iter()
                .map(|item| event(session_event::Of::Item(item.clone()))),
        );
        msgs.push(caught_up(self.revision));
        msgs
    }

    /// What a replica publishes to its subscribers when it takes the block
    /// of a new generation: Reset naming the generation, the snapshot, the
    /// newest `tail` items by order, then CaughtUp, with no Opening.
    fn reset(&self, tail: usize) -> Vec<Msg> {
        let mut msgs = self.opening(tail);
        msgs[0] = event(session_event::Of::Reset(wire::Reset {
            generation: self.generation,
        }));
        msgs
    }

    /// The oldest order an opening with this tail carries, or the next
    /// order when it carries none.
    fn opened_from(&self, tail: usize) -> u64 {
        let from = self.items.len().saturating_sub(tail);
        match self.items.get(from) {
            Some(item) => item.order,
            None => self.items.last().map_or(1, |item| item.order + 1),
        }
    }

    fn get(&self, key: &str) -> Option<&Item> {
        self.items.iter().find(|item| item.key == key)
    }
}

/// What has happened along a path that the properties judge.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Seen {
    /// An uncertain input that left uncertain with nothing to show for it.
    resolved_without_evidence: Option<String>,
    /// An uncertain input stayed so past a CaughtUp.
    stayed_uncertain: bool,
    /// An append met a revision the client did not hold and was answered
    /// with Get.
    got: bool,
    /// A Reset's transcript was swapped in.
    swapped: bool,
    /// A page landed in the window.
    paged: bool,
    /// An item reflecting the input, or a queue listing it, reached the
    /// client.
    reflected: bool,
}

/// How much of the bounds a path has used.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Used {
    writes: u8,
    lags: u8,
    resets: u8,
    pages: u8,
}

/// The input's part in the world.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Sent {
    sent: bool,
    arrived: bool,
    answered: bool,
}

#[derive(Clone)]
pub struct State {
    daemon: Daemon,
    /// What the stream carries to the client, oldest first.
    stream: VecDeque<Msg>,
    /// The stream is open: a write the daemon makes reaches it.
    open: bool,
    /// A page asked for and not yet arrived, as the daemon read it then.
    page: Option<Msg>,
    /// The oldest order the stream's latest opening carried, or the next
    /// order when it carried none. Rows below it reached the client before
    /// a gap in the stream or on a page, and a revision the daemon made of
    /// them meanwhile may not have.
    opened_from: u64,
    client: SessionState,
    whole: SessionState,
    input: Sent,
    used: Used,
    seen: Seen,
    /// The client's session as its stream last opened, which is where a
    /// client's dump starts, and every message it was fed since. Outside
    /// identity: a session is a function of what it was fed, which is what
    /// the replay property checks.
    replay: Replay,
    /// The messages the client was fed by the step that made this state,
    /// in order. Outside identity: what a trace of a path records.
    fed: Vec<Msg>,
}

#[derive(Clone)]
struct Replay {
    from: SessionState,
    since: Vec<Msg>,
}

impl State {
    /// The client's session.
    pub fn client(&self) -> &SessionState {
        &self.client
    }

    /// The messages the client was fed by the step that made this state,
    /// in order, the driver's Get answers among them.
    pub fn fed(&self) -> &[Msg] {
        &self.fed
    }

    /// The client has caught up and nothing is on its way to it.
    fn at_rest(&self) -> bool {
        self.client.caught_up()
            && self.stream.is_empty()
            && self.page.is_none()
            && !self.client.reset_pending()
    }

    /// Two states are one when they print alike. A `SessionState` keeps
    /// its indexes in ordered maps, so equal sessions print alike whatever
    /// path built them.
    fn identity(&self) -> String {
        format!(
            "{:?}|{:?}|{}|{:?}|{}|{:?}|{:?}|{:?}|{:?}|{:?}",
            self.daemon,
            self.stream,
            self.open,
            self.page,
            self.opened_from,
            self.client,
            self.whole,
            self.input,
            self.used,
            self.seen
        )
    }

    /// Feeds the client one message, as its driver does: recorded for the
    /// replay, and an append whose base it does not hold answered with Get
    /// before anything else.
    fn feed(&mut self, msg: Msg) -> ui_state::Outcome {
        if reflects(&msg) {
            self.seen.reflected = true;
        }
        self.replay.since.push(msg.clone());
        self.fed.push(msg.clone());
        let outcome = self.client.update(msg);
        if let Some(key) = &outcome.need_get {
            self.seen.got = true;
            if let Some(item) = self.daemon.get(key).cloned() {
                let got = event(session_event::Of::Item(item));
                self.replay.since.push(got.clone());
                self.fed.push(got.clone());
                self.client.update(got);
            }
        }
        if outcome.reloaded {
            self.seen.swapped = true;
        }
        outcome
    }

    /// The stream opens again with a fresh tail behind whatever it holds.
    fn reopen(&mut self, tail: usize) {
        self.opened_from = self.daemon.opened_from(tail);
        self.stream.extend(self.daemon.opening(tail));
    }

    /// The daemon wrote `msg`: the uninterrupted client reads it now, and
    /// the stream carries it while it is open.
    fn written(&mut self, msg: Msg) {
        self.whole.update(msg.clone());
        if self.open {
            self.stream.push_back(msg);
        }
    }

    fn revise(&mut self) -> u64 {
        self.daemon.revision += 1;
        self.used.writes += 1;
        self.daemon.revision
    }
}

impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
}

impl Eq for State {}

impl Hash for State {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.identity().hash(state);
    }
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rows: Vec<String> = self
            .client
            .transcript()
            .iter()
            .map(|held| {
                format!(
                    "{}@r{} {:?}",
                    held.item.key, held.item.revision, held.item.text
                )
            })
            .collect();
        let daemon: Vec<String> = self
            .daemon
            .items
            .iter()
            .map(|item| format!("{}@r{} {:?}", item.key, item.revision, item.text))
            .collect();
        let whole: Vec<String> = self
            .whole
            .transcript()
            .iter()
            .map(|held| format!("{}@r{}", held.item.key, held.item.revision))
            .collect();
        f.debug_struct("State")
            .field("daemon", &daemon)
            .field("client", &rows)
            .field("caught_up", &self.client.caught_up())
            .field("input", &self.client.input_state(INPUT))
            .field("snapshot", &self.client.agent_state().revision)
            .field("whole", &whole)
            .field("whole_snapshot", &self.whole.agent_state().revision)
            .field("stream", &self.stream.len())
            .field("open", &self.open)
            .field("page", &self.page.is_some())
            .finish()
    }
}

fn event(of: session_event::Of) -> Msg {
    Msg::Event(SessionEvent { of: Some(of) })
}

fn caught_up(revision: u64) -> Msg {
    event(session_event::Of::CaughtUp(wire::CaughtUp { revision }))
}

fn agent() -> Agent {
    Agent {
        agent_id: AGENT_ID.to_vec(),
        host_id: b"host-0000000000a".to_vec(),
        kind: KIND as i32,
        name: "worker".into(),
        lifecycle: Lifecycle::Live as i32,
        phase: Phase::Working as i32,
        incarnation: 1,
        ..Agent::default()
    }
}

/// An item of the agent's prose, which a client draws as its text.
fn text_item(order: u64, revision: u64, text: &str) -> Item {
    item(
        order,
        revision,
        text,
        claude_item::Kind::Message(wire::Text { complete: true }),
    )
}

fn item(order: u64, revision: u64, text: &str, kind: claude_item::Kind) -> Item {
    Item {
        key: format!("k{order}"),
        order,
        revision,
        text: text.into(),
        kind: KIND as i32,
        body: ClaudeItem { kind: Some(kind) }.encode_to_vec(),
        ..Item::default()
    }
}

fn prompt() -> wire::Input {
    wire::Input::new(
        INPUT.to_vec(),
        wire::AgentInput {
            of: Some(wire::agent_input::Of::Prompt(wire::PromptInput {
                text: "and then this".into(),
                attachments: Vec::new(),
            })),
        },
    )
}

/// The rows a client holds from `from` up, as items.
fn rows_from(session: &SessionState, from: u64) -> Vec<Item> {
    session
        .transcript()
        .iter()
        .filter(|held| held.item.order >= from)
        .map(|held| held.item.clone())
        .collect()
}

/// Whether a message says where the input went: an item reflecting it, or
/// a snapshot whose queue lists it.
fn reflects(msg: &Msg) -> bool {
    match msg {
        Msg::Event(SessionEvent {
            of: Some(session_event::Of::Item(item)),
        }) => item.input_id == INPUT,
        Msg::Event(SessionEvent {
            of: Some(session_event::Of::Snapshot(snapshot)),
        }) => snapshot.queue.iter().any(|queued| queued.input_id == INPUT),
        Msg::Page { items, .. } => items.iter().any(|item| item.input_id == INPUT),
        _ => false,
    }
}

/// Whether the client was shown anything that says where its input went:
/// an item reflecting it, or a queue listing it.
fn evidence(session: &SessionState) -> bool {
    session.transcript().key_for_input(INPUT).is_some()
        || session
            .agent_state()
            .queue
            .iter()
            .any(|queued| queued.input_id == INPUT)
}

pub(crate) const ARRIVAL_ORDER: &str = "same state under any arrival order: once the client is caught \
     up with nothing on its way to it, it holds every item the daemon holds from the oldest order its \
     stream opened with up, each at the daemon's newest revision, and nothing else; a row below that is \
     the daemon's row at its order, at the newest revision or an older one";
pub(crate) const CATCH_UP_PLUS_LIVE: &str = "catch-up plus live equals uninterrupted delivery at every \
     cut: once caught up with nothing on its way, the client's rows from the oldest order its stream \
     opened with up and its agent state read as those of a client that received every event \
     uninterrupted";
pub(crate) const GENERATION_KNOWN: &str = "a caught-up client knows the generation it holds: once \
     caught up with nothing on its way, the client holds the generation of the daemon's rows, so a \
     stream it reopens under that generation continues its window rather than rebuilding it";
pub(crate) const REPLAY: &str = "replay from any checkpoint equals the whole: the session at a \
     checkpoint, fed every message since, is the live session";
pub(crate) const UNCERTAIN_LEFT: &str = "an uncertain input absent at CaughtUp is left with resend and \
     discard: an input whose send was lost leaves uncertain only once an item reflecting it or a queue \
     listing it reached the client, and settles only on an item reflecting it";
/// The client's promise a replay of a dump judges too, after every event
/// its session takes.
pub const ONE_BLOCK: &str = "the window is one block: the rows a client holds have contiguous orders \
     and every index names a held row";

/// The client's promises a run of one session can be judged by alone, as
/// this model and a dump's replay both judge them after every message: the
/// first broken, with what broke it.
pub fn judge_session(session: &SessionState) -> Option<(&'static str, String)> {
    session
        .transcript()
        .check()
        .err()
        .map(|detail| (ONE_BLOCK, detail))
}

impl Machine for ClientModel {
    type State = State;
    type Action = Action;

    fn init(&self) -> State {
        let items: Vec<Item> = (1..=OPENING_ITEMS)
            .map(|order| text_item(order, order, "t"))
            .collect();
        let daemon = Daemon {
            revision: OPENING_ITEMS,
            items,
            phase: Phase::Working,
            generation: 1,
            newest_snapshot: OPENING_ITEMS,
        };
        let client = SessionState::new(agent(), 200);
        let mut whole = client.clone();
        for msg in daemon.opening(usize::MAX) {
            whole.update(msg);
        }
        State {
            stream: daemon.opening(self.tail).into(),
            open: true,
            page: None,
            opened_from: daemon.opened_from(self.tail),
            replay: Replay {
                from: client.clone(),
                since: Vec::new(),
            },
            client,
            whole,
            daemon,
            input: Sent::default(),
            used: Used::default(),
            seen: Seen::default(),
            fed: Vec::new(),
        }
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        if s.used.writes < self.writes {
            out.push(Action::NewItem);
            for item in &s.daemon.items {
                if item.input_id.is_empty() {
                    out.push(Action::Append(item.order));
                }
                out.push(Action::Revise(item.order));
            }
            out.push(Action::Snapshot);
            if s.input.sent && !s.input.arrived {
                out.push(Action::Arrive);
            }
        }
        match s.stream.front() {
            Some(Msg::Event(SessionEvent {
                of: Some(session_event::Of::CaughtUp(_)),
            })) => out.push(Action::CaughtUp),
            Some(_) => out.push(Action::Event),
            None => {}
        }
        if s.open && s.used.lags < self.lags {
            out.push(Action::Lagged);
        }
        if s.used.resets < self.resets && s.daemon.items.len() > 1 {
            out.push(Action::Reset);
        }
        if s.page.is_none()
            && s.used.pages < self.pages
            && s.client
                .transcript()
                .oldest_held()
                .is_some_and(|oldest| oldest > 1)
        {
            out.push(Action::AskPage);
        }
        if s.page.is_some() {
            out.push(Action::Page);
        }
        if !s.input.sent {
            out.push(Action::Send);
        }
        if s.input.sent && !s.input.answered {
            if s.input.arrived {
                out.push(Action::InputVerdict(true));
            }
            out.push(Action::InputVerdict(false));
        }
        if s.input.answered
            && matches!(
                s.client.input_state(INPUT),
                Some(InputState::Uncertain | InputState::Sent)
            )
        {
            out.push(Action::Withdraw);
        }
    }

    fn next(&self, s: &State, action: &Action) -> Option<State> {
        let mut next = s.clone();
        next.fed.clear();
        match action {
            Action::NewItem => {
                let revision = next.revise();
                let order = next.daemon.items.last().map_or(1, |item| item.order + 1);
                let item = text_item(order, revision, "t");
                next.daemon.items.push(item.clone());
                next.written(event(session_event::Of::Item(item)));
            }
            Action::Arrive => {
                let revision = next.revise();
                let order = next.daemon.items.last().map_or(1, |item| item.order + 1);
                let mut item = item(
                    order,
                    revision,
                    "and then this",
                    claude_item::Kind::Prompt(wire::Prompt {}),
                );
                item.input_id = INPUT.to_vec();
                next.daemon.items.push(item.clone());
                next.input.arrived = true;
                next.written(event(session_event::Of::Item(item)));
            }
            Action::Append(order) => {
                let revision = next.revise();
                let item = next
                    .daemon
                    .items
                    .iter_mut()
                    .find(|item| item.order == *order)?;
                let append = Append {
                    key: item.key.clone(),
                    base_revision: item.revision,
                    revision,
                    text: "+".into(),
                };
                item.text.push('+');
                item.revision = revision;
                next.written(event(session_event::Of::Append(append)));
            }
            Action::Revise(order) => {
                let revision = next.revise();
                let item = next
                    .daemon
                    .items
                    .iter_mut()
                    .find(|item| item.order == *order)?;
                item.revision = revision;
                item.text = format!("r{revision}");
                let item = item.clone();
                next.written(event(session_event::Of::Item(item)));
            }
            Action::Snapshot => {
                next.daemon.newest_snapshot = next.revise();
                next.daemon.phase = match next.daemon.phase {
                    Phase::Working => Phase::Idle,
                    _ => Phase::Working,
                };
                let snapshot = next.daemon.snapshot();
                next.written(event(session_event::Of::Snapshot(snapshot)));
            }
            Action::Event | Action::CaughtUp => {
                let msg = next.stream.pop_front()?;
                let lagged = matches!(
                    &msg,
                    Msg::Event(SessionEvent {
                        of: Some(session_event::Of::Lagged(_))
                    })
                );
                let uncertain = next.client.input_state(INPUT) == Some(InputState::Uncertain);
                next.feed(msg);
                if lagged {
                    // The driver reopens at once with a fresh tail.
                    next.feed(Msg::Connection(ui_state::Connection::Live));
                    next.replay = Replay {
                        from: next.client.clone(),
                        since: Vec::new(),
                    };
                    next.stream.clear();
                    next.reopen(self.tail);
                    next.open = true;
                }
                if *action == Action::CaughtUp && uncertain {
                    match next.client.input_state(INPUT) {
                        Some(InputState::Uncertain) => next.seen.stayed_uncertain = true,
                        state if !evidence(&next.client) => {
                            next.seen
                                .resolved_without_evidence
                                .get_or_insert(format!("uncertain became {state:?} at CaughtUp"));
                        }
                        _ => {}
                    }
                }
            }
            Action::Lagged => {
                next.used.lags += 1;
                next.stream.clear();
                next.stream
                    .push_back(event(session_event::Of::Lagged(wire::Lagged {})));
                next.open = false;
            }
            Action::Reset => {
                next.used.resets += 1;
                next.daemon.generation += 1;
                next.daemon.items.pop();
                for index in 0..next.daemon.items.len() {
                    next.daemon.revision += 1;
                    let revision = next.daemon.revision;
                    let item = &mut next.daemon.items[index];
                    item.revision = revision;
                    item.text = format!("g{revision}");
                }
                next.daemon.newest_snapshot = next.daemon.revision;
                for msg in next.daemon.reset(usize::MAX) {
                    next.whole.update(msg);
                }
                if next.open {
                    next.opened_from = next.daemon.opened_from(self.tail);
                    next.stream.extend(next.daemon.reset(self.tail));
                }
            }
            Action::AskPage => {
                next.used.pages += 1;
                let oldest = next.client.transcript().oldest_held()?;
                let below: Vec<Item> = next
                    .daemon
                    .items
                    .iter()
                    .filter(|item| item.order < oldest)
                    .cloned()
                    .collect();
                let from = below.len().saturating_sub(self.page);
                next.page = Some(Msg::Page {
                    items: below[from..].iter().rev().cloned().collect(),
                    exhausted: from == 0,
                    epoch: next.client.epoch(),
                });
            }
            Action::Page => {
                let page = next.page.take()?;
                let before = next.client.transcript().oldest_held();
                next.feed(page);
                if next.client.transcript().oldest_held() < before {
                    next.seen.paged = true;
                }
            }
            Action::Send => {
                next.input.sent = true;
                next.feed(Msg::Send(prompt()));
                next.whole.update(Msg::Send(prompt()));
            }
            Action::InputVerdict(accepted) => {
                next.input.answered = true;
                let outcome = if *accepted {
                    InputOutcome::Reply(SendInputResponse {
                        of: Some(wire::send_input_response::Of::Accepted(wire::Accepted {
                            queued: false,
                        })),
                    })
                } else {
                    InputOutcome::Lost
                };
                next.feed(Msg::Sent(INPUT.to_vec(), outcome.clone()));
                next.whole.update(Msg::Sent(INPUT.to_vec(), outcome));
            }
            Action::Withdraw => {
                next.feed(Msg::Discard(INPUT.to_vec()));
                next.whole.update(Msg::Discard(INPUT.to_vec()));
            }
        }
        Some(next)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(ARRIVAL_ORDER, |_, s: &State| {
                if !s.at_rest() {
                    return true;
                }
                let held = rows_from(&s.client, 0);
                let below = held
                    .iter()
                    .filter(|row| row.order < s.opened_from)
                    .all(|row| {
                        s.daemon.items.iter().any(|item| {
                            item.order == row.order
                                && item.key == row.key
                                && (item.revision > row.revision || item == row)
                        })
                    });
                let daemon: Vec<Item> = s
                    .daemon
                    .items
                    .iter()
                    .filter(|item| item.order >= s.opened_from)
                    .cloned()
                    .collect();
                below && rows_from(&s.client, s.opened_from) == daemon
            }),
            Property::always(CATCH_UP_PLUS_LIVE, |_, s: &State| {
                if !s.at_rest() {
                    return true;
                }
                rows_from(&s.client, s.opened_from) == rows_from(&s.whole, s.opened_from)
                    && s.client.agent_state() == s.whole.agent_state()
            }),
            // Read back from the form a dump carries them in, as a dump's
            // replay reads them.
            Property::always(REPLAY, |_, s: &State| {
                let Ok(mut replayed) = SessionState::from_wire(s.replay.from.to_wire()) else {
                    return false;
                };
                for msg in &s.replay.since {
                    match Msg::from_wire(msg.to_wire()) {
                        Ok(msg) => drop(replayed.update(msg)),
                        Err(_) => return false,
                    }
                }
                replayed == s.client
            }),
            Property::always(UNCERTAIN_LEFT, |_, s: &State| {
                s.seen.resolved_without_evidence.is_none()
                    && (s.client.input_state(INPUT) != Some(InputState::Settled)
                        || s.seen.reflected)
            }),
            Property::always(GENERATION_KNOWN, |_, s: &State| {
                !s.at_rest() || s.client.generation() == Some(s.daemon.generation)
            }),
            Property::always(ONE_BLOCK, |_, s: &State| {
                judge_session(&s.client).is_none_or(|(promise, _)| promise != ONE_BLOCK)
            }),
            Property::sometimes(
                "an append to a revision the client does not hold is answered with Get",
                |_, s: &State| s.seen.got,
            ),
            Property::sometimes(
                "an uncertain input sometimes stays uncertain past a CaughtUp",
                |_, s: &State| s.seen.stayed_uncertain,
            ),
            Property::sometimes(
                "a Reset sometimes swaps a fresh transcript in",
                |_, s: &State| s.seen.swapped,
            ),
            Property::sometimes(
                "an older page sometimes extends the window",
                |_, s: &State| s.seen.paged,
            ),
        ]
    }
}
