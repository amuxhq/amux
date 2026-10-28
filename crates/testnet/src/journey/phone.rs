//! The phone journey driver: starts a served topology with `testnet serve`,
//! installs the debug app fresh on the leased simulator and launches it with
//! its door, the net's discovery scope, a loopback direct-link listener and,
//! when the net has one, the served relay. It then acts only as a person
//! would, through door verbs a person's action maps to: tap, type, paste,
//! and pair by the link or code the machine printed. It judges
//! by what the hosts recorded (the served net's Chat, Inventory and
//! ProviderInput verbs) and by compared screens: the door's element geometry
//! against `journeys/goldens/phone/<story>/<label>.elements.txt`. Each
//! compared screen is photographed too, into the run's `actual/` beside its
//! geometry, for a person to look at; no picture is compared, since a
//! simulator's display depends on the Mac drawing it. Stories supply the acts
//! and the assertions; `UPDATE_JOURNEY_GOLDENS=1` rewrites the geometry that
//! differs. The accessibility audit and the performance suite drive the app
//! through the same driver.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{LazyLock, OnceLock, mpsc};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{Value, json};

use super::device::Phone;
use super::{Record, Story, door, line_diff, root};
use crate::Binaries;
use crate::ctl::session::pin_in;
use crate::door::{Control, Readiness};

pub(crate) const BUNDLE_ID: &str = "sh.amux.app";
/// The pinned simulator kind every phone journey runs on.
const SIMULATOR: &str = "golden";
/// A surface a view reports under a name ending here holds text that moves
/// with the run (an age, a scratch path); its words are left out of the
/// geometry.
const VOLATILE: &str = ".volatile";
/// This phone's own name and key, which change with the leased simulator
/// and every fresh install.
const OWN_IDENTITY: [&str; 2] = ["you.name", "you.fingerprint"];
/// One fixed place, so every path the phone draws (an agent's working
/// directory, an ask's scope) is the same on every run and every machine.
/// Short: daemons bind Unix sockets under it. Journeys in other checkouts
/// wait for it on the lock.
pub(crate) const SCRATCH_DIR: &str = "/tmp/amux-phone-journey";
const SCRATCH_LOCK: &str = "/tmp/amux-phone-journey.lock";
/// How far a line with masked words may move sideways or change width:
/// "Worked 9s" is narrower than "Worked 12s", and trailing text moves its
/// left edge.
const MASKED_SLACK: i64 = 16;
/// The tabs' roots by the name the door gives the tab on screen. A tab
/// somebody has been to is kept and still reports its elements while another
/// tab, or a page pushed over it, is on screen; one never reached for is not
/// built and reports nothing.
const TAB_ROOTS: [(&str, &str); 2] = [("agents", "home."), ("hosts", "hosts.")];
/// Pages pushed over a tab, which hide the tab's root beneath them.
const PAGES: [&str; 3] = ["chat", "pin", "pair-confirm"];
/// The served machines say what they did with each link and stream, so a
/// reconciliation that took longer than its round trips can be read from
/// their side too; a caller's own filter wins.
const RUST_LOG: &str = "info,node::link=debug,node::services::reachability=debug,node::routing=debug,node::sources=debug,node::dispatcher=debug,node::edge::peer=debug";

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid pattern")
}

