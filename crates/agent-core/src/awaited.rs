//! The children a one-shot agent waits on before it may exit.

use std::collections::BTreeMap;

/// The children this agent waits on. Every child incarnation ends with
/// exactly one last report to its parent: the report of a turn the child
/// marked as its incarnation's last, or the report that it exited. So the agent
/// keeps each direct child incarnation the daemon names live, and whether
/// its last report has come; a one-shot agent exits at a turn end only
/// once every one has. A child's last report starts a turn of its own, at
/// whose end the check runs again, so nothing waits on the daemon's word
/// that a child has gone.
///
/// Children are told apart, and last reports from earlier ones, because a
/// count is not enough: one child's report can start a turn that spawns the
/// next before the daemon's count of live children ever falls to none, and
/// a child with its own child reports mid-way and again at its end.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Awaited {
    /// Each direct child incarnation named live or heard from, by agent id
    /// and incarnation: whether its last report has come. A report and the
    /// daemon's word of its incarnation can come in either order.
    children: BTreeMap<(Vec<u8>, u32), bool>,
}

impl Awaited {
    /// The daemon's word of which child incarnations are live: each owes a
    /// last report, unless it already came.
    pub(crate) fn children(&mut self, frame: &wire::Children) {
        for child in &frame.children {
            self.children
                .entry((child.agent_id.clone(), child.incarnation))
                .or_insert(false);
        }
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

    /// Every child incarnation seen has sent its last report.
    pub(crate) fn none(&self) -> bool {
        self.children.values().all(|done| *done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        awaited.children(&frame(&[(b"a", 1)]));
        awaited.report(b"a", 1, true);
        assert!(awaited.none());
    }

    /// The child's report can come before the daemon's first word of it.
    #[test]
    fn a_report_before_its_child_is_counted_still_ends_the_wait() {
        let mut awaited = Awaited::default();
        awaited.report(b"a", 1, true);
        awaited.children(&frame(&[(b"a", 1)]));
        assert!(awaited.none());
    }

    /// A's report starts a turn that spawns B before the daemon's word that
    /// A has gone: A's report must not stand in for B's, still on its way
    /// when B has gone too.
    #[test]
    fn an_earlier_childs_report_never_stands_in_for_a_later_childs() {
        let mut awaited = Awaited::default();
        awaited.children(&frame(&[(b"a", 1)]));
        awaited.report(b"a", 1, true);
        awaited.children(&frame(&[(b"b", 1)]));
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
        awaited.children(&frame(&[(b"c", 1)]));
        awaited.report(b"c", 1, false);
        awaited.children(&frame(&[]));
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
        awaited.children(&frame(&[(b"c", 1)]));
        assert!(!awaited.none());
        awaited.report(b"c", 1, true);
        assert!(awaited.none());
    }

    /// A child resumed by its parent's message reports again.
    #[test]
    fn a_resumed_childs_next_report_is_awaited() {
        let mut awaited = Awaited::default();
        awaited.children(&frame(&[(b"a", 1)]));
        awaited.report(b"a", 1, true);
        awaited.children(&frame(&[(b"a", 2)]));
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
        awaited.children(&frame(&[(b"a", 1)]));
        awaited.report(b"a", 1, true);
        awaited.report(b"a", 2, true);
        awaited.children(&frame(&[(b"a", 2)]));
        assert!(awaited.none(), "a's second last report was already taken");
    }

    /// A frame that names the same child again, even after its last
    /// report, awaits nothing more of it.
    #[test]
    fn a_child_named_again_after_its_report_is_not_awaited_again() {
        let mut awaited = Awaited::default();
        awaited.children(&frame(&[(b"a", 1), (b"b", 1)]));
        awaited.report(b"a", 1, true);
        awaited.children(&frame(&[(b"a", 1)]));
        awaited.report(b"b", 1, true);
        assert!(awaited.none());
    }
}
