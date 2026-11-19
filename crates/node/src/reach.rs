//! Reaching a paired host: the channel a call to it goes out on, and why
//! none could be had.
//!
//! Finding the channel and waiting for an answer on it have separate
//! limits, so a call that fails says whether it ever left: one that never
//! left reached nothing and can be held and tried again; one that left may
//! have been acted on.

use std::time::Duration;

use tonic::transport::Channel;

use crate::HostId;
use crate::link::ChannelError;
use crate::runtime::ProfileRuntime;

/// How long reaching a host may take: finding its link and opening a
/// channel on it. Past it the call never left, and the host counts as
/// unreachable.
pub(crate) const REACH_PATIENCE: Duration = Duration::from_secs(10);

/// How long a call that went out waits for its answer to begin. A spawn
/// waits for the child's Hello on the far side, so this covers a start as
/// well as a round trip. Past it the call may or may not have arrived.
pub(crate) const ANSWER_PATIENCE: Duration = Duration::from_secs(30);

/// Why no call to a host could leave this one. Nothing reached the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unreached {
    /// No link to it, it is not trusted, or no channel opened in time.
    Unreachable,
    /// Only the relay reaches it, and the account's plan lapsed to free,
    /// which carries no calls between hosts.
    Lapsed,
    /// It refused this host's certificate: it no longer trusts this one.
    Revoked,
}

impl ProfileRuntime {
    /// The channel calls to `host` ride, or why none could be had within
    /// [`REACH_PATIENCE`].
    pub(crate) async fn reach(&self, host: HostId) -> Result<Channel, Unreached> {
        let edge = self.edge().ok_or(Unreached::Unreachable)?;
        if !edge.is_trusted(host) {
            return Err(Unreached::Unreachable);
        }
        let since = crate::routing::LinkId::next_instance();
        match tokio::time::timeout(REACH_PATIENCE, edge.channel(host)).await {
            Ok(Ok(channel)) => Ok(channel),
            // On a free account the relay lists the account's hosts and
            // opens no tunnel between them; the host already reads away.
            Ok(Err(ChannelError::Refused(wire::ForwardRefusal::PaymentRequired))) => {
                Err(Unreached::Lapsed)
            }
            Ok(Err(ChannelError::Revoked)) => Err(Unreached::Revoked),
            // A channel that failed on its link closed the link already;
            // one that found none in time closes the host's links it had
            // to use, not one that came up while it waited. The host reads
            // offline once none is up, and a link of its own coming up
            // again is what releases sends held for it.
            Ok(Err(_)) => Err(Unreached::Unreachable),
            Err(_) => {
                edge.calls_timed_out(host, since);
                Err(Unreached::Unreachable)
            }
        }
    }
}
