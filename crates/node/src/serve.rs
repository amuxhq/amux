//! Serving: what every client call reads. Subscribe, Fetch and Get read an
//! agent's rows; SubscribeInventory and ResolveAgent read the fleet. All of
//! them are store reads plus, for the streams, the fan-out channels; none
//! touches an agent process.

use std::collections::VecDeque;
use std::sync::Arc;

use prost::{Message as _, Name as _};
use store::{AgentKey, AgentRow, Marker, PageEnd, Sqlite, Store as _, StoreError};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::broadcast::{self};
use wire::{
    Agent, AmbiguousAgentName, CaughtUp, ErrorCode, ErrorDetail, FetchRequest, FetchResponse,
    GetRequest, InventoryEvent, Item, Lagged, SessionEvent, Snapshot, inventory_event,
    session_event,
};

use crate::runtime::{ProfileRuntime, to_wire};

/// The most items one Fetch returns, whatever the limit asked for.
pub const MAX_PAGE: u32 = 500;
/// The most rows a peer's catch-up may ask for.
const MAX_CAP: u32 = 10_000;

/// Where a Subscribe starts.
enum Opening {
    /// A client's: the newest rows.
    Tail(u32),
    /// A peer source's: what came after the revision it holds, taken
    /// under `generation` of this origin.
    After {
        revision: u64,
        cap: u32,
        generation: u64,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("no agent with that id")]
    NoAgent,
    #[error("no agent named {0}")]
    NoAgentNamed(String),
    #[error("{} agents are named {}", .0.candidates.len(), .0.name)]
    Ambiguous(AmbiguousAgentName),
    #[error("the agent holds no item {0}")]
    NoItem(String),
    #[error("older history is held by the agent's host, which cannot be reached")]
    OriginUnreachable,
    #[error("the store: {0}")]
    Store(#[from] StoreError),
}

impl ServeError {
    /// The error as the wire carries it: a code, and for an ambiguous name
    /// the candidates as an `amux.v1.AmbiguousAgentName` detail.
    pub fn to_wire(&self) -> wire::Error {
        let code = match self {
            Self::NoAgent | Self::NoAgentNamed(_) | Self::NoItem(_) => ErrorCode::NotFound,
            Self::Ambiguous(_) => ErrorCode::FailedPrecondition,
            Self::OriginUnreachable => ErrorCode::Unreachable,
            Self::Store(_) => ErrorCode::Internal,
        };
        let details = match self {
            Self::Ambiguous(ambiguous) => vec![ErrorDetail {
                r#type: AmbiguousAgentName::full_name(),
                value: ambiguous.encode_to_vec(),
            }],
            _ => Vec::new(),
        };
        wire::Error {
            code: code as i32,
            message: self.to_string(),
            details,
        }
    }
}

/// One Subscribe stream: the opening read under the store lock, then every
/// event the agent's channel carries, verbatim.
pub struct Subscription {
    opening: VecDeque<Arc<SessionEvent>>,
    live: broadcast::Receiver<Arc<SessionEvent>>,
    ended: bool,
}

impl Subscription {
    /// The next event, or None once the stream has ended: after Lagged, or
    /// when the agent is deleted.
    pub async fn next(&mut self) -> Option<Arc<SessionEvent>> {
        if let Some(event) = self.opening.pop_front() {
            return Some(event);
        }
        if self.ended {
            return None;
        }
        match self.live.recv().await {
            Ok(event) => Some(event),
            Err(RecvError::Lagged(_)) => {
                // The ring moved on without this reader; what it missed is
                // in the store, and the client re-tails to read it.
                self.ended = true;
                Some(Arc::new(event(session_event::Of::Lagged(Lagged {}))))
            }
            Err(RecvError::Closed) => {
                self.ended = true;
                None
            }
        }
    }
}

/// One SubscribeInventory stream: hosts and agent rows, CaughtUp, then
/// every change.
pub struct InventorySubscription {
    opening: VecDeque<Arc<InventoryEvent>>,
    live: broadcast::Receiver<Arc<InventoryEvent>>,
    ended: bool,
    /// The one host whose entry and agents the stream carries, for a
    /// paired host's stream; every host's for a client's.
    only: Option<Vec<u8>>,
}

impl InventorySubscription {
    /// The next event, or None once the stream has ended. A reader that
    /// falls behind the ring is closed and subscribes again for a fresh
    /// current set.
    pub async fn next(&mut self) -> Option<Arc<InventoryEvent>> {
        if let Some(event) = self.opening.pop_front() {
            return Some(event);
        }
        if self.ended {
            return None;
        }
        loop {
            match self.live.recv().await {
                Ok(event) if self.carries(&event) => return Some(event),
                Ok(_) => {}
                Err(RecvError::Lagged(_) | RecvError::Closed) => {
                    self.ended = true;
                    return None;
                }
            }
        }
    }

