//! The session driver: one open chat's stream, its RPCs and its model.
//!
//! The driver owns the Subscribe stream and the client handle and feeds
//! everything into [`SessionState`]; nothing else about a chat does I/O. It
//! never holds a cursor and never sees the origin: the local runtime answers
//! from its rows, and CaughtUp and Detached on the stream say whether the
//! chat is current.

use std::collections::{BTreeSet, HashMap};
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use client::{Client, Clock, EventStream, RpcError};
use futures_util::{FutureExt as _, StreamExt as _};
use prost::Message as _;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;
use ui_state::{
    BlobStatus, Connection, InputId, InputOutcome, InputState, Key, Msg, Outcome, Refusal,
    SessionState,
};
use ui_view::SessionLine;
use wire::dump::{ClientSessionEvents, ClientSessionHistory, ClientTraced, client_traced};
use wire::{
    Agent, BlobRef, DumpRequest, ErrorCode, GetBlobRequest, GetItemRequest, HostEntry, Input,
    ListItemsRequest, PromptSubmission, PutBlobRequest, ResumeAgentRequest, SendInputRequest,
    SendInputResponse, SessionEvent, SubscribeSessionRequest, send_input_response, session_event,
};

use crate::trace::{DriverEvent, DriverTrace, Ring, TraceEvent};

/// Prefixes the reason of a dump a chat's transcript asked for.
pub const TRANSCRIPT_DUMP: &str = "the client's transcript: ";
/// The check a transcript that fails its own check broke, as dumps and
/// usage events name it.
pub const TRANSCRIPT_CHECK: &str = "transcript.window";
use crate::{Backoff, inputs};

/// What became of a send.
#[derive(Clone, Debug, PartialEq)]
pub struct Sent {
    pub id: InputId,
    pub outcome: InputOutcome,
}

/// Why an act on the chat did not take effect.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InputError {
    #[error("rejected: {0:?}")]
    Rejected(Refusal),
    /// It never left: the agent's host could not be reached. The client
    /// holds it, shown pending, and sends it once the host is back.
    #[error("held: the agent's host cannot be reached; it goes once it is back")]
    Held,
    /// The connection dropped before the verdict: the chat shows it not
    /// confirmed until it has caught up again.
    #[error("not confirmed: the connection dropped before the agent answered")]
    Uncertain,
}

/// Why an older page did not arrive.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum PageError {
    /// Older history is held only by the agent's host, which cannot be
    /// reached. The chat says so; it is never an empty page.
    #[error("older history is on the agent's host, which cannot be reached")]
    OriginUnreachable,
    #[error(transparent)]
    Rpc(RpcError),
}

/// What changed since the host last asked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Every row key an update may have changed, once each.
    pub keys: Vec<Key>,
    /// A Reset's transcript was swapped in: every row id may be new.
    pub reloaded: bool,
    /// Something outside the rows moved.
    pub session: bool,
}

impl Changes {
    fn absorb(&mut self, outcome: &Outcome, seen: &mut BTreeSet<Key>) {
        for key in &outcome.changed {
            if seen.insert(key.clone()) {
                self.keys.push(key.clone());
            }
        }
        self.reloaded |= outcome.reloaded;
        self.session |= outcome.session;
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && !self.reloaded && !self.session
    }
}

struct Model {
    state: SessionState,
    trace: Ring<SessionState, Msg>,
    /// The trace as it stood when the transcript first failed its check,
    /// ending at the message that broke it: what the dump carries.
    cut: Option<DriverTrace<SessionState, Msg>>,
    changes: Changes,
    changed_keys: BTreeSet<Key>,
    /// Attachment bytes fetched during this open. Dropped with the session.
    blobs: HashMap<Vec<u8>, Arc<[u8]>>,
    /// The runtime refused to serve the stream again: the agent is gone.
    ended: Option<RpcError>,
    /// What home last drew for this agent, as of the rows applied so far;
    /// home is woken when a row moves it.
    home_line: Option<SessionLine>,
}

/// Called when something outside the rows arrived on the stream, or rows
/// that move the line home draws for the agent: what the fleet's home is
/// woken by.
pub(crate) type HomeWake = Box<dyn Fn() + Send + Sync>;

/// The catalogues this client fetched, by hash: one fetch per version,
/// shared by every chat whose agent offers the same.
#[derive(Default)]
pub(crate) struct Catalogues(Mutex<HashMap<Vec<u8>, wire::Catalogue>>);

impl Catalogues {
    fn get(&self, hash: &[u8]) -> Option<wire::Catalogue> {
        self.held().get(hash).cloned()
    }

    fn keep(&self, hash: Vec<u8>, catalogue: wire::Catalogue) {
        self.held().insert(hash, catalogue);
    }

    fn held(&self) -> MutexGuard<'_, HashMap<Vec<u8>, wire::Catalogue>> {
        self.0.lock().unwrap_or_else(|poison| poison.into_inner())
    }
}

