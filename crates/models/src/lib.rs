//! The protocol's promises, as properties a checker explores over the real code.
//!
//! A [`Machine`] is a slice of real state, the actions its environment may
//! take, and a transition that calls the real code. Its [`Property`] list is
//! the spec: each reads as a sentence and is a predicate over the state.
//! [`check`] explores every state breadth first within its [`Bounds`] and
//! reports the shortest path to a failing state; [`walk`] follows one schedule
//! chosen by a seed, so a seed alone reproduces a run.

use std::fmt::{self, Debug, Display};
use std::hash::Hash;
use std::sync::Arc;
use std::time::Instant;

use stateright::{Checker, Chooser, UniformChooser};

pub mod family;
mod held;
pub mod presence;
pub mod replication;
pub mod single_daemon;
pub use family::FamilyDeliveries;
pub use presence::PresenceRouting;
pub use replication::Replication;
pub use single_daemon::SingleDaemon;

/// What the world may do to a machine between its own actions: the one
/// fault vocabulary every model's environment and the simulator draw from,
/// and the only thing either simulates. Each model offers the faults its
/// slice of the world can suffer and says how each plays out on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fault {
    /// The message at this place in flight is lost. On an ordered stream
    /// the stream dies with it, taking everything behind it.
    Lose(MsgIndex),
    /// The message at this place in flight is held back behind the rest.
    Delay(MsgIndex),
    /// Two messages in flight swap places.
    Reorder(MsgIndex, MsgIndex),
    /// The host's process dies at this step; what it had written stays.
    Crash(Host),
    /// The host's process dies without its clean shutdown, and comes back
    /// knowing it did.
    RebootUnclean(Host),
    /// The host loses power: what was written but not synced is gone.
    PowerLoss(Host),
    /// The host's drive refuses the next write it is asked for, as a full
    /// or failing drive does.
    WriteRefused(Host),
    /// The clock moves on.
    Tick { ms: u64 },
}

/// A message's place among those in flight, oldest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MsgIndex(pub usize);

/// A host of a model, by its place in the model's topology.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Host(pub u8);

/// Every parked property of the protocol's models: the model, the
/// property's sentence and why it is parked. The landing gate,
/// `just parked-check`, fails while this is not empty.
pub fn parked() -> Vec<(&'static str, &'static str, Parked)> {
    fn of<M: Machine>(name: &'static str, model: &M) -> Vec<(&'static str, &'static str, Parked)> {
        model
            .properties()
            .into_iter()
            .filter_map(|property| Some((name, property.sentence, property.parked?)))
            .collect()
    }
    let mut parked = of("replication", &Replication::ci());
    parked.extend(of("single daemon", &SingleDaemon::ci()));
    parked.extend(of("presence and routing", &PresenceRouting::ci()));
    parked.extend(of("family and deliveries", &FamilyDeliveries::ci()));
    parked
}

/// How many threads a model's test explores with: `MODELS_THREADS` when
/// set, so a run can price a model on a CI runner's core count, otherwise
/// every core. The verdict and the state count are the same on any number
/// of threads; only which of several shortest counterexamples is printed
/// may differ.
pub fn threads() -> usize {
    std::env::var("MODELS_THREADS")
        .ok()
        .and_then(|threads| threads.parse().ok())
        .filter(|threads| *threads > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |threads| threads.get()))
}

/// Turns off SQLite's memory statistics for this process before any
/// database opens. Every SQLite allocation otherwise takes one global lock
/// to count itself, so checker threads exploring real stores side by side
/// spend their time waiting on each other. Nothing reads the statistics.
/// Too late once a database has opened, when SQLite refuses the change and
/// keeps counting.
pub(crate) fn share_sqlite_across_threads() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: configuration before SQLite initializes, with the integer
        // argument this option takes; afterwards it returns SQLITE_MISUSE and
        // changes nothing.
        unsafe {
            rusqlite::ffi::sqlite3_config(
                rusqlite::ffi::SQLITE_CONFIG_MEMSTATUS,
                0 as std::ffi::c_int,
            );
        }
    });
}