    fn carries(&self, event: &InventoryEvent) -> bool {
        let Some(only) = &self.only else {
            return true;
        };
        match &event.of {
            Some(inventory_event::Of::Host(host)) => &host.host_id == only,
            Some(inventory_event::Of::HostRemoved(removed)) => &removed.host_id == only,
            Some(inventory_event::Of::Agent(agent)) => &agent.host_id == only,
            Some(inventory_event::Of::AgentRemoved(removed)) => &removed.host_id == only,
            Some(inventory_event::Of::CaughtUp(_)) | None => true,
        }
    }
}

pub(crate) fn event(of: session_event::Of) -> SessionEvent {
    SessionEvent { of: Some(of) }
}

pub(crate) fn inventory(of: inventory_event::Of) -> InventoryEvent {
    InventoryEvent { of: Some(of) }
}

/// The row for an agent id, whichever host owns it.
fn row_by_id(store: &Sqlite, own_host: &[u8], agent_id: &[u8]) -> Result<AgentRow, ServeError> {
    if let Some(row) = store.agent(&AgentKey::new(own_host, agent_id))? {
        return Ok(row);
    }
    store
        .agents()?
        .into_iter()
        .find(|row| row.agent.agent == agent_id)
        .ok_or(ServeError::NoAgent)
}

impl ProfileRuntime {
    /// Opens a Subscribe stream on an agent: its Opening, its Snapshot,
    /// the newest `tail` rows of its block (at most K), its marker if one
    /// is set, then every event its source broadcasts.
    pub async fn subscribe(&self, agent_id: &[u8], tail: u32) -> Result<Subscription, ServeError> {
        self.open_subscription(agent_id, Opening::Tail(tail)).await
    }

    /// Opens a Subscribe stream for a peer that holds everything through
    /// `revision` of this origin's `generation`: the rows revised since,
    /// in revision order, when there are at most `cap`; otherwise, or when
    /// the generation is not the one this origin runs, Reset and the
    /// newest `cap` rows. Either way the Opening and Snapshot lead and the
    /// marker follows, then every event.
    pub async fn subscribe_after(
        &self,
        agent_id: &[u8],
        revision: u64,
        cap: u32,
        generation: u64,
    ) -> Result<Subscription, ServeError> {
        self.open_subscription(
            agent_id,
            Opening::After {
                revision,
                cap,
                generation,
            },
        )
        .await
    }

