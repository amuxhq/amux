use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

pub(crate) const QUIC_HANDSHAKE_RATE_LIMIT: usize = 10;
const QUIC_HANDSHAKE_RATE_WINDOW: Duration = Duration::from_secs(60);
pub(crate) const QUIC_HANDSHAKE_CONCURRENCY: usize = 128;
/// How soon one connection attempt may be sent a Retry again. A client that
/// lost its Retry sends its Initial again only after a probe timeout, about
/// a second at the start of a connection; anything sooner is a closing
/// connection, which answers every packet, a Retry included, with another
/// close.
const QUIC_RETRY_REPEAT_INTERVAL: Duration = Duration::from_millis(500);
/// How many sources a listener counts attempts for at once. Every source
/// that sends an Initial is counted for the window, and one sender can
/// send from as many addresses as it has; past this, the source counted
/// longest ago is forgotten first.
const QUIC_RETRY_SOURCES: usize = 65_536;

/// Which QUIC handshakes a listener takes on, before quinn allocates any
/// connection or TLS state for them: the daemon's listener and the relay's
/// decide alike. A sender whose address is unproven is answered with a
/// Retry; each source may start `QUIC_HANDSHAKE_RATE_LIMIT` attempts a
/// window, a token from an earlier connection skipping the Retry but not
/// the limit; and at most `QUIC_HANDSHAKE_CONCURRENCY` handshakes run at
/// once. An attempt that brings up a link, a paired host's or a signed-in
/// one's, is given back to its source, so a phone opened again and again,
/// each launch dialling afresh, is never shut out, while a stranger's
/// attempts all count. A sender whose address needs no proof (one carried
/// by a forwarder that admitted it, or over a byte stream it opened) skips
/// the Retry and the per-source limit, not the concurrency.
#[derive(Debug)]
pub(crate) struct HandshakeAdmission {
    retries: Arc<Mutex<Attempts>>,
    slots: Arc<Semaphore>,
    retries_sent: AtomicU64,
    limited: AtomicU64,
}

impl Default for HandshakeAdmission {
    fn default() -> Self {
        Self::with_limits(QUIC_HANDSHAKE_RATE_LIMIT, QUIC_HANDSHAKE_CONCURRENCY)
    }
}

impl HandshakeAdmission {
    pub(crate) fn with_limits(rate: usize, concurrency: usize) -> Self {
        Self {
            retries: Arc::new(Mutex::new(RetryAdmission::new(
                rate,
                QUIC_HANDSHAKE_RATE_WINDOW,
                QUIC_RETRY_REPEAT_INTERVAL,
            ))),
            slots: Arc::new(Semaphore::new(concurrency)),
            retries_sent: AtomicU64::new(0),
            limited: AtomicU64::new(0),
        }
    }

    /// Answers or drops `incoming` from `source` as these limits say, or
    /// hands it back to be accepted, holding a handshake slot until the
    /// permit is dropped, with the attempt it spent of its source's limit.
    /// `proven` when the sender's address needs no validation.
    pub(crate) fn admit(
        &self,
        incoming: quinn::Incoming,
        source: SocketAddr,
        proven: bool,
    ) -> Option<Admitted> {
        let mut spent = None;
        if !proven {
            let attempt = incoming.orig_dst_cid();
            let mut retries = lock(&self.retries);
            let admission = if incoming.remote_address_validated() {
                retries.admit_validated(source.ip(), attempt)
            } else {
                retries.admit(source.ip(), attempt)
            };
            drop(retries);
            match admission {
                Admission::Retry if incoming.remote_address_validated() => {
                    spent = Some(Spent {
                        retries: self.retries.clone(),
                        source: source.ip(),
                        attempt,
                    });
                }
                Admission::Retry => {
                    tracing::debug!(peer = %source, "answering an unvalidated QUIC Initial with a Retry");
                    self.retries_sent.fetch_add(1, Ordering::Relaxed);
                    if let Err(error) = incoming.retry() {
                        tracing::debug!(peer = %source, error = %error, "QUIC address was already validated");
                        error.into_incoming().ignore();
                    }
                    return None;
                }
                Admission::Repeat => {
                    incoming.ignore();
                    return None;
                }
                // Refused out loud, in one small packet: a dialler told at
                // once tries another path, where one ignored waits out its
                // handshake timeout first.
                Admission::Refused => {
                    tracing::warn!(peer = %source, "QUIC handshake rate limit exceeded");
                    self.limited.fetch_add(1, Ordering::Relaxed);
                    incoming.refuse();
                    return None;
                }
            }
        }
        match self.slots.clone().try_acquire_owned() {
            Ok(permit) => Some(Admitted {
                incoming,
                permit,
                spent,
            }),
            Err(_) => {
                tracing::warn!(peer = %source, "QUIC handshake concurrency limit exceeded");
                incoming.refuse();
                None
            }
        }
    }

