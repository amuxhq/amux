//! Reaching the relay: when the link to it is dialled again, and over
//! which carriers.
//!
//! The relay is wanted while the profile is signed in. One attempt is out
//! at a time, from [`Output::DialRelay`] until [`Input::RelayEnded`]: the
//! driver fetches the token, says which relay host it names
//! ([`Input::RelayPrepared`]), dials it on the carriers the core then
//! chooses, and runs the link until it ends. A link that ends, or an
//! attempt that fails, is followed by a pause of a second doubling to five
//! minutes, with jitter, so a fleet that lost the relay together does not
//! come back together; a clean close, or a link that lived thirty seconds,
//! starts the count afresh. Foreground or a network change allows one
//! immediate retry of a previously working relay, including a stale link
//! whose end arrives within the two-second recovery window. Only a new
//! working link earns another immediate retry. A kept token the relay refused is replaced at
//! once, and an ending that says the credentials are gone or the versions
//! do not match is final until the profile signs in again.
//!
//! The carriers: QUIC, with TLS over TCP started after a head start, the
//! first to link kept; TLS alone to a relay host found within the hour to
//! drop UDP, so a network that eats UDP costs no head start.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Daemon;
use crate::ids::{AttemptId, TimerId};
use crate::io::Output;

/// The pause after the first failure in a row, doubling after each to
/// [`RELAY_RETRY_MAX_MS`].
pub const RELAY_RETRY_FIRST_MS: u64 = 1_000;
pub const RELAY_RETRY_MAX_MS: u64 = 300_000;
/// How long a link must have lived for its loss to start the count afresh.
pub const RELAY_LIVED_MS: u64 = 30_000;
/// Up to a quarter of the pause either way, and up to five seconds more.
const JITTER_SHARE: f64 = 0.25;
const JITTER_EXTRA_MS: f64 = 5_000.0;
/// A resumed runtime can report the old connection's end after the
/// lifecycle event. Keep its prompt retry available while paths are
/// probed; a later failure belongs to ordinary connection recovery.
pub(super) const RELAY_RECOVERY_WINDOW_MS: u64 = 2_000;
/// How long a relay host found to drop UDP is dialled over TLS alone, in
/// policy time.
pub const UDP_BLOCKED_MS: u64 = 3_600_000;
/// QUIC's head start over TLS.
pub const TCP_HEAD_START_MS: u64 = 300;

/// The carriers an attempt dials the relay on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RelayCarriers {
    /// QUIC at once, TLS over TCP after `head_start_ms`, the first to link
    /// kept.
    Both { head_start_ms: u64 },
    /// TLS over TCP alone.
    Tcp,
}

/// How an attempt at the relay ended, as the driver saw it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RelayOutcome {
    /// The link closed cleanly.
    Closed,
    /// The attempt failed, or the link was lost, in the driver's words.
    Lost(String),
    /// The relay refused a token kept from an earlier fetch.
    KeptTokenRefused,
    /// The relay or the account service refused this profile for good:
    /// its credentials, or the protocol version.
    Final(String),
}

/// The relay link's attempts, pause and carrier memory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayReach {
    /// The profile is signed in, and nothing final ended its attempts.
    wanted: bool,
    attempt: Option<RelayAttempt>,
    /// The pause before the next attempt, before jitter.
    backoff_ms: u64,
    /// The retry timer is armed.
    paused: bool,
    /// When each relay host was last found to drop UDP, in policy time.
    udp_blocked: BTreeMap<String, u64>,
    /// Earned by a working link, spent only on an immediate recovery:
    /// repeated lifecycle notifications cannot bypass failure backoff.
    recoverable: bool,
    /// The live attempt may still report its pre-resume failure.
    recovery_until_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct RelayAttempt {
    id: AttemptId,
    /// What it dials on, once the driver named the relay host.
    carriers: Option<RelayCarriers>,
    /// Since when its link has been up, in link time.
    linked_since_ms: Option<u64>,
}

