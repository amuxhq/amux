//! The fleet driver: the inventory stream into [`FleetState`], with the
//! same reconnect and trace as a session, one level up, and the one
//! [`Session`] each live agent has.
//!
//! Home and the chat read the same session, so an agent is subscribed to
//! once however many places show it. Off screen a session holds a few rows,
//! enough for its fleet row; a chat widens it, paging older rows in from the
//! local runtime, and narrows it again when it closes. An exited agent has
//! no session until a chat opens it, and loses it when the chat closes.

use std::collections::{BTreeSet, HashMap};
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use client::{Client, Clock, EventStream, RpcError};
use futures_util::StreamExt as _;
use tokio::sync::{OnceCell, watch};
use tokio::task::JoinHandle;
use ui_state::{AgentKey, Connection, FleetMsg, FleetState};
use wire::{
    Agent, DumpFile, DumpPart, ErrorCode, InventoryEvent, Lifecycle, StopAgentRequest,
    inventory_event,
};

use crate::Backoff;
use crate::session::{Catalogues, Session};
use crate::trace::{DriverEvent, DriverTrace, Ring, Structure as _, TraceEvent};

/// The rows a session holds while it is off screen: enough for the fleet
/// row's second line, which reads what the agent last said or is running.
pub const HELD_ROWS: u32 = 16;

/// A chat's window: the rows it opens with and the most it keeps while the
/// reader follows the newest (never fewer than the tail).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub tail: u32,
    pub cap: u32,
}

/// What became of a person's stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Stopped,
    /// The agent's host could not be reached, so it never left: the fleet
    /// holds it, the row shows it with Cancel, and it goes when the host is
    /// back. Closing the client drops it.
    Held,
}

/// Whether a lifecycle call's error says it never reached the agent's
/// host: the daemon could not reach it and wrote nothing.
fn never_left(error: &RpcError) -> bool {
    error.code() == Some(ErrorCode::Unavailable)
        || error.reason() == Some(wire::ErrorReason::HostUnreachable)
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("no agent {0:?} in the fleet")]
    NoAgent(AgentKey),
    #[error(transparent)]
    Rpc(#[from] RpcError),
}

/// One agent's session, or the open on its way, and how many chats show it.
struct Slot {
    session: Arc<OnceCell<Arc<Session>>>,
    viewers: usize,
    /// The fleet's own open of an off-screen session, stopped with the slot.
    opening: Option<JoinHandle<()>>,
}

impl Slot {
    fn new() -> Slot {
        Slot {
            session: Arc::new(OnceCell::new()),
            viewers: 0,
            opening: None,
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(opening) = self.opening.take() {
            opening.abort();
        }
    }
}

struct Model {
    state: FleetState,
    trace: Ring<FleetState, FleetMsg>,
    changed: BTreeSet<AgentKey>,
    /// A host was listed, changed or removed since the last take.
    hosts_changed: bool,
    ended: Option<RpcError>,
}

struct Inner {
    client: Arc<dyn Client>,
    clock: Arc<dyn Clock>,
    model: Mutex<Model>,
    changed: watch::Sender<()>,
    closed: AtomicBool,
    /// Never locked while the model is, nor the model while this is.
    sessions: Mutex<HashMap<AgentKey, Slot>>,
    foreground: AtomicBool,
    /// Shared by every session, so a catalogue is fetched once per hash.
    catalogues: Arc<Catalogues>,
    me: Weak<Inner>,
}

impl Inner {
    fn model(&self) -> MutexGuard<'_, Model> {
        self.model
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Stops the agent; a stop that never left, its host out of reach, is
    /// held and goes again when the host is back.
    async fn stop(self: &Arc<Self>, agent: AgentKey) -> Result<Stop, RpcError> {
        let request = StopAgentRequest {
            agent_id: agent.agent.clone(),
        };
        let listing = self.model().state.listings();
        match self.client.stop_agent(request).await {
            Ok(()) => {
                self.apply(FleetMsg::DropStop(agent));
                Ok(Stop::Stopped)
            }
            Err(error) if never_left(&error) => {
                self.apply(FleetMsg::HoldStop { agent, listing });
                Ok(Stop::Held)
            }
            Err(error) => {
                self.apply(FleetMsg::DropStop(agent));
                Err(error)
            }
        }
    }