/// Relative ages move with the clock.
static AGE: LazyLock<Regex> = LazyLock::new(|| regex(r"\b(just now|\d+[smhd] ago)\b"));
/// A key made fresh with every install: whole, as sixteen groups of four
/// hex digits, or shortened to its ends.
static KEY: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\b[0-9a-f]{4}(?: [0-9a-f]{4}){15}\b|\b[0-9a-f]{4}…[0-9a-f]{4}\b"));
/// The leased simulator's name, which the phone shows as its own: which of
/// the pool's devices a run is given is not the run's to choose.
static DEVICE: LazyLock<Regex> = LazyLock::new(|| regex(r"\bamux-[a-z]+-\d+\b"));
/// Durations measured on the run's own clock, and countdowns.
static DURATION: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\b\d+m \d+s\b|\b\d+(?:\.\d+)?(?:ms|s|m|h)\b"));
/// When a usage limit resets: a time of day on the run's wall clock, or a
/// weekday.
static RESETS: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\bresets (?:\d{1,2}:\d{2}(?:\s?[AP]M)?|[A-Z][a-z]{2})"));
static UUID: LazyLock<Regex> =
    LazyLock::new(|| regex(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"));
/// Words `normalize` masked, whose drawn width still follows what they said.
static MASKED: LazyLock<Regex> = LazyLock::new(|| regex(r"<(?:age|key|device|t|time)>"));

/// The served net's scratch suffixes (`aj-` or `testnet` and up to eight
/// word characters ending a word) as `x`s, keeping every width.
fn mask_scratch(text: &str) -> String {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let head = ["aj-", "testnet"]
            .into_iter()
            .find(|head| rest.starts_with(head));
        if let Some(head) = head {
            let tail = &rest[head.len()..];
            let run = tail.len() - tail.trim_start_matches(word).len();
            if (1..=8).contains(&run) {
                out.push_str(head);
                out.push_str(&"x".repeat(run));
                at += head.len() + run;
                continue;
            }
        }
        let next = rest.chars().next().expect("not at the end");
        out.push(next);
        at += next.len_utf8();
    }
    out
}

/// Ages, keys, the phone's own name, durations, reset times and the served
/// net's scratch suffixes, masked as the terminal driver masks ages and
/// scratch paths.
pub(crate) fn normalize(text: &str) -> String {
    let text = AGE.replace_all(text, "<age>");
    let text = KEY.replace_all(&text, "<key>");
    let text = DEVICE.replace_all(&text, "<device>");
    let text = DURATION.replace_all(&text, "<t>");
    let text = RESETS.replace_all(&text, "resets <time>");
    mask_scratch(&text)
}

/// What a driven launch is told: its door, the net's discovery scope and
/// the machines its browser may report, loopback direct links and, when the
/// net has one, the served relay by both its carriers. Element geometry is
/// asked for unless the caller measures the app and turns it on itself only
/// to tap. A `relay_quic` address stands in for the relay's own, a gate in
/// front of it, and leaves the plaintext TCP carrier out: a measurement
/// wants the carrier a phone away from home uses, and the fallback, dialled
/// a moment later on loopback, would win the race against a gated dial.
fn launch_arguments(
    ready: &Readiness,
    scope: &str,
    found: &[String],
    door_port: u16,
    geometry: bool,
    relay_quic: Option<&str>,
) -> Vec<String> {
    let mut arguments = vec!["-amux-door-port".to_owned(), door_port.to_string()];
    if geometry {
        arguments.push("-amux-element-geometry".to_owned());
    }
    arguments.extend([
        "-amux-discovery-scope".to_owned(),
        scope.to_owned(),
        "-amux-discover-only".to_owned(),
        found.join(","),
        "-amux-lan-bind".to_owned(),
        "127.0.0.1:0".to_owned(),
        // The fleet's offer of notifications, put away as a person who
        // answered it once would have: the screens the journeys read are
        // the list's, and the offer has a component snapshot of its own.
        "-notificationsOfferDeclined".to_owned(),
        "YES".to_owned(),
    ]);
    if let Some(cloud) = &ready.cloud_url
        && (ready.relay_tcp.is_some() || ready.relay_quic.is_some())
    {
        arguments.extend([
            "-amux-scripted-cloud".to_owned(),
            "-amux-relay".to_owned(),
            cloud.clone(),
        ]);
        if let Some(tcp) = ready.relay_tcp
            && relay_quic.is_none()
        {
            arguments.extend(["-amux-relay-tcp".to_owned(), tcp.to_string()]);
        }
        if let (Some(quic), Some(root)) = (ready.relay_quic, &ready.relay_root) {
            arguments.extend([
                "-amux-relay-quic".to_owned(),
                relay_quic.map_or_else(|| quic.to_string(), str::to_owned),
                "-amux-relay-root".to_owned(),
                root.clone(),
            ]);
        }
    }
    arguments
}

fn identifier(element: &Value) -> &str {
    element["identifier"].as_str().unwrap_or_default()
}

fn is_volatile(identifier: &str, named: &[&str]) -> bool {
    identifier.ends_with(VOLATILE) || named.contains(&identifier)
}

/// What is drawn, without the tab roots a pushed page covers: they are
/// still laid out underneath, where nobody can see them, and keep changing
/// (a fleet re-sorting behind a chat).
fn uncovered(elements: &[Value]) -> Vec<Value> {
    if !elements
        .iter()
        .any(|element| PAGES.contains(&identifier(element)))
    {
        return elements.to_vec();
    }
    elements
        .iter()
        .filter(|element| {
            let name = identifier(element);
            !TAB_ROOTS
                .iter()
                .any(|(_, root)| name.starts_with(root) || format!("{name}.") == *root)
        })
        .cloned()
        .collect()
}

fn on_screen(identifier: &str, screen: Option<&str>) -> bool {
    let Some(root) = TAB_ROOTS
        .iter()
        .map(|(_, root)| *root)
        .find(|root| identifier.starts_with(root))
    else {
        return true;
    };
    let Some(screen) = screen else {
        return true;
    };
    TAB_ROOTS
        .iter()
        .find(|(tab, _)| *tab == screen)
        .is_some_and(|(_, tab_root)| *tab_root == root)
}

fn number(value: &Value) -> f64 {
    value.as_f64().unwrap_or_default()
}

/// Every id the net made this run, as the name it was declared by; any
/// other id as `<id>`.
fn named_ids(text: &str, ids: &BTreeMap<String, String>) -> String {
    UUID.replace_all(text, |found: &regex::Captures| {
        format!("<{}>", ids.get(&found[0]).map_or("id", String::as_str))
    })
    .into_owned()
}

/// Whether an element can be acted on: a missing `enabled` means it can,
/// and a null one that it cannot.
fn enabled(element: &Value) -> bool {
    match element.get("enabled") {
        None => true,
        Some(Value::Bool(enabled)) => *enabled,
        Some(Value::Null) => false,
        Some(_) => true,
    }
}

/// One line per element: its name, its words and where it is drawn, in
/// whole points. Volatile surfaces keep their place and lose their words;
/// ids read as the names the topology gave them.
fn geometry(elements: &[Value], named: &[&str], ids: &BTreeMap<String, String>) -> String {
    let mut lines = Vec::new();
    for element in elements {
        let name = identifier(element);
        if is_volatile(name, named) {
            // Its size follows its words (a duration's width), so only that
            // it is drawn is recorded.
            lines.push(named_ids(&format!("{name} | <volatile>"), ids));
            continue;
        }
        let frame = &element["frame"];
        let place: Vec<String> = ["x", "y", "width", "height"]
            .iter()
            .map(|key| (number(&frame[key]).round_ties_even() as i64).to_string())
            .collect();
        let words = normalize(&format!(
            "{} | {}",
            element["label"].as_str().unwrap_or_default(),
            element["value"].as_str().unwrap_or_default()
        ));
        let state = if enabled(element) { "" } else { " (disabled)" };
        lines.push(named_ids(
            &format!("{name} | {words} | {}{state}", place.join(",")),
            ids,
        ));
    }
    lines.join("\n") + "\n"
}

fn same_line(approved: &str, drawn: &str) -> bool {
    if approved == drawn {
        return true;
    }
    if !MASKED.is_match(approved) {
        return false;
    }
    let parts = |line: &str| -> Option<(String, String, Vec<i64>)> {
        let (words, place) = line.rsplit_once(" | ").unwrap_or(("", line));
        let (frame, state) = place.split_once(' ').unwrap_or((place, ""));
        let numbers: Vec<i64> = frame
            .split(',')
            .map(|value| value.parse().ok())
            .collect::<Option<_>>()?;
        (numbers.len() == 4).then(|| (words.to_owned(), state.to_owned(), numbers))
    };
    let (Some((words, state, frame)), Some((drawn_words, drawn_state, drawn_frame))) =
        (parts(approved), parts(drawn))
    else {
        return false;
    };
    (words, state) == (drawn_words, drawn_state)
        && (frame[1], frame[3]) == (drawn_frame[1], drawn_frame[3])
        && (frame[0] - drawn_frame[0]).abs() <= MASKED_SLACK
        && (frame[2] - drawn_frame[2]).abs() <= MASKED_SLACK
}

/// Whether two geometry records hold the same lines, in any order.
///
/// Each line carries its own frame, so the order says nothing about where
/// anything is drawn; it is the order SwiftUI reduced the screen's reports
/// in and the system walked its accessibility tree, and both move between
/// runs of one build. An exact line is matched before a near one, so a near
/// match never takes the line another needed exactly.
fn same_geometry(approved: &str, drawn: &str) -> bool {
    let mut left: Vec<&str> = drawn.lines().collect();
    let mut near = Vec::new();
    for line in approved.lines() {
        match left.iter().position(|candidate| *candidate == line) {
            Some(at) => {
                left.remove(at);
            }
            None => near.push(line),
        }
    }
    for line in near {
        match left.iter().position(|candidate| same_line(line, candidate)) {
            Some(at) => {
                left.remove(at);
            }
            None => return false,
        }
    }
    left.is_empty()
}

/// What is drawn by name, in drawing order; a name drawn again (every
/// prompt row is `chat.row.prompt`) gets `#2`, `#3`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Drawn(Vec<(String, Value)>);

impl Drawn {
    pub(crate) fn of(elements: &[Value]) -> Drawn {
        let mut named: Vec<(String, Value)> = Vec::new();
        for element in elements {
            let base = identifier(element);
            let mut name = base.to_owned();
            let mut count = 1;
            while named.iter().any(|(taken, _)| *taken == name) {
                count += 1;
                name = format!("{base}#{count}");
            }
            named.push((name, element.clone()));
        }
        Drawn(named)
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Value> {
        self.0
            .iter()
            .find(|(drawn, _)| drawn == name)
            .map(|(_, element)| element)
    }

    pub(crate) fn has(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// The element's `key`, or null when either is missing.
    pub(crate) fn field(&self, name: &str, key: &str) -> &Value {
        self.get(name).map_or(&Value::Null, |element| &element[key])
    }

    pub(crate) fn label(&self, name: &str) -> &str {
        self.field(name, "label").as_str().unwrap_or_default()
    }

    pub(crate) fn value(&self, name: &str) -> &str {
        self.field(name, "value").as_str().unwrap_or_default()
    }

    /// The element is drawn and says it is enabled.
    pub(crate) fn enabled(&self, name: &str) -> bool {
        *self.field(name, "enabled") == true
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0
            .iter()
            .map(|(name, element)| (name.as_str(), element))
    }

    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(name, _)| name.as_str())
    }
}

/// How a request to the app's door failed: refused by the app, or never
/// answered.
#[derive(Debug)]
pub(crate) enum AppError {
    Refused(String),
    Unreachable(String),
}

impl From<AppError> for String {
    fn from(error: AppError) -> String {
        match error {
            AppError::Refused(said) | AppError::Unreachable(said) => said,
        }
    }
}

/// `command`'s output, or its failure, within `timeout`.
pub(crate) fn run_within(mut command: Command, timeout: Duration) -> Result<String, String> {
    let described = format!("{command:?}");
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("{described}: {error}"))?;
    let read = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut said = String::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_string(&mut said);
            }
            said
        })
    };
    let stdout = read(
        child
            .stdout
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn std::io::Read + Send>),
    );
    let stderr = read(
        child
            .stderr
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn std::io::Read + Send>),
    );
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{described} took longer than {}s",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let (stdout, stderr) = (
        stdout.join().unwrap_or_default(),
        stderr.join().unwrap_or_default(),
    );
    if status.success() {
        Ok(stdout)
    } else {
        Err(format!("{described}: {status}: {}", stderr.trim()))
    }
}

