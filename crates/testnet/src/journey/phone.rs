//! The phone journey driver: starts a served topology with `testnet serve`,
//! installs the debug app fresh on the leased simulator and launches it with
//! its door, the net's discovery scope, a loopback direct-link listener and,
//! when the net has one, the served relay. It then acts only as a person
//! would, through door verbs a person's action maps to: tap, type, paste,
//! pair by the link or code the machine printed, and appearance. It judges
//! by what the hosts recorded (the served net's Chat, Inventory and
//! ProviderInput verbs) and by compared screens: the simulator's display PNG
//! against `journeys/goldens/phone/<story>/<label>.png` with `xtask golden
//! diff` under the pinned simulator's system-chrome masks, and the door's
//! element geometry against `<label>.elements.txt`. Stories supply the acts
//! and the assertions; `UPDATE_JOURNEY_GOLDENS=1` rewrites the goldens that
//! differ.

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

use super::{Record, Story, door, line_diff, root};
use crate::door::{Control, Readiness};

pub(crate) const BUNDLE_ID: &str = "sh.amux.app";
/// The pinned simulator whose system-chrome masks every comparison uses.
const SIMULATOR: &str = "golden";
/// The golden simulator draws three pixels to the point.
const SCALE: f64 = 3.0;
/// A surface a view reports under a name ending here holds text that moves
/// with the run (an age, a scratch path); its rectangle is masked in the
/// pixel diff and its words are left out of the geometry.
const VOLATILE: &str = ".volatile";
/// This phone's own name and key, which change with the leased simulator
/// and every fresh install.
const OWN_IDENTITY: [&str; 2] = ["you.name", "you.fingerprint"];
/// One fixed place, so every path the phone draws (an agent's working
/// directory, an ask's scope) is the same on every run and every machine.
/// Short: daemons bind Unix sockets under it. Journeys in other checkouts
/// wait for it on the lock.
const SCRATCH_DIR: &str = "/tmp/amux-phone-journey";
const SCRATCH_LOCK: &str = "/tmp/amux-phone-journey.lock";
/// How far either side of a volatile surface its mask reaches, at least:
/// its words are as wide as they are in this run, and an age that read
/// "39s" when the golden was taken is wider than one reading "1m" now. The
/// reach grows with the line's height, as a character's width does with
/// the text size.
const VOLATILE_SLACK: f64 = 8.0;
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

/// Pixel rectangles, as `xtask golden diff --mask` reads them, of every
/// surface a view declared volatile and every element a story named so (a
/// key fingerprint made fresh each run, a countdown), on the page the door
/// says is on screen.
fn volatile_masks(elements: &[Value], named: &[&str], screen: Option<&str>) -> Vec<String> {
    elements
        .iter()
        .filter(|element| {
            let name = identifier(element);
            is_volatile(name, named) && on_screen(name, screen)
        })
        .map(|element| {
            let frame = &element["frame"];
            let (x, y) = (number(&frame["x"]), number(&frame["y"]));
            let (width, height) = (number(&frame["width"]), number(&frame["height"]));
            let slack = VOLATILE_SLACK.max(height);
            let left = (((x - slack) * SCALE) as i64).max(0);
            let top = ((y * SCALE) as i64).max(0);
            let wide = ((x + width + slack) * SCALE + 0.999) as i64 - left;
            let tall = ((y + height) * SCALE + 0.999) as i64 - top;
            format!("{left},{top},{wide},{tall}")
        })
        .collect()
}

/// The volatile surfaces of a screen the app froze, where the report draws
/// its picture of that screen (`report.frame`, the whole display shrunk to
/// fit). They are pixels in a picture there, not elements, so the picture
/// of an age moves with the run as the age itself does.
fn pictured(elements: &[Value], frozen: &[Value], display: (u32, u32)) -> Vec<Value> {
    let Some(frame) = elements
        .iter()
        .find(|element| identifier(element) == "report.frame")
        .map(|element| &element["frame"])
    else {
        return Vec::new();
    };
    let shrink = number(&frame["width"]) / (f64::from(display.0) / SCALE);
    frozen
        .iter()
        .map(|element| {
            let inner = &element["frame"];
            json!({
                "identifier": format!("report.frame:{}{VOLATILE}", identifier(element)),
                "frame": {
                    "x": number(&frame["x"]) + number(&inner["x"]) * shrink,
                    "y": number(&frame["y"]) + number(&inner["y"]) * shrink,
                    "width": number(&inner["width"]) * shrink,
                    "height": number(&inner["height"]) * shrink,
                },
            })
        })
        .collect()
}