pub(crate) struct Inner {
    client: Arc<dyn Client>,
    agent_id: Vec<u8>,
    /// The rows a (re)opened stream starts with.
    tail: AtomicU32,
    clock: Arc<dyn Clock>,
    model: Mutex<Model>,
    changed: watch::Sender<()>,
    home: OnceLock<HomeWake>,
    /// Whether anybody takes the changes: a session held off screen does
    /// not gather them, so they cannot grow while nobody reads them.
    gathering: AtomicBool,
    /// The model asked for a reload: the pump reopens the stream.
    reload: Notify,
    closed: AtomicBool,
    /// The transcript failed its check and a dump was asked for.
    dumped: AtomicBool,
    catalogues: Arc<Catalogues>,
    /// The catalogue hash last fetched for; asked again only when a chat
    /// opens anew.
    asked: Mutex<Option<Vec<u8>>>,
}

impl Inner {
    fn model(&self) -> MutexGuard<'_, Model> {
        self.model
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Applies one message; after close, a late result changes nothing.
    fn apply(&self, msg: Msg) -> Outcome {
        self.apply_then(msg, |_| ()).0
    }

    /// [`Self::apply`], reading the state the message left under the same
    /// lock, before any other message can change it; None after close.
    fn apply_then<R>(
        &self,
        msg: Msg,
        read: impl FnOnce(&SessionState) -> R,
    ) -> (Outcome, Option<R>) {
        if self.closed.load(Ordering::Acquire) {
            return (Outcome::default(), None);
        }
        let at_ms = self.clock.now_ms();
        let streamed = matches!(msg, Msg::Event(_));
        let mut model = self.model();
        let Model {
            state,
            trace,
            cut,
            changes,
            changed_keys,
            home_line,
            ..
        } = &mut *model;
        trace.record(state, at_ms, TraceEvent::Msg(msg.clone()));
        let outcome = state.update(msg);
        // Taken under the same guard, before another message applies.
        if outcome.broken.is_some() && cut.is_none() && !self.dumped.load(Ordering::Acquire) {
            *cut = Some(trace.trace());
        }
        let read = read(state);
        if self.gathering.load(Ordering::Acquire) {
            changes.absorb(&outcome, changed_keys);
        }
        let home = self.home.get().filter(|_| streamed);
        // Rows wake home only when they move the line it draws for the
        // agent: a new running step, the step ending, what it last said.
        // Read at time zero, so a step's age alone never counts as a move.
        let line_moved = home.is_some()
            && (!outcome.changed.is_empty() || outcome.reloaded || outcome.session)
            && {
                let line = ui_view::session_line(state, 0);
                home_line.replace(line.clone()).as_ref() != Some(&line)
            };
        drop(model);
        let moved = !outcome.changed.is_empty() || outcome.reloaded || outcome.session;
        if moved {
            self.changed.send_replace(());
        }
        // Besides those rows, a snapshot (and with it a turn's end), the
        // queue or the stream's markers wake home.
        if let Some(home) = home
            && (outcome.session || line_moved)
        {
            home();
        }
        if outcome.reload {
            self.reload.notify_one();
        }
        (outcome, Some(read))
    }

