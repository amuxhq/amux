//! The checks every interpreter's conclusions meet after every step,
//! whatever its kind. They are judged in [`crate::Shared`], which every kind
//! steps through, and a step that breaks one carries the violation on its
//! [`crate::Stepped`]: the agent core reports it as it reports its own, with
//! a dump of the agent's record.
//!
//! Each is a property of what the interpreter concluded, not of the
//! provider's words: a provider that misbehaves is still owed an
//! interpreter that leaves no spinner turning and answers nothing twice.

use std::collections::{HashSet, VecDeque};

pub use core_ring::{Check, Violation};

pub const TOOL_RESULT_WITHOUT_CALL: Check = Check {
    id: "agent.tool_result_without_call",
    sentence: "every tool result answers a call the interpreter saw",
};
pub const EXIT_LEFT_OPEN: Check = Check {
    id: "agent.exit_left_open",
    sentence: "the provider's exit leaves no turn open and no item streaming",
};
pub const CLOSED_ASK_ANSWERED: Check = Check {
    id: "agent.closed_ask_answered",
    sentence: "an ask that closed is never answered later",
};
pub const ITEM_BEFORE_OPEN: Check = Check {
    id: "agent.item_before_open",
    sentence: "every item key is opened before it is updated or closed, and never reopened",
};

/// Every check an interpreter's step is judged by.
pub const CHECKS: &[Check] = &[
    TOOL_RESULT_WITHOUT_CALL,
    EXIT_LEFT_OPEN,
    CLOSED_ASK_ANSWERED,
    ITEM_BEFORE_OPEN,
];

/// How many closed item keys are remembered, so a key opened again after
/// its final item is seen. A reopened key is almost always the next fact
/// or two after the close; older keys are forgotten rather than carried in
/// every checkpoint.
const CLOSED_REMEMBERED: usize = 1024;

/// What the checks watch: the step in progress, and the keys closed lately.
/// Held in memory only, never checkpointed: a resumed interpreter's checks
/// start from what it does next, and they never judge two states unequal.
#[derive(Clone, Debug, Default)]
pub(crate) struct Watch {
    /// The first violation in the step in progress.
    pub(crate) broken: Option<Violation>,
    /// The provider exited, or the incarnation ended, in this step.
    pub(crate) exited: bool,
    /// The answers this step took, by input id, and whether each one's ask
    /// was open when it arrived.
    pub(crate) answers: Vec<(Vec<u8>, bool)>,
    closed: VecDeque<String>,
    closed_set: HashSet<String>,
}

impl PartialEq for Watch {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Watch {
    /// Notes the first violation of the step.
    pub(crate) fn broke(&mut self, check: Check, detail: impl Into<String>) {
        self.broken.get_or_insert_with(|| Violation {
            invariant: check,
            detail: detail.into(),
        });
    }

    pub(crate) fn closed(&mut self, key: &str) {
        if self.closed_set.contains(key) {
            return;
        }
        if self.closed.len() == CLOSED_REMEMBERED
            && let Some(oldest) = self.closed.pop_front()
        {
            self.closed_set.remove(&oldest);
        }
        self.closed.push_back(key.to_owned());
        self.closed_set.insert(key.to_owned());
    }

    pub(crate) fn was_closed(&self, key: &str) -> bool {
        self.closed_set.contains(key)
    }

    /// The step's verdict; what was watched for this step starts over.
    pub(crate) fn finish(&mut self) -> Option<Violation> {
        self.exited = false;
        self.answers.clear();
        self.broken.take()
    }
}