    /// How many Retries unvalidated Initials were answered with.
    pub(crate) fn retries_sent(&self) -> u64 {
        self.retries_sent.load(Ordering::Relaxed)
    }

    /// How many handshakes the per-source rate limit dropped.
    pub(crate) fn limited(&self) -> u64 {
        self.limited.load(Ordering::Relaxed)
    }
}

type Attempts = RetryAdmission<IpAddr, quinn::ConnectionId>;

fn lock(attempts: &Mutex<Attempts>) -> std::sync::MutexGuard<'_, Attempts> {
    attempts.lock().unwrap_or_else(|e| e.into_inner())
}

/// A handshake taken on: the `Incoming` to accept, the slot it holds while
/// it runs, and the attempt it spent of its source's limit, if it was
/// counted.
pub(crate) struct Admitted {
    pub(crate) incoming: quinn::Incoming,
    pub(crate) permit: OwnedSemaphorePermit,
    pub(crate) spent: Option<Spent>,
}

/// An attempt counted against its source's limit, which the listener
/// gives back once the connection proves to be a link's.
#[derive(Clone)]
pub(crate) struct Spent {
    retries: Arc<Mutex<Attempts>>,
    source: IpAddr,
    attempt: quinn::ConnectionId,
}

impl Spent {
    pub(crate) fn refund(self) {
        lock(&self.retries).refund(self.source, self.attempt);
    }
}

impl std::fmt::Debug for Spent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Spent")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// Which unvalidated QUIC Initials a listener answers with a Retry. Each
/// source may start `limit` connection attempts per window. An attempt is
/// named by its original destination connection id, so the same attempt
/// arriving again, after a lost Retry or from a closing client answering
/// each Retry with a close, spends nothing and is answered at most once per
/// `repeat`. Without that, one abandoned dial spends a source's whole budget
/// in milliseconds and every host behind the address loses QUIC for the
/// window.
#[derive(Debug)]
struct RetryAdmission<K, A> {
    limit: usize,
    window: Duration,
    repeat: Duration,
    sources: usize,
    entries: HashMap<K, VecDeque<Attempt<A>>>,
    /// Every attempt counted, oldest first, by its source and when it was
    /// counted: what ages out of the window, or is forgotten first, is
    /// found at its front. An attempt given back stays here until it
    /// reaches the front.
    order: VecDeque<(Instant, K)>,
}

