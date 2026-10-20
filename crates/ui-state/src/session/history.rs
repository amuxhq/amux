//! A session and its messages as a dump carries them: wire values a replay
//! reads back into the same state and the same messages. The rows travel as
//! the wire's items, with their orders and revisions; every index over them
//! is rebuilt on the way back, so a window caught broken reads back broken.

use std::collections::BTreeMap;

use wire::{
    ClientBlob, ClientBlobState, ClientCatalogue, ClientConnection, ClientInput, ClientInputState,
    ClientPage, ClientSent, ClientSession, ClientSessionMsg, ClientTranscript, Rejected,
    client_sent, client_session_msg,
};

use super::{Arrivals, Msg, SessionState};
use crate::body::AgentState;
use crate::inputs::{
    InputOutcome, InputState, InputWhat, Inputs, Refusal, RefusalReason, SentInput,
};
use crate::transcript::{Held, Transcript};
use crate::{BlobStatus, Connection};

impl SessionState {
    /// The session as a dump carries it.
    pub fn to_wire(&self) -> ClientSession {
        ClientSession {
            agent: Some(self.agent.clone()),
            host: self.host.clone(),
            snapshot: self.snapshot.clone(),
            catalogue: self
                .catalogue
                .as_ref()
                .map(|(hash, catalogue)| ClientCatalogue {
                    hash: hash.clone(),
                    catalogue: Some(catalogue.clone()),
                }),
            transcript: Some(transcript(&self.transcript)),
            pending: self.pending.as_ref().map(transcript),
            retail: self.retail,
            generation: self.generation,
            inputs: self.inputs.iter().map(input).collect(),
            caught_up: self.caught_up,
            detached: self.detached,
            connection: connection(self.connection) as i32,
            exited_under: self.exited_under,
            reach: self.reach,
            epoch: self.epoch,
            blobs: self
                .blobs
                .iter()
                .map(|(hash, status)| blob(hash, status))
                .collect(),
            cap: self.cap as u64,
            following: self.following,
            arrivals: self
                .arrivals
                .rows
                .values()
                .map(|held| held.item.clone())
                .collect(),
            arrivals_moved_on: self.arrivals.moved_on,
            arrivals_changes: self.arrivals.changes,
            reload_owed: self.reload_owed,
        }
    }

    /// The session a dump carried, as it stood.
    pub fn from_wire(session: ClientSession) -> Result<SessionState, String> {
        let agent = session.agent.ok_or("a session with no agent")?;
        let kind = agent.kind();
        let state = match &session.snapshot {
            Some(snapshot) => AgentState::from_snapshot(kind, snapshot),
            None => AgentState {
                kind,
                ..AgentState::default()
            },
        };
        let restore = |held: Option<ClientTranscript>| {
            let held = held.unwrap_or_default();
            Transcript::restore(kind, held.items, held.exhausted)
        };
        let mut arrivals = Arrivals {
            moved_on: session.arrivals_moved_on,
            changes: session.arrivals_changes,
            ..Arrivals::default()
        };
        for item in session.arrivals {
            arrivals.by_key.insert(item.key.clone(), item.order);
            arrivals.rows.insert(item.order, Held::new(kind, item));
        }
        let inputs = session
            .inputs
            .into_iter()
            .map(sent_input)
            .collect::<Result<Vec<_>, _>>()?;
        let mut restored = SessionState {
            agent,
            host: session.host,
            state,
            snapshot: session.snapshot,
            catalogue: session
                .catalogue
                .map(|held| (held.hash, held.catalogue.unwrap_or_default())),
            transcript: restore(session.transcript),
            pending: session.pending.map(|pending| restore(Some(pending))),
            retail: session.retail,
            generation: session.generation,
            inputs: Inputs::restore(inputs),
            caught_up: session.caught_up,
            detached: session.detached,
            connection: from_connection(session.connection),
            exited_under: session.exited_under,
            reach: session.reach,
            epoch: session.epoch,
            blobs: session
                .blobs
                .into_iter()
                .map(|blob| {
                    let status = blob_status(&blob);
                    (blob.hash, status)
                })
                .collect::<BTreeMap<_, _>>(),
            cap: usize::try_from(session.cap).unwrap_or(usize::MAX).max(1),
            following: session.following,
            arrivals,
            reload_owed: session.reload_owed,
        };
        restored.offer();
        Ok(restored)
    }
}