    fn sessions(&self) -> MutexGuard<'_, HashMap<AgentKey, Slot>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn apply(&self, msg: FleetMsg) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let at_ms = self.clock.now_ms();
        let mut model = self.model();
        let Model { state, trace, .. } = &mut *model;
        trace.record(state, at_ms, TraceEvent::Msg(msg.clone()));
        let connection = matches!(msg, FleetMsg::Connection(_));
        let host = matches!(
            &msg,
            FleetMsg::Event(event) if matches!(
                event.of,
                Some(inventory_event::Of::Host(_) | inventory_event::Of::HostRemoved(_))
            )
        );
        let keys = state.update(msg);
        let released = state.take_released_stops();
        let moved = connection || host || !keys.is_empty();
        model.changed.extend(keys.iter().cloned());
        model.hosts_changed |= host;
        drop(model);
        if !keys.is_empty() || host {
            self.tend(&keys);
        }
        if moved {
            self.changed.send_replace(());
        }
        if !released.is_empty()
            && let Some(me) = self.me.upgrade()
        {
            for agent in released {
                let me = me.clone();
                tokio::spawn(async move {
                    // Held again if the host went before it landed; any
                    // other failure is the stop's own, and it is not held.
                    let _ = me.stop(agent).await;
                });
            }
        }
    }

    /// Starts a session for each live agent among `keys` that has none,
    /// drops the unviewed sessions of agents that exited or left, and feeds
    /// every session its entry and host where they moved.
    fn tend(&self, keys: &[AgentKey]) {
        let entries: Vec<(AgentKey, Option<Agent>)> = {
            let model = self.model();
            keys.iter()
                .map(|key| (key.clone(), model.state.agent(key).cloned()))
                .collect()
        };
        let foreground = self.foreground.load(Ordering::Acquire);
        let mut start = Vec::new();
        {
            let mut sessions = self.sessions();
            for (key, entry) in entries {
                match entry {
                    Some(entry) if entry.lifecycle() == Lifecycle::Live => {
                        if foreground && !sessions.contains_key(&key) {
                            let slot = Slot::new();
                            start.push((key.clone(), slot.session.clone(), entry));
                            sessions.insert(key, slot);
                        }
                    }
                    _ => {
                        if sessions.get(&key).is_some_and(|slot| slot.viewers == 0) {
                            sessions.remove(&key);
                        }
                    }
                }
            }
        }
        for (key, cell, entry) in start {
            let Some(inner) = self.me.upgrade() else {
                return;
            };
            let opened = key.clone();
            let opening = tokio::spawn(async move {
                let made = cell
                    .get_or_try_init(|| inner.make(key.clone(), entry, HELD_ROWS, HELD_ROWS, false))
                    .await;
                if made.is_err() {
                    // The agent's next row, or a chat opening it, tries again.
                    let mut sessions = inner.sessions();
                    let same = sessions
                        .get(&key)
                        .is_some_and(|slot| slot.viewers == 0 && Arc::ptr_eq(&slot.session, &cell));
                    if same {
                        sessions.remove(&key);
                    }
                }
            });
            if let Some(slot) = self.sessions().get_mut(&opened) {
                slot.opening = Some(opening);
            }
        }
        self.feed();
    }

    /// Opens an agent's session; off screen it gathers no changes and the
    /// chat that opens it sets its window.
    async fn make(
        &self,
        key: AgentKey,
        entry: Agent,
        tail: u32,
        cap: u32,
        on_screen: bool,
    ) -> Result<Arc<Session>, RpcError> {
        let host = self.model().state.host(&entry.host_id).cloned();
        let session = Session::start(
            self.client.clone(),
            entry,
            tail,
            cap,
            self.clock.clone(),
            on_screen,
            self.catalogues.clone(),
        )
        .await?;
        let session = Arc::new(session);
        if !on_screen {
            session.set_window(tail, cap, false);
        }
        if let Some(host) = host {
            session.set_host(host);
        }
        let fleet = self.me.clone();
        let woken = key.clone();
        session.on_home(Box::new(move || {
            if let Some(fleet) = fleet.upgrade() {
                fleet.model().changed.insert(woken.clone());
                fleet.changed.send_replace(());
            }
        }));
        // A chat opened out of the foreground (a notification bringing it
        // current) streams; the fleet's own sessions wait for the foreground.
        if !on_screen && !self.foreground.load(Ordering::Acquire) {
            session.set_foreground(false);
        }
        Ok(session)
    }