impl RelayReach {
    /// The attempt out to the relay, if one is.
    pub fn attempt(&self) -> Option<AttemptId> {
        self.attempt.as_ref().map(|attempt| attempt.id)
    }

    /// The carriers `attempt` dials on, once chosen; none for an attempt
    /// that is not out.
    pub fn carriers(&self, attempt: AttemptId) -> Option<RelayCarriers> {
        self.attempt
            .as_ref()
            .filter(|out| out.id == attempt)
            .and_then(|out| out.carriers)
    }

    /// Until when `host` is dialled over TLS alone, in policy time.
    pub fn udp_blocked_until(&self, host: &str) -> Option<u64> {
        self.udp_blocked
            .get(host)
            .map(|found| found.saturating_add(UDP_BLOCKED_MS))
    }

    /// The state as a checker tells two apart.
    pub(crate) fn canonical(&self, rank: impl Fn(AttemptId) -> AttemptId) -> RelayReach {
        let mut relay = self.clone();
        if let Some(attempt) = &mut relay.attempt {
            attempt.id = rank(attempt.id);
            attempt.linked_since_ms = attempt.linked_since_ms.map(|_| 0);
        }
        relay
    }
}

impl Daemon {
    /// The relay link, as the core holds it.
    pub fn relay_reach(&self) -> &RelayReach {
        &self.routing.reach.relay
    }

    /// The profile signed in, or out: the relay is dialled afresh from now
    /// on, or no longer. The driver stops whatever attempt was out, and its
    /// ending is heard no more.
    pub(crate) fn relay_wanted(&mut self, wanted: bool, out: &mut Vec<Output>) {
        let relay = &mut self.routing.reach.relay;
        relay.wanted = wanted;
        relay.backoff_ms = RELAY_RETRY_FIRST_MS;
        relay.attempt = None;
        relay.recoverable = false;
        relay.recovery_until_ms = None;
        if relay.paused {
            relay.paused = false;
            self.cancel(TimerId::RelayRetry, out);
        }
        self.dial_relay(out);
    }

    /// Recover once from a previously working connection, in either
    /// ordering of the lifecycle notification and its loss. An attempt
    /// already dialing (including a cold start) is left alone.
    pub(crate) fn recover_relay(&mut self, out: &mut Vec<Output>) {
        let relay = &mut self.routing.reach.relay;
        if !relay.wanted || !relay.recoverable {
            return;
        }
        match relay.attempt.as_ref() {
            Some(attempt) if attempt.linked_since_ms.is_some() => {
                relay.recovery_until_ms =
                    Some(self.timers.link_ms.saturating_add(RELAY_RECOVERY_WINDOW_MS));
            }
            Some(_) => {}
            None => self.recover_relay_now(out),
        }
    }

    fn recover_relay_now(&mut self, out: &mut Vec<Output>) {
        let relay = &mut self.routing.reach.relay;
        relay.recoverable = false;
        relay.recovery_until_ms = None;
        relay.backoff_ms = RELAY_RETRY_FIRST_MS;
        if relay.paused {
            relay.paused = false;
            self.cancel(TimerId::RelayRetry, out);
        }
        tracing::info!("recovering the relay immediately after foreground or network change");
        self.dial_relay(out);
    }

    fn dial_relay(&mut self, out: &mut Vec<Output>) {
        let reach = &mut self.routing.reach;
        if !reach.relay.wanted || reach.relay.attempt.is_some() || reach.relay.paused {
            return;
        }
        reach.next_attempt += 1;
        let id = AttemptId(reach.next_attempt);
        reach.relay.attempt = Some(RelayAttempt {
            id,
            carriers: None,
            linked_since_ms: None,
        });
        out.push(Output::DialRelay { attempt: id });
    }