    fn note(&self, event: DriverEvent) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let at_ms = self.clock.now_ms();
        let mut model = self.model();
        let Model { state, trace, .. } = &mut *model;
        trace.record(state, at_ms, TraceEvent::Driver(event));
    }

    fn subscribe_request(&self) -> SubscribeSessionRequest {
        SubscribeSessionRequest {
            agent_id: self.agent_id.clone(),
            tail: self.tail.load(Ordering::Acquire),
        }
    }

    /// Applies one stream event, answering an Append whose base is not
    /// held with Get before the next event, so later appends meet it.
    async fn event(self: &Arc<Self>, event: SessionEvent) -> Outcome {
        let outcome = self.apply(Msg::Event(event));
        if outcome.session {
            self.want_catalogue();
        }
        self.resend(outcome.resend.clone());
        if let Some(why) = &outcome.broken {
            self.dump_broken(why);
        }
        if let Some(key) = &outcome.need_get {
            self.note(DriverEvent::Get { key: key.clone() });
            let request = GetItemRequest {
                agent_id: self.agent_id.clone(),
                key: key.clone(),
            };
            match self.client.get_item(request).await {
                Ok(item) => {
                    self.apply(Msg::Event(SessionEvent {
                        of: Some(session_event::Of::Item(item)),
                    }));
                }
                Err(error) => self.note(DriverEvent::GetFailed {
                    key: key.clone(),
                    error: error.to_string(),
                }),
            }
        }
        outcome
    }

    /// Asks the runtime for a dump of the agent with this session's trace
    /// as it stood at the message that broke the transcript: a transcript
    /// that fails its own check is a client bug, and a replay of the trace
    /// stops where it broke. Once per session, since the same transcript
    /// fails again at the next CaughtUp.
    fn dump_broken(self: &Arc<Self>, why: &str) {
        let Some(cut) = self.model().cut.take() else {
            return;
        };
        if self.dumped.swap(true, Ordering::AcqRel) {
            return;
        }
        self.note(DriverEvent::Broken {
            why: why.to_owned(),
        });
        let request = DumpRequest {
            agent_ids: vec![self.agent_id.clone()],
            reason: format!("{TRANSCRIPT_DUMP}{why}"),
            automatic: true,
            sessions: vec![cut.history(&self.agent_id).encode_to_vec()],
            scope: wire::DumpScope::Transcript.into(),
            check: TRANSCRIPT_CHECK.to_owned(),
        };
        let inner = self.clone();
        tokio::spawn(async move {
            if let Err(error) = inner.client.dump(request).await {
                inner.note(DriverEvent::DumpFailed {
                    error: error.to_string(),
                });
            }
        });
    }

    /// This session as a dump carries it: the trace from the state its
    /// oldest segment began from.
    fn history(&self) -> ClientSessionHistory {
        self.model().trace.trace().history(&self.agent_id)
    }

    /// On screen, fetches the catalogue the newest snapshot names unless
    /// this client holds it; off screen nothing is fetched.
    fn want_catalogue(self: &Arc<Self>) {
        if !self.gathering.load(Ordering::Acquire) || self.closed.load(Ordering::Acquire) {
            return;
        }
        let (wanted, held) = {
            let model = self.model();
            (
                model.state.agent_state().catalogue.clone(),
                model.state.held_catalogue().map(<[u8]>::to_vec),
            )
        };
        let Some(wanted) = wanted else {
            return;
        };
        if held.as_ref() == Some(&wanted) {
            return;
        }
        if let Some(catalogue) = self.catalogues.get(&wanted) {
            self.apply(Msg::Catalogue {
                hash: wanted,
                catalogue,
            });
            return;
        }
        {
            let mut asked = self
                .asked
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if asked.as_ref() == Some(&wanted) {
                return;
            }
            *asked = Some(wanted.clone());
        }
        tokio::spawn(self.clone().fetch_catalogue(wanted));
    }

    /// Reads the catalogue blob the snapshot named and keeps it under the
    /// hash asked for; it shows while a snapshot names it.
    async fn fetch_catalogue(self: Arc<Self>, wanted: Vec<u8>) {
        self.note(DriverEvent::Catalogue {
            hash: wanted.clone(),
        });
        let request = GetBlobRequest {
            agent_id: self.agent_id.clone(),
            hash: wanted.clone(),
        };
        let fetched = match self.client.get_blob(request).await {
            Ok(fetched) => wire::Catalogue::decode(fetched.bytes.as_slice())
                .map_err(|error| format!("the catalogue does not decode: {error}")),
            Err(error) => Err(error.to_string()),
        };
        match fetched {
            Ok(catalogue) => {
                self.catalogues.keep(wanted.clone(), catalogue.clone());
                self.apply(Msg::Catalogue {
                    hash: wanted,
                    catalogue,
                });
            }
            Err(error) => self.note(DriverEvent::CatalogueFailed {
                hash: wanted,
                error,
            }),
        }
    }

    async fn send(self: &Arc<Self>, mut input: Input) -> Sent {
        if input.input_id.is_empty() {
            input.input_id = inputs::input_id();
        }
        let id = input.input_id.clone();
        // Sent while the chat is not current: held, never handed on, until
        // the chat catches up again, which then sends it. Read under the
        // lock the send applied in: a catch-up in between would release it
        // to its resend, and this send would deliver it a second time.
        let (_, state) = self.apply_then(Msg::Send(input.clone()), |state| state.input_state(&id));
        if state.flatten() == Some(InputState::Held) {
            return Sent {
                id,
                outcome: InputOutcome::Held,
            };
        }
        self.deliver(input).await
    }

    /// Held inputs whose host is back: each goes again under its own id.
    fn resend(self: &Arc<Self>, inputs: Vec<Input>) {
        for input in inputs {
            self.note(DriverEvent::Resend {
                id: input.input_id.clone(),
            });
            let inner = self.clone();
            tokio::spawn(async move {
                inner.deliver(input).await;
            });
        }
    }

    /// Hands an input the state already holds as sent to the runtime and
    /// applies the verdict; one held by it that the host's return already
    /// overtook goes again at once.
    async fn deliver(self: &Arc<Self>, input: Input) -> Sent {
        let id = input.input_id.clone();
        let request = SendInputRequest {
            agent_id: self.agent_id.clone(),
            input: Some(input),
        };
        let outcome = match self.client.send_input(request).await {
            Ok(reply) => InputOutcome::Reply(reply),
            Err(RpcError::Transport(_)) => InputOutcome::Lost,
            // The runtime handed the input on and lost the agent's answer:
            // it may or may not have arrived.
            Err(error) if error.code() == Some(ErrorCode::Aborted) => InputOutcome::Lost,
            // Any other refusal never reached the agent, which is a
            // rejection, never uncertain.
            Err(RpcError::Refused(error)) => InputOutcome::Reply(refused(error)),
        };
        let applied = self.apply(Msg::Sent(id.clone(), outcome.clone()));
        self.resend(applied.resend);
        Sent { id, outcome }
    }

    async fn fetch_blob(self: Arc<Self>, hash: Vec<u8>) {
        self.note(DriverEvent::Blob { hash: hash.clone() });
        self.apply(Msg::Blob {
            hash: hash.clone(),
            status: BlobStatus::Fetching,
        });
        let request = GetBlobRequest {
            agent_id: self.agent_id.clone(),
            hash: hash.clone(),
        };
        let status = match self.client.get_blob(request).await {
            Ok(blob) => {
                self.model().blobs.insert(hash.clone(), blob.bytes.into());
                BlobStatus::Ready
            }
            Err(error) => BlobStatus::Failed(error.to_string()),
        };
        self.apply(Msg::Blob { hash, status });
    }
}

