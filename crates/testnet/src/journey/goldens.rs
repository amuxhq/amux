//! `testnet journey goldens`: the phone's whole screens photographed through
//! the served door and compared with their goldens, in light and in dark; and
//! `testnet journey accessibility`, the same way through the app with every
//! page audited for names and hit areas instead.
//!
//! The screens are the `screens` half of apps/apple/Goldens/manifest.json.
//! Each is reached the way a person reaches it, on the phone driver: the
//! manifest's topology is served, the debug app is installed fresh on the
//! leased simulator, the desk is paired by the code it printed, and the app
//! is tapped to each screen while the served net makes the desk's agents do
//! what the screen needs. A screen is judged by the display's pixels and the
//! door's element geometry against apps/apple/Goldens/<screen>.<appearance>.png
//! and <screen>.elements.txt (both appearances share one geometry). A screen
//! with frames (origin-rewind's before and after) has a golden per frame.
//!
//! Every screen is drawn with the app's reduce-transparency flag on, so each
//! frosted surface is flat: a raised fill with a hairline rim, what a person
//! who turned Reduce Transparency on sees. Liquid Glass and material are
//! finished by the render server on its own schedule, out of the app's
//! sight, so a photograph of them is not a fixed picture a tight comparison
//! can hold.
//!
//! The components half of the manifest is photographed in-process by
//! `just ios component-snapshots`; this reads only its screens.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::{Value, json};

use super::phone::{
    Drawn, Launch, PhoneJourney, Setup, Target, debug_app, golden_udid, labelled, printed_pin,
    simctl,
};
use super::{holds, phase, root};
use crate::door::Control;

const MANIFEST: &str = "apps/apple/Goldens/manifest.json";
const GOLDENS: &str = "apps/apple/Goldens";
const APPEARANCES: [&str; 2] = ["light", "dark"];
/// Longer than the render server has been seen to take to finish glass
/// after it first draws (under 2 s), for the review captures.
const GLASS_FINISHES: Duration = Duration::from_millis(2500);
/// A turn's duration and a pairing code's fingerprint and countdown are made
/// fresh by every run.
const TURN: &[&str] = &["chat.row.turn-end"];
const PAIRING: &[&str] = &["pair-confirm.fingerprint", "pair-confirm.expiry"];

#[derive(Deserialize)]
struct Manifest {
    topology: String,
    screens: Vec<Screen>,
}

#[derive(Deserialize)]
struct Screen {
    id: String,
    #[serde(default)]
    frames: Vec<Frame>,
}

#[derive(Deserialize)]
struct Frame {
    name: String,
}

fn manifest() -> Result<Manifest, String> {
    let path = root().join(MANIFEST);
    let text =
        std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
}

type Reach = fn(&mut Goldens) -> Result<(), String>;

/// Every screen the manifest may declare, in the order the way through them
/// passes it. origin-rewind comes last: it takes the desk's power.
const WAY: &[(&str, Option<Reach>)] = &[
    ("pairing", None),
    ("hosts", Some(Goldens::hosts)),
    ("fleet", Some(Goldens::fleet)),
    ("chat-turn", Some(Goldens::chat_turn)),
    ("claude-sdk-ask-question", Some(Goldens::question)),
    ("ask-escape", Some(Goldens::escape)),
    ("composer-chips", Some(Goldens::composer_chips)),
    ("large-text", Some(Goldens::large_text)),
    ("origin-rewind", Some(Goldens::origin_rewind)),
];

/// The name a screen's photographs go by: the screen's own, or
/// `<screen>-<frame>` for one of the frames the manifest declares for it.
fn label(
    frames: &BTreeMap<String, Vec<String>>,
    screen: &str,
    frame: Option<&str>,
) -> Result<String, String> {
    let declared = frames.get(screen).map_or(&[][..], Vec::as_slice);
    match frame {
        None if declared.is_empty() => Ok(screen.to_owned()),
        Some(frame) if declared.iter().any(|name| name == frame) => Ok(format!("{screen}-{frame}")),
        _ => Err(format!(
            "the manifest does not declare {screen} with frame {frame:?}"
        )),
    }
}

/// What a run does where the way through the app reaches a screen.
enum Camera {
    /// Each appearance compared with its golden, or rewritten with updating on.
    Compare,
    /// Each appearance written to the directory, glass and all, for a
    /// person to look at; nothing compared.
    Review(PathBuf),
    /// The page on screen audited by the running accessibility test.
    Audit(Audit),
}

