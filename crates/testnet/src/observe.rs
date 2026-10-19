//! Typed observations and the waiters built on them.
//!
//! An observer records a stream exactly as a client receives it and waits
//! for a predicate over everything seen so far. Waiting has three outcomes
//! and only one of them passes: the predicate held, the deadline passed
//! with it unmet ([`Stuck::Deadline`]), or the stream ended with it unmet
//! ([`Stuck::Closed`]), reported at once rather than at the deadline. The
//! polling waiters beside it, and the plain ones re-exported from the
//! `patience` crate, hold to the same rule: a check that never answers is
//! a failure, never a pass.

use std::fmt::{self, Write as _};
use std::time::Duration;

pub use patience::{PATIENCE, Stuck, holds_for, until, until_within};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use wire::{InventoryEvent, SessionEvent, inventory_event, session_event};

/// One line per event, the way a failure report and a transcript show it.
pub trait Describe {
    fn describe(&self) -> String;
}

/// A stream's events as a client saw them, in order.
pub struct ObserverOf<E> {
    what: String,
    events: Vec<E>,
    source: Source<E>,
    closed: bool,
}

/// Where an observer's events come from.
enum Source<E> {
    /// A real net's stream, which a reader task forwards as it arrives.
    Stream {
        incoming: mpsc::UnboundedReceiver<E>,
        reader: JoinHandle<()>,
    },
    /// A simulated net's stream, whose world moves only when a wait asks.
    Simulated(Box<dyn Feed<E>>),
}

/// A simulated stream: what it carried, and a world that moves one
/// delivery at a time when a wait asks it to.
pub(crate) trait Feed<E>: Send {
    /// What the stream carried since the last call and whether it ended;
    /// with `step`, the world first moves one delivery on, or says that
    /// nothing more can happen without the story acting.
    fn pump(&mut self, step: bool) -> Pumped<E>;
}

pub(crate) struct Pumped<E> {
    pub(crate) events: Vec<E>,
    pub(crate) closed: bool,
    /// Nothing moved: no delivery was waiting.
    pub(crate) quiet: bool,
    /// The simulated net failed, in its own words: the stream ends with it.
    pub(crate) failure: Option<String>,
}

/// A Subscribe stream on one agent.
pub type Observer = ObserverOf<SessionEvent>;
/// A SubscribeInventory stream on one host.
pub type InventoryObserver = ObserverOf<InventoryEvent>;