fn png_size(png: &Path) -> Result<(u32, u32), String> {
    let bytes = std::fs::read(png).map_err(|error| format!("{}: {error}", png.display()))?;
    let header = bytes.get(16..24).ok_or("not a PNG")?;
    let read = |at: usize| {
        u32::from_be_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
    };
    Ok((read(0), read(4)))
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
fn run_within(mut command: Command, timeout: Duration) -> Result<String, String> {
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
    let mut command = Command::new(root().join("scripts/py"));
    command
        .args(["-B", "scripts/ios_simulators.py", SIMULATOR])
        .current_dir(root());
    let said = run_within(command, Duration::from_secs(1800))?;
    // `golden (amux-iphone-1): <udid> (iOS 26.5, booted, ...)`
    said.lines()
        .find_map(|line| {
            let rest = line.strip_prefix(&format!("{SIMULATOR} ("))?;
            let (_, rest) = rest.split_once("): ")?;
            rest.split_whitespace().next().map(str::to_owned)
        })
        .ok_or_else(|| format!("the simulator pool named no {SIMULATOR} device: {said}"))
}

/// The debug app built for the simulator.
fn debug_app() -> PathBuf {
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
static UNDO: OnceLock<std::sync::Mutex<Option<(String, SocketAddr)>>> = OnceLock::new();

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
            if let Some((udid, control)) = undo.lock().ok().and_then(|held| held.clone()) {
                let _ = simctl(&["terminate", &udid, BUNDLE_ID], Duration::from_secs(30));
                let _ = door(control, &Control::Shutdown, Duration::from_secs(10));
            }
            std::process::exit(143);
        });
    });
}

pub(crate) struct PhoneJourney {
    pub udid: String,
    pub output: PathBuf,
    /// Where this run's screens are compared, and whether a screen that no
    /// longer matches is rewritten there instead.
    goldens: PathBuf,
    update: bool,
    /// Whether every launch turns the app's reduce-transparency setting on,
    /// so each frosted surface is drawn flat. The render server finishes
    /// glass after the app has drawn, on its own schedule, and a photograph
    /// at any fixed moment shows one stage or another of that work; drawn
    /// flat, the same screens repeat pixel for pixel.
    flat: bool,
    topology: Value,
    _lock: File,
    process: Option<Child>,
    pub ready: Readiness,
    pub record: Record,
    /// The volatile surfaces on screen when the system last photographed
    /// the app, masked again wherever a report shows that photograph.
    frozen: Vec<Value>,
    port: u16,
}

