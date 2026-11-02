//! A session's record: its history as a dump carries it with nothing
//! anybody wrote. The session model reads a row's key, order, revision,
//! kind, input id, attachment hashes and time, and of its body only what
//! [`crate::body::item_structure`] keeps; it reads a snapshot's envelope and
//! what [`crate::body::snapshot_structure`] keeps; it reads an input's arm
//! and the keys and ids it names. Everything else is emptied, so the record
//! replays through the same model to the same windows, runs, inputs and
//! failed checks as the whole history, and holds no text, name or path.

use wire::dump::{
    ClientBlob, ClientCatalogue, ClientInput, ClientPage, ClientSent, ClientSession,
    ClientSessionEvents, ClientSessionHistory, ClientSessionMsg, ClientTraced, ClientTranscript,
    client_sent, client_session_msg, client_traced,
};
use wire::{
    Agent, Attachment, HostEntry, Input, Item, Kind, QueuedInput, SendInputResponse, SessionEvent,
    Snapshot, attachment, host_entry, send_input_response, session_event,
};

use crate::body::{item_structure, named_kind, snapshot_structure};

/// The record of a session's history: the same checkpoint and events, each
/// event's size kept, with every body, text, name and path emptied.
pub fn record(history: ClientSessionHistory) -> ClientSessionHistory {
    let kind = history
        .start
        .as_ref()
        .and_then(|start| start.agent.as_ref())
        .map_or(Kind::Unspecified, Agent::kind);
    ClientSessionHistory {
        agent_id: history.agent_id,
        start: history.start.map(|start| session(kind, start)),
        events: history.events.map(|events| ClientSessionEvents {
            traced: events
                .traced
                .into_iter()
                .map(|traced| ClientTraced {
                    event: traced.event.map(|event| match event {
                        client_traced::Event::Msg(msg) => {
                            client_traced::Event::Msg(message(kind, msg))
                        }
                        // The driver's own acts name keys, counts and its
                        // errors; nothing in them comes from a body.
                        driver @ client_traced::Event::Driver(_) => driver,
                    }),
                    ..traced
                })
                .collect(),
        }),
        structure_only: true,
    }
}

fn session(kind: Kind, start: ClientSession) -> ClientSession {
    let items = |items: Vec<Item>| items.into_iter().map(|held| item(kind, held)).collect();
    let window = |held: ClientTranscript| ClientTranscript {
        items: items(held.items),
        exhausted: held.exhausted,
    };
    ClientSession {
        agent: start.agent.map(agent),
        host: start.host.map(host),
        snapshot: start.snapshot.map(|held| snapshot(kind, held)),
        catalogue: start.catalogue.map(catalogue),
        transcript: start.transcript.map(window),
        pending: start.pending.map(window),
        inputs: start
            .inputs
            .into_iter()
            .map(|held| ClientInput {
                input: held.input.map(input),
                rejected: held.rejected.map(rejected),
                ..held
            })
            .collect(),
        blobs: start.blobs.into_iter().map(blob).collect(),
        arrivals: items(start.arrivals),
        ..start
    }
}

fn message(kind: Kind, msg: ClientSessionMsg) -> ClientSessionMsg {
    use client_session_msg::Of;
    ClientSessionMsg {
        of: msg.of.map(|of| match of {
            Of::Event(held) => Of::Event(event(kind, held)),
            Of::Page(page) => Of::Page(ClientPage {
                items: page
                    .items
                    .into_iter()
                    .map(|held| item(kind, held))
                    .collect(),
                ..page
            }),
            Of::Send(held) => Of::Send(input(held)),
            Of::Sent(sent) => Of::Sent(ClientSent {
                outcome: sent.outcome.map(|outcome| match outcome {
                    client_sent::Outcome::Reply(reply) => {
                        client_sent::Outcome::Reply(reply_of(reply))
                    }
                    lost @ client_sent::Outcome::Lost(_) => lost,
                }),
                ..sent
            }),
            Of::Entry(held) => Of::Entry(agent(held)),
            Of::Host(held) => Of::Host(host(held)),
            Of::Blob(held) => Of::Blob(blob(held)),
            Of::Catalogue(held) => Of::Catalogue(catalogue(held)),
            other @ (Of::Connection(_)
            | Of::DiscardInputId(_)
            | Of::Following(_)
            | Of::Reloading(_)
            | Of::Window(_)) => other,
        }),
    }
}