impl<E: Describe + Clone + Send + 'static> ObserverOf<E> {
    /// An observer over events a reader task sends into `incoming`.
    pub(crate) fn new(
        what: String,
        incoming: mpsc::UnboundedReceiver<E>,
        reader: JoinHandle<()>,
    ) -> Self {
        Self {
            what,
            events: Vec::new(),
            source: Source::Stream { incoming, reader },
            closed: false,
        }
    }

    /// An observer over a simulated stream.
    pub(crate) fn simulated(what: String, feed: Box<dyn Feed<E>>) -> Self {
        Self {
            what,
            events: Vec::new(),
            source: Source::Simulated(feed),
            closed: false,
        }
    }

    /// Takes everything already delivered, without waiting or moving a
    /// simulated world.
    fn take_delivered(&mut self) {
        match &mut self.source {
            Source::Stream { incoming, .. } => {
                while let Ok(event) = incoming.try_recv() {
                    self.events.push(event);
                }
            }
            Source::Simulated(feed) => {
                let pumped = feed.pump(false);
                self.took(pumped);
            }
        }
    }

    /// Records what a simulated stream carried; a net that failed ends
    /// the stream, and says why.
    fn took(&mut self, pumped: Pumped<E>) {
        self.events.extend(pumped.events);
        self.closed |= pumped.closed;
        if let Some(failure) = pumped.failure
            && !self.closed
        {
            self.closed = true;
            self.what = format!("{} (the simulated net failed: {failure})", self.what);
        }
    }

    /// Waits until `pred` holds over everything seen, and returns it.
    pub async fn observe_until(
        &mut self,
        pred: impl Fn(&[E]) -> bool,
        deadline: Duration,
    ) -> Result<&[E], Stuck> {
        let until = Instant::now() + deadline;
        loop {
            // Take everything already delivered before judging, so the
            // predicate sees the stream as far as it has come.
            self.take_delivered();
            if pred(&self.events) {
                return Ok(&self.events);
            }
            if self.closed {
                return Err(Stuck::Closed {
                    what: self.what.clone(),
                    seen: self.transcript(),
                });
            }
            let stuck = match &mut self.source {
                Source::Stream { incoming, .. } => {
                    match tokio::time::timeout_at(until, incoming.recv()).await {
                        Ok(Some(event)) => {
                            self.events.push(event);
                            false
                        }
                        Ok(None) => {
                            self.closed = true;
                            false
                        }
                        Err(_) => true,
                    }
                }
                // A simulated world that has nothing left to deliver has
                // reached what the deadline stands for: nothing more comes
                // unless the story acts.
                Source::Simulated(feed) => {
                    let pumped = feed.pump(true);
                    let moved = !pumped.quiet
                        || !pumped.events.is_empty()
                        || pumped.closed
                        || pumped.failure.is_some();
                    self.took(pumped);
                    !moved
                }
            };
            if stuck {
                return Err(Stuck::Deadline {
                    what: self.what.clone(),
                    waited: deadline,
                    seen: self.transcript(),
                });
            }
        }
    }

    /// Waits until the stream ends, and returns everything it carried.
    pub async fn until_closed(&mut self, deadline: Duration) -> Result<&[E], Stuck> {
        match self.observe_until(|_| false, deadline).await {
            Err(Stuck::Closed { .. }) => Ok(&self.events),
            Err(stuck) => Err(stuck),
            Ok(_) => unreachable!("nothing satisfies a false predicate"),
        }
    }

    /// Everything seen so far, without waiting.
    pub fn events(&mut self) -> &[E] {
        self.take_delivered();
        &self.events
    }

    /// Whether the stream has ended, as far as the observer has read.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// The events seen so far, one described line each.
    pub fn transcript(&self) -> String {
        let mut out = String::new();
        for event in &self.events {
            let _ = writeln!(out, "  {}", event.describe());
        }
        out
    }
}

impl<E> Drop for ObserverOf<E> {
    fn drop(&mut self) {
        // A simulated stream lets its call go as its feed drops.
        if let Source::Stream { reader, .. } = &self.source {
            reader.abort();
        }
    }
}

impl<E> fmt::Debug for ObserverOf<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Observer")
            .field("what", &self.what)
            .field("seen", &self.events.len())
            .field("closed", &self.closed)
            .finish()
    }
}

/// What a session event is, without its payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    Opening(u64),
    Snapshot,
    Item(String),
    Append(String),
    CaughtUp(u64),
    Lagged,
    Reset,
    Detached,
    Empty,
}

pub fn mark(event: &SessionEvent) -> Mark {
    match &event.of {
        Some(session_event::Of::Opening(opening)) => Mark::Opening(opening.generation),
        Some(session_event::Of::Snapshot(_)) => Mark::Snapshot,
        Some(session_event::Of::Item(item)) => Mark::Item(item.key.clone()),
        Some(session_event::Of::Append(append)) => Mark::Append(append.key.clone()),
        Some(session_event::Of::CaughtUp(caught_up)) => Mark::CaughtUp(caught_up.revision),
        Some(session_event::Of::Lagged(_)) => Mark::Lagged,
        Some(session_event::Of::Reset(_)) => Mark::Reset,
        Some(session_event::Of::Detached(_)) => Mark::Detached,
        None => Mark::Empty,
    }
}

/// The session events' marks in order.
pub fn marks(events: &[SessionEvent]) -> Vec<Mark> {
    events.iter().map(mark).collect()
}

/// Whether a CaughtUp has been seen.
pub fn caught_up(events: &[SessionEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event.of, Some(session_event::Of::CaughtUp(_))))
}

/// Whether an inventory stream has reached its CaughtUp.
pub fn inventory_caught_up(events: &[InventoryEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event.of, Some(inventory_event::Of::CaughtUp(_))))
}

/// The newest state of every agent row an inventory stream has named,
/// without the ones it removed since.
pub fn inventory_agents(events: &[InventoryEvent]) -> Vec<wire::Agent> {
    let mut agents: Vec<wire::Agent> = Vec::new();
    for event in events {
        match &event.of {
            Some(inventory_event::Of::Agent(agent)) => {
                match agents
                    .iter_mut()
                    .find(|held| held.agent_id == agent.agent_id && held.host_id == agent.host_id)
                {
                    Some(held) => *held = agent.clone(),
                    None => agents.push(agent.clone()),
                }
            }
            Some(inventory_event::Of::AgentRemoved(removed)) => {
                agents.retain(|held| held.agent_id != removed.agent_id);
            }
            _ => {}
        }
    }
    agents
}

