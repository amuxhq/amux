//! Answering client calls: Subscribe, Fetch and Get read an agent's rows;
//! SubscribeInventory reads the fleet. Each is store reads in one step, so
//! nothing commits between a stream's opening and the point it follows
//! from.

use std::sync::Arc;

use prost::Message as _;
use serde::{Deserialize, Serialize};
use store::{AgentRow, Marker, PageEnd};
use wire::{
    Agent, AgentRef, CaughtUp, FetchRequest, FetchResponse, GetRequest, InventoryEvent, Item,
    Lagged, SessionEvent, Snapshot, inventory_event, session_event,
};

use crate::ids::{CallId, JobId};
use crate::io::{Job, Output};
use crate::payload::{CallError, Caller, ClientRequest, JobResult, Opening, Response, Status};
use crate::{Daemon, Pending};

/// The most items one Fetch returns, whatever the limit asked for.
pub const MAX_PAGE: u32 = 500;
/// The most rows a peer's catch-up may ask for.
pub const MAX_CAP: u32 = 10_000;

/// Open calls, by id: what each streams, or that it awaits a job.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answers {
    open: std::collections::BTreeMap<CallId, OpenCall>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum OpenCall {
    /// A Subscribe stream on one agent.
    Session { host: Vec<u8>, agent: Vec<u8> },
    /// A SubscribeInventory stream.
    Inventory,
    /// A Fetch waiting on the agent's origin.
    Fetch,
}

/// A Fetch that went below a replica's block: the call it answers and the
/// held items, encoded, that lead the origin's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PendingFetch {
    call: CallId,
    held: Vec<Vec<u8>>,
}

impl Daemon {
    pub(crate) fn client_call(
        &mut self,
        call: CallId,
        _who: Caller,
        request: ClientRequest,
        out: &mut Vec<Output>,
    ) {
        let answered = match request {
            ClientRequest::Subscribe {
                agent_id,
                from,
                tail_rows,
                followed,
            } => self.subscribe(call, &agent_id, from, tail_rows, followed, out),
            ClientRequest::Fetch(request) => self.fetch(call, &request, out),
            ClientRequest::Get(request) => self.get(call, &request, out),
            ClientRequest::SubscribeInventory { hosts, only } => {
                self.subscribe_inventory(call, hosts, only, out)
            }
        };
        if let Err(error) = answered {
            self.answers.open.remove(&call);
            out.push(Output::EndCall {
                call,
                status: Status::Failed(error),
            });
        }
    }