impl PhoneJourney {
    pub(crate) fn start(story: &Story, udid: &str) -> Result<PhoneJourney, String> {
        let root = root();
        let declared = story
            .phone_topology
            .as_deref()
            .or(story.topology.as_deref())
            .ok_or("the story names no topology")?;
        let topology_path = root.join(declared);
        let topology: Value = serde_json::from_str(
            &std::fs::read_to_string(&topology_path)
                .map_err(|error| format!("{declared}: {error}"))?,
        )
        .map_err(|error| format!("{declared}: {error}"))?;
        let output = root.join("target/journeys/phone").join(&story.id);
        let _ = std::fs::remove_dir_all(&output);
        std::fs::create_dir_all(&output).map_err(|error| error.to_string())?;
        let lock = File::create(SCRATCH_LOCK).map_err(|error| error.to_string())?;
        lock.lock().map_err(|error| error.to_string())?;
        let scratch = Path::new(SCRATCH_DIR);
        let _ = std::fs::remove_dir_all(scratch);
        std::fs::create_dir_all(scratch).map_err(|error| error.to_string())?;
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
            .stdout(Stdio::piped())
            .stderr(File::create(output.join("testnet.log")).map_err(|error| error.to_string())?);
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
            *held = Some((udid.to_owned(), ready.control));
        }
        let mut record = Record::default();
        record
            .actions
            .push(format!("ready control={}", ready.control));
        Ok(PhoneJourney {
            udid: udid.to_owned(),
            goldens: root.join("journeys/goldens/phone").join(&story.id),
            update: std::env::var("UPDATE_JOURNEY_GOLDENS").is_ok_and(|value| value == "1"),
            flat: true,
            output,
            topology,
            _lock: lock,
            process: Some(process),
            ready,
            record,
            frozen: Vec::new(),
            port: 0,
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
        let mut child = Command::new(
            std::env::current_exe()
                .map_err(|error| error.to_string())?
                .with_file_name("amux"),
        )
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

    /// The debug app, installed fresh so no earlier run's identity or trust
    /// is on the phone, launched against this net.
    pub(crate) fn launch(&mut self, launch: Launch) -> Result<(), String> {
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
            &[
                "install",
                &self.udid,
                debug_app().to_string_lossy().as_ref(),
            ],
            Duration::from_secs(300),
        )?;
        self.relaunch(launch)
    }

    /// The same installation launched again, as a person reopens it. Its
    /// browser reports the machines named in `found`, every one of the
    /// net's when nothing is said. Without `geometry` the app reports what
    /// is drawn but not where. A `relay_quic` address is dialled in the
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
        run_within(command, Duration::from_secs(120))?;
        self.record.actions.push("launch the app".to_owned());
        // The door answering is the launch; what a capture needs settled it
        // settles itself.
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

    /// The app closed, as a person swipes it away.
    pub(crate) fn quit(&mut self) -> Result<(), String> {
        simctl(
            &["terminate", &self.udid, BUNDLE_ID],
            Duration::from_secs(60),
        )?;
        self.record.actions.push("quit the app".to_owned());
        Ok(())
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
        let unreachable =
            |error: std::io::Error| AppError::Unreachable(format!("the app's door: {error}"));
        let address = SocketAddr::from(([127, 0, 0, 1], self.port));
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

    pub(crate) fn query(&self) -> Result<Value, String> {
        Ok(self.app(json!({"kind": "query"}))?["state"].take())
    }

    /// The system photographs the app, as a person's screenshot does. What
    /// on screen moves with the run is remembered, for a report that shows
    /// the photograph.
    pub(crate) fn system_screenshot(&mut self) -> Result<(), String> {
        let state = self.query()?;
        let screen = state["screen"].as_str();
        self.frozen = uncovered(elements_of(&state))
            .into_iter()
            .filter(|element| {
                let name = identifier(element);
                is_volatile(name, &OWN_IDENTITY) && on_screen(name, screen)
            })
            .collect();
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
    /// display's pixels and the door's element geometry, with `volatile`
    /// elements masked.
    pub(crate) fn screen(&mut self, label: &str, volatile: &[&str]) -> Result<(), String> {
        match self.compare(label, volatile, None)? {
            Some(differs) => Err(differs),
            None => Ok(()),
        }
    }

    /// What differs between the phone now and the golden `label`, or None
    /// when nothing does. The element geometry is read from `geometry_label`
    /// when several pictures share one layout (the same screen in light and
    /// dark). With updating on, a golden that differs is rewritten and
    /// nothing is reported; one that matches is left alone, so a masked
    /// region never churns.
    pub(crate) fn compare(
        &mut self,
        label: &str,
        volatile: &[&str],
        geometry_label: Option<&str>,
    ) -> Result<Option<String>, String> {
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
        let png = actual.join(format!("{label}.png"));
        self.steady_display(&png)?;
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
        let geometry_label = geometry_label.unwrap_or(label);
        std::fs::write(
            actual.join(format!("{geometry_label}.elements.txt")),
            &drawn,
        )
        .map_err(|error| error.to_string())?;
        // A volatile surface is masked where it stood before the photograph
        // and after it: an age crossing into another digit while the display
        // settles (9s to 19s) grows past the frame read first.
        let after = self.query()?;
        let mut masks = volatile_masks(&elements, &named, screen);
        masks.extend(volatile_masks(
            &uncovered(elements_of(&after)),
            &named,
            after["screen"].as_str(),
        ));
        masks.extend(volatile_masks(
            &pictured(&elements, &self.frozen, png_size(&png)?),
            &named,
            None,
        ));
        self.record.actions.push(format!("captured {label}"));
        if self.update && std::env::var_os("CI").is_some() {
            return Err("rewriting goldens is refused in CI".to_owned());
        }
        let golden_png = self.goldens.join(format!("{label}.png"));
        let expected = self.goldens.join(format!("{geometry_label}.elements.txt"));
        let differs = self.differs(&png, &golden_png, &expected, &drawn, &masks, label)?;
        match differs {
            Some(differs) if self.update => {
                std::fs::create_dir_all(&self.goldens).map_err(|error| error.to_string())?;
                std::fs::copy(&png, &golden_png).map_err(|error| error.to_string())?;
                std::fs::write(&expected, &drawn).map_err(|error| error.to_string())?;
                self.record.actions.push(format!(
                    "rewrote {label}: {}",
                    differs.lines().next().unwrap_or_default()
                ));
                Ok(None)
            }
            differs => Ok(differs),
        }
    }

    fn differs(
        &mut self,
        png: &Path,
        golden_png: &Path,
        expected: &Path,
        drawn: &str,
        masks: &[String],
        label: &str,
    ) -> Result<Option<String>, String> {
        if !expected.exists() || !golden_png.exists() {
            return Ok(Some(format!(
                "missing golden {}; review a rewritten one",
                golden_png.display()
            )));
        }
        let approved = std::fs::read_to_string(expected).map_err(|error| error.to_string())?;
        if !same_geometry(&approved, drawn) {
            return Ok(Some(format!(
                "geometry differs: {}\n{}",
                expected.display(),
                line_diff(&approved, drawn)
            )));
        }
        let out = self.output.join("diff").join(label);
        let xtask = std::env::current_exe()
            .map_err(|error| error.to_string())?
            .with_file_name("xtask");
        let mut command = Command::new(xtask);
        command
            .args(["golden", "diff", "--expected"])
            .arg(golden_png)
            .arg("--actual")
            .arg(png)
            .arg("--out")
            .arg(&out)
            .args(["--simulator", SIMULATOR])
            .args(masks.iter().flat_map(|mask| ["--mask", mask.as_str()]))
            .current_dir(root());
        match run_within(command, Duration::from_secs(600)) {
            Err(said) => Ok(Some(format!(
                "screen {label} differs: {said}; expected, actual and diff are under {}",
                out.display()
            ))),
            Ok(verdict) => {
                // A match that spent some of the comparison's allowance for
                // stray pixels says how much, so the allowance can be judged
                // from the runs.
                let verdict = verdict.trim();
                if verdict != "same" {
                    println!("AMUX_PICTURE_STRAYS screen={label} {verdict}");
                    self.record
                        .actions
                        .push(format!("compared {label}: {verdict}"));
                }
                Ok(None)
            }
        }
    }

    /// The display once two photographs a moment apart agree: a transition
    /// or the glass settling behind it is over.
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
        Err(format!(
            "the display never held still for {}",
            png.file_stem().unwrap_or_default().to_string_lossy()
        ))
    }

    // --- the end ---------------------------------------------------------

    pub(crate) fn finish(&self, proved: &[String]) {
        let lines: String = proved.iter().map(|line| format!("- {line}\n")).collect();
        self.record.write(&self.output, &format!("PASS\n{lines}"));
    }

    pub(crate) fn fail(&mut self, error: &str) {
        // The display as the story stopped, beside what it says it drew.
        let failed = self.output.join("failed.png");
        if let Err(missed) = simctl(
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
        let _ = simctl(
            &["terminate", &self.udid, BUNDLE_ID],
            Duration::from_secs(60),
        );
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
    fn a_launch_names_its_door_scope_found_machines_and_loopback_links() {
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
    fn volatile_surfaces_and_named_elements_are_masked_in_pixels() {
        let elements = [
            element(
                &format!("home.row.{AGENT}"),
                [0.0, 100.0, 390.0, 60.0],
                None,
            ),
            element(
                &format!("home.row.{AGENT}.age.volatile"),
                [340.2, 104.0, 30.5, 14.0],
                None,
            ),
            element("pair-confirm.fingerprint", [16.0, 300.0, 358.0, 40.0], None),
        ];
        assert_eq!(
            volatile_masks(&elements, &["pair-confirm.fingerprint"], None),
            ["978,312,177,42", "0,900,1242,120"]
        );
        assert_eq!(volatile_masks(&elements, &[], None), ["978,312,177,42"]);
    }

    #[test]
    fn a_larger_text_size_masks_further_either_side() {
        // "1m" now, where the golden's "39s" reached about 16 points further left.
        let large = [element(
            &format!("home.row.{AGENT}.age.volatile"),
            [330.0, 104.0, 30.0, 36.0],
            None,
        )];
        assert_eq!(volatile_masks(&large, &[], None), ["882,312,306,108"]);
    }

    #[test]
    fn a_tab_behind_a_pushed_page_masks_nothing() {
        let elements = [
            element("hosts.fact.identity", [0.0, 10.0, 1.0, 1.0], None),
            element("chat.row.turn-end", [0.0, 20.0, 1.0, 1.0], None),
        ];
        let named = ["hosts.fact.identity", "chat.row.turn-end"];
        assert_eq!(
            volatile_masks(&elements, &named, Some("chat")),
            ["0,60,27,3"]
        );
        assert_eq!(volatile_masks(&elements, &named, Some("hosts")).len(), 2);
    }

    #[test]
    fn a_reported_photograph_masks_what_moved_on_the_frozen_screen() {
        let frozen = [element(
            &format!("home.row.{AGENT}.age.volatile"),
            [300.0, 160.0, 40.0, 20.0],
            None,
        )];
        // The whole 400-point display, drawn at half size from (100, 50).
        let drawn = [element("report.frame", [100.0, 50.0, 200.0, 437.0], None)];
        assert_eq!(
            volatile_masks(&pictured(&drawn, &frozen, (1200, 2622)), &[], None),
            ["720,390,120,30"]
        );
        assert!(pictured(&[], &frozen, (1200, 2622)).is_empty());
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
