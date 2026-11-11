//! Waiting in tests: one deadline, one way to fail, and what was last seen.
//!
//! A test waits on a condition the system reports, never on time, and a
//! wait that passes its deadline fails with what it last observed, so the
//! failure says what happened instead of only what did not. A probe that
//! never answers is a failure as well, never a pass. The harness-aware
//! waits (observers over a stream, fences on a host's cursors) live in
//! testnet and fail the same way, through [`Stuck`].

use std::fmt::Display;
use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;

/// How long a wait lasts by default. Real work on loopback settles well
/// inside it; a wait that reaches it is a hang to diagnose, not a slow
/// machine to accommodate.
pub const PATIENCE: Duration = Duration::from_secs(30);

/// How often a polling wait looks again.
const POLL: Duration = Duration::from_millis(20);

/// Added to what a wait last saw when its final look was still under way
/// as the deadline passed.
const UNANSWERED: &str = "one more look had not answered when the time ran out";

/// Why a wait did not pass.
#[derive(Debug, thiserror::Error)]
pub enum Stuck {
    #[error("{what}: not within {waited:?}; last seen:\n{seen}")]
    Deadline {
        what: String,
        waited: Duration,
        seen: String,
    },
    #[error("{what}: the stream ended before the predicate held; saw:\n{seen}")]
    Closed { what: String, seen: String },
    #[error("{what}: the check itself did not answer within {waited:?}")]
    Hung { what: String, waited: Duration },
    #[error("{what}: the condition broke after {after:?} of {window:?}")]
    Broke {
        what: String,
        after: Duration,
        window: Duration,
    },
}

/// Waits until `probe` answers `Ok`, looking again every poll for
/// [`PATIENCE`], and returns what it answered. The probe returns a plain
/// future (`|| async { .. }`), so a wait can run inside a spawned task;
/// a probe that must mutate what it captures does so before the future,
/// as in `|| std::future::ready(step())`. Each `Err` is what the probe
/// saw instead, and the last of them is reported if the deadline passes,
/// also when it passes while the probe is looking once more.
pub async fn until<T, E, F>(what: &str, probe: impl FnMut() -> F) -> Result<T, Stuck>
where
    E: Display,
    F: Future<Output = Result<T, E>>,
{
    until_within(what, PATIENCE, probe).await
}

/// [`until`] with its own deadline, for a wait whose work is known to take
/// longer than loopback traffic: a process spawn, a build, a live provider.
pub async fn until_within<T, E, F>(
    what: &str,
    patience: Duration,
    mut probe: impl FnMut() -> F,
) -> Result<T, Stuck>
where
    E: Display,
    F: Future<Output = Result<T, E>>,
{
    let deadline = Instant::now() + patience;
    let mut last: Option<String> = None;
    loop {
        let seen = match tokio::time::timeout_at(deadline, probe()).await {
            Ok(Ok(found)) => return Ok(found),
            Ok(Err(saw)) => saw.to_string(),
            // The deadline passed while a look was under way. After an
            // earlier look answered, that is the ordinary end of a wait
            // whose condition never held, and what that look saw is the
            // news; only a wait whose first look never answered is hung.
            Err(_) => {
                return Err(match last {
                    Some(seen) => Stuck::Deadline {
                        what: what.to_owned(),
                        waited: patience,
                        seen: format!("{seen}\n({UNANSWERED})"),
                    },
                    None => Stuck::Hung {
                        what: what.to_owned(),
                        waited: patience,
                    },
                });
            }
        };
        if Instant::now() + POLL >= deadline {
            return Err(Stuck::Deadline {
                what: what.to_owned(),
                waited: patience,
                seen,
            });
        }
        last = Some(seen);
        tokio::time::sleep(POLL).await;
    }
}

/// Holds `check` true for all of `window`, looking every poll. A false
/// answer fails at once, and so does a check that does not answer within
/// [`PATIENCE`]. This is the one wait that measures time: it is for
/// asserting that something does not happen when nothing the test controls
/// gates it, and every use is a window to justify.
pub async fn holds_for<F>(
    what: &str,
    window: Duration,
    mut check: impl FnMut() -> F,
) -> Result<(), Stuck>
where
    F: Future<Output = bool>,
{
    let start = Instant::now();
    loop {
        match tokio::time::timeout(PATIENCE, check()).await {
            Ok(true) => {}
            Ok(false) => {
                return Err(Stuck::Broke {
                    what: what.to_owned(),
                    after: start.elapsed(),
                    window,
                });
            }
            Err(_) => {
                return Err(Stuck::Hung {
                    what: what.to_owned(),
                    waited: PATIENCE,
                });
            }
        }
        if start.elapsed() >= window {
            return Ok(());
        }
        tokio::time::sleep(POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[tokio::test]
    async fn until_returns_what_the_probe_found() {
        let calls = AtomicUsize::new(0);
        let found = until("the third look", || async {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            if n >= 3 {
                Ok(n)
            } else {
                Err(format!("look {n}"))
            }
        })
        .await
        .unwrap();
        assert_eq!(found, 3);
    }

    #[tokio::test]
    async fn a_missed_deadline_reports_what_was_last_seen() {
        let calls = AtomicUsize::new(0);
        let stuck = until_within("a count of ten", Duration::from_millis(80), || async {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            Err::<(), _>(format!("count {n}"))
        })
        .await
        .unwrap_err();
        let Stuck::Deadline { what, seen, .. } = &stuck else {
            panic!("expected a deadline, got {stuck}");
        };
        assert_eq!(what, "a count of ten");
        let last = calls.load(Ordering::SeqCst);
        assert_eq!(
            seen,
            &format!("count {last}"),
            "the last answer is reported"
        );
        assert!(stuck.to_string().contains("last seen:\ncount"));
    }

    #[tokio::test]
    async fn a_probe_that_never_answers_is_hung_not_passed() {
        let stuck = until_within("an answer", Duration::from_millis(50), || async {
            std::future::pending::<Result<(), String>>().await
        })
        .await
        .unwrap_err();
        assert!(matches!(stuck, Stuck::Hung { .. }), "got {stuck}");
    }

    /// A look takes time, so the deadline often lands inside one. The
    /// wait then still says what the look before it saw.
    #[tokio::test(start_paused = true)]
    async fn a_deadline_inside_a_look_reports_what_the_look_before_saw() {
        let calls = AtomicUsize::new(0);
        let stuck = until_within("a slow look", Duration::from_millis(100), || async {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Err::<(), _>("first look")
            } else {
                std::future::pending().await
            }
        })
        .await
        .unwrap_err();
        let Stuck::Deadline { seen, .. } = &stuck else {
            panic!("expected a deadline, got {stuck}");
        };
        assert!(seen.starts_with("first look\n"), "{seen}");
    }

    #[tokio::test]
    async fn holds_for_passes_a_condition_that_stays_true() {
        holds_for("truth", Duration::from_millis(60), || async { true })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn holds_for_fails_the_moment_the_condition_breaks() {
        let calls = AtomicUsize::new(0);
        let stuck = holds_for("a short truth", Duration::from_secs(10), || async {
            calls.fetch_add(1, Ordering::SeqCst) < 2
        })
        .await
        .unwrap_err();
        let Stuck::Broke { after, .. } = stuck else {
            panic!("expected broke, got {stuck}");
        };
        assert!(
            after < Duration::from_secs(1),
            "failed at once, after {after:?}"
        );
    }
}