impl Msg {
    /// The message as a dump carries it.
    pub fn to_wire(&self) -> ClientSessionMsg {
        use client_session_msg::Of;
        let of = match self {
            Msg::Event(event) => Of::Event(event.clone()),
            Msg::Page {
                items,
                exhausted,
                epoch,
            } => Of::Page(ClientPage {
                items: items.clone(),
                exhausted: *exhausted,
                epoch: *epoch,
            }),
            Msg::Connection(state) => Of::Connection(connection(*state) as i32),
            Msg::Send(input) => Of::Send(input.clone()),
            Msg::Sent(id, outcome) => Of::Sent(ClientSent {
                input_id: id.clone(),
                outcome: Some(match outcome {
                    InputOutcome::Reply(reply) => client_sent::Outcome::Reply(reply.clone()),
                    InputOutcome::Lost => client_sent::Outcome::Lost(wire::ClientLost {}),
                    InputOutcome::Held => client_sent::Outcome::Held(wire::ClientHeld {}),
                }),
            }),
            Msg::Discard(id) => Of::DiscardInputId(id.clone()),
            Msg::Entry(agent) => Of::Entry(agent.clone()),
            Msg::Host(host) => Of::Host(host.clone()),
            Msg::Blob { hash, status } => Of::Blob(blob(hash, status)),
            Msg::Following(following) => Of::Following(*following),
            Msg::Reloading => Of::Reloading(wire::ClientReloading {}),
            Msg::Window(cap) => Of::Window(*cap as u64),
            Msg::Catalogue { hash, catalogue } => Of::Catalogue(ClientCatalogue {
                hash: hash.clone(),
                catalogue: Some(catalogue.clone()),
            }),
        };
        ClientSessionMsg { of: Some(of) }
    }

    /// The message a dump carried.
    pub fn from_wire(msg: ClientSessionMsg) -> Result<Msg, String> {
        use client_session_msg::Of;
        Ok(match msg.of.ok_or("a message of no kind")? {
            Of::Event(event) => Msg::Event(event),
            Of::Page(page) => Msg::Page {
                items: page.items,
                exhausted: page.exhausted,
                epoch: page.epoch,
            },
            Of::Connection(state) => Msg::Connection(from_connection(state)),
            Of::Send(input) => Msg::Send(input),
            Of::Sent(sent) => Msg::Sent(
                sent.input_id,
                match sent.outcome.ok_or("a send with no outcome")? {
                    client_sent::Outcome::Reply(reply) => InputOutcome::Reply(reply),
                    client_sent::Outcome::Lost(_) => InputOutcome::Lost,
                    client_sent::Outcome::Held(_) => InputOutcome::Held,
                },
            ),
            Of::DiscardInputId(id) => Msg::Discard(id),
            Of::Entry(agent) => Msg::Entry(agent),
            Of::Host(host) => Msg::Host(host),
            Of::Blob(blob) => Msg::Blob {
                status: blob_status(&blob),
                hash: blob.hash,
            },
            Of::Following(following) => Msg::Following(following),
            Of::Reloading(_) => Msg::Reloading,
            Of::Window(cap) => Msg::Window(usize::try_from(cap).unwrap_or(usize::MAX)),
            Of::Catalogue(held) => Msg::Catalogue {
                hash: held.hash,
                catalogue: held.catalogue.unwrap_or_default(),
            },
        })
    }
}

fn transcript(transcript: &Transcript) -> ClientTranscript {
    ClientTranscript {
        items: transcript.iter().map(|held| held.item.clone()).collect(),
        exhausted: transcript.exhausted(),
    }
}

