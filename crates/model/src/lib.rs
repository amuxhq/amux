//! The plain values the amux views and the phone bridge share.
//!
//! The session model computes them, the views put them in what they return,
//! and the bridge hands them to Swift, whose mirrors are generated from
//! these definitions through their schemas, so there is one definition of
//! each. Nothing here is protobuf, does I/O or knows an agent's kind; the
//! constructors that read wire records live beside the session model.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// An item key: the row id, which never moves.
pub type Key = String;

pub type HostId = Vec<u8>;

/// A client-generated input id.
pub type InputId = Vec<u8>;

/// An agent is named by its host and its id: a parent may live elsewhere.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub struct AgentKey {
    pub host: HostId,
    pub agent: Vec<u8>,
}

/// How loudly an agent asks for the person, quietest first. A family is as
/// loud as its loudest member.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub enum Attention {
    Exited,
    Idle,
    Starting,
    Working,
    NeedsYou,
}

/// The client's connection to its local runtime, as the driver reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Connection {
    #[default]
    Connecting,
    Live,
    /// The stream ended; the driver is re-tailing.
    Reconnecting,
}

/// An attachment's bytes in the driver's cache; rows hold a placeholder from
/// the reference's name, type and size until they are ready.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum BlobStatus {
    #[default]
    Missing,
    Fetching,
    Ready,
    Failed(String),
}

/// What the composer offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Composer {
    Send,
    /// The agent has exited: the draft goes through ResumeAgent as the new
    /// incarnation's first prompt, one tap.
    Resume,
    /// Drafting continues; sending waits.
    Disabled(Waiting),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Waiting {
    /// Rows are painted but the origin has not been reached yet.
    CatchingUp,
    /// The origin is not being followed; rows may be stale.
    Detached,
    /// The local runtime is being reconnected.
    Reconnecting,
}

/// The header's phase: lifecycle from the entry, the rest from the snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum PhaseView {
    Starting,
    Idle,
    Working,
    NeedsYou,
    Exited { cause: ExitCause },
}

/// How an agent ended, as every client tells it: it said it was done (a
/// one-shot agent whose turn ended), it was stopped or exited cleanly, or
/// it ended some other way, with the host's own account of why, to show as
/// it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ExitCause {
    Finished,
    Ended,
    Failed(String),
}

/// The line above the composer while the agent works. Not a row; timed
/// against item timestamps with the caller's clock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Activity {
    pub kind: ActivityKind,
    pub since_ms: i64,
    pub elapsed_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ActivityKind {
    /// Busy and nothing more specific applies.
    Working,
    /// A thinking block is open.
    Thinking,
    /// A tool is in flight; the key names the call.
    Running {
        key: Key,
    },
    /// The agent waits on its subagents.
    Subagents {
        count: u32,
    },
    Compacting,
    Retrying {
        attempt: u32,
        max_attempts: u32,
        retry_at_ms: Option<i64>,
    },
}

/// An input as the client that sent it observes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum InputState {
    /// In flight, or accepted and waiting for its reflection. The optimistic
    /// row exists.
    Sent,
    /// Accepted into the agent's queue; listed by every snapshot until it is
    /// submitted or withdrawn.
    Queued,
    /// Accepted and, for a prompt, reflected by an item.
    Settled,
    Rejected(Refusal),
    /// The connection dropped before a reply. Resolved only at CaughtUp from
    /// the snapshot's queue and the items; one found in neither stays here
    /// and the person decides. Nothing is resent on its own.
    Uncertain,
}

/// Why the agent or its host refused an input, as every client tells it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Refusal {
    pub reason: RefusalReason,
    /// The agent's or its provider's own words, when it has any; a client
    /// says them in place of the reason's.
    pub detail: Option<String>,
}

impl Refusal {
    /// A refusal that has no words of its own.
    pub fn of(reason: RefusalReason) -> Self {
        Self {
            reason,
            detail: None,
        }
    }
}

/// The reasons the wire's refusals carry, one for one: the agent had exited
/// or was exiting, its host is shutting down or cannot be reached, this
/// agent cannot take an input of the kind, the ask the answer names had
/// closed, the queued prompt it names was no longer queued, or terminal
/// Claude's own view is on another conversation. A reason this build does
/// not know is unspecified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RefusalReason {
    Unspecified,
    Exited,
    Exiting,
    Draining,
    HostUnreachable,
    Unsupported,
    ClosedAsk,
    NotQueued,
    ConversationElsewhere,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shared_value_has_a_schema_for_the_swift_mirrors() {
        let schema = schemars::schema_for!((
            AgentKey, Attention, Connection, BlobStatus, Composer, PhaseView, ExitCause, Activity,
            InputState, Refusal
        ));
        let text = serde_json::to_string(&schema).unwrap();
        for name in [
            "AgentKey",
            "Attention",
            "Composer",
            "Waiting",
            "ActivityKind",
        ] {
            assert!(text.contains(name), "{name} is missing from {text}");
        }
    }

    #[test]
    fn values_round_trip_through_their_serialized_form() {
        let values = vec![
            Composer::Disabled(Waiting::Detached),
            Composer::Resume,
            Composer::Send,
        ];
        let text = serde_json::to_string(&values).unwrap();
        let back: Vec<Composer> = serde_json::from_str(&text).unwrap();
        assert_eq!(back, values);
        let activity = Activity {
            kind: ActivityKind::Retrying {
                attempt: 2,
                max_attempts: 5,
                retry_at_ms: None,
            },
            since_ms: 10,
            elapsed_ms: 3,
        };
        let text = serde_json::to_string(&activity).unwrap();
        assert_eq!(serde_json::from_str::<Activity>(&text).unwrap(), activity);
    }
}
