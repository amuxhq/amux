//! Turns: a finished turn read answer first, its work folded under one
//! line until opened, keeping in view what the reader must see; and every
//! row a change moves named, so a client redrawing only those draws what a
//! fresh read draws.

use std::collections::{BTreeMap, HashSet};

use ui_state::{Key, Msg, SessionState};
use ui_view::{Row, ToolRows, TurnPart, TurnPlace, Worked, chat_rows_for, turn_keys, turn_rows_moved};
use wire::session_event;

use crate::runs::{It, event, item, session, session_from};

fn turn_rows(state: &SessionState, open: &HashSet<Key>) -> Vec<Row> {
    let none = HashSet::new();
    crate::support::all_rows(
        state,
        ToolRows::Turns {
            runs: &none,
            turns: open,
        },
    )
}

/// The orders drawn, each with its part in a finished turn.
fn drawn(state: &SessionState, open: &HashSet<Key>) -> Vec<(u64, Option<TurnPart>)> {
    turn_rows(state, open)
        .into_iter()
        .filter(|row| !row.collapsed)
        .map(|row| (row.order, row.turn.map(|turn| turn.part)))
        .collect()
}

fn part(state: &SessionState, order: u64) -> Option<TurnPlace> {
    turn_rows(state, &HashSet::new())
        .into_iter()
        .find(|row| row.order == order)
        .and_then(|row| row.turn)
}

const WORKED: Option<Worked> = Some(Worked {
    duration_ms: None,
    failed: false,
});

#[test]
fn a_finished_turn_folds_its_work_under_the_prompt_and_reads_its_answer() {
    let state = session(&[
        It::Prompt("fix the test"),
        It::Read("a.rs"),
        It::Say("the fixture is stale"),
        It::Edit("a.rs"),
        It::Edit("a.rs"),
        It::Run("cargo test", 0),
        It::Say("fixed it"),
        It::Say("the fixture now matches"),
        It::Turn,
    ]);
    let end = |answer: Vec<&str>| TurnPart::End {
        answer: answer.into_iter().map(str::to_owned).collect(),
        files: Vec::new(),
    };
    let drawn = drawn(&state, &HashSet::new());
    assert_eq!(
        drawn.iter().map(|(order, _)| *order).collect::<Vec<_>>(),
        vec![1, 7, 8, 9],
        "the prompt, the answer and the foot; the work, prose in it too, folds"
    );
    assert_eq!(drawn[0].1, Some(TurnPart::Prompt { worked: WORKED }));
    assert_eq!(drawn[1].1, Some(TurnPart::Answer));
    // The foot names the answer's rows and each file the turn changed once,
    // with what its edits added and removed between them.
    let Some(TurnPart::End { answer, files }) = &drawn[3].1 else {
        panic!("no foot: {:?}", drawn[3]);
    };
    assert_eq!(answer, &["k7".to_owned(), "k8".to_owned()]);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "a.rs");
    assert_eq!((files[0].added, files[0].removed), (2, 2));
    assert_ne!(drawn[3].1, Some(end(vec!["k7", "k8"])), "it carries the file");

    // Every row of the turn names it by its end; the work says it is work.
    for order in 2..=6 {
        let turn = part(&state, order).unwrap();
        assert_eq!((turn.end.as_str(), turn.part), ("k9", TurnPart::Work));
    }
}

#[test]
fn an_opened_turn_draws_its_work_as_the_run_fold_does() {
    let state = session(&[
        It::Prompt("look"),
        It::Read("a.rs"),
        It::Read("b.rs"),
        It::Read("c.rs"),
        It::Say("found it"),
        It::Turn,
    ]);
    let open: HashSet<Key> = ["k6".to_owned()].into();
    assert_eq!(
        drawn(&state, &open)
            .iter()
            .map(|(order, _)| *order)
            .collect::<Vec<_>>(),
        vec![1, 4, 5, 6],
        "the run folds to its newest step inside the opened turn"
    );
    assert!(turn_rows(&state, &open).iter().all(|row| row.turn.as_ref().unwrap().open));
    assert_eq!(
        turn_keys(&state, &"k6".to_owned()),
        (1..=6).map(|order| format!("k{order}")).collect::<Vec<_>>()
    );
}

#[test]
fn a_closed_turn_keeps_a_failure_it_never_fixed_in_view() {
    let state = session(&[
        It::Prompt("check"),
        It::Run("just docs-check", 1),
        It::Read("a.rs"),
        It::Say("it still fails on the link; I left it"),
        It::Turn,
    ]);
    assert_eq!(
        drawn(&state, &HashSet::new()),
        vec![
            (1, Some(TurnPart::Prompt { worked: WORKED })),
            (2, Some(TurnPart::Kept)),
            (4, Some(TurnPart::Answer)),
            (
                5,
                Some(TurnPart::End {
                    answer: vec!["k4".to_owned()],
                    files: Vec::new()
                })
            ),
        ]
    );

    // Fixed later in the turn, it folds with the rest.
    let fixed = session(&[
        It::Prompt("check"),
        It::Run("just docs-check", 1),
        It::Run("just docs-check", 0),
        It::Say("passes"),
        It::Turn,
    ]);
    assert_eq!(part(&fixed, 2).unwrap().part, TurnPart::Work);
}