fn rejected(refusal: wire::Rejected) -> SendInputResponse {
    SendInputResponse {
        of: Some(send_input_response::Of::Rejected(refusal)),
    }
}

/// A call the daemon refused before the agent had it: a host it cannot
/// reach, or the daemon's own words for anything else.
fn refused(error: wire::Error) -> SendInputResponse {
    rejected(match error.code() {
        ErrorCode::Unavailable => wire::Rejected {
            refusal: wire::Refusal::HostUnreachable as i32,
            detail: None,
        },
        _ => wire::Rejected {
            refusal: wire::Refusal::Unspecified as i32,
            detail: Some(error.message),
        },
    })
}

/// The session's state while the guard lives. Never hold it across an
/// await: the pump waits for it to apply the next event.
pub struct StateGuard<'a>(MutexGuard<'a, Model>);

impl StateGuard<'_> {
    /// The bounded trace: the state its oldest segment began from and every
    /// event since, in this client's order. Read under the same guard as
    /// the state, so it replays to exactly that state while events arrive.
    pub fn trace(&self) -> DriverTrace<SessionState, Msg> {
        self.0.trace.trace()
    }
}

impl Deref for StateGuard<'_> {
    type Target = SessionState;

    fn deref(&self) -> &SessionState {
        &self.0.state
    }
}

/// One agent's chat. The fleet holds one for every live agent, with a
/// small window while it is off screen, and widens it for a chat.
pub struct Session {
    inner: Arc<Inner>,
    /// None while suspended.
    pump: Mutex<Option<JoinHandle<()>>>,
}

/// How a stream the pump was reading ended.
enum End {
    /// Lagged: the runtime closed it; reopen with a tail at once.
    Lagged,
    /// Back in the foreground: reopen with a tail at once.
    Resumed,
    /// The reader returned after the head moved on: reopen with a tail at
    /// once and build the window from it apart.
    Reload,
    /// The stream is gone: reconnect with backoff.
    Closed(Option<RpcError>),
}

impl Session {
    /// Subscribes with a tail of `tail` rows and resolves once the Snapshot
    /// and the rows the runtime already holds are applied, so the first
    /// render is correct: at once for an own agent, and from the replica
    /// for another host's even with that host away. CaughtUp may follow.
    /// While the reader follows, the window keeps at most `cap` rows, never
    /// fewer than the tail.
    pub async fn open(
        client: Arc<dyn Client>,
        agent: Agent,
        tail: u32,
        cap: u32,
        clock: impl Clock,
    ) -> Result<Session, RpcError> {
        Self::start(client, agent, tail, cap, clock, true, Arc::default()).await
    }