/// A model: real state, the actions that may happen to it, and its promises.
///
/// The state must be a value (cloned, hashed and compared per explored state);
/// a model over a store keeps the store's canonical dump and rebuilds the store
/// to take a transition.
pub trait Machine: Clone + Send + Sync + 'static {
    type State: Clone + Hash + Eq + Debug + Send + Sync + 'static;
    type Action: Clone + PartialEq + Debug + Send + Sync + 'static;

    /// The one state every exploration starts from.
    fn init(&self) -> Self::State;
    /// Every action the environment may take from `state`.
    fn actions(&self, state: &Self::State, out: &mut Vec<Self::Action>);
    /// The state after `action`, or `None` when the action is not enabled.
    fn next(&self, state: &Self::State, action: &Self::Action) -> Option<Self::State>;
    /// The promises, each a sentence and its predicate.
    fn properties(&self) -> Vec<Property<Self>>;
}

/// How a property is judged over the explored states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyKind {
    /// Holds in every explored state.
    Always,
    /// Holds in at least one explored state; a property that is never reached
    /// fails, which keeps an `Always` from passing on a machine that does
    /// nothing.
    Sometimes,
    /// Holds somewhere on every path that ends because no action is enabled.
    /// The checker has no fairness, so a model writes its liveness in with a
    /// bound on consecutive faults.
    Eventually,
}

/// One promise: a sentence a person would argue over, and its predicate.
pub struct Property<M: Machine> {
    pub kind: PropertyKind,
    pub sentence: &'static str,
    pub holds: fn(&M, &M::State) -> bool,
    /// Set while a decision about the property waits: the checker reports
    /// it with its reason instead of exploring it.
    pub parked: Option<Parked>,
}

/// A property whose decision waits, with the counterexample that raised it
/// and why it waits. The property keeps its sentence and predicate as they
/// were written; it is never weakened to pass. `just parked-check` fails
/// while any property is parked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parked {
    /// The date it was parked.
    pub since: &'static str,
    /// The path to the failing state, as the checker printed it, in short.
    pub counterexample: &'static str,
    /// What decision it waits on.
    pub reason: &'static str,
}

impl<M: Machine> Property<M> {
    pub fn always(sentence: &'static str, holds: fn(&M, &M::State) -> bool) -> Self {
        Self {
            kind: PropertyKind::Always,
            sentence,
            holds,
            parked: None,
        }
    }

    pub fn sometimes(sentence: &'static str, holds: fn(&M, &M::State) -> bool) -> Self {
        Self {
            kind: PropertyKind::Sometimes,
            sentence,
            holds,
            parked: None,
        }
    }

    pub fn eventually(sentence: &'static str, holds: fn(&M, &M::State) -> bool) -> Self {
        Self {
            kind: PropertyKind::Eventually,
            sentence,
            holds,
            parked: None,
        }
    }

    /// Parks the property while a decision about it waits.
    pub fn parked(self, parked: Parked) -> Self {
        Self {
            parked: Some(parked),
            ..self
        }
    }
}

impl<M: Machine> Clone for Property<M> {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind,
            sentence: self.sentence,
            holds: self.holds,
            parked: self.parked,
        }
    }
}

/// How far an exploration goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// The longest path explored, in actions. A checked state is one reached
    /// by fewer actions; `None` explores until no new state appears. A walk
    /// needs a depth.
    pub depth: Option<usize>,
    /// Exploration threads. One keeps the reported counterexample the same on
    /// every run; more only change which of several equally short paths is
    /// reported.
    pub threads: usize,
}

impl Bounds {
    /// Every reachable state, on one thread.
    pub const fn exhaustive() -> Self {
        Self {
            depth: None,
            threads: 1,
        }
    }

