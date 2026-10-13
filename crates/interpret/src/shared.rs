//! The kind-neutral state every interpreter composes: the person's queue, the
//! ask list, the set of accepted but unconsumed agent messages, working_on,
//! the turn counter, and the items still open for streaming. It also owns
//! step assembly, so phase derivation and "a change forces a snapshot" are
//! written once.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use prost::Message as _;
use serde::{Deserialize, Serialize};
use wire::{
    AgentMessage, AgentSpec, Append, Attachment, Bootstrap, Envelope, Item, Phase, PromptInput,
    QueuedInput, Refusal, Rejected, Sender, Snapshot, Step, TurnEnd, bootstrap, sender,
};

use crate::{Effect, Stepped, reply, serde_pb};

/// The tool server name amux's own tools are registered under, and the tool
/// whose call sets working_on.
pub const AMUX_TOOL_SERVER: &str = "amux";
pub const STATUS_TOOL: &str = "status";
/// amux's tool that messages another agent; its call is drawn as the
/// message it sent, never as a tool call.
pub const SEND_TOOL: &str = "send";
/// amux's tool that attaches a file: it answers with the attachment's
/// element, which its step carries as the attachment, never as text.
pub const ATTACH_TOOL: &str = "attach";

/// An open ask as the shared list needs to see it. Implemented by the
/// per-kind ask messages.
pub trait OpenAsk: prost::Message + prost::Name + Default + Clone + PartialEq {
    fn key(&self) -> &str;
    fn item_key(&self) -> &str;
}

impl OpenAsk for wire::Ask {
    fn key(&self) -> &str {
        &self.key
    }
    fn item_key(&self) -> &str {
        &self.item_key
    }
}

impl OpenAsk for wire::CodexAsk {
    fn key(&self) -> &str {
        &self.key
    }
    fn item_key(&self) -> &str {
        &self.item_key
    }
}

/// The person's queue: prompts waiting for the provider to go idle, in
/// arrival order, whichever client sent them. Nothing is edited in place;
/// an edit is a withdraw and a new prompt.
///
/// An entry sent into the running turn stays in place marked `steer` until
/// its reflection lands. It is in the provider's hands by then: it cannot be
/// withdrawn or sent again, and it is never submitted from here.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Queue {
    #[serde(with = "serde_pb::msgs")]
    entries: Vec<QueuedInput>,
}

impl Queue {
    pub fn push(&mut self, entry: QueuedInput) {
        self.entries.push(entry);
    }

    /// Removes the waiting entry with this input id; false when it is not
    /// waiting.
    pub fn withdraw(&mut self, input_id: &[u8]) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|entry| entry.steer || entry.input_id != input_id);
        self.entries.len() != before
    }

    /// The oldest waiting entry, removed.
    pub fn pop(&mut self) -> Option<QueuedInput> {
        let at = self.entries.iter().position(|entry| !entry.steer)?;
        Some(self.entries.remove(at))
    }

    /// Marks the waiting entry with this input id as sent into the running
    /// turn and returns it; None when it is not waiting.
    pub fn steer(&mut self, input_id: &[u8]) -> Option<QueuedInput> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| !entry.steer && entry.input_id == input_id)?;
        entry.steer = true;
        Some(entry.clone())
    }

    /// Returns a steered entry to waiting: the provider refused it.
    pub fn unsteer(&mut self, input_id: &[u8]) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.steer && entry.input_id == input_id)
        {
            entry.steer = false;
        }
    }

    /// Removes and returns the oldest steered entry that `matches`: its
    /// reflection landed.
    pub fn take_steered(&mut self, matches: impl Fn(&QueuedInput) -> bool) -> Option<QueuedInput> {
        let at = self
            .entries
            .iter()
            .position(|entry| entry.steer && matches(entry))?;
        Some(self.entries.remove(at))
    }

    /// Forgets every steered entry: the provider will not reflect them.
    pub fn drop_steered(&mut self) {
        self.entries.retain(|entry| !entry.steer);
    }

    pub fn has_steered(&self) -> bool {
        self.entries.iter().any(|entry| entry.steer)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, input_id: &[u8]) -> bool {
        self.entries.iter().any(|entry| entry.input_id == input_id)
    }

    pub fn entries(&self) -> &[QueuedInput] {
        &self.entries
    }
}

/// The open asks, in the order they opened. Opened and closed only by
/// provider facts, or by an answer this interpreter sent: never by a timer,
/// a quiet period, or anything downstream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "", deserialize = ""))]
pub struct Asks<A: OpenAsk> {
    #[serde(with = "serde_pb::msgs")]
    open: Vec<A>,
}

impl<A: OpenAsk> Default for Asks<A> {
    fn default() -> Self {
        Self { open: Vec::new() }
    }
}

impl<A: OpenAsk> Asks<A> {
    /// Opens an ask, or replaces the open one with the same key.
    pub fn open(&mut self, ask: A) {
        match self.open.iter_mut().find(|open| open.key() == ask.key()) {
            Some(open) => *open = ask,
            None => self.open.push(ask),
        }
    }

    pub fn close(&mut self, key: &str) -> Option<A> {
        let at = self.open.iter().position(|open| open.key() == key)?;
        Some(self.open.remove(at))
    }

    /// Closes every ask whose item is `item_key`: a tool result or a later
    /// message closes the asks that pointed at the call.
    pub fn close_for_item(&mut self, item_key: &str) -> Vec<A> {
        let (closed, open) = std::mem::take(&mut self.open)
            .into_iter()
            .partition(|ask| !item_key.is_empty() && ask.item_key() == item_key);
        self.open = open;
        closed
    }

    pub fn close_all(&mut self) -> Vec<A> {
        std::mem::take(&mut self.open)
    }

    pub fn get(&self, key: &str) -> Option<&A> {
        self.open.iter().find(|open| open.key() == key)
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    pub fn open_asks(&self) -> &[A] {
        &self.open
    }
}

/// The per-agent turn counter. A turn begins when a prompt is submitted or
/// the provider reports one starting, and ends on the provider's turn-end
/// fact; an ask is mid-turn.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnCounter {
    last_id: u64,
    current: Option<OpenTurn>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenTurn {
    pub id: u64,
    pub started_at_ms: i64,
    /// The turn's newest complete assistant text item.
    pub last_message_key: String,
}

impl TurnCounter {
    /// The running turn's id, starting one if none is running.
    pub fn begin(&mut self, now_ms: i64) -> u64 {
        if let Some(turn) = &self.current {
            return turn.id;
        }
        self.last_id += 1;
        self.current = Some(OpenTurn {
            id: self.last_id,
            started_at_ms: now_ms,
            last_message_key: String::new(),
        });
        self.last_id
    }

    pub fn current(&self) -> Option<&OpenTurn> {
        self.current.as_ref()
    }

    pub fn note_message(&mut self, key: &str) {
        if let Some(turn) = &mut self.current {
            turn.last_message_key = key.to_owned();
        }
    }

