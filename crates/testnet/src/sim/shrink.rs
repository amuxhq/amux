//! Making a failing seed's run shorter.
//!
//! A run is its seed's ring: the choices it made, one per delivery.
//! [`shrink`] looks first among other seeds for a run that fails the same
//! way in fewer deliveries, then cuts the best ring: it replays ever
//! shorter prefixes of it, the net delivering in its own order once a
//! prefix runs out, and keeps the shortest prefix that still fails the same
//! way. What it returns replays on demand with [`SimNet::replay`].

use std::future::Future;
use std::ops::Range;

use super::{Choice, Faults, SimNet, SimOptions};
use crate::modes::attempt;
use crate::topology::Topology;

/// A failing run as short as the search found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reproduction {
    /// The seed whose run it cuts.
    pub seed: u64,
    pub faults: Faults,
    /// The choices to replay; past them the net delivers in its own order.
    pub ring: Vec<Choice>,
    /// The deliveries the replayed run makes before it fails.
    pub deliveries: usize,
    /// What it fails on.
    pub failure: String,
}

impl Reproduction {
    pub fn options(&self) -> SimOptions {
        SimOptions {
            seed: self.seed,
            faults: self.faults,
        }
    }
}

/// How a run went: what it failed on, if it did, and the ring it made.
struct Run {
    failure: Option<String>,
    ring: Vec<Choice>,
}

/// Runs `story` on `net` to its end and reads back the ring it made.
async fn run<S>(net: SimNet, story: &impl Fn(SimNet) -> S) -> Run
where
    S: Future<Output = ()>,
{
    let reader = net.ring_reader();
    let failure = attempt(story(net)).await.err();
    Run {
        failure,
        ring: reader.ring(),
    }
}

/// Two failures are one when they read alike but for their numbers: the
/// seed, the step count and the times a message names.
pub fn same_failure(a: &str, b: &str) -> bool {
    let shape = |text: &str| -> String {
        text.lines()
            .next()
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_ascii_digit())
            .collect()
    };
    shape(a) == shape(b)
}

/// Shortens the run `failing` makes of `story` on `topology` under
/// `faults`: first by looking through `search` for a seed that fails the
/// same way in fewer deliveries, then by cutting that seed's ring to the
/// shortest prefix that still fails the same way. `None` when `failing`
/// does not fail.
pub async fn shrink<S>(
    topology: fn() -> Topology,
    faults: Faults,
    failing: u64,
    search: Range<u64>,
    story: impl Fn(SimNet) -> S,
) -> Option<Reproduction>
where
    S: Future<Output = ()>,
{
    let start = |seed: u64| {
        SimNet::start_with(topology(), SimOptions { seed, faults })
            .unwrap_or_else(|error| panic!("seed {seed}: the net starts: {error}"))
    };
    let first = run(start(failing), &story).await;
    let failure = first.failure?;
    let mut best = (failing, first.ring, failure.clone());
    for seed in search {
        let other = run(start(seed), &story).await;
        if let Some(other_failure) = other.failure
            && same_failure(&other_failure, &failure)
            && other.ring.len() < best.1.len()
        {
            best = (seed, other.ring, other_failure);
        }
    }

    let (seed, ring, failure) = best;
    let options = SimOptions { seed, faults };
    let replay = |prefix: &[Choice]| {
        SimNet::replay(topology(), options, prefix.to_vec())
            .unwrap_or_else(|error| panic!("seed {seed}: the replay starts: {error}"))
    };
    let mut reproduction = Reproduction {
        seed,
        faults,
        deliveries: ring.len(),
        ring: ring.clone(),
        failure,
    };
    // The whole ring fails as the run did; find the shortest prefix that
    // still does, by halving between a prefix that passes and one that
    // fails.
    let (mut passes, mut fails) = (0, ring.len());
    let empty = run(replay(&[]), &story).await;
    if let Some(empty_failure) = &empty.failure
        && same_failure(empty_failure, &reproduction.failure)
    {
        fails = 0;
        reproduction.deliveries = empty.ring.len();
        reproduction.failure = empty_failure.clone();
    }
    while passes + 1 < fails {
        let middle = (passes + fails) / 2;
        let cut = run(replay(&ring[..middle]), &story).await;
        match cut.failure {
            Some(cut_failure) if same_failure(&cut_failure, &reproduction.failure) => {
                fails = middle;
                reproduction.deliveries = cut.ring.len();
                reproduction.failure = cut_failure;
            }
            _ => passes = middle,
        }
    }
    reproduction.ring.truncate(fails);
    Some(reproduction)
}
