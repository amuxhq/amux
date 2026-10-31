//! The fleet: home, where a chat always returns, and what its keys ask the
//! event loop to do.

use ui_state::AgentKey;
use wire::Attachment;

/// What a fleet key asks the event loop to do.
#[derive(Clone, Debug, PartialEq)]
pub enum FleetEffect {
    Open(AgentKey),
    /// The agent's own interface, raw.
    Attach(AgentKey),
    /// Create an agent with its first prompt, and open its chat or stay.
    Start {
        setup: Box<crate::setup::Setup>,
        text: String,
        attachments: Vec<Attachment>,
        open: bool,
    },
    Rename {
        agent: AgentKey,
        name: String,
    },
    Stop(AgentKey),
    /// Takes back a stop held while the agent's host was out of reach.
    CancelStop(AgentKey),
    Delete(AgentKey),
    /// Leave amux.
    Quit,
    /// Show the key help.
    Help,
    /// Freeze the screen and report a problem with it.
    Report,
}