fn connection(connection: Connection) -> ClientConnection {
    match connection {
        Connection::Connecting => ClientConnection::Connecting,
        Connection::Live => ClientConnection::Live,
        Connection::Reconnecting => ClientConnection::Reconnecting,
    }
}

fn from_connection(connection: i32) -> Connection {
    match ClientConnection::try_from(connection).unwrap_or_default() {
        ClientConnection::Connecting => Connection::Connecting,
        ClientConnection::Live => Connection::Live,
        ClientConnection::Reconnecting => Connection::Reconnecting,
    }
}

fn blob(hash: &[u8], status: &BlobStatus) -> ClientBlob {
    let (state, failed) = match status {
        BlobStatus::Missing => (ClientBlobState::Missing, String::new()),
        BlobStatus::Fetching => (ClientBlobState::Fetching, String::new()),
        BlobStatus::Ready => (ClientBlobState::Ready, String::new()),
        BlobStatus::Failed(why) => (ClientBlobState::Failed, why.clone()),
    };
    ClientBlob {
        hash: hash.to_vec(),
        state: state as i32,
        failed,
    }
}

fn blob_status(blob: &ClientBlob) -> BlobStatus {
    match blob.state() {
        ClientBlobState::Missing => BlobStatus::Missing,
        ClientBlobState::Fetching => BlobStatus::Fetching,
        ClientBlobState::Ready => BlobStatus::Ready,
        ClientBlobState::Failed => BlobStatus::Failed(blob.failed.clone()),
    }
}

fn input(sent: &SentInput) -> ClientInput {
    let (state, rejected) = match &sent.state {
        InputState::Sent => (ClientInputState::Sent, None),
        InputState::Queued => (ClientInputState::Queued, None),
        InputState::Settled => (ClientInputState::Settled, None),
        InputState::Uncertain => (ClientInputState::Uncertain, None),
        InputState::Held => (ClientInputState::Held, None),
        InputState::Rejected(refusal) => (
            ClientInputState::Rejected,
            Some(Rejected {
                refusal: wire_refusal(refusal.reason) as i32,
                detail: refusal.detail.clone(),
            }),
        ),
    };
    ClientInput {
        input: Some(sent.input.clone()),
        state: state as i32,
        rejected,
        seen_queued: sent.seen_queued,
        left_at: sent.left_at,
    }
}

fn sent_input(held: ClientInput) -> Result<SentInput, String> {
    let state = match held.state() {
        ClientInputState::Sent => InputState::Sent,
        ClientInputState::Queued => InputState::Queued,
        ClientInputState::Settled => InputState::Settled,
        ClientInputState::Uncertain => InputState::Uncertain,
        ClientInputState::Held => InputState::Held,
        ClientInputState::Rejected => InputState::Rejected(held.rejected.as_ref().map_or_else(
            || Refusal::of(RefusalReason::Unspecified),
            crate::inputs::refusal,
        )),
    };
    let input = held
        .input
        .ok_or("an input this client sent with no input")?;
    Ok(SentInput {
        id: input.input_id.clone(),
        what: InputWhat::of(&input),
        state,
        input,
        seen_queued: held.seen_queued,
        left_at: held.left_at,
    })
}

/// The wire's refusal for the client's reason.
fn wire_refusal(reason: RefusalReason) -> wire::Refusal {
    match reason {
        RefusalReason::Unspecified => wire::Refusal::Unspecified,
        RefusalReason::Exited => wire::Refusal::Exited,
        RefusalReason::Exiting => wire::Refusal::Exiting,
        RefusalReason::Draining => wire::Refusal::Draining,
        RefusalReason::HostUnreachable => wire::Refusal::HostUnreachable,
        RefusalReason::Unsupported => wire::Refusal::Unsupported,
        RefusalReason::ClosedAsk => wire::Refusal::ClosedAsk,
        RefusalReason::NotQueued => wire::Refusal::NotQueued,
        RefusalReason::ConversationElsewhere => wire::Refusal::ConversationElsewhere,
        RefusalReason::TooLong => wire::Refusal::TooLong,
    }
}