fn event(kind: Kind, event: SessionEvent) -> SessionEvent {
    use session_event::Of;
    SessionEvent {
        of: event.of.map(|of| match of {
            Of::Snapshot(held) => Of::Snapshot(snapshot(kind, held)),
            Of::Item(held) => Of::Item(item(kind, held)),
            Of::Append(append) => Of::Append(wire::Append {
                key: append.key,
                base_revision: append.base_revision,
                revision: append.revision,
                text: String::new(),
            }),
            other @ (Of::CaughtUp(_)
            | Of::Lagged(_)
            | Of::Reset(_)
            | Of::Detached(_)
            | Of::Opening(_)) => other,
        }),
    }
}

fn item(kind: Kind, item: Item) -> Item {
    let body = item_structure(named_kind(item.kind).unwrap_or(kind), &item.body);
    Item {
        key: item.key,
        order: item.order,
        revision: item.revision,
        input_id: item.input_id,
        text: String::new(),
        attachments: item.attachments.into_iter().map(attachment).collect(),
        kind: item.kind,
        body,
        at_ms: item.at_ms,
    }
}

/// An attachment by its arm and the hash of the bytes it names: what a
/// row redraws on when those bytes arrive.
fn attachment(held: Attachment) -> Attachment {
    use attachment::Of;
    let blob = |blob: wire::BlobRef| wire::BlobRef {
        hash: blob.hash,
        size: blob.size,
        ..Default::default()
    };
    Attachment {
        of: held.of.map(|of| match of {
            Of::Image(held) => Of::Image(blob(held)),
            Of::File(held) => Of::File(blob(held)),
            Of::Text(_) => Of::Text(Default::default()),
            Of::Review(review) => Of::Review(wire::Review {
                diff: review.diff.map(|diff| wire::Diff {
                    patch: diff.patch.map(blob),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        }),
    }
}

fn snapshot(kind: Kind, snapshot: Snapshot) -> Snapshot {
    let body = snapshot_structure(named_kind(snapshot.kind).unwrap_or(kind), &snapshot.body);
    Snapshot {
        revision: snapshot.revision,
        queue: snapshot.queue.into_iter().map(queued).collect(),
        kind: snapshot.kind,
        body,
        phase: snapshot.phase,
        working_on: None,
        at_ms: snapshot.at_ms,
        phase_since_ms: snapshot.phase_since_ms,
        git: None,
        catalogue: snapshot.catalogue,
        background_jobs: snapshot.background_jobs,
    }
}

fn queued(queued: QueuedInput) -> QueuedInput {
    QueuedInput {
        input_id: queued.input_id,
        text: String::new(),
        attachments: Vec::new(),
        steer: queued.steer,
        sender: None,
    }
}

/// An input by its arm, with the ask it answers and the queued input it
/// withdraws or sends now; a prompt's text and attachments are emptied.
fn input(input: Input) -> Input {
    use wire::{KindInput, claude_pty_input as pty, claude_sdk_input as sdk, codex_input as codex};
    let prompt = || wire::PromptInput::default();
    let claude_answer = |answer: wire::ClaudeAnswer| wire::ClaudeAnswer {
        ask_key: answer.ask_key,
        of: None,
    };
    let outline = match input.decode_body() {
        Some(KindInput::ClaudePty(held)) => KindInput::ClaudePty(wire::ClaudePtyInput {
            of: held.of.map(|of| match of {
                pty::Of::Prompt(_) => pty::Of::Prompt(prompt()),
                pty::Of::Answer(answer) => pty::Of::Answer(claude_answer(answer)),
                // Each names the queued input it acts on by its id.
                named @ (pty::Of::Withdraw(_) | pty::Of::SendNow(_)) => named,
                pty::Of::CyclePermissionMode(_) => pty::Of::CyclePermissionMode(Default::default()),
                pty::Of::Clear(_) => pty::Of::Clear(Default::default()),
                pty::Of::Interrupt(_) => pty::Of::Interrupt(Default::default()),
                pty::Of::SwitchBack(_) => pty::Of::SwitchBack(Default::default()),
            }),
        }),
        Some(KindInput::ClaudeSdk(held)) => KindInput::ClaudeSdk(wire::ClaudeSdkInput {
            of: held.of.map(|of| match of {
                sdk::Of::Prompt(_) => sdk::Of::Prompt(prompt()),
                sdk::Of::Answer(answer) => sdk::Of::Answer(claude_answer(answer)),
                // Each names the queued input it acts on by its id.
                named @ (sdk::Of::Withdraw(_) | sdk::Of::SendNow(_)) => named,
                sdk::Of::Interrupt(_) => sdk::Of::Interrupt(Default::default()),
                sdk::Of::Permission(_) => sdk::Of::Permission(Default::default()),
                sdk::Of::Model(_) => sdk::Of::Model(Default::default()),
                sdk::Of::Clear(_) => sdk::Of::Clear(Default::default()),
                sdk::Of::Effort(_) => sdk::Of::Effort(Default::default()),
                sdk::Of::StopJob(_) => sdk::Of::StopJob(Default::default()),
            }),
        }),
        Some(KindInput::Codex(held)) => KindInput::Codex(wire::CodexInput {
            of: held.of.map(|of| match of {
                codex::Of::Prompt(_) => codex::Of::Prompt(prompt()),
                codex::Of::Answer(answer) => codex::Of::Answer(wire::CodexAnswer {
                    ask_key: answer.ask_key,
                    of: None,
                }),
                // Each names the queued input it acts on by its id.
                named @ (codex::Of::Withdraw(_) | codex::Of::SendNow(_)) => named,
                codex::Of::Interrupt(_) => codex::Of::Interrupt(Default::default()),
                codex::Of::Permission(_) => codex::Of::Permission(Default::default()),
                codex::Of::Mode(_) => codex::Of::Mode(Default::default()),
                codex::Of::Model(_) => codex::Of::Model(Default::default()),
                codex::Of::Effort(_) => codex::Of::Effort(Default::default()),
                codex::Of::StopJob(_) => codex::Of::StopJob(Default::default()),
            }),
        }),
        None => {
            return Input {
                input_id: input.input_id,
                kind: input.kind,
                body: Vec::new(),
            };
        }
    };
    Input::new(input.input_id, outline)
}

/// A verdict without the agent's or its provider's own words.
fn reply_of(reply: SendInputResponse) -> SendInputResponse {
    use send_input_response::Of;
    SendInputResponse {
        of: reply.of.map(|of| match of {
            Of::Rejected(held) => Of::Rejected(rejected(held)),
            accepted @ Of::Accepted(_) => accepted,
        }),
    }
}

fn rejected(rejected: wire::Rejected) -> wire::Rejected {
    wire::Rejected {
        refusal: rejected.refusal,
        detail: None,
    }
}

/// The inventory row by its ids, kind and states: no name, folder, task,
/// branch or exit words.
fn agent(agent: Agent) -> Agent {
    Agent {
        agent_id: agent.agent_id,
        host_id: agent.host_id,
        kind: agent.kind,
        name: String::new(),
        cwd: String::new(),
        parent: agent.parent,
        created_at_ms: agent.created_at_ms,
        lifecycle: agent.lifecycle,
        exit_cause: agent.exit_cause,
        exit_detail: None,
        phase: agent.phase,
        working_on: None,
        phase_since_ms: agent.phase_since_ms,
        incarnation: agent.incarnation,
        git: None,
        descendants: agent.descendants,
        background_jobs: agent.background_jobs,
    }
}

/// The host by its id, generation and standing's arm.
fn host(host: HostEntry) -> HostEntry {
    use host_entry::Standing;
    HostEntry {
        host: host.host.map(|held| wire::Host {
            host_id: held.host_id,
            ..Default::default()
        }),
        generation: host.generation,
        providers: Vec::new(),
        standing: host.standing.map(|standing| match standing {
            Standing::Trusted(_) => Standing::Trusted(Default::default()),
            Standing::Candidate(_) => Standing::Candidate(Default::default()),
        }),
    }
}

/// A catalogue by the hash it was fetched by: whether it is the one the
/// newest snapshot names is all the model asks of it.
fn catalogue(held: ClientCatalogue) -> ClientCatalogue {
    ClientCatalogue {
        hash: held.hash,
        catalogue: None,
    }
}

fn blob(held: ClientBlob) -> ClientBlob {
    ClientBlob {
        failed: String::new(),
        ..held
    }
}