    /// Every session's entry and host, where the inventory moved them.
    fn feed(&self) {
        let held: Vec<(AgentKey, Arc<Session>)> = self
            .sessions()
            .iter()
            .filter_map(|(key, slot)| Some((key.clone(), slot.session.get()?.clone())))
            .collect();
        for (key, session) in held {
            let (entry, host) = {
                let model = self.model();
                let entry = model.state.agent(&key).cloned();
                let host = entry
                    .as_ref()
                    .and_then(|entry| model.state.host(&entry.host_id).cloned());
                (entry, host)
            };
            let (same_entry, same_host) = {
                let state = session.state();
                (
                    entry.as_ref().is_none_or(|entry| state.agent() == entry),
                    host.as_ref().is_none_or(|host| state.host() == Some(host)),
                )
            };
            if !same_entry && let Some(entry) = entry {
                session.set_entry(entry);
            }
            if !same_host && let Some(host) = host {
                session.set_host(host);
            }
        }
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

    fn event(&self, event: InventoryEvent) {
        let caught_up = matches!(
            event.of,
            Some(inventory_event::Of::CaughtUp(_) | inventory_event::Of::Lagged(_))
        );
        self.apply(FleetMsg::Event(Box::new(event)));
        if caught_up {
            // Being caught up, or no longer, is news even when no row moved.
            self.changed.send_replace(());
        }
    }
}

/// The fleet's state while the guard lives; never hold it across an await.
pub struct FleetGuard<'a>(MutexGuard<'a, Model>);

impl FleetGuard<'_> {
    /// The bounded trace, read under the same guard as the state it
    /// replays to.
    pub fn trace(&self) -> DriverTrace<FleetState, FleetMsg> {
        self.0.trace.trace()
    }
}

impl Deref for FleetGuard<'_> {
    type Target = FleetState;

    fn deref(&self) -> &FleetState {
        &self.0.state
    }
}

pub struct Fleet {
    inner: Arc<Inner>,
    pump: JoinHandle<()>,
}

impl Fleet {
    /// Subscribes to the inventory and resolves once it has caught up, or
    /// once its stream ended first, in which case the pump reconnects.
    pub async fn connect(client: Arc<dyn Client>, clock: impl Clock) -> Result<Fleet, RpcError> {
        let state = FleetState::new();
        let (changed, _) = watch::channel(());
        let inner = Arc::new_cyclic(|me| Inner {
            client,
            clock: Arc::new(clock),
            model: Mutex::new(Model {
                trace: Ring::new(state.clone()),
                state,
                changed: BTreeSet::new(),
                hosts_changed: false,
                ended: None,
            }),
            changed,
            closed: AtomicBool::new(false),
            sessions: Mutex::new(HashMap::new()),
            foreground: AtomicBool::new(true),
            catalogues: Arc::default(),
            me: me.clone(),
        });
        let mut stream = inner.client.subscribe_inventory().await?;
        inner.note(DriverEvent::Subscribed { tail: 0 });
        inner.apply(FleetMsg::Connection(Connection::Live));
        let mut ended = None;
        loop {
            match stream.next().await {
                Some(Ok(event)) => {
                    let caught_up = matches!(event.of, Some(inventory_event::Of::CaughtUp(_)));
                    let lagged = matches!(event.of, Some(inventory_event::Of::Lagged(_)));
                    inner.event(event);
                    if lagged {
                        ended = Some(End::Lagged);
                        break;
                    }
                    if caught_up {
                        break;
                    }
                }
                Some(Err(error)) => {
                    ended = Some(End::Closed(Some(error)));
                    break;
                }
                None => {
                    ended = Some(End::Closed(None));
                    break;
                }
            }
        }
        let pump = tokio::spawn(pump(inner.clone(), stream, ended));
        Ok(Fleet { inner, pump })
    }

