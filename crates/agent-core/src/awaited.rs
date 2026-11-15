//! The children a one-shot agent waits on before it may exit.

use std::collections::BTreeMap;

use crate::ConnId;

/// What the agent process heard of a child its own tool server asked for,
/// as the request passed through it on the way to the daemon and back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Spawn {
    /// The request arrived and is about to go to the daemon.
    Requested,
    /// The daemon made the child, in this incarnation.
    Created { incarnation: u32 },
    /// The daemon refused: there is no child.
    Refused,
    /// The daemon went away after it took the request and before it
    /// answered: whether there is a child is the next daemon's to say.
    Unconfirmed,
}

/// A child asked for and not yet known to exist or not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Requested {
    /// The daemon has the request, or is about to, and has not answered.
    Asked,
    /// The daemon that took the request went away without answering.
    /// `under` is the daemon connection the agent had when it heard so, if
    /// it had one: the connection of the daemon that went away, unless its
    /// end was heard first.
    Unconfirmed { under: Option<ConnId> },
}

/// The children this agent waits on. Every child incarnation ends with
/// exactly one last report to its parent: the report of a turn the child
/// marked as its incarnation's last, or the report that it exited. So the agent
/// keeps each direct child incarnation it knows of, and whether its last
/// report has come; a one-shot agent exits at a turn end only once every
/// one has. A child's last report starts a turn of its own, at whose end
/// the check runs again, so nothing waits on the daemon's word that a child
/// has gone.
///
/// The agent knows of a child from two sides. What it asked for it knows
/// itself: its tool server's spawn goes through the agent process, so the
/// child is held from the moment the request arrives, before the daemon has
/// heard of it, until the daemon's answer says it was made (it then owes a
/// last report) or refused. What exists the daemon knows: its `children`
/// frame names every live child incarnation, which is how a child comes to
/// be waited on that this incarnation did not ask for (one its last
/// incarnation left running, say), and how a spawn is settled whose daemon
/// went away before answering.
///
/// A frame that names a child always counts: the child exists. A frame
/// that leaves one out counts only when it cannot have been written before
/// the child was made. So a child still being asked for is never dropped
/// for being absent from a frame, and neither is one whose daemon went
/// away, until a frame comes from a daemon that connected afterwards: that
/// daemon read its children from the store the first one left.
///
/// Children are told apart, and last reports from earlier ones, because a
/// count is not enough: one child's report can start a turn that spawns the
/// next before the daemon's count of live children ever falls to none, and
/// a child with its own child reports mid-way and again at its end.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Awaited {
    /// Each direct child incarnation made, named live or heard from, by
    /// agent id and incarnation: whether its last report has come. A report
    /// and the word of its incarnation can come in either order.
    children: BTreeMap<(Vec<u8>, u32), bool>,
    /// Each child asked for whose answer has not settled it, by agent id.
    requested: BTreeMap<Vec<u8>, Requested>,
}

impl Awaited {
    /// What the process heard of the child `child` it asked for, while its
    /// daemon connection was `connection`. A child the daemon made owes a
    /// last report, unless it already came.
    pub(crate) fn heard(&mut self, child: &[u8], heard: Spawn, connection: Option<ConnId>) {
        match heard {
            Spawn::Requested => {
                self.requested.insert(child.to_vec(), Requested::Asked);
            }
            Spawn::Created { incarnation } => {
                self.requested.remove(child);
                self.children
                    .entry((child.to_vec(), incarnation))
                    .or_insert(false);
            }
            Spawn::Refused => {
                self.requested.remove(child);
            }
            // A child already named or heard from was made, whatever
            // became of the answer.
            Spawn::Unconfirmed if self.children.keys().any(|(known, _)| known == child) => {
                self.requested.remove(child);
            }
            Spawn::Unconfirmed => {
                self.requested
                    .insert(child.to_vec(), Requested::Unconfirmed { under: connection });
            }
        }
    }

    /// The word of the daemon on `connection` of which child incarnations
    /// are live: each owes a last report, unless it already came. It
    /// settles a spawn whose daemon went away without answering when it
    /// names the child, which was made then, or when it leaves the child
    /// out and comes from a daemon that connected since, which was not.
    /// Whether it settled any is the answer.
    pub(crate) fn children(&mut self, frame: &wire::Children, connection: ConnId) -> bool {
        for child in &frame.children {
            self.children
                .entry((child.agent_id.clone(), child.incarnation))
                .or_insert(false);
        }
        let before = self.requested.len();
        self.requested.retain(|child, requested| match requested {
            Requested::Asked => true,
            Requested::Unconfirmed { under } => {
                let named = frame.children.iter().any(|live| live.agent_id == *child);
                !named && *under == Some(connection)
            }
        });
        self.requested.len() < before
    }

    /// A turn or exit report taken from incarnation `incarnation` of
    /// the child `from`; `last` when it is that incarnation's last. A
    /// mid-way report alone leaves the incarnation awaited.
    pub(crate) fn report(&mut self, from: &[u8], incarnation: u32, last: bool) {
        *self
            .children
            .entry((from.to_vec(), incarnation))
            .or_insert(false) |= last;
    }

