//! Properties over seeded streams that the client model does not explore:
//! full items arriving in any order at all, not only the orders a stream
//! and its pages make, and the block held through following, reading,
//! paging and reloads.

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::Index;
use ui_state::{Msg, SessionState};
use wire::{Item, Kind, Phase, ToolState};

use crate::harness::*;

fn run(state: &mut SessionState, msgs: &[Msg]) {
    for msg in msgs {
        apply_checked(state, msg.clone());
    }
}

/// A case: the kind, how many revisions each of the twelve orders gets, and
/// the order the revisions and two snapshots arrive in.
fn arrivals() -> impl Strategy<Value = (Kind, Vec<u64>, Vec<usize>)> {
    (kinds(), vec(0..4u64, 12)).prop_flat_map(|(kind, counts)| {
        let messages = counts.iter().sum::<u64>() as usize + 2;
        (
            Just(kind),
            Just(counts),
            Just((0..messages).collect::<Vec<_>>()).prop_shuffle(),
        )
    })
}

proptest! {
    #![proptest_config(cases(96))]

    #[test]
    fn full_items_arriving_in_any_order_inside_the_window_give_the_same_state(
        (kind, counts, arrival) in arrivals()
    ) {
        let base: Vec<Msg> = (1..=12)
            .map(|order| ev_item(text(kind, order, order, "v0")))
            .collect();
        // Several revisions per key, as a re-derivation, a page and the
        // broadcast might each deliver them.
        let mut revisions: Vec<Msg> = Vec::new();
        for (order, count) in (1..=12u64).zip(&counts) {
            for revision in 0..*count {
                let item = match (order + revision) % 3 {
                    0 => read(kind, order, 100 + order * 10 + revision),
                    1 => text(kind, order, 100 + order * 10 + revision, "later"),
                    _ => command(
                        kind,
                        order,
                        100 + order * 10 + revision,
                        ToolState::Succeeded,
                    ),
                };
                let mut item = item;
                item.key = format!("k{order}");
                revisions.push(ev_item(item));
            }
        }
        revisions.push(ev_snapshot(snapshot(kind, 5, Phase::Working, &[], &[])));
        revisions.push(ev_snapshot(snapshot(kind, 9, Phase::Idle, &[], &[])));
        let mut expected = SessionState::new(agent(kind), CAP);
        run(&mut expected, &base);
        let mut in_order = SessionState::new(agent(kind), CAP);
        run(&mut in_order, &base);
        run(&mut in_order, &revisions);
        // Snapshots keep their stream order; items move freely.
        let mut snapshots = revisions
            .iter()
            .filter(|msg| is_snapshot(msg))
            .cloned()
            .collect::<Vec<_>>()
            .into_iter();
        let shuffled: Vec<Msg> = arrival
            .iter()
            .map(|&at| {
                let msg = &revisions[at];
                if is_snapshot(msg) {
                    snapshots.next().unwrap()
                } else {
                    msg.clone()
                }
            })
            .collect();
        run(&mut expected, &shuffled);
        prop_assert_eq!(expected, in_order);
    }
}

fn is_snapshot(msg: &Msg) -> bool {
    matches!(msg, Msg::Event(event) if matches!(event.of, Some(wire::session_event::Of::Snapshot(_))))
}

/// What the runtime holds for one agent: every row's newest revision.
struct Store {
    rows: Vec<Item>,
}

impl Store {
    fn page(&self, before: u64, limit: usize) -> (Vec<Item>, bool) {
        let older: Vec<Item> = self
            .rows
            .iter()
            .filter(|item| item.order < before)
            .rev()
            .take(limit)
            .cloned()
            .collect();
        let exhausted = older.last().is_none_or(|item| item.order == 1);
        (older, exhausted)
    }

    fn tail(&self, n: usize) -> Vec<Item> {
        let from = self.rows.len().saturating_sub(n);
        self.rows[from..].to_vec()
    }
}

/// One thing that happens to an agent's transcript while a client watches.
#[derive(Clone, Debug)]
enum Happening {
    /// A new row at the head: a read, a streaming text or a finished text.
    Row(u8),
    /// An append to the newest row, when it is streaming.
    Append,
    /// A revision of an older row.
    Revise(Index),
    /// The reader moves: following or not.
    Follow(bool),
    /// The client pages, as much as the session has room for.
    Page,
}