pub(crate) fn simctl(args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut command = Command::new("xcrun");
    command.arg("simctl").args(args);
    run_within(command, timeout)
}

/// The udid of the device the golden kind means: the one this command
/// leased, present, booted and pinned by the simulator pool's own tool.
pub(crate) fn golden_udid() -> Result<String, String> {
    golden_device().map(|(_, udid)| udid)
}

/// The golden device's name and udid.
pub(crate) fn golden_device() -> Result<(String, String), String> {
    let mut command = Command::new(root().join("scripts/py"));
    command
        .args(["-B", "apps/apple/Tools/ios_simulators.py", SIMULATOR])
        .current_dir(root());
    let said = run_within(command, Duration::from_secs(1800))?;
    // `golden (amux-iphone-1): <udid> (iOS 26.5, booted, ...)`
    said.lines()
        .find_map(|line| {
            let rest = line.strip_prefix(&format!("{SIMULATOR} ("))?;
            let (name, rest) = rest.split_once("): ")?;
            let udid = rest.split_whitespace().next()?;
            Some((name.to_owned(), udid.to_owned()))
        })
        .ok_or_else(|| format!("the simulator pool named no {SIMULATOR} device: {said}"))
}

/// The debug app built for the simulator.
pub(crate) fn debug_app() -> PathBuf {
    root().join("target/ios/DerivedData/Build/Products/Debug-iphonesimulator/Amux.app")
}

fn free_port() -> Result<u16, String> {
    let probe = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    probe
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| error.to_string())
}

/// What a SIGTERM must undo when it ends a run from outside: the app on its
/// simulator and the served net. The bound a recipe puts on a run sends it.
static UNDO: OnceLock<std::sync::Mutex<Option<(Target, SocketAddr)>>> = OnceLock::new();

fn undo_on_terminate() {
    let undo = UNDO.get_or_init(|| std::sync::Mutex::new(None));
    static WATCHING: OnceLock<()> = OnceLock::new();
    WATCHING.get_or_init(|| {
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async {
                if let Ok(mut terminate) =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                {
                    terminate.recv().await;
                }
            });
            if let Some((target, control)) = undo.lock().ok().and_then(|held| held.clone()) {
                let _ = target.terminate();
                let _ = door(control, &Control::Shutdown, Duration::from_secs(10));
            }
            std::process::exit(143);
        });
    });
}

/// Something other than devicectl that launches the app on a phone with the
/// arguments and environment given: Instruments, to record the launch.
pub(crate) type Launcher = Box<dyn FnOnce(&[String], &[(&str, &str)]) -> Result<(), String>>;

/// What a run drives the app on: a simulator by its udid, or the phone
/// plugged into this Mac.
#[derive(Clone, Debug)]
pub(crate) enum Target {
    Simulator(String),
    Phone(Phone),
}

impl Target {
    fn terminate(&self) -> Result<(), String> {
        match self {
            Target::Simulator(udid) => {
                simctl(&["terminate", udid, BUNDLE_ID], Duration::from_secs(60)).map(|_| ())
            }
            Target::Phone(phone) => phone.terminate(),
        }
    }
}

/// Where a run serves its net from, writes what it found and finds its
/// goldens, and the build it installs.
pub(crate) struct Setup {
    pub topology: PathBuf,
    pub output: PathBuf,
    pub goldens: PathBuf,
    pub app: PathBuf,
}

pub(crate) struct PhoneJourney {
    /// The simulator's udid, or the phone's as xctrace names it.
    pub udid: String,
    /// The phone plugged in, when the run drives one rather than a
    /// simulator.
    pub phone: Option<Phone>,
    pub output: PathBuf,
    /// Where this run's screens' element geometry is compared, and whether
    /// geometry that no longer matches is rewritten there instead.
    goldens: PathBuf,
    pub update: bool,
    /// Whether every launch turns the app's reduce-transparency setting on,
    /// so each frosted surface is drawn flat. The render server finishes
    /// glass after the app has drawn, on its own schedule, and a photograph
    /// at any fixed moment shows one stage or another of that work; drawn
    /// flat, a journey's pictures show each screen finished. A run that
    /// measures the app as it ships turns this off before it launches.
    pub flat: bool,
    /// The build installed: the debug app, or the optimised one the
    /// performance suite measures.
    build: PathBuf,
    topology: Value,
    /// The binaries the served net runs, which a person at one of its
    /// machines runs too.
    binaries: Binaries,
    _lock: File,
    process: Option<Child>,
    pub ready: Readiness,
    pub record: Record,
    /// The app's door's port: on this Mac's loopback for a simulator, on
    /// the phone's link address for a phone.
    pub port: u16,
    /// The process id the app was launched as, on this Mac for a simulator
    /// or on the phone, for a profile to attach to or be filtered by.
    pub pid: u32,
    /// What launches the app on a phone next time, once, in devicectl's place.
    through: Option<Launcher>,
}

impl PhoneJourney {
    pub(crate) fn start(story: &Story, udid: &str) -> Result<PhoneJourney, String> {
        let root = root();
        let declared = story
            .phone_topology
            .as_deref()
            .or(story.topology.as_deref())
            .ok_or("the story names no topology")?;
        PhoneJourney::serve(
            Setup {
                topology: root.join(declared),
                output: root.join("target/journeys/phone").join(&story.id),
                goldens: root.join("journeys/goldens/phone").join(&story.id),
                app: debug_app(),
            },
            &Target::Simulator(udid.to_owned()),
        )
    }