#[derive(Debug)]
struct Attempt<A> {
    id: A,
    first: Instant,
    last: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Admission {
    /// Answer with a Retry.
    Retry,
    /// An attempt already answered moments ago: drop it quietly.
    Repeat,
    /// The source has started its limit of attempts this window.
    Refused,
}

impl<K, A> RetryAdmission<K, A>
where
    K: Eq + Hash + Clone,
    A: Eq,
{
    fn new(limit: usize, window: Duration, repeat: Duration) -> Self {
        Self::with_sources(limit, window, repeat, QUIC_RETRY_SOURCES)
    }

    fn with_sources(limit: usize, window: Duration, repeat: Duration, sources: usize) -> Self {
        Self {
            limit,
            window,
            repeat,
            sources,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn admit(&mut self, source: K, attempt: A) -> Admission {
        self.admit_at(source, attempt, Instant::now())
    }

    fn admit_at(&mut self, source: K, attempt: A, now: Instant) -> Admission {
        self.prune(now);
        let attempts = self.entries.entry(source.clone()).or_default();
        if let Some(known) = attempts.iter_mut().find(|known| known.id == attempt) {
            if now.duration_since(known.last) < self.repeat {
                return Admission::Repeat;
            }
            known.last = now;
            return Admission::Retry;
        }
        if attempts.len() >= self.limit {
            return Admission::Refused;
        }
        attempts.push_back(Attempt {
            id: attempt,
            first: now,
            last: now,
        });
        self.counted(source, now);
        Admission::Retry
    }

    /// Whether to take a sender that proved its address without a Retry
    /// just now: by answering one, an attempt counted when the Retry went
    /// out, or by a token from an earlier connection, counted now as a new
    /// one. A token skips the round trip, never the limit.
    fn admit_validated(&mut self, source: K, attempt: A) -> Admission {
        self.admit_validated_at(source, attempt, Instant::now())
    }

    fn admit_validated_at(&mut self, source: K, attempt: A, now: Instant) -> Admission {
        self.prune(now);
        let attempts = self.entries.entry(source.clone()).or_default();
        if attempts.iter().any(|known| known.id == attempt) {
            return Admission::Retry;
        }
        if attempts.len() >= self.limit {
            return Admission::Refused;
        }
        attempts.push_back(Attempt {
            id: attempt,
            first: now,
            last: now,
        });
        self.counted(source, now);
        Admission::Retry
    }

    /// An attempt of `source`'s was counted at `now`. Past the sources
    /// counted at once, or the attempts they may hold, those counted
    /// longest ago are forgotten.
    fn counted(&mut self, source: K, now: Instant) {
        self.order.push_back((now, source));
        while self.entries.len() > self.sources
            || self.order.len() > self.sources.saturating_mul(self.limit)
        {
            let Some((first, source)) = self.order.pop_front() else {
                break;
            };
            // The source's oldest attempt, unless it was given back or its
            // source forgotten and counted afresh since.
            if self
                .entries
                .get(&source)
                .and_then(VecDeque::front)
                .is_some_and(|oldest| oldest.first == first)
            {
                self.entries.remove(&source);
            }
        }
    }

    /// Gives `attempt` back to `source`'s limit.
    fn refund(&mut self, source: K, attempt: A) {
        if let Some(attempts) = self.entries.get_mut(&source) {
            attempts.retain(|known| known.id != attempt);
            if attempts.is_empty() {
                self.entries.remove(&source);
            }
        }
    }

    /// Lets go of the attempts that aged out of the window, from the
    /// front of the order they were counted in: no pass over every source.
    fn prune(&mut self, now: Instant) {
        while let Some((first, _)) = self.order.front()
            && *first + self.window <= now
        {
            let (_, source) = self.order.pop_front().expect("just seen");
            let Some(attempts) = self.entries.get_mut(&source) else {
                continue;
            };
            while attempts
                .front()
                .is_some_and(|attempt| attempt.first + self.window <= now)
            {
                attempts.pop_front();
            }
            if attempts.is_empty() {
                self.entries.remove(&source);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admission(limit: usize) -> RetryAdmission<&'static str, u32> {
        RetryAdmission::new(limit, Duration::from_secs(60), Duration::from_millis(500))
    }

    #[test]
    fn retry_admission_caps_new_attempts_per_source_then_recovers() {
        let start = Instant::now();
        let mut retries = admission(2);
        assert_eq!(retries.admit_at("peer", 1, start), Admission::Retry);
        assert_eq!(retries.admit_at("peer", 2, start), Admission::Retry);
        assert_eq!(retries.admit_at("peer", 3, start), Admission::Refused);
        assert_eq!(retries.admit_at("other", 3, start), Admission::Retry);
        let later = start + Duration::from_secs(60);
        assert_eq!(retries.admit_at("peer", 3, later), Admission::Retry);
    }

    /// One sender can send from as many addresses as it has: the table
    /// counts a bounded number of sources, forgetting the one counted
    /// longest ago, and lets aged attempts go without a pass over all.
    #[test]
    fn retry_admission_counts_a_bounded_number_of_sources() {
        let start = Instant::now();
        let mut retries =
            RetryAdmission::with_sources(2, Duration::from_secs(60), Duration::from_millis(500), 3);
        for (n, source) in ["a", "b", "c", "d"].into_iter().enumerate() {
            let at = start + Duration::from_secs(n as u64);
            assert_eq!(retries.admit_at(source, 1, at), Admission::Retry);
        }
        assert_eq!(retries.entries.len(), 3);
        assert!(!retries.entries.contains_key("a"), "the oldest went");
        // "b" still counts its attempt: one more fills its limit.
        let now = start + Duration::from_secs(5);
        assert_eq!(retries.admit_at("b", 2, now), Admission::Retry);
        assert_eq!(retries.admit_at("b", 3, now), Admission::Refused);

        // Past the window everything ages out, given back or not.
        retries.refund("c", 1);
        let later = start + Duration::from_secs(70);
        assert_eq!(retries.admit_at("e", 1, later), Admission::Retry);
        assert_eq!(retries.entries.len(), 1);
        assert_eq!(retries.order.len(), 1);
    }

    #[test]
    fn a_repeated_attempt_spends_nothing_and_is_answered_at_most_once_per_interval() {
        let start = Instant::now();
        let mut retries = admission(2);
        assert_eq!(retries.admit_at("peer", 1, start), Admission::Retry);
        // A closing client answers each Retry with a close at once.
        for step in 1..20 {
            let at = start + Duration::from_micros(400 * step);
            assert_eq!(retries.admit_at("peer", 1, at), Admission::Repeat);
        }
        // A client whose Retry was lost asks again after its probe timeout.
        let resent = start + Duration::from_secs(1);
        assert_eq!(retries.admit_at("peer", 1, resent), Admission::Retry);
        assert_eq!(retries.admit_at("peer", 1, resent), Admission::Repeat);
        // The budget still has room for a new attempt.
        assert_eq!(retries.admit_at("peer", 2, resent), Admission::Retry);
        assert_eq!(retries.admit_at("peer", 3, resent), Admission::Refused);
    }

    #[test]
    fn a_token_skips_the_retry_but_not_the_limit_until_its_attempt_is_given_back() {
        let start = Instant::now();
        let mut retries = admission(2);
        // The attempt that answered its Retry was counted then.
        assert_eq!(retries.admit_at("peer", 1, start), Admission::Retry);
        assert_eq!(
            retries.admit_validated_at("peer", 1, start),
            Admission::Retry
        );
        // Attempts presenting tokens count as new ones.
        assert_eq!(
            retries.admit_validated_at("peer", 2, start),
            Admission::Retry
        );
        assert_eq!(
            retries.admit_validated_at("peer", 3, start),
            Admission::Refused
        );
        assert_eq!(retries.admit_at("peer", 4, start), Admission::Refused);
        // Each that brought up a link is given back.
        retries.refund("peer", 1);
        retries.refund("peer", 2);
        for attempt in 3..5 {
            assert_eq!(
                retries.admit_validated_at("peer", attempt, start),
                Admission::Retry
            );
        }
        assert_eq!(retries.admit_at("peer", 5, start), Admission::Refused);
    }
}
