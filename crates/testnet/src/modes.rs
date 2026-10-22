//! Which net a story runs on, and under which seeds.
//!
//! A story written against [`crate::World`] runs on the real net by
//! default. Under `TESTNET_MODE=simulated` it runs on [`SimNet`] instead,
//! once per seed: a few by default, `TESTNET_SEEDS=N` for N of them (the
//! nightly walk), `TESTNET_SEED=N` for exactly one, which is how a failing
//! seed is reproduced. A seed that fails is printed with the command that
//! reruns it, the remaining seeds still run, and the story fails at the end
//! naming every failing seed. `just sim` runs every suite the test
//! catalogue says has a simulated mode this way.

use std::any::Any;
use std::future::Future;
use std::ops::Range;
use std::panic::AssertUnwindSafe;
use std::time::Instant;

use futures_util::FutureExt as _;

use crate::net::Net;
use crate::sim::SimNet;
use crate::topology::Topology;

/// The variable that picks the net a story runs on.
pub const MODE_ENV: &str = "TESTNET_MODE";
/// The variable that names the one seed a simulated story runs under.
pub const SEED_ENV: &str = "TESTNET_SEED";
/// The variable that says how many seeds, from 0, a simulated story runs
/// under.
pub const SEEDS_ENV: &str = "TESTNET_SEEDS";
/// Seeds a simulated story runs under when nothing says otherwise: enough
/// to see several schedules, few enough for the fast lane.
pub const DEFAULT_SEEDS: u64 = 6;

/// The net a story runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Production daemons and agent processes on loopback.
    Real,
    /// Every core in one process, one schedule per seed.
    Simulated,
}

/// The mode `TESTNET_MODE` names: `real` (or nothing) or `simulated`.
///
/// # Panics
///
/// On any other value, so a misspelt mode never quietly runs real.
pub fn mode() -> Mode {
    match std::env::var(MODE_ENV).as_deref() {
        Err(_) | Ok("" | "real") => Mode::Real,
        Ok("simulated") => Mode::Simulated,
        Ok(other) => panic!("{MODE_ENV}={other:?}: the modes are real and simulated"),
    }
}

/// The seeds a simulated story runs under: `TESTNET_SEED` alone if set,
/// otherwise the first `TESTNET_SEEDS` (by default [`DEFAULT_SEEDS`]).
///
/// # Panics
///
/// When either variable is not a number.
pub fn seeds() -> Range<u64> {
    let number = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| {
                value
                    .parse::<u64>()
                    .unwrap_or_else(|_| panic!("{name}={value:?} is not a number"))
            })
    };
    match number(SEED_ENV) {
        Some(seed) => seed..seed + 1,
        None => 0..number(SEEDS_ENV).unwrap_or(DEFAULT_SEEDS),
    }
}

/// Runs a story on the net [`mode`] names: on the real net once, or on the
/// simulated net under each of [`seeds`], every seed starting the topology
/// afresh. `real` and `simulated` are the same story, given once for each
/// net's type.
///
/// # Panics
///
/// When the story fails: on the real net as it failed, on the simulated
/// net once every seed has run, naming the seeds that failed.
pub async fn run_story<R, S>(
    topology: fn() -> Topology,
    real: impl FnOnce(Net) -> R,
    simulated: impl Fn(SimNet) -> S,
) where
    R: Future<Output = ()>,
    S: Future<Output = ()>,
{
    match mode() {
        Mode::Real => {
            let net = Net::start(topology())
                .await
                .unwrap_or_else(|error| panic!("the net starts: {error}"));
            real(net).await;
        }
        Mode::Simulated => {
            // The net starts inside the seed's run, so a start that fails
            // is that seed's failure and the walk goes on.
            let simulated = &simulated;
            walk_seeds(|seed| async move {
                let net = SimNet::start(topology(), seed)
                    .unwrap_or_else(|error| panic!("seed {seed}: the net starts: {error}"));
                simulated(net).await;
            })
            .await;
        }
    }
}

/// Runs a simulated story under each of [`seeds`], `story` making the run
/// of one seed (it starts its own net, so it may choose its own
/// `SimOptions`), and fails once all have run if any seed failed.
///
/// # Panics
///
/// When a seed's run failed, naming every failing seed.
pub async fn walk_seeds<S>(story: impl Fn(u64) -> S)
where
    S: Future<Output = ()>,
{
    let name = story_name();
    let seeds = seeds();
    let started = Instant::now();
    let failing: Vec<u64> = walk(seeds.clone(), story)
        .await
        .into_iter()
        .map(|(seed, failure)| {
            eprintln!(
                "seed {seed} failed: {failure}\n  reproduce it with: just sim --seed {seed} {name}"
            );
            seed
        })
        .collect();
    // The line the sim recipe reads its report from.
    println!(
        "simulated story {name}: {} seeds walked in {:.2}s, failing seeds {failing:?}",
        seeds.end - seeds.start,
        started.elapsed().as_secs_f64()
    );
    assert!(
        failing.is_empty(),
        "{name} failed under seeds {failing:?}; reproduce one with `just sim --seed {} {name}`",
        failing[0]
    );
}

/// Runs `story` under each of `seeds` and returns the seeds whose run
/// failed, each with what it failed on, without failing itself. A seed
/// fails as well when making its run panics, before the run is awaited.
pub async fn walk<S>(seeds: Range<u64>, story: impl Fn(u64) -> S) -> Vec<(u64, String)>
where
    S: Future<Output = ()>,
{
    let mut failing = Vec::new();
    for seed in seeds {
        if let Err(failure) = attempt(async { story(seed).await }).await {
            failing.push((seed, failure));
        }
    }
    failing
}

/// Runs one story's run to its end, catching the panic a failing story
/// ends in as what it failed on.
pub async fn attempt(run: impl Future<Output = ()>) -> Result<(), String> {
    AssertUnwindSafe(run)
        .catch_unwind()
        .await
        .map_err(|panic| panic_text(&*panic).to_owned())
}

/// The test running this story: libtest names its thread after it.
fn story_name() -> String {
    std::thread::current()
        .name()
        .filter(|name| *name != "main")
        .unwrap_or("story")
        .to_owned()
}

fn panic_text(panic: &(dyn Any + Send)) -> &str {
    panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("a panic without a message")
}
