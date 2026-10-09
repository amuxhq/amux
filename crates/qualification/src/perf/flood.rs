//! The flood: twenty agents streaming at once on one host, and a
//! second runtime with no block opening the fleet and one chat.
//!
//! One run measures what the parameters table promises under that load:
//! how long the fleet and a chat take to reach CaughtUp on a runtime that
//! holds nothing yet, how far ingest trails the journals, how fast the
//! journals grow while the daemon is dead and how long it takes to drain
//! them after, what a replica's catch-up costs at a distance under and
//! over K, and each agent process's memory.
//!
//! "Full rate" is the fastest a real provider streams, not the fastest a
//! fake can write: see [`PACE_MS`]. What the daemon could take beyond that
//! is measured apart, with the agents unthrottled, as ingest's cost per
//! committed frame.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use chrono::Utc;
use node::Launch;
use provider_fakes::script::{Script, Step};
use testnet::observe::{self, Mark, marks};
use testnet::{AgentDecl, ClockMode, HostDecl, Net, NetOptions, PATIENCE, Topology};
use wire::{InventoryEvent, SessionEvent, inventory_event};

use super::{Metric, MetricRun, Sample, Statistic, Unit, Workload, sample_memory};

/// The host whose agents flood.
pub const ORIGIN: &str = "desk";
/// The runtime that opens the fleet and one chat with no block.
pub const VIEWER: &str = "phone";
/// The replica tail and catch-up cap, K in the parameters table.
pub const K: u32 = 200;
/// The tail a client asks for on open, N in the parameters table; at
/// most K.
pub const N: u32 = 40;

/// How big a flood is and how long each phase samples.
#[derive(Clone, Debug)]
pub struct FloodOptions {
    pub agents: usize,
    /// Messages each agent's one turn emits before it ends.
    pub messages: usize,
    /// The pause between one agent's messages; none is the fake's
    /// unthrottled rate.
    pub pace_ms: Option<u64>,
    /// The replica tail and catch-up cap every host runs with.
    pub k: u32,
    /// The revisions every agent's history reaches before the viewer
    /// connects: well past K.
    pub history: u64,
    /// How long ingest lag and memory are sampled.
    pub sampling: Duration,
    /// How long the daemon stays dead while its journals grow.
    pub outage: Duration,
    /// The journal bytes the capacity phase has the agents write ahead of
    /// ingest before it drains them.
    pub backlog: u64,
}

impl FloodOptions {
    /// The measured flood: twenty agents.
    pub fn full() -> Self {
        Self {
            agents: 20,
            messages: 10_000_000,
            pace_ms: Some(PACE_MS),
            k: K,
            // About six seconds of flood at full rate.
            history: 300,
            sampling: Duration::from_secs(3),
            outage: Duration::from_secs(2),
            // About 115 000 frames, several seconds of draining.
            backlog: 16 << 20,
        }
    }

    /// The smoke the ordinary test lane runs: three agents, short phases.
    pub fn smoke() -> Self {
        Self {
            agents: 3,
            messages: 10_000_000,
            pace_ms: Some(PACE_MS),
            // A small K keeps the distance past it a couple of seconds of
            // flood.
            k: 20,
            history: 40,
            sampling: Duration::from_millis(500),
            outage: Duration::from_millis(500),
            // About 36 000 frames: enough for every window to span eight
            // ingest batches.
            backlog: 5 << 20,
        }
    }
}

/// The pause between one agent's messages in the measured flood: full
/// rate, the fastest a real provider streams.
///
/// Every recording in the claude-specs (headless and terminal) and
/// codex-specs corpora peaks at 47 provider frames in any one second, the
/// next highest at 40, with median gaps between frames of 20 to 90 ms. A
/// message every 20 ms is about 25 messages and 46 revisions a second per
/// agent, at or above any recorded provider, so twenty of them put about
/// a thousand frames a second on one host. An unthrottled fake writes tens
/// of thousands a second per process, which no provider does; nothing
/// holds an agent back while ingest trails (its journal is the buffer), so
/// under that load ingest lag grows without bound by design, and the
/// capacity phase measures that load as a cost per frame instead.
pub const PACE_MS: u64 = 20;

/// One agent's name in the flood.
pub fn agent_name(at: usize) -> String {
    format!("flood-{at:02}")
}

