//! The per-kind interpreters: the only place provider facts are read and the
//! only code, besides the client's thin per-kind layer, that decodes an item
//! or snapshot body.
//!
//! An interpreter is pure. A provider fact, an input or a tick goes in; a
//! journal [`Step`] and the [`Effect`]s the agent process performs come out.
//! There is no clock (time arrives as [`Event::Tick`]), no id generation
//! (ids arrive on inputs and facts) and no I/O. The kind-neutral pieces every
//! interpreter shares live in [`Shared`]; the golden harness that replays
//! recorded and authored facts through an interpreter is [`run_golden`].

#![forbid(unsafe_code)]

mod ask_item;
mod claude_answers;
mod claude_common;
pub mod claude_pty;
pub mod claude_sdk;
pub mod codex;
mod golden;
pub mod redact;
pub mod ring;
mod serde_pb;
mod shared;
pub mod unknown;

#[cfg(any(test, feature = "testkind"))]
pub mod testkind;

pub use claude_common::{FileStep, HOOK_ANSWER_FIELD, NOTEBOOK_OLD_SOURCE, file_step, tool_input};
pub use golden::{
    EndRules, FixtureInput, FixtureScript, GoldenReport, Replayed, UPDATE_GOLDENS_ENV,
    check_invariants, claude_pty_input, claude_sdk_input, codex_input, fixture_script, render_item,
    render_steps, replay, run_golden, run_until,
};
pub use redact::{redact, redact_message, redact_text};
use serde::Serialize;
use serde::de::DeserializeOwned;
pub use serde_pb::{from_hex, to_hex};
pub use shared::{
    AMUX_TOOL_SERVER, Asks, Emit, ItemDraft, OUTPUT_CAP, OpenAsk, OpenTurn, Output, Queue,
    SEND_TOOL, STATUS_TOOL, SendOutcome, Shared, TurnCounter, agent_message_body,
    agent_message_key, human, is_attach_tool, is_send_tool, is_status_tool, output_cut,
    sent_message, status_working_on,
};
use wire::{AgentSpec, Envelope, Input, SendInputResponse, Step};

/// One interpreter per agent kind.
pub trait Interpreter {
    /// Everything the interpreter holds; written as the checkpoint at facts
    /// ring rotation and resumed from on restart.
    type State: Checkpoint;

    /// The kind its items and snapshots carry.
    const KIND: wire::Kind;

    /// The state an agent starts in and the first journal frame: a Snapshot
    /// with phase starting and every field at its explicit unknown.
    fn initial(spec: &AgentSpec) -> (Self::State, Step);

    /// One event in; the step to journal and the effects to perform out.
    fn step(state: &mut Self::State, event: Event) -> Stepped;

    /// Continues a checkpointed state in the agent's next incarnation under
    /// `spec`: the spec's prompt joins the queue, what belonged to the
    /// previous provider process starts over, and every counter behind an
    /// item key carries on, so the new incarnation never reuses a key. The
    /// step it starts with re-emits every open item in full on its existing
    /// key, and the snapshot.
    fn reincarnate(state: Self::State, spec: &AgentSpec) -> (Self::State, Step);

    /// Removes secrets from a body, a facts-ring entry or a checkpoint while
    /// keeping its structure; the daemon calls it (through [`redact()`]) on a
    /// store slice at dump time and the agent process on its own dump part.
    fn redact(target: RedactTarget) -> RedactTarget;

    /// The snapshot body with every field at its explicit unknown.
    fn unknown_snapshot() -> Vec<u8>;

    /// The envelope ids of agent messages accepted but not yet consumed by
    /// the provider, in id order.
    fn pending_messages(state: &Self::State) -> Vec<Vec<u8>>;

    /// A prompt was typed that the provider has not recorded, where its
    /// record starts the turn: in flight, though the phase reads idle and
    /// nothing is queued, so a one-shot agent waits for it too.
    fn typed_unrecorded(state: &Self::State) -> bool;

    /// Reads an item body for goldens and invariants.
    fn describe_item(body: &[u8]) -> ItemView;

    /// Reads a snapshot body for goldens and invariants.
    fn describe_snapshot(body: &[u8]) -> SnapshotView;

    /// The wire input an authored fixture's input stands for. None for an
    /// input this kind has no arm for.
    fn fixture_input(input_id: Vec<u8>, input: &FixtureInput) -> Option<Input>;

