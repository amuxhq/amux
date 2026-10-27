//! The pure session and fleet model both amux clients reduce their streams
//! into.
//!
//! A [`SessionState`] is one open chat: the agent's inventory entry, its
//! newest Snapshot decoded by the thin per-kind layer in [`body`], the
//! [`Transcript`] window of items by order, and the [`Inputs`] this client
//! sent. A [`FleetState`] is the inventory: hosts, agent rows and families.
//! Neither does I/O, reads a clock or holds a handle, and neither is ever
//! persisted: the local runtime can re-serve every record they are built
//! from. The driver in ui-runtime owns the streams and forwards each event
//! as a [`Msg`]; the views in ui-view read the state.

pub mod body;
mod fleet;
mod inputs;
mod session;
mod transcript;

pub use body::{AgentState, Fold, ItemBody, ItemClass, OpenAsk, ToolFacts, Usage, decode_snapshot};
pub use fleet::{
    AgentKey, Attention, ExitCause, Families, FleetMsg, FleetState, HostId, agent_key, attention,
    exit_cause, exit_words, parent_key,
};
pub use inputs::{
    InputId, InputOutcome, InputState, InputWhat, Inputs, Refusal, RefusalReason, SentInput,
    refusal, refusal_reason,
};
pub use model::Key;
pub use session::{
    Activity, ActivityKind, BlobStatus, Composer, Connection, Msg, Outcome, PhaseView, QueueRow,
    SessionState, Waiting,
};
pub use transcript::{Held, Run, RunIndex, Transcript};
