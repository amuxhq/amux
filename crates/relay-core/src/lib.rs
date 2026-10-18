//! The relay's admission and splice decisions as one function of its state
//! and an input.
//!
//! The driver verifies a token's signature and decodes a stream's preface
//! before the core sees either; [`Relay::step`] judges the claims, the tier
//! and the adjacency, and answers with a refusal or a splice. The driver
//! copies the bytes of a spliced stream; no byte enters the core. Every
//! step ends by checking the cheap invariants, and a violation is reported
//! as [`Output::Dump`]. See the principles in `docs/ARCHITECTURE.md`.

#![forbid(unsafe_code)]

use daemon_core::payload::{LinkCloseReason, StreamPreface, StreamRefusal};
use daemon_core::{DumpReason, HostId, LinkId, LinkRegistry, StepBudget, Violation};

/// An account, by the id its tokens carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UserId(pub [u8; 16]);

/// One stream a host opened over its link to the relay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StreamId(pub u64);

/// What an account's plan lets its hosts do through the relay.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Tier;

/// Everything that can happen to the relay, decoded by the driver.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    LinkUp {
        link: LinkId,
        user: UserId,
        host: HostId,
        tier: Tier,
    },
    LinkDown {
        link: LinkId,
    },
    StreamOpened {
        stream: StreamId,
        link: LinkId,
        preface: StreamPreface,
    },
    StreamEnded {
        stream: StreamId,
    },
    TierChanged {
        user: UserId,
        tier: Tier,
    },
    /// The clock, as an input.
    Tick {
        now_ms: u64,
    },
}

/// Everything the relay wants done.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    /// Refuse the stream before any byte of it is copied.
    Refuse {
        stream: StreamId,
        reason: StreamRefusal,
    },
    /// Copy the stream both ways to a new stream on `to_link`.
    Splice { stream: StreamId, to_link: LinkId },
    Close {
        link: LinkId,
        reason: LinkCloseReason,
    },
    /// A host of the account came up or went down, for its other hosts.
    Presence {
        user: UserId,
        host: HostId,
        up: bool,
    },
    /// Take a dump: the core saw something it must not carry on from.
    Dump { reason: DumpReason },
}

/// The relay's whole state: the links up, which hosts of one account may
/// reach each other, and its step budget.
#[expect(
    dead_code,
    reason = "the links and adjacency are read once link inputs are"
)]
pub struct Relay {
    links: LinkRegistry,
    adjacency: Adjacency,
    budget: StepBudget,
}

/// Which links each account's streams may be spliced between.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Adjacency {}

impl Relay {
    /// Tighter than the daemon's: admission must never queue.
    pub const BUDGET: StepBudget = StepBudget { micros: 1_000 };

    /// A relay with no link up.
    pub fn new() -> Self {
        Self {
            links: LinkRegistry::default(),
            adjacency: Adjacency::default(),
            budget: Self::BUDGET,
        }
    }

    /// One input in, outputs appended. Ends by checking the cheap
    /// invariants; a violation appends [`Output::Dump`].
    pub fn step(&mut self, input: Input, out: &mut Vec<Output>) {
        match input {
            Input::LinkUp { .. }
            | Input::LinkDown { .. }
            | Input::StreamOpened { .. }
            | Input::StreamEnded { .. }
            | Input::TierChanged { .. }
            | Input::Tick { .. } => {}
        }
        if let Err(violation) = self.check_step_invariants() {
            out.push(Output::Dump {
                reason: DumpReason::Violation(violation),
            });
        }
    }

    /// The cheap predicates checked after every step, each in constant time
    /// per link or stream the step touched.
    pub fn check_step_invariants(&self) -> Result<(), Violation> {
        Ok(())
    }

    /// How long one step may take before it is reported.
    pub fn budget(&self) -> StepBudget {
        self.budget
    }
}

impl Default for Relay {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tick_asks_for_nothing() {
        let mut relay = Relay::new();
        let mut out = Vec::new();
        relay.step(Input::Tick { now_ms: 5 }, &mut out);
        assert_eq!(out, Vec::new());
    }
}