    /// Opens on screen, fetching the catalogue, or off screen, where
    /// nothing is fetched until a chat shows it.
    pub(crate) async fn start(
        client: Arc<dyn Client>,
        agent: Agent,
        tail: u32,
        cap: u32,
        clock: impl Clock,
        on_screen: bool,
        catalogues: Arc<Catalogues>,
    ) -> Result<Session, RpcError> {
        let state = SessionState::new(agent.clone(), cap.max(tail) as usize);
        let (changed, _) = watch::channel(());
        let inner = Arc::new(Inner {
            client,
            agent_id: agent.agent_id,
            tail: AtomicU32::new(tail),
            clock: Arc::new(clock),
            model: Mutex::new(Model {
                trace: Ring::new(state.clone()),
                cut: None,
                state,
                changes: Changes::default(),
                changed_keys: BTreeSet::new(),
                blobs: HashMap::new(),
                ended: None,
                home_line: None,
            }),
            changed,
            home: OnceLock::new(),
            gathering: AtomicBool::new(on_screen),
            reload: Notify::new(),
            closed: AtomicBool::new(false),
            dumped: AtomicBool::new(false),
            catalogues,
            asked: Mutex::new(None),
        });
        let mut stream = inner
            .client
            .subscribe_session(inner.subscribe_request())
            .await?;
        inner.note(DriverEvent::Subscribed { tail });
        inner.apply(Msg::Connection(Connection::Live));
        let mut ended = None;
        // The Opening and the Snapshot lead the opening.
        loop {
            match stream.next().await {
                Some(Ok(event)) => {
                    let snapshot = matches!(event.of, Some(session_event::Of::Snapshot(_)));
                    let lagged = matches!(event.of, Some(session_event::Of::Lagged(_)));
                    inner.event(event).await;
                    if lagged {
                        ended = Some(End::Lagged);
                        break;
                    }
                    if snapshot {
                        break;
                    }
                }
                Some(Err(error)) => {
                    return Err(error);
                }
                None => {
                    return Err(RpcError::Transport(
                        "the stream ended before its snapshot".into(),
                    ));
                }
            }
        }
        // The held rows are already on their way: apply what has arrived,
        // up to the marker. Anything later is the pump's.
        while ended.is_none() {
            let Some(next) = stream.next().now_or_never() else {
                break;
            };
            match next {
                Some(Ok(event)) => {
                    let marker = matches!(
                        event.of,
                        Some(session_event::Of::CaughtUp(_) | session_event::Of::Detached(_))
                    );
                    let lagged = matches!(event.of, Some(session_event::Of::Lagged(_)));
                    inner.event(event).await;
                    if lagged {
                        ended = Some(End::Lagged);
                    } else if marker {
                        break;
                    }
                }
                Some(Err(error)) => ended = Some(End::Closed(Some(error))),
                None => ended = Some(End::Closed(None)),
            }
        }
        inner.want_catalogue();
        let pump = tokio::spawn(pump(inner.clone(), Some(stream), ended));
        Ok(Session {
            inner,
            pump: Mutex::new(Some(pump)),
        })
    }

    /// Wakes the fleet's home when something outside the rows arrives, or
    /// rows that move the line home draws for the agent.
    pub(crate) fn on_home(&self, wake: HomeWake) {
        {
            let mut model = self.inner.model();
            model.home_line = Some(ui_view::session_line(&model.state, 0));
        }
        let _ = self.inner.home.set(wake);
    }

    /// Sets the window: the tail a reopened stream starts with and the most
    /// rows held while the reader follows. Off screen the reader follows
    /// and nobody takes the changes; on screen they are gathered from now.
    pub(crate) fn set_window(&self, tail: u32, cap: u32, on_screen: bool) {
        self.inner.tail.store(tail, Ordering::Release);
        self.inner.note(DriverEvent::Window { tail, cap });
        {
            let mut model = self.inner.model();
            model.changes = Changes::default();
            model.changed_keys.clear();
        }
        let was_on_screen = self.inner.gathering.swap(on_screen, Ordering::AcqRel);
        if !on_screen {
            self.inner.apply(Msg::Following(true));
        }
        self.inner.apply(Msg::Window(cap.max(tail) as usize));
        if on_screen && !was_on_screen {
            // A chat opening anew asks again after a failed fetch.
            *self
                .inner
                .asked
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = None;
        }
        if on_screen {
            self.inner.want_catalogue();
        }
    }

    /// Out of the foreground the stream is dropped, the rows stay and the
    /// chat reads as reconnecting; back in it, the stream reopens with a
    /// tail at once.
    pub(crate) fn set_foreground(&self, foreground: bool) {
        if foreground {
            self.reopen();
            return;
        }
        let Some(pump) = self.pump().take() else {
            return;
        };
        pump.abort();
        self.inner.note(DriverEvent::Suspended);
        self.inner.apply(Msg::Connection(Connection::Reconnecting));
    }

    fn reopen(&self) {
        let mut pump = self.pump();
        if pump.is_some() || self.inner.model().ended.is_some() {
            return;
        }
        self.inner.note(DriverEvent::Resumed);
        *pump = Some(tokio::spawn(self::pump(
            self.inner.clone(),
            None,
            Some(End::Resumed),
        )));
    }

    /// Whether the stream is dropped for the phone's background.
    pub fn suspended(&self) -> bool {
        self.pump().is_none()
    }

    fn pump(&self) -> MutexGuard<'_, Option<JoinHandle<()>>> {
        self.pump
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub fn agent_id(&self) -> &[u8] {
        &self.inner.agent_id
    }

