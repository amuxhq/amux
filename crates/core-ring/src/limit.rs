//! How often a driver writes a dump its core asked for: once per kind of
//! reason in each interval, the rest counted and named in the next dump of
//! that kind.
//!
//! A broken invariant stays broken step after step, and a loaded machine
//! runs one step after another over its budget, so a core that asks once
//! asks again at once. Every dump past the first in a burst says nothing
//! the first did not, and each costs a bundle on the drive and a read of
//! the store between steps. A different invariant, or an overrun beside a
//! violation, is news, so each kind keeps its own window.

use std::borrow::Cow;
use std::collections::HashMap;

/// What a dump is for, as far as the limit tells dumps apart.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DumpKind {
    /// A cheap invariant failed: the check's id. A core names its own; a
    /// driver limiting what another process asked for has the id that
    /// process sent.
    Violation(Cow<'static, str>),
    /// A step took longer than its budget, however far over.
    OverBudget,
}

impl DumpLimit {
    /// The dump a core asked for at `now_ms`, or `None` when the limit
    /// holds it back: a provider stuck in a loop, or a busy relay, meets the
    /// same broken invariant with every event. `whose` names the core ("the
    /// daemon core: ") in front of the reason in its own words.
    pub fn reason(
        &mut self,
        whose: &str,
        reason: &crate::DumpReason,
        now_ms: u64,
    ) -> Option<Admitted> {
        let kind = match reason {
            crate::DumpReason::Violation(broken) => DumpKind::Violation(broken.invariant.id.into()),
            crate::DumpReason::OverBudget { .. } => DumpKind::OverBudget,
        };
        let held_back = self.admit(kind, now_ms)?;
        Some(Admitted {
            reason: with_suppressed(format!("{whose}{reason}"), held_back),
            held_back,
        })
    }
}

/// A dump the limit let through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admitted {
    /// What the dump is written with, saying how many were held back.
    pub reason: String,
    /// How many of its kind the limit held back since the last one.
    pub held_back: u64,
}

/// The dumps a driver has written, by kind, and those it held back since.
#[derive(Debug)]
pub struct DumpLimit {
    interval_ms: u64,
    kinds: HashMap<DumpKind, Window>,
}

#[derive(Debug)]
struct Window {
    /// When the last dump of the kind was written.
    dumped_ms: u64,
    /// Asked for since then and not written.
    suppressed: u64,
}

impl DumpLimit {
    /// Ten minutes: a burst of the same failure is one bundle, a failure
    /// that persists still leaves a fresh bundle for every ten minutes it
    /// lasts, and a busy host overrunning its budget writes six bundles an hour
    /// at most instead of one a second.
    pub const INTERVAL_MS: u64 = 10 * 60_000;

    pub fn new(interval_ms: u64) -> Self {
        Self {
            interval_ms,
            kinds: HashMap::new(),
        }
    }

    /// Whether a dump of `kind` asked for at `now_ms` is written: `Some`
    /// with how many of the kind were held back since the last one
    /// written, or `None` when it is held back itself.
    pub fn admit(&mut self, kind: DumpKind, now_ms: u64) -> Option<u64> {
        let Some(window) = self.kinds.get_mut(&kind) else {
            self.kinds.insert(
                kind,
                Window {
                    dumped_ms: now_ms,
                    suppressed: 0,
                },
            );
            return Some(0);
        };
        if now_ms.saturating_sub(window.dumped_ms) < self.interval_ms {
            window.suppressed += 1;
            return None;
        }
        window.dumped_ms = now_ms;
        Some(std::mem::take(&mut window.suppressed))
    }
}

impl Default for DumpLimit {
    fn default() -> Self {
        Self::new(Self::INTERVAL_MS)
    }
}

/// `reason`, saying how many dumps like it were held back before it.
pub fn with_suppressed(reason: String, suppressed: u64) -> String {
    match suppressed {
        0 => reason,
        1 => format!("{reason} (1 more like it was not dumped since the last)"),
        n => format!("{reason} ({n} more like it were not dumped since the last)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BROKE: DumpKind = DumpKind::Violation(Cow::Borrowed("test.generation_rises"));

    #[test]
    fn one_dump_per_kind_in_an_interval_and_the_next_counts_the_rest() {
        let mut limit = DumpLimit::new(1_000);
        assert_eq!(limit.admit(BROKE, 5), Some(0));
        assert_eq!(limit.admit(BROKE, 6), None);
        assert_eq!(limit.admit(BROKE, 1_004), None);
        // Another kind has a window of its own.
        assert_eq!(limit.admit(DumpKind::OverBudget, 7), Some(0));
        assert_eq!(
            limit.admit(DumpKind::Violation("another".into()), 8),
            Some(0)
        );
        assert_eq!(limit.admit(BROKE, 1_005), Some(2));
        assert_eq!(limit.admit(BROKE, 1_006), None);
        assert_eq!(limit.admit(BROKE, 2_005), Some(1));
    }

    #[test]
    fn a_clock_that_steps_back_holds_the_window() {
        let mut limit = DumpLimit::new(1_000);
        assert_eq!(limit.admit(BROKE, 5_000), Some(0));
        assert_eq!(limit.admit(BROKE, 10), None);
    }

    #[test]
    fn the_reason_names_what_was_held_back() {
        assert_eq!(with_suppressed("slow".into(), 0), "slow");
        assert_eq!(
            with_suppressed("slow".into(), 3),
            "slow (3 more like it were not dumped since the last)"
        );
    }
}