    pub fn end(&mut self) -> Option<OpenTurn> {
        self.current.take()
    }
}

/// An item as an interpreter builds it; the envelope's agent, kind and
/// producer version are filled in by [`Shared::item`].
#[derive(Clone, Debug, Default)]
pub struct ItemDraft {
    pub key: String,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub input_id: Vec<u8>,
    /// The per-kind item message, encoded.
    pub body: Vec<u8>,
    /// The provider's own timestamp. When absent: the time an open item
    /// was first emitted, so a stream keeps the moment it started, else the
    /// interpreter's clock. A kind re-emitting a completed item (a tool
    /// call's result) passes the time it started.
    pub at_ms: Option<i64>,
    /// No append will follow. Anything not complete stays open, and a
    /// resume re-emits it in full.
    pub complete: bool,
}

/// A finished reply's text and attachments. The attach tool answers a model
/// with an attachment element to put in its reply; each element that
/// parses becomes the placeholder and an attachment, so clients draw the
/// file where the model put it and fetch its bytes by hash. An element that
/// does not parse stays text as written, and a reply with none is returned
/// untouched.
pub fn parse_reply(text: String) -> (String, Vec<Attachment>) {
    let parsed = attachments::parse(&text);
    if parsed.positioned.attachments.is_empty() {
        return (text, Vec::new());
    }
    (parsed.positioned.text, parsed.positioned.attachments)
}

/// One step's output as it is assembled.
#[derive(Debug, Default)]
pub struct Emit {
    step: Step,
    effects: Vec<Effect>,
}

impl Emit {
    pub fn effect(&mut self, effect: Effect) {
        self.effects.push(effect);
    }

    pub fn effects(&self) -> &[Effect] {
        &self.effects
    }

    pub fn items(&self) -> &[Item] {
        &self.step.items
    }

    /// Makes this step the incarnation's last, saying why it ended: the
    /// daemon reads the cause from the journal and puts it on the row.
    pub fn end_incarnation(&mut self, end: wire::IncarnationEnd) {
        self.step.incarnation_end = Some(end);
    }
}

/// How much of a running command's output an item keeps: the newest
/// output up to twice this, then the last this much again.
pub const OUTPUT_CAP: usize = 64 * 1024;

/// What extending a command's output did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// Sent as an append.
    Appended,
    /// The kept text was cut, dropping this many bytes from its front; the
    /// caller sends the item whole.
    Cut { dropped: u64 },
    /// The item is not open; the caller sends it whole.
    NotOpen,
}

/// Where to cut output longer than twice [`OUTPUT_CAP`] so the last cap's
/// worth stays: just after the first line break in it, else at the first
/// character boundary. Zero when the text is short enough to keep whole.
pub fn output_cut(text: &str) -> usize {
    if text.len() <= 2 * OUTPUT_CAP {
        return 0;
    }
    let from = text.len() - OUTPUT_CAP;
    match text.as_bytes()[from..]
        .iter()
        .position(|byte| *byte == b'\n')
    {
        Some(at) if from + at + 1 < text.len() => from + at + 1,
        _ => (from..=text.len())
            .find(|at| text.is_char_boundary(*at))
            .unwrap_or(text.len()),
    }
}

/// Everything kind-neutral an interpreter holds. Every field is checkpointed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "", deserialize = ""))]
pub struct Shared<A: OpenAsk> {
    #[serde(with = "serde_pb::bytes")]
    agent: Vec<u8>,
    /// The wire Kind's integer.
    kind: i32,
    producer_version: String,
    now_ms: i64,
    /// The provider accepts input.
    started: bool,
    /// A turn is running: submitted, or reported by the provider.
    busy: bool,
    queue: Queue,
    asks: Asks<A>,
    /// Agent messages the provider accepted and has not yet consumed, by
    /// envelope id. A one-shot agent does not exit while this is non-empty.
    #[serde(with = "serde_pb::bytes_set")]
    pending_messages: BTreeSet<Vec<u8>>,
    working_on: Option<String>,
    turn: TurnCounter,
    /// Prompts handed to the provider and not yet reflected, oldest first:
    /// the reflection FIFO.
    #[serde(with = "serde_pb::bytes_vec")]
    submitted: Vec<Vec<u8>>,
    /// Items emitted and not complete, at their full current state.
    #[serde(with = "serde_pb::msg_map")]
    open_items: BTreeMap<String, Item>,
    /// The last snapshot emitted, so a step emits one only when it changed.
    #[serde(with = "serde_pb::opt_msg")]
    last_snapshot: Option<Snapshot>,
    /// The repository facts the agent process last read.
    #[serde(with = "serde_pb::opt_msg")]
    git: Option<wire::Git>,
    /// What the provider runs in the background, as its interpreter last
    /// tracked it; None until the provider says.
    #[serde(with = "serde_pb::opt_msg")]
    jobs: Option<wire::BackgroundJobs>,
    /// The hash of the catalogue last written; None until the provider
    /// says what it offers.
    catalogue: Option<Vec<u8>>,
    /// A submitted prompt starts no turn: the provider's own record of the
    /// prompt does (terminal Claude, where keys typed may never arrive).
    #[serde(default)]
    turn_on_reflection: bool,
    /// The agent is being stopped: the turn the stop cuts short ends
    /// stopped, however the provider words its end.
    #[serde(default)]
    stopping: bool,
}

impl<A: OpenAsk> Shared<A> {
    /// The state an agent starts from: its queue seeded from the spawn's
    /// prompt, working_on from that prompt's first line, the clock at the
    /// spec's creation time.
    pub fn new(spec: &AgentSpec, kind: wire::Kind, producer_version: &str) -> Self {
        let mut shared = Self {
            agent: spec.agent_id.clone(),
            kind: kind as i32,
            producer_version: producer_version.to_owned(),
            now_ms: spec.created_at_ms,
            started: false,
            busy: false,
            queue: Queue::default(),
            asks: Asks::default(),
            pending_messages: BTreeSet::new(),
            working_on: None,
            turn: TurnCounter::default(),
            submitted: Vec::new(),
            open_items: BTreeMap::new(),
            last_snapshot: None,
            git: None,
            jobs: None,
            catalogue: None,
            turn_on_reflection: false,
            stopping: false,
        };
        if let Some(entry) = spec.bootstrap.as_ref().and_then(queued_from_bootstrap) {
            shared.working_on = first_line(&entry.text);
            shared.queue.push(entry);
        }
        shared
    }

    /// Carries this state into the agent's next incarnation under `spec`:
    /// the producer and clock move on, and a prompt the spec brings (a
    /// resume with a prompt, or a parent's message) joins the queue.
    pub fn reincarnate(&mut self, spec: &AgentSpec, producer_version: &str) {
        self.producer_version = producer_version.to_owned();
        self.stopping = false;
        // A new provider process runs nothing in the background yet.
        self.set_jobs(Vec::new());
        self.now_ms = self.now_ms.max(spec.created_at_ms);
        if let Some(entry) = spec.bootstrap.as_ref().and_then(queued_from_bootstrap)
            && !self.queue.contains(&entry.input_id)
        {
            self.working_on = first_line(&entry.text);
            self.queue.push(entry);
        }
    }

    /// The first journal frame: a Snapshot, phase starting, the body at the
    /// kind's explicit unknowns.
    pub fn initial_step(&mut self, unknown_body: Vec<u8>) -> Step {
        let mut emit = Emit::default();
        self.snapshot_into(&mut emit, unknown_body, true);
        emit.step
    }

    /// Finishes a step: appends a Snapshot if anything a client draws from
    /// it changed.
    pub fn finish(&mut self, mut emit: Emit, body: Vec<u8>) -> Stepped {
        self.snapshot_into(&mut emit, body, false);
        Stepped {
            step: emit.step,
            effects: emit.effects,
        }
    }