    /// Paths of at most `depth` actions, on one thread.
    pub const fn depth(depth: usize) -> Self {
        Self {
            depth: Some(depth),
            threads: 1,
        }
    }

    pub const fn threads(self, threads: usize) -> Self {
        Self { threads, ..self }
    }
}

/// What an exploration cost.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measured {
    /// Distinct states explored.
    pub states: u64,
    pub seconds: f64,
    pub states_per_second: f64,
}

impl Measured {
    fn new(states: u64, seconds: f64) -> Self {
        let states_per_second = if seconds > 0.0 {
            states as f64 / seconds
        } else {
            0.0
        };
        Self {
            states,
            seconds,
            states_per_second,
        }
    }
}

/// A path from the initial state to the state that decided a property.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counterexample {
    /// Each action taken, in order.
    pub actions: Vec<String>,
    /// The state each action reached, in order; the last is `state`.
    pub states: Vec<String>,
    /// The state the last action reached.
    pub state: String,
}

/// How one property fared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Holds,
    /// An `Always` reached a state where it does not hold, or an `Eventually`
    /// reached the end of a path without holding on it.
    Fails(Counterexample),
    /// A `Sometimes` held in no explored state.
    NeverReached,
    /// Not explored: the property is parked while a decision waits.
    Parked(Parked),
}

/// One property's sentence and outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Judged {
    pub kind: PropertyKind,
    pub sentence: &'static str,
    pub outcome: Outcome,
}

/// The result of [`check`].
#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    pub properties: Vec<Judged>,
    pub measured: Measured,
    /// The longest path explored, in actions.
    pub depth: usize,
}

impl Verdict {
    pub fn holds(&self) -> bool {
        self.properties
            .iter()
            .all(|judged| matches!(judged.outcome, Outcome::Holds | Outcome::Parked(_)))
    }

    /// The outcome of the property with this sentence.
    pub fn outcome(&self, sentence: &str) -> Option<&Outcome> {
        self.properties
            .iter()
            .find(|judged| judged.sentence == sentence)
            .map(|judged| &judged.outcome)
    }

    /// Panics with the whole report unless every property holds.
    #[track_caller]
    pub fn assert_holds(&self) {
        assert!(self.holds(), "a property does not hold\n{self}");
    }
}

impl Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Measured {
            states,
            seconds,
            states_per_second,
        } = self.measured;
        writeln!(
            f,
            "{states} states to depth {} in {seconds:.3}s ({states_per_second:.0} states per second)",
            self.depth
        )?;
        for judged in &self.properties {
            let (mark, path) = match &judged.outcome {
                Outcome::Holds => ("holds", None),
                Outcome::Fails(path) => ("FAILS", Some(path)),
                Outcome::NeverReached => ("NEVER REACHED", None),
                Outcome::Parked(_) => ("PARKED", None),
            };
            writeln!(f, "  {mark}: {:?} {}", judged.kind, judged.sentence)?;
            if let Outcome::Parked(parked) = &judged.outcome {
                writeln!(f, "    since {}: {}", parked.since, parked.reason)?;
                writeln!(f, "    counterexample: {}", parked.counterexample)?;
            }
            if let Some(path) = path {
                for (step, (action, state)) in path.actions.iter().zip(&path.states).enumerate() {
                    writeln!(f, "    {}. {action}\n       {state}", step + 1)?;
                }
            }
        }
        Ok(())
    }
}