/// What every flood agent plays: one turn of `messages` numbered messages,
/// `pace_ms` apart, then idle. The numbers let a person watching a client
/// see how far its chat is behind the origin's.
pub fn script(messages: usize, pace_ms: Option<u64>) -> Script {
    let mut message = vec![Step::Text {
        chunks: vec![
            "message {pass}: the agent reports progress on its task at full rate".to_owned(),
        ],
    }];
    if let Some(ms) = pace_ms {
        message.push(Step::Pause { ms });
    }
    Script {
        steps: vec![
            Step::Repeat {
                times: messages,
                steps: message,
                played: 0,
            },
            Step::TurnEnd,
        ],
        ..Script::default()
    }
}

/// The flood's topology: the origin with its agents, and the viewer
/// linked to it. The agents start with their first prompt.
pub fn topology(options: &FloodOptions) -> Topology {
    let script = script(options.messages, options.pace_ms);
    let mut topology = Topology::new()
        .host_decl(HostDecl {
            name: ORIGIN.to_owned(),
            lan: true,
            ..HostDecl::default()
        })
        .host(VIEWER)
        .link(ORIGIN, VIEWER);
    for at in 0..options.agents {
        let mut agent = AgentDecl::new(&agent_name(at), ORIGIN).prompt("flood");
        agent.script = Some(script.clone());
        topology = topology.agent(agent);
    }
    topology
}

const WORKLOAD: Workload = Workload {
    description: "flood: agents streaming at full rate, a message every 20 ms (at or above the fastest recorded provider, 47 frames in one second), on one host, a second runtime with no block opening the fleet and one chat",
    seed: 0,
    identity_growth: "one agent per flood slot, fixed",
    warm_up: "agents run before the viewer connects until each history is past K",
};

/// The metrics, their budgets and the basis for each budget in the
/// parameters table.
mod budgets {
    /// Fleet CaughtUp on a runtime with no block: one inventory stream
    /// over one link and one transaction writing the rows. The latency
    /// budget is a few ms per hop locally; the flood's twenty catch-ups
    /// starting at the same moment share the link, so a second is the
    /// ceiling.
    pub const FLEET_MS: f64 = 1_000.0;
    /// A chat's CaughtUp: a tail of K rows and a Snapshot through one
    /// source while nineteen others catch up beside it.
    pub const CHAT_MS: f64 = 2_000.0;
    /// Ingest lag: a record's commit trails its journal write by one
    /// read and one commit, a few ms locally; at twenty agents at full
    /// rate the p99 must stay within one fan-out ring's worth of work.
    pub const INGEST_LAG_MS: f64 = 250.0;
    /// Journal growth with the daemon dead: the agents' own write rate,
    /// recorded; the ceiling is twenty agents each rotating its 1 MiB
    /// segment once a second.
    pub const BACKLOG_GROWTH_MIB_PER_MIN: f64 = 20.0 * 60.0;
    /// Draining that backlog after the restart: every journal read to the
    /// end it had at restart.
    pub const BACKLOG_DRAIN_MS: f64 = 5_000.0;
    /// Catch-up after a break: a delta under K, a Reset and a tail of K
    /// over it; both bounded by K rows, so both within the chat budget.
    pub const CATCH_UP_MS: f64 = 2_000.0;
    /// An agent process: the facts ring (2-4 MB), two journal segments
    /// (1 MiB each) and the interpreter's state, with room for the
    /// runtime.
    pub const AGENT_MEMORY_MIB: f64 = 48.0;
    /// Ingest's cost per committed frame draining a backlog. Tens of
    /// microseconds a frame is what `INGEST_BATCH` is sized by; at 100 µs
    /// one host still commits ten thousand frames a second, ten times the
    /// full-rate flood's demand of about a thousand.
    pub const INGEST_COST_US: f64 = 100.0;
}

const CAPACITY_WORKLOAD: Workload = Workload {
    description: "flood capacity: agents write unthrottled on one host, far faster than any provider, while ingest is held until their journals hold a fixed backlog, then pause while ingest drains it alone; cost is wall time over frames committed in each seventh of the drain",
    seed: 0,
    identity_growth: "one agent per flood slot, fixed",
    warm_up: "agents write unthrottled, ingest held, until the journals hold the backlog",
};

