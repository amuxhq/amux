//! What the host was last handed of a chat, so the next delta carries only
//! what moved.

use std::collections::{HashSet, VecDeque};

use ui_state::{Key, Transcript};
use ui_view::{AskCard, Overview, SettingsView};

use crate::values::{ChatFrame, KeysMoved};

/// Each field is None until it was first handed over.
#[derive(Default)]
pub(crate) struct Sent {
    /// The keys the host holds, oldest first, with their orders.
    keys: VecDeque<(Key, u64)>,
    /// With the clock taken out: see [`drawn`].
    frame: Option<ChatFrame>,
    ask: Option<Option<AskCard>>,
    overview: Option<Overview>,
    settings: Option<SettingsView>,
}

impl Sent {
    /// How the window's keys moved since they were last handed over, and
    /// the keys whose rows go with the move: those `changed` the host
    /// already held. A key new at either edge is read when it is drawn. The
    /// window changes only at its edges until `reloaded`; one that moved any
    /// other way is handed over whole all the same.
    pub(crate) fn keys_moved(
        &mut self,
        transcript: &Transcript,
        reloaded: bool,
        changed: &[Key],
    ) -> (Option<KeysMoved>, Vec<Key>) {
        let edges = if reloaded {
            None
        } else {
            self.edges(transcript)
        };
        let Some((above, below, dropped)) = edges else {
            let held = !self.keys.is_empty();
            self.keys = transcript
                .iter()
                .map(|held| (held.item.key.clone(), held.item.order))
                .collect();
            if !reloaded && !held && self.keys.is_empty() {
                return (None, Vec::new());
            }
            let keys = self.keys.iter().map(|(key, _)| key.clone()).collect();
            return (Some(KeysMoved::Reload(keys)), Vec::new());
        };
        self.keys.drain(..dropped);
        for held in below.iter().rev() {
            self.keys.push_front(held.clone());
        }
        self.keys.extend(above.iter().cloned());
        let new: HashSet<&Key> = below.iter().chain(&above).map(|(key, _)| key).collect();
        let wanted: Vec<Key> = changed
            .iter()
            .filter(|key| transcript.get(key).is_some() && !new.contains(key))
            .cloned()
            .collect();
        let moved =
            (!above.is_empty() || !below.is_empty() || dropped > 0).then(|| KeysMoved::Edges {
                above: above.into_iter().map(|(key, _)| key).collect(),
                below: below.into_iter().map(|(key, _)| key).collect(),
                dropped: dropped as u32,
            });
        (moved, wanted)
    }

    /// The keys new above the newest held and below the oldest, and how
    /// many of the oldest held the window dropped; None when the window
    /// did not move only at its edges.
    #[allow(clippy::type_complexity)]
    fn edges(&self, transcript: &Transcript) -> Option<(Vec<(Key, u64)>, Vec<(Key, u64)>, usize)> {
        let ((oldest, oldest_order), (newest, newest_order)) =
            (self.keys.front()?, self.keys.back()?);
        let at = |key: &Key| transcript.get(key).map(|held| held.item.order);
        if at(newest) != Some(*newest_order) {
            return None;
        }
        let pair = |held: &ui_state::Held| (held.item.key.clone(), held.item.order);
        let above: Vec<_> = transcript
            .range(newest_order.saturating_add(1)..=u64::MAX)
            .map(pair)
            .collect();
        let (below, dropped) = if at(oldest) == Some(*oldest_order) {
            let below = match oldest_order.checked_sub(1) {
                Some(before) => transcript.range(0..=before).map(pair).collect(),
                None => Vec::new(),
            };
            (below, 0)
        } else {
            let first = transcript.iter().next()?.item.order;
            let dropped = self
                .keys
                .iter()
                .take_while(|(_, order)| *order < first)
                .count();
            (Vec::new(), dropped)
        };
        let held = self.keys.len() - dropped + above.len() + below.len();
        (held == transcript.len()).then_some((above, below, dropped))
    }

    /// The frame when it moved other than with the clock.
    pub(crate) fn frame(&mut self, frame: ChatFrame) -> Option<ChatFrame> {
        moved(&mut self.frame, drawn(&frame)).then_some(frame)
    }

    pub(crate) fn ask(&mut self, card: Option<AskCard>) -> Option<Option<AskCard>> {
        moved(&mut self.ask, card.clone()).then_some(card)
    }

    pub(crate) fn overview(&mut self, overview: Overview) -> Option<Overview> {
        moved(&mut self.overview, overview.clone()).then_some(overview)
    }

