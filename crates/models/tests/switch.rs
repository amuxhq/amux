//! The checker on the smallest machine there is: a switch that flips a lamp,
//! and a fragile lamp that may burn out for good.

use models::{Bounds, Machine, Outcome, Parked, Property, check, walk};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Lamp {
    Off,
    On,
    Burnt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Act {
    Flip,
    BurnOut,
}

#[derive(Clone)]
struct Switch {
    /// Whether a lit lamp may burn out, which breaks the switch's promise.
    fragile: bool,
}

impl Machine for Switch {
    type State = Lamp;
    type Action = Act;

    fn init(&self) -> Lamp {
        Lamp::Off
    }

    fn actions(&self, _: &Lamp, out: &mut Vec<Act>) {
        out.push(Act::Flip);
        if self.fragile {
            out.push(Act::BurnOut);
        }
    }

    fn next(&self, lamp: &Lamp, act: &Act) -> Option<Lamp> {
        match (lamp, act) {
            (Lamp::Off, Act::Flip) => Some(Lamp::On),
            (Lamp::On, Act::Flip) => Some(Lamp::Off),
            (Lamp::On, Act::BurnOut) => Some(Lamp::Burnt),
            _ => None,
        }
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("the lamp never burns out", |_, lamp| *lamp != Lamp::Burnt),
            Property::sometimes("the lamp is sometimes lit", |_, lamp| *lamp == Lamp::On),
        ]
    }
}

#[test]
fn every_property_of_a_sound_switch_holds() {
    let verdict = check(&Switch { fragile: false }, Bounds::exhaustive());
    println!("{verdict}");
    verdict.assert_holds();
    assert_eq!(verdict.measured.states, 2);
}

#[test]
fn a_wrong_promise_fails_with_the_shortest_path_to_it() {
    let verdict = check(&Switch { fragile: true }, Bounds::depth(10));
    println!("{verdict}");
    assert!(!verdict.holds());
    let Some(Outcome::Fails(path)) = verdict.outcome("the lamp never burns out") else {
        panic!("the wrong promise should fail\n{verdict}");
    };
    assert_eq!(path.actions, ["Flip", "BurnOut"]);
    assert_eq!(path.state, "Burnt");
    assert_eq!(
        verdict.outcome("the lamp is sometimes lit"),
        Some(&Outcome::Holds)
    );
}

#[test]
fn a_sometimes_never_reached_fails() {
    #[derive(Clone)]
    struct Stuck;
    impl Machine for Stuck {
        type State = Lamp;
        type Action = Act;
        fn init(&self) -> Lamp {
            Lamp::Off
        }
        fn actions(&self, _: &Lamp, _: &mut Vec<Act>) {}
        fn next(&self, _: &Lamp, _: &Act) -> Option<Lamp> {
            None
        }
        fn properties(&self) -> Vec<Property<Self>> {
            vec![Property::sometimes(
                "the lamp is sometimes lit",
                |_, lamp| *lamp == Lamp::On,
            )]
        }
    }
    let verdict = check(&Stuck, Bounds::exhaustive());
    assert_eq!(
        verdict.outcome("the lamp is sometimes lit"),
        Some(&Outcome::NeverReached)
    );
}

#[test]
fn an_unmet_eventually_fails_where_its_path_ends() {
    // A sturdy lamp never burns out, so a promise that it will cannot hold
    // on a path that ends; the only ending is cut by a lamp with no action.
    #[derive(Clone)]
    struct Once;
    impl Machine for Once {
        type State = Lamp;
        type Action = Act;
        fn init(&self) -> Lamp {
            Lamp::Off
        }
        fn actions(&self, _: &Lamp, out: &mut Vec<Act>) {
            out.push(Act::Flip);
        }
        fn next(&self, lamp: &Lamp, _: &Act) -> Option<Lamp> {
            (*lamp == Lamp::Off).then_some(Lamp::On)
        }
        fn properties(&self) -> Vec<Property<Self>> {
            vec![Property::eventually("the lamp burns out", |_, lamp| {
                *lamp == Lamp::Burnt
            })]
        }
    }
    let verdict = check(&Once, Bounds::exhaustive());
    let Some(Outcome::Fails(path)) = verdict.outcome("the lamp burns out") else {
        panic!("the unmet eventually should fail\n{verdict}");
    };
    assert_eq!(path.actions, ["Flip"]);
    let walked = walk(&Once, Bounds::depth(10), 7);
    assert_eq!(
        walked.failure.map(|failure| failure.sentence),
        Some("the lamp burns out")
    );
}

#[test]
fn one_seed_walks_one_schedule() {
    let switch = Switch { fragile: true };
    let first = walk(&switch, Bounds::depth(12), 3);
    let again = walk(&switch, Bounds::depth(12), 3);
    assert_eq!(first.actions, again.actions);
    assert_eq!(first.failure, again.failure);

    let schedules: std::collections::HashSet<Vec<Act>> = (0..32)
        .map(|seed| walk(&switch, Bounds::depth(12), seed).actions)
        .collect();
    assert!(
        schedules.len() > 1,
        "different seeds should walk different schedules"
    );
}

#[test]
fn a_walk_reports_the_seed_and_prefix_of_a_failure() {
    let switch = Switch { fragile: true };
    let failing = (0..64)
        .map(|seed| walk(&switch, Bounds::depth(12), seed))
        .find(|walked| walked.failure.is_some())
        .expect("some seed burns the lamp out");
    println!("{failing}");
    let failure = failing.failure.clone().unwrap();
    assert_eq!(failure.sentence, "the lamp never burns out");
    assert_eq!(failure.after, failing.actions.len());
    assert_eq!(failing.actions.last(), Some(&Act::BurnOut));

    let replayed = walk(&switch, Bounds::depth(12), failing.seed);
    assert_eq!(replayed.actions, failing.actions);
    assert_eq!(replayed.failure, failing.failure);
}

#[test]
fn a_parked_property_is_reported_with_its_reason_and_not_explored() {
    #[derive(Clone)]
    struct Waiting;
    const PARKED: Parked = Parked {
        since: "2026-10-18",
        counterexample: "Flip, BurnOut",
        reason: "whether a lamp may burn out waits on a decision",
    };
    impl Machine for Waiting {
        type State = Lamp;
        type Action = Act;
        fn init(&self) -> Lamp {
            Switch { fragile: true }.init()
        }
        fn actions(&self, lamp: &Lamp, out: &mut Vec<Act>) {
            Switch { fragile: true }.actions(lamp, out);
        }
        fn next(&self, lamp: &Lamp, act: &Act) -> Option<Lamp> {
            Switch { fragile: true }.next(lamp, act)
        }
        fn properties(&self) -> Vec<Property<Self>> {
            vec![
                Property::always("the lamp never burns out", |_, lamp: &Lamp| {
                    *lamp != Lamp::Burnt
                })
                .parked(PARKED),
                Property::sometimes("the lamp is sometimes lit", |_, lamp: &Lamp| {
                    *lamp == Lamp::On
                }),
            ]
        }
    }
    let verdict = check(&Waiting, Bounds::exhaustive());
    println!("{verdict}");
    assert!(verdict.holds(), "a parked property keeps the verdict green");
    assert_eq!(
        verdict.outcome("the lamp never burns out"),
        Some(&Outcome::Parked(PARKED))
    );
    assert!(
        verdict
            .to_string()
            .contains("whether a lamp may burn out waits on a decision")
    );
    assert!((0..32).all(|seed| walk(&Waiting, Bounds::depth(12), seed).failure.is_none()));
}