/// A metric held to its budget alone. The flood's timings are one
/// observation each, or sub-millisecond medians, and vary several-fold
/// between runs on the same machine, so a drift limit on them would fail
/// runs for noise; the budget is what they must meet.
fn metric(name: &'static str, statistic: Statistic, budget: f64, unit: Unit) -> Metric {
    Metric {
        name,
        statistic,
        budget,
        unit,
        ceiling_only: true,
        workload: WORKLOAD,
    }
}

/// A metric held to its budget and to drift from the recorded baseline.
fn drifting(name: &'static str, statistic: Statistic, budget: f64, unit: Unit) -> Metric {
    Metric {
        ceiling_only: false,
        ..metric(name, statistic, budget, unit)
    }
}

/// Collects one metric's samples between its start and end.
struct Recording {
    metric: Metric,
    samples: Vec<Sample>,
    started_at: chrono::DateTime<Utc>,
}

impl Recording {
    fn start(metric: Metric) -> Self {
        Self {
            metric,
            samples: Vec::new(),
            started_at: Utc::now(),
        }
    }

    fn add(&mut self, value: f64) {
        self.samples.push(Sample {
            metric: self.metric.name,
            value,
            unit: self.metric.unit,
        });
    }

    fn finish(self) -> MetricRun {
        MetricRun {
            observation_count: self.samples.len(),
            metric: self.metric,
            samples: self.samples,
            started_at: self.started_at,
            ended_at: Utc::now(),
        }
    }
}

fn ms(elapsed: Duration) -> f64 {
    elapsed.as_secs_f64() * 1_000.0
}

/// Runs the flood, then its capacity phase, and returns their
/// measurements.
pub async fn run(options: &FloodOptions) -> Result<Vec<MetricRun>> {
    let mut net = start(options.k).await?;
    let measured = measure(&mut net, options).await;
    let shutdown = net.shutdown().await;
    let mut runs = measured?;
    shutdown.context("shut the flood's hosts down")?;

    let mut net = start(options.k).await?;
    let measured = capacity(&mut net, options).await;
    let shutdown = net.shutdown().await;
    runs.push(measured?);
    shutdown.context("shut the capacity phase's hosts down")?;
    Ok(runs)
}

async fn start(k: u32) -> Result<Net> {
    Net::start_with(
        Topology::new()
            .host_decl(HostDecl {
                name: ORIGIN.to_owned(),
                lan: true,
                ..HostDecl::default()
            })
            .host(VIEWER)
            .link(ORIGIN, VIEWER),
        NetOptions {
            clock: ClockMode::Wall,
            launch: Some(Arc::new(move |_: &str, launch: &mut Launch| {
                launch.tail_rows = k;
            })),
            ..NetOptions::default()
        },
    )
    .await
    .context("start the flood's hosts")
}

/// The windows the capacity phase prices ingest over: equal shares of
/// the frames it drains.
const CAPACITY_WINDOWS: usize = 7;
/// The fewest frames one window holds: several ingest batches, so the
/// batch a window's edge cuts through is a small part of its time.
const WINDOW_FRAMES: u64 = 8 * node::INGEST_BATCH as u64;

