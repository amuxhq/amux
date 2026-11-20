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

pub mod acp;
mod ask_item;
pub mod checks;
mod claude_answers;
mod claude_common;
pub mod claude_pty;
pub mod claude_sdk;
mod claude_subagent;
pub mod codex;
mod golden;
mod notice;
pub mod record;
pub mod redact;
pub mod ring;
mod serde_pb;
pub mod shape;
mod shared;
pub mod skeleton;
mod subagent;
pub mod unknown;

#[cfg(any(test, feature = "testkind"))]
pub mod testkind;

pub use claude_common::{FileStep, HOOK_ANSWER_FIELD, NOTEBOOK_OLD_SOURCE, file_step, tool_input};
pub use golden::{
    Committer, EndRules, FixtureInput, FixtureScript, GoldenReport, Replayed, UPDATE_GOLDENS_ENV,
    acp_input, check_invariants, claude_pty_input, claude_sdk_input, codex_input, fixture_script,
    render_item, render_steps, replay, run_golden, run_until,
};
pub use redact::{redact_text, structure_message};
use serde::Serialize;
use serde::de::DeserializeOwned;
pub use serde_pb::{from_hex, to_hex};
pub use shared::{
    AMUX_TOOL_SERVER, Asks, Emit, ItemDraft, OUTPUT_CAP, OpenAsk, OpenTurn, Output, Queue,
    SEND_TOOL, STATUS_TOOL, SendOutcome, Shared, TurnCounter, Wakeup, agent_message_body,
    agent_message_key, human, is_attach_tool, is_send_tool, is_status_tool, output_cut,
    sent_message, status_working_on, without_terminal_codes,
};
pub use subagent::{Subagent, subagent_id};
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

    /// The snapshot body with every field at its explicit unknown.
    fn unknown_snapshot() -> Vec<u8>;

    /// The envelope ids of agent messages accepted but not yet consumed by
    /// the provider, in id order.
    fn pending_messages(state: &Self::State) -> Vec<Vec<u8>>;

    /// A prompt was typed that the provider has not recorded, where its
    /// record starts the turn: in flight, though the phase reads idle and
    /// nothing is queued, so a one-shot agent waits for it too.
    fn typed_unrecorded(state: &Self::State) -> bool;

    /// What the provider process holds that ending it now would lose or
    /// cut short: an agent suspends only while it holds nothing.
    fn holds(state: &Self::State) -> Holds;
    /// When the interpreter next wants a [`Event::Tick`] that no other
    /// event would bring: the agent process arms a timer for it and feeds
    /// the tick when it fires. None asks for nothing.
    fn wake_at(state: &Self::State) -> Option<i64> {
        let _ = state;
        None
    }
    /// Where the housekeeping the interpreter does on its own while the
    /// agent sits idle (an idle compaction) stands in this idle stretch:
    /// an agent suspends only once it is settled, so a suspension never
    /// cuts it short or comes before it.
    fn housekeeping(state: &Self::State) -> Housekeeping {
        let _ = state;
        Housekeeping::Settled
    }

    /// Reads an item body for goldens and invariants.
    fn describe_item(body: &[u8]) -> ItemView;

    /// What the agent's record keeps of a provider fact. No default: each
    /// kind says which reader its provider's facts need.
    fn fact_shape(fact: &Fact) -> shape::FactShape;

    /// What the agent's record keeps of a person's input: its arm, and the
    /// ask or queued input it names.
    fn input_shape(input: &Input) -> shape::InputShape;

    /// What the agent's record keeps of an item body: its arm, and whether
    /// it can still grow.
    fn item_shape(body: &[u8]) -> shape::ItemShape {
        let view = Self::describe_item(body);
        shape::ItemShape {
            arm: view.arm,
            complete: view.complete,
        }
    }

    /// What the agent's record keeps of the state: what is open and
    /// pending, by key.
    fn structure(state: &Self::State) -> shape::Structure;

    /// Reads a snapshot body for goldens and invariants.
    fn describe_snapshot(body: &[u8]) -> SnapshotView;

    /// The wire input an authored fixture's input stands for. None for an
    /// input this kind has no arm for.
    fn fixture_input(input_id: Vec<u8>, input: &FixtureInput) -> Option<Input>;

    /// Turns a recorded provider corpus into events, for recorded fixtures.
    /// A recording knows a provider subagent only by the provider's id for
    /// it, so a person's input to one comes back as [`Event::ChildInput`]
    /// whose `agent_id` is that id's UTF-8: the harness addresses it under
    /// the fixture's agent, as it does an authored `child_input`.
    fn recording(format: &str, bytes: &[u8]) -> Result<Vec<Event>, String> {
        let _ = bytes;
        Err(format!(
            "{:?} has no reader for recordings in format {format:?}",
            Self::KIND
        ))
    }
}