    pub fn state(&self) -> FleetGuard<'_> {
        FleetGuard(self.inner.model())
    }

    /// A person's stop of the agent. One whose host cannot be reached
    /// never left: it is held, shown on the agent's row with Cancel, and
    /// goes when the host is back. Rename and delete are never held.
    pub async fn stop(&self, agent: &AgentKey) -> Result<Stop, RpcError> {
        self.inner.stop(agent.clone()).await
    }

    /// Takes back a held stop before it goes.
    pub fn cancel_stop(&self, agent: &AgentKey) {
        self.inner.apply(FleetMsg::DropStop(agent.clone()));
    }

    /// The one session for a live agent, held with a small window while no
    /// chat shows it. None for an exited agent no chat has open, and while
    /// a live agent's session is still opening.
    pub fn session(&self, agent: &AgentKey) -> Option<Arc<Session>> {
        self.inner
            .sessions()
            .get(agent)
            .and_then(|slot| slot.session.get().cloned())
    }

    /// Every session the fleet holds, for a dump.
    pub fn sessions(&self) -> Vec<Arc<Session>> {
        self.inner
            .sessions()
            .values()
            .filter_map(|slot| slot.session.get().cloned())
            .collect()
    }

    /// The same session, widened for a chat: older rows page in from the
    /// local runtime up to the window's tail. An exited agent's session
    /// opens here. Every open is paired with a [`Fleet::close`].
    pub async fn open(&self, agent: &AgentKey, window: Window) -> Result<Arc<Session>, OpenError> {
        let entry = self
            .state()
            .agent(agent)
            .cloned()
            .ok_or_else(|| OpenError::NoAgent(agent.clone()))?;
        let cell = {
            let mut sessions = self.inner.sessions();
            let slot = sessions.entry(agent.clone()).or_insert_with(Slot::new);
            slot.viewers += 1;
            slot.session.clone()
        };
        let made = cell
            .get_or_try_init(|| {
                self.inner
                    .make(agent.clone(), entry, window.tail, window.cap, true)
            })
            .await;
        let session = match made {
            Ok(session) => session.clone(),
            Err(error) => {
                self.close(agent);
                return Err(error.into());
            }
        };
        session.set_window(window.tail, window.cap, true);
        let (held, older) = {
            let state = session.state();
            (state.transcript().len(), state.transcript().has_older())
        };
        let wanted = (window.tail as usize).saturating_sub(held);
        if older && wanted > 0 {
            // A failed page leaves the chat's own paging to say why.
            let _ = session
                .page_older(u32::try_from(wanted).unwrap_or(u32::MAX))
                .await;
        }
        if !self.inner.foreground.load(Ordering::Acquire) {
            session.set_foreground(true);
        }
        Ok(session)
    }

    /// A chat on the agent closed: the last one to close narrows its
    /// session to the fleet's few rows, or drops it if the agent is not
    /// live.
    pub fn close(&self, agent: &AgentKey) {
        let live = self
            .state()
            .agent(agent)
            .is_some_and(|entry| entry.lifecycle() == Lifecycle::Live);
        let narrow = {
            let mut sessions = self.inner.sessions();
            let Some(slot) = sessions.get_mut(agent) else {
                return;
            };
            slot.viewers = slot.viewers.saturating_sub(1);
            if slot.viewers > 0 {
                return;
            }
            if !live {
                sessions.remove(agent);
                return;
            }
            slot.session.get().cloned()
        };
        if let Some(session) = narrow {
            session.set_window(HELD_ROWS, HELD_ROWS, false);
            if !self.inner.foreground.load(Ordering::Acquire) {
                session.set_foreground(false);
            }
        }
    }

    /// The phone leaving (false) or returning to (true) the foreground:
    /// drops every session's stream, or reopens each with a tail and starts
    /// the sessions of agents that went live meanwhile. Out of the
    /// foreground a chat that opens streams until it closes.
    pub fn set_foreground(&self, foreground: bool) {
        let was = self.inner.foreground.swap(foreground, Ordering::AcqRel);
        if was == foreground {
            return;
        }
        let held: Vec<Arc<Session>> = self
            .inner
            .sessions()
            .values()
            .filter_map(|slot| slot.session.get().cloned())
            .collect();
        for session in held {
            session.set_foreground(foreground);
        }
        if foreground {
            let keys: Vec<AgentKey> = self.state().agents().map(ui_state::agent_key).collect();
            self.inner.tend(&keys);
        }
    }

    /// Level-triggered, as a session's.
    pub fn changed(&self) -> watch::Receiver<()> {
        self.inner.changed.subscribe()
    }

    /// The agents whose card or family attention may have changed since the
    /// last call.
    pub fn take_changed(&self) -> Vec<AgentKey> {
        std::mem::take(&mut self.inner.model().changed)
            .into_iter()
            .collect()
    }

    /// Whether a host was listed, changed or removed since the last call.
    pub fn take_hosts_changed(&self) -> bool {
        std::mem::take(&mut self.inner.model().hosts_changed)
    }

    /// Set when the runtime refused to serve the inventory again.
    pub fn ended(&self) -> Option<RpcError> {
        self.inner.model().ended.clone()
    }

    /// The fleet's part of a dump bundle: the structure of its state and
    /// its trace, with no names, paths or status text. Each session writes
    /// its own.
    pub fn dump_part(&self) -> DumpPart {
        let (state, trace) = {
            let model = self.inner.model();
            (model.state.structure(), model.trace.trace())
        };
        DumpPart {
            dump_id: Vec::new(),
            files: vec![
                DumpFile {
                    name: "client/fleet/state.txt".into(),
                    contents: state.into_bytes(),
                },
                DumpFile {
                    name: "client/fleet/trace.txt".into(),
                    contents: trace.render().into_bytes(),
                },
            ],
        }
    }
}

