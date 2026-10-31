//! A profile's standing with its cloud account: what the person asked for
//! and, while signed in, what its relay link is doing. The front door and
//! an embedder report it as one value, so no reader sees a link connected
//! on a profile that is paused or signed out.

use tokio::sync::watch;

/// Which carrier a relay link runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayCarrier {
    Quic,
    Tcp,
}

/// A profile's standing with its cloud account.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Cloud {
    /// Never signed in.
    Unbound,
    /// Signed out, still tied to its account.
    SignedOut,
    SignedIn(Relay),
}

impl Cloud {
    /// Whether the profile is signed in to the account it is bound to:
    /// none while it was never bound, so a profile that only ever worked
    /// on its own network never reads as signed out.
    pub fn signed_in(&self) -> Option<bool> {
        match self {
            Cloud::Unbound => None,
            Cloud::SignedOut => Some(false),
            Cloud::SignedIn(_) => Some(true),
        }
    }

    /// The relay link, while it is connected.
    pub fn connected(&self) -> Option<(crate::Tier, RelayCarrier)> {
        match *self {
            Cloud::SignedIn(Relay::Connected { tier, carrier }) => Some((tier, carrier)),
            _ => None,
        }
    }
}

/// What a signed-in profile's relay link is doing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Relay {
    Connecting,
    Connected {
        tier: crate::Tier,
        carrier: RelayCarrier,
    },
    Retrying,
    /// The account service refused the profile's credentials.
    SignInAgain,
    /// The relay speaks no protocol version this build does.
    VersionMismatch,
}

/// The relay link's state and its watchers, none while no link runs.
/// Kept apart from any one link, so the state outlives a connector that
/// stopped.
#[derive(Clone)]
pub(crate) struct RuntimeStatus {
    tx: watch::Sender<Option<Relay>>,
}

impl Default for RuntimeStatus {
    fn default() -> Self {
        Self {
            tx: watch::channel(None).0,
        }
    }
}

impl RuntimeStatus {
    pub(crate) fn subscribe(&self) -> watch::Receiver<Option<Relay>> {
        self.tx.subscribe()
    }

    pub(crate) fn current(&self) -> Option<Relay> {
        self.tx.borrow().clone()
    }

    pub(crate) fn report(&self, relay: Relay) {
        self.tx.send_replace(Some(relay));
    }

    /// The link stopped and none runs.
    pub(crate) fn clear(&self) {
        self.tx.send_replace(None);
    }
}