    pub fn state(&self) -> StateGuard<'_> {
        StateGuard(self.inner.model())
    }

    /// Level-triggered: marked on every change; the host draws on change
    /// and never on a clock, so changes during a draw collapse into one.
    pub fn changed(&self) -> watch::Receiver<()> {
        self.inner.changed.subscribe()
    }

    /// Everything that changed since the last call, for a host that
    /// reconfigures only the rows an update touched.
    pub fn take_changes(&self) -> Changes {
        let mut model = self.inner.model();
        model.changed_keys.clear();
        std::mem::take(&mut model.changes)
    }

    /// Set when the runtime would no longer serve the chat, as when the
    /// agent was deleted: the host closes it to the fleet.
    pub fn ended(&self) -> Option<RpcError> {
        self.inner.model().ended.clone()
    }

    /// The agent's inventory row, from the fleet: lifecycle and phase.
    pub fn set_entry(&self, agent: Agent) {
        self.inner.apply(Msg::Entry(agent));
    }

    /// The agent's host, from the fleet, for the header.
    pub fn set_host(&self, host: HostEntry) {
        let outcome = self.inner.apply(Msg::Host(host));
        self.inner.resend(outcome.resend);
    }

    /// Where the reader is: at the newest row, or in history. A session
    /// starts following; the client says when that changes.
    pub fn follow(&self, following: bool) {
        self.inner.apply(Msg::Following(following));
    }

    /// Fetches up to `n` rows older than the oldest held and merges them
    /// under the window; returns how many arrived. While the reader follows
    /// it fetches only what fits under the cap, and nothing at the cap.
    pub async fn page_older(&self, n: u32) -> Result<usize, PageError> {
        let (before, epoch, room) = {
            let model = self.inner.model();
            (
                model.state.oldest_order(),
                model.state.epoch(),
                model.state.page_room(),
            )
        };
        let n = room.map_or(n, |room| n.min(u32::try_from(room).unwrap_or(u32::MAX)));
        if n == 0 {
            return Ok(0);
        }
        self.inner.note(DriverEvent::Page { before, limit: n });
        let request = ListItemsRequest {
            agent_id: self.inner.agent_id.clone(),
            before_order: before,
            page_size: n,
        };
        match self.inner.client.list_items(request).await {
            Ok(page) => {
                let count = page.items.len();
                self.inner.apply(Msg::Page {
                    items: page.items,
                    exhausted: page.exhausted,
                    epoch,
                });
                Ok(count)
            }
            Err(error) => {
                self.inner.note(DriverEvent::PageFailed {
                    error: error.to_string(),
                });
                if error.reason() == Some(wire::ErrorReason::HostUnreachable) {
                    Err(PageError::OriginUnreachable)
                } else {
                    Err(PageError::Rpc(error))
                }
            }
        }
    }

    /// Sends an input, under a fresh id when it has none. The optimistic
    /// row exists from now. A prompt or an answer that certainly never left
    /// (the chat not current, or its host unreachable) is held and goes
    /// again under its id once the host is back; a dropped connection
    /// leaves the input uncertain, and that is never resent on its own.
    pub async fn send(&self, input: Input) -> Sent {
        self.inner.send(input).await
    }

    /// A prompt in this agent's kind.
    pub async fn send_prompt(&self, text: &str, attachments: Vec<wire::Attachment>) -> Sent {
        match inputs::prompt(self.kind(), text, attachments) {
            Some(input) => self.send(input).await,
            None => Sent {
                id: Vec::new(),
                outcome: InputOutcome::Reply(rejected(wire::Rejected {
                    refusal: wire::Refusal::Unsupported as i32,
                    detail: None,
                })),
            },
        }
    }

    /// Resumes an exited agent with no first message.
    pub async fn resume(&self) -> Result<Agent, RpcError> {
        let request = ResumeAgentRequest {
            agent_id: self.inner.agent_id.clone(),
            initial_prompt: None,
        };
        let agent = self.inner.client.resume_agent(request).await?;
        self.inner.apply(Msg::Entry(agent.clone()));
        Ok(agent)
    }

    /// The exited composer's one tap: the draft becomes the new
    /// incarnation's first prompt. On failure the draft is the composer's
    /// to keep.
    pub async fn resume_with(&self, mut draft: PromptSubmission) -> Result<Agent, RpcError> {
        if draft.input_id.is_empty() {
            draft.input_id = inputs::input_id();
        }
        let id = draft.input_id.clone();
        // The draft shows as a prompt on its way until the resume answers.
        if let Some(sending) = inputs::submitted(self.kind(), &draft) {
            self.inner.apply(Msg::Send(sending));
        }
        let request = ResumeAgentRequest {
            agent_id: self.inner.agent_id.clone(),
            initial_prompt: Some(draft),
        };
        match self.inner.client.resume_agent(request).await {
            Ok(agent) => {
                self.inner.apply(Msg::Entry(agent.clone()));
                let accepted = SendInputResponse {
                    of: Some(send_input_response::Of::Accepted(wire::Accepted {
                        queued: false,
                    })),
                };
                self.inner
                    .apply(Msg::Sent(id, InputOutcome::Reply(accepted)));
                Ok(agent)
            }
            Err(error) => {
                self.inner.apply(Msg::Discard(id));
                Err(error)
            }
        }
    }

    /// amux's rename of this agent. Never held: a host that cannot be
    /// reached fails it at once, with the reason.
    pub async fn rename(&self, name: &str) -> Result<Agent, RpcError> {
        let request = wire::RenameAgentRequest {
            agent_id: self.inner.agent_id.clone(),
            name: name.to_owned(),
        };
        let agent = self.inner.client.rename_agent(request).await?;
        self.inner.apply(Msg::Entry(agent.clone()));
        Ok(agent)
    }

    /// Takes a queued prompt back out of the queue.
    pub async fn withdraw(&self, queued: &[u8]) -> Result<(), InputError> {
        self.act(inputs::withdraw(self.kind(), queued)).await
    }

    /// Steers a queued prompt into the running turn; the entry reads
    /// steered until its reflection lands.
    pub async fn send_now(&self, queued: &[u8]) -> Result<(), InputError> {
        self.act(inputs::send_now(self.kind(), queued)).await
    }

    /// Answers an open ask with the input `ui_view::answer_input` made
    /// from its card and the person's choice; any open ask can be answered.
    pub async fn answer(&self, input: Input) -> Result<(), InputError> {
        self.act(Some(input)).await
    }

    pub async fn interrupt(&self) -> Result<(), InputError> {
        self.act(inputs::interrupt(self.kind())).await
    }

    /// Stops one background job by its key; the agent keeps running.
    pub async fn stop_job(&self, key: &str) -> Result<(), InputError> {
        self.act(inputs::stop_job(self.kind(), key)).await
    }

    /// Takes the agent's own view back to its conversation after it
    /// switched to another (terminal Claude).
    pub async fn switch_back(&self) -> Result<(), InputError> {
        self.act(inputs::switch_back(self.kind())).await
    }

    /// Forgets an input that was not confirmed.
    pub fn discard(&self, id: &[u8]) {
        self.inner.apply(Msg::Discard(id.to_vec()));
    }

    /// Sends a not-confirmed input again under a new id, forgetting the
    /// old one; None when this client sent no such input.
    pub async fn resend(&self, id: &[u8]) -> Option<Sent> {
        let mut input = self.state().inputs().get(id)?.input.clone();
        self.discard(id);
        input.input_id.clear();
        Some(self.send(input).await)
    }

    async fn act(&self, input: Option<Input>) -> Result<(), InputError> {
        let Some(input) = input else {
            return Err(InputError::Rejected(Refusal::of(
                wire::Refusal::Unsupported,
            )));
        };
        let sent = self.send(input).await;
        if self.inner.model().state.input_state(&sent.id) == Some(InputState::Held) {
            return Err(InputError::Held);
        }
        match sent.outcome {
            InputOutcome::Reply(SendInputResponse {
                of: Some(send_input_response::Of::Accepted(_)),
            }) => Ok(()),
            InputOutcome::Reply(SendInputResponse {
                of: Some(send_input_response::Of::Rejected(rejected)),
            }) => Err(InputError::Rejected(ui_state::refusal(&rejected))),
            InputOutcome::Reply(SendInputResponse { of: None }) | InputOutcome::Lost => {
                Err(InputError::Uncertain)
            }
            InputOutcome::Held => Err(InputError::Held),
        }
    }

    fn kind(&self) -> wire::Kind {
        self.inner.model().state.kind()
    }

    /// An attachment's bytes, if fetched. A missing one starts its fetch,
    /// and the rows that show it change when it lands; until then they
    /// draw a placeholder from the reference.
    pub fn blob(&self, hash: &[u8]) -> Option<Arc<[u8]>> {
        let status = {
            let model = self.inner.model();
            if let Some(bytes) = model.blobs.get(hash) {
                return Some(bytes.clone());
            }
            model.state.blob(hash)
        };
        if status == BlobStatus::Missing {
            self.fetch_blob(hash);
        }
        None
    }

    /// Fetches an attachment's bytes again, as after a failure.
    pub fn fetch_blob(&self, hash: &[u8]) {
        if self.inner.closed.load(Ordering::Acquire) {
            return;
        }
        tokio::spawn(self.inner.clone().fetch_blob(hash.to_vec()));
    }

    /// The files changed against `base`, without a patch, for an overview.
    pub async fn changed_files(&self, base: wire::DiffBase) -> Result<wire::Diff, RpcError> {
        crate::review::changed_files(self.inner.client.as_ref(), &self.inner.agent_id, base).await
    }

    /// The agent's diff against `base` and its patch, for a review page.
    pub async fn review(&self, base: wire::DiffBase) -> Result<(wire::Diff, String), RpcError> {
        crate::review::review(self.inner.client.as_ref(), &self.inner.agent_id, base).await
    }

    /// Stores bytes to attach to a prompt.
    pub async fn put_blob(
        &self,
        name: &str,
        mime: &str,
        bytes: Vec<u8>,
    ) -> Result<BlobRef, RpcError> {
        let request = PutBlobRequest {
            agent_id: self.inner.agent_id.clone(),
            name: name.to_owned(),
            mime: mime.to_owned(),
            bytes,
        };
        self.inner.client.put_blob(request).await
    }

    /// This session as a dump carries it, for a dump this client asks for:
    /// the state its trace began from and every event since. The daemon
    /// redacts it through the agent's kind before it writes it.
    pub fn history(&self) -> ClientSessionHistory {
        self.inner.history()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.inner.closed.store(true, Ordering::Release);
        if let Some(pump) = self.pump().take() {
            pump.abort();
        }
    }
}