/// Ingest's cost per committed frame. The agents write unthrottled while
/// the phase holds the origin's store, so ingest commits nothing, until
/// their journals hold `backlog` bytes past the origin's cursors; then
/// the writers stop, the store is let go, and ingest drains the backlog
/// alone. The drain is cut into equal shares of the frames it committed,
/// and each share's wall time over its frames is one sample. Pausing the
/// writers leaves ingest the machine to itself, so the figure is ingest's
/// own cost rather than how busy writers share the cores with it, and
/// how much there is to drain does not depend on how fast they ran.
async fn capacity(net: &mut Net, options: &FloodOptions) -> Result<MetricRun> {
    let mut run = Recording::start(Metric {
        workload: CAPACITY_WORKLOAD,
        ..drifting(
            "flood ingest cost per frame",
            Statistic::Median,
            budgets::INGEST_COST_US,
            Unit::Microseconds,
        )
    });
    let unthrottled = FloodOptions {
        pace_ms: None,
        ..options.clone()
    };
    for agent in topology(&unthrottled).agents {
        net.spawn(agent).await?;
    }
    let names: Vec<String> = (0..options.agents).map(agent_name).collect();
    let pids = agent_pids(net, &names)?;
    ensure!(
        pids.len() == names.len(),
        "found {} of {} agent processes",
        pids.len(),
        names.len()
    );
    let runtime = net.runtime(ORIGIN)?;
    let store = runtime.store().await;
    let keys = names
        .iter()
        .map(|name| Ok(net.agent(name)?.key()))
        .collect::<Result<Vec<_>>>()?;
    let deadline = Instant::now() + PATIENCE;
    let ends = loop {
        let ends = journals(net, &names)?;
        let mut behind = 0;
        for (key, end) in keys.iter().zip(&ends) {
            behind += end.saturating_sub(store.cursor(key)?);
        }
        if behind >= options.backlog {
            break ends;
        }
        ensure!(
            Instant::now() < deadline,
            "the agents wrote {behind} of {} bytes ahead of ingest",
            options.backlog
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    signal(&pids, "-STOP")?;
    // What the writers had written by the time they stopped.
    let ends = journals(net, &names)?
        .into_iter()
        .zip(ends)
        .map(|(now, then)| now.max(then))
        .collect::<Vec<_>>();

    // Every change in the committed count, until the cursors reach the
    // ends; the check takes the store, so it runs far less often.
    let mut drain = vec![(Instant::now(), runtime.ingested_frames())];
    drop(store);
    let mut checked = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(1)).await;
        let frames = runtime.ingested_frames();
        if frames != drain.last().expect("the start").1 {
            drain.push((Instant::now(), frames));
        }
        if checked.elapsed() < Duration::from_millis(50) {
            continue;
        }
        checked = Instant::now();
        let store = runtime.store().await;
        let mut drained = true;
        for (key, end) in keys.iter().zip(&ends) {
            drained &= store.cursor(key)? >= *end;
        }
        if drained {
            break;
        }
        ensure!(
            drain.last().expect("the start").0.elapsed() < PATIENCE,
            "ingest stopped draining the backlog"
        );
    }
    drop(runtime);
    for sample in drain_costs(&drain)? {
        run.add(sample);
    }
    // A stop ingests its agent's journal to the end, and these agents
    // write far faster than ingest drains: crash the origin instead, and
    // the shutdown kills its agents unread.
    signal(&pids, "-CONT")?;
    net.kill_daemon(ORIGIN).await?;
    Ok(run.finish())
}

/// Cuts a drain, the committed count at each moment it changed, into
/// [`CAPACITY_WINDOWS`] equal shares of its frames, and prices each in
/// microseconds a frame. The drain ends at its last commit, so no share
/// times ingest waiting for work.
fn drain_costs(drain: &[(Instant, u64)]) -> Result<Vec<f64>> {
    let (started, first) = drain[0];
    let last = drain.last().expect("the start").1;
    let frames = last - first;
    ensure!(
        frames >= WINDOW_FRAMES * CAPACITY_WINDOWS as u64,
        "the backlog drained in {frames} frames, too few to price; raise it"
    );
    let mut costs = Vec::new();
    let mut from = (started, first);
    for window in 1..=CAPACITY_WINDOWS as u64 {
        let edge = first + frames * window / CAPACITY_WINDOWS as u64;
        let to = *drain
            .iter()
            .find(|(_, committed)| *committed >= edge)
            .expect("the last commit reaches every edge");
        // One look can see commits past two edges when the looking
        // thread was held off the cores; those shares are one window.
        if to.1 > from.1 {
            costs.push((to.0 - from.0).as_secs_f64() * 1e6 / (to.1 - from.1) as f64);
            from = to;
        }
    }
    Ok(costs)
}

fn signal(pids: &[u32], signal: &str) -> Result<()> {
    let status = std::process::Command::new("kill")
        .arg(signal)
        .args(pids.iter().map(u32::to_string))
        .status()
        .context("run kill")?;
    ensure!(status.success(), "kill {signal} failed: {status}");
    Ok(())
}