/// What a provider process holds that ending it would lose or cut short,
/// as its interpreter last tracked it: what keeps an idle agent from
/// suspending.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Holds {
    /// Jobs it runs in the background, which a kill would end or orphan.
    pub background_jobs: usize,
    /// Subagents it runs as threads of its own (Codex's), which a kill
    /// would end.
    pub subagents: usize,
    /// A background job ended between turns, and the provider reports it
    /// in a turn of its own that has not started yet.
    pub turn_owed: bool,
    /// Wake-ups it scheduled and has not run, which live only in the
    /// process.
    pub wakeups: usize,
    /// amux granted it something for the rest of its session, which no
    /// resume restores.
    pub session_grant: bool,
    /// The provider cannot reopen its conversation in a new process, so
    /// ending this one ends the conversation (an ACP agent that offers
    /// neither to resume nor to load a session).
    pub unresumable: bool,
}

impl Holds {
    /// Nothing that ending the process would lose or cut short.
    pub fn nothing(&self) -> bool {
        *self == Holds::default()
    }
}

/// The housekeeping an interpreter does on its own in an idle stretch, an
/// idle compaction, as it stands. It runs at most once a stretch, and a
/// stretch ends only with the agent's next turn: once settled, it stays
/// settled until then.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Housekeeping {
    /// It starts at this time, by the interpreter's clock, if the agent is
    /// still idle then. The interpreter's [`Interpreter::wake_at`] asks to
    /// hear that time.
    Due { at_ms: i64 },
    /// It runs.
    Running,
    /// Nothing more this stretch: it ran, or there is nothing worth doing
    /// (a small conversation, a cache that has gone cold or is not one that
    /// would outlive the wait).
    Settled,
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
    /// A person's input to one of the provider subagents this agent's
    /// provider runs, named by the subagent's agent id
    /// ([`subagent_id`]): an answer to one of its asks, or a stop of its
    /// own job. The body is of this agent's kind, which is the subagent's.
    /// The interpreter answers the provider for the subagent; the reply is
    /// keyed by the input id, as for [`Event::Input`].
    ChildInput {
        agent_id: Vec<u8>,
        input: Input,
    },
    /// An agent message with the envelope its daemon set: who it is from
    /// is the daemon's word, never a client's. The reply is keyed by the
    /// envelope id.
    AgentMessage(Envelope),
    /// The daemon's word that the agent was renamed, which ends its title.
    /// Codex's thread takes the name, so Codex's own app shows what amux
    /// does.
    Rename {
        name: String,
    },
    /// The clock, as a fact: the agent process sends one before each event
    /// whose time matters, and when the time [`Interpreter::wake_at`] asked
    /// for comes.
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
/// exit code or signal as the detail. A clean exit is not a failure: it is
/// how a person quits a terminal provider.
pub fn provider_exit(code: Option<i32>) -> wire::IncarnationEnd {
    if code == Some(0) {
        return wire::IncarnationEnd {
            cause: wire::ExitCause::Quit as i32,
            detail: None,
        };
    }
    wire::IncarnationEnd {
        cause: wire::ExitCause::ProviderExited as i32,
        detail: Some(match code {
            Some(code) => format!("{}{code}", wire::PROVIDER_EXIT_CODE),
            None => wire::PROVIDER_SIGNALLED.to_owned(),
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
    /// A row of one of terminal Claude's subagent transcript files, one per
    /// subagent its Agent tool started. Each row names its subagent
    /// (`agentId`), so the fact needs no more than the row. Apart from
    /// `Transcript` because the rows are a subagent's conversation, not the
    /// agent's own. A facts ring from before this channel has none.
    SubagentTranscript,
    /// A hook payload on private/hooks.sock.
    Hook,
    /// A line of stream-JSON from headless Claude's stdout.
    Stream,
    /// A JSON-RPC message from the Codex app server, or from an ACP agent.
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
    /// The first of [`checks::CHECKS`] the step broke.
    pub broken: Option<checks::Violation>,
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
    /// A line for the agent's own log, which a debug report carries:
    /// something the provider said that is for amux's developers and no
    /// client draws.
    Log { line: String },
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
    /// Claude in a terminal: the subagent Claude calls `agent_id` started.
    /// The agent process follows its own transcript, beside the followed
    /// session's (`<session>/subagents/agent-<id>.jsonl`), from where an
    /// earlier follower stopped: it reports the subagent's `meta.json`
    /// first, then each row on the subagent transcript channel, and says so
    /// if the file cannot be read.
    FollowSubagent { agent_id: String },
    /// Claude in a terminal: the subagent's parent learned it ended. The
    /// agent process reads its file to its end and says it has: at once
    /// with no `last_reply`, else once it has read the row that says it,
    /// or after a bounded moment without it.
    DrainSubagent {
        agent_id: String,
        last_reply: Option<String>,
    },
    /// Headless Claude or an ACP agent: the provider's session is now `id`,
    /// which the next incarnation resumes (and, for Claude, later user
    /// messages name).
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
    /// An ACP agent: a `session/prompt` request whose prompt carries
    /// attachments. The agent process appends each attachment to the
    /// request's prompt as a content block (an image blob as an image block,
    /// inline text and files as embedded resources, or as text where the
    /// agent offers no embedded context) and writes the request.
    AcpPrompt {
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
