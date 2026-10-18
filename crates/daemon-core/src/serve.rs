//! Answering client calls: Subscribe, Fetch and Get read an agent's rows;
//! SubscribeInventory reads the fleet. Each opening is store reads in one
//! step, and every change after it is answered by the step that committed
//! it, so a stream reads one cut and then each commit once, in commit
//! order.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use store::{AgentKey, AgentRow, Marker, PageEnd};
use wire::{
    Agent, AgentRef, CaughtUp, FetchRequest, FetchResponse, GetRequest, InventoryEvent, Lagged,
    SessionEvent, Snapshot, inventory_event, session_event,
};

use crate::ids::{CallId, JobId};
use crate::io::{FromShell, Output};
use crate::payload::{CallError, ClientRequest, Opening, Response, Status};
use crate::{Daemon, Pending};

/// The most items one Fetch returns, whatever the limit asked for.
pub const MAX_PAGE: u32 = 500;
/// The most rows a peer's catch-up may ask for.
pub const MAX_CAP: u32 = 10_000;

/// Open calls, by id: what each streams, or that it awaits a job; and the
/// agents nothing is told about for now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answers {
    open: BTreeMap<CallId, OpenCall>,
    muted: BTreeSet<Key>,
    /// The highest generation a stream on this host's agents has opened
    /// under.
    opened_under: u64,
}

/// An agent by its host and id, as the checkpoint keeps it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct Key {
    pub(crate) host: Vec<u8>,
    pub(crate) agent: Vec<u8>,
}

impl From<&AgentKey> for Key {
    fn from(key: &AgentKey) -> Self {
        Key {
            host: key.host.clone(),
            agent: key.agent.clone(),
        }
    }
}