    /// The step a resume starts with: every open item in full on its known
    /// key, and the snapshot. At-least-once; downstream dedupes by key.
    pub fn resume_step(&mut self, body: Vec<u8>) -> Step {
        let mut emit = Emit::default();
        emit.step.items = self.open_items.values().cloned().collect();
        self.snapshot_into(&mut emit, body, true);
        emit.step
    }

    fn snapshot_into(&mut self, emit: &mut Emit, body: Vec<u8>, force: bool) {
        let phase = self.phase() as i32;
        // Since when is stamped only when the phase changes; streamed output
        // and every other change within a phase keep the earlier stamp.
        let phase_since_ms = match &self.last_snapshot {
            Some(last) if last.phase == phase => last.phase_since_ms,
            _ => self.now_ms,
        };
        let snapshot = Snapshot {
            agent_id: self.agent.clone(),
            revision: 0,
            queue: self.queue.entries.clone(),
            kind: self.kind,
            body,
            phase,
            working_on: self.working_on.clone(),
            at_ms: self.now_ms,
            phase_since_ms,
            git: self.git.clone(),
            catalogue: self.catalogue.clone(),
            background_jobs: self.jobs.as_ref().map_or(0, |jobs| jobs.jobs.len() as u32),
        };
        let changed = self.last_snapshot.as_ref().is_none_or(|last| {
            Snapshot {
                at_ms: snapshot.at_ms,
                ..last.clone()
            } != snapshot
        });
        if force || changed {
            self.last_snapshot = Some(snapshot.clone());
            emit.step.snapshot = Some(snapshot);
        }
    }

    /// Needs-you whenever an ask is open; starting until the provider
    /// accepts input; working while a turn runs; idle otherwise.
    pub fn phase(&self) -> Phase {
        if !self.asks.is_empty() {
            Phase::NeedsYou
        } else if !self.started {
            Phase::Starting
        } else if self.busy {
            Phase::Working
        } else {
            Phase::Idle
        }
    }

    // --- repository ------------------------------------------------------

    /// Publishes the branch and change totals on the next snapshot; a step
    /// emits one only when they changed.
    pub fn set_git(&mut self, git: Option<wire::Git>) {
        self.git = git;
    }

    // --- catalogue -------------------------------------------------------

    /// Hashes the encoded catalogue. A changed hash asks the agent process
    /// to write the bytes as a blob before the step is journaled and
    /// publishes the new hash on the snapshot; an unchanged one does
    /// nothing.
    pub fn set_catalogue(&mut self, emit: &mut Emit, catalogue: wire::Catalogue) {
        use sha2::Digest as _;
        let bytes = catalogue.encode_to_vec();
        let hash = sha2::Sha256::digest(&bytes).to_vec();
        if self.catalogue.as_ref() == Some(&hash) {
            return;
        }
        self.catalogue = Some(hash.clone());
        emit.effect(Effect::WriteBlob { hash, bytes });
    }

    // --- background jobs -------------------------------------------------

    /// Replaces the list of jobs the provider runs in the background.
    pub fn set_jobs(&mut self, jobs: Vec<wire::BackgroundJob>) {
        self.jobs = Some(wire::BackgroundJobs { known: true, jobs });
    }

    /// The jobs for the snapshot body: the explicit unknown until the
    /// interpreter has set them.
    pub fn jobs(&self) -> wire::BackgroundJobs {
        self.jobs.clone().unwrap_or_default()
    }

    // --- clock -----------------------------------------------------------

    /// The interpreter's clock: the newest tick. It never runs backwards,
    /// so durations between two items are never negative.
    pub fn tick(&mut self, at_ms: i64) {
        self.now_ms = self.now_ms.max(at_ms);
    }

    pub fn now_ms(&self) -> i64 {
        self.now_ms
    }

    // --- items -----------------------------------------------------------

    /// Emits an item at its full current state.
    pub fn item(&mut self, emit: &mut Emit, draft: ItemDraft) {
        let at_ms = draft.at_ms.unwrap_or_else(|| {
            self.open_items
                .get(&draft.key)
                .map_or(self.now_ms, |open| open.at_ms)
        });
        let item = Item {
            agent_id: self.agent.clone(),
            key: draft.key,
            order: 0,
            revision: 0,
            producer_version: self.producer_version.clone(),
            input_id: draft.input_id,
            text: draft.text,
            attachments: draft.attachments,
            kind: self.kind,
            body: draft.body,
            at_ms,
        };
        if draft.complete {
            self.open_items.remove(&item.key);
        } else {
            self.open_items.insert(item.key.clone(), item.clone());
        }
        // A step's items commit before its appends, and a key appears once
        // per step: this full state supersedes anything the step already
        // holds for the key.
        emit.step.appends.retain(|append| append.key != item.key);
        match emit.step.items.iter_mut().find(|held| held.key == item.key) {
            Some(held) => *held = item,
            None => emit.step.items.push(item),
        }
    }

    /// Extends the text of an open item. False, and nothing emitted, when
    /// the key is not open: the caller emits a full item instead.
    pub fn append(&mut self, emit: &mut Emit, key: &str, text: &str) -> bool {
        let Some(item) = self.open_items.get_mut(key) else {
            return false;
        };
        item.text.push_str(text);
        if let Some(held) = emit.step.items.iter_mut().find(|held| held.key == key) {
            held.text.push_str(text);
            return true;
        }
        emit.step.appends.push(Append {
            agent_id: self.agent.clone(),
            key: key.to_owned(),
            base_revision: 0,
            revision: 0,
            text: text.to_owned(),
        });
        true
    }

    /// Extends a running command's output. Past twice [`OUTPUT_CAP`] the
    /// kept text is cut to its last cap's worth, at a line boundary, and
    /// nothing is emitted: the caller sends the item again whole, with the
    /// bytes dropped in its body, and appends continue from there.
    pub fn append_output(&mut self, emit: &mut Emit, key: &str, text: &str) -> Output {
        let Some(item) = self.open_items.get_mut(key) else {
            return Output::NotOpen;
        };
        if item.text.len() + text.len() <= 2 * OUTPUT_CAP {
            self.append(emit, key, text);
            return Output::Appended;
        }
        item.text.push_str(text);
        let dropped = output_cut(&item.text);
        item.text.drain(..dropped);
        Output::Cut {
            dropped: dropped as u64,
        }
    }

    pub fn open_item(&self, key: &str) -> Option<&Item> {
        self.open_items.get(key)
    }

    // --- inputs ----------------------------------------------------------

    pub fn accept(&mut self, emit: &mut Emit, input_id: &[u8], queued: bool) {
        emit.effect(Effect::Reply {
            input_id: input_id.to_vec(),
            verdict: reply::accepted(queued),
        });
    }

    /// A refusal is a reply and nothing else: no item, no snapshot change.
    pub fn reject(&mut self, emit: &mut Emit, input_id: &[u8], refusal: Refusal) {
        emit.effect(Effect::Reply {
            input_id: input_id.to_vec(),
            verdict: reply::rejected(refusal),
        });
    }

    /// [`Self::reject`] with the refusal's own words.
    pub fn refuse(&mut self, emit: &mut Emit, input_id: &[u8], rejected: Rejected) {
        emit.effect(Effect::Reply {
            input_id: input_id.to_vec(),
            verdict: reply::refused(rejected),
        });
    }

    /// The provider takes the next prompt: started, no turn, no ask, and
    /// no steered prompt still waiting to be reflected (the provider may run
    /// it as a turn of its own, which a new submission must not overtake).
    fn ready(&self) -> bool {
        self.started && !self.busy && self.asks.is_empty() && !self.queue.has_steered()
    }