impl Drop for Fleet {
    fn drop(&mut self) {
        self.inner.closed.store(true, Ordering::Release);
        self.pump.abort();
        self.inner.sessions().clear();
    }
}

impl DriverTrace<FleetState, FleetMsg> {
    pub fn replay(&self) -> FleetState {
        let mut state = self.start.clone();
        for msg in self.msgs() {
            state.update(msg.clone());
        }
        state
    }
}

/// How one inventory stream ended.
enum End {
    /// The runtime fell behind and said so before closing: resubscribe at
    /// once, since nothing is wrong with the connection.
    Lagged,
    /// The stream closed or the transport failed: reconnect with backoff.
    Closed(Option<RpcError>),
}

/// Reads one inventory stream until it ends.
async fn read(
    inner: &Inner,
    stream: &mut EventStream<InventoryEvent>,
    backoff: &mut Backoff,
) -> End {
    loop {
        match stream.next().await {
            Some(Ok(event)) => {
                let lagged = matches!(event.of, Some(inventory_event::Of::Lagged(_)));
                if matches!(event.of, Some(inventory_event::Of::CaughtUp(_))) {
                    backoff.reset();
                }
                inner.event(event);
                if lagged {
                    return End::Lagged;
                }
            }
            Some(Err(error)) => return End::Closed(Some(error)),
            None => return End::Closed(None),
        }
    }
}

/// Reads the inventory until it ends. After Lagged it resubscribes at
/// once; after any other end it reconnects with backoff. Either way the
/// state re-lists and drops what the new stream no longer names at its
/// CaughtUp.
async fn pump(inner: Arc<Inner>, stream: EventStream<InventoryEvent>, ended: Option<End>) {
    let mut backoff = Backoff::default();
    let mut stream = Some(stream);
    let mut ended = ended;
    loop {
        let end = match ended.take() {
            Some(end) => end,
            None => {
                let Some(open) = stream.as_mut() else { return };
                read(&inner, open, &mut backoff).await
            }
        };
        stream = None;
        let mut wait = match end {
            End::Lagged => {
                inner.note(DriverEvent::Retail);
                false
            }
            End::Closed(error) => {
                inner.note(DriverEvent::StreamEnded {
                    error: error.map(|error| error.to_string()),
                });
                inner.apply(FleetMsg::Connection(Connection::Reconnecting));
                true
            }
        };
        while stream.is_none() {
            if wait {
                let until_ms = inner.clock.now_ms() + backoff.next_ms();
                inner.note(DriverEvent::Backoff { until_ms });
                inner.clock.sleep_until(until_ms).await;
            }
            match inner.client.subscribe_inventory().await {
                Ok(opened) => {
                    inner.note(DriverEvent::Subscribed { tail: 0 });
                    inner.apply(FleetMsg::Connection(Connection::Live));
                    stream = Some(opened);
                }
                Err(RpcError::Transport(error)) => {
                    inner.note(DriverEvent::SubscribeFailed { error });
                    if !wait {
                        inner.apply(FleetMsg::Connection(Connection::Reconnecting));
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