async fn measure(net: &mut Net, options: &FloodOptions) -> Result<Vec<MetricRun>> {
    // The viewer holds nothing: cut it off before any agent exists.
    net.sever_link(ORIGIN, VIEWER)?;
    net.wait_link(ORIGIN, VIEWER, false).await?;
    for agent in topology(options).agents {
        net.spawn(agent).await?;
    }
    let names: Vec<String> = (0..options.agents).map(agent_name).collect();
    let chat = names[0].clone();
    for name in &names {
        wait_newest(net, name, options.history).await?;
    }

    // Fleet and chat on a runtime with no block.
    let mut fleet_run = Recording::start(metric(
        "flood fleet caught up",
        Statistic::Worst,
        budgets::FLEET_MS,
        Unit::Milliseconds,
    ));
    let mut fleet = net.observe_inventory(VIEWER).await?;
    fleet
        .observe_until(observe::inventory_caught_up, PATIENCE)
        .await?;
    let origin_id = net.host(ORIGIN)?.host_id;
    let opened = Instant::now();
    net.restore_link(ORIGIN, VIEWER).await?;
    net.wait_link(ORIGIN, VIEWER, true).await?;
    let listed = options.agents;
    fleet
        .observe_until(
            |events| listed_agents(events, origin_id.as_bytes()) == listed,
            PATIENCE,
        )
        .await?;
    fleet_run.add(ms(opened.elapsed()));

    let mut chat_run = Recording::start(metric(
        "flood chat caught up",
        Statistic::Worst,
        budgets::CHAT_MS,
        Unit::Milliseconds,
    ));
    let opened = Instant::now();
    let mut observer = net.observe(VIEWER, &chat, N.min(options.k)).await?;
    observer.observe_until(observe::caught_up, PATIENCE).await?;
    chat_run.add(ms(opened.elapsed()));

    // Ingest lag and memory at the origin while everything runs.
    let mut lag_run = Recording::start(metric(
        "flood ingest lag",
        Statistic::P99,
        budgets::INGEST_LAG_MS,
        Unit::Milliseconds,
    ));
    let mut memory_run = Recording::start(drifting(
        "flood agent process memory",
        Statistic::Peak,
        budgets::AGENT_MEMORY_MIB,
        Unit::Megabytes,
    ));
    let pids = agent_pids(net, &names)?;
    ensure!(
        pids.len() == names.len(),
        "found {} of {} agent processes",
        pids.len(),
        names.len()
    );
    let until = Instant::now() + options.sampling;
    let mut at = 0;
    while Instant::now() < until {
        let name = &names[at % names.len()];
        lag_run.add(ms(ingest_lag(net, name).await?));
        if at % names.len() == 0 {
            for pid in &pids {
                let sample =
                    sample_memory(*pid).with_context(|| format!("sample agent process {pid}"))?;
                memory_run.add(sample.bytes as f64 / (1024.0 * 1024.0));
            }
        }
        at += 1;
    }

    // Catch-up at a distance under K, then over it.
    let under = catch_up(
        net,
        &mut observer,
        &chat,
        u64::from(options.k) / 2,
        "flood catch-up under K",
    )
    .await?;
    let over = catch_up(
        net,
        &mut observer,
        &chat,
        u64::from(options.k) * 5,
        "flood catch-up over K",
    )
    .await?;

    // The daemon dies; the agents keep writing.
    let mut growth_run = Recording::start(metric(
        "flood backlog growth with the daemon killed",
        Statistic::Worst,
        budgets::BACKLOG_GROWTH_MIB_PER_MIN,
        Unit::MegabytesPerMinute,
    ));
    net.kill_daemon(ORIGIN).await?;
    let before = journals(net, &names)?;
    let killed = Instant::now();
    tokio::time::sleep(options.outage).await;
    let after = journals(net, &names)?;
    let grown = after.iter().sum::<u64>() - before.iter().sum::<u64>();
    growth_run.add(grown as f64 / (1024.0 * 1024.0) / (killed.elapsed().as_secs_f64() / 60.0));

    let mut drain_run = Recording::start(metric(
        "flood backlog drain after restart",
        Statistic::Worst,
        budgets::BACKLOG_DRAIN_MS,
        Unit::Milliseconds,
    ));
    net.restart_daemon(ORIGIN).await?;
    let restarted = Instant::now();
    let ends = journals(net, &names)?;
    for (name, end) in names.iter().zip(&ends) {
        wait_ingested(net, name, *end).await?;
    }
    drain_run.add(ms(restarted.elapsed()));

    Ok(vec![
        fleet_run.finish(),
        chat_run.finish(),
        lag_run.finish(),
        memory_run.finish(),
        under,
        over,
        growth_run.finish(),
        drain_run.finish(),
    ])
}