    /// Opens a Subscribe stream: its Opening, a Reset when a peer's cursor
    /// cannot be continued, its Snapshot, the rows the opening asks for,
    /// its marker if one is set, then whatever follows.
    fn subscribe(
        &mut self,
        call: CallId,
        agent_id: &[u8],
        from: Opening,
        tail_rows: u32,
        followed: bool,
        out: &mut Vec<Output>,
    ) -> Result<(), CallError> {
        let store = &self.store;
        let row = store.agent_by_id(agent_id)?.ok_or(CallError::NoAgent)?;
        let own = row.agent.host == store.own_host();
        // The origin generation the rows belong to: this run's for an
        // agent of this host, and what the replica's source last caught
        // up under for a replica (0 until one has).
        let generation = if own {
            self.options.generation
        } else {
            row.source_generation
        };
        let (reset, rows, cut) = match from {
            // A client's tail never exceeds what a replica keeps.
            Opening::Tail(tail) => {
                let mut cut = store.cut(&row.agent, tail.min(tail_rows))?;
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
        let mut answer = |of| {
            out.push(Output::Answer {
                call,
                response: Response::Session(Arc::new(event(of))),
            })
        };
        answer(session_event::Of::Opening(wire::Opening { generation }));
        if reset {
            answer(session_event::Of::Reset(wire::Reset {}));
        }
        let snapshot = cut.snapshot.unwrap_or_else(|| Snapshot {
            // Nothing emitted yet: the kind tag and an empty body.
            agent_id: row.agent.agent.clone(),
            kind: row.kind,
            ..Snapshot::default()
        });
        answer(session_event::Of::Snapshot(snapshot));
        for item in rows {
            answer(session_event::Of::Item(item));
        }
        let marker = match cut.marker {
            // A replica nobody follows is served as it stands, stale and
            // honestly: its host is away, or no source has reached it yet.
            None if !own && !followed => Some(Marker::Detached),
            marker => marker,
        };
        match marker {
            Some(Marker::CaughtUp) => {
                let revision = if own {
                    row.next_revision.saturating_sub(1)
                } else {
                    row.source_cursor
                };
                answer(session_event::Of::CaughtUp(CaughtUp { revision }));
            }
            Some(Marker::Detached) => answer(session_event::Of::Detached(wire::Detached {})),
            None => {}
        }
        self.answers.open.insert(
            call,
            OpenCall::Session {
                host: row.agent.host.clone(),
                agent: row.agent.agent.clone(),
            },
        );
        out.push(Output::Follow {
            call,
            agent: row.agent,
        });
        Ok(())
    }

    /// Opens a SubscribeInventory stream: the host set and every agent row
    /// the store holds, CaughtUp, then every change.
    fn subscribe_inventory(
        &mut self,
        call: CallId,
        hosts: Vec<wire::HostEntry>,
        only: Option<Vec<u8>>,
        out: &mut Vec<Output>,
    ) -> Result<(), CallError> {
        let rows = self.store.agents()?;
        let carried = |host: &[u8]| only.as_deref().is_none_or(|only| only == host);
        let mut answer = |of| {
            out.push(Output::Answer {
                call,
                response: Response::Inventory(Arc::new(inventory(of))),
            })
        };
        for host in hosts.into_iter().filter(|host| carried(&host.host_id)) {
            answer(inventory_event::Of::Host(host));
        }
        for row in rows.iter().filter(|row| carried(&row.agent.host)) {
            answer(inventory_event::Of::Agent(to_wire(row)));
        }
        answer(inventory_event::Of::CaughtUp(CaughtUp::default()));
        self.answers.open.insert(call, OpenCall::Inventory);
        out.push(Output::FollowInventory { call, only });
        Ok(())
    }

    /// A page of older items by order, newest first. Held rows answer it;
    /// below a replica's block the history is at the origin, which is asked
    /// and whose answer extends the block where it joins it. An origin that
    /// cannot be asked is an error, never an empty page.
    fn fetch(
        &mut self,
        call: CallId,
        request: &FetchRequest,
        out: &mut Vec<Output>,
    ) -> Result<(), CallError> {
        let limit = request.limit.min(MAX_PAGE);
        let row = self
            .store
            .agent_by_id(&request.agent_id)?
            .ok_or(CallError::NoAgent)?;
        let page = self.store.page(&row.agent, request.before_order, limit)?;
        let own = row.agent.host == self.store.own_host();
        if own || page.end != PageEnd::Boundary || page.items.len() >= limit as usize {
            answered(
                call,
                Response::Fetched(FetchResponse {
                    exhausted: page.end == PageEnd::Exhausted,
                    items: page.items,
                }),
                out,
            );
            return Ok(());
        }
        // Below the block: the origin's page starts where the held rows
        // stop, or where the request asked if that is lower still.
        let before_order = match (row.complete_from_order, request.before_order) {
            (Some(floor), Some(before)) => Some(floor.min(before)),
            (Some(floor), None) => Some(floor),
            (None, before) => before,
        };
        let job = self.pending.mint();
        self.pending.fetches.insert(
            job,
            PendingFetch {
                call,
                held: page.items.iter().map(|item| item.encode_to_vec()).collect(),
            },
        );
        self.answers.open.insert(call, OpenCall::Fetch);
        out.push(Output::Job {
            id: job,
            op: Job::FetchFromOrigin {
                agent: row.agent,
                before_order,
                limit: limit - page.items.len() as u32,
            },
        });
        Ok(())
    }

    /// One item in full.
    fn get(
        &mut self,
        call: CallId,
        request: &GetRequest,
        out: &mut Vec<Output>,
    ) -> Result<(), CallError> {
        let row = self
            .store
            .agent_by_id(&request.agent_id)?
            .ok_or(CallError::NoAgent)?;
        let item = self
            .store
            .get(&row.agent, &request.key)?
            .ok_or_else(|| CallError::NoItem(request.key.clone()))?;
        answered(call, Response::Item(item), out);
        Ok(())
    }

    /// A job's result, for the call that waits on it.
    pub(crate) fn job_done(&mut self, job: JobId, result: JobResult, out: &mut Vec<Output>) {
        let Some(PendingFetch { call, held }) = self.pending.fetches.remove(&job) else {
            return;
        };
        // A call cancelled while its origin was asked wants no answer.
        if self.answers.open.remove(&call).is_none() {
            return;
        }
        let mut items: Vec<Item> = held
            .iter()
            .map(|bytes| Item::decode(bytes.as_slice()).expect("an item the core encoded"))
            .collect();
        let response = match result {
            JobResult::Fetched(Some(older)) => {
                items.extend(older.items);
                FetchResponse {
                    items,
                    exhausted: older.exhausted,
                }
            }
            _ if items.is_empty() => {
                out.push(Output::EndCall {
                    call,
                    status: Status::Failed(CallError::OriginUnreachable),
                });
                return;
            }
            // What is held is served; the next page down asks again.
            _ => FetchResponse {
                items,
                exhausted: false,
            },
        };
        answered(call, Response::Fetched(response), out);
    }

    /// The call's stream fell more than its queue behind. Saying so in
    /// band lets the reader resubscribe at once for a fresh opening, where
    /// a bare close would read as the far side going away; what it missed
    /// is in the store.
    pub(crate) fn lagged(&mut self, call: CallId, out: &mut Vec<Output>) {
        let response = match self.answers.open.get(&call) {
            Some(OpenCall::Session { .. }) => {
                Response::Session(Arc::new(event(session_event::Of::Lagged(Lagged {}))))
            }
            Some(OpenCall::Inventory) => {
                Response::Inventory(Arc::new(inventory(inventory_event::Of::Lagged(Lagged {}))))
            }
            Some(OpenCall::Fetch) | None => return,
        };
        self.answers.open.remove(&call);
        answered(call, response, out);
    }

    /// The caller dropped the call or its stream.
    pub(crate) fn cancelled(&mut self, call: CallId) {
        self.answers.open.remove(&call);
    }
}

impl Pending {
    fn mint(&mut self) -> JobId {
        self.next_job += 1;
        JobId(self.next_job)
    }
}

/// One answer and the call's end.
fn answered(call: CallId, response: Response, out: &mut Vec<Output>) {
    out.push(Output::Answer { call, response });
    out.push(Output::EndCall {
        call,
        status: Status::Ok,
    });
}

fn event(of: session_event::Of) -> SessionEvent {
    SessionEvent { of: Some(of) }
}

fn inventory(of: inventory_event::Of) -> InventoryEvent {
    InventoryEvent { of: Some(of) }
}

/// An agent's row as the inventory carries it.
pub fn to_wire(row: &AgentRow) -> Agent {
    Agent {
        agent_id: row.agent.agent.clone(),
        host_id: row.agent.host.clone(),
        kind: row.kind,
        name: row.name.clone(),
        cwd: row.cwd.clone(),
        parent: row.parent.as_ref().map(|parent| AgentRef {
            host_id: parent.host.clone(),
            agent_id: parent.agent.clone(),
        }),
        created_at_ms: row.created_at_ms,
        lifecycle: row.lifecycle,
        exit_cause: row.exit_cause,
        exit_detail: row.exit_detail.clone(),
        phase: row.phase,
        working_on: row.working_on.clone(),
        // Before its first snapshot an agent has been starting since it
        // was created.
        phase_since_ms: row.phase_since_ms.unwrap_or(row.created_at_ms),
        incarnation: row.incarnation,
        git: row.git.clone(),
        descendants: row.descendants,
        background_jobs: row.background_jobs,
    }
}

#[cfg(test)]
mod tests {
    use store::{AgentKey, Sqlite};

    use super::*;
    use crate::{Input, StartOptions};

    const OWN: &[u8; 16] = b"own-host-0000000";
    const OTHER: &[u8; 16] = b"far-host-0000000";

    fn daemon() -> Daemon {
        let mut store = Sqlite::open_in_memory(*OWN).expect("a store opens");
        let row = AgentRow::new(
            AgentKey::new(*OWN, *b"agent-0000000000"),
            wire::Kind::Codex,
            "/src",
        );
        store.put_agent(&row).expect("the store takes its agent");
        Daemon::new(store, StartOptions { generation: 7 })
    }

    fn call(daemon: &mut Daemon, call: u64, request: ClientRequest) -> Vec<Output> {
        let mut out = Vec::new();
        daemon.step(
            Input::ClientCall {
                call: CallId(call),
                who: Caller::Local,
                request,
            },
            &mut out,
        );
        out
    }

    fn step(daemon: &mut Daemon, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        daemon.step(input, &mut out);
        out
    }

    fn subscribe(agent_id: &[u8]) -> ClientRequest {
        ClientRequest::Subscribe {
            agent_id: agent_id.to_vec(),
            from: Opening::Tail(10),
            tail_rows: 200,
            followed: false,
        }
    }

    fn session(of: session_event::Of) -> Response {
        Response::Session(Arc::new(event(of)))
    }

    #[test]
    fn a_call_on_an_unknown_agent_ends_refused_without_an_answer() {
        let mut daemon = daemon();
        let get = ClientRequest::Get(GetRequest {
            agent_id: b"nobody-000000000".to_vec(),
            key: "k".into(),
        });
        assert_eq!(
            call(&mut daemon, 1, get),
            vec![Output::EndCall {
                call: CallId(1),
                status: Status::Failed(CallError::NoAgent),
            }]
        );
        assert_eq!(
            call(&mut daemon, 2, subscribe(b"nobody-000000000")),
            vec![Output::EndCall {
                call: CallId(2),
                status: Status::Failed(CallError::NoAgent),
            }]
        );
        assert_eq!(daemon.answers, Answers::default());
    }

    #[test]
    fn a_missing_item_is_refused_by_its_key() {
        let mut daemon = daemon();
        let get = ClientRequest::Get(GetRequest {
            agent_id: b"agent-0000000000".to_vec(),
            key: "absent".into(),
        });
        assert_eq!(
            call(&mut daemon, 1, get),
            vec![Output::EndCall {
                call: CallId(1),
                status: Status::Failed(CallError::NoItem("absent".into())),
            }]
        );
    }

    #[test]
    fn a_subscribe_answers_its_opening_then_follows_and_a_lag_ends_it_once() {
        let mut daemon = daemon();
        let out = call(&mut daemon, 1, subscribe(b"agent-0000000000"));
        let snapshot = Snapshot {
            agent_id: b"agent-0000000000".to_vec(),
            kind: wire::Kind::Codex as i32,
            ..Snapshot::default()
        };
        assert_eq!(
            out,
            vec![
                Output::Answer {
                    call: CallId(1),
                    response: session(session_event::Of::Opening(wire::Opening { generation: 7 })),
                },
                Output::Answer {
                    call: CallId(1),
                    response: session(session_event::Of::Snapshot(snapshot)),
                },
                Output::Follow {
                    call: CallId(1),
                    agent: AgentKey::new(*OWN, *b"agent-0000000000"),
                },
            ]
        );

        let lagged = step(&mut daemon, Input::AnswerLagged { call: CallId(1) });
        assert_eq!(
            lagged,
            vec![
                Output::Answer {
                    call: CallId(1),
                    response: session(session_event::Of::Lagged(Lagged {})),
                },
                Output::EndCall {
                    call: CallId(1),
                    status: Status::Ok,
                },
            ]
        );
        assert_eq!(
            step(&mut daemon, Input::AnswerLagged { call: CallId(1) }),
            Vec::new(),
            "a call already ended lags no more"
        );
    }

    #[test]
    fn an_inventory_opening_carries_only_the_named_host_and_a_cancel_releases_it() {
        let mut daemon = daemon();
        let host = |id: &[u8]| wire::HostEntry {
            host_id: id.to_vec(),
            ..wire::HostEntry::default()
        };
        let out = call(
            &mut daemon,
            3,
            ClientRequest::SubscribeInventory {
                hosts: vec![host(OWN), host(OTHER)],
                only: Some(OWN.to_vec()),
            },
        );
        let inventory_answer = |of| Output::Answer {
            call: CallId(3),
            response: Response::Inventory(Arc::new(inventory(of))),
        };
        let row = daemon
            .store
            .agent_by_id(b"agent-0000000000")
            .unwrap()
            .unwrap();
        assert_eq!(
            out,
            vec![
                inventory_answer(inventory_event::Of::Host(host(OWN))),
                inventory_answer(inventory_event::Of::Agent(to_wire(&row))),
                inventory_answer(inventory_event::Of::CaughtUp(CaughtUp::default())),
                Output::FollowInventory {
                    call: CallId(3),
                    only: Some(OWN.to_vec()),
                },
            ]
        );
        assert_eq!(
            step(&mut daemon, Input::CallCancelled { call: CallId(3) }),
            Vec::new()
        );
        assert_eq!(
            step(&mut daemon, Input::AnswerLagged { call: CallId(3) }),
            Vec::new(),
            "a cancelled stream is answered no more"
        );
        assert_eq!(daemon.answers, Answers::default());
    }
}