impl DriverTrace<SessionState, Msg> {
    /// The state the trace ends in, rebuilt from its start and messages.
    pub fn replay(&self) -> SessionState {
        let mut state = self.start.clone();
        for msg in self.msgs() {
            state.update(msg.clone());
        }
        state
    }

    /// The trace as a dump carries it: the start and each message in wire
    /// form, the driver's own acts as it describes them.
    pub fn history(&self, agent_id: &[u8]) -> ClientSessionHistory {
        let traced = self
            .events
            .iter()
            .map(|traced| ClientTraced {
                seq: traced.seq,
                at_ms: traced.at_ms,
                event: Some(match &traced.event {
                    TraceEvent::Msg(msg) => client_traced::Event::Msg(msg.to_wire()),
                    TraceEvent::Driver(driver) => {
                        client_traced::Event::Driver(format!("{driver:?}"))
                    }
                }),
            })
            .collect();
        ClientSessionHistory {
            agent_id: agent_id.to_vec(),
            start: Some(self.start.to_wire()),
            events: Some(ClientSessionEvents { traced }),
        }
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Reads one stream until it ends or the model asks for a reload.
async fn read(
    inner: &Arc<Inner>,
    stream: &mut EventStream<SessionEvent>,
    backoff: &mut Backoff,
) -> End {
    loop {
        let next = tokio::select! {
            next = stream.next() => next,
            () = inner.reload.notified() => return End::Reload,
        };
        match next {
            Some(Ok(event)) => {
                let lagged = matches!(event.of, Some(session_event::Of::Lagged(_)));
                let caught_up = matches!(event.of, Some(session_event::Of::CaughtUp(_)));
                inner.event(event).await;
                if lagged {
                    return End::Lagged;
                }
                if caught_up {
                    backoff.reset();
                }
            }
            Some(Err(error)) => return End::Closed(Some(error)),
            None => return End::Closed(None),
        }
    }
}

/// The pump: Lagged reopens with a tail at once; the end of the stream
/// reconnects with backoff on the session's clock and re-tails. Uncertain
/// inputs settle at the next CaughtUp from the queue or the items, and the
/// rest stay for the person; nothing is resent.
async fn pump(inner: Arc<Inner>, stream: Option<EventStream<SessionEvent>>, ended: Option<End>) {
    let mut backoff = Backoff::default();
    let mut stream = stream;
    let mut ended = ended;
    loop {
        let end = match ended.take() {
            Some(end) => end,
            None => match stream.as_mut() {
                Some(stream) => read(&inner, stream, &mut backoff).await,
                None => End::Closed(None),
            },
        };
        stream = None;
        let mut wait = match end {
            End::Lagged => {
                inner.note(DriverEvent::Retail);
                false
            }
            End::Resumed => false,
            End::Reload => {
                inner.note(DriverEvent::Reload);
                inner.apply(Msg::Reloading);
                false
            }
            End::Closed(error) => {
                inner.note(DriverEvent::StreamEnded {
                    error: error.map(|error| error.to_string()),
                });
                inner.apply(Msg::Connection(Connection::Reconnecting));
                true
            }
        };
        while stream.is_none() {
            if wait {
                let until_ms = inner.clock.now_ms() + backoff.next_ms();
                inner.note(DriverEvent::Backoff { until_ms });
                inner.clock.sleep_until(until_ms).await;
            }
            match inner
                .client
                .subscribe_session(inner.subscribe_request())
                .await
            {
                Ok(opened) => {
                    inner.note(DriverEvent::Subscribed {
                        tail: inner.tail.load(Ordering::Acquire),
                    });
                    inner.apply(Msg::Connection(Connection::Live));
                    stream = Some(opened);
                }
                Err(RpcError::Transport(error)) => {
                    inner.note(DriverEvent::SubscribeFailed { error });
                    if !wait {
                        inner.apply(Msg::Connection(Connection::Reconnecting));
                    }
                    wait = true;
                }
                Err(error) => {
                    inner.note(DriverEvent::Ended {
                        error: error.to_string(),
                    });
                    inner.model().ended = Some(error);
                    inner.changed.send_replace(());
                    return;
                }
            }
        }
    }
}