/// The newest entry of every host an inventory stream has named, without
/// the ones it removed since.
pub fn inventory_hosts(events: &[InventoryEvent]) -> Vec<wire::HostEntry> {
    let mut hosts: Vec<wire::HostEntry> = Vec::new();
    for event in events {
        match &event.of {
            Some(inventory_event::Of::Host(host)) => {
                match hosts.iter_mut().find(|held| held.host_id == host.host_id) {
                    Some(held) => *held = host.clone(),
                    None => hosts.push(host.clone()),
                }
            }
            Some(inventory_event::Of::HostRemoved(removed)) => {
                hosts.retain(|held| held.host_id != removed.host_id);
            }
            _ => {}
        }
    }
    hosts
}

/// Whether a host's inventory entry says it is trusted and online now.
pub fn host_online(host: &wire::HostEntry) -> bool {
    host.trust() == wire::Trust::Trusted && host.presence() == wire::Presence::Online
}

impl Describe for SessionEvent {
    fn describe(&self) -> String {
        match &self.of {
            Some(session_event::Of::Opening(opening)) => {
                format!("Opening generation={}", opening.generation)
            }
            Some(session_event::Of::Snapshot(snapshot)) => format!(
                "Snapshot revision={} kind={} phase={:?} queue={}",
                snapshot.revision,
                snapshot.kind,
                wire::Phase::try_from(snapshot.phase).unwrap_or(wire::Phase::Starting),
                snapshot.queue.len()
            ),
            Some(session_event::Of::Item(item)) => format!(
                "Item {} order={} revision={} kind={} text={:?}",
                item.key,
                item.order,
                item.revision,
                item.kind,
                clip(&item.text)
            ),
            Some(session_event::Of::Append(append)) => format!(
                "Append {} base={} revision={} text={:?}",
                append.key,
                append.base_revision,
                append.revision,
                clip(&append.text)
            ),
            Some(session_event::Of::CaughtUp(caught_up)) => {
                format!("CaughtUp revision={}", caught_up.revision)
            }
            Some(session_event::Of::Lagged(_)) => "Lagged".to_owned(),
            Some(session_event::Of::Reset(_)) => "Reset".to_owned(),
            Some(session_event::Of::Detached(_)) => "Detached".to_owned(),
            None => "(empty event)".to_owned(),
        }
    }
}

impl Describe for InventoryEvent {
    fn describe(&self) -> String {
        match &self.of {
            Some(inventory_event::Of::Host(host)) => format!(
                "Host {} {} generation={} trust={:?} presence={:?}",
                host.name,
                short(&host.host_id),
                host.generation,
                wire::Trust::try_from(host.trust).unwrap_or_default(),
                wire::Presence::try_from(host.presence).unwrap_or_default(),
            ),
            Some(inventory_event::Of::HostRemoved(removed)) => {
                format!("HostRemoved {}", short(&removed.host_id))
            }
            Some(inventory_event::Of::Agent(agent)) => format!(
                "Agent {} {} on {} lifecycle={:?} phase={:?}",
                agent.name,
                short(&agent.agent_id),
                short(&agent.host_id),
                wire::Lifecycle::try_from(agent.lifecycle).unwrap_or_default(),
                wire::Phase::try_from(agent.phase).unwrap_or(wire::Phase::Starting),
            ),
            Some(inventory_event::Of::AgentRemoved(removed)) => {
                format!("AgentRemoved {}", short(&removed.agent_id))
            }
            Some(inventory_event::Of::CaughtUp(_)) => "CaughtUp".to_owned(),
            Some(inventory_event::Of::Lagged(_)) => "Lagged".to_owned(),
            None => "(empty event)".to_owned(),
        }
    }
}

fn clip(text: &str) -> String {
    const MAX: usize = 60;
    match text.char_indices().nth(MAX) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

/// The first eight hex digits of an id, as failure reports show it.
pub fn short(id: &[u8]) -> String {
    id.iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