    async fn open_subscription(
        &self,
        agent_id: &[u8],
        from: Opening,
    ) -> Result<Subscription, ServeError> {
        let k = self.launch().tail_rows;
        // The store stays locked from the join through the whole opening.
        // Commits and marker changes need the store exclusively and publish
        // while they hold it, so with the lock held nothing commits between
        // the join and the cut: every record is either in the opening or
        // arrives live, never both and never neither, and the marker the
        // cut reads describes the same point as its rows and Snapshot. A
        // Snapshot read on its own could pair a queue from before a
        // withdrawal with a CaughtUp from after it. The opening is built
        // into memory, so nothing here waits on the client with the lock
        // held; do not release it between the marker read and the row read.
        let store = self.store.lock().await;
        let row = row_by_id(&store, store.own_host(), agent_id)?;
        let own = row.agent.host == store.own_host();
        if !own {
            self.used(&row.agent);
            self.ensure_source(&row.agent);
        }
        let live = self.fanout.join(&row.agent);
        let hook = self.join_hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook().await;
        }
        // The origin generation the rows belong to: this run's for an
        // agent of this host, and what the replica's source last caught
        // up under for a replica (0 until one has).
        let generation = if own {
            self.generation()
        } else {
            row.source_generation
        };
        let (reset, rows, cut) = match from {
            // A client's tail never exceeds what a replica keeps.
            Opening::Tail(tail) => {
                let mut cut = store.cut(&row.agent, tail.min(k))?;
                (false, std::mem::take(&mut cut.held), cut)
            }
            Opening::After {
                revision,
                cap,
                generation: held,
            } => {
                let cap = cap.min(MAX_CAP);
                // A cursor of another generation names revisions this
                // origin may have minted again for different content, so
                // it is answered as a cursor too far behind is: with a
                // fresh tail.
                let delta = if held == generation {
                    store.after(&row.agent, revision, cap)?
                } else {
                    None
                };
                match delta {
                    Some(delta) => (false, delta, store.cut(&row.agent, 0)?),
                    None => {
                        let mut cut = store.cut(&row.agent, cap)?;
                        (true, std::mem::take(&mut cut.held), cut)
                    }
                }
            }
        };
        let mut opening = VecDeque::with_capacity(rows.len() + 4);
        opening.push_back(Arc::new(event(session_event::Of::Opening(wire::Opening {
            generation,
        }))));
        if reset {
            opening.push_back(Arc::new(event(session_event::Of::Reset(wire::Reset {}))));
        }
        let snapshot = cut.snapshot.unwrap_or_else(|| Snapshot {
            // Nothing emitted yet: the kind tag and an empty body.
            agent_id: row.agent.agent.clone(),
            kind: row.kind,
            ..Snapshot::default()
        });
        opening.push_back(Arc::new(event(session_event::Of::Snapshot(snapshot))));
        for item in rows {
            opening.push_back(Arc::new(event(session_event::Of::Item(item))));
        }
        let marker = match cut.marker {
            // A replica nobody follows is served as it stands, stale and
            // honestly: its host is away, or no source has reached it yet.
            None if !own && !self.source_open(&row.agent) => Some(Marker::Detached),
            marker => marker,
        };
        match marker {
            Some(Marker::CaughtUp) => {
                let revision = if own {
                    row.next_revision.saturating_sub(1)
                } else {
                    row.source_cursor
                };
                opening.push_back(Arc::new(event(session_event::Of::CaughtUp(CaughtUp {
                    revision,
                }))));
            }
            Some(Marker::Detached) => {
                opening.push_back(Arc::new(event(session_event::Of::Detached(
                    wire::Detached {},
                ))));
            }
            None => {}
        }
        drop(store);
        Ok(Subscription {
            opening,
            live,
            ended: false,
        })
    }

    /// A page of older items by order, newest first. Held rows answer it;
    /// below a replica's block the history is at the origin, which is asked
    /// and whose answer extends the block where it joins it. An origin that
    /// cannot be asked is an error, never an empty page.
    pub async fn fetch(&self, request: &FetchRequest) -> Result<FetchResponse, ServeError> {
        let limit = request.limit.min(MAX_PAGE);
        let (row, page) = {
            let store = self.store.lock().await;
            let row = row_by_id(&store, store.own_host(), &request.agent_id)?;
            let page = store.page(&row.agent, request.before_order, limit)?;
            (row, page)
        };
        let own = row.agent.host == self.host().as_bytes().as_slice();
        if own || page.end != PageEnd::Boundary || page.items.len() >= limit as usize {
            return Ok(FetchResponse {
                exhausted: page.end == PageEnd::Exhausted,
                items: page.items,
            });
        }
        self.used(&row.agent);
        // Below the block: the origin's page starts where the held rows
        // stop, or where the request asked if that is lower still.
        let before = match (row.complete_from_order, request.before_order) {
            (Some(floor), Some(before)) => Some(floor.min(before)),
            (Some(floor), None) => Some(floor),
            (None, before) => before,
        };
        let wanted = limit - page.items.len() as u32;
        match self.fetch_from_origin(&row.agent, before, wanted).await {
            Ok(older) => {
                let mut items = page.items;
                items.extend(older.items);
                Ok(FetchResponse {
                    items,
                    exhausted: older.exhausted,
                })
            }
            Err(()) if page.items.is_empty() => Err(ServeError::OriginUnreachable),
            // What is held is served; the next page down asks again.
            Err(()) => Ok(FetchResponse {
                items: page.items,
                exhausted: false,
            }),
        }
    }