struct Goldens {
    journey: PhoneJourney,
    frames: BTreeMap<String, Vec<String>>,
    wanted: BTreeSet<String>,
    camera: Camera,
    outcomes: Vec<(String, Option<String>)>,
}

impl Goldens {
    fn new(
        journey: PhoneJourney,
        manifest: &Manifest,
        wanted: BTreeSet<String>,
        camera: Camera,
    ) -> Goldens {
        Goldens {
            journey,
            frames: manifest
                .screens
                .iter()
                .map(|screen| {
                    let frames = screen.frames.iter().map(|frame| frame.name.clone());
                    (screen.id.clone(), frames.collect())
                })
                .collect(),
            wanted,
            camera,
            outcomes: Vec::new(),
        }
    }

    fn agent(&self, name: &str) -> String {
        self.journey.agent_id(name)
    }

    fn settle(&self) -> Result<(), String> {
        self.journey.app(json!({"kind": "settle"}))?;
        Ok(())
    }

    /// The screen as it is now, in every appearance, against its golden.
    fn photograph(
        &mut self,
        screen: &str,
        frame: Option<&str>,
        volatile: &[&str],
    ) -> Result<(), String> {
        if let Camera::Audit(_) = self.camera {
            return self.audit(screen);
        }
        if !self.wanted.contains(screen) {
            return Ok(());
        }
        let label = label(&self.frames, screen, frame)?;
        for appearance in APPEARANCES {
            self.journey.appearance(appearance)?;
            let name = format!("{label}.{appearance}");
            let differs = match &self.camera {
                Camera::Review(directory) => {
                    self.settle()?;
                    std::thread::sleep(GLASS_FINISHES);
                    self.journey
                        .steady_display(&directory.join(format!("{name}.png")))?;
                    println!("wrote {name}");
                    None
                }
                _ => {
                    let differs = self.journey.compare(&name, volatile, Some(&label))?;
                    let verdict = if differs.is_none() { "ok" } else { "DIFFERS" };
                    println!("{verdict} {name}");
                    differs
                }
            };
            self.outcomes.push((name, differs));
        }
        self.journey.appearance("light")
    }

    // --- the way through the screens -------------------------------------

    /// The desk's agents act one turn at a time, so the fleet's order (most
    /// recently active first, among agents needing the same attention) is
    /// the same on every run.
    fn prepare_the_desk(&mut self) -> Result<(), String> {
        for (agent, prompt, reply) in [
            (
                "planner",
                "Why do the pairing errors disagree?",
                "share one string",
            ),
            ("keeper", "Remember this recovery turn.", "safely stored"),
        ] {
            self.journey.request(
                Control::Send {
                    agent: agent.to_owned(),
                    text: prompt.to_owned(),
                },
                None,
            )?;
            self.journey.wait_chat(
                "desk",
                agent,
                |chat| phase(chat) == "IDLE" && holds(chat, reply),
                &format!("{agent}-answered"),
            )?;
        }
        // The asker was started with its prompt, so it is asking already and
        // the fleet names what it was asked to do.
        self.journey.wait_chat(
            "desk",
            "asker",
            |chat| phase(chat) == "NEEDS_YOU",
            "asker-asking",
        )?;
        Ok(())
    }

    /// The six digits the desk printed, typed on the keypad, reach the desk's
    /// own name and key before anything is trusted.
    fn pairing(&mut self) -> Result<(), String> {
        let desk = self.journey.host_id("desk");
        self.journey.tab("Hosts")?;
        self.journey.wait_for(&[&format!("hosts.pair.{desk}")])?;
        let mut process = self.journey.amux("desk", &["pair"])?;
        let pin = printed_pin(&mut process)?;
        self.journey.tap(&format!("hosts.pair.{desk}"))?;
        self.journey.wait_for(&["pin.code"])?;
        self.journey.type_text("pin.code", &pin)?;
        self.journey.wait(
            |drawn| drawn.has("pair-confirm.trust") && drawn.value("pair-confirm.name") == "desk",
            "the desk's confirmation",
        )?;
        self.photograph("pairing", None, PAIRING)?;
        self.journey.tap("pair-confirm.trust")?;
        let row = format!("hosts.row.{desk}");
        self.journey.wait(
            |drawn| drawn.has(&row) && !drawn.has("pair-confirm"),
            "the desk paired",
        )?;
        process.wait(Duration::from_secs(30))
    }