    /// A person's prompt from any client. Submitted now when the provider
    /// is idle and nothing is queued ahead of it (returned, so the kind
    /// writes it to the provider), else queued; replies either way.
    pub fn admit_prompt(
        &mut self,
        emit: &mut Emit,
        input_id: &[u8],
        prompt: PromptInput,
        sender: Sender,
    ) -> Option<QueuedInput> {
        let entry = QueuedInput {
            input_id: input_id.to_vec(),
            text: prompt.text,
            attachments: prompt.attachments,
            steer: false,
            sender: Some(sender),
        };
        if self.ready() && self.queue.is_empty() {
            self.mark_submitted(&entry.input_id);
            self.accept(emit, input_id, false);
            Some(entry)
        } else {
            self.queue.push(entry);
            self.accept(emit, input_id, true);
            None
        }
    }

    /// The head of the queue, when the provider is idle: the kind writes it
    /// to the provider. Call whenever the provider may have become idle.
    pub fn next_queued(&mut self) -> Option<QueuedInput> {
        if !self.ready() {
            return None;
        }
        let entry = self.queue.pop()?;
        self.mark_submitted(&entry.input_id);
        Some(entry)
    }

    fn mark_submitted(&mut self, input_id: &[u8]) {
        self.submitted.push(input_id.to_vec());
        if !self.turn_on_reflection {
            self.busy = true;
            self.turn.begin(self.now_ms);
        }
    }

    /// A submitted prompt starts no turn; the kind calls
    /// [`Self::turn_started`] when the provider records the prompt.
    pub fn start_turns_on_reflection(&mut self) {
        self.turn_on_reflection = true;
    }

    /// The input id of the oldest submitted prompt the provider has not yet
    /// reflected; the reflection's item carries it.
    pub fn reflect_prompt(&mut self) -> Option<Vec<u8>> {
        (!self.submitted.is_empty()).then(|| self.submitted.remove(0))
    }

    /// The submitted prompt with this input id has been reflected: true
    /// when it was awaiting its reflection.
    pub fn reflect_prompt_id(&mut self, input_id: &[u8]) -> bool {
        let at = self.submitted.iter().position(|id| id == input_id);
        at.map(|at| self.submitted.remove(at)).is_some()
    }

    pub fn awaiting_reflection(&self) -> &[Vec<u8>] {
        &self.submitted
    }

    /// Withdraws a queued prompt; refused as `REFUSAL_NOT_QUEUED` if it already left
    /// the queue.
    pub fn withdraw(&mut self, emit: &mut Emit, input_id: &[u8], target: &[u8]) {
        if self.queue.withdraw(target) {
            self.accept(emit, input_id, false);
        } else {
            self.reject(emit, input_id, Refusal::NotQueued);
        }
    }

    /// Sends a queued prompt into the running turn: marked steered and
    /// returned, so the kind hands it to the provider's steering path.
    /// Rejected not queued if it is not waiting in the queue, and
    /// unsupported if no turn is `running` or the entry is an
    /// agent's message, which only ever arrives through its own channel.
    pub fn send_now(
        &mut self,
        emit: &mut Emit,
        input_id: &[u8],
        target: &[u8],
        running: bool,
    ) -> Option<QueuedInput> {
        let Some(waiting) = self
            .queue
            .entries()
            .iter()
            .find(|entry| !entry.steer && entry.input_id == target)
        else {
            self.reject(emit, input_id, Refusal::NotQueued);
            return None;
        };
        let from_person = matches!(
            waiting
                .sender
                .as_ref()
                .and_then(|sender| sender.value.as_ref()),
            Some(sender::Value::Human(_))
        );
        if !running || !from_person {
            self.reject(emit, input_id, Refusal::Unsupported);
            return None;
        }
        let entry = self.queue.steer(target);
        self.accept(emit, input_id, false);
        entry
    }

    /// The reflection of a steered prompt landed: its entry leaves the
    /// queue and is returned.
    pub fn steer_reflected(
        &mut self,
        matches: impl Fn(&QueuedInput) -> bool,
    ) -> Option<QueuedInput> {
        self.queue.take_steered(matches)
    }

    /// The provider refused a steered prompt: it waits again.
    pub fn steer_refused(&mut self, input_id: &[u8]) {
        self.queue.unsteer(input_id);
    }

    /// Steered prompts the provider will not reflect any more (the turn was
    /// interrupted, the conversation cleared or the provider exited): they
    /// leave the queue.
    pub fn steers_lost(&mut self) {
        self.queue.drop_steered();
    }

    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    // --- asks ------------------------------------------------------------

    pub fn open_ask(&mut self, ask: A) {
        self.asks.open(ask);
    }

    pub fn close_ask(&mut self, key: &str) -> Option<A> {
        self.asks.close(key)
    }

    pub fn close_asks_for_item(&mut self, item_key: &str) -> Vec<A> {
        self.asks.close_for_item(item_key)
    }

    pub fn close_all_asks(&mut self) -> Vec<A> {
        self.asks.close_all()
    }

    pub fn asks(&self) -> &Asks<A> {
        &self.asks
    }

    /// The stale-answer rule. An answer to an open ask closes it and hands
    /// it back, so the kind writes the answer and accepts. An answer to an
    /// ask that is not open is refused as `REFUSAL_CLOSED_ASK` and writes nothing.
    pub fn answer(&mut self, emit: &mut Emit, input_id: &[u8], ask_key: &str) -> Option<A> {
        let ask = self.asks.close(ask_key);
        if ask.is_none() {
            self.reject(emit, input_id, Refusal::ClosedAsk);
        }
        ask
    }

    // --- agent messages --------------------------------------------------

    /// The provider accepted an injected agent message: it waits in the
    /// pending set until the carrier's consumption point.
    pub fn message_accepted(&mut self, envelope_id: &[u8]) {
        self.pending_messages.insert(envelope_id.to_vec());
    }

    /// The carrier consumed the message. False if it was not pending.
    pub fn message_consumed(&mut self, envelope_id: &[u8]) -> bool {
        self.pending_messages.remove(envelope_id)
    }

    pub fn pending_messages(&self) -> &BTreeSet<Vec<u8>> {
        &self.pending_messages
    }

    // --- provider and turn state ----------------------------------------

    /// The provider accepts input.
    pub fn provider_started(&mut self) {
        self.started = true;
    }