    pub(crate) fn serve(setup: Setup, target: &Target) -> Result<PhoneJourney, String> {
        let root = root();
        let topology_path = setup.topology;
        let named = |error: &dyn std::fmt::Display| format!("{}: {error}", topology_path.display());
        let topology: Value = serde_json::from_str(
            &std::fs::read_to_string(&topology_path).map_err(|error| named(&error))?,
        )
        .map_err(|error| named(&error))?;
        let output = setup.output;
        let _ = std::fs::remove_dir_all(&output);
        std::fs::create_dir_all(&output).map_err(|error| error.to_string())?;
        let lock = File::create(SCRATCH_LOCK).map_err(|error| error.to_string())?;
        lock.lock().map_err(|error| error.to_string())?;
        let scratch = Path::new(SCRATCH_DIR);
        let _ = std::fs::remove_dir_all(scratch);
        std::fs::create_dir_all(scratch).map_err(|error| error.to_string())?;
        let binaries = Binaries::for_driver()?;
        let testnet = std::env::current_exe().map_err(|error| error.to_string())?;
        let mut command = Command::new(&testnet);
        command
            .arg("serve")
            .arg(&topology_path)
            .arg("--root-in")
            .arg(scratch)
            .current_dir(&root)
            .env_remove("AMUX_LOG")
            .env_remove("AMUX_CONFIG")
            .env("TMPDIR", scratch)
            .env("TMP", scratch)
            .env("TEMP", scratch)
            .env("AMUX_TEST_DISCOVERY_MODE", "disabled")
            .env("RUST_LOG", rust_log())
            .env(crate::PREBUILT_ENV, binaries.dir())
            .stdout(Stdio::piped())
            .stderr(File::create(output.join("testnet.log")).map_err(|error| error.to_string())?);
        if let Target::Phone(phone) = target {
            // What the phone dials (the machines' gates, the relay's, the
            // account service) listens on the Mac's side of its USB link.
            command
                .arg("--client-ip")
                .arg(phone.mac_address.to_string());
        }
        let mut process = command
            .spawn()
            .map_err(|error| format!("testnet serve: {error}"))?;
        let stdout = process.stdout.take().expect("piped");
        let (sent, readiness) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            let _ = sent.send(line);
        });
        let ready = match readiness.recv_timeout(Duration::from_secs(120)) {
            Ok(line) if !line.is_empty() => serde_json::from_str::<Readiness>(&line)
                .map_err(|error| format!("readiness: {error}")),
            Ok(_) => Err("testnet exited before readiness".to_owned()),
            Err(_) => Err("testnet readiness timed out".to_owned()),
        };
        let ready = match ready {
            Ok(ready) => ready,
            Err(error) => {
                let _ = process.kill();
                let _ = process.wait();
                return Err(error);
            }
        };
        undo_on_terminate();
        if let Some(undo) = UNDO.get()
            && let Ok(mut held) = undo.lock()
        {
            *held = Some((target.clone(), ready.control));
        }
        let mut record = Record::default();
        record
            .actions
            .push(format!("ready control={}", ready.control));
        let (udid, phone) = match target {
            Target::Simulator(udid) => (udid.clone(), None),
            Target::Phone(phone) => (phone.udid.clone(), Some(phone.clone())),
        };
        Ok(PhoneJourney {
            udid,
            phone,
            goldens: setup.goldens,
            update: std::env::var("UPDATE_JOURNEY_GOLDENS").is_ok_and(|value| value == "1"),
            flat: true,
            build: setup.app,
            output,
            topology,
            binaries,
            _lock: lock,
            process: Some(process),
            ready,
            record,
            port: 0,
            pid: 0,
            through: None,
        })
    }

    // --- the served net --------------------------------------------------

    pub(crate) fn request(
        &mut self,
        request: Control,
        label: Option<&str>,
    ) -> Result<Value, String> {
        self.record.actions.push(format!(
            "net {}",
            serde_json::to_string(&request).unwrap_or_default()
        ));
        let reply = door(self.ready.control, &request, Duration::from_secs(90))?;
        if let Some(label) = label {
            self.record
                .observations
                .insert(label.to_owned(), reply.clone());
        }
        Ok(reply)
    }

    pub(crate) fn chat(&mut self, host: &str, agent: &str, label: &str) -> Result<Value, String> {
        self.request(
            Control::Chat {
                host: host.to_owned(),
                agent: agent.to_owned(),
            },
            Some(label),
        )
    }

    pub(crate) fn wait_chat(
        &mut self,
        host: &str,
        agent: &str,
        ready: impl Fn(&Value) -> bool,
        description: &str,
    ) -> Result<Value, String> {
        let control = self.ready.control;
        let request = Control::Chat {
            host: host.to_owned(),
            agent: agent.to_owned(),
        };
        match super::poll(
            Duration::from_secs(60),
            Duration::from_millis(200),
            || door(control, &request, Duration::from_secs(90)),
            |chat| ready(chat),
        ) {
            Ok(chat) => {
                self.record
                    .observations
                    .insert(description.to_owned(), chat.clone());
                self.record
                    .actions
                    .push(format!("observed at {host}: {description}"));
                Ok(chat)
            }
            Err((last, why)) => Err(format!(
                "timed out waiting for {description} ({why}); {host} holds {}",
                last.unwrap_or_default()
            )),
        }
    }

    pub(crate) fn wait_inventory(
        &mut self,
        host: &str,
        ready: impl Fn(&[Value]) -> bool,
        description: &str,
    ) -> Result<Vec<Value>, String> {
        let control = self.ready.control;
        let request = Control::Inventory {
            host: host.to_owned(),
        };
        match super::poll(
            Duration::from_secs(60),
            Duration::from_millis(200),
            || {
                door(control, &request, Duration::from_secs(90))
                    .map(|reply| reply["agents"].as_array().cloned().unwrap_or_default())
            },
            |agents| ready(agents),
        ) {
            Ok(agents) => {
                self.record
                    .observations
                    .insert(description.to_owned(), Value::Array(agents.clone()));
                self.record
                    .actions
                    .push(format!("observed at {host}: {description}"));
                Ok(agents)
            }
            Err((last, why)) => Err(format!(
                "timed out waiting for {description} ({why}); {host} lists {}",
                Value::Array(last.unwrap_or_default())
            )),
        }
    }

    pub(crate) fn provider_input(
        &mut self,
        agent: &str,
        label: &str,
    ) -> Result<Vec<String>, String> {
        let reply = self.request(
            Control::ProviderInput {
                agent: agent.to_owned(),
            },
            Some(label),
        )?;
        Ok(reply["lines"]
            .as_array()
            .map(|lines| {
                lines
                    .iter()
                    .filter_map(|line| line.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub(crate) fn host_id(&self, host: &str) -> String {
        self.ready
            .hosts
            .iter()
            .find(|ready| ready.name == host)
            .map(|ready| ready.host_id.to_string())
            .unwrap_or_default()
    }

    /// The id of the agent the topology declared as `name`.
    pub(crate) fn agent_id(&self, name: &str) -> String {
        self.ready
            .agents
            .iter()
            .find(|agent| agent.name == name)
            .map(|agent| agent.id.to_string())
            .unwrap_or_default()
    }

    fn config(&self, host: &str) -> PathBuf {
        self.ready
            .hosts
            .iter()
            .find(|ready| ready.name == host)
            .map(|ready| ready.config.clone())
            .unwrap_or_default()
    }

    /// `amux` on a served machine, as a person at that machine types it;
    /// what it prints, line by line.
    pub(crate) fn amux(&mut self, host: &str, args: &[&str]) -> Result<Printing, String> {
        self.record
            .actions
            .push(format!("at {host}: amux {}", args.join(" ")));
        let scratch = Path::new(SCRATCH_DIR);
        let mut child = Command::new(self.binaries.amux())
            .arg("--config")
            .arg(self.config(host))
            .args(args)
            .env_remove("AMUX_LOG")
            .env_remove("AMUX_CONFIG")
            .env("TMPDIR", scratch)
            .env("TMP", scratch)
            .env("TEMP", scratch)
            .env("AMUX_TEST_DISCOVERY_MODE", "disabled")
            .env("RUST_LOG", rust_log())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("amux: {error}"))?;
        let (sent, lines) = mpsc::channel();
        for pipe in [
            child
                .stdout
                .take()
                .map(|pipe| Box::new(pipe) as Box<dyn std::io::Read + Send>),
            child
                .stderr
                .take()
                .map(|pipe| Box::new(pipe) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let sent = sent.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    if sent.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        Ok(Printing { child, lines })
    }

    /// The link `amux pair --qr --print-link` prints on `host`; the process
    /// stays open, as the machine keeps offering it.
    pub(crate) fn pairing_link(&mut self, host: &str) -> Result<(Printing, String), String> {
        let mut process = self.amux(host, &["pair", "--qr", "--print-link"])?;
        match process.find(Duration::from_secs(60), |line| {
            line.strip_prefix("Pairing link: ")
                .map(|link| link.trim().to_owned())
        }) {
            Some(link) => Ok((process, link)),
            None => {
                process.kill();
                Err(format!("{host} printed no pairing link"))
            }
        }
    }

    // --- the phone -------------------------------------------------------

    /// The app, installed fresh so no earlier run's identity or trust is on
    /// the phone, launched against this net.
    pub(crate) fn launch(&mut self, launch: Launch) -> Result<(), String> {
        if let Some(phone) = &self.phone {
            phone.terminate()?;
            phone.install(&self.build)?;
            let mut launch = launch;
            launch
                .extra
                .extend(super::device::FRESH_START.map(str::to_owned));
            return self.relaunch(launch);
        }
        if self.running() {
            simctl(
                &["terminate", &self.udid, BUNDLE_ID],
                Duration::from_secs(60),
            )?;
        }
        let _ = simctl(
            &["uninstall", &self.udid, BUNDLE_ID],
            Duration::from_secs(120),
        );
        simctl(
            &["install", &self.udid, self.build.to_string_lossy().as_ref()],
            Duration::from_secs(300),
        )?;
        self.relaunch(launch)
    }

    /// The same installation launched again, as a person reopens it. Its
    /// browser reports the machines named in `found`, every one of the
    /// net's when nothing is said. Without `geometry` the app reports what
    /// is drawn but not where, which a measurement wants (see `geometry`).
    /// A `relay_quic` address is dialled in the
    /// relay's place, QUIC only. When the topology puts any host behind a
    /// router, on a network of its own, the phone dials the relay through
    /// the net's gate unless told otherwise: the relay then sees the phone
    /// at the gate's socket, which takes datagrams from the relay alone, so
    /// no host punches to it.
    pub(crate) fn relaunch(&mut self, launch: Launch) -> Result<(), String> {
        let mut relay_quic = launch.relay_quic;
        let routed = self.topology["hosts"]
            .as_array()
            .is_some_and(|hosts| hosts.iter().any(|host| truthy(&host["router"])));
        if relay_quic.is_none() && routed {
            let reply = self.request(Control::RelayGate {}, None)?;
            relay_quic = reply["addr"].as_str().map(str::to_owned);
        }
        let scope = self.topology["scope"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let found = launch.found.unwrap_or_else(|| {
            self.ready
                .hosts
                .iter()
                .map(|host| host.host_id.to_string())
                .collect()
        });
        self.port = free_port()?;
        let mut arguments = launch_arguments(
            &self.ready,
            &scope,
            &found,
            self.port,
            launch.geometry,
            relay_quic.as_deref(),
        );
        arguments.extend(launch.extra);
        if let Some(phone) = &self.phone {
            // The phone's link socket dials out from where it is bound, so it
            // binds the phone's address on the USB link, and the door listens
            // there for the Mac.
            let address = SocketAddr::from((phone.address, 0)).to_string();
            if let Some(bind) = arguments
                .iter()
                .position(|argument| argument == "-amux-lan-bind")
            {
                arguments[bind + 1] = address;
            }
            arguments.extend(["-amux-door-bind".to_owned(), phone.address.to_string()]);
            let log = rust_log();
            let environment = [("RUST_LOG", log.as_str())];
            let phone = phone.clone();
            match self.through.take() {
                Some(through) => {
                    through(&arguments, &environment)?;
                    self.record.actions.push("launch the app".to_owned());
                    self.await_door()?;
                    self.pid = phone.running()?.unwrap_or_default();
                    return Ok(());
                }
                None => self.pid = phone.launch(&arguments, &environment)?,
            }
            self.record.actions.push("launch the app".to_owned());
            return self.await_door();
        }
        let mut command = Command::new("xcrun");
        command
            .args([
                "simctl",
                "launch",
                "--terminate-running-process",
                &self.udid,
                BUNDLE_ID,
            ])
            .args(&arguments)
            // The phone's runtime logs under the same filter as the served
            // hosts, so one run's two logs can be read side by side.
            .env("SIMCTL_CHILD_RUST_LOG", rust_log());
        let launched = run_within(command, Duration::from_secs(120))?;
        // `simctl launch` prints `<bundle id>: <pid>`.
        self.pid = launched
            .rsplit(':')
            .next()
            .and_then(|pid| pid.trim().parse().ok())
            .unwrap_or(0);
        self.record.actions.push("launch the app".to_owned());
        self.await_door()
    }

    /// The door answering is the launch; what a capture needs settled it
    /// settles itself.
    fn await_door(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if self.app(json!({"kind": "signposts"})).is_ok() {
                break;
            }
            if Instant::now() > deadline {
                return Err("the app's door did not open within a minute".to_owned());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        if self.flat {
            // Motion stays as the run has it: the door's own default, off.
            self.app(json!({"kind": "assist", "motion": false, "transparency": true}))?;
            self.record
                .actions
                .push("reduce transparency on".to_owned());
        }
        Ok(())
    }

    /// The same, launched on the phone by `through` rather than devicectl.
    pub(crate) fn relaunch_through(
        &mut self,
        launch: Launch,
        through: Launcher,
    ) -> Result<(), String> {
        self.through = Some(through);
        let launched = self.relaunch(launch);
        self.through = None;
        launched
    }

    /// The app closed, as a person swipes it away.
    pub(crate) fn quit(&mut self) -> Result<(), String> {
        self.target().terminate()?;
        self.record.actions.push("quit the app".to_owned());
        Ok(())
    }

    fn target(&self) -> Target {
        match &self.phone {
            Some(phone) => Target::Phone(phone.clone()),
            None => Target::Simulator(self.udid.clone()),
        }
    }

    fn running(&self) -> bool {
        simctl(
            &["spawn", &self.udid, "launchctl", "list"],
            Duration::from_secs(60),
        )
        .is_ok_and(|listed| listed.contains(BUNDLE_ID))
    }

    /// One request to the app's door; its reply.
    pub(crate) fn app(&self, request: Value) -> Result<Value, AppError> {
        self.app_within(request, Duration::from_secs(90))
    }

    pub(crate) fn app_within(&self, request: Value, timeout: Duration) -> Result<Value, AppError> {
        ask(self.door(), &request, timeout)
    }

    /// Where the app's door listens, for a request made off this driver's
    /// thread.
    pub(crate) fn door(&self) -> SocketAddr {
        match &self.phone {
            Some(phone) => phone.door(self.port),
            None => SocketAddr::from(([127, 0, 0, 1], self.port)),
        }
    }

    pub(crate) fn query(&self) -> Result<Value, String> {
        Ok(self.app(json!({"kind": "query"}))?["state"].take())
    }

    /// The system photographs the app, as a person's screenshot does.
    pub(crate) fn system_screenshot(&mut self) -> Result<(), String> {
        self.app(json!({"kind": "screenshot"}))?;
        self.record
            .actions
            .push("the system took a screenshot".to_owned());
        Ok(())
    }

    pub(crate) fn elements(&self) -> Result<Drawn, String> {
        Ok(Drawn::of(elements_of(&self.query()?)))
    }

    /// Until what is drawn satisfies `ready`, by element name.
    pub(crate) fn wait(
        &mut self,
        ready: impl Fn(&Drawn) -> bool,
        description: &str,
    ) -> Result<Drawn, String> {
        self.wait_within(ready, description, Duration::from_secs(60))
    }

    pub(crate) fn wait_within(
        &mut self,
        ready: impl Fn(&Drawn) -> bool,
        description: &str,
        timeout: Duration,
    ) -> Result<Drawn, String> {
        let deadline = Instant::now() + timeout;
        let mut last = Drawn::default();
        while Instant::now() < deadline {
            last = self.elements()?;
            if ready(&last) {
                self.record.actions.push(format!("reached {description}"));
                return Ok(last);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let drawn: Vec<String> = last
            .iter()
            .map(|(name, element)| format!("{name} | {} | {}", element["label"], element["value"]))
            .collect();
        Err(format!(
            "timed out waiting for {description}; the phone draws:\n{}",
            drawn.join("\n")
        ))
    }

    pub(crate) fn wait_for(&mut self, names: &[&str]) -> Result<Drawn, String> {
        self.wait(
            |drawn| names.iter().all(|name| drawn.has(name)),
            &format!("{names:?}"),
        )
    }

    /// A tap as a person makes one, as `ctl phone tap` does: on a control
    /// that is there and enabled, and again while the app has not yet drawn
    /// a just-enabled control able to take it.
    pub(crate) fn tap(&mut self, name: &str) -> Result<(), String> {
        let patience = Duration::from_secs(10);
        self.wait_within(
            |drawn| {
                drawn
                    .get(name)
                    .and_then(|element| element.get("enabled"))
                    .is_some_and(|enabled| *enabled != false)
            },
            &format!("{name} there to tap"),
            patience,
        )?;
        let deadline = Instant::now() + patience;
        loop {
            match self.app(json!({"kind": "tap", "identifier": name})) {
                Ok(_) => break,
                Err(AppError::Refused(error))
                    if error.contains(" did not activate") && Instant::now() <= deadline =>
                {
                    self.app(json!({"kind": "settle"}))?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        self.record.actions.push(format!("tap {name}"));
        Ok(())
    }

    /// Whether every identified element reports its frame. A tap needs the
    /// frames; producing them costs a real share of the main thread, so a
    /// measurement launches without them and asks only to tap.
    pub(crate) fn geometry(&mut self, on: bool) -> Result<(), String> {
        self.app(json!({"kind": "geometry", "on": on}))?;
        self.record.actions.push(format!(
            "element geometry {}",
            if on { "on" } else { "off" }
        ));
        Ok(())
    }

    /// The item a person reads as `label` in a menu the app presented, once
    /// the menu is drawn.
    pub(crate) fn choose(&mut self, label: &str) -> Result<(), String> {
        self.wait(
            |drawn| drawn.iter().any(|(_, element)| element["label"] == label),
            &format!("{label:?} presented"),
        )?;
        self.app(json!({"kind": "choose", "label": label}))?;
        self.record.actions.push(format!("choose {label:?}"));
        Ok(())
    }

    /// The tab reading `title` in the system's tab bar, whose tabs carry
    /// their titles and no names of their own.
    pub(crate) fn tab(&mut self, title: &str) -> Result<(), String> {
        self.choose(title)
    }

    /// Settings, as a person reaches them: the account disc on the agent
    /// list, then Settings in its menu. They open as a sheet over the tabs.
    pub(crate) fn settings(&mut self) -> Result<(), String> {
        self.tab("Agents")?;
        self.wait_for(&["home.accounts"])?;
        self.tap("home.accounts")?;
        self.choose("Settings")?;
        self.wait_for(&["you"])?;
        Ok(())
    }

    /// An element's named accessibility action, as VoiceOver offers it.
    pub(crate) fn perform(&mut self, name: &str, action: &str) -> Result<(), String> {
        self.app(json!({"kind": "perform", "identifier": name, "action": action}))?;
        self.record.actions.push(format!("{action} on {name}"));
        Ok(())
    }

    pub(crate) fn type_text(&mut self, name: &str, text: &str) -> Result<(), String> {
        self.app(json!({"kind": "type", "identifier": name, "text": text}))?;
        self.record
            .actions
            .push(format!("type {text:?} into {name}"));
        Ok(())
    }

    pub(crate) fn paste(&mut self, name: &str, text: &str) -> Result<(), String> {
        self.app(json!({"kind": "paste", "identifier": name, "text": text}))?;
        self.record
            .actions
            .push(format!("paste {} lines into {name}", text.lines().count()));
        Ok(())
    }

    /// The link the machine printed, as a scanned code hands it over; the
    /// host that answered.
    pub(crate) fn pair(&mut self, link: &str) -> Result<String, AppError> {
        let reply = self.app(json!({"kind": "pair", "qr": link}))?;
        let host = reply["host"].as_str().unwrap_or_default().to_owned();
        self.record
            .actions
            .push(format!("pair by the link, answered by {host}"));
        Ok(host)
    }

    // --- screens ---------------------------------------------------------

    /// What the phone shows now, compared with its reviewed golden: the
    /// door's element geometry, with `volatile` elements' words left out.
    /// The display is photographed beside it for a person to look at and is
    /// never compared. With updating on, geometry that differs is rewritten
    /// instead; geometry that matches is left alone, so a near match never
    /// churns.
    pub(crate) fn screen(&mut self, label: &str, volatile: &[&str]) -> Result<(), String> {
        self.app(json!({"kind": "settle"}))?;
        let state = self.query()?;
        let screen = state["screen"].as_str();
        // A tab not on screen still reports what it holds, and its bar's
        // items come and go from that report while it is hidden.
        let elements: Vec<Value> = uncovered(elements_of(&state))
            .into_iter()
            .filter(|element| on_screen(identifier(element), screen))
            .collect();
        let actual = self.output.join("actual");
        std::fs::create_dir_all(&actual).map_err(|error| error.to_string())?;
        self.steady_display(&actual.join(format!("{label}.png")))?;
        let named: Vec<&str> = volatile.iter().copied().chain(OWN_IDENTITY).collect();
        let mut ids: BTreeMap<String, String> = self
            .ready
            .hosts
            .iter()
            .map(|host| (host.host_id.to_string(), host.name.clone()))
            .collect();
        ids.extend(
            self.ready
                .agents
                .iter()
                .map(|agent| (agent.id.to_string(), agent.name.clone())),
        );
        let drawn = geometry(&elements, &named, &ids);
        std::fs::write(actual.join(format!("{label}.elements.txt")), &drawn)
            .map_err(|error| error.to_string())?;
        self.record.actions.push(format!("captured {label}"));
        if self.update && std::env::var_os("CI").is_some() {
            return Err("rewriting goldens is refused in CI".to_owned());
        }
        let expected = self.goldens.join(format!("{label}.elements.txt"));
        let differs = if !expected.exists() {
            Some(format!(
                "missing golden {}; review a rewritten one",
                expected.display()
            ))
        } else {
            let approved = std::fs::read_to_string(&expected).map_err(|error| error.to_string())?;
            (!same_geometry(&approved, &drawn)).then(|| {
                format!(
                    "geometry differs: {}\n{}",
                    expected.display(),
                    line_diff(&approved, &drawn)
                )
            })
        };
        match differs {
            Some(differs) if self.update => {
                std::fs::create_dir_all(&self.goldens).map_err(|error| error.to_string())?;
                std::fs::write(&expected, &drawn).map_err(|error| error.to_string())?;
                self.record.actions.push(format!(
                    "rewrote {label}: {}",
                    differs.lines().next().unwrap_or_default()
                ));
                Ok(())
            }
            Some(differs) => Err(differs),
            None => Ok(()),
        }
    }

    /// The display once two photographs a moment apart agree, so a picture
    /// for a person is not caught mid-transition; or, for a screen that never
    /// holds still, whatever the last photograph caught.
    fn steady_display(&self, png: &Path) -> Result<(), String> {
        let mut previous = Vec::new();
        for _ in 0..20 {
            simctl(
                &[
                    "io",
                    &self.udid,
                    "screenshot",
                    "--type=png",
                    png.to_string_lossy().as_ref(),
                ],
                Duration::from_secs(120),
            )?;
            let taken = std::fs::read(png).map_err(|error| error.to_string())?;
            if taken == previous {
                return Ok(());
            }
            previous = taken;
            std::thread::sleep(Duration::from_millis(300));
        }
        Ok(())
    }

    // --- the end ---------------------------------------------------------

    pub(crate) fn finish(&self, proved: &[String]) {
        let lines: String = proved.iter().map(|line| format!("- {line}\n")).collect();
        self.record.write(&self.output, &format!("PASS\n{lines}"));
    }

    pub(crate) fn fail(&mut self, error: &str) {
        // The display as the story stopped, beside what it says it drew.
        let failed = self.output.join("failed.png");
        if self.phone.is_some() {
            self.record
                .actions
                .push("no picture of the failure: the run drove a phone".to_owned());
        } else if let Err(missed) = simctl(
            &[
                "io",
                &self.udid,
                "screenshot",
                "--type=png",
                failed.to_string_lossy().as_ref(),
            ],
            Duration::from_secs(120),
        ) {
            self.record
                .actions
                .push(format!("no picture of the failure: {missed}"));
        }
        self.record
            .write(&self.output, &format!("FAIL\n- {error}\n"));
    }

    pub(crate) fn close(mut self) -> Result<(), String> {
        let _ = self.target().terminate();
        let mut result = Ok(());
        if let Some(mut process) = self.process.take()
            && process.try_wait().ok().flatten().is_none()
        {
            let _ = door(
                self.ready.control,
                &Control::Shutdown,
                Duration::from_secs(10),
            );
            let deadline = Instant::now() + Duration::from_secs(60);
            while process.try_wait().ok().flatten().is_none() {
                if Instant::now() >= deadline {
                    let _ = process.kill();
                    let _ = process.wait();
                    result = Err("testnet did not shut down within a minute".to_owned());
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if let Some(undo) = UNDO.get()
            && let Ok(mut held) = undo.lock()
        {
            *held = None;
        }
        let _ = std::fs::write(
            self.output.join("actions.txt"),
            self.record.actions.join("\n") + "\n",
        );
        let _ = std::fs::remove_dir_all(SCRATCH_DIR);
        result
    }
}

/// How a launch is told to start, beyond the net it is launched against.
#[derive(Default)]
pub(crate) struct Launch {
    pub extra: Vec<String>,
    pub found: Option<Vec<String>>,
    pub geometry: bool,
    pub relay_quic: Option<String>,
}

impl Launch {
    pub(crate) fn plain() -> Launch {
        Launch {
            geometry: true,
            ..Launch::default()
        }
    }
}

/// A command still running on a served machine, and the lines it printed.
pub(crate) struct Printing {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Printing {
    /// The first printed line `matches` reads something from, within
    /// `timeout`; None once the command ends or the time is up.
    pub(crate) fn find<T>(
        &mut self,
        timeout: Duration,
        matches: impl Fn(&str) -> Option<T>,
    ) -> Option<T> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            match self
                .lines
                .recv_timeout(left.min(Duration::from_millis(500)))
            {
                Ok(line) => {
                    if let Some(found) = matches(&line) {
                        return Some(found);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// Waits for the command to end.
    pub(crate) fn wait(mut self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            if self
                .child
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_some()
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                self.kill();
                return Err(format!(
                    "the command did not end within {}s",
                    timeout.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub(crate) fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Any element's label holds `text`.
pub(crate) fn labelled(drawn: &Drawn, text: &str) -> bool {
    drawn.iter().any(|(_, element)| {
        element["label"]
            .as_str()
            .is_some_and(|label| label.contains(text))
    })
}

/// The six digits a machine's `amux pair` printed.
pub(crate) fn printed_pin(process: &mut Printing) -> Result<String, String> {
    process
        .find(Duration::from_secs(60), |line| {
            line.contains("Pairing PIN: ")
                .then(|| pin_in(line))
                .flatten()
        })
        .ok_or_else(|| "the machine printed no pairing PIN".to_owned())
}

fn elements_of(state: &Value) -> &[Value] {
    state["elements"].as_array().map_or(&[], Vec::as_slice)
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}

/// The served machines' and the phone's log filter: the caller's own, else
/// the links' and streams' debug lines.
fn rust_log() -> String {
    std::env::var("RUST_LOG").unwrap_or_else(|_| RUST_LOG.to_owned())
}

/// One request to the app's door at `address` and its reply. The door
/// answers each connection on a thread of its own, so a request that waits
/// (a measurement) does not hold up another made meanwhile.
pub(crate) fn ask(
    address: SocketAddr,
    request: &Value,
    timeout: Duration,
) -> Result<Value, AppError> {
    let unreachable =
        |error: std::io::Error| AppError::Unreachable(format!("the app's door: {error}"));
    let mut stream = TcpStream::connect_timeout(&address, timeout).map_err(unreachable)?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(unreachable)?;
    writeln!(stream, "{request}").map_err(unreachable)?;
    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .map_err(unreachable)?;
    if line.is_empty() {
        return Err(AppError::Refused(format!(
            "the app's door closed on {request}"
        )));
    }
    let reply: Value = serde_json::from_str(&line)
        .map_err(|error| AppError::Unreachable(format!("the app's reply: {error}: {line}")))?;
    if reply["kind"] == "error" {
        return Err(AppError::Refused(format!(
            "the app refused {request}: {}",
            reply["message"].as_str().unwrap_or_default()
        )));
    }
    Ok(reply)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const DESK: &str = "d6e6b93c-114e-4d20-b29a-6fac10bfb78d";
    const AGENT: &str = "082210b9-b56d-4557-b77b-5303f1eca198";

    pub(crate) fn element(identifier: &str, frame: [f64; 4], label: Option<&str>) -> Value {
        json!({
            "identifier": identifier,
            "label": label,
            "value": null,
            "enabled": true,
            "frame": {"x": frame[0], "y": frame[1], "width": frame[2], "height": frame[3]},
        })
    }

    fn ready(relay: bool) -> Readiness {
        let mut ready = json!({
            "control": "127.0.0.1:1", "root": "/net", "gates": "/gates", "hosts": [], "agents": [],
        });
        if relay {
            ready["cloud_url"] = json!("http://127.0.0.1:9");
            ready["relay_tcp"] = json!("127.0.0.1:8");
            ready["relay_quic"] = json!("127.0.0.1:7");
            ready["relay_root"] = json!("3082");
        }
        serde_json::from_value(ready).expect("readiness")
    }

    fn after<'a>(arguments: &'a [String], flag: &str) -> Option<&'a str> {
        let at = arguments.iter().position(|argument| argument == flag)?;
        arguments.get(at + 1).map(String::as_str)
    }

    #[test]
    fn a_measured_launch_leaves_element_geometry_off() {
        let arguments = launch_arguments(
            &ready(false),
            "phone-reach",
            &[DESK.to_owned()],
            4711,
            false,
            None,
        );
        assert!(!arguments.contains(&"-amux-element-geometry".to_owned()));
        assert_eq!(arguments[..2], ["-amux-door-port", "4711"]);
    }

    #[test]
    fn a_launch_names_its_door_scope_found_machines_and_loopback_links_and_puts_the_offer_away() {
        let arguments = launch_arguments(
            &ready(false),
            "phone-reach",
            &[DESK.to_owned()],
            4711,
            true,
            None,
        );
        assert_eq!(
            arguments,
            [
                "-amux-door-port",
                "4711",
                "-amux-element-geometry",
                "-amux-discovery-scope",
                "phone-reach",
                "-amux-discover-only",
                DESK,
                "-amux-lan-bind",
                "127.0.0.1:0",
                "-notificationsOfferDeclined",
                "YES",
            ]
        );
    }

    #[test]
    fn a_net_with_a_relay_hands_the_launch_its_cloud_and_carriers() {
        let arguments = launch_arguments(&ready(true), "", &[], 1, true, None);
        assert!(arguments.contains(&"-amux-scripted-cloud".to_owned()));
        assert_eq!(after(&arguments, "-amux-relay"), Some("http://127.0.0.1:9"));
        assert_eq!(after(&arguments, "-amux-relay-tcp"), Some("127.0.0.1:8"));
        assert_eq!(after(&arguments, "-amux-relay-quic"), Some("127.0.0.1:7"));
        assert_eq!(after(&arguments, "-amux-relay-root"), Some("3082"));
    }

    #[test]
    fn a_gated_relay_is_dialled_by_quic_alone() {
        let arguments = launch_arguments(&ready(true), "", &[], 1, true, Some("127.0.0.1:6"));
        assert_eq!(after(&arguments, "-amux-relay-quic"), Some("127.0.0.1:6"));
        assert!(!arguments.contains(&"-amux-relay-tcp".to_owned()));
    }

    #[test]
    fn a_tab_behind_a_pushed_page_is_not_on_screen() {
        assert!(!on_screen("hosts.fact.identity", Some("chat")));
        assert!(on_screen("chat.row.turn-end", Some("chat")));
        assert!(on_screen("hosts.fact.identity", Some("hosts")));
        assert!(on_screen("hosts.fact.identity", None));
    }

    #[test]
    fn geometry_names_ids_and_drops_volatile_words() {
        let mut row = element(
            &format!("home.row.{AGENT}"),
            [0.0, 100.4, 390.0, 60.0],
            Some("desk-work, Idle, desk, 34s ago"),
        );
        row["value"] = json!("idle");
        let mut send = element("chat.send", [350.0, 700.0, 44.0, 44.0], Some("Send"));
        send["enabled"] = json!(false);
        let elements = [
            row,
            element(
                &format!("home.row.{AGENT}.age.volatile"),
                [340.0, 104.0, 30.0, 14.0],
                None,
            ),
            element(
                "hosts.fact.identity",
                [16.0, 500.0, 358.0, 44.0],
                Some("Identity, amux-iphone-1 · af8c…5013"),
            ),
            send,
            element(
                "home.row.11111111-2222-3333-4444-555555555555",
                [0.0, 0.0, 1.0, 1.0],
                None,
            ),
        ];
        let ids = BTreeMap::from([(AGENT.to_owned(), "desk-work".to_owned())]);
        let drawn = geometry(&elements, &["hosts.fact.identity"], &ids);
        assert_eq!(
            drawn.lines().collect::<Vec<_>>(),
            [
                "home.row.<desk-work> | desk-work, Idle, desk, <age> | idle | 0,100,390,60",
                "home.row.<desk-work>.age.volatile | <volatile>",
                "hosts.fact.identity | <volatile>",
                "chat.send | Send |  | 350,700,44,44 (disabled)",
                "home.row.<id> |  |  | 0,0,1,1",
            ]
        );
    }

    #[test]
    fn scratch_paths_keep_their_width() {
        assert_eq!(
            normalize("/tmp/aj-Ab3dE9_x/testnetC80HLF/desk/work"),
            "/tmp/aj-xxxxxxxx/testnetxxxxxx/desk/work"
        );
    }

    #[test]
    fn keys_and_the_leased_device_name_are_masked() {
        let whole = ["d326", "50ce", "ef5c", "7836"].repeat(4).join(" ");
        assert_eq!(
            normalize(&format!("Fingerprint | {whole}")),
            "Fingerprint | <key>"
        );
        assert_eq!(
            normalize("Identity, amux-iphone-1 · 9ab9…c45d"),
            "Identity, <device> · <key>"
        );
        assert_eq!(normalize("Name, amux-small-2"), "Name, <device>");
        assert_eq!(
            normalize("/tmp/amux-phone-journey/net"),
            "/tmp/amux-phone-journey/net"
        );
    }

    #[test]
    fn masked_words_may_change_their_width() {
        let approved = " | Worked <t> |  | 16,271,69,16\nchat.send | Send |  | 335,779,44,44\n";
        assert!(same_geometry(approved, &approved.replace("69,16", "71,16")));
        assert!(!same_geometry(
            approved,
            &approved.replace("16,271", "16,280")
        ));
        assert!(!same_geometry(
            approved,
            &approved.replace("69,16", "99,16")
        ));
        assert!(!same_geometry(
            approved,
            &approved.replace("44,44", "46,44")
        ));
        assert!(!same_geometry(
            approved,
            &format!("{approved} | Worked <t> |  | 16,271,69,16\n")
        ));
    }

    #[test]
    fn geometry_is_compared_without_its_order() {
        let approved = "home.accounts | Switch account |  | 19,69,30,30\nhome.newAgent | New Agent |  | 338,62,44,44\n";
        let swapped = "home.newAgent | New Agent |  | 338,62,44,44\nhome.accounts | Switch account |  | 19,69,30,30\n";
        let moved = "home.newAgent | New Agent |  | 338,63,44,44\nhome.accounts | Switch account |  | 19,69,30,30\n";
        assert!(same_geometry(approved, swapped));
        assert!(!same_geometry(approved, moved));
    }

    #[test]
    fn durations_and_countdowns_are_masked() {
        assert_eq!(normalize("Worked 1ms · $0.00"), "Worked <t> · $0.00");
        assert_eq!(
            normalize("expires in 4m. 1m 4s, 1.2s"),
            "expires in <t>. <t>, <t>"
        );
        assert_eq!(normalize("5 minutes of work"), "5 minutes of work");
        assert_eq!(normalize("resets 9:41 PM"), "resets <time>");
    }

    #[test]
    fn repeated_names_are_kept_in_drawing_order() {
        let named = Drawn::of(&[
            element("chat.row.prompt", [0.0; 4], Some("one")),
            element("chat.row.prompt", [0.0; 4], Some("two")),
        ]);
        assert_eq!(
            named.names().collect::<Vec<_>>(),
            ["chat.row.prompt", "chat.row.prompt#2"]
        );
        assert_eq!(named.label("chat.row.prompt#2"), "two");
    }

    #[test]
    fn a_pushed_page_hides_the_tab_beneath_it() {
        let elements = [
            json!({"identifier": "home"}),
            json!({"identifier": "home.row.x"}),
            json!({"identifier": "chat"}),
        ];
        let left: Vec<String> = uncovered(&elements)
            .iter()
            .map(|e| identifier(e).to_owned())
            .collect();
        assert_eq!(left, ["chat"]);
        assert!(on_screen("home.row.x", Some("agents")));
        assert!(!on_screen("home.row.x", Some("hosts")));
        assert!(on_screen("chat.field", Some("hosts")));
    }
}
