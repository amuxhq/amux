//! The C ABI the phone calls.
//!
//! One [`AmuxRuntime`] hosts the embedded installation, with one profile
//! per account. An [`AmuxProfile`] is the
//! fleet and chats of one of its profiles, opened for as long as it is on
//! screen; each open chat is an [`AmuxChat`]. View values cross as JSON whose Swift mirrors are generated
//! from the Rust definitions (`cargo run -p ios-tools -- swift-types`), so the
//! two sides share one definition of every value.
//!
//! A profile's fleet and each open chat open with the whole of what they
//! show (`amux_fleet_opening`, `amux_session_opening`); after that a step
//! hands over what moved in each as a delta, only what moved. Rows are
//! handed out by item key: a chat's id sequence changes only at its two
//! edges, newer keys above the newest the host holds and older ones below
//! its oldest, until a delta reloads it; there is no slot identity to keep
//! in step.
//!
//! Threads: the app owns the loop. The daemon cores step on whichever
//! queue calls `amux_runtime_step`; links, channels and gRPC run on one
//! confined Rust thread, which never calls into the app. Whenever
//! something waits for a step, the descriptor `amux_runtime_wake_fd`
//! answers becomes readable, and the app steps when it sees it. Reads
//! return at once. An act that waits on the agent returns a ticket at
//! once, and its JSON result comes back from a later step under that
//! ticket, after the deltas of what it moved; a step also carries the
//! fleets' and chats' deltas, and the profile list when it moved. The calls
//! that block until the cores have done something (start, stop, a profile
//! or chat opening, a lifecycle input) step the cores on the calling
//! thread while they wait.
//!
//! Every returned string is the caller's to free with `amux_string_free`,
//! and every byte buffer with `amux_bytes_free`. A null return means the
//! call failed; the reason goes to the log.

use std::sync::{Arc, Mutex};

use app_embedded::{EmbeddedRuntime, Phone, ProfileId};
use app_runtime::{AppRuntime, Chat};
use serde::Serialize;
use tokio::runtime::Handle;

mod fleet;
mod helpers;
mod outbox;
mod profile;
mod review;
mod runtime;
mod session;

pub use fleet::*;
pub use profile::*;
pub use runtime::*;
pub use session::*;

// The tests drive real agent processes, which run on Unix.
#[cfg(all(test, unix))]
mod tests;

use outbox::Outbox;

/// Bytes the caller frees with `amux_bytes_free`.
#[repr(C)]
pub struct AmuxBytes {
    pub data: *mut u8,
    pub len: usize,
}

impl AmuxBytes {
    /// No bytes: a null pointer the caller may still free.
    pub(crate) fn empty() -> AmuxBytes {
        AmuxBytes {
            data: std::ptr::null_mut(),
            len: 0,
        }
    }

    /// A copy of `bytes` the caller owns until it frees them.
    pub(crate) fn leak(bytes: &[u8]) -> AmuxBytes {
        let boxed: Box<[u8]> = bytes.into();
        let len = boxed.len();
        AmuxBytes {
            data: Box::into_raw(boxed).cast(),
            len,
        }
    }
}

/// The embedded installation and every profile it hosts.
pub struct AmuxRuntime {
    pub(crate) shared: Arc<Shared>,
    pub(crate) watch: Option<tokio::task::JoinHandle<()>>,
    pub(crate) tail: u32,
}

/// What the runtime, its profiles and its chats share.
pub(crate) struct Shared {
    pub(crate) phone: Phone,
    /// The profiles, managed the same way whether the installation is
    /// hosted here or is a daemon this app attached to.
    pub(crate) installation: client::Installation,
    pub(crate) outbox: Arc<Outbox>,
}

/// What a phone-only call answers on an app attached to a daemon.
pub(crate) const ATTACHED: &str =
    "an app attached to amux does not host its profiles; ask the daemon";

/// One profile's fleet and chats.
pub struct AmuxProfile {
    pub(crate) profile: ProfileId,
    /// The number deltas name this fleet by.
    pub(crate) fleet: u64,
    shared: Arc<Shared>,
    pub(crate) app: Arc<AppRuntime>,
    tail: u32,
    /// Hands the app the sign-in offers paired machines make this profile,
    /// while the profile is open.
    pub(crate) offers: tokio::task::JoinHandle<()>,
    /// Hands the app the profile's setting up, once it asked to follow it.
    pub(crate) onboarding: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Hands the app the pairing window's status while it holds one open.
    pub(crate) pairing: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Drop for AmuxProfile {
    fn drop(&mut self) {
        self.offers.abort();
        for watch in [&self.onboarding, &self.pairing] {
            if let Some(watch) = watch.lock().unwrap_or_else(|p| p.into_inner()).take() {
                watch.abort();
            }
        }
    }
}

/// One open chat.
pub struct AmuxChat {
    pub(crate) chat: Arc<Chat>,
    pub(crate) fleet: u64,
    shared: Arc<Shared>,
}

impl AmuxRuntime {
    /// The installation hosted in process; none when attached.
    pub(crate) fn embedded(&self) -> Option<&Arc<EmbeddedRuntime>> {
        self.shared.embedded()
    }

    pub(crate) fn handle(&self) -> &Handle {
        self.shared.handle()
    }

    pub(crate) fn act<T, F>(&self, act: F) -> u64
    where
        T: Serialize,
        F: Future<Output = T> + Send + 'static,
    {
        self.shared.act(act)
    }
}

impl AmuxProfile {
    /// Starts an act on the profile; what it moved in the fleet is handed
    /// over before its answer.
    pub(crate) fn act<T, F>(&self, act: F) -> u64
    where
        T: Serialize + Send + 'static,
        F: Future<Output = T> + Send + 'static,
    {
        let outbox = self.shared.outbox.clone();
        let fleet = self.fleet;
        self.shared.act(async move {
            let answer = act.await;
            outbox.redraw(fleet, 0);
            answer
        })
    }
}

impl Shared {
    pub(crate) fn embedded(&self) -> Option<&Arc<EmbeddedRuntime>> {
        self.phone.hosted()
    }

    pub(crate) fn handle(&self) -> &Handle {
        self.phone.handle()
    }

    /// Starts `act` on the transport and answers the ticket its result
    /// comes back under, from a later step.
    pub(crate) fn act<T, F>(&self, act: F) -> u64
    where
        T: Serialize,
        F: Future<Output = T> + Send + 'static,
    {
        self.outbox.act(self.handle(), act)
    }

    /// Starts an act that changes the profile list. The list as the act
    /// left it is posted before the act's answer, so the app holds it by
    /// the time it hears the answer.
    pub(crate) fn act_on_profiles<T, F>(&self, act: F) -> u64
    where
        T: Serialize + Send + 'static,
        F: Future<Output = T> + Send + 'static,
    {
        let installation = self.installation.clone();
        let outbox = self.outbox.clone();
        self.act(async move {
            let answer = act.await;
            outbox.post_profiles(&installation).await;
            answer
        })
    }
}

#[cfg(all(test, unix))]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(all(test, unix))]
use app_embedded::{EdgeOverrides, StartConfig};
#[cfg(all(test, unix))]
use helpers::*;
#[cfg(all(test, unix))]
use node::SourcePolicy;
#[cfg(all(test, unix))]
use outbox::Waiting;