#[test]
fn a_turn_that_only_answered_has_no_line() {
    let state = session(&[It::Prompt("hi"), It::Say("hello"), It::Turn]);
    assert_eq!(
        part(&state, 1).unwrap().part,
        TurnPart::Prompt { worked: None }
    );
    assert_eq!(part(&state, 2).unwrap().part, TurnPart::Answer);
}

#[test]
fn the_turn_under_way_and_one_whose_prompt_is_not_held_do_not_fold() {
    let live = session(&[It::Prompt("go"), It::Read("a.rs"), It::Say("reading")]);
    assert!(turn_rows(&live, &HashSet::new()).iter().all(|row| row.turn.is_none()));

    // The window starts inside the turn: older history holds its prompt.
    let cut = session_from(40, 200, &[It::Read("a.rs"), It::Say("done"), It::Turn]);
    assert!(turn_rows(&cut, &HashSet::new()).iter().all(|row| row.turn.is_none()));
}

/// The client's copy of the rows, as a host keeps them: read whole once,
/// then only the rows each update names, with those the turn fold moves.
struct Client {
    rows: BTreeMap<Key, Row>,
}

impl Client {
    fn read(state: &SessionState, open: &HashSet<Key>, keys: &[Key]) -> Vec<Row> {
        let none = HashSet::new();
        chat_rows_for(
            state,
            keys,
            ToolRows::Turns {
                runs: &none,
                turns: open,
            },
        )
    }

    fn new(state: &SessionState, open: &HashSet<Key>) -> Client {
        let keys: Vec<Key> = state.transcript().keys().cloned().collect();
        let rows = Self::read(state, open, &keys)
            .into_iter()
            .map(|row| (row.id.clone(), row))
            .collect();
        Client { rows }
    }

    fn update(&mut self, state: &mut SessionState, open: &HashSet<Key>, msg: Msg) {
        let outcome = state.update(msg);
        let mut keys = outcome.changed.clone();
        keys.extend(turn_rows_moved(state, &outcome.changed));
        let held: HashSet<Key> = state.transcript().keys().cloned().collect();
        self.rows.retain(|key, _| held.contains(key));
        // A row new at an edge is read when it is drawn.
        keys.extend(held.iter().filter(|key| !self.rows.contains_key(*key)).cloned());
        for row in Self::read(state, open, &keys) {
            self.rows.insert(row.id.clone(), row);
        }
    }

    fn agrees(&self, state: &SessionState, open: &HashSet<Key>, after: &str) {
        let fresh = Client::new(state, open);
        for (key, row) in &fresh.rows {
            assert_eq!(
                self.rows.get(key).map(|row| (&row.turn, row.collapsed)),
                Some((&row.turn, row.collapsed)),
                "{key} after {after}"
            );
        }
    }
}

#[test]
fn each_change_names_every_row_whose_turn_place_moves() {
    let mut state = session(&[]);
    let open = HashSet::new();
    let mut client = Client::new(&state, &open);
    let turns = [
        It::Prompt("one"),
        It::Read("a.rs"),
        It::Say("so far"),
        It::Edit("a.rs"),
        It::Say("done"),
        It::Turn,
        It::Prompt("two"),
        It::Run("just check", 1),
        It::Say("left it failing"),
        It::Turn,
        It::Prompt("three"),
        It::Say("hello"),
        It::Turn,
    ];
    for (index, it) in turns.iter().enumerate() {
        let order = index as u64 + 1;
        client.update(&mut state, &open, event(session_event::Of::Item(item(order, it))));
        client.agrees(&state, &open, &format!("item {order}"));
    }
}

#[test]
fn the_turn_at_the_low_edge_folds_once_its_prompt_pages_in_and_unfolds_when_cut() {
    let items = [
        It::Prompt("one"),
        It::Read("a.rs"),
        It::Read("b.rs"),
        It::Say("done"),
        It::Turn,
        It::Prompt("two"),
        It::Say("ok"),
        It::Turn,
    ];
    // The window holds the first turn from its second step.
    let mut state = session_from(3, 200, &items[2..]);
    let open = HashSet::new();
    let mut client = Client::new(&state, &open);
    assert!(part(&state, 4).is_none());

    let epoch = state.epoch();
    client.update(
        &mut state,
        &open,
        Msg::Page {
            items: [(2, &items[1]), (1, &items[0])]
                .into_iter()
                .map(|(order, it)| item(order, it))
                .collect(),
            exhausted: true,
            epoch,
        },
    );
    client.agrees(&state, &open, "the page");
    assert_eq!(part(&state, 3).unwrap().part, TurnPart::Work);

    // A window capped at five rows cuts the first turn's prompt away as the
    // next turn arrives.
    let mut capped = session_from(1, 5, &items[..5]);
    let mut client = Client::new(&capped, &open);
    assert_eq!(part(&capped, 4).unwrap().part, TurnPart::Answer);
    for (index, it) in items[5..].iter().enumerate() {
        let order = index as u64 + 6;
        client.update(&mut capped, &open, event(session_event::Of::Item(item(order, it))));
        client.agrees(&capped, &open, &format!("item {order}"));
    }
    assert!(part(&capped, 4).is_none(), "its prompt was cut");
}