impl Key {
    pub(crate) fn agent_key(&self) -> AgentKey {
        AgentKey::new(self.host.clone(), self.agent.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum OpenCall {
    /// A Subscribe stream on one agent.
    Session(Key),
    /// A SubscribeInventory stream, `only` one host's when it names one.
    Inventory { only: Option<Vec<u8>> },
    /// A Fetch waiting on the agent's origin.
    Fetch,
    /// A call forwarded to the host that owns its agent, waiting on its
    /// reply.
    Forward,
}

impl Answers {
    fn muted(&self, agent: &AgentKey) -> bool {
        !self.muted.is_empty() && self.muted.contains(&Key::from(agent))
    }

    /// Nothing about the agent reaches a subscriber until it is unmuted;
    /// its streams stay open.
    pub(crate) fn mute(&mut self, agent: &AgentKey) {
        self.muted.insert(Key::from(agent));
    }

    pub(crate) fn unmute(&mut self, agent: &AgentKey) {
        self.muted.remove(&Key::from(agent));
    }

    /// Holds `call` open until the host it was forwarded to replies.
    pub(crate) fn forwarding(&mut self, call: CallId) {
        self.open.insert(call, OpenCall::Forward);
    }

    pub(crate) fn watched(&self, key: &Key) -> bool {
        self.open
            .values()
            .any(|open| matches!(open, OpenCall::Session(watching) if watching == key))
    }

    /// Opens `call` as a stream on `agent`.
    fn watch(&mut self, call: CallId, agent: &AgentKey) {
        self.open.insert(call, OpenCall::Session(Key::from(agent)));
    }

    /// Lets `call` go.
    fn release(&mut self, call: CallId) -> Option<OpenCall> {
        self.open.remove(&call)
    }
}

impl Daemon {
    pub(crate) fn client_call(
        &mut self,
        call: CallId,
        request: ClientRequest,
        out: &mut Vec<Output>,
    ) {
        let answered = match request {
            ClientRequest::Subscribe { agent_id, tail } => {
                self.subscribe(call, &agent_id, Opening::Tail(tail), out)
            }
            ClientRequest::Fetch(request) => self.fetch(call, &request, out),
            ClientRequest::Get(request) => self.get(call, &request, out),
            ClientRequest::SubscribeInventory => self.subscribe_inventory(call, None, out),
            ClientRequest::Forward { host, request } => {
                self.forward(call, host, request, out);
                Ok(())
            }
            // The registry answers its calls itself, once each is over.
            ClientRequest::CreateAgent(create) => {
                self.create(call, create, out);
                Ok(())
            }
            ClientRequest::ResumeAgent(resume) => {
                self.resume(call, resume, out);
                Ok(())
            }
            ClientRequest::StopAgent { agent } => {
                self.stop(call, agent, out);
                Ok(())
            }
            ClientRequest::KillAgent { agent } => {
                self.kill(call, agent, out);
                Ok(())
            }
            ClientRequest::DeleteAgent { agent } => {
                self.delete(call, agent, out);
                Ok(())
            }
            ClientRequest::RenameAgent { agent, name } => {
                self.rename(call, agent, name, out);
                Ok(())
            }
            ClientRequest::PutBlob { request, hash } => self.put_blob(call, request, hash, out),
            ClientRequest::KeepBlob {
                agent_id,
                hash,
                bytes,
            } => self.keep_blob(call, &agent_id, hash, bytes, out),
            ClientRequest::GetBlob(request) => self.get_blob(call, &request, out),
            ClientRequest::Diff(request) => self.diff(call, request, out),
            ClientRequest::SaveDraft { agent_id, draft } => {
                self.save_draft(call, agent_id, &draft, out);
                Ok(())
            }
            ClientRequest::GetDraft { agent_id } => {
                self.get_draft(call, &agent_id, out);
                Ok(())
            }
            ClientRequest::ChangeTrust(change) => {
                self.change_trust_call(call, change, out);
                Ok(())
            }
            ClientRequest::StartPairing { method, ttl_ms } => {
                self.start_pairing(call, method, ttl_ms, out);
                Ok(())
            }
            ClientRequest::CancelPairing => {
                self.cancel_pairing(call, out);
                Ok(())
            }
            ClientRequest::ConfirmPair { token } => {
                self.confirm_pair(call, token, out);
                Ok(())
            }
            ClientRequest::AbandonPair { token } => {
                self.abandon_pair(call, token, out);
                Ok(())
            }
            ClientRequest::PairOverSsh {
                local,
                peer,
                reachability,
            } => {
                self.pair_over_ssh(call, local, peer, reachability, out);
                Ok(())
            }
            ClientRequest::SendMessage { envelope, from } => {
                self.send_message(call, *envelope, from, out)
            }
        };
        self.end_unanswered(call, answered, out);
    }

    /// Ends a call that could not be answered, saying why.
    pub(crate) fn end_unanswered(
        &mut self,
        call: CallId,
        answered: Result<(), CallError>,
        out: &mut Vec<Output>,
    ) {
        if let Err(error) = answered {
            self.answers.release(call);
            out.push(Output::EndCall {
                call,
                status: Status::Failed(error),
            });
        }
    }

    /// Opens a Subscribe stream: its Opening, a Reset when a peer's cursor
    /// cannot be continued, its Snapshot, the rows the opening asks for,
    /// its marker if one is set, then whatever follows. A replica's source
    /// is opened for it if none is.
    pub(crate) fn subscribe(
        &mut self,
        call: CallId,
        agent_id: &[u8],
        from: Opening,
        out: &mut Vec<Output>,
    ) -> Result<(), CallError> {
        let row = self
            .store
            .agent_by_id(agent_id)?
            .ok_or(CallError::NoAgent)?;
        let own = row.agent.host == self.store.own_host();
        let followed = !own && self.follow_replica(&row.agent, out);
        let store = &self.store;
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
                let mut cut = store.cut(&row.agent, tail.min(self.settings.tail_rows))?;
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
        if own {
            self.touched.opened_under = Some((generation, self.answers.opened_under));
            self.answers.opened_under = self.answers.opened_under.max(generation);
        }
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
        self.answers.watch(call, &row.agent);
        Ok(())
    }

    /// Opens a SubscribeInventory stream: the host set and every agent row
    /// the store holds, CaughtUp, then every change.
    pub(crate) fn subscribe_inventory(
        &mut self,
        call: CallId,
        only: Option<Vec<u8>>,
        out: &mut Vec<Output>,
    ) -> Result<(), CallError> {
        // What moved earlier in this step reaches the streams already open
        // first, so the opening lists the set as it stands.
        self.touched.host_set = true;
        self.publish_host_set(out);
        let hosts = self.host_set().to_vec();
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
        self.answers.open.insert(call, OpenCall::Inventory { only });
        Ok(())
    }

    /// A page of older items by order, newest first. Held rows answer it;
    /// below a replica's block the history is at the origin, which is asked
    /// and whose answer extends the block where it joins it. An origin that
    /// cannot be asked is an error, never an empty page.
    pub(crate) fn fetch(
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
        let rest = limit - page.items.len() as u32;
        match self.ask_page(call, &row.agent, before_order, page.items, rest, out) {
            Ok(()) => {
                self.answers.open.insert(call, OpenCall::Fetch);
            }
            Err(held) => crate::sources::answer_page(call, held, None, out),
        }
        Ok(())
    }

    /// One item in full.
    pub(crate) fn get(
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

    /// The call's stream fell more than its queue behind. Saying so in
    /// band lets the reader resubscribe at once for a fresh opening, where
    /// a bare close would read as the far side going away; what it missed
    /// is in the store.
    pub(crate) fn lagged(&mut self, call: CallId, out: &mut Vec<Output>) {
        let response = match self.answers.open.get(&call) {
            Some(OpenCall::Session(_)) => {
                Response::Session(Arc::new(event(session_event::Of::Lagged(Lagged {}))))
            }
            Some(OpenCall::Inventory { .. }) => {
                Response::Inventory(Arc::new(inventory(inventory_event::Of::Lagged(Lagged {}))))
            }
            Some(OpenCall::Fetch | OpenCall::Forward) | None => return,
        };
        self.answers.release(call);
        answered(call, response, out);
    }

    /// Lets `call` go; whether it was still open.
    pub(crate) fn release_call(&mut self, call: CallId) -> bool {
        self.answers.release(call).is_some()
    }

    /// The caller dropped the call or its stream.
    pub(crate) fn cancelled(&mut self, call: CallId) {
        self.answers.release(call);
    }

    /// Tells every stream on `agent` of one committed event, unless the
    /// agent is muted.
    pub(crate) fn publish_session(
        &mut self,
        agent: &AgentKey,
        event: Arc<SessionEvent>,
        out: &mut Vec<Output>,
    ) {
        if self.answers.muted(agent) {
            return;
        }
        if agent.host == self.store.own_host()
            && let Some(revision) = revision(&event)
        {
            self.touched.answered(agent, revision);
        }
        let key = Key::from(agent);
        for (call, open) in &self.answers.open {
            if matches!(open, OpenCall::Session(watching) if *watching == key) {
                out.push(Output::Answer {
                    call: *call,
                    response: Response::Session(event.clone()),
                });
            }
        }
    }

    /// Tells every inventory stream that carries it of one change.
    pub(crate) fn publish_inventory(&self, event: Arc<InventoryEvent>, out: &mut Vec<Output>) {
        for (call, open) in &self.answers.open {
            if let OpenCall::Inventory { only } = open
                && carries(only.as_deref(), &event)
            {
                out.push(Output::Answer {
                    call: *call,
                    response: Response::Inventory(event.clone()),
                });
            }
        }
    }

    /// The agent is gone: its streams end, and every inventory stream that
    /// carries it hears `removed`, which says why.
    pub(crate) fn agent_removed(
        &mut self,
        agent: &AgentKey,
        removed: Arc<InventoryEvent>,
        out: &mut Vec<Output>,
    ) {
        let key = Key::from(agent);
        let streams: Vec<CallId> = self
            .answers
            .open
            .iter()
            .filter(|(_, open)| matches!(open, OpenCall::Session(watching) if *watching == key))
            .map(|(call, _)| *call)
            .collect();
        for call in streams {
            self.answers.release(call);
            out.push(Output::EndCall {
                call,
                status: Status::Ok,
            });
        }
        self.publish_inventory(removed, out);
    }

    /// Tells inventory streams what the agent's row now says, unless the
    /// agent is muted.
    pub(crate) fn publish_row(
        &self,
        agent: &AgentKey,
        out: &mut Vec<Output>,
    ) -> Result<(), store::StoreError> {
        if self.answers.muted(agent) {
            return Ok(());
        }
        if let Some(row) = self.store.agent(agent)? {
            let event = inventory(inventory_event::Of::Agent(to_wire(&row)));
            self.publish_inventory(Arc::new(event), out);
        }
        Ok(())
    }

    /// What the shell changed outside a step.
    pub(crate) fn shell_changed(&mut self, change: FromShell, out: &mut Vec<Output>) {
        match change {
            FromShell::Session { agent, event } => self.publish_session(&agent, event, out),
            FromShell::Row { agent, event } => {
                if !self.answers.muted(&agent) {
                    self.publish_inventory(event, out);
                }
            }
            FromShell::Mute(agent) => {
                self.answers.muted.insert(Key::from(&agent));
            }
            FromShell::Unmute(agent) => {
                self.answers.muted.remove(&Key::from(&agent));
            }
        }
    }
}

/// The revision of the agent's history an event carries, if it carries one.
fn revision(event: &SessionEvent) -> Option<u64> {
    match event.of.as_ref()? {
        session_event::Of::Item(item) => Some(item.revision),
        session_event::Of::Append(append) => Some(append.revision),
        session_event::Of::Snapshot(snapshot) => Some(snapshot.revision),
        session_event::Of::CaughtUp(caught_up) => Some(caught_up.revision),
        _ => None,
    }
}

/// Whether an inventory stream that carries `only` one host's changes, or
/// every host's, carries `event`.
fn carries(only: Option<&[u8]>, event: &InventoryEvent) -> bool {
    let Some(only) = only else {
        return true;
    };
    match &event.of {
        Some(inventory_event::Of::Host(host)) => host.host_id == only,
        Some(inventory_event::Of::HostRemoved(removed)) => removed.host_id == only,
        Some(inventory_event::Of::Agent(agent)) => agent.host_id == only,
        Some(inventory_event::Of::AgentRemoved(removed)) => removed.host_id == only,
        Some(inventory_event::Of::CaughtUp(_) | inventory_event::Of::Lagged(_)) | None => true,
    }
}

impl Pending {
    pub(crate) fn mint(&mut self) -> JobId {
        self.next_job += 1;
        JobId(self.next_job)
    }
}

/// One answer and the call's end.
pub(crate) fn answered(call: CallId, response: Response, out: &mut Vec<Output>) {
    out.push(Output::Answer { call, response });
    out.push(Output::EndCall {
        call,
        status: Status::Ok,
    });
}

pub(crate) fn event(of: session_event::Of) -> SessionEvent {
    SessionEvent { of: Some(of) }
}

pub(crate) fn inventory(of: inventory_event::Of) -> InventoryEvent {
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
        Daemon::new(
            store,
            StartOptions {
                generation: 7,
                ..StartOptions::default()
            },
        )
    }

    fn call(daemon: &mut Daemon, call: u64, request: ClientRequest) -> Vec<Output> {
        let mut out = Vec::new();
        daemon.step(
            Input::ClientCall {
                call: CallId(call),
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
            tail: 10,
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
        // This host's entry as the core lists it, named as the driver says.
        let own = |name: &str| wire::HostEntry {
            host_id: OWN.to_vec(),
            name: name.to_owned(),
            generation: 7,
            trust: wire::Trust::Trusted as i32,
            presence: wire::Presence::Online as i32,
            current: Some(true),
            ..wire::HostEntry::default()
        };
        let answers = |out: Vec<Output>| -> Vec<Output> {
            out.into_iter()
                .filter(|output| matches!(output, Output::Answer { .. }))
                .collect()
        };
        // A paired host's inventory carries this host's entry and agents.
        let out = step(
            &mut daemon,
            Input::PeerCall {
                from: crate::HostId(*OTHER),
                call: CallId(3),
                request: crate::payload::PeerRequest::SubscribeInventory,
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
            answers(out),
            vec![
                inventory_answer(inventory_event::Of::Host(own(""))),
                inventory_answer(inventory_event::Of::Agent(to_wire(&row))),
                inventory_answer(inventory_event::Of::CaughtUp(CaughtUp::default())),
            ]
        );
        let elsewhere = crate::host_set::Candidate {
            host: crate::HostId(*OTHER),
            name: "box".into(),
            addrs: Vec::new(),
        };
        assert_eq!(
            answers(step(
                &mut daemon,
                Input::Candidates {
                    found: vec![elsewhere]
                }
            )),
            Vec::new(),
            "another host's change is not carried"
        );
        assert_eq!(
            answers(step(
                &mut daemon,
                Input::OwnHost(Box::new(wire::HostEntry {
                    name: "desk".into(),
                    ..wire::HostEntry::default()
                }))
            )),
            vec![inventory_answer(inventory_event::Of::Host(own("desk")))]
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

    #[test]
    fn a_muted_agent_is_told_nothing_until_unmuted_and_its_removal_ends_its_streams() {
        let mut daemon = daemon();
        let agent = AgentKey::new(*OWN, *b"agent-0000000000");
        call(&mut daemon, 1, subscribe(b"agent-0000000000"));
        call(&mut daemon, 2, subscribe(b"agent-0000000000"));
        let detached = Arc::new(event(session_event::Of::Detached(wire::Detached {})));
        step(
            &mut daemon,
            Input::FromShell(FromShell::Mute(agent.clone())),
        );
        assert_eq!(
            step(
                &mut daemon,
                Input::FromShell(FromShell::Session {
                    agent: agent.clone(),
                    event: detached.clone(),
                })
            ),
            Vec::new()
        );
        step(
            &mut daemon,
            Input::FromShell(FromShell::Unmute(agent.clone())),
        );
        assert_eq!(
            step(
                &mut daemon,
                Input::FromShell(FromShell::Session {
                    agent: agent.clone(),
                    event: detached.clone(),
                })
            ),
            vec![
                Output::Answer {
                    call: CallId(1),
                    response: Response::Session(detached.clone()),
                },
                Output::Answer {
                    call: CallId(2),
                    response: Response::Session(detached),
                },
            ]
        );

        let mut removed = Vec::new();
        daemon.removed_for_good(&agent, wire::RemovalReason::Retention, &mut removed);
        assert_eq!(
            removed,
            vec![
                Output::EndCall {
                    call: CallId(1),
                    status: Status::Ok,
                },
                Output::EndCall {
                    call: CallId(2),
                    status: Status::Ok,
                },
            ]
        );
        assert_eq!(daemon.answers.open, BTreeMap::new(), "no stream is left");
        assert_eq!(daemon.answers.muted, BTreeSet::new(), "and no mute");
    }
}