/// Explores every state of `model` within `bounds`, breadth first, judging
/// every property in every explored state.
pub fn check<M: Machine>(model: &M, bounds: Bounds) -> Verdict {
    let adapter = Adapter::new(model);
    let properties = model.properties();
    let mut builder = stateright::Model::checker(adapter).threads(bounds.threads.max(1));
    if let Some(depth) = bounds.depth {
        builder = builder.target_max_depth(depth);
    }
    let started = Instant::now();
    let checker = builder.spawn_bfs().join();
    let seconds = started.elapsed().as_secs_f64();

    let judged = properties
        .iter()
        .map(|property| {
            if let Some(parked) = property.parked {
                return Judged {
                    kind: property.kind,
                    sentence: property.sentence,
                    outcome: Outcome::Parked(parked),
                };
            }
            let found = checker.discovery(property.sentence);
            let outcome = match (property.kind, found) {
                (PropertyKind::Sometimes, Some(_)) => Outcome::Holds,
                (PropertyKind::Sometimes, None) => Outcome::NeverReached,
                (_, None) => Outcome::Holds,
                (_, Some(path)) => {
                    let state = format!("{:?}", path.last_state());
                    let steps = path.into_vec();
                    Outcome::Fails(Counterexample {
                        state,
                        // Each pair is a state and the action taken from it.
                        actions: steps
                            .iter()
                            .filter_map(|(_, action)| action.as_ref())
                            .map(|action| format!("{action:?}"))
                            .collect(),
                        states: steps
                            .iter()
                            .skip(1)
                            .map(|(state, _)| format!("{state:?}"))
                            .collect(),
                    })
                }
            };
            Judged {
                kind: property.kind,
                sentence: property.sentence,
                outcome,
            }
        })
        .collect();
    Verdict {
        properties: judged,
        measured: Measured::new(checker.unique_state_count() as u64, seconds),
        depth: checker.max_depth(),
    }
}

/// A property that failed during a walk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalkFailure {
    pub kind: PropertyKind,
    pub sentence: &'static str,
    /// How many of the walk's actions led to the failing state.
    pub after: usize,
}

/// One schedule of a model, chosen by a seed.
pub struct Walk<M: Machine> {
    pub seed: u64,
    /// Each action taken, in order; on a failure, the path to the failing state.
    pub actions: Vec<M::Action>,
    /// The first property the walk broke, if any.
    pub failure: Option<WalkFailure>,
    /// The `Sometimes` properties the walk reached.
    pub reached: Vec<&'static str>,
}

impl<M: Machine> Display for Walk<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.failure {
            None => writeln!(
                f,
                "seed {}: {} steps, every property held",
                self.seed,
                self.actions.len()
            )?,
            Some(failure) => writeln!(
                f,
                "seed {}: {:?} {} failed after {} steps",
                self.seed, failure.kind, failure.sentence, failure.after
            )?,
        }
        for (step, action) in self.actions.iter().enumerate() {
            writeln!(f, "  {}. {action:?}", step + 1)?;
        }
        Ok(())
    }
}

/// Follows one schedule of `model` for at most `bounds.depth` actions, each
/// chosen by the seeded chooser among the enabled actions. One seed gives one
/// schedule on every run; every property is judged on every state reached.
pub fn walk<M: Machine>(model: &M, bounds: Bounds, seed: u64) -> Walk<M> {
    let depth = bounds.depth.expect("a walk needs a depth bound");
    let properties: Vec<_> = model
        .properties()
        .into_iter()
        .filter(|property| property.parked.is_none())
        .collect();
    let mut chooser = <UniformChooser as Chooser<Adapter<M>>>::new_state(&UniformChooser, seed);
    let mut walk = Walk {
        seed,
        actions: Vec::new(),
        failure: None,
        reached: Vec::new(),
    };
    let mut pending_eventually: Vec<bool> = properties
        .iter()
        .map(|property| property.kind == PropertyKind::Eventually)
        .collect();
    let mut state = model.init();
    let mut actions = Vec::new();

    loop {
        for (index, property) in properties.iter().enumerate() {
            let holds = (property.holds)(model, &state);
            match property.kind {
                PropertyKind::Always if !holds => {
                    walk.failure = Some(WalkFailure {
                        kind: property.kind,
                        sentence: property.sentence,
                        after: walk.actions.len(),
                    });
                    return walk;
                }
                PropertyKind::Sometimes if holds && !walk.reached.contains(&property.sentence) => {
                    walk.reached.push(property.sentence);
                }
                PropertyKind::Eventually if holds => pending_eventually[index] = false,
                _ => {}
            }
        }
        if walk.actions.len() >= depth {
            return walk;
        }

        actions.clear();
        model.actions(&state, &mut actions);
        let next = loop {
            if actions.is_empty() {
                break None;
            }
            let index = <UniformChooser as Chooser<Adapter<M>>>::choose_action(
                &UniformChooser,
                &mut chooser,
                &state,
                &actions,
            );
            let action = actions.swap_remove(index);
            if let Some(next) = model.next(&state, &action) {
                break Some((action, next));
            }
        };
        let Some((action, next)) = next else {
            // No action is enabled: the path has ended, so every eventually
            // property must have held somewhere on it.
            if let Some(index) = pending_eventually.iter().position(|pending| *pending) {
                walk.failure = Some(WalkFailure {
                    kind: PropertyKind::Eventually,
                    sentence: properties[index].sentence,
                    after: walk.actions.len(),
                });
            }
            return walk;
        };
        walk.actions.push(action);
        state = next;
    }
}

