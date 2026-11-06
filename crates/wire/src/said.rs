//! What an agent wants of the person, in the plain form its readers share.
//!
//! The fleet row and a push say the same thing, so they read one type. A
//! protobuf `Notice` is what travels; this is what a reader words, on any
//! client and in a notification extension with no protobuf in it, whose
//! mirror is generated from this definition.

use schemars::JsonSchema;
use serde::Serialize;

use crate::{ExitCause, Notice, ask_notice, blocked_notice, notice};

impl ExitCause {
    /// Whether the agent ended in a way nobody meant. A cause this build
    /// doesn't know is not failure.
    pub fn failed(cause: i32) -> bool {
        !matches!(
            Self::try_from(cause).unwrap_or_default(),
            Self::Unspecified | Self::Finished | Self::Stopped | Self::Killed | Self::Quit
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum Said {
    /// The head ask.
    Ask {
        /// What an answer would name.
        key: String,
        wants: Wants,
        /// The provider's reason, when it gave one.
        why: String,
        /// How many other asks are open.
        more: u32,
    },
    /// A turn stopped because the provider isn't signed in.
    SignedOut,
    /// A turn stopped at a spent usage window; it resets at the latest reset
    /// of the windows that are, when the provider says.
    UsageLimit { resets_at_ms: Option<i64> },
    /// The provider's words for why its turn failed.
    Failed { message: String },
    /// The process ended unexpectedly: pushed, never on a row, which has the
    /// exit from the inventory.
    Died { detail: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum Wants {
    Run {
        command: String,
    },
    Change {
        path: String,
        files: u32,
        /// Whether the file is new; absent when the provider doesn't say.
        created: Option<bool>,
    },
    Use {
        server: String,
        tool: String,
    },
    Question {
        text: String,
    },
    /// The title is empty when the plan has none of its own.
    Plan {
        title: String,
    },
    Access {
        reason: String,
    },
    /// A form, a link or an ask this build cannot read: the provider's own
    /// sentence, and the server that sent it when one did.
    Other {
        server: String,
        text: String,
    },
}

impl From<Option<ask_notice::Wants>> for Wants {
    fn from(wants: Option<ask_notice::Wants>) -> Self {
        match wants {
            Some(ask_notice::Wants::Run(run)) => Self::Run {
                command: run.command,
            },
            Some(ask_notice::Wants::Change(change)) => Self::Change {
                path: change.path,
                files: change.files,
                created: change.created,
            },
            Some(ask_notice::Wants::Use(tool)) => Self::Use {
                server: tool.server,
                tool: tool.tool,
            },
            Some(ask_notice::Wants::Question(question)) => Self::Question {
                text: question.text,
            },
            Some(ask_notice::Wants::Plan(plan)) => Self::Plan { title: plan.title },
            Some(ask_notice::Wants::Access(access)) => Self::Access {
                reason: access.reason,
            },
            Some(ask_notice::Wants::Other(other)) => Self::Other {
                server: other.server,
                text: other.text,
            },
            None => Self::Other {
                server: String::new(),
                text: String::new(),
            },
        }
    }
}

impl Said {
    /// None for a notice that says nothing, as one from a newer build might.
    pub fn of(notice: Notice) -> Option<Self> {
        Some(match notice.of? {
            notice::Of::Ask(ask) => Self::Ask {
                key: ask.key,
                wants: ask.wants.into(),
                why: ask.why,
                more: ask.more,
            },
            notice::Of::Blocked(blocked) => match blocked.why? {
                blocked_notice::Why::SignedOut(_) => Self::SignedOut,
                blocked_notice::Why::Usage(usage) => Self::UsageLimit {
                    resets_at_ms: usage.resets_at_ms,
                },
                blocked_notice::Why::Failed(message) => Self::Failed { message },
            },
            notice::Of::Died(died) => Self::Died {
                detail: died.detail,
            },
        })
    }
}
