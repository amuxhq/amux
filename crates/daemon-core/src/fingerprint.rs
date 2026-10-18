//! The core's state as a checker tells two states apart.
//!
//! Two cores that will go on to do the same may still differ in how they
//! numbered things: the calls and jobs they minted, the inventory sessions
//! they counted, the drain passes they ran, and how long their backoffs
//! have grown. A checker exploring every interleaving would explore each
//! numbering apart, though none changes what any later step decides but
//! the ids it names and the times it arms. [`Daemon::fingerprint`] is the
//! state with every such id replaced by its rank among the ones in use and
//! every counter and wait left out, so two cores that differ only there
//! read alike. It is never restored from: a checkpoint is the state.

use std::collections::BTreeMap;

use crate::ids::{CallId, JobId};
use crate::{Daemon, Pending};

/// Ids the core numbered itself, each mapped to its rank among those in
/// use. One no longer in use maps to `unknown`.
pub(crate) struct Renumber<T> {
    ranks: BTreeMap<T, T>,
    unknown: T,
}

impl<T: Ord + Copy> Renumber<T> {
    fn new(ids: impl IntoIterator<Item = T>, rank: impl Fn(u64) -> T, unknown: T) -> Self {
        let mut ids: Vec<T> = ids.into_iter().collect();
        ids.sort();
        ids.dedup();
        let ranks = ids
            .into_iter()
            .zip(1..)
            .map(|(id, place)| (id, rank(place)))
            .collect();
        Self { ranks, unknown }
    }

    pub(crate) fn get(&self, id: T) -> T {
        self.ranks.get(&id).copied().unwrap_or(self.unknown)
    }
}

/// How each kind of id the core mints is renumbered.
pub(crate) struct Ids {
    pub(crate) calls: Renumber<CallId>,
    pub(crate) jobs: Renumber<JobId>,
    pub(crate) sessions: Renumber<u64>,
}

/// The core's state with its own numbering made canonical, and how the
/// calls it made were renumbered, so a checker can carry the same
/// renumbering to the calls it holds for the core.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Fingerprint {
    pub state: Vec<u8>,
    pub calls: BTreeMap<CallId, CallId>,
}

impl Daemon {
    /// The core's state as a checker tells two states apart: what its
    /// later steps decide on, without the numbering of its calls, jobs,
    /// sessions and passes or the length of its waits.
    pub fn fingerprint(&self) -> Fingerprint {
        let mut jobs = Vec::new();
        self.families.jobs(&mut jobs);
        self.deliveries.jobs(&mut jobs);
        self.notifications.jobs(&mut jobs);
        self.blobs.jobs(&mut jobs);
        self.drafts.jobs(&mut jobs);
        self.checkpoints.jobs(&mut jobs);
        let mut sessions = Vec::new();
        self.sources.sessions(&mut sessions);
        self.pending.peers.sessions(&mut sessions);
        let ids = Ids {
            calls: Renumber::new(
                self.pending.peers.waiting.keys().copied(),
                crate::peers::minted,
                CallId(u64::MAX),
            ),
            jobs: Renumber::new(jobs, JobId, JobId(u64::MAX)),
            sessions: Renumber::new(sessions, |place| place, u64::MAX),
        };
        let pending = Pending {
            next_job: 0,
            peers: self.pending.peers.canonical(&ids),
        };
        let core = (
            &self.routing,
            &self.presence.canonical(),
            &self.sources.canonical(&ids),
            &self.families.canonical(&ids),
            &self.pairing,
            &self.answers,
            &self.journals,
            &self.checkpoints.canonical(&ids),
            &self.notifications.canonical(&ids),
            &self.blobs.canonical(&ids),
            &self.drafts.canonical(&ids),
            &self.deliveries.canonical(&ids),
            &self.timers,
            &pending,
            &self.settings,
            &self.options,
        );
        Fingerprint {
            state: postcard::to_allocvec(&core).expect("the core's state serializes"),
            calls: ids.calls.ranks,
        }
    }
}

#[cfg(test)]
mod tests {
    use store::Sqlite;

    use crate::payload::{Answer, CarrierKind, PeerRequest};
    use crate::{CallId, Daemon, HostId, Input, LinkId, Output, StartOptions, TimerId};

    const PEER: HostId = HostId(*b"far-host-0000000");

    /// A daemon that trusts `PEER` and has a link to it.
    fn linked() -> Daemon {
        let mut daemon = Daemon::new(
            Sqlite::open_in_memory(*b"own-host-0000000").expect("a store opens"),
            StartOptions {
                generation: 1,
                ..StartOptions::default()
            },
        );
        let mut out = Vec::new();
        daemon.step(Input::Trusted { hosts: vec![PEER] }, &mut out);
        daemon.step(
            Input::LinkUp {
                link: LinkId(1),
                peer: PEER,
                kind: CarrierKind::Direct,
            },
            &mut out,
        );
        daemon
    }

    /// Fires the follow timer and returns the inventory call it made.
    fn follow(daemon: &mut Daemon) -> CallId {
        let mut out = Vec::new();
        daemon.step(
            Input::Timer {
                id: TimerId::Follow(PEER),
            },
            &mut out,
        );
        out.iter()
            .find_map(|output| match output {
                Output::Call {
                    call,
                    request: PeerRequest::SubscribeInventory { .. },
                    ..
                } => Some(*call),
                _ => None,
            })
            .expect("the host is followed")
    }

    #[test]
    fn cores_that_differ_only_in_how_they_numbered_things_fingerprint_alike() {
        let mut first = linked();
        let first_call = follow(&mut first);

        // The second lost its first inventory stream and followed again:
        // another call, another session and a backoff grown, and otherwise
        // the same.
        let mut second = linked();
        let lost = follow(&mut second);
        second.step(
            Input::PeerAnswer {
                call: lost,
                answer: Answer::Ended,
            },
            &mut Vec::new(),
        );
        let second_call = follow(&mut second);
        assert_ne!(first_call, second_call);
        assert_ne!(first.checkpoint(), second.checkpoint());

        let (first, second) = (first.fingerprint(), second.fingerprint());
        assert_eq!(first.state, second.state);
        assert_eq!(first.calls.get(&first_call), second.calls.get(&second_call));

        // One that is not following reads apart.
        assert_ne!(linked().fingerprint().state, first.state);
    }
}