/// Binds a [`Machine`] to the checker's model trait.
struct Adapter<M: Machine> {
    machine: M,
    properties: Arc<Vec<Property<M>>>,
}

impl<M: Machine> Adapter<M> {
    fn new(machine: &M) -> Self {
        let properties = machine.properties();
        assert!(
            properties.len() <= MAX_PROPERTIES,
            "a machine has {} properties; the checker binds at most {MAX_PROPERTIES}",
            properties.len()
        );
        for (index, property) in properties.iter().enumerate() {
            assert!(
                properties[..index]
                    .iter()
                    .all(|earlier| earlier.sentence != property.sentence),
                "two properties share the sentence {:?}",
                property.sentence
            );
        }
        Self {
            machine: machine.clone(),
            properties: Arc::new(
                properties
                    .into_iter()
                    .filter(|property| property.parked.is_none())
                    .collect(),
            ),
        }
    }
}

impl<M: Machine> stateright::Model for Adapter<M> {
    type State = M::State;
    type Action = M::Action;

    fn init_states(&self) -> Vec<Self::State> {
        vec![self.machine.init()]
    }

    fn actions(&self, state: &Self::State, actions: &mut Vec<Self::Action>) {
        self.machine.actions(state, actions);
    }

    fn next_state(&self, state: &Self::State, action: Self::Action) -> Option<Self::State> {
        self.machine.next(state, &action)
    }

    fn properties(&self) -> Vec<stateright::Property<Self>> {
        let conditions = conditions::<M>();
        self.properties
            .iter()
            .zip(conditions)
            .map(|(property, condition)| match property.kind {
                PropertyKind::Always => stateright::Property::always(property.sentence, condition),
                PropertyKind::Sometimes => {
                    stateright::Property::sometimes(property.sentence, condition)
                }
                PropertyKind::Eventually => {
                    stateright::Property::eventually(property.sentence, condition)
                }
            })
            .collect()
    }
}

// The checker takes a property's predicate as a plain function pointer over
// its own model type, so each of a machine's properties is reached through a
// function monomorphized on the property's index.
const MAX_PROPERTIES: usize = 32;

type Condition<M> = fn(&Adapter<M>, &<M as Machine>::State) -> bool;

fn condition<M: Machine, const INDEX: usize>(adapter: &Adapter<M>, state: &M::State) -> bool {
    (adapter.properties[INDEX].holds)(&adapter.machine, state)
}

macro_rules! conditions {
    ($($index:literal)*) => {
        fn conditions<M: Machine>() -> [Condition<M>; MAX_PROPERTIES] {
            [$(condition::<M, $index>),*]
        }
    };
}

conditions!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31);
