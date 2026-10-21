use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::time::Duration;

use tokio::time::Instant;

pub(crate) const EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_LIMIT: usize = 10;
pub(crate) const EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_WINDOW: Duration = Duration::from_secs(60);
pub(crate) const EXTERNAL_QUIC_TLS_HANDSHAKE_CONCURRENCY: usize = 128;
/// How soon one connection attempt may be sent a Retry again. A client that
/// lost its Retry sends its Initial again only after a probe timeout, about
/// a second at the start of a connection; anything sooner is a closing
/// connection, which answers every packet, a Retry included, with another
/// close.
pub(crate) const QUIC_RETRY_REPEAT_INTERVAL: Duration = Duration::from_millis(500);

/// Which unvalidated QUIC Initials a listener answers with a Retry. Each
/// source may start `limit` connection attempts per window. An attempt is
/// named by its original destination connection id, so the same attempt
/// arriving again, after a lost Retry or from a closing client answering
/// each Retry with a close, spends nothing and is answered at most once per
/// `repeat`. Without that, one abandoned dial spends a source's whole budget
/// in milliseconds and every host behind the address loses QUIC for the
/// window.
#[derive(Debug)]
pub(crate) struct RetryAdmission<K, A> {
    limit: usize,
    window: Duration,
    repeat: Duration,
    entries: HashMap<K, VecDeque<Attempt<A>>>,
}

#[derive(Debug)]
struct Attempt<A> {
    id: A,
    first: Instant,
    last: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    /// Answer with a Retry.
    Retry,
    /// An attempt already answered moments ago: drop it quietly.
    Repeat,
    /// The source has started its limit of attempts this window.
    Refused,
}

impl<K, A> RetryAdmission<K, A>
where
    K: Eq + Hash,
    A: Eq,
{
    pub(crate) fn new(limit: usize, window: Duration, repeat: Duration) -> Self {
        Self {
            limit,
            window,
            repeat,
            entries: HashMap::new(),
        }
    }

    pub(crate) fn admit(&mut self, source: K, attempt: A) -> Admission {
        self.admit_at(source, attempt, Instant::now())
    }

    fn admit_at(&mut self, source: K, attempt: A, now: Instant) -> Admission {
        self.prune(now);
        let attempts = self.entries.entry(source).or_default();
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
        Admission::Retry
    }

    /// Whether to take a sender that proved its address without a Retry
    /// just now, by a token from an earlier connection. It is counted
    /// against its source's limit as a new attempt, so a token skips the
    /// round trip but never the limit; an attempt that answered a Retry
    /// was counted when the Retry went out.
    pub(crate) fn admit_validated(&mut self, source: K, attempt: A) -> bool {
        self.admit_validated_at(source, attempt, Instant::now())
    }

    fn admit_validated_at(&mut self, source: K, attempt: A, now: Instant) -> bool {
        self.prune(now);
        let attempts = self.entries.entry(source).or_default();
        if attempts.iter().any(|known| known.id == attempt) {
            return true;
        }
        if attempts.len() >= self.limit {
            return false;
        }
        attempts.push_back(Attempt {
            id: attempt,
            first: now,
            last: now,
        });
        true
    }

    fn prune(&mut self, now: Instant) {
        let window = self.window;
        self.entries.retain(|_, attempts| {
            while attempts
                .front()
                .is_some_and(|attempt| attempt.first + window <= now)
            {
                attempts.pop_front();
            }
            !attempts.is_empty()
        });
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
    fn a_token_skips_the_retry_but_not_the_limit() {
        let start = Instant::now();
        let mut retries = admission(2);
        // The attempt that answered its Retry was counted then.
        assert_eq!(retries.admit_at("peer", 1, start), Admission::Retry);
        assert!(retries.admit_validated_at("peer", 1, start));
        // Attempts presenting tokens count as new ones.
        assert!(retries.admit_validated_at("peer", 2, start));
        assert!(!retries.admit_validated_at("peer", 3, start));
        assert_eq!(retries.admit_at("peer", 4, start), Admission::Refused);
        assert!(retries.admit_validated_at("other", 3, start));
        let later = start + Duration::from_secs(60);
        assert!(retries.admit_validated_at("peer", 3, later));
    }
}
