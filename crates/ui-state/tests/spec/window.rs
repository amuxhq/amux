//! The transcript window: append above the head, upsert inside if newer,
//! drop below the oldest held row, pages extend the low edge only.

use ui_state::{BlobStatus, Msg, SessionState};
use wire::{Kind, Phase};

use crate::harness::*;

fn open_with(kind: Kind, orders: std::ops::RangeInclusive<u64>) -> SessionState {
    let mut state = SessionState::new(agent(kind), CAP);
    apply_checked(
        &mut state,
        ev_snapshot(snapshot(kind, 1, Phase::Idle, &[], &[])),
    );
    for order in orders {
        apply_checked(&mut state, ev_item(text(kind, order, 10, "held")));
    }
    apply_checked(&mut state, caught_up(10));
    state
}

#[test]
fn a_live_row_below_the_window_is_dropped() {
    for kind in KINDS {
        let mut state = open_with(kind, 5..=7);
        let outcome = apply_checked(&mut state, ev_item(text(kind, 3, 20, "old row revised")));
        assert!(outcome.changed.is_empty());
        assert_eq!(state.oldest_order(), Some(5));
        assert!(state.transcript().get("k3").is_none());
    }
}

#[test]
fn pages_extend_the_low_edge_and_never_the_top() {
    for kind in KINDS {
        let mut state = open_with(kind, 5..=7);
        assert!(state.transcript().has_older());
        let page_items = vec![
            text(kind, 4, 3, "p4"),
            text(kind, 3, 3, "p3"),
            text(kind, 9, 3, "future"),
        ];
        let outcome = apply_page(&mut state, page_items, false);
        assert_eq!(outcome.changed, vec!["k4", "k3"]);
        assert_eq!(state.oldest_order(), Some(3));
        assert_eq!(
            state.transcript().head(),
            Some(7),
            "a page never lands above the head"
        );
        assert!(state.transcript().has_older());
        apply_page(&mut state, vec![text(kind, 2, 3, "p2")], true);
        assert!(
            !state.transcript().has_older(),
            "an exhausted page ends older history"
        );
        let mut from_one = open_with(kind, 1..=2);
        assert!(
            !from_one.transcript().has_older(),
            "a window reaching order one holds everything"
        );
        apply_checked(&mut from_one, ev_item(text(kind, 3, 11, "x")));
    }
}

#[test]
fn page_and_live_overlap_dedupe_by_key() {
    for kind in KINDS {
        let mut state = open_with(kind, 5..=7);
        // A page that overlaps the window brings a newer revision of a held
        // row and an older one of another.
        let items = vec![
            text(kind, 6, 15, "newer from page"),
            text(kind, 5, 2, "older from page"),
            text(kind, 4, 1, "p4"),
        ];
        let outcome = apply_page(&mut state, items, false);
        assert_eq!(outcome.changed, vec!["k6", "k4"]);
        assert_eq!(
            state.transcript().get("k6").unwrap().item.text,
            "newer from page"
        );
        assert_eq!(state.transcript().get("k5").unwrap().item.text, "held");
    }
}

#[test]
fn the_store_read_and_broadcast_overlap_is_deduped_by_key() {
    for kind in KINDS {
        let mut state = SessionState::new(agent(kind), CAP);
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 1, Phase::Working, &[], &[])),
        );
        // Held rows read from the store, already at revision 12...
        apply_checked(&mut state, ev_item(streaming(kind, 1, 12, "Hello world")));
        // ...then the broadcast buffer replays what the read already covered.
        for msg in [
            ev_item(streaming(kind, 1, 10, "Hello")),
            ev_append("k1", 10, 11, " wor"),
            ev_append("k1", 11, 12, "ld"),
        ] {
            let outcome = apply_checked(&mut state, msg);
            assert!(outcome.changed.is_empty());
            assert!(outcome.need_get.is_none());
        }
        assert_eq!(
            state.transcript().get("k1").unwrap().item.text,
            "Hello world"
        );
    }
}

#[test]
fn an_append_for_a_key_not_held_is_dropped() {
    for kind in KINDS {
        let mut state = open_with(kind, 1..=2);
        let outcome = apply_checked(&mut state, ev_append("k3", 3, 4, "x"));
        assert!(
            outcome.need_get.is_none(),
            "an append for a row not held is not a Get"
        );
        assert!(outcome.changed.is_empty());
        assert!(state.transcript().get("k3").is_none());
        // The row comes whole when the item itself arrives.
        let outcome = apply_checked(&mut state, ev_item(text(kind, 3, 5, "whole.x")));
        assert_eq!(outcome.changed, vec!["k3"]);
        assert_eq!(state.transcript().get("k3").unwrap().item.text, "whole.x");
    }
}

#[test]
fn an_attachment_is_a_placeholder_until_its_bytes_arrive() {
    for kind in KINDS {
        let mut state = open_with(kind, 1..=1);
        let blob = wire::BlobRef {
            hash: vec![7; 32],
            name: "shot.png".into(),
            mime: "image/png".into(),
            size: 2048,
        };
        let mut with_image = prompt(kind, 2, 11, "see \u{FFFC}", b"p1");
        with_image.attachments = vec![wire::Attachment {
            of: Some(wire::attachment::Of::Image(blob.clone())),
        }];
        apply_checked(&mut state, ev_item(with_image));
        assert_eq!(
            state.blob(&blob.hash),
            BlobStatus::Missing,
            "the row draws a placeholder from name, type and size"
        );
        let outcome = state.update(Msg::Blob {
            hash: blob.hash.clone(),
            status: BlobStatus::Fetching,
        });
        assert_eq!(outcome.changed, vec!["k2"]);
        let outcome = state.update(Msg::Blob {
            hash: blob.hash.clone(),
            status: BlobStatus::Ready,
        });
        assert_eq!(
            outcome.changed,
            vec!["k2"],
            "the row updates when the bytes arrive"
        );
        let outcome = state.update(Msg::Blob {
            hash: blob.hash.clone(),
            status: BlobStatus::Ready,
        });
        assert!(outcome.changed.is_empty());
        let outcome = state.update(Msg::Blob {
            hash: vec![9; 32],
            status: BlobStatus::Ready,
        });
        assert!(
            outcome.changed.is_empty(),
            "only rows that reference the blob change"
        );
    }
}

#[test]
fn narrowing_the_window_trims_its_oldest_rows_while_following_and_widening_makes_room() {
    for kind in KINDS {
        let mut state = open_with(kind, 1..=30);
        let outcome = apply_block(&mut state, Msg::Window(8));
        assert_eq!(state.cap(), 8);
        assert_eq!(state.oldest_order(), Some(23));
        assert_eq!(outcome.changed.len(), 22, "every trimmed row changed");
        let outcome = apply_block(&mut state, Msg::Window(40));
        assert!(outcome.changed.is_empty(), "widening moves no row");
        assert_eq!(state.page_room(), Some(32));
        assert_eq!(state.oldest_order(), Some(23));
    }
}

#[test]
fn a_window_narrowed_in_history_trims_only_once_the_reader_returns() {
    for kind in KINDS {
        let mut state = open_with(kind, 1..=30);
        apply_block(&mut state, Msg::Following(false));
        apply_block(&mut state, Msg::Window(8));
        assert_eq!(state.oldest_order(), Some(1), "the reader's rows stay");
        apply_block(&mut state, Msg::Following(true));
        assert_eq!(state.oldest_order(), Some(23));
    }
}