    /// Whether the agent is one of this host's own, not a replica of
    /// another host's.
    pub(crate) async fn is_own(&self, agent_id: &[u8]) -> Result<bool, ServeError> {
        let store = self.store.lock().await;
        Ok(store
            .agent(&AgentKey::new(store.own_host(), agent_id))?
            .is_some())
    }

    /// One item in full.
    pub async fn get(&self, request: &GetRequest) -> Result<Item, ServeError> {
        let store = self.store.lock().await;
        let row = row_by_id(&store, store.own_host(), &request.agent_id)?;
        store
            .get(&row.agent, &request.key)?
            .ok_or_else(|| ServeError::NoItem(request.key.clone()))
    }

    /// Opens a SubscribeInventory stream: the host set and every agent row
    /// the store holds, CaughtUp, then every change.
    pub async fn subscribe_inventory(&self) -> Result<InventorySubscription, ServeError> {
        self.open_inventory(None).await
    }

    /// Opens the SubscribeInventory stream a paired host reads: this host's
    /// entry and its own agents, on opening and live. What this host knows
    /// of other hosts is theirs to tell, so it is never passed on.
    pub(crate) async fn subscribe_own_inventory(
        &self,
    ) -> Result<InventorySubscription, ServeError> {
        self.open_inventory(Some(self.host().as_bytes().to_vec()))
            .await
    }

    async fn open_inventory(
        &self,
        only: Option<Vec<u8>>,
    ) -> Result<InventorySubscription, ServeError> {
        // Locked across the join and the read for the reason Subscribe is:
        // every row change publishes while it holds the store.
        let store = self.store.lock().await;
        let live = self.fanout.join_inventory();
        let rows = store.agents()?;
        let hosts = self.published_hosts();
        drop(store);
        let mut opening = VecDeque::with_capacity(rows.len() + hosts.len() + 1);
        let carried = |host: &[u8]| only.as_deref().is_none_or(|only| only == host);
        for host in hosts.into_iter().filter(|host| carried(&host.host_id)) {
            opening.push_back(Arc::new(inventory(inventory_event::Of::Host(host))));
        }
        for row in rows.iter().filter(|row| carried(&row.agent.host)) {
            opening.push_back(Arc::new(inventory(inventory_event::Of::Agent(to_wire(
                row,
            )))));
        }
        opening.push_back(Arc::new(inventory(inventory_event::Of::CaughtUp(
            CaughtUp::default(),
        ))));
        Ok(InventorySubscription {
            opening,
            live,
            ended: false,
            only,
        })
    }

    /// The one agent with this name across the fleet the store knows.
    pub async fn resolve_agent(&self, name: &str) -> Result<Agent, ServeError> {
        let mut matches: Vec<Agent> = self
            .store
            .lock()
            .await
            .agents()?
            .iter()
            .filter(|row| row.name.as_deref() == Some(name))
            .map(to_wire)
            .collect();
        match matches.len() {
            0 => Err(ServeError::NoAgentNamed(name.to_owned())),
            1 => Ok(matches.remove(0)),
            _ => Err(ServeError::Ambiguous(AmbiguousAgentName {
                name: name.to_owned(),
                candidates: matches,
            })),
        }
    }
}