    fn hosts(&mut self) -> Result<(), String> {
        let row = format!("hosts.row.{}", self.journey.host_id("desk"));
        self.journey.tab("Hosts")?;
        self.journey.wait(
            |drawn| labelled(drawn, "desk") && drawn.has(&row),
            "the hosts",
        )?;
        self.photograph("hosts", None, &[])
    }

    fn fleet(&mut self) -> Result<(), String> {
        let rows: Vec<String> = ["planner", "keeper", "asker", "dialog"]
            .iter()
            .map(|name| format!("home.row.{}", self.agent(name)))
            .collect();
        self.journey.tab("Agents")?;
        self.journey.wait(
            |drawn| rows.iter().all(|row| drawn.has(row)),
            "every agent in the fleet",
        )?;
        self.photograph("fleet", None, &[])
    }

    fn open(
        &mut self,
        agent: &str,
        ready: impl Fn(&Drawn) -> bool,
        description: &str,
    ) -> Result<Drawn, String> {
        let row = format!("home.row.{}", self.agent(agent));
        self.journey.tab("Agents")?;
        self.journey.wait_for(&[&row])?;
        // A row redrawn by an update landing as it is tapped (a fleet row
        // takes every change to its agent) drops the tap or refuses it; a
        // person taps again.
        for attempt in 0..3 {
            let opened = self.journey.tap(&row).and_then(|()| {
                self.journey.wait_within(
                    |drawn| drawn.has("chat.back"),
                    "the chat opened",
                    Duration::from_secs(5),
                )
            });
            match opened {
                Ok(_) => break,
                Err(error) if attempt == 2 => return Err(error),
                Err(_) => self.settle()?,
            }
        }
        self.journey
            .wait(|drawn| drawn.has("chat.back") && ready(drawn), description)
    }

    fn back(&mut self) -> Result<(), String> {
        self.journey.tap("chat.back")?;
        self.journey
            .wait(|drawn| !drawn.has("chat.back"), "the fleet")?;
        // A row tapped while the chat is still leaving does not open.
        self.settle()
    }

    fn chat_turn(&mut self) -> Result<(), String> {
        self.open(
            "planner",
            |drawn| labelled(drawn, "share one string") && drawn.has(TURN[0]),
            "the answered chat",
        )?;
        self.photograph("chat-turn", None, TURN)?;
        self.back()
    }

    fn question(&mut self) -> Result<(), String> {
        self.open(
            "asker",
            |drawn| drawn.has("ask") && labelled(drawn, "Only what users will notice"),
            "the question card",
        )?;
        self.photograph("claude-sdk-ask-question", None, &[])?;
        self.back()
    }

    /// Terminal Claude's tool-server dialog is an ask the phone cannot
    /// answer; its card offers the way out.
    fn escape(&mut self) -> Result<(), String> {
        self.journey.request(
            Control::Send {
                agent: "dialog".to_owned(),
                text: "File the relay bug.".to_owned(),
            },
            None,
        )?;
        self.journey.wait_chat(
            "desk",
            "dialog",
            |chat| phase(chat) == "NEEDS_YOU",
            "dialog-open",
        )?;
        self.open(
            "dialog",
            |drawn| drawn.has("ask") && drawn.has("chat.row.unanswerable"),
            "the unanswerable card",
        )?;
        self.photograph("ask-escape", None, &[])?;
        self.back()
    }

