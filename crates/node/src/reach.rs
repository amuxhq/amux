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
use crate::link::{ChannelError, TRUST_REVOKED};
use crate::runtime::ProfileRuntime;

/// How long reaching a host may take: finding its link and opening a
/// channel on it. Past it the call never left, and the host counts as
/// unreachable.
pub(crate) const REACH_PATIENCE: Duration = Duration::from_secs(10);

/// How long a call that went out waits for its answer to begin, from when
/// the last of its request went to the link: a large request still being
/// sent is not waiting yet. A spawn waits for the child's Hello on the far
/// side, so this covers a start as well as a round trip. Past it the call
/// may or may not have arrived.
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

impl Unreached {
    /// What a call to the host named `name` says when it never left for
    /// this reason: the same words for every call, the reason a client
    /// tells apart beside them.
    pub(crate) fn error(self, name: &str) -> wire::Error {
        match self {
            Self::Unreachable => wire::Error::because(
                wire::ErrorReason::HostUnreachable,
                format!("{name} cannot be reached"),
            ),
            // Said as such, so no surface blames the network.
            Self::Lapsed => wire::Error::because(
                wire::ErrorReason::PaymentRequired,
                format!(
                    "{name} can be reached only through the relay, and this account's \
                     subscription has lapsed to the free plan, which carries no calls between \
                     hosts; renew it to reach {name}"
                ),
            ),
            Self::Revoked => wire::Error::new(wire::ErrorCode::Unauthenticated, TRUST_REVOKED),
        }
    }
}

/// What a call to the host named `name` says when it went out and no
/// answer came: within [`ANSWER_PATIENCE`], or before the link failed under
/// it. Whether it arrived, nobody here can know.
pub(crate) fn uncertain(name: &str) -> wire::Error {
    wire::Error::new(
        wire::ErrorCode::Aborted,
        format!("{name} stopped answering; the call may or may not have arrived"),
    )
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
            Ok(Err(_)) => Err(self.unreached(host)),
            Err(_) => {
                edge.calls_timed_out(host, since);
                Err(self.unreached(host))
            }
        }
    }

    /// `host` as a person knows it: the name it was paired under.
    pub(crate) fn host_name(&self, host: HostId) -> String {
        self.edge()
            .and_then(|edge| {
                edge.trusted()
                    .into_iter()
                    .find(|(id, name, _)| *id == host && !name.is_empty())
            })
            .map_or_else(|| format!("host {host}"), |(_, name, _)| name)
    }

    /// Why `host` has no channel, when the channel lookup did not say. A
    /// host that refused this one's certificate closed the link as it did,
    /// so a later lookup finds no route at all; the refusal stays on the
    /// host's entry, and is the reason.
    fn unreached(&self, host: HostId) -> Unreached {
        if self
            .host_entry_of(host)
            .is_some_and(|entry| entry.revoked())
        {
            Unreached::Revoked
        } else {
            Unreached::Unreachable
        }
    }
}