    pub(crate) fn settings(&mut self, settings: SettingsView) -> Option<SettingsView> {
        moved(&mut self.settings, settings.clone()).then_some(settings)
    }
}

/// Holds `now` and says whether it differs from what was held.
fn moved<T: PartialEq>(held: &mut Option<T>, now: T) -> bool {
    if held.as_ref() == Some(&now) {
        return false;
    }
    *held = Some(now);
    true
}

/// The frame as the screen draws it. A running activity's elapsed time is
/// read off the clock with every frame, so every frame differs; the
/// activity line counts it from when the activity began, so a frame that
/// moved only with the clock is not handed over.
fn drawn(frame: &ChatFrame) -> ChatFrame {
    let mut frame = frame.clone();
    if let Some(activity) = frame.composer.activity.as_mut()
        && activity.since_ms > 0
    {
        activity.elapsed_ms = 0;
    }
    frame
}

#[cfg(test)]
mod tests {
    use ui_state::{Msg, SessionState};
    use wire::{Kind, Phase, SessionEvent, session_event};

    use super::*;

    fn event(of: session_event::Of) -> Msg {
        Msg::Event(SessionEvent { of: Some(of) })
    }

    fn item(order: u64) -> Msg {
        event(session_event::Of::Item(wire::Item {
            agent_id: b"agent".to_vec(),
            key: format!("k{order}"),
            order,
            revision: order,
            kind: Kind::Codex as i32,
            ..wire::Item::default()
        }))
    }

    /// A running activity's elapsed time moves with every frame; a frame
    /// that moved only with it is not handed over, unless the activity has
    /// no start to count from.
    #[test]
    fn a_frame_moved_only_by_the_clock_is_not_handed_over() {
        let agent = wire::Agent {
            agent_id: b"agent".to_vec(),
            kind: Kind::Codex as i32,
            ..wire::Agent::default()
        };
        let state = SessionState::new(agent, 3);
        let mut frame = crate::chat::frame(&state, 0, None, &std::collections::HashMap::new());
        let activity = |since_ms, elapsed_ms| model::Activity {
            kind: model::ActivityKind::Working,
            since_ms,
            elapsed_ms,
        };
        let mut sent = Sent::default();
        frame.composer.activity = Some(activity(1_000, 500));
        assert!(
            sent.frame(frame.clone()).is_some(),
            "the first frame is handed over"
        );
        frame.composer.activity = Some(activity(1_000, 900));
        assert!(sent.frame(frame.clone()).is_none(), "only the clock moved");
        frame.composer.activity = Some(activity(0, 1_200));
        assert!(sent.frame(frame.clone()).is_some());
        frame.composer.activity = Some(activity(0, 1_400));
        assert!(
            sent.frame(frame).is_some(),
            "without a start, the elapsed time is all there is"
        );
    }

    /// The client model's schedules never fill a window, so its trim is
    /// walked here: while the reader follows, rows arriving past the cap
    /// drop the oldest the host holds.
    #[test]
    fn a_window_trimmed_to_its_cap_drops_the_oldest_keys_the_host_holds() {
        let agent = wire::Agent {
            agent_id: b"agent".to_vec(),
            host_id: b"host".to_vec(),
            kind: Kind::Codex as i32,
            lifecycle: wire::Lifecycle::Live as i32,
            incarnation: 1,
            ..wire::Agent::default()
        };
        let mut state = SessionState::new(agent, 3);
        state.update(event(session_event::Of::Snapshot(wire::Snapshot {
            kind: Kind::Codex as i32,
            phase: Phase::Working as i32,
            ..wire::Snapshot::default()
        })));
        for order in 1..=3 {
            state.update(item(order));
        }
        state.update(event(session_event::Of::CaughtUp(wire::CaughtUp {
            revision: 3,
        })));
        let mut sent = Sent::default();
        let (opening, _) = sent.keys_moved(state.transcript(), true, &[]);
        assert_eq!(
            opening,
            Some(KeysMoved::Reload(vec![
                "k1".into(),
                "k2".into(),
                "k3".into()
            ]))
        );
        let mut changed = state.update(item(4)).changed;
        changed.extend(state.update(item(5)).changed);
        let (sequence, wanted) = sent.keys_moved(state.transcript(), false, &changed);
        assert_eq!(
            sequence,
            Some(KeysMoved::Edges {
                above: vec!["k4".into(), "k5".into()],
                below: Vec::new(),
                dropped: 2,
            })
        );
        assert!(wanted.is_empty(), "a key new above is read when drawn");
        assert_eq!(
            sent.keys_moved(state.transcript(), false, &[]),
            (None, Vec::new())
        );
    }
}