fn happening() -> impl Strategy<Value = Happening> {
    prop_oneof![
        5 => (0..3u8).prop_map(Happening::Row),
        1 => Just(Happening::Append),
        1 => any::<Index>().prop_map(Happening::Revise),
        1 => any::<bool>().prop_map(Happening::Follow),
        2 => Just(Happening::Page),
    ]
}

proptest! {
    #![proptest_config(cases(128))]

    #[test]
    fn the_block_holds_through_following_reading_paging_and_reloads(
        kind in kinds(),
        happenings in vec(happening(), 0..=120),
    ) {
        const CAP: usize = 6;
        const TAIL: usize = 4;
        let mut store = Store { rows: Vec::new() };
        let mut state = SessionState::new(agent(kind), CAP);
        apply_block(
            &mut state,
            ev_snapshot(snapshot(kind, 1, Phase::Working, &[], &[])),
        );
        apply_block(&mut state, caught_up(1));
        let mut revision = 1;
        for happening in happenings {
            match happening {
                Happening::Row(shape) => {
                    revision += 1;
                    let order = store.rows.len() as u64 + 1;
                    let item = match shape {
                        0 => read(kind, order, revision),
                        1 => streaming(kind, order, revision, "a"),
                        _ => text(kind, order, revision, "t"),
                    };
                    store.rows.push(item.clone());
                    apply_block(&mut state, ev_item(item));
                }
                Happening::Append => {
                    if let Some(last) = store.rows.last_mut()
                        && last.text.starts_with('a')
                    {
                        revision += 1;
                        let append = ev_append(&last.key, last.revision, revision, "b");
                        last.text.push('b');
                        last.revision = revision;
                        let outcome = apply_block(&mut state, append);
                        if outcome.need_get.is_some() {
                            apply_block(&mut state, ev_item(last.clone()));
                        }
                    }
                }
                Happening::Revise(at) => {
                    if !store.rows.is_empty() {
                        revision += 1;
                        let at = at.index(store.rows.len());
                        let mut item = text(kind, store.rows[at].order, revision, "rev");
                        item.key = store.rows[at].key.clone();
                        store.rows[at] = item.clone();
                        apply_block(&mut state, ev_item(item));
                    }
                }
                Happening::Follow(following) => {
                    let outcome = apply_block(&mut state, Msg::Following(following));
                    if outcome.reload {
                        // The driver reopens the stream with a fresh tail.
                        apply_block(&mut state, Msg::Reloading);
                        apply_block(
                            &mut state,
                            ev_snapshot(snapshot(kind, revision, Phase::Working, &[], &[])),
                        );
                        for item in store.tail(TAIL) {
                            apply_block(&mut state, ev_item(item));
                        }
                        let outcome = apply_block(&mut state, caught_up(revision));
                        prop_assert!(outcome.reloaded);
                    }
                }
                Happening::Page => {
                    if let Some(oldest) = state.oldest_order()
                        && state.transcript().has_older()
                    {
                        let limit = state.page_room().unwrap_or(3).min(3);
                        if limit > 0 {
                            let (items, exhausted) = store.page(oldest, limit);
                            let epoch = state.epoch();
                            apply_block(
                                &mut state,
                                Msg::Page {
                                    items,
                                    exhausted,
                                    epoch,
                                },
                            );
                        }
                    }
                }
            }
            let newest = store.rows.len() as u64;
            let transcript = state.transcript();
            if state.following() && !state.head_moved_on() && newest > 0 {
                prop_assert_eq!(
                    transcript.head(),
                    Some(newest),
                    "a following window ends at the head"
                );
                prop_assert!(
                    transcript.len() <= CAP,
                    "a following window keeps at most the cap"
                );
            }
            if !state.following() {
                prop_assert!(state.held_arrivals() <= CAP);
            }
            // Every held row is the store's current revision of that
            // order, or one the store has since revised.
            for held in transcript.iter() {
                let stored = &store.rows[held.item.order as usize - 1];
                prop_assert_eq!(&held.item.key, &stored.key);
                prop_assert!(held.item.revision <= stored.revision);
            }
        }
    }
}
