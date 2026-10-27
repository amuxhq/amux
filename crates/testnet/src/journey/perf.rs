//! `testnet journey perf`: the iPhone app measured on this Mac's simulator,
//! or with `--device` on the phone plugged into it.
//!
//! The optimised `Measured` app, which keeps the driving door, runs against
//! the served performance network (`qualification::perf::phone`: three
//! machines on the phone's local network, forty agents, one conversation a
//! thousand rows long that streams on cue). Every packet between the phone
//! and a machine crosses a gate the run can delay, so reconciliation is
//! measured over a household network's latency as well as over none; away
//! from home, every way to the relay (the phone's and each machine's) crosses
//! one, so a relayed launch pays the latency on both legs, as it does over
//! mobile data. Every number is taken by the app itself and read back through
//! the door; this only arranges the workload, keeps the samples, judges them
//! against the budgets below and the enrolled Mac's baseline, and writes the
//! report.
//!
//! On a phone the same run is driven over its USB link (`device`), and the
//! numbers are the phone's own: its loader, its CPU, its display.
//!
//! docs/PERFORMANCE.md, "The phone", is the reference: the tables there are
//! checked against `BUDGETS` and `MACHINES` here by this module's tests.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use clap::ValueEnum;
use serde::Serialize;
use serde::ser::{SerializeMap as _, Serializer};
use serde_json::{Value, json};

use super::device::{self, Phone};
use super::phone::{
    BUNDLE_ID, Launch, Launcher, PhoneJourney, SCRATCH_DIR, Setup, Target, ask, golden_device,
};
use super::root;
use crate::door::Control;

const APP: &str = "target/ios/DerivedData/Build/Products/Measured-iphonesimulator/Amux.app";
const DEVICE_APP: &str = "target/ios/DerivedData/Build/Products/Measured-iphoneos/Amux.app";
const TOPOLOGY_TOOL: &str = "target/debug/phone-topology";
const OUTPUT: &str = "target/ios/perf";
const BASELINES: &str = "perf/baselines/phone";

/// The Macs a run is allowed on, by the model `sysctl -n hw.model` prints.
/// A machine not listed is refused; enrolling one is adding a reviewed row
/// here and in docs/PERFORMANCE.md, then recording its baseline
/// deliberately.
const MACHINES: &[(&str, &str)] = &[("Mac14,6", "pinned-mac")];

/// The phones a `--device` run is allowed on, by hardware model, each with
/// budgets of its own. A phone's baseline is its name with the iOS major
/// version, `iphone-14-pro-ios27`: a system update changes the loader and the
/// frameworks under the app, so it is a new reference, recorded
/// deliberately.
const PHONES: &[EnrolledPhone] = &[EnrolledPhone {
    model: "iPhone15,2",
    name: "iphone-14-pro",
    // Its first measurement with the simulator's headroom over it: the cold
    // frame with the simulator's room in milliseconds (a share of 200 ms is
    // inside one launch's spread), the reconciliations by the simulator's
    // rule (the ungated launch plus the gated round trip, with 15% over),
    // and the rest the simulator's budget over its baseline as a share.
    budgets: &[
        ("cold first frame", 260.0, Some(310.0)),
        ("cold store read", 50.0, None),
        ("cold fleet render", 60.0, None),
        ("reconciliation at 0 ms", 60.0, None),
        ("reconciliation at 100 ms", 283.0, None),
        ("reconciliation through the relay at 100 ms", 513.0, None),
        ("opening hitch time", 120.0, None),
        ("opening main-thread CPU", 25.0, None),
        ("streaming hitch time", 5.0, None),
        ("streaming main-thread CPU", 60.0, None),
        ("streaming footprint", 100.0, None),
        ("idle transcript commits", 0.0, Some(0.0)),
        ("idle display ticks", 0.0, Some(0.0)),
        ("idle main-thread CPU", 8.0, None),
        ("idle home main-thread CPU", 8.0, None),
    ],
}];

/// A phone a run may measure, and what each metric is held to on it: the
/// median's budget and the worst sample's, in `BUDGETS`' order, whose units,
/// groups and tolerances it shares.
struct EnrolledPhone {
    model: &'static str,
    name: &'static str,
    budgets: &'static [(&'static str, f64, Option<f64>)],
}

impl EnrolledPhone {
    fn budgets(&self) -> Vec<Budget> {
        BUDGETS
            .iter()
            .zip(self.budgets)
            .map(|(budget, &(metric, median, worst))| {
                debug_assert_eq!(budget.metric, metric);
                Budget {
                    median,
                    worst,
                    ..*budget
                }
            })
            .collect()
    }
}

/// The cold first frame a phone is asked for (docs/IOS.md's physical-phone
/// checklist). A run on a phone reports the median against it; it is a
/// target, not a budget.
const LAUNCH_TARGET_MS: f64 = 400.0;

const SAMPLES: usize = 5;
const HOSTS: [&str; 3] = ["desk", "laptop", "studio"];
const FLEET_AGENTS: usize = 40;
const STREAM_AGENT: &str = "stream";
/// The relay account every machine signs in to, and the phone away from home.
const ACCOUNT: &str = "ada";
const STREAM_SECONDS: f64 = 20.0;
/// Rows in each streamed burst, as `qualification::perf::phone` writes them.
const STREAM_ROWS: i64 = 1_000;
const IDLE_SECONDS: f64 = 5.0;
/// How long a chat's opening is watched from just before the tap: the
/// push, the transcript's first build and its settling.
const OPENING_SECONDS: f64 = 2.5;
const LATENCIES_MS: [u64; 2] = [0, 100];
/// A machine admits ten QUIC handshakes a minute from one address (its
/// guard against a flood), and every launch here is one handshake to each
/// machine from the same address. Launches this far apart stay under it
/// with the pairing launch counted; a person does not relaunch faster.
const LAUNCH_SPACING: Duration = Duration::from_secs(10);
/// The trace outlasts the measured stretch by the settle after it, so the
/// whole burst is in the recording.
const TRACE_SECONDS: f64 = STREAM_SECONDS + 4.0;
/// A traced launch: the app's start, its reconciliation and the settle
/// after, with room for devicectl's own seconds before the process starts.
const LAUNCH_TRACE_SECONDS: f64 = 12.0;

/// The parts of a run, any of which may be run alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum Group {
    Cold,
    Reconciliation,
    Opening,
    Streaming,
    Idle,
}

impl Group {
    fn name(self) -> &'static str {
        match self {
            Group::Cold => "cold",
            Group::Reconciliation => "reconciliation",
            Group::Opening => "opening",
            Group::Streaming => "streaming",
            Group::Idle => "idle",
        }
    }
}

/// What one metric is held to.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Budget {
    metric: &'static str,
    unit: &'static str,
    group: Group,
    /// For the median.
    median: f64,
    /// For the worst sample.
    worst: Option<f64>,
    /// Drift tolerated over the baseline median, in percent, or None for a
    /// metric held to its ceiling alone. A tolerance of 0 is a metric with
    /// no slack at all; the count metrics are exact. Hitch time rests at
    /// nothing between runs of the same build and shows a millisecond or two
    /// on others, so a share of its baseline would fail runs for noise: its
    /// budget is what it must meet.
    tolerance: Option<u32>,
}

const fn budget(
    metric: &'static str,
    unit: &'static str,
    group: Group,
    median: f64,
    worst: Option<f64>,
    tolerance: Option<u32>,
) -> Budget {
    Budget {
        metric,
        unit,
        group,
        median,
        worst,
        tolerance,
    }
}