    /// A draft holding two files at an accessibility text size: each name
    /// wraps rather than losing its extension, its size sits under it, and
    /// each remove mark is a thumb's target. The accessibility audit takes
    /// this screen too, and holds those marks to 44 points.
    fn composer_chips(&mut self) -> Result<(), String> {
        self.open(
            "planner",
            |drawn| labelled(drawn, "share one string"),
            "the answered chat",
        )?;
        self.journey
            .app(json!({"kind": "dynamicType", "size": "accessibility3"}))?;
        // One at a time: two attached together land in the draft in
        // whichever order they finish.
        for (count, (name, text)) in [
            ("pairing-errors-before-and-after.md", "# Pairing errors\n"),
            ("relay.log", "relay up\n"),
        ]
        .into_iter()
        .enumerate()
        {
            self.journey.app(json!({
                "kind": "attach", "agent": "planner", "attachment": "file", "name": name,
                "mime": "text/plain", "base64": STANDARD.encode(text),
            }))?;
            let chip = if count == 0 {
                "chat.attachment".to_owned()
            } else {
                format!("chat.attachment#{}", count + 1)
            };
            self.journey
                .wait(|drawn| drawn.has(&chip), &format!("{name} in the draft"))?;
        }
        self.journey.wait(
            |drawn| drawn.has("chat.attachment#2") && drawn.has("chat.attachment.remove#2"),
            "both chips in the draft",
        )?;
        self.photograph("composer-chips", None, TURN)?;
        // The draft goes back as it was, and the text size with it, so the
        // screens after this one are drawn as they always were.
        for _ in 0..2 {
            self.journey.tap("chat.attachment.remove")?;
            self.settle()?;
        }
        self.journey
            .wait(|drawn| !drawn.has("chat.attachment"), "the chips removed")?;
        self.journey
            .app(json!({"kind": "dynamicType", "size": "large"}))?;
        self.back()
    }

    /// Every screen a person reads most, at an accessibility text size:
    /// nothing overprints, nothing is cut mid-word, and every control is on
    /// the screen. The answered chat is opened before the text grows, since
    /// its row is below the fold at that size. The text size goes back
    /// afterwards, so the screens after this one are drawn as they always
    /// were.
    fn large_text(&mut self) -> Result<(), String> {
        let desk = format!("hosts.row.{}", self.journey.host_id("desk"));
        let asker = format!("home.row.{}", self.agent("asker"));
        self.open(
            "planner",
            |drawn| labelled(drawn, "share one string") && drawn.has(TURN[0]),
            "the answered chat",
        )?;
        self.journey
            .app(json!({"kind": "dynamicType", "size": "accessibility3"}))?;
        self.settle()?;
        self.photograph("large-text", Some("chat"), TURN)?;
        self.back()?;
        self.journey
            .wait(|drawn| drawn.has(&asker), "the fleet at a large text size")?;
        self.settle()?;
        self.photograph("large-text", Some("fleet"), &[])?;
        self.journey.tab("Hosts")?;
        self.journey
            .wait(|drawn| drawn.has(&desk), "the hosts at a large text size")?;
        self.settle()?;
        self.photograph("large-text", Some("hosts"), &[])?;
        self.journey.settings()?;
        self.journey.wait(
            |drawn| drawn.has("you.appearance"),
            "Settings at a large text size",
        )?;
        self.settle()?;
        self.photograph("large-text", Some("you"), &[])?;
        self.journey.tap("you.close")?;
        self.journey
            .wait(|drawn| !drawn.has("you"), "Settings put away")?;
        self.journey.tab("Agents")?;
        self.journey.tap("home.newAgent")?;
        self.journey.wait(
            |drawn| {
                drawn.value("new-agent.host") == "desk"
                    && !drawn.value("new-agent.directory").is_empty()
            },
            "New Agent at a large text size",
        )?;
        self.settle()?;
        self.photograph("large-text", Some("new-agent"), &[])?;
        self.journey.tap("new-agent.cancel")?;
        self.journey
            .wait(|drawn| !drawn.has("new-agent"), "the fleet again")?;
        self.settle()?;
        self.open(
            "asker",
            |drawn| drawn.has("ask") && labelled(drawn, "Only what users will notice"),
            "the question card at a large text size",
        )?;
        self.settle()?;
        self.photograph("large-text", Some("question"), &[])?;
        self.back()?;
        self.journey
            .app(json!({"kind": "dynamicType", "size": "large"}))?;
        Ok(())
    }