/// How many agents of `host` the inventory lists now.
fn listed_agents(events: &[InventoryEvent], host: &[u8]) -> usize {
    let mut listed = std::collections::HashSet::new();
    for event in events {
        match &event.of {
            Some(inventory_event::Of::Agent(agent)) if agent.host_id == host => {
                listed.insert(agent.agent_id.clone());
            }
            Some(inventory_event::Of::AgentRemoved(removed)) if removed.host_id == host => {
                listed.remove(&removed.agent_id);
            }
            _ => {}
        }
    }
    listed.len()
}

/// The origin's newest revision for `agent`.
async fn origin_newest(net: &Net, agent: &str) -> Result<u64> {
    let key = net.agent(agent)?.key();
    let runtime = net.runtime(ORIGIN)?;
    let store = runtime.store().await;
    let row = store.agent(&key)?.context("the origin lists its agent")?;
    Ok(row.next_revision - 1)
}

/// Waits until the origin has committed `agent`'s revision `revision`.
/// The agent's turn outlasts any flood, so only a stalled agent or ingest
/// runs out the harness's patience between two revisions.
async fn wait_newest(net: &Net, agent: &str, revision: u64) -> Result<()> {
    let mut last = (origin_newest(net, agent).await?, Instant::now());
    while last.0 < revision {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let newest = origin_newest(net, agent).await?;
        if newest > last.0 {
            last = (newest, Instant::now());
        }
        ensure!(
            last.1.elapsed() < PATIENCE,
            "{agent} stopped at revision {newest}, short of {revision}"
        );
    }
    Ok(())
}

/// The time from a journal's end now to the origin's cursor passing it.
async fn ingest_lag(net: &Net, agent: &str) -> Result<Duration> {
    let end = net.journal_end(agent)?;
    let written = Instant::now();
    wait_ingested(net, agent, end).await?;
    Ok(written.elapsed())
}

async fn wait_ingested(net: &Net, agent: &str, end: u64) -> Result<()> {
    let key = net.agent(agent)?.key();
    let deadline = Instant::now() + PATIENCE;
    loop {
        let cursor = net.runtime(ORIGIN)?.store().await.cursor(&key)?;
        if cursor >= end {
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!("{agent}'s ingest stayed at {cursor} short of {end}");
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

fn journals(net: &Net, names: &[String]) -> Result<Vec<u64>> {
    names
        .iter()
        .map(|name| Ok(net.journal_end(name)?))
        .collect()
}

/// Cuts the viewer off until `agent` has moved about `distance` rows on,
/// then restores it and measures until the chat's next CaughtUp.
async fn catch_up(
    net: &mut Net,
    observer: &mut testnet::Observer,
    agent: &str,
    distance: u64,
    name: &'static str,
) -> Result<MetricRun> {
    let mut run = Recording::start(metric(
        name,
        Statistic::Worst,
        budgets::CATCH_UP_MS,
        Unit::Milliseconds,
    ));
    let seen = observer.events().len();
    net.sever_link(ORIGIN, VIEWER)?;
    net.wait_link(ORIGIN, VIEWER, false).await?;
    let from = origin_newest(net, agent).await?;
    wait_newest(net, agent, from + distance).await?;
    let restored = Instant::now();
    net.restore_link(ORIGIN, VIEWER).await?;
    observer
        .observe_until(
            |events: &[SessionEvent]| {
                marks(&events[seen..])
                    .iter()
                    .rposition(|mark| *mark == Mark::Detached)
                    .is_some_and(|detached| {
                        marks(&events[seen..])[detached..]
                            .iter()
                            .any(|mark| matches!(mark, Mark::CaughtUp(_)))
                    })
            },
            PATIENCE,
        )
        .await?;
    run.add(ms(restored.elapsed()));
    Ok(run.finish())
}

/// The `amux agent` process of each named agent.
fn agent_pids(net: &Net, names: &[String]) -> Result<Vec<u32>> {
    let mut pids = Vec::new();
    for name in names {
        let dir = net.agent_dir(name)?;
        pids.extend(pids_with_argument(&dir)?);
    }
    Ok(pids)
}

fn pids_with_argument(dir: &Path) -> Result<Vec<u32>> {
    let output = std::process::Command::new("pgrep")
        .arg("-f")
        .arg(format!("agent {}", dir.display()))
        .output()
        .context("run pgrep")?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect())
}