// The cold first frame's budget is set from a profile of the launch on the
// pinned Mac: 300 ms loading images, of which 270 is the simulator's loader
// on the main thread (85 of it dyld_sim re-pointing the shared cache at the
// host, which a device does not do), 100 of UIKit building the scene, then
// SwiftUI building a navigation stack, a tab bar and a home for the first
// time; nothing of ours is among the heavy leaves. The home's system chrome
// and its redrawn rows added about 100 ms to that, split in
// docs/PERFORMANCE.md. A phone has budgets of its own, in `PHONES`.
const BUDGETS: &[Budget] = &[
    budget(
        "cold first frame",
        "ms",
        Group::Cold,
        850.0,
        Some(900.0),
        Some(15),
    ),
    // The store read is forty rows from SQLite and the node's own start;
    // the fleet render is the main thread's work between it and the first
    // frame: the home's rows, and the rest of the shell's first frame, which
    // waits for the same thread. Each is a part of the first frame, and a
    // part's budget cannot be looser than what a person would feel in the
    // whole.
    budget("cold store read", "ms", Group::Cold, 50.0, None, Some(15)),
    budget(
        "cold fleet render",
        "ms",
        Group::Cold,
        250.0,
        None,
        Some(15),
    ),
    // On the same network the fleet should be current within a few frames
    // of the first one. A launch resumes every link with its first calls as
    // early data, so over a gate it should cost one round trip more than the
    // ungated launch: the budget is the ungated median the starting product
    // measured (91.8 ms) plus the gated round trip, with the recipe's 15%
    // over that. Through the relay the round trip crosses both gated legs,
    // 400 ms; on the same network it is 200 ms.
    budget(
        "reconciliation at 0 ms",
        "ms",
        Group::Reconciliation,
        250.0,
        None,
        Some(15),
    ),
    budget(
        "reconciliation at 100 ms",
        "ms",
        Group::Reconciliation,
        336.0,
        None,
        Some(15),
    ),
    budget(
        "reconciliation through the relay at 100 ms",
        "ms",
        Group::Reconciliation,
        566.0,
        None,
        Some(15),
    ),
    // Opening a chat whose agent is working, from home. The push is played
    // by the render server, but the main thread spends about 200 ms building
    // and laying out the page and its first rows, and the display link's
    // hitch time is that stall: anything the main thread animates stops for
    // it. The first measurement read 78 ms/s and 12.5%; the ceiling holds a
    // stall half as long again.
    budget(
        "opening hitch time",
        "ms/s",
        Group::Opening,
        120.0,
        None,
        None,
    ),
    budget(
        "opening main-thread CPU",
        "%",
        Group::Opening,
        20.0,
        None,
        Some(15),
    ),
    budget(
        "streaming hitch time",
        "ms/s",
        Group::Streaming,
        5.0,
        None,
        None,
    ),
    budget(
        "streaming main-thread CPU",
        "%",
        Group::Streaming,
        60.0,
        None,
        Some(15),
    ),
    // What keeps the app resident when it is put away, rather than what the
    // system would allow it in front.
    budget(
        "streaming footprint",
        "MB",
        Group::Streaming,
        120.0,
        None,
        Some(10),
    ),
    budget(
        "idle transcript commits",
        "count",
        Group::Idle,
        0.0,
        Some(0.0),
        Some(0),
    ),
    budget(
        "idle display ticks",
        "count",
        Group::Idle,
        0.0,
        Some(0.0),
        Some(0),
    ),
    // What the two counts cannot see: a SwiftUI animation that never stops
    // (a `TimelineView` driving a working mark or the activity line) keeps
    // the main thread busy on every display frame without asking for a tick
    // or committing a row. Held to a ceiling alone. With the motion run by
    // Core Animation, what is left is the one-second clock of the activity
    // line's words and the run's own frame counter (about 2% in a chat, 3%
    // on home); one animation back on SwiftUI's clock costs about 16% on a
    // phone and would cross it.
    budget("idle main-thread CPU", "%", Group::Idle, 8.0, None, None),
    budget(
        "idle home main-thread CPU",
        "%",
        Group::Idle,
        8.0,
        None,
        None,
    ),
];

/// Metrics whose measurement was redefined and whose budget and baseline are
/// to be set from the first run under the new definition: each is measured
/// and reported, but neither its budget nor a baseline recorded under the
/// old definition can fail a run. A metric leaves this list in the commit
/// that writes its measured budget and baseline.
const SET_FROM_FIRST_MEASUREMENT: &[&str] = &[];