    /// The desk checkpoints, moves on a turn the phone sees, then loses power
    /// back to the checkpoint: the phone's replica is Reset and the rebuilt
    /// chat swaps in under the rows on screen.
    fn origin_rewind(&mut self) -> Result<(), String> {
        self.journey.request(
            Control::Checkpoint {
                host: "desk".to_owned(),
            },
            None,
        )?;
        self.journey.request(
            Control::Send {
                agent: "keeper".to_owned(),
                text: "Carry on.".to_owned(),
            },
            None,
        )?;
        self.journey.wait_chat(
            "desk",
            "keeper",
            |chat| phase(chat) == "IDLE" && holds(chat, "after the checkpoint"),
            "keeper-moved-on",
        )?;
        self.open(
            "keeper",
            |drawn| labelled(drawn, "after the checkpoint") && drawn.has(TURN[0]),
            "the chat before",
        )?;
        self.photograph("origin-rewind", Some("before"), TURN)?;
        self.journey.request(
            Control::Rewind {
                host: "desk".to_owned(),
                cuts: Vec::new(),
            },
            None,
        )?;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let drawn = self.journey.elements()?;
            if !(labelled(&drawn, "safely stored") && labelled(&drawn, "after the checkpoint")) {
                return Err("rows left the screen during the Reset".to_owned());
            }
            if labelled(&drawn, "Exited") {
                break;
            }
            if Instant::now() > deadline {
                return Err("the rebuilt chat never swapped in".to_owned());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.journey
            .record
            .actions
            .push("the rows stayed on screen until the rebuilt chat swapped in".to_owned());
        self.photograph("origin-rewind", Some("after"), TURN)?;
        self.back()
    }

    /// The whole way every time, photographing only what was asked: a
    /// screen's element geometry includes the pages under it, which hold
    /// whatever the earlier steps left there.
    fn the_whole_way(&mut self) -> Result<(), String> {
        self.prepare_the_desk()?;
        self.pairing()?;
        for (_, reach) in WAY {
            if let Some(reach) = reach {
                reach(self)?;
            }
        }
        Ok(())
    }
}

/// `testnet journey goldens`: every screen asked for (all of them when
/// `only` is empty), compared, rewritten with `update`, or written to
/// `review` with glass on. With `perturb`, the named design token is moved
/// (or, named `needs-you-dot`, that one small mark taken away) before
/// anything is drawn, and the run fails unless every photograph comes back
/// different with a difference image: what proves the comparison would
/// notice.
pub(crate) fn run(
    only: Vec<String>,
    update: bool,
    perturb: Option<String>,
    review: Option<PathBuf>,
) -> ExitCode {
    match photograph_the_way(only, update, perturb.as_deref(), review) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(2)
        }
    }
}

/// `testnet journey goldens-perturb`: the golden run once per change, each
/// required to notice. By default a design colour token moved, then only the
/// needs-you dot taken away (some 450 pixels no element geometry names, so
/// only the pixel budget can notice), both on the fleet.
pub(crate) fn perturb(only: Vec<String>, changes: Vec<String>) -> ExitCode {
    let mut passed = true;
    for change in changes {
        match photograph_the_way(only.clone(), false, Some(&change), None) {
            Ok(noticed) => passed &= noticed,
            Err(error) => {
                eprintln!("{error}");
                passed = false;
            }
        }
    }
    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn photograph_the_way(
    only: Vec<String>,
    update: bool,
    perturb: Option<&str>,
    review: Option<PathBuf>,
) -> Result<bool, String> {
    let manifest = manifest()?;
    let declared: BTreeSet<String> = manifest
        .screens
        .iter()
        .map(|screen| screen.id.clone())
        .collect();
    let reached: BTreeSet<String> = WAY.iter().map(|(id, _)| (*id).to_owned()).collect();
    if declared != reached {
        return Err(format!(
            "{MANIFEST} declares {declared:?}; this driver reaches {reached:?}"
        ));
    }
    let unknown: Vec<&str> = only
        .iter()
        .filter(|name| !declared.contains(*name))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "{MANIFEST} has no screen named {}",
            unknown.join(", ")
        ));
    }
    let wanted = if only.is_empty() {
        declared
    } else {
        only.into_iter().collect()
    };
    let root = root();
    let udid = golden_udid()?;
    let began = Instant::now();
    let output = match (&review, perturb) {
        (Some(_), _) => root.join("target/ios/goldens-review"),
        (None, Some(token)) => root.join("target/ios/goldens-perturb").join(token),
        (None, None) => root.join("target/ios/goldens"),
    };
    let review = match review {
        Some(directory) => {
            std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
            Some(
                directory
                    .canonicalize()
                    .map_err(|error| error.to_string())?,
            )
        }
        None => None,
    };
    let mut journey = PhoneJourney::serve(
        Setup {
            topology: root.join(&manifest.topology),
            output: output.clone(),
            goldens: root.join(GOLDENS),
            app: debug_app(),
        },
        &Target::Simulator(udid.clone()),
    )?;
    journey.update = update;
    // Review captures are of the glass itself.
    journey.flat = review.is_none();
    let camera = match &review {
        Some(directory) => Camera::Review(directory.clone()),
        None => Camera::Compare,
    };
    let mut goldens = Goldens::new(journey, &manifest, wanted, camera);
    let taken = goldens.journey.launch(Launch::plain()).and_then(|()| {
        if let Some(token) = perturb {
            goldens
                .journey
                .app(json!({"kind": "perturb", "token": token}))?;
        }
        goldens.the_whole_way()
    });
    let names: Vec<String> = goldens
        .outcomes
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    match &taken {
        Ok(()) => goldens.journey.finish(&names),
        Err(error) => goldens.journey.fail(error),
    }
    let closed = goldens.journey.close();
    taken?;
    closed?;
    let outcomes = goldens.outcomes;
    let seconds = began.elapsed().as_secs_f64();
    if let Some(directory) = review {
        println!(
            "{} review captures with glass on in {seconds:.1}s under {}",
            outcomes.len(),
            directory.display()
        );
        return Ok(true);
    }
    println!(
        "{} photographs in {seconds:.1}s; captures under {}",
        outcomes.len(),
        output.display()
    );
    if let Some(token) = perturb {
        let missed: Vec<&str> = outcomes
            .iter()
            .filter(|(name, differs)| {
                differs.is_none() || !output.join("diff").join(name).join("diff.png").is_file()
            })
            .map(|(name, _)| name.as_str())
            .collect();
        if !missed.is_empty() || outcomes.is_empty() {
            eprintln!(
                "the {token} token was moved and nothing noticed on {}",
                missed.join(", ")
            );
            return Ok(false);
        }
        println!(
            "every photograph differs with {token} moved; difference images under {}",
            output.join("diff").display()
        );
        return Ok(true);
    }
    let mut passed = true;
    for (name, differs) in &outcomes {
        if let Some(differs) = differs {
            eprintln!("FAILED {name}: {differs}");
            passed = false;
        }
    }
    Ok(passed)
}