    pub fn is_started(&self) -> bool {
        self.started
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// The provider reports a turn running; idempotent with a submission.
    pub fn turn_started(&mut self) -> u64 {
        self.busy = true;
        self.turn.begin(self.now_ms)
    }

    /// A complete assistant text item: the turn's newest message so far.
    pub fn note_message(&mut self, key: &str) {
        self.turn.note_message(key);
    }

    pub fn turn(&self) -> Option<&OpenTurn> {
        self.turn.current()
    }

    /// The provider reports the turn over, ended as `outcome`: the step
    /// carries TurnEnd, and the ended turn is returned for the kind's Turn
    /// item. None when no turn was running.
    pub fn turn_ended(&mut self, emit: &mut Emit, outcome: wire::TurnOutcome) -> Option<OpenTurn> {
        self.busy = false;
        let stopping = std::mem::take(&mut self.stopping);
        let outcome = match outcome {
            wire::TurnOutcome::Completed => outcome,
            _ if stopping => wire::TurnOutcome::Stopped,
            _ => outcome,
        };
        let turn = self.turn.end()?;
        emit.step.turn_end = Some(TurnEnd {
            turn_id: turn.id,
            last_message_key: turn.last_message_key.clone(),
            outcome: outcome as i32,
        });
        Some(turn)
    }

    /// The daemon asked the agent to stop: the turn the kind now cancels
    /// ends as stopped, however the provider words its end. Set whether or
    /// not a turn is known to run: terminal Claude's turn starts on its
    /// transcript row, which may come after the stop.
    pub fn stop_requested(&mut self) {
        self.stopping = true;
    }

    /// What began as a turn was a local command (terminal Claude's
    /// /compact runs no model turn): nothing runs any more, and no TurnEnd
    /// is written because no turn happened.
    pub fn turn_abandoned(&mut self) {
        self.busy = false;
        self.turn.end();
    }

    /// The provider is gone: no turn runs and nothing is submitted. A turn
    /// cut short has no TurnEnd; the daemon reports the incarnation failed.
    pub fn provider_exited(&mut self) -> Option<OpenTurn> {
        self.started = false;
        // A clean exit stops its jobs; a provider killed alone may orphan
        // them, but nothing reports on them any more either way.
        self.set_jobs(Vec::new());
        self.busy = false;
        self.submitted.clear();
        self.queue.drop_steered();
        self.turn.end()
    }

    /// A one-shot agent may exit: no turn running, nothing queued, no
    /// accepted agent message waiting to be consumed.
    pub fn quiescent(&self) -> bool {
        !self.busy && self.queue.is_empty() && self.pending_messages.is_empty()
    }

    // --- working_on ------------------------------------------------------

    /// Set from the status tool call; never cleared automatically.
    pub fn set_working_on(&mut self, working_on: Option<String>) {
        self.working_on = working_on.filter(|text| !text.trim().is_empty());
    }

    pub fn working_on(&self) -> Option<&str> {
        self.working_on.as_deref()
    }
}

/// The working_on a status tool call declares: `{"working_on": string |
/// null}`. None when the arguments are not a status call's.
pub fn status_working_on(arguments_json: &[u8]) -> Option<Option<String>> {
    let value: serde_json::Value = serde_json::from_slice(arguments_json).ok()?;
    match value.get("working_on")? {
        serde_json::Value::Null => Some(None),
        serde_json::Value::String(text) => Some(Some(text.clone())),
        _ => None,
    }
}

/// Whether a tool-server call is amux's status tool.
pub fn is_status_tool(server: &str, tool: &str) -> bool {
    server == AMUX_TOOL_SERVER && tool == STATUS_TOOL
}

/// Whether a tool-server call is amux's attach tool.
pub fn is_attach_tool(server: &str, tool: &str) -> bool {
    server == AMUX_TOOL_SERVER && tool == ATTACH_TOOL
}

/// Whether a tool-server call is amux's send tool.
pub fn is_send_tool(server: &str, tool: &str) -> bool {
    server == AMUX_TOOL_SERVER && tool == SEND_TOOL
}

/// Where a send tool call has got to.
#[derive(Clone, Copy, Debug)]
pub enum SendOutcome<'a> {
    Running,
    /// It returned this text: `{"id": "<hex envelope id>"}` when the
    /// recipient's provider has the message.
    Returned(&'a str),
    /// It failed, was refused or never ran, with this text.
    Failed(&'a str),
}

impl<'a> SendOutcome<'a> {
    /// From a tool call's state and the text it returned.
    pub fn of(state: wire::ToolState, text: &'a str) -> Self {
        match state {
            wire::ToolState::Unspecified | wire::ToolState::Pending | wire::ToolState::Running => {
                Self::Running
            }
            wire::ToolState::Succeeded => Self::Returned(text),
            _ => Self::Failed(text),
        }
    }
}

/// A send tool call as the agent-message item it is drawn as: the text it
/// sent and the body naming the recipient and the send's state. The item
/// stays on the call's own key.
pub fn sent_message(arguments_json: &[u8], outcome: SendOutcome<'_>) -> (String, AgentMessage) {
    let arguments: serde_json::Value =
        serde_json::from_slice(arguments_json).unwrap_or(serde_json::Value::Null);
    let field = |name: &str| {
        arguments
            .get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let mut body = AgentMessage {
        kind: wire::EnvelopeKind::Message as i32,
        to: field("to"),
        context: field("context").into_bytes(),
        send_state: wire::SendState::Sending as i32,
        ..Default::default()
    };
    match outcome {
        SendOutcome::Running => {}
        SendOutcome::Returned(result) => {
            let id = serde_json::from_str::<serde_json::Value>(result)
                .ok()
                .and_then(|result| result.get("id")?.as_str().map(serde_pb::from_hex))
                .and_then(Result::ok);
            match id {
                Some(id) => {
                    body.envelope_id = id;
                    body.send_state = wire::SendState::Sent as i32;
                }
                None => {
                    body.send_state = wire::SendState::Rejected as i32;
                    body.rejection = result.to_owned();
                }
            }
        }
        SendOutcome::Failed(reason) => {
            body.send_state = wire::SendState::Rejected as i32;
            body.rejection = reason.to_owned();
        }
    }
    (field("text"), body)
}

/// A usage state as goldens print it: `?` while unknown, else its name.
pub(crate) fn describe_usage_state(state: i32) -> &'static str {
    match wire::UsageState::try_from(state).unwrap_or_default() {
        wire::UsageState::Unknown => "?",
        wire::UsageState::Ok => "ok",
        wire::UsageState::NearLimit => "near",
        wire::UsageState::Blocked => "blocked",
    }
}

/// One usage window as goldens print it: its limit, with the one model a
/// weekly window belongs to when it belongs to one, then its use and its
/// state. `unnamed` labels a length this build has no word for, from what
/// the provider said of it.
pub(crate) fn describe_usage_window(
    limit: wire::Limit,
    model: Option<&str>,
    unnamed: impl FnOnce() -> String,
    meter: Option<wire::UsageMeter>,
) -> String {
    let label = match (limit, model) {
        (wire::Limit::FiveHour, _) => "5h".to_owned(),
        (wire::Limit::Weekly, None) => "7d".to_owned(),
        (wire::Limit::Weekly, Some(model)) => format!("7d/{model}"),
        (wire::Limit::Unspecified, _) => unnamed(),
    };
    let meter = meter.unwrap_or_default();
    format!(
        " {label}:{:.0}%({})",
        meter.used_percent,
        describe_usage_state(meter.state)
    )
}

/// A background job list as goldens print it: `?` while unknown, else each
/// job as the key of the item that started it, its command and start time.
pub(crate) fn describe_jobs(jobs: Option<&wire::BackgroundJobs>) -> String {
    match jobs {
        Some(jobs) if jobs.known => format!(
            "[{}]",
            jobs.jobs
                .iter()
                .map(|job| format!(
                    "{}:{}@{}",
                    if job.item_key.is_empty() {
                        "-"
                    } else {
                        &job.item_key
                    },
                    serde_json::Value::String(job.command.clone()),
                    job.started_at_ms
                ))
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => "?".into(),
    }
}

pub(crate) fn describe_plan_ask(plan: &wire::PlanAsk) -> String {
    let mut out = format!(
        "plan:[{}]",
        plan.choices()
            .map(|choice| choice.as_str_name())
            .collect::<Vec<_>>()
            .join(",")
    );
    if !plan.switch_permission.is_empty() {
        out.push_str(&format!(" switch={}", plan.switch_permission));
    }
    out
}

pub(crate) fn describe_plan(plan: &wire::Plan) -> String {
    let mut out = plan.verdict().as_str_name().to_owned();
    if !plan.permission.is_empty() {
        out.push_str(&format!(" to={}", plan.permission));
    }
    if let Some(note) = &plan.note {
        out.push_str(&format!(" note={note:?}"));
    }
    out
}

/// An agent-message body as the goldens print it; a sent one adds its
/// recipient and how the send went.
pub(crate) fn describe_agent_message(message: &AgentMessage) -> String {
    let mut out = format!("envelope={}", serde_pb::to_hex(&message.envelope_id));
    if message.send_state != wire::SendState::Unspecified as i32 {
        out.push_str(&format!(
            " to={:?} {}",
            message.to,
            message
                .send_state()
                .as_str_name()
                .trim_start_matches("SEND_STATE_")
        ));
        if !message.rejection.is_empty() {
            out.push_str(&format!(" rejection={:?}", message.rejection));
        }
    }
    out
}

/// The kind-neutral key for an agent message's item.
pub fn agent_message_key(envelope_id: &[u8]) -> String {
    format!("agent-message:{}", serde_pb::to_hex(envelope_id))
}

/// An agent message with the text its recipient's provider reads. A failed
/// report carries how the child's incarnation ended as data, so it is worded
/// here, for a model; every other message is its own text.
pub fn worded(mut envelope: Envelope) -> Envelope {
    if envelope.kind == wire::EnvelopeKind::Failed as i32 {
        envelope.text = ended_words(envelope.ended.as_ref());
    }
    envelope
}

/// A worded agent message as its recipient's model reads it, labelled with
/// its sender and what it is: `[agent <name> finished its turn]` before a
/// child's report (or how else its turn ended: stopped, interrupted or
/// failed before it finished), `[agent <name> stopped: <cause>]` for a failed one, and
/// `[message from agent <name>]` before anything else an agent sends. The
/// provider's own channel carries only text, so without the label a model
/// takes another agent's words for its person's. A message from no agent
/// is its own text.
pub(crate) fn carried(envelope: &Envelope) -> String {
    use wire::{EnvelopeKind, sender};
    let from = match envelope.from.as_ref().and_then(|from| from.value.as_ref()) {
        Some(sender::Value::Agent(agent)) if !agent.name.is_empty() => {
            format!("agent {}", agent.name)
        }
        Some(sender::Value::Agent(_)) => "another agent".to_owned(),
        _ => return envelope.text.clone(),
    };
    match EnvelopeKind::try_from(envelope.kind).unwrap_or_default() {
        EnvelopeKind::Finished => {
            let ended = match envelope.outcome.map(wire::TurnOutcome::try_from) {
                Some(Ok(wire::TurnOutcome::Stopped)) => "was stopped before it finished",
                Some(Ok(wire::TurnOutcome::Interrupted)) => {
                    "was interrupted before it finished its turn"
                }
                Some(Ok(wire::TurnOutcome::Failed)) => "failed before it finished its turn",
                _ => "finished its turn",
            };
            format!("[{from} {ended}]\n{}", envelope.text)
        }
        EnvelopeKind::Failed => format!("[{from} stopped: {}]", envelope.text),
        _ => format!("[message from {from}]\n{}", envelope.text),
    }
}

/// Positioned text with each attachment's element written at its
/// placeholder, naming the file by hash and name but no path: for a
/// carrier the interpreter writes itself, which cannot know where the agent
/// keeps its blobs. Text with no attachments is returned as it is.
pub(crate) fn with_elements(text: &str, attachments: &[Attachment]) -> String {
    if attachments.is_empty() {
        return text.to_owned();
    }
    let mut attachments = attachments.iter();
    let mut out = String::new();
    for (index, prose) in text.split(attachments::PLACEHOLDER).enumerate() {
        if index > 0
            && let Some(attachment) = attachments.next()
        {
            out.push_str(&attachments::element(attachment, None));
        }
        out.push_str(prose);
    }
    out
}

/// What a provider reads for a queued input: its words, labelled as
/// [`carried`] labels a message when another agent sent them. A queued
/// input from an agent is a parent's message that resumed its recipient;
/// every other agent message is handed over as it arrives.
pub(crate) fn provider_text(entry: &QueuedInput) -> String {
    match entry.sender.as_ref().and_then(|from| from.value.as_ref()) {
        Some(sender::Value::Agent(_)) => carried(&Envelope {
            from: entry.sender.clone(),
            kind: wire::EnvelopeKind::Message as i32,
            text: entry.text.clone(),
            ..Default::default()
        }),
        _ => entry.text.clone(),
    }
}

/// How a child's incarnation ended, as its parent's model reads it: the
/// cause, then its own words when it has any ("the provider exited: exit
/// code 1"). A cause this build does not know reads as a plain end.
fn ended_words(end: Option<&wire::IncarnationEnd>) -> String {
    use wire::ExitCause;
    let cause = end.map_or(Ok(ExitCause::Unspecified), |end| {
        ExitCause::try_from(end.cause)
    });
    let words = match cause {
        Ok(ExitCause::Finished) => "finished",
        Ok(ExitCause::Stopped) => "stopped",
        Ok(ExitCause::Killed) => "killed",
        Ok(ExitCause::ProviderExited) => "the provider exited",
        Ok(ExitCause::DaemonLost | ExitCause::EndedWhileAway) => "ended while its daemon was away",
        Ok(ExitCause::Orphaned) => "ended while waiting for an answer nobody could give",
        Ok(ExitCause::CouldNotStart) => "could not start",
        Ok(ExitCause::DirectoryFailed) => "its directory failed",
        Ok(ExitCause::EndedUnexpectedly) => "ended unexpectedly",
        Ok(ExitCause::Unspecified) | Err(_) => "ended",
    };
    match end
        .and_then(|end| end.detail.as_deref())
        .filter(|detail| !detail.is_empty())
    {
        Some(detail) => format!("{words}: {detail}"),
        None => words.to_owned(),
    }
}

/// The body every kind's agent-message item carries.
pub fn agent_message_body(envelope: &Envelope) -> AgentMessage {
    AgentMessage {
        envelope_id: envelope.id.clone(),
        kind: envelope.kind,
        from: envelope.from.clone(),
        context: envelope.context.clone().unwrap_or_default(),
        ..Default::default()
    }
}

/// A prompt or agent message as a queue entry: the spawn's first input.
fn queued_from_bootstrap(bootstrap: &Bootstrap) -> Option<QueuedInput> {
    Some(match bootstrap.of.as_ref()? {
        bootstrap::Of::AgentMessage(envelope) => QueuedInput {
            input_id: envelope.id.clone(),
            text: worded(envelope.clone()).text,
            attachments: envelope.attachments.clone(),
            steer: false,
            sender: envelope.from.clone(),
        },
        bootstrap::Of::Prompt(submission) => {
            let prompt = submission.prompt.as_ref()?;
            QueuedInput {
                input_id: submission.input_id.clone(),
                text: prompt.text.clone(),
                attachments: prompt.attachments.clone(),
                steer: false,
                sender: Some(human()),
            }
        }
    })
}

/// The first line with visible text, placeholders removed.
fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(|line| line.replace('\u{FFFC}', "").trim().to_owned())
        .find(|line| !line.is_empty())
}

/// The sender of a person's input.
pub fn human() -> Sender {
    Sender {
        value: Some(sender::Value::Human(wire::Human {})),
    }
}

/// The new-side line of a hunk's first added or removed line, counting
/// from the hunk's start past its leading context.
pub(crate) fn first_change<S: AsRef<str>>(new_start: u32, lines: &[S]) -> Option<u32> {
    let mut at = new_start;
    for line in lines {
        match line.as_ref().chars().next() {
            Some('+' | '-') => return Some(at.max(1)),
            Some('\\') => {}
            _ => at += 1,
        }
    }
    None
}

/// The new-side line a unified diff's first change landed on: its first
/// hunk with a change, read from the hunk's header.
pub(crate) fn patch_first_change(patch: &str) -> Option<u32> {
    let mut lines = patch.lines().peekable();
    while let Some(line) = lines.next() {
        let Some(header) = line.strip_prefix("@@ ") else {
            continue;
        };
        let start = header
            .split_whitespace()
            .find_map(|range| range.strip_prefix('+'))
            .and_then(|range| range.split(',').next()?.parse::<u32>().ok())?;
        let mut body = Vec::new();
        while let Some(next) = lines.next_if(|next| !next.starts_with("@@ ")) {
            body.push(next);
        }
        if let Some(line) = first_change(start, &body) {
            return Some(line);
        }
    }
    None
}

/// The JSON at `path` inside `payload`, exactly as the provider wrote it;
/// None when it is absent or null. A parsed `Value` keeps object keys
/// sorted, so encoding one again loses the provider's order, and a form
/// schema's order is the order its fields are asked in.
pub(crate) fn json_as_written(payload: &[u8], path: &[&str]) -> Option<Vec<u8>> {
    let mut at: Box<serde_json::value::RawValue> = serde_json::from_slice(payload).ok()?;
    for key in path {
        let mut members: HashMap<String, Box<serde_json::value::RawValue>> =
            serde_json::from_str(at.get()).ok()?;
        at = members.remove(*key)?;
    }
    (at.get() != "null").then(|| at.get().as_bytes().to_vec())
}

/// The running model as a person reads it: the display name of the offered
/// model it was chosen as, when that entry stands for it; else of the
/// offered model whose value is its id; else its id tidied by the kind.
/// None while the model is unknown.
pub fn model_name(
    model: Option<&str>,
    chosen: Option<&str>,
    offered: &[wire::OfferedModel],
    tidy: fn(&str) -> String,
) -> Option<String> {
    let model = model.filter(|model| !model.is_empty())?;
    let named = |entry: &&wire::OfferedModel| !entry.display_name.is_empty();
    // An alias stands for the model when it is the model, or resolves to it
    // or to nothing it says.
    let chosen = chosen.and_then(|chosen| {
        offered.iter().filter(named).find(|entry| {
            entry.value == chosen
                && (entry.value == model
                    || entry.resolved_model.is_empty()
                    || entry.resolved_model == model)
        })
    });
    let by_id = || {
        offered
            .iter()
            .filter(named)
            .find(|entry| entry.value == model)
    };
    Some(match chosen.or_else(by_id) {
        Some(entry) => entry.display_name.clone(),
        None => tidy(model),
    })
}

/// A model id's parts read as a name: version numbers join with dots ("4",
/// "1" reads "4.1"), an o-series name stays as written, other words are
/// capitalised. `words` holds what the kind already named.
pub fn tidy_model_parts(id: &str, mut words: Vec<String>, parts: &[&str]) -> String {
    let numeric = |part: &str| part.chars().all(|c| c.is_ascii_digit() || c == '.');
    for part in parts {
        match words.last_mut() {
            Some(last)
                if numeric(part) && last.chars().last().is_some_and(|c| c.is_ascii_digit()) =>
            {
                last.push('.');
                last.push_str(part);
            }
            _ if part.starts_with('o') && part[1..].chars().all(|c| c.is_ascii_digit()) => {
                words.push((*part).to_owned())
            }
            _ => words.push(capitalised(part)),
        }
    }
    if words.is_empty() {
        id.to_owned()
    } else {
        words.join(" ")
    }
}

fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use wire::Ask;

    use super::*;

    #[test]
    fn output_is_cut_to_its_last_caps_worth_at_a_line_boundary() {
        let short = "a\n".repeat(OUTPUT_CAP);
        assert_eq!(output_cut(&short), 0, "twice the cap is kept whole");
        let lines = format!("{short}b\n");
        let cut = output_cut(&lines);
        assert!(lines.len() - cut <= OUTPUT_CAP);
        assert_eq!(&lines[cut - 1..cut], "\n", "just after a line break");
        let one_line = "é".repeat(OUTPUT_CAP + 1);
        let cut = output_cut(&one_line);
        assert!(one_line.is_char_boundary(cut));
        assert!(one_line.len() - cut <= OUTPUT_CAP);
    }

    fn shared() -> Shared<Ask> {
        let spec = AgentSpec {
            agent_id: b"a".to_vec(),
            created_at_ms: 10,
            ..Default::default()
        };
        Shared::new(&spec, wire::Kind::Unspecified, "v")
    }

    #[test]
    fn a_one_shot_agent_is_quiet_only_with_nothing_queued_running_or_pending() {
        let mut shared = shared();
        shared.provider_started();
        assert!(shared.quiescent());

        shared.message_accepted(b"e1");
        assert!(!shared.quiescent(), "an accepted message is still pending");
        assert!(!shared.message_consumed(b"other"));
        assert!(shared.message_consumed(b"e1"));
        assert!(shared.quiescent());

        let mut emit = Emit::default();
        let prompt = PromptInput {
            text: "go".into(),
            ..Default::default()
        };
        assert!(
            shared
                .admit_prompt(&mut emit, b"p1", prompt.clone(), human())
                .is_some()
        );
        assert!(!shared.quiescent(), "a turn is running");
        assert!(
            shared
                .admit_prompt(&mut emit, b"p2", prompt, human())
                .is_none()
        );
        assert_eq!(
            shared
                .turn_ended(&mut emit, wire::TurnOutcome::Completed)
                .map(|turn| turn.id),
            Some(1)
        );
        assert!(!shared.quiescent(), "p2 is queued");
        assert_eq!(
            shared.next_queued().map(|entry| entry.input_id),
            Some(b"p2".to_vec())
        );
        shared.turn_ended(&mut emit, wire::TurnOutcome::Completed);
        assert!(shared.quiescent());
    }

    #[test]
    fn a_provider_exit_mid_turn_has_no_turn_end() {
        let mut shared = shared();
        shared.provider_started();
        shared.turn_started();
        let mut emit = Emit::default();
        assert_eq!(shared.provider_exited().map(|turn| turn.id), Some(1));
        assert!(
            shared
                .turn_ended(&mut emit, wire::TurnOutcome::Completed)
                .is_none()
        );
        assert!(emit.step.turn_end.is_none());
        assert_eq!(shared.phase(), Phase::Starting);
    }

    #[test]
    fn since_when_moves_only_when_the_phase_changes() {
        let mut shared = shared();
        let since = |step: &Step| step.snapshot.as_ref().map(|s| (s.phase, s.phase_since_ms));
        let first = shared.initial_step(Vec::new());
        assert_eq!(since(&first), Some((Phase::Starting as i32, 10)));

        shared.tick(20);
        shared.provider_started();
        let idle = shared.finish(Emit::default(), Vec::new()).step;
        assert_eq!(since(&idle), Some((Phase::Idle as i32, 20)));

        shared.tick(30);
        shared.turn_started();
        let mut emit = Emit::default();
        shared.item(
            &mut emit,
            ItemDraft {
                key: "t".into(),
                text: "he".into(),
                ..Default::default()
            },
        );
        let working = shared.finish(emit, Vec::new()).step;
        assert_eq!(since(&working), Some((Phase::Working as i32, 30)));

        shared.tick(40);
        let mut emit = Emit::default();
        assert!(shared.append(&mut emit, "t", "llo"));
        let chunk = shared.finish(emit, Vec::new()).step;
        assert_eq!(chunk.appends.len(), 1);
        assert_eq!(
            since(&chunk),
            None,
            "a streamed chunk publishes no snapshot"
        );

        shared.tick(50);
        shared.set_working_on(Some("elsewhere".into()));
        let same_phase = shared.finish(Emit::default(), Vec::new()).step;
        assert_eq!(
            since(&same_phase),
            Some((Phase::Working as i32, 30)),
            "a snapshot within the phase keeps its stamp"
        );

        shared.tick(60);
        let mut emit = Emit::default();
        shared.turn_ended(&mut emit, wire::TurnOutcome::Completed);
        let done = shared.finish(emit, Vec::new()).step;
        assert_eq!(since(&done), Some((Phase::Idle as i32, 60)));
    }

    #[test]
    fn git_facts_ride_the_snapshot_and_a_folder_outside_a_repository_has_none() {
        let mut shared = shared();
        let git = |step: &Step| step.snapshot.as_ref().map(|s| s.git.clone());
        assert_eq!(git(&shared.initial_step(Vec::new())), Some(None));

        let facts = wire::Git {
            branch: Some("fix".into()),
            ..Default::default()
        };
        shared.set_git(Some(facts.clone()));
        let read = shared.finish(Emit::default(), Vec::new()).step;
        assert_eq!(git(&read), Some(Some(facts.clone())));
        shared.set_git(Some(facts));
        let same = shared.finish(Emit::default(), Vec::new()).step;
        assert_eq!(git(&same), None, "the same facts publish nothing");

        shared.set_git(None);
        let gone = shared.finish(Emit::default(), Vec::new()).step;
        assert_eq!(git(&gone), Some(None));
    }

    #[test]
    fn an_open_ask_outranks_every_other_phase() {
        let mut shared = shared();
        shared.open_ask(Ask {
            key: "k".into(),
            ..Default::default()
        });
        assert_eq!(shared.phase(), Phase::NeedsYou, "even while starting");
        shared.provider_started();
        shared.turn_started();
        assert_eq!(shared.phase(), Phase::NeedsYou);
        shared.close_ask("k");
        assert_eq!(shared.phase(), Phase::Working);
    }

    #[test]
    fn working_on_is_set_and_cleared_only_by_the_status_call() {
        assert_eq!(
            status_working_on(br#"{"working_on": "x"}"#),
            Some(Some("x".into()))
        );
        assert_eq!(status_working_on(br#"{"working_on": null}"#), Some(None));
        assert_eq!(status_working_on(br#"{"other": 1}"#), None);
        assert!(is_status_tool("amux", "status"));
        assert!(!is_status_tool("other", "status"));
        let mut shared = shared();
        shared.set_working_on(Some("  ".into()));
        assert_eq!(shared.working_on(), None);
    }

    #[test]
    fn a_checkpoint_round_trips_every_field() {
        let mut shared = shared();
        let mut emit = Emit::default();
        shared.provider_started();
        shared.admit_prompt(&mut emit, b"p1", PromptInput::default(), human());
        shared.admit_prompt(&mut emit, b"p2", PromptInput::default(), human());
        shared.open_ask(Ask {
            key: "k".into(),
            item_key: "t".into(),
            ..Default::default()
        });
        shared.message_accepted(&[0, 255]);
        shared.set_working_on(Some("w".into()));
        shared.item(
            &mut emit,
            ItemDraft {
                key: "m".into(),
                text: "open".into(),
                ..Default::default()
            },
        );
        shared.finish(emit, Vec::new());
        let json = serde_json::to_string(&shared).unwrap();
        let back: Shared<Ask> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, shared);
    }

    #[test]
    fn json_as_written_keeps_the_providers_key_order() {
        let payload = br#"{"request":{"requested_schema":{"properties":{"title":{"type":"string"},"body":{"type":"string"},"assignee":{"type":"string"}}}}}"#;
        let schema = json_as_written(payload, &["request", "requested_schema"]).unwrap();
        assert_eq!(
            String::from_utf8(schema).unwrap(),
            r#"{"properties":{"title":{"type":"string"},"body":{"type":"string"},"assignee":{"type":"string"}}}"#
        );
        assert_eq!(json_as_written(br#"{"request":null}"#, &["request"]), None);
        assert_eq!(
            json_as_written(br#"{"request":{}}"#, &["request", "x"]),
            None
        );
    }

    #[test]
    fn an_edit_lands_on_its_first_changed_line_past_the_context() {
        assert_eq!(first_change(10, &[" a", " b", "-c", "+d"]), Some(12));
        assert_eq!(first_change(1, &["+new"]), Some(1));
        assert_eq!(first_change(0, &["-gone"]), Some(1));
        assert_eq!(first_change(4, &[" only context"]), None);
        assert_eq!(patch_first_change("@@ -1 +1 @@\n-one\n+two\n"), Some(1));
        let patch = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -7,4 +7,4 @@ fn x\n keep\n keep\n-old\n+new\n";
        assert_eq!(patch_first_change(patch), Some(9));
        assert_eq!(patch_first_change("just text\n"), None);
    }

    fn offered(value: &str, display_name: &str, resolved_model: &str) -> wire::OfferedModel {
        wire::OfferedModel {
            value: value.into(),
            display_name: display_name.into(),
            resolved_model: resolved_model.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_model_is_named_by_its_choice_then_its_id_then_tidied() {
        let offered = [
            offered("opus", "Opus", "claude-opus-5-5"),
            offered("fable", "Fable", ""),
            offered("claude-sonnet-5", "Sonnet 5 (offered)", ""),
        ];
        let name =
            |model, chosen| super::model_name(model, chosen, &offered, |id| format!("tidy {id}"));
        assert_eq!(
            name(Some("claude-opus-5-5"), Some("opus")).as_deref(),
            Some("Opus")
        );
        assert_eq!(
            name(Some("claude-fable-5-1"), Some("fable")).as_deref(),
            Some("Fable"),
            "an alias that says nothing of what it resolves to stands for the model"
        );
        assert_eq!(
            name(Some("claude-sonnet-5"), Some("opus")).as_deref(),
            Some("Sonnet 5 (offered)"),
            "a choice resolving elsewhere does not name it; the entry with its id does"
        );
        assert_eq!(
            name(Some("claude-haiku-4-5"), None).as_deref(),
            Some("tidy claude-haiku-4-5")
        );
        assert_eq!(name(None, Some("opus")), None);
    }

    #[test]
    fn claude_ids_tidy_into_names() {
        let claude = crate::claude_common::tidy_model;
        assert_eq!(claude("claude-opus-4-1-20250805"), "Opus 4.1");
        assert_eq!(claude("claude-opus-5[1m]"), "Opus 5");
        assert_eq!(claude("claude-haiku-4-5-20251001"), "Haiku 4.5");
        assert_eq!(claude("opus"), "Opus");
    }
}