    /// Every child incarnation seen has sent its last report, and no child
    /// asked for is still unsettled.
    pub(crate) fn none(&self) -> bool {
        self.requested.is_empty() && self.children.values().all(|done| *done)
    }

    /// How many children asked for are still unsettled.
    pub(crate) fn unsettled(&self) -> usize {
        self.requested.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAEMON: ConnId = ConnId(1);
    /// A daemon that connected after the first.
    const NEXT: ConnId = ConnId(2);

    fn frame(children: &[(&[u8], u32)]) -> wire::Children {
        wire::Children {
            children: children
                .iter()
                .map(|(id, incarnation)| wire::LiveChild {
                    agent_id: id.to_vec(),
                    incarnation: *incarnation,
                })
                .collect(),
        }
    }

    #[test]
    fn a_report_from_the_only_child_ends_the_wait() {
        let mut awaited = Awaited::default();
        awaited.children(&frame(&[(b"a", 1)]), DAEMON);
        awaited.report(b"a", 1, true);
        assert!(awaited.none());
    }

    /// The child's report can come before the daemon's first word of it.
    #[test]
    fn a_report_before_its_child_is_counted_still_ends_the_wait() {
        let mut awaited = Awaited::default();
        awaited.report(b"a", 1, true);
        awaited.children(&frame(&[(b"a", 1)]), DAEMON);
        assert!(awaited.none());
    }

    /// A's report starts a turn that spawns B before the daemon's word that
    /// A has gone: A's report must not stand in for B's, still on its way
    /// when B has gone too.
    #[test]
    fn an_earlier_childs_report_never_stands_in_for_a_later_childs() {
        let mut awaited = Awaited::default();
        awaited.children(&frame(&[(b"a", 1)]), DAEMON);
        awaited.report(b"a", 1, true);
        awaited.children(&frame(&[(b"b", 1)]), DAEMON);
        assert!(!awaited.none());
        awaited.report(b"b", 1, true);
        assert!(awaited.none());
    }

    /// A middle child reports mid-way (its own child still runs), then
    /// again when it ends; the daemon's word that it has gone can come
    /// before its last report, and the mid-way report must not stand in
    /// for it.
    #[test]
    fn a_childs_mid_way_report_never_stands_in_for_its_last() {
        let mut awaited = Awaited::default();
        awaited.children(&frame(&[(b"c", 1)]), DAEMON);
        awaited.report(b"c", 1, false);
        awaited.children(&frame(&[]), DAEMON);
        assert!(
            !awaited.none(),
            "no child is live, but c's last report is to come"
        );
        awaited.report(b"c", 1, true);
        assert!(awaited.none());
    }

    /// A mid-way report before the daemon names its child leaves the
    /// child awaited.
    #[test]
    fn a_mid_way_report_before_its_child_is_named_leaves_it_awaited() {
        let mut awaited = Awaited::default();
        awaited.report(b"c", 1, false);
        awaited.children(&frame(&[(b"c", 1)]), DAEMON);
        assert!(!awaited.none());
        awaited.report(b"c", 1, true);
        assert!(awaited.none());
    }

    /// A child resumed by its parent's message reports again.
    #[test]
    fn a_resumed_childs_next_report_is_awaited() {
        let mut awaited = Awaited::default();
        awaited.children(&frame(&[(b"a", 1)]), DAEMON);
        awaited.report(b"a", 1, true);
        awaited.children(&frame(&[(b"a", 2)]), DAEMON);
        assert!(!awaited.none());
        awaited.report(b"a", 2, true);
        assert!(awaited.none());
    }

    /// A resumed child's last report can come before the daemon names its
    /// new incarnation: the report is kept for that incarnation, and the
    /// frame that names it later awaits nothing more.
    #[test]
    fn a_resumed_childs_last_report_before_its_frame_ends_the_wait() {
        let mut awaited = Awaited::default();
        awaited.children(&frame(&[(b"a", 1)]), DAEMON);
        awaited.report(b"a", 1, true);
        awaited.report(b"a", 2, true);
        awaited.children(&frame(&[(b"a", 2)]), DAEMON);
        assert!(awaited.none(), "a's second last report was already taken");
    }

    /// A frame that names the same child again, even after its last
    /// report, awaits nothing more of it.
    #[test]
    fn a_child_named_again_after_its_report_is_not_awaited_again() {
        let mut awaited = Awaited::default();
        awaited.children(&frame(&[(b"a", 1), (b"b", 1)]), DAEMON);
        awaited.report(b"a", 1, true);
        awaited.children(&frame(&[(b"a", 1)]), DAEMON);
        awaited.report(b"b", 1, true);
        assert!(awaited.none());
    }

    /// A child asked for is waited on before the daemon has said anything
    /// of it; once made, it owes its last report like any other.
    #[test]
    fn a_child_asked_for_is_awaited_until_its_last_report() {
        let mut awaited = Awaited::default();
        awaited.heard(b"a", Spawn::Requested, Some(DAEMON));
        assert!(!awaited.none(), "asked for, not yet answered");
        awaited.heard(b"a", Spawn::Created { incarnation: 1 }, Some(DAEMON));
        assert_eq!(awaited.unsettled(), 0);
        assert!(!awaited.none(), "made: its last report is to come");
        awaited.children(&frame(&[(b"a", 1)]), DAEMON);
        awaited.report(b"a", 1, true);
        assert!(awaited.none());
    }

    /// A quick child's last report can come before the answer that it was
    /// made.
    #[test]
    fn a_last_report_before_the_answer_ends_the_wait() {
        let mut awaited = Awaited::default();
        awaited.heard(b"a", Spawn::Requested, Some(DAEMON));
        awaited.report(b"a", 1, true);
        awaited.heard(b"a", Spawn::Created { incarnation: 1 }, Some(DAEMON));
        assert!(awaited.none());
    }

    #[test]
    fn a_refused_child_is_not_awaited() {
        let mut awaited = Awaited::default();
        awaited.heard(b"a", Spawn::Requested, Some(DAEMON));
        awaited.heard(b"a", Spawn::Refused, Some(DAEMON));
        assert!(awaited.none());
    }

    /// The daemon wrote the frame before it made the child, or before it
    /// took the request at all: the frame does not say there is no such
    /// child.
    #[test]
    fn a_frame_before_the_answer_does_not_drop_a_child_asked_for() {
        let mut awaited = Awaited::default();
        awaited.heard(b"a", Spawn::Requested, Some(DAEMON));
        assert!(!awaited.children(&frame(&[]), DAEMON), "nothing to settle");
        assert!(!awaited.none(), "a is still being asked for");
        awaited.heard(b"a", Spawn::Created { incarnation: 1 }, Some(DAEMON));
        assert!(!awaited.none());
    }

    /// A spawn its daemon never answered is settled by a frame that names
    /// the child, whichever daemon wrote it: the child was made and owes a
    /// last report.
    #[test]
    fn an_unconfirmed_child_a_frame_names_is_awaited() {
        for daemon in [DAEMON, NEXT] {
            let mut awaited = Awaited::default();
            awaited.heard(b"a", Spawn::Requested, Some(DAEMON));
            awaited.heard(b"a", Spawn::Unconfirmed, Some(DAEMON));
            assert!(!awaited.none());
            assert!(awaited.children(&frame(&[(b"a", 1)]), daemon));
            assert_eq!(awaited.unsettled(), 0);
            assert!(!awaited.none(), "a was made: its last report is to come");
            awaited.report(b"a", 1, true);
            assert!(awaited.none());
        }
    }

    /// The daemon named the child and then went away without answering:
    /// the child was made, and nothing more is needed to say so.
    #[test]
    fn an_unconfirmed_child_already_named_is_awaited_at_once() {
        let mut awaited = Awaited::default();
        awaited.heard(b"a", Spawn::Requested, Some(DAEMON));
        awaited.children(&frame(&[(b"a", 1)]), DAEMON);
        awaited.heard(b"a", Spawn::Unconfirmed, Some(DAEMON));
        assert_eq!(awaited.unsettled(), 0);
        assert!(!awaited.none(), "its last report is to come");
    }

    /// A child that a frame leaves out was never made, once the frame is
    /// from a daemon that connected after the one that went away: it read
    /// its children from the store the first one left.
    #[test]
    fn an_unconfirmed_child_the_next_daemons_frame_leaves_out_is_dropped() {
        let mut awaited = Awaited::default();
        awaited.heard(b"a", Spawn::Requested, Some(DAEMON));
        awaited.heard(b"a", Spawn::Unconfirmed, Some(DAEMON));
        awaited.heard(b"b", Spawn::Requested, Some(DAEMON));
        assert!(awaited.children(&frame(&[]), NEXT));
        assert_eq!(awaited.unsettled(), 1, "b is still being asked for");
        awaited.heard(b"b", Spawn::Refused, Some(NEXT));
        assert!(awaited.none());
    }

    /// A frame from the daemon that went away can have been written before
    /// it made the child, and read after its answer was lost: it does not
    /// say there is no such child.
    #[test]
    fn a_frame_from_the_daemon_that_went_away_does_not_drop_an_unconfirmed_child() {
        let mut awaited = Awaited::default();
        awaited.heard(b"a", Spawn::Requested, Some(DAEMON));
        awaited.heard(b"a", Spawn::Unconfirmed, Some(DAEMON));
        assert!(!awaited.children(&frame(&[]), DAEMON));
        assert_eq!(awaited.unsettled(), 1);
        assert!(awaited.children(&frame(&[]), NEXT));
        assert!(awaited.none());
    }

    /// With the daemon's end already heard when its answer was lost, any
    /// frame is a later daemon's.
    #[test]
    fn an_unconfirmed_child_heard_of_with_no_daemon_is_settled_by_the_next_frame() {
        let mut awaited = Awaited::default();
        awaited.heard(b"a", Spawn::Requested, Some(DAEMON));
        awaited.heard(b"a", Spawn::Unconfirmed, None);
        assert!(awaited.children(&frame(&[]), NEXT));
        assert!(awaited.none());
    }
}