    /// Turns a recorded provider corpus into events, for recorded fixtures.
    fn recording(format: &str, bytes: &[u8]) -> Result<Vec<Event>, String> {
        let _ = bytes;
        Err(format!(
            "{:?} has no reader for recordings in format {format:?}",
            Self::KIND
        ))
    }
}

/// What an interpreter consumes. One event is in hand at a time, so the
/// input variant's size costs nothing worth a box.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Fact(Fact),
    /// A person's input, its body as the client encoded it. The reply is
    /// keyed by its input id.
    Input(Input),
    /// An agent message with the envelope its daemon set: who it is from
    /// is the daemon's word, never a client's. The reply is keyed by the
    /// envelope id.
    AgentMessage(Envelope),
    /// The daemon's word that the agent was renamed. Only Codex hears it:
    /// its thread takes the name, so Codex's own app shows what amux does.
    Rename {
        name: String,
    },
    /// The clock, as a fact: the agent process sends one before each event
    /// whose time matters and on a timer while a quiet period is watched.
    Tick {
        at_ms: i64,
    },
    ProviderExit {
        code: Option<i32>,
    },
    /// The grace after ctl.sock closed ran out with no daemon back: the
    /// agent drains. The interpreter writes a daemon-lost boundary.
    DaemonLost,
    /// A stop from the daemon: the running turn is cancelled the
    /// provider's own way, as the interrupt input does, before the agent
    /// exits. A kill reaches no interpreter.
    StopRequested,
    /// The agent process is ending this incarnation for `cause` (a drain,
    /// a stop, the one-shot rule): the final boundary, open asks closed.
    /// The provider's own exit that follows is not reported.
    Exiting(wire::IncarnationEnd),
    /// What the agent's folder says about its repository, read by the agent
    /// process at start and at each turn end; None outside a repository.
    Git(Option<wire::Git>),
}

/// The cause the final boundary gives for a provider's own exit, with its
/// exit code or signal as the detail.
pub fn provider_exit(code: Option<i32>) -> wire::IncarnationEnd {
    wire::IncarnationEnd {
        cause: wire::ExitCause::ProviderExited as i32,
        detail: Some(match code {
            Some(code) => format!("exit code {code}"),
            None => "killed by a signal".to_owned(),
        }),
    }
}

/// One provider fact exactly as it arrived; what the facts ring records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    pub channel: Channel,
    pub payload: Vec<u8>,
}

/// Where a fact came from. Each kind reads the channels its provider has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// A row of Claude's transcript file.
    Transcript,
    /// A hook payload on private/hooks.sock.
    Hook,
    /// A line of stream-JSON from headless Claude's stdout.
    Stream,
    /// A JSON-RPC message from the Codex app server.
    Rpc,
    /// What the agent process itself observed about the provider it
    /// launched: for Claude in a terminal, the version it found.
    Agent,
}

/// A step's output.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stepped {
    pub step: Step,
    pub effects: Vec<Effect>,
}

/// What the agent process does on an interpreter's behalf, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Bytes to the provider's one input channel: the PTY, stdin or the
    /// app-server connection.
    ProviderWrite(Vec<u8>),
    /// The verdict on an input or an agent message, keyed by the input id
    /// or the envelope id: the daemon relays an input's as the SendInput
    /// reply and settles a message's delivery by it.
    Reply {
        input_id: Vec<u8>,
        verdict: SendInputResponse,
    },
    /// Hand an agent message to the provider's own injection channel: its
    /// envelope id, the text the model reads, label and all, and a
    /// message's attachments.
    Inject {
        id: Vec<u8>,
        text: String,
        attachments: Vec<wire::Attachment>,
        via: Carrier,
    },
    /// The agent process should exit with this cause.
    Exit { cause: String },
    /// Write `bytes` to the agent's blob directory as `blobs/<hex of
    /// hash>`, the SHA-256 of the bytes: an image the model read, which an
    /// item in this step references, or the encoded `wire::Catalogue` the
    /// snapshot in this step names. The agent process writes it before it
    /// journals the step.
    WriteBlob { hash: Vec<u8>, bytes: Vec<u8> },
    /// Claude in a terminal: a semantic input the agent process types into
    /// the PTY.
    Terminal(claude_pty::TerminalInput),
    /// Claude in a terminal: the output of the hook Claude waits on whose
    /// payload the agent numbered `id`, which Claude reads as the hook's
    /// decision. Empty, it releases the hook without one.
    HookAnswer { id: u64, answer: Vec<u8> },
    /// Headless Claude: a user message on stdin carrying `uuid`, which
    /// Claude echoes on everything it reports about the message. The agent
    /// process encodes the attachments as content blocks.
    UserMessage {
        uuid: String,
        text: String,
        attachments: Vec<wire::Attachment>,
    },
    /// Claude in a terminal: a session started on this transcript file;
    /// the agent process tails it from now on.
    FollowTranscript { path: String },
    /// Headless Claude: the provider's session is now `id`, which the next
    /// incarnation resumes and later user messages name.
    ProviderSession { id: String },
    /// Codex: a `turn/start`, `turn/steer` or `thread/inject_items`
    /// request whose input carries attachments. The agent process appends
    /// each attachment as a Codex input item (an image blob by its path in
    /// the agent's blob directory, inline text and files as text; an
    /// injected message's as its element) and writes the request.
    CodexTurnInput {
        request: Vec<u8>,
        attachments: Vec<wire::Attachment>,
    },
}

