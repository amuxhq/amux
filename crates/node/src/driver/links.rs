//! The profile's core, as its links and channels reach it.

use std::sync::Weak;

use daemon_core::payload::{self, PublicKey};
use daemon_core::{ChannelId, Input};

use super::Opened;
use crate::HostId;
use crate::link::{ChannelClass, ChannelError};
use crate::routing::{LinkId, core_host};
use crate::runtime::ProfileRuntime;

/// What a link or channel tells its profile's core, and asks it. Holds the
/// profile weakly: a link outliving it reports to nobody.
#[derive(Clone)]
pub(crate) struct CoreLinks(Weak<ProfileRuntime>);

impl CoreLinks {
    pub(crate) fn new(runtime: Weak<ProfileRuntime>) -> Self {
        Self(runtime)
    }

    /// Reports what a link or channel did, without waiting.
    pub(crate) fn report(&self, input: Input) {
        if let Some(runtime) = self.0.upgrade() {
            runtime.driver().report(input);
        }
    }

    /// Tells the core a link resuming with a ticket came up, and waits for
    /// whether it takes the identity the ticket names. A profile gone takes
    /// nothing.
    pub(crate) async fn resumed(&self, up: Input) -> bool {
        let Some(runtime) = self.0.upgrade() else {
            return false;
        };
        runtime.driver().resumed(up).await
    }

    /// Asks the core for a channel to `peer` over `link`, with the key the
    /// shell pinned for the peer: the id and key to open it under, or why
    /// it is not opened.
    pub(crate) async fn want_channel(
        &self,
        link: LinkId,
        peer: HostId,
        class: ChannelClass,
        pin: Option<Vec<u8>>,
    ) -> Result<Opened, payload::ChannelError> {
        let Some(runtime) = self.0.upgrade() else {
            return Err(payload::ChannelError::LinkClosed);
        };
        runtime
            .driver()
            .want_channel(
                link.core(),
                core_host(peer),
                core_class(class),
                pin.map(PublicKey),
            )
            .await
    }

    /// Looks at what the core decided, between its steps; nothing while the
    /// profile is gone.
    pub(crate) async fn look<T: Send + 'static>(
        &self,
        look: impl FnOnce(&daemon_core::Daemon) -> T + Send + 'static,
    ) -> Option<T> {
        let runtime = self.0.upgrade()?;
        runtime.driver().look(look).await
    }

    /// The channel's handshake verified the key the core gave.
    pub(crate) fn opened(
        &self,
        channel: ChannelId,
        link: LinkId,
        peer: HostId,
        class: ChannelClass,
    ) {
        self.report(Input::ChannelOpened {
            channel,
            link: link.core(),
            peer: core_host(peer),
            class: core_class(class),
        });
    }

    pub(crate) fn closed(&self, channel: ChannelId, reason: payload::ChannelError) {
        self.report(Input::ChannelClosed { channel, reason });
    }
}

pub(crate) fn core_class(class: ChannelClass) -> payload::ChannelClass {
    match class {
        ChannelClass::Calls => payload::ChannelClass::Calls,
        ChannelClass::Session => payload::ChannelClass::Session,
    }
}

/// Why a channel the core asked for did not open, as the core hears it.
pub(crate) fn core_error(error: &ChannelError) -> payload::ChannelError {
    match error {
        ChannelError::NoRoute { .. } | ChannelError::LinkUnavailable { .. } => {
            payload::ChannelError::LinkClosed
        }
        ChannelError::Refused(refusal) => payload::ChannelError::Refused(*refusal),
        ChannelError::NoPairingRoute
        | ChannelError::Handshake(_)
        | ChannelError::Identity(_)
        | ChannelError::Tls(_)
        | ChannelError::Revoked => payload::ChannelError::Handshake(error.to_string()),
    }
}