// --- the accessibility audit ----------------------------------------------

const AUDIT_OUTPUT: &str = "target/ios/accessibility";
const AUDIT_TEST: &str = "AmuxUITests/AccessibilityAuditTests";
const AUDIT_RECORD: &str = "accessibility-audit.json";
const UI_TESTS: &str = "sh.amux.AmuxUITests.xctrunner";
/// How long the test may take to build and start, and to audit one page.
const STARTING: Duration = Duration::from_secs(900);
const AUDITING: Duration = Duration::from_secs(300);

/// The running UI test and the pages it has audited. XCUITest is the only
/// accessibility client an app cannot be for itself, so element kinds,
/// VoiceOver names and rectangles are only real from over there: at each
/// page the run asks the test to audit what is on screen and waits for its
/// answer, the two taking turns through files in a directory both can see.
struct Audit {
    directory: PathBuf,
    test: Child,
    pages: Vec<(String, u64)>,
}

impl Goldens {
    /// Asks the test to audit the page on screen, and waits until it has.
    fn audit(&mut self, page: &str) -> Result<(), String> {
        self.settle()?;
        let port = self.journey.port;
        let Camera::Audit(audit) = &mut self.camera else {
            unreachable!("audited only by an audit");
        };
        let index = audit.pages.len();
        let written = audit.directory.join(format!(".page-{index}.json"));
        std::fs::write(&written, json!({"page": page, "port": port}).to_string())
            .map_err(|error| error.to_string())?;
        std::fs::rename(&written, audit.directory.join(format!("page-{index}.json")))
            .map_err(|error| error.to_string())?;
        let done = audit.directory.join(format!("page-{index}.done"));
        let deadline = Instant::now() + AUDITING;
        while !done.is_file() {
            if audit.test.try_wait().ok().flatten().is_some() {
                return Err(format!("the audit ended before it answered for {page}"));
            }
            if Instant::now() > deadline {
                return Err(format!("the audit never answered for {page}"));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let answer: Value = serde_json::from_str(
            &std::fs::read_to_string(&done).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let controls = answer["controls"].as_u64().unwrap_or_default();
        audit.pages.push((page.to_owned(), controls));
        let said = format!("audited {page}: {controls} controls");
        println!("{said}");
        self.journey.record.actions.push(said);
        Ok(())
    }

    fn you(&mut self) -> Result<(), String> {
        self.journey.settings()?;
        self.audit("you")?;
        self.journey.tap("you.close")?;
        self.journey
            .wait(|drawn| !drawn.has("you"), "Settings put away")?;
        Ok(())
    }

    fn new_agent(&mut self) -> Result<(), String> {
        self.journey.tab("Agents")?;
        self.journey.wait_for(&["home.newAgent"])?;
        self.journey.tap("home.newAgent")?;
        self.journey.wait_for(&["new-agent"])?;
        self.audit("new-agent")
    }

    /// The goldens' way through the app as far as the pages a person
    /// reads, auditing each where the goldens photograph it, then Settings
    /// and New Agent.
    fn the_audited_way(&mut self) -> Result<(), String> {
        self.prepare_the_desk()?;
        self.pairing()?;
        self.hosts()?;
        self.fleet()?;
        self.chat_turn()?;
        self.question()?;
        self.escape()?;
        self.composer_chips()?;
        self.you()?;
        self.new_agent()
    }
}

/// `testnet journey accessibility`: every control on the app's live pages
/// audited for a name and a hit area. Links a markdown parser made out of a
/// run of an agent's prose are judged on their name but not on their size
/// (a span of a sentence is not a control this app draws and has no
/// rectangle to grow); each is named in the record so nothing small goes
/// unreported. The record the test leaves in its container is copied out
/// to somewhere a person can read it.
pub(crate) fn accessibility() -> ExitCode {
    match audit_the_way() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn audit_the_way() -> Result<bool, String> {
    let root = root();
    let manifest = manifest()?;
    let udid = golden_udid()?;
    let output = root.join(AUDIT_OUTPUT);
    std::fs::create_dir_all(&output).map_err(|error| error.to_string())?;
    if let Some(container) = test_container(&udid) {
        let _ = std::fs::remove_file(container.join("tmp").join(AUDIT_RECORD));
    }
    // Somewhere short the test on the simulator reads as well.
    let directory = tempfile::Builder::new()
        .prefix("amux-audit-")
        .tempdir_in("/tmp")
        .map_err(|error| error.to_string())?;
    let log = output.join("audit.log");
    let journey = PhoneJourney::serve(
        Setup {
            topology: root.join(&manifest.topology),
            output: output.join("journey"),
            goldens: root.join(GOLDENS),
            app: debug_app(),
        },
        &Target::Simulator(udid.clone()),
    )?;
    let started = File::create(&log)
        .and_then(|sink| {
            Command::new("xcodebuild")
                .args([
                    "test",
                    "-project",
                    "apps/apple/Amux.xcodeproj",
                    "-scheme",
                    "Amux",
                ])
                .args(["-configuration", "Debug", "-destination"])
                .arg(format!("id={udid}"))
                .arg("-derivedDataPath")
                .arg(root.join("target/ios/DerivedData"))
                .args(["-only-testing", AUDIT_TEST, "-quiet"])
                .env("TEST_RUNNER_AMUX_AUDIT_DIRECTORY", directory.path())
                .current_dir(&root)
                .stdout(sink.try_clone()?)
                .stderr(sink)
                .stdin(Stdio::null())
                .spawn()
        })
        .map_err(|error| format!("xcodebuild: {error}"));
    let test = match started {
        Ok(test) => test,
        Err(error) => {
            let _ = journey.close();
            return Err(error);
        }
    };
    let audit = Audit {
        directory: directory.path().to_owned(),
        test,
        pages: Vec::new(),
    };
    let mut goldens = Goldens::new(journey, &manifest, BTreeSet::new(), Camera::Audit(audit));
    let audited = (|| {
        let Camera::Audit(audit) = &mut goldens.camera else {
            unreachable!()
        };
        let deadline = Instant::now() + STARTING;
        while !audit.directory.join("ready").is_file() {
            if audit.test.try_wait().ok().flatten().is_some() || Instant::now() > deadline {
                return Err(format!("the audit never started; see {}", log.display()));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        // Installed and launched once the test has started: the test run
        // installs the app it targets, which would end one already running.
        goldens.journey.launch(Launch::plain())?;
        goldens.the_audited_way()?;
        let Camera::Audit(audit) = &mut goldens.camera else {
            unreachable!()
        };
        File::create(audit.directory.join("end")).map_err(|error| error.to_string())?;
        let deadline = Instant::now() + AUDITING;
        loop {
            if let Some(status) = audit.test.try_wait().map_err(|error| error.to_string())? {
                return Ok(status.success());
            }
            if Instant::now() > deadline {
                return Err("the audit did not end once every page was audited".to_owned());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    })();
    let Camera::Audit(audit) = &mut goldens.camera else {
        unreachable!()
    };
    match &audited {
        Ok(_) => {
            let pages: Vec<String> = audit
                .pages
                .iter()
                .map(|(page, controls)| format!("{page}: {controls} controls"))
                .collect();
            goldens.journey.finish(&pages);
        }
        Err(error) => goldens.journey.fail(error),
    }
    if audit.test.try_wait().ok().flatten().is_none() {
        let _ = audit.test.kill();
        let _ = audit.test.wait();
    }
    goldens.journey.close()?;
    let passed = audited?;
    let written = collect(&udid, &output);
    let found = written.as_deref().and_then(|written| {
        serde_json::from_str::<Value>(&std::fs::read_to_string(written).ok()?).ok()
    });
    if !passed {
        eprintln!("the audit failed; what it found is in {}", log.display());
        for fault in found
            .iter()
            .flat_map(|found| found["faults"].as_array().into_iter().flatten())
        {
            eprintln!(
                "  {}",
                fault
                    .as_str()
                    .map_or_else(|| fault.to_string(), str::to_owned)
            );
        }
        return Ok(false);
    }
    let (Some(written), Some(found)) = (written, found) else {
        eprintln!("the audit passed but left no record behind");
        return Ok(false);
    };
    let pages: Vec<&str> = found["pages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let inline = found["inlineLinks"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    println!(
        "{} controls across {} live pages ({}): every one named, and every one the app draws \
         at least 44 pt (links inside agent prose, judged on their names alone: {})",
        found["controls"],
        pages.len(),
        pages.join(", "),
        inline.len()
    );
    for link in inline {
        println!(
            "  inline link in {}: {} -> {} ({})",
            link["state"].as_str().unwrap_or_default(),
            link["label"],
            link["url"].as_str().unwrap_or_default(),
            link["size"]
                .as_str()
                .map_or_else(|| link["size"].to_string(), str::to_owned)
        );
    }
    println!("record: {}", written.display());
    Ok(true)
}

/// The UI test runner's data container on the simulator.
fn test_container(udid: &str) -> Option<PathBuf> {
    simctl(
        &["get_app_container", udid, UI_TESTS, "data"],
        Duration::from_secs(120),
    )
    .ok()
    .map(|path| PathBuf::from(path.trim()))
}

/// Copies the record out of the test process's own container.
fn collect(udid: &str, output: &Path) -> Option<PathBuf> {
    let source = test_container(udid)?.join("tmp").join(AUDIT_RECORD);
    if !source.is_file() {
        return None;
    }
    let destination = output.join(AUDIT_RECORD);
    std::fs::copy(&source, &destination).ok()?;
    let _ = std::fs::remove_file(source);
    Some(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames() -> BTreeMap<String, Vec<String>> {
        BTreeMap::from([
            ("fleet".to_owned(), Vec::new()),
            (
                "origin-rewind".to_owned(),
                vec!["before".to_owned(), "after".to_owned()],
            ),
        ])
    }

    #[test]
    fn the_driver_reaches_every_screen_the_committed_manifest_names() {
        let manifest = manifest().expect("the manifest");
        let declared: BTreeSet<&str> = manifest
            .screens
            .iter()
            .map(|screen| screen.id.as_str())
            .collect();
        let reached: BTreeSet<&str> = WAY.iter().map(|(id, _)| *id).collect();
        assert_eq!(declared, reached);
        assert!(root().join(&manifest.topology).is_file());
    }

    #[test]
    fn a_frame_is_named_after_its_screen() {
        assert_eq!(label(&frames(), "fleet", None).unwrap(), "fleet");
        assert_eq!(
            label(&frames(), "origin-rewind", Some("after")).unwrap(),
            "origin-rewind-after"
        );
    }

    #[test]
    fn a_frame_the_manifest_does_not_declare_is_refused() {
        assert!(label(&frames(), "origin-rewind", Some("during")).is_err());
        assert!(label(&frames(), "origin-rewind", None).is_err());
        assert!(label(&frames(), "fleet", Some("before")).is_err());
    }
}