/// The provider's own injection channel for agent messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carrier {
    /// Claude in a PTY: the messaging socket.
    MessagingSocket,
    /// Headless Claude: a user message on stdin.
    Stdin,
}

/// What [`Interpreter::redact`] works on.
#[derive(Clone, Debug, PartialEq)]
pub enum RedactTarget {
    /// An `Item.body` of the kind.
    ItemBody(Vec<u8>),
    /// A `Snapshot.body` of the kind.
    SnapshotBody(Vec<u8>),
    /// A facts-ring entry that is a provider fact.
    Fact(Fact),
    /// A facts-ring entry that is an input: an encoded `wire::Input`.
    Input(Vec<u8>),
    /// A facts-ring entry that is an agent message: an encoded
    /// `wire::Envelope`.
    AgentMessage(Vec<u8>),
    /// An encoded checkpoint of the kind's state.
    Checkpoint(Vec<u8>),
    /// An encoded `wire::AgentSpec`: its environment values and prompt.
    Spec(Vec<u8>),
    /// An encoded `wire::Step` of the kind: its items, appends and
    /// snapshot, bodies included. A journal frame is one; so is a slice of
    /// the store written as one step.
    Step(Vec<u8>),
    /// An encoded `wire::Agent`, the inventory row: its name, directory,
    /// what it is working on and its exit cause.
    Agent(Vec<u8>),
    /// An encoded `wire::dump::ClientSessionHistory` of the kind: a client's
    /// session state and the messages since, with the items, snapshots and
    /// inputs inside them, bodies included.
    ClientSession(Vec<u8>),
}

/// An item body as the harness and goldens read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemView {
    /// The body's oneof arm; a key never changes arm.
    pub arm: String,
    /// No append may follow.
    pub complete: bool,
    /// A one-line rendering for goldens.
    pub text: String,
}

/// A snapshot body as the harness and goldens read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotView {
    /// Open asks as (ask key, item key).
    pub asks: Vec<(String, String)>,
    pub text: String,
}

/// Interpreter state that can be written at ring rotation and resumed.
pub trait Checkpoint: Serialize + DeserializeOwned + Sized {
    /// The state to continue from and the step a resume starts with: every
    /// open item in full on its existing key, and the snapshot.
    fn resume(self) -> (Self, Step);
}

pub fn encode_checkpoint<S: Checkpoint>(state: &S) -> Vec<u8> {
    serde_json::to_vec(state).expect("interpreter state serializes")
}

pub fn decode_checkpoint<S: Checkpoint>(bytes: &[u8]) -> Result<S, serde_json::Error> {
    serde_json::from_slice(bytes)
}

/// SendInput verdicts.
pub mod reply {
    use wire::{Accepted, Refusal, Rejected, SendInputResponse, send_input_response};

    pub fn accepted(queued: bool) -> SendInputResponse {
        SendInputResponse {
            of: Some(send_input_response::Of::Accepted(Accepted { queued })),
        }
    }

    pub fn rejected(refusal: Refusal) -> SendInputResponse {
        refused(Rejected {
            refusal: refusal as i32,
            detail: None,
        })
    }

    /// A refusal that may carry the interpreter's or provider's own words.
    pub fn refused(rejected: Rejected) -> SendInputResponse {
        SendInputResponse {
            of: Some(send_input_response::Of::Rejected(rejected)),
        }
    }

    /// An input this agent cannot take, said in its own words.
    pub fn unsupported_saying(detail: impl Into<String>) -> Rejected {
        Rejected {
            refusal: Refusal::Unsupported as i32,
            detail: Some(detail.into()),
        }
    }
}
