//! Where a call enters this node, which settles who may be calling.

use crate::AgentId;

/// A way into this node's services. Each is served its own set of them,
/// and its calls are answered for the caller it implies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    /// The profile's socket: a person at this machine, on whichever client
    /// names itself in its calls. Reaching the socket is the authority.
    Profile,
    /// An agent's tools socket, bound for that agent alone: the agent is
    /// the caller of everything that arrives on it.
    Tools(AgentId),
    /// The phone's runtime, called in memory by the app it runs in: a
    /// person on the phone.
    Phone,
    /// The streams of links whose handshake presented a trusted key. The
    /// paired host is each connection's own, and the caller header says
    /// whom, if anyone, a call is for.
    Link,
}

impl Entry {
    /// The agent whose tools socket this is.
    pub(crate) fn agent(self) -> Option<AgentId> {
        match self {
            Self::Tools(agent) => Some(agent),
            Self::Profile | Self::Phone | Self::Link => None,
        }
    }
}
