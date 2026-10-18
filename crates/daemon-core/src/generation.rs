//! The installation's generation: the one signal that this host may have
//! lost revisions it already published.
//!
//! The store commits without a sync per transaction, so a power cut can
//! take it back to before revisions a peer has already seen, and the host
//! would then mint those numbers again for different content. Only an
//! unclean reboot loses the page cache: a daemon crash, an agent crash, an
//! update and a clean reboot lose nothing. So the installation keeps the
//! boot id seen at its last start, whether that run shut down cleanly, and
//! a counter, and bumps the counter when the machine rebooted under a run
//! that never shut down cleanly. Both writes of that record, at start and
//! as the last write of a clean shutdown, are [`Job::Fsync`] jobs: an
//! unsynced flag or counter could itself be rolled back.
//!
//! [`Job::Fsync`]: crate::Job::Fsync

/// What the last run left of the record, as a start reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LastRun<'a> {
    /// The machine's boot at the last start.
    pub boot_id: &'a str,
    /// Set only by a clean shutdown, as its last write.
    pub clean: bool,
    pub counter: u64,
}

/// The generation a start runs under: the first is 1, and it rises by one
/// exactly when the machine rebooted under a run that never shut down
/// cleanly.
pub fn at_start(last: Option<LastRun<'_>>, boot_id: &str) -> u64 {
    match last {
        None => 1,
        Some(last) if last.boot_id != boot_id && !last.clean => last.counter + 1,
        Some(last) => last.counter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn last(boot_id: &str, clean: bool) -> Option<LastRun<'_>> {
        Some(LastRun {
            boot_id,
            clean,
            counter: 4,
        })
    }

    #[test]
    fn the_generation_rises_only_after_an_unclean_reboot() {
        assert_eq!(at_start(None, "boot-1"), 1, "the first start");
        assert_eq!(at_start(last("boot-1", false), "boot-2"), 5, "power lost");
        assert_eq!(at_start(last("boot-1", false), "boot-1"), 4, "a crash");
        assert_eq!(
            at_start(last("boot-1", true), "boot-2"),
            4,
            "a clean reboot"
        );
        assert_eq!(at_start(last("boot-1", true), "boot-1"), 4, "a restart");
    }
}