    /// The driver has the token for `attempt`, which names `host`: the
    /// carriers are chosen now.
    pub(crate) fn relay_prepared(&mut self, attempt: AttemptId, host: &str) {
        let now_ms = self.timers.now_ms;
        let relay = &mut self.routing.reach.relay;
        let blocked = relay
            .udp_blocked
            .get(host)
            .is_some_and(|found| now_ms < found.saturating_add(UDP_BLOCKED_MS));
        if !blocked {
            relay.udp_blocked.remove(host);
        }
        if let Some(out) = relay.attempt.as_mut().filter(|out| out.id == attempt) {
            out.carriers = Some(match blocked {
                true => RelayCarriers::Tcp,
                false => RelayCarriers::Both {
                    head_start_ms: TCP_HEAD_START_MS,
                },
            });
        }
    }

    /// QUIC reached the relay host, or failed where TLS did not.
    pub(crate) fn relay_udp(&mut self, host: String, reached: bool) {
        let now_ms = self.timers.now_ms;
        let relay = &mut self.routing.reach.relay;
        if reached {
            relay.udp_blocked.remove(&host);
        } else {
            relay.udp_blocked.insert(host, now_ms);
        }
    }

    /// The relay admitted `attempt`'s link.
    pub(crate) fn relay_linked(&mut self, attempt: AttemptId) {
        let link_ms = self.timers.link_ms;
        if let Some(out) = self
            .routing
            .reach
            .relay
            .attempt
            .as_mut()
            .filter(|out| out.id == attempt)
        {
            out.linked_since_ms = Some(link_ms);
            self.routing.reach.relay.recoverable = true;
            self.routing.reach.relay.recovery_until_ms = None;
        }
    }

    pub(crate) fn relay_ended(
        &mut self,
        attempt: AttemptId,
        outcome: RelayOutcome,
        out: &mut Vec<Output>,
    ) {
        let link_ms = self.timers.link_ms;
        let relay = &mut self.routing.reach.relay;
        let Some(ended) = relay.attempt.take_if(|out| out.id == attempt) else {
            return;
        };
        let recover = relay.recoverable
            && relay
                .recovery_until_ms
                .take()
                .is_some_and(|until| link_ms <= until);
        let lived = ended
            .linked_since_ms
            .is_some_and(|since| link_ms.saturating_sub(since) >= RELAY_LIVED_MS);
        match outcome {
            RelayOutcome::KeptTokenRefused => return self.dial_relay(out),
            RelayOutcome::Final(_) => {
                relay.wanted = false;
                return;
            }
            RelayOutcome::Closed => relay.backoff_ms = RELAY_RETRY_FIRST_MS,
            RelayOutcome::Lost(_) if lived => relay.backoff_ms = RELAY_RETRY_FIRST_MS,
            RelayOutcome::Lost(_) => {}
        }
        if !relay.wanted {
            return;
        }
        if recover {
            return self.recover_relay_now(out);
        }
        let base = relay.backoff_ms.max(RELAY_RETRY_FIRST_MS);
        relay.backoff_ms = base.saturating_mul(2).min(RELAY_RETRY_MAX_MS);
        relay.paused = true;
        let pause = self.jittered(base);
        let at_ms = link_ms.saturating_add(pause);
        self.arm(TimerId::RelayRetry, at_ms, out);
    }

    /// The pause before the relay's next attempt is over.
    pub(crate) fn relay_retry_due(&mut self, out: &mut Vec<Output>) {
        if !self.timers.armed.remove(&TimerId::RelayRetry) {
            return;
        }
        self.routing.reach.relay.paused = false;
        self.dial_relay(out);
    }

    /// `base` with up to a quarter either way and up to five seconds more,
    /// drawn from the entropy the core was given; `base` itself before
    /// any arrived.
    fn jittered(&mut self, base: u64) -> u64 {
        let Some(random) = self.draw_entropy() else {
            return base;
        };
        let unit = |bytes: &[u8]| {
            let mut word = [0_u8; 8];
            word.copy_from_slice(bytes);
            u64::from_le_bytes(word) as f64 / u64::MAX as f64
        };
        let (share, extra) = (unit(&random[..8]), unit(&random[8..16]));
        let base_f = base as f64;
        let jittered =
            base_f + base_f * JITTER_SHARE * (share * 2.0 - 1.0) + JITTER_EXTRA_MS * extra;
        jittered.max(0.0) as u64
    }
}
