//! The services as each entry serves them, sorted by who answers.
//!
//! Local services (inventory, sessions, drafts) are answered by this node
//! from what it holds, for clients attached to it: never on a link. Host
//! services (agents, what the host offers, changes) are answered by this
//! host for what it owns, on every entry. The peer service is answered to a
//! paired daemon asking for itself, on links only.

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the caller is read by the handlers each service brings with it"
    )
)]
mod caller;