/// Every metric's samples, in the budgets' order.
type Samples = Vec<(&'static str, Vec<f64>)>;

/// A list of pairs written as a JSON object in its own order.
struct Ordered<'a, T>(&'a [(&'a str, T)]);

impl<T: Serialize> Serialize for Ordered<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// This Mac's model and its enrolled name, or why a run must not happen.
fn machine() -> Result<(String, &'static str), String> {
    if !cfg!(target_os = "macos") {
        return Err("the phone is measured on a Mac's simulator".to_owned());
    }
    let model = sysctl("hw.model")?;
    match MACHINES.iter().find(|(enrolled, _)| *enrolled == model) {
        Some((_, name)) => Ok((model, name)),
        None => Err(format!(
            "this Mac is {model}, which is not enrolled; the enrolled machines are {}",
            MACHINES
                .iter()
                .map(|(model, name)| format!("{name} ({model})"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn sysctl(name: &str) -> Result<String, String> {
    let output = Command::new("sysctl")
        .args(["-n", name])
        .output()
        .map_err(|error| format!("sysctl: {error}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The load averages over 1, 5 and 15 minutes, as `uptime` gives them. A
/// busy Mac slows the simulator; the load says whether a miss is the app's.
fn load_average() -> String {
    sysctl("vm.loadavg")
        .unwrap_or_default()
        .trim_matches(|c| c == '{' || c == '}' || c == ' ')
        .split_whitespace()
        .map(|load| {
            load.parse::<f64>()
                .map_or_else(|_| load.to_owned(), |load| format!("{load:.2}"))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    }
}

/// A budget as the page and the report write it: 850, 5, 0.
fn plain(number: f64) -> String {
    if number.fract() == 0.0 {
        format!("{number:.0}")
    } else {
        number.to_string()
    }
}

/// An Instruments recording asked for: where it goes, the template it is
/// made with, and the part of the run it watches.
#[derive(Clone, Debug)]
pub(crate) struct Trace {
    pub path: PathBuf,
    pub template: String,
    pub during: Group,
}

/// One measured run: the served network, the phone, and the samples.
struct Run {
    journey: PhoneJourney,
    only: Vec<Group>,
    flat: bool,
    trace: Option<Trace>,
    /// Whether the asked-for recording has been made: one per run.
    traced: bool,
    samples: Samples,
    notes: Vec<String>,
    gates: BTreeMap<&'static str, String>,
    relay_gate: Option<String>,
    last_launch: Instant,
}

impl Run {
    fn takes(&self, group: Group) -> bool {
        self.only.is_empty() || self.only.contains(&group)
    }

    fn sample(&mut self, metric: &str, value: f64) {
        if let Some((_, values)) = self.samples.iter_mut().find(|(name, _)| *name == metric) {
            values.push(value);
        }
    }

    fn note(&mut self, note: String) {
        println!("{note}");
        self.notes.push(note);
    }

    fn app(&self, request: Value, timeout: Duration) -> Result<Value, String> {
        Ok(self.journey.app_within(request, timeout)?)
    }

    /// Whether the next sample of `group` is the one to record.
    fn traces(&self, group: Group) -> bool {
        !self.traced
            && self
                .trace
                .as_ref()
                .is_some_and(|trace| trace.during == group)
    }

    /// A phone that is hot or saving power measures itself rather than the
    /// app: such a run stops and says why. `fair` is a phone warmed by use,
    /// noted rather than refused, since a minute of streaming reaches it.
    fn check_condition(&mut self, before: &str) -> Result<(), String> {
        let Some(phone) = &self.journey.phone else {
            return Ok(());
        };
        let name = phone.name.clone();
        let reply = self.app(json!({"kind": "condition"}), Duration::from_secs(30))?;
        let condition = &reply["condition"];
        let thermal = condition["thermal"]
            .as_str()
            .unwrap_or("unknown")
            .to_owned();
        if condition["lowPower"].as_bool().unwrap_or(false) {
            return Err(format!(
                "{name} is in Low Power Mode, which slows its CPU and holds work back;                  turn it off (Settings > Battery) and run again"
            ));
        }
        if matches!(thermal.as_str(), "serious" | "critical") {
            return Err(format!(
                "{name} is {thermal}ly hot and throttling; let it cool unplugged from any charger                  that warms it and run again"
            ));
        }
        if thermal != "nominal" {
            self.note(format!("{name} thermal state {thermal} {before}"));
        }
        Ok(())
    }

    // --- the network -----------------------------------------------------

    /// A gate in front of every machine's LAN listener, dialled in the
    /// machine's place, so the run can put latency between the phone and
    /// all of them.
    fn gate_the_machines(&mut self) -> Result<(), String> {
        for host in HOSTS {
            let reply = self.journey.request(
                Control::LanGate {
                    host: host.to_owned(),
                },
                None,
            )?;
            let addr = reply["addr"].as_str().ok_or("the gate has no address")?;
            self.gates.insert(host, addr.to_owned());
        }
        Ok(())
    }

    fn lan_faults(&mut self, delay_ms: u64, loss_percent: u32) -> Result<(), String> {
        for host in HOSTS {
            self.journey.request(
                Control::LanFaults {
                    host: host.to_owned(),
                    delay_ms,
                    loss_percent,
                    jitter_ms: 0,
                    reorder_percent: 0,
                    rate_bytes_per_sec: 0,
                },
                None,
            )?;
        }
        Ok(())
    }

    fn latency(&mut self, ms: u64) -> Result<(), String> {
        self.lan_faults(ms, 0)?;
        self.notes
            .push(format!("latency {ms} ms each way on every gate"));
        Ok(())
    }

    /// A gate in front of the served relay's QUIC carrier, dialled in the
    /// relay's place, so the run can put latency between the phone and the
    /// relay the way mobile data does. Each machine already reaches the
    /// relay's QUIC carrier through a gate of its own.
    fn gate_the_relay(&mut self) -> Result<(), String> {
        let reply = self.journey.request(Control::RelayGate {}, None)?;
        self.relay_gate = reply["addr"].as_str().map(str::to_owned);
        Ok(())
    }

    /// Every way to the relay holds each packet `ms`: the phone's gate and
    /// each machine's, so a relayed round trip pays the latency on both legs
    /// as it would with the machines away from the relay too. The account
    /// service is not gated: a launch reuses its relay token.
    fn relay_latency(&mut self, ms: u64) -> Result<(), String> {
        self.journey.request(
            Control::NetFaults {
                target: "relay".to_owned(),
                delay_ms: ms,
                loss_percent: 0,
                jitter_ms: 0,
                reorder_percent: 0,
                rate_bytes_per_sec: 0,
            },
            None,
        )?;
        self.notes.push(format!(
            "latency {ms} ms each way on every way to the relay, the phone's and each machine's"
        ));
        Ok(())
    }

    /// The phone away from home: signed in to the machines' account at the
    /// relay, with every direct dial lost, as the machines' home addresses
    /// are from mobile data.
    fn leave_home(&mut self) -> Result<(), String> {
        let relay = self.journey.ready.cloud_url.clone();
        self.journey.app(json!({
            "kind": "connect", "relay": relay,
            "token": format!("refresh-{ACCOUNT}"), "user": ACCOUNT,
        }))?;
        self.lan_faults(0, 100)?;
        self.notes.push(format!(
            "signed in as {ACCOUNT} at the relay; every direct dial lost"
        ));
        Ok(())
    }

    fn come_home(&mut self) -> Result<(), String> {
        self.lan_faults(0, 0)?;
        self.relay_latency(0)
    }

    /// Pairs with every machine by the link it prints, with the link's
    /// addresses rewritten to the machine's gate, so the pairing and every
    /// link after it cross the gate.
    fn pair_through_the_gates(&mut self) -> Result<(), String> {
        for host in HOSTS {
            let (pairing, link) = self.journey.pairing_link(host)?;
            self.journey.pair(&gated_link(&link, &self.gates[host])?)?;
            pairing.wait(Duration::from_secs(30))?;
        }
        let agents = (0..FLEET_AGENTS)
            .map(|at| format!("agent-{at:02}"))
            .chain([STREAM_AGENT.to_owned()]);
        for agent in agents {
            self.app(
                json!({"kind": "awaitAgent", "agent": agent, "seconds": 120}),
                Duration::from_secs(130),
            )?;
        }
        self.app(
            json!({"kind": "awaitReconciled", "seconds": 60}),
            Duration::from_secs(90),
        )?;
        self.await_reconciled(Duration::from_secs(60))?;
        Ok(())
    }

    // --- the app's marks -------------------------------------------------

    /// The first time each signpost was marked, in ms since the process
    /// started.
    fn marks(&self) -> Result<BTreeMap<String, f64>, String> {
        let reply = self.app(json!({"kind": "signposts"}), Duration::from_secs(90))?;
        let mut first = BTreeMap::new();
        for mark in reply["marks"].as_array().into_iter().flatten() {
            if let (Some(signpost), Some(since)) = (
                mark["signpost"].as_str(),
                mark["sinceProcessStart"].as_f64(),
            ) {
                first.entry(signpost.to_owned()).or_insert(since * 1000.0);
            }
        }
        Ok(first)
    }

    fn await_reconciled(&self, within: Duration) -> Result<BTreeMap<String, f64>, String> {
        let deadline = Instant::now() + within;
        loop {
            let marks = self.marks()?;
            if marks.contains_key("reconciled") {
                return Ok(marks);
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "the app never reconciled with its hosts; it marked {:?}",
                    marks.keys().collect::<Vec<_>>()
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    // --- cold launches ---------------------------------------------------

    /// Five launches of the installed app with its state on disk and the
    /// machines up, the app terminated between them. Each launch marks its
    /// own first frame, store read and reconciliation; this reads them back.
    fn cold_launches(&mut self, latency_ms: u64) -> Result<(), String> {
        self.latency(latency_ms)?;
        let record_cold = latency_ms == 0 && self.takes(Group::Cold);
        let record_reconciliation = self.takes(Group::Reconciliation);
        for attempt in 0..SAMPLES {
            // A launch with no latency is the first sample of both groups, so
            // it is the one traced for either.
            let group = if latency_ms == 0 && !self.traces(Group::Reconciliation) {
                Group::Cold
            } else {
                Group::Reconciliation
            };
            let marks = self.marked_launch(group, attempt, &format!("at {latency_ms} ms"))?;
            let store_read = (marks["nodeStarted"] - marks["storeReadBegan"])
                + (marks["storeReadEnded"] - marks["fleetOpenBegan"]);
            if record_cold {
                self.sample("cold first frame", marks["firstCachedFrame"]);
                self.sample("cold store read", store_read);
                self.sample(
                    "cold fleet render",
                    marks["firstCachedFrame"] - marks["storeReadEnded"],
                );
            }
            if record_reconciliation {
                self.sample(
                    &format!("reconciliation at {latency_ms} ms"),
                    marks["reconciled"] - marks["storeReadEnded"],
                );
            }
        }
        Ok(())
    }

    /// Five launches of the phone away from home, every way to the relay
    /// holding each packet `latency_ms`: the fleet is current only once
    /// every machine has been reached through the relay.
    fn relay_launches(&mut self, latency_ms: u64) -> Result<(), String> {
        self.relay_latency(latency_ms)?;
        for attempt in 0..SAMPLES {
            let marks = self.marked_launch(
                Group::Reconciliation,
                attempt,
                &format!("through the relay at {latency_ms} ms"),
            )?;
            self.sample(
                &format!("reconciliation through the relay at {latency_ms} ms"),
                marks["reconciled"] - marks["storeReadEnded"],
            );
        }
        Ok(())
    }

    /// One launch, spaced from the last, read back to its reconciliation,
    /// with every mark a cold launch needs. The last launch stays open until
    /// the spacing is up, as a person looking at their fleet keeps the app
    /// open: a machine sends a resumed link's next ticket once the phone's
    /// handshake finishes, two round trips in, and an app quit at its
    /// reconciliation, one round trip in, would keep none and make every
    /// other launch a full handshake.
    fn marked_launch(
        &mut self,
        group: Group,
        attempt: usize,
        named: &str,
    ) -> Result<BTreeMap<String, f64>, String> {
        self.space_launches();
        self.journey.quit()?;
        // On a phone Instruments launches the app itself, the one way its
        // recording of the launch carries the app's own symbols. On the
        // simulator the launch is recorded over the whole Mac.
        let profiler = match (self.traces(group), self.journey.phone.is_some()) {
            (false, _) => {
                self.relaunch()?;
                None
            }
            (true, true) => {
                let (launcher, started) = self.launching_profiler(LAUNCH_TRACE_SECONDS)?;
                let launch = self.launching();
                self.journey.relaunch_through(launch, launcher)?;
                Some(
                    started
                        .recv()
                        .map_err(|_| "xctrace never started recording".to_owned())??,
                )
            }
            (true, false) => {
                let profiler = self.start_profiler(LAUNCH_TRACE_SECONDS, Watch::Everything)?;
                self.relaunch()?;
                Some(profiler)
            }
        };
        let marks = self.await_reconciled(Duration::from_secs(120))?;
        if let Some(profiler) = profiler {
            self.finish_profiler(profiler)?;
        }
        for needed in [
            "firstCachedFrame",
            "storeReadBegan",
            "nodeStarted",
            "fleetOpenBegan",
            "storeReadEnded",
        ] {
            if !marks.contains_key(needed) {
                return Err(format!(
                    "launch {} {named} never marked {needed}; it marked {:?}",
                    attempt + 1,
                    marks.keys().collect::<Vec<_>>()
                ));
            }
        }
        let at = |mark: &str| marks.get(mark).copied().unwrap_or_default();
        self.note(format!(
            "launch {} {named}: images {:.0}, entered {:.0}, built {:.0}, shell {:.0}, \
             node {:.0}-{:.0}, fleet {:.0}-{:.0}, first frame {:.0}, reconciled {:.0} ms",
            attempt + 1,
            at("imagesLoaded"),
            at("appEntered"),
            at("compositionBuilt"),
            at("shellPresented"),
            at("storeReadBegan"),
            at("nodeStarted"),
            at("fleetOpenBegan"),
            at("storeReadEnded"),
            at("firstCachedFrame"),
            at("reconciled"),
        ));
        Ok(marks)
    }

    /// Every launch dials the relay through its gate, whose socket toward
    /// the relay takes datagrams from the relay alone, as a strict router
    /// does. Signed in through anything else, the relay would observe the
    /// phone's own loopback socket, every machine would punch straight to
    /// it, and the relay launches would measure that ungated path.
    fn launching(&self) -> Launch {
        Launch {
            geometry: false,
            relay_quic: self.relay_gate.clone(),
            ..Launch::default()
        }
    }

    fn relaunch(&mut self) -> Result<(), String> {
        let launch = self.launching();
        self.journey.relaunch(launch)
    }

    fn space_launches(&mut self) {
        let since = self.last_launch.elapsed();
        if since < LAUNCH_SPACING {
            std::thread::sleep(LAUNCH_SPACING - since);
        }
        self.last_launch = Instant::now();
    }

    // --- the conversation on screen --------------------------------------

    fn open_the_conversation(&mut self) -> Result<(), String> {
        if self.flat {
            self.notes
                .push("every surface drawn flat (reduce transparency)".to_owned());
        }
        let row = format!("home.row.{}", self.journey.agent_id(STREAM_AGENT));
        // Frames only while tapping: the app is measured as it ships.
        self.journey.geometry(true)?;
        self.journey.tab("Agents")?;
        self.journey.wait_for(&[&row])?;
        self.journey.tap(&row)?;
        self.journey.wait_for(&["chat.field"])?;
        self.journey.geometry(false)?;
        let deadline = Instant::now() + Duration::from_secs(120);
        while self.newest_order()?.is_none() {
            if Instant::now() > deadline {
                return Err("the conversation on screen never showed its rows".to_owned());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        // The page's push and first build finish before anything is watched.
        // Not the door's settle: that photographs the window until two
        // pictures agree, which they never do while the working line moves,
        // so it held the main thread for its whole bound, seconds on a phone.
        std::thread::sleep(Duration::from_secs(1));
        Ok(())
    }

    /// How many rows have arrived since the chat's newest stood at
    /// `before`, once the burst has delivered them all or stopped coming.
    fn burst_arrived(&self, before: i64) -> Result<i64, String> {
        let mut arrived = 0;
        let mut moved = Instant::now();
        while arrived < STREAM_ROWS && moved.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(200));
            let now = self.newest_order()?.unwrap_or_default() - before;
            if now > arrived {
                arrived = now;
                moved = Instant::now();
            }
        }
        Ok(arrived)
    }

    /// The order of the newest row the chat on screen holds, or nothing
    /// while it holds none.
    fn newest_order(&self) -> Result<Option<i64>, String> {
        let reply = self.app(
            json!({"kind": "conversation", "agent": STREAM_AGENT}),
            Duration::from_secs(90),
        )?;
        Ok(reply["conversation"]["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| row["order"].as_i64())
            .max())
    }

    fn measure(&self, seconds: f64) -> Result<Value, String> {
        let reply = self.app(
            json!({"kind": "measure", "seconds": seconds}),
            Duration::from_secs_f64(seconds + 60.0),
        )?;
        Ok(reply["measurement"].clone())
    }

    /// Nothing arriving, nothing drawn: after a settle, the chat takes no
    /// rows and the app asks the display for nothing.
    fn idle(&mut self) -> Result<(), String> {
        for attempt in 0..SAMPLES {
            std::thread::sleep(Duration::from_secs(2));
            let profiler = if self.traces(Group::Idle) {
                Some(self.start_profiler(IDLE_SECONDS + 4.0, Watch::App)?)
            } else {
                None
            };
            let measured = self.measure(IDLE_SECONDS)?;
            if let Some(profiler) = profiler {
                self.finish_profiler(profiler)?;
            }
            let commits = measured["transcriptCommits"].as_f64().unwrap_or_default();
            let ticks = measured["idleTicks"].as_f64().unwrap_or_default();
            let cpu = measured["mainThreadCpuPercent"]
                .as_f64()
                .unwrap_or_default();
            self.sample("idle transcript commits", commits);
            self.sample("idle display ticks", ticks);
            self.sample("idle main-thread CPU", cpu);
            self.note(format!(
                "idle {}: {commits} commits, {ticks} ticks, main thread {cpu:.1}% over {IDLE_SECONDS:.0} s",
                attempt + 1
            ));
        }
        Ok(())
    }

    /// The working agent's chat opened from home and left again, five
    /// times: what the display missed and the main thread spent over the
    /// opening. The door finds the row and holds the screen still, then
    /// presses it `PRESS_AFTER` later; the measurement starts in between, on
    /// the driver's own connection while the press waits on another, so it
    /// sees the app's answer to the press and none of the driver's search.
    /// One opening first is not measured: the first press of a process
    /// starts UIKit's accessibility machinery, which a person's finger never
    /// does. Element frames are reported throughout, since a page only
    /// reports what it was built reporting and the driver needs the chat's
    /// back button; that cost is in every sample alike.
    fn openings(&mut self) -> Result<(), String> {
        const PRESS_AFTER: f64 = 0.6;
        let row = format!("home.row.{}", self.journey.agent_id(STREAM_AGENT));
        self.journey.geometry(true)?;
        self.journey.tab("Agents")?;
        for attempt in 0..=SAMPLES {
            self.journey.wait_for(&[&row])?;
            std::thread::sleep(Duration::from_secs(2));
            if attempt == 0 {
                self.journey.tap(&row)?;
                self.journey.wait_for(&["chat.field"])?;
                self.journey.tap("chat.back")?;
                continue;
            }
            let profiler = if self.traces(Group::Opening) {
                Some(self.start_profiler(OPENING_SECONDS + 4.0, Watch::App)?)
            } else {
                None
            };
            let door = self.journey.door();
            let press = json!({"kind": "tap", "identifier": row, "after": PRESS_AFTER});
            let pressing = std::thread::spawn(move || ask(door, &press, Duration::from_secs(30)));
            // The door's search is done well inside this, and the press is
            // still to come.
            std::thread::sleep(Duration::from_secs_f64(PRESS_AFTER / 2.0));
            let measurement = self.measure(OPENING_SECONDS)?;
            pressing
                .join()
                .map_err(|_| "the opening's press panicked".to_owned())??;
            if let Some(profiler) = profiler {
                self.finish_profiler(profiler)?;
            }
            self.journey.wait_for(&["chat.field"])?;
            self.journey.tap("chat.back")?;
            let hitch = measurement["hitchMsPerS"].as_f64().unwrap_or_default();
            let cpu = measurement["mainThreadCpuPercent"]
                .as_f64()
                .unwrap_or_default();
            let frames = measurement["frames"].as_f64().unwrap_or_default();
            self.sample("opening hitch time", hitch);
            self.sample("opening main-thread CPU", cpu);
            self.note(format!(
                "opening {attempt}: {frames} frames, hitch {hitch:.2} ms/s, main thread {cpu:.1}% over {OPENING_SECONDS} s"
            ));
        }
        // Left on: the conversation is opened next, by a tap that needs
        // frames, and a page shown while they were switched off and on again
        // reports none.
        Ok(())
    }

    /// Home with nothing arriving, after the same settle: what the main
    /// thread costs while somebody looks at a fleet whose agents work.
    fn idle_home(&mut self) -> Result<(), String> {
        for attempt in 0..SAMPLES {
            std::thread::sleep(Duration::from_secs(2));
            let measured = self.measure(IDLE_SECONDS)?;
            let cpu = measured["mainThreadCpuPercent"]
                .as_f64()
                .unwrap_or_default();
            self.sample("idle home main-thread CPU", cpu);
            self.note(format!(
                "idle home {}: main thread {cpu:.1}% over {IDLE_SECONDS:.0} s",
                attempt + 1
            ));
        }
        Ok(())
    }

    /// Fifty rows a second for twenty seconds into the conversation on
    /// screen, resting at its tail so every row is laid out as it lands; the
    /// display, the main thread and the footprint watched meanwhile.
    fn streaming(&mut self) -> Result<(), String> {
        for attempt in 0..SAMPLES {
            let before = self.newest_order()?.unwrap_or_default();
            let profiler = if self.traces(Group::Streaming) {
                Some(self.start_profiler(TRACE_SECONDS, Watch::App)?)
            } else {
                None
            };
            self.journey.request(
                Control::OpenGate {
                    name: format!("stream-{attempt}"),
                },
                None,
            )?;
            let measured = self.measure(STREAM_SECONDS)?;
            // The burst outlasts the watch: its pauses run a little long, so
            // its rows take a few seconds more than they are paced for. Every
            // one is waited for, so the next sample starts from rest and a row
            // that never reaches the phone is caught, not taken for a slow
            // burst.
            let arrived = self.burst_arrived(before)?;
            if let Some(profiler) = profiler {
                self.finish_profiler(profiler)?;
            }
            if arrived < STREAM_ROWS {
                return Err(format!(
                    "stream sample {}: {arrived} of {STREAM_ROWS} rows reached the phone and no more came",
                    attempt + 1
                ));
            }
            let reading = |key: &str| measured[key].as_f64().unwrap_or_default();
            self.sample("streaming hitch time", reading("hitchMsPerS"));
            self.sample("streaming main-thread CPU", reading("mainThreadCpuPercent"));
            self.sample("streaming footprint", reading("footprintMB"));
            self.note(format!(
                "stream {}: {arrived} rows, {} frames, hitch {:.2} ms/s, CPU {:.1}%, footprint {:.1} MB",
                attempt + 1,
                measured["frames"],
                reading("hitchMsPerS"),
                reading("mainThreadCpuPercent"),
                reading("footprintMB"),
            ));
        }
        Ok(())
    }

    // --- the profiler ----------------------------------------------------

    /// Instruments recording with the asked-for template until its time
    /// limit; returns once xctrace says it is recording, so what it is to
    /// watch starts inside the trace. On a phone it attaches to the app, or
    /// records the whole phone for a launch; on the simulator it records
    /// every process on this Mac, since xctrace cannot attach to a
    /// simulator's process, with the app's pid noted to filter by.
    fn start_profiler(&mut self, seconds: f64, watch: Watch) -> Result<Profiling, String> {
        let trace = self.trace.clone().ok_or("no trace was asked for")?;
        self.traced = true;
        if trace.path.exists() {
            std::fs::remove_dir_all(&trace.path).map_err(|error| error.to_string())?;
        }
        if let Some(parent) = trace.path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let started = format!("sh.amux.perf.tracing.{}", std::process::id());
        // Waiting for the notification must begin before xctrace can post it.
        let mut waiter = Command::new("notifyutil")
            .args(["-1", &started])
            .stdout(Stdio::null())
            .spawn()
            .map_err(|error| format!("notifyutil: {error}"))?;
        let log = root().join(OUTPUT).join("xctrace.log");
        let sink = File::create(&log).map_err(|error| error.to_string())?;
        let mut command = Command::new("xcrun");
        command.args([
            "xctrace",
            "record",
            "--template",
            &trace.template,
            "--no-prompt",
        ]);
        let pid = self.journey.pid;
        let watched = match (&self.journey.phone, watch) {
            (Some(phone), Watch::App) => {
                command
                    .args(["--device", &phone.udid])
                    .args(["--attach", &pid.to_string()]);
                format!("the app on {} (pid {pid})", phone.name)
            }
            (Some(phone), Watch::Everything) => {
                command
                    .args(["--device", &phone.udid])
                    .arg("--all-processes");
                format!(
                    "every process on {}, the app among them once launched",
                    phone.name
                )
            }
            (None, _) => {
                command.arg("--all-processes");
                format!("every process on this Mac; the app is pid {pid}")
            }
        };
        let mut profiler = command
            .args(["--time-limit", &format!("{seconds:.0}s")])
            .args(["--notify-tracing-started", &started])
            .arg("--output")
            .arg(&trace.path)
            .stdout(sink.try_clone().map_err(|error| error.to_string())?)
            .stderr(sink)
            .spawn()
            .map_err(|error| format!("xctrace: {error}"))?;
        if !exited_within(&mut waiter, Duration::from_secs(90)) {
            let _ = waiter.kill();
            let _ = profiler.kill();
            let _ = profiler.wait();
            return Err(format!(
                "xctrace did not start recording within 90 s: {}",
                std::fs::read_to_string(&log).unwrap_or_default()
            ));
        }
        self.note(format!(
            "trace: {} over {watched} for {seconds:.0} s, during {}",
            trace.template,
            trace.during.name()
        ));
        Ok(Profiling {
            child: profiler,
            path: trace.path,
            seconds,
        })
    }

    /// A launcher that has Instruments launch the app on the phone with the
    /// run's arguments, recording from the process's start, and where the
    /// recording it started is sent once xctrace says it is recording.
    fn launching_profiler(
        &mut self,
        seconds: f64,
    ) -> Result<(Launcher, mpsc::Receiver<Result<Profiling, String>>), String> {
        let trace = self.trace.clone().ok_or("no trace was asked for")?;
        let phone = self.journey.phone.clone().ok_or("no phone to launch on")?;
        self.traced = true;
        if trace.path.exists() {
            std::fs::remove_dir_all(&trace.path).map_err(|error| error.to_string())?;
        }
        if let Some(parent) = trace.path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        self.note(format!(
            "trace: {} launching the app on {} for {seconds:.0} s, during {}",
            trace.template,
            phone.name,
            trace.during.name()
        ));
        let log = root().join(OUTPUT).join("xctrace.log");
        let (sent, started) = mpsc::channel();
        let launcher: Launcher = Box::new(move |arguments, environment| {
            let begun = format!("sh.amux.perf.tracing.{}", std::process::id());
            let recording = (|| {
                let mut waiter = Command::new("notifyutil")
                    .args(["-1", &begun])
                    .stdout(Stdio::null())
                    .spawn()
                    .map_err(|error| format!("notifyutil: {error}"))?;
                let sink = File::create(&log).map_err(|error| error.to_string())?;
                let mut command = Command::new("xcrun");
                command
                    .args([
                        "xctrace",
                        "record",
                        "--template",
                        &trace.template,
                        "--no-prompt",
                    ])
                    .args(["--device", &phone.udid])
                    .args(["--time-limit", &format!("{seconds:.0}s")])
                    .args(["--notify-tracing-started", &begun])
                    .arg("--output")
                    .arg(&trace.path);
                for (key, value) in environment {
                    command.args(["--env", &format!("{key}={value}")]);
                }
                let mut child = command
                    .args(["--launch", "--", BUNDLE_ID])
                    .args(arguments)
                    .stdout(sink.try_clone().map_err(|error| error.to_string())?)
                    .stderr(sink)
                    .spawn()
                    .map_err(|error| format!("xctrace: {error}"))?;
                if !exited_within(&mut waiter, Duration::from_secs(90)) {
                    let _ = waiter.kill();
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "xctrace did not start recording within 90 s: {}",
                        std::fs::read_to_string(&log).unwrap_or_default()
                    ));
                }
                Ok(Profiling {
                    child,
                    path: trace.path.clone(),
                    seconds,
                })
            })();
            let failed = recording.as_ref().err().cloned();
            let _ = sent.send(recording);
            failed.map_or(Ok(()), Err)
        });
        Ok((launcher, started))
    }

    /// Waits for the recording to end and be written, then exports its
    /// tables beside it as XML: the table of contents, and each table it
    /// lists, for reading without Instruments.
    fn finish_profiler(&mut self, profiling: Profiling) -> Result<(), String> {
        let Profiling {
            mut child,
            path,
            seconds,
        } = profiling;
        // Saving a whole-Mac trace takes minutes.
        if !exited_within(&mut child, Duration::from_secs_f64(seconds + 600.0)) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("xctrace did not finish writing the trace".to_owned());
        }
        let status = child.wait().map_err(|error| error.to_string())?;
        if !status.success() || !path.exists() {
            let log = root().join(OUTPUT).join("xctrace.log");
            return Err(format!(
                "xctrace failed ({status}): {}",
                std::fs::read_to_string(log).unwrap_or_default()
            ));
        }
        self.note(format!("trace written to {}", path.display()));
        match export_tables(&path) {
            Ok(tables) => self.note(format!(
                "exported {} tables beside it: {}",
                tables.len(),
                tables.join(", ")
            )),
            Err(error) => self.note(format!("its tables were not exported: {error}")),
        }
        Ok(())
    }

    // --- the whole run ---------------------------------------------------

    fn measure_everything(&mut self) -> Result<(), String> {
        self.gate_the_machines()?;
        self.gate_the_relay()?;
        let launch = self.launching();
        self.journey.launch(launch)?;
        self.check_condition("before pairing")?;
        self.pair_through_the_gates()?;
        // A launch the cold launches are compared against: the same install,
        // the fleet already in its store. Every reconciled launch below
        // reads the remembered fleet first, which is what a cold first frame
        // shows.
        if self.takes(Group::Cold) || self.takes(Group::Reconciliation) {
            for latency in LATENCIES_MS {
                if latency != 0 && !self.takes(Group::Reconciliation) {
                    continue;
                }
                self.cold_launches(latency)?;
                self.check_condition(&format!("after the launches at {latency} ms"))?;
            }
            self.latency(0)?;
        }
        if self.takes(Group::Reconciliation) {
            self.leave_home()?;
            self.relay_launches(LATENCIES_MS[LATENCIES_MS.len() - 1])?;
            self.come_home()?;
        }
        if self.takes(Group::Idle) || self.takes(Group::Opening) || self.takes(Group::Streaming) {
            self.journey.quit()?;
            self.space_launches();
            self.relaunch()?;
            self.await_reconciled(Duration::from_secs(120))?;
            if self.takes(Group::Idle) {
                self.idle_home()?;
            }
            if self.takes(Group::Opening) {
                self.openings()?;
            }
            self.open_the_conversation()?;
            self.check_condition("before idle and streaming")?;
            if self.takes(Group::Idle) {
                self.idle()?;
            }
            if self.takes(Group::Streaming) {
                self.streaming()?;
                self.check_condition("after streaming")?;
            }
        }
        Ok(())
    }
}

fn exited_within(child: &mut Child, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if child.try_wait().ok().flatten().is_some() {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// What a recording watches.
#[derive(Clone, Copy)]
enum Watch {
    /// The app's running process.
    App,
    /// The whole device, for a launch the app's process does not yet exist
    /// for.
    Everything,
}

struct Profiling {
    child: Child,
    path: PathBuf,
    seconds: f64,
}

/// Every table a trace's table of contents lists, exported beside it as
/// `<trace>.<schema>.xml`, with the table of contents as `<trace>.toc.xml`.
/// The names of the tables exported.
fn export_tables(trace: &Path) -> Result<Vec<String>, String> {
    let export = |args: &[&str], to: PathBuf| -> Result<(), String> {
        let sink = File::create(&to).map_err(|error| error.to_string())?;
        let status = Command::new("xcrun")
            .args(["xctrace", "export", "--input"])
            .arg(trace)
            .args(args)
            .stdout(sink)
            .stderr(Stdio::null())
            .status()
            .map_err(|error| format!("xctrace export: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("xctrace export {}: {status}", args.join(" ")))
        }
    };
    let stem = trace.with_extension("");
    let beside = |suffix: &str| PathBuf::from(format!("{}.{suffix}.xml", stem.display()));
    let toc = beside("toc");
    export(&["--toc"], toc.clone())?;
    let contents = std::fs::read_to_string(&toc).map_err(|error| error.to_string())?;
    let mut schemas: Vec<String> = contents
        .split("schema=\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next().map(str::to_owned))
        .collect();
    schemas.sort();
    schemas.dedup();
    let mut exported = Vec::new();
    for schema in schemas {
        let xpath = format!("/trace-toc/run[@number=\"1\"]/data/table[@schema=\"{schema}\"]");
        if export(&["--xpath", &xpath], beside(&schema)).is_ok() {
            exported.push(schema);
        }
    }
    Ok(exported)
}

/// The pairing link with the gate's address in place of the machine's.
fn gated_link(link: &str, gate: &str) -> Result<String, String> {
    let (prefix, encoded) = match link.split_once('=') {
        Some((prefix, encoded)) => (Some(prefix), encoded),
        None => (None, link),
    };
    let decoded = URL_SAFE_NO_PAD
        .decode(encoded.trim_end_matches('='))
        .map_err(|error| format!("the pairing link: {error}"))?;
    let mut payload: Value =
        serde_json::from_slice(&decoded).map_err(|error| format!("the pairing link: {error}"))?;
    payload["addrs"] = json!([gate]);
    let rewritten = URL_SAFE_NO_PAD.encode(payload.to_string());
    Ok(match prefix {
        Some(prefix) => format!("{prefix}={rewritten}"),
        None => rewritten,
    })
}

// --- judging -------------------------------------------------------------

#[derive(Serialize)]
struct Judged {
    metric: &'static str,
    unit: &'static str,
    group: &'static str,
    samples: Vec<f64>,
    median: f64,
    worst: f64,
    budget: f64,
    worst_budget: Option<f64>,
    tolerance_percent: Option<u32>,
    baseline: Option<f64>,
    drift_percent: Option<f64>,
    passed: bool,
    provisional: bool,
    note: String,
}

#[derive(Serialize)]
struct Verdict {
    passed: bool,
    results: Vec<Judged>,
}

/// Every measured metric against its budgets and the baseline; a metric
/// named in `provisional` is reported but not judged.
fn judge(
    samples: &Samples,
    budgets: &[Budget],
    baseline: Option<&Value>,
    provisional: &[&str],
) -> Verdict {
    let mut results = Vec::new();
    for budget in budgets {
        let values = samples
            .iter()
            .find(|(name, _)| *name == budget.metric)
            .map_or(&[][..], |(_, values)| values.as_slice());
        if values.is_empty() {
            continue;
        }
        let unit = budget.unit;
        let med = median(values);
        let worst = values.iter().copied().fold(f64::MIN, f64::max);
        let mut notes = Vec::new();
        let mut passed = true;
        let provisional = provisional.contains(&budget.metric);
        if provisional {
            notes
                .push("budget and baseline to be set from this measurement; not judged".to_owned());
        }
        if med > budget.median && !provisional {
            passed = false;
            notes.push(format!(
                "median {med:.1} {unit} over the budget of {}",
                plain(budget.median)
            ));
        }
        if let Some(worst_budget) = budget.worst
            && worst > worst_budget
            && !provisional
        {
            passed = false;
            notes.push(format!(
                "worst {worst:.1} {unit} over the worst-sample budget of {}",
                plain(worst_budget)
            ));
        }
        let recorded = baseline
            .filter(|_| !provisional)
            .and_then(|baseline| baseline[budget.metric].as_f64());
        let mut drift = None;
        if let (Some(recorded), Some(tolerance)) = (recorded, budget.tolerance) {
            let allowed = recorded * (1.0 + f64::from(tolerance) / 100.0);
            drift = Some(if recorded != 0.0 {
                (med - recorded) / recorded * 100.0
            } else if med == recorded {
                0.0
            } else {
                f64::INFINITY
            });
            if med > allowed && med > recorded {
                passed = false;
                notes.push(format!(
                    "median {med:.1} {unit} is {:+.1}% over the baseline of {recorded:.1}, past the {tolerance}% tolerance",
                    drift.unwrap_or_default()
                ));
            }
        }
        results.push(Judged {
            metric: budget.metric,
            unit,
            group: budget.group.name(),
            samples: values.to_vec(),
            median: med,
            worst,
            budget: budget.median,
            worst_budget: budget.worst,
            tolerance_percent: budget.tolerance,
            baseline: recorded,
            drift_percent: drift,
            passed,
            provisional,
            note: notes.join("; "),
        });
    }
    Verdict {
        passed: results.iter().all(|result| result.passed),
        results,
    }
}

/// One metric as the run prints it: the median and worst against the
/// budget, the baseline and drift where there is one, and the verdict.
fn summary_line(result: &Judged) -> String {
    let against = match (result.baseline, result.drift_percent) {
        (Some(baseline), Some(drift)) => format!(", baseline {baseline:.1} ({drift:+.1}%)"),
        (Some(baseline), None) => format!(", baseline {baseline:.1} (ceiling only)"),
        (None, _) => String::new(),
    };
    let verdict = if result.provisional {
        format!(" — {}", result.note)
    } else if result.passed {
        String::new()
    } else {
        format!(" — FAILED: {}", result.note)
    };
    format!(
        "{}: median {:.1} {}, worst {:.1}, budget {}{against}{verdict}",
        result.metric,
        result.median,
        result.unit,
        result.worst,
        plain(result.budget)
    )
}

fn verdict_cell(result: &Judged) -> String {
    if result.provisional {
        format!("MEASURED: {}", result.note)
    } else if result.passed {
        "PASS".to_owned()
    } else {
        format!("FAIL: {}", result.note)
    }
}

fn report(
    verdict: &Verdict,
    enrolled: &str,
    bench: &Bench,
    minutes: f64,
    notes: &[String],
    load: (&str, &str),
) -> String {
    let mut lines = vec![
        "# The phone measured".to_owned(),
        String::new(),
        format!(
            "`{enrolled}`: {}, {SAMPLES} samples per metric; the run took {minutes:.1} minutes.",
            bench.described()
        ),
        String::new(),
    ];
    match bench {
        Bench::Simulator { .. } => lines.extend([
            // A busy Mac slows the simulator; the load says whether a miss is the app's.
            format!(
                "Load average (1, 5, 15 minutes) before the run: {} · after: {}.",
                load.0, load.1
            ),
            String::new(),
            "The simulator reports 60 Hz and composites through the Mac's display: hitch time is \
             display-link accounting, a proxy for a device's hitch metric."
                .to_owned(),
            String::new(),
        ]),
        Bench::Phone(_) => {
            if let Some(cold) = verdict
                .results
                .iter()
                .find(|result| result.metric == "cold first frame")
            {
                let off = cold.median - LAUNCH_TARGET_MS;
                lines.extend([
                    format!(
                        "Cold first frame: median {:.1} ms against the {LAUNCH_TARGET_MS:.0} ms \
                         launch target, {:.1} ms {}.",
                        cold.median,
                        off.abs(),
                        if off > 0.0 { "over" } else { "under" }
                    ),
                    String::new(),
                ]);
            }
        }
    }
    lines.extend([
        "| Metric | Median | Worst | Budget | Baseline | Drift | Verdict |".to_owned(),
        "| --- | --- | --- | --- | --- | --- | --- |".to_owned(),
    ]);
    for result in &verdict.results {
        let unit = result.unit;
        let mut budget = if result.provisional {
            "to be set from this run".to_owned()
        } else {
            format!("{} {unit}", plain(result.budget))
        };
        if let Some(worst) = result.worst_budget {
            budget += &format!(" (worst {})", plain(worst));
        }
        let baseline = result
            .baseline
            .map_or_else(|| "none".to_owned(), |baseline| format!("{baseline:.1}"));
        let drift = match (result.tolerance_percent, result.drift_percent) {
            (None, _) => "ceiling only".to_owned(),
            (Some(_), Some(drift)) => format!("{drift:+.1}%"),
            (Some(_), None) => "unavailable".to_owned(),
        };
        lines.push(format!(
            "| {} | {:.1} {unit} | {:.1} {unit} | {budget} | {baseline} | {drift} | {} |",
            result.metric,
            result.median,
            result.worst,
            verdict_cell(result)
        ));
    }
    lines.extend(["".to_owned(), "## Samples".to_owned(), String::new()]);
    lines.extend(notes.iter().map(|note| format!("- {note}")));
    lines.push(String::new());
    lines.join("\n")
}

// --- the command ---------------------------------------------------------

/// What `testnet journey perf` was asked to do.
pub(crate) struct Asked {
    /// Say which machine this is and whether its baseline exists.
    pub describe: bool,
    pub only: Vec<Group>,
    /// Record this run's medians as the machine's baseline, after every
    /// budget passes; needs a whole run.
    pub baseline: bool,
    /// Draw every surface flat (the reduce-transparency setting) for the
    /// whole run: a diagnostic, never a baseline, that says how much of a
    /// number is the glass.
    pub flat: bool,
    /// Record the first sample of `trace.during` under Instruments.
    pub trace: Option<Trace>,
    /// Drive the phone plugged into this Mac rather than the simulator.
    pub device: bool,
}

/// Where a run measures: the enrolled Mac's simulator, or a phone.
enum Bench {
    Simulator { model: String },
    Phone(Phone),
}

impl Bench {
    /// What the report says the numbers were taken on.
    fn described(&self) -> String {
        match self {
            Bench::Simulator { model } => {
                format!("this Mac ({model}), the `Measured` app on the `golden` simulator")
            }
            Bench::Phone(phone) => format!(
                "{} ({}, iOS {}), the `Measured` app driven over its USB link",
                phone.name, phone.model, phone.os_version
            ),
        }
    }
}

/// The enrolled name a phone's baseline is kept under and its budgets, or
/// why it cannot run.
fn enrolled_phone(phone: &Phone) -> Result<(String, Vec<Budget>), String> {
    let major = phone.os_version.split('.').next().unwrap_or_default();
    PHONES
        .iter()
        .find(|enrolled| enrolled.model == phone.model)
        .map(|enrolled| (format!("{}-ios{major}", enrolled.name), enrolled.budgets()))
        .ok_or_else(|| {
            format!(
                "{} is a {}, which is not enrolled; the enrolled phones are {}",
                phone.name,
                phone.model,
                PHONES
                    .iter()
                    .map(|enrolled| format!("{} ({})", enrolled.name, enrolled.model))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

pub(crate) fn run(asked: Asked) -> ExitCode {
    let root = root();
    let refused = |why: String| {
        eprintln!("phone perf: {why}");
        ExitCode::from(2)
    };
    let enrolled = if asked.device {
        device::connected().and_then(|phone| {
            let (name, budgets) = enrolled_phone(&phone)?;
            Ok((Bench::Phone(phone), name, budgets))
        })
    } else {
        machine().map(|(model, name)| {
            (
                Bench::Simulator { model },
                name.to_owned(),
                BUDGETS.to_vec(),
            )
        })
    };
    let (bench, name, budgets) = match enrolled {
        Ok(enrolled) => enrolled,
        Err(why) => return refused(why),
    };
    let model = match &bench {
        Bench::Simulator { model } => model.clone(),
        Bench::Phone(phone) => phone.model.clone(),
    };
    let app = root.join(if asked.device { DEVICE_APP } else { APP });
    let baseline_file = root.join(BASELINES).join(format!("{name}.json"));
    if asked.describe {
        println!("measuring on {}, enrolled as {name}", bench.described());
        let present = if baseline_file.is_file() {
            "present"
        } else {
            "missing"
        };
        println!("baseline {}: {present}", baseline_file.display());
        let built = if app.is_dir() { "built" } else { "not built" };
        println!("app: {} ({built})", app.display());
        return ExitCode::SUCCESS;
    }
    if asked.baseline && !asked.only.is_empty() {
        return refused("a baseline is a whole run; --baseline takes no --only".to_owned());
    }
    if asked.baseline && asked.flat {
        return refused(
            "a flat run is a diagnostic; --baseline records the app as it ships".to_owned(),
        );
    }
    if let Some(trace) = &asked.trace
        && (asked.baseline || !(asked.only.is_empty() || asked.only.contains(&trace.during)))
    {
        return refused(format!(
            "--trace records the {} group; it takes no --baseline and needs that group in --only",
            trace.during.name()
        ));
    }
    if asked.trace.as_ref().is_some_and(|trace| {
        trace
            .path
            .extension()
            .is_none_or(|extension| extension != "trace")
    }) {
        return refused("--trace names a .trace bundle".to_owned());
    }
    if !app.is_dir() {
        return refused(format!(
            "{} is not built; `scripts/ios-build.py --configuration Measured{}` builds it",
            app.display(),
            if asked.device { " --device" } else { "" }
        ));
    }
    let tool = root.join(TOPOLOGY_TOOL);
    if !tool.is_file() {
        return refused(format!(
            "{} is not built; `just ios tools` builds it",
            tool.display()
        ));
    }
    match measure(asked, bench, &budgets, &model, &name, app, &baseline_file) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("phone perf: over budget");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn measure(
    asked: Asked,
    bench: Bench,
    budgets: &[Budget],
    model: &str,
    name: &str,
    app: PathBuf,
    baseline_file: &Path,
) -> Result<bool, String> {
    let root = root();
    let output = root.join(OUTPUT);
    let _ = std::fs::remove_dir_all(&output);
    std::fs::create_dir_all(&output).map_err(|error| error.to_string())?;
    let topology = output.join("topology.json");
    let generated = Command::new(root.join(TOPOLOGY_TOOL))
        .arg(&topology)
        .output()
        .map_err(|error| format!("phone-topology: {error}"))?;
    if !generated.status.success() {
        return Err(format!(
            "phone-topology: {}",
            String::from_utf8_lossy(&generated.stderr)
        ));
    }
    // A run recording a baseline replaces the old one, so drift from it
    // says nothing: the new medians are judged on the budgets alone.
    let baseline = if asked.baseline {
        None
    } else {
        std::fs::read_to_string(baseline_file)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .map(|recorded| recorded["medians"].clone())
    };
    if baseline.is_none() {
        println!("no baseline for {name} in use: judged on budgets alone");
    }

    let load_before = load_average();
    let began = Instant::now();
    let (device, target) = match &bench {
        Bench::Simulator { .. } => {
            let (device, udid) = golden_device()?;
            (device, Target::Simulator(udid))
        }
        // The hardware, not the name its owner gave the phone: the baseline
        // is committed, and that name is personal.
        Bench::Phone(phone) => (
            format!("{} (iOS {})", phone.model, phone.os_version),
            Target::Phone(phone.clone()),
        ),
    };
    let mut journey = PhoneJourney::serve(
        Setup {
            topology,
            output: output.join("journey"),
            goldens: output.clone(),
            app,
        },
        &target,
    )?;
    // The app is measured as it ships, glass and all, unless a flat run
    // was asked for as a diagnostic.
    journey.flat = asked.flat;
    let mut run = Run {
        journey,
        only: asked.only,
        flat: asked.flat,
        trace: match asked.trace {
            Some(trace) => Some(Trace {
                path: std::path::absolute(trace.path).map_err(|error| error.to_string())?,
                ..trace
            }),
            None => None,
        },
        traced: false,
        samples: BUDGETS
            .iter()
            .map(|budget| (budget.metric, Vec::new()))
            .collect(),
        notes: Vec::new(),
        gates: BTreeMap::new(),
        relay_gate: None,
        last_launch: Instant::now(),
    };
    let measured = run.measure_everything();
    match &measured {
        Ok(()) => run.journey.finish(&[]),
        Err(error) => {
            run.journey.fail(error);
            // The served machines' own logs, which closing the driver
            // discards, are what says why a launch never reconciled.
            copy_logs(Path::new(SCRATCH_DIR), &output.join("served"));
        }
    }
    let closed = run.journey.close();
    measured?;
    closed?;
    let minutes = began.elapsed().as_secs_f64() / 60.0;

    let verdict = judge(
        &run.samples,
        budgets,
        baseline.as_ref(),
        SET_FROM_FIRST_MEASUREMENT,
    );
    let write = |file: &Path, text: String| {
        std::fs::write(file, text).map_err(|error| format!("{}: {error}", file.display()))
    };
    write(
        &output.join("samples.json"),
        pretty(&Ordered(&run.samples))?,
    )?;
    write(&output.join("verdict.json"), pretty(&verdict)?)?;
    let after = load_average();
    write(
        &output.join("report.md"),
        report(
            &verdict,
            name,
            &bench,
            minutes,
            &run.notes,
            (&load_before, &after),
        ),
    )?;
    for result in &verdict.results {
        println!("{}", summary_line(result));
    }
    println!(
        "the run took {minutes:.1} minutes; {}",
        output.join("report.md").display()
    );
    if !verdict.passed {
        return Ok(false);
    }
    if asked.baseline {
        if let Some(parent) = baseline_file.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let medians: Vec<(&str, f64)> = verdict
            .results
            .iter()
            .map(|result| (result.metric, result.median))
            .collect();
        let recorded = Recorded {
            schema_version: 1,
            machine_model: model,
            configuration: "Measured",
            device: &device,
            medians: Ordered(&medians),
        };
        write(baseline_file, pretty(&recorded)?)?;
        println!("recorded {}", baseline_file.display());
    }
    Ok(true)
}

/// A machine's baseline as committed.
#[derive(Serialize)]
struct Recorded<'a> {
    schema_version: u32,
    machine_model: &'a str,
    configuration: &'a str,
    /// The simulator, or the phone and its iOS version.
    device: &'a str,
    medians: Ordered<'a, f64>,
}

fn pretty(value: &impl Serialize) -> Result<String, String> {
    serde_json::to_string_pretty(value)
        .map(|text| text + "\n")
        .map_err(|error| error.to_string())
}

/// A copy of the served machines' directory, without their sockets and
/// stores.
fn copy_logs(from: &Path, to: &Path) {
    let Ok(entries) = std::fs::read_dir(from) else {
        return;
    };
    let _ = std::fs::create_dir_all(to);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let named = name.to_string_lossy();
        if named == "sock" || named.ends_with(".sock") || named.contains(".sqlite") {
            continue;
        }
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => copy_logs(&path, &to.join(&name)),
            Ok(kind) if kind.is_file() => {
                let _ = std::fs::copy(&path, to.join(&name));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A budget row: metric, unit, group, budget, worst, tolerance.
    type Stated = (String, String, String, f64, Option<f64>, Option<u32>);

    fn phone_section() -> String {
        let text = std::fs::read_to_string(root().join("docs/PERFORMANCE.md")).expect("the page");
        text[text.find("## The phone").expect("The phone")..].to_owned()
    }

    /// The body rows of the first table whose header row starts with
    /// `header`.
    fn table_rows(section: &str, header: &str) -> Vec<Vec<String>> {
        let lines: Vec<&str> = section.lines().collect();
        let at = lines
            .iter()
            .position(|line| line.starts_with(header))
            .unwrap_or_else(|| panic!("no table starting {header:?} under The phone"));
        lines[at + 2..]
            .iter()
            .take_while(|row| row.starts_with('|'))
            .map(|row| {
                row.trim()
                    .trim_matches('|')
                    .split('|')
                    .map(|cell| cell.trim().to_owned())
                    .collect()
            })
            .collect()
    }

    /// The number a cell starts with, and what follows it.
    fn number(cell: &str) -> Option<(f64, &str)> {
        let end = cell
            .find(|c: char| !(c.is_ascii_digit() || c == ',' || c == '.'))
            .unwrap_or(cell.len());
        let number = cell[..end].replace(',', "").parse().ok()?;
        Some((number, cell[end..].trim()))
    }

    #[test]
    fn the_budget_table_matches_the_runners_budgets() {
        let rows = table_rows(&phone_section(), "| Metric | Group |");
        let stated: Vec<Stated> = rows
            .iter()
            .map(|row| {
                let (budget, unit) = number(&row[3]).expect("a budget");
                let tolerance = (!row[5].contains("ceiling"))
                    .then(|| number(&row[5]).map_or(0, |(drift, _)| drift as u32));
                (
                    row[0].trim_matches('`').to_owned(),
                    unit.to_owned(),
                    row[1].clone(),
                    budget,
                    number(&row[4]).map(|(worst, _)| worst),
                    tolerance,
                )
            })
            .collect();
        let enforced: Vec<Stated> = BUDGETS
            .iter()
            .map(|budget| {
                (
                    budget.metric.to_owned(),
                    budget.unit.to_owned(),
                    budget.group.name().to_owned(),
                    budget.median,
                    budget.worst,
                    budget.tolerance,
                )
            })
            .collect();
        assert_eq!(stated, enforced);
    }

    #[test]
    fn a_budget_still_to_be_measured_says_so_on_the_page() {
        let rows = table_rows(&phone_section(), "| Metric | Group |");
        let provisional: Vec<&str> = rows
            .iter()
            .filter(|row| row[2].contains("set from the first measurement"))
            .map(|row| row[0].trim_matches('`'))
            .collect();
        assert_eq!(provisional, SET_FROM_FIRST_MEASUREMENT);
    }

    #[test]
    fn the_machine_table_matches_the_runners_machines() {
        let rows = table_rows(&phone_section(), "| Machine | Model |");
        let stated: Vec<(&str, &str)> = rows
            .iter()
            .map(|row| (row[1].trim_matches('`'), row[0].trim_matches('`')))
            .collect();
        assert_eq!(stated, MACHINES);
    }

    #[test]
    fn every_enrolled_phone_budgets_every_metric_in_order() {
        let metrics: Vec<&str> = BUDGETS.iter().map(|budget| budget.metric).collect();
        for enrolled in PHONES {
            let budgeted: Vec<&str> = enrolled
                .budgets
                .iter()
                .map(|(metric, ..)| *metric)
                .collect();
            assert_eq!(budgeted, metrics, "{}", enrolled.name);
        }
    }

    #[test]
    fn the_phone_tables_match_the_runners_phones() {
        let rows = table_rows(&phone_section(), "| Phone | Model |");
        let stated: Vec<(&str, &str)> = rows
            .iter()
            .map(|row| (row[1].trim_matches('`'), row[0].trim_matches('`')))
            .collect();
        let enrolled: Vec<(&str, &str)> = PHONES
            .iter()
            .map(|enrolled| (enrolled.model, enrolled.name))
            .collect();
        assert_eq!(stated, enrolled);

        let rows = table_rows(&phone_section(), "| Metric | Phone |");
        let stated: Vec<(String, String, f64, Option<f64>)> = rows
            .iter()
            .map(|row| {
                (
                    row[0].trim_matches('`').to_owned(),
                    row[1].trim_matches('`').to_owned(),
                    number(&row[2]).expect("a budget").0,
                    number(&row[3]).map(|(worst, _)| worst),
                )
            })
            .collect();
        let enforced: Vec<(String, String, f64, Option<f64>)> = PHONES
            .iter()
            .flat_map(|enrolled| {
                enrolled.budgets().into_iter().map(|budget| {
                    (
                        budget.metric.to_owned(),
                        enrolled.name.to_owned(),
                        budget.median,
                        budget.worst,
                    )
                })
            })
            .collect();
        assert_eq!(stated, enforced);
    }

    #[test]
    fn a_link_rewritten_to_its_gate_changes_only_the_addresses() {
        let payload = json!({"host_id": "h", "secret": [1, 2], "addrs": ["127.0.0.1:5000"], "cloud_url": "https://c"});
        let encoded = URL_SAFE_NO_PAD.encode(payload.to_string());
        let link = gated_link(&format!("amux://pair?payload={encoded}"), "127.0.0.1:6000").unwrap();
        let (prefix, rewritten) = link.split_once('=').unwrap();
        assert_eq!(prefix, "amux://pair?payload");
        let decoded: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(rewritten).unwrap()).unwrap();
        let mut expected = payload;
        expected["addrs"] = json!(["127.0.0.1:6000"]);
        assert_eq!(decoded, expected);
    }

    fn samples(given: &[(&str, &[f64])]) -> Samples {
        BUDGETS
            .iter()
            .map(|budget| {
                let values = given
                    .iter()
                    .find(|(name, _)| *name == budget.metric)
                    .map_or_else(Vec::new, |(_, values)| values.to_vec());
                (budget.metric, values)
            })
            .collect()
    }

    #[test]
    fn a_median_over_budget_fails_and_an_unmeasured_metric_is_not_reported() {
        let verdict = judge(
            &samples(&[("cold first frame", &[400.0, 450.0, 520.0, 430.0, 410.0])]),
            BUDGETS,
            None,
            &[],
        );
        assert!(verdict.passed);
        let metrics: Vec<&str> = verdict.results.iter().map(|result| result.metric).collect();
        assert_eq!(metrics, ["cold first frame"]);
        let verdict = judge(
            &samples(&[("cold first frame", &[400.0, 610.0, 520.0, 530.0, 950.0])]),
            BUDGETS,
            None,
            &[],
        );
        assert!(!verdict.passed);
        assert!(verdict.results[0].note.contains("worst"));
    }

    #[test]
    fn a_ceiling_only_metric_ignores_a_baseline_it_rests_well_above() {
        let baseline = json!({"streaming hitch time": 0.0});
        let verdict = judge(
            &samples(&[("streaming hitch time", &[1.7; 5])]),
            BUDGETS,
            Some(&baseline),
            &[],
        );
        assert!(verdict.passed);
        assert_eq!(verdict.results[0].drift_percent, None);
        let text = report(
            &verdict,
            "pinned-mac",
            &Bench::Simulator {
                model: "Mac14,6".to_owned(),
            },
            1.0,
            &[],
            ("1.00 2.00 3.00", "1.50 2.00 3.00"),
        );
        assert!(text.contains("ceiling only"));
        assert!(text.contains("before the run: 1.00 2.00 3.00 · after: 1.50 2.00 3.00"));
        assert!(summary_line(&verdict.results[0]).contains("ceiling only"));
    }

    #[test]
    fn drift_past_the_tolerance_fails_inside_the_budget() {
        let baseline = json!({"cold first frame": 300.0});
        let inside = judge(
            &samples(&[("cold first frame", &[340.0; 5])]),
            BUDGETS,
            Some(&baseline),
            &[],
        );
        assert!(inside.passed);
        let past = judge(
            &samples(&[("cold first frame", &[350.0; 5])]),
            BUDGETS,
            Some(&baseline),
            &[],
        );
        assert!(!past.passed);
        assert!(past.results[0].note.contains("tolerance"));
    }

    #[test]
    fn a_budget_still_to_be_measured_is_reported_but_not_judged() {
        // The path is kept for the next metric whose measurement is
        // redefined; the relay launch stands in for it.
        let name = "reconciliation through the relay at 100 ms";
        let budget = BUDGETS
            .iter()
            .find(|budget| budget.metric == name)
            .unwrap()
            .median;
        let baseline = json!({name: budget / 2.0});
        let verdict = judge(
            &samples(&[(name, &[budget * 1.2; 5])]),
            BUDGETS,
            Some(&baseline),
            &[name],
        );
        assert!(verdict.passed);
        let result = &verdict.results[0];
        assert!(result.provisional);
        assert_eq!(result.baseline, None);
        assert!(result.note.contains("set from this measurement"));
        let text = report(
            &verdict,
            "pinned-mac",
            &Bench::Simulator {
                model: "Mac14,6".to_owned(),
            },
            1.0,
            &[],
            ("1.00 2.00 3.00", "1.00 2.00 3.00"),
        );
        assert!(text.contains("to be set from this run"));
        assert!(text.contains("MEASURED"));
        assert!(!summary_line(result).contains("FAILED"));
    }

    #[test]
    fn a_count_metric_has_no_slack() {
        let verdict = judge(
            &samples(&[("idle display ticks", &[0.0, 0.0, 1.0, 0.0, 0.0])]),
            BUDGETS,
            None,
            &[],
        );
        assert!(!verdict.passed);
    }

    #[test]
    fn a_median_of_an_even_count_is_between_the_middle_two() {
        assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
    }
}
