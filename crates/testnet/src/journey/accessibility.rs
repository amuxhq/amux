//! `testnet journey accessibility`: the phone app driven to each page a
//! person reads, every page audited for names and hit areas.
//!
//! The run serves journeys/topologies/phone-audit.json, installs the debug
//! app fresh on the leased simulator, pairs the desk by the code it printed,
//! and taps through the app while the served net makes the desk's agents do
//! what each page needs. At each page it asks a running UI test to audit
//! what is on screen.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use super::phone::{
    Drawn, Launch, PhoneJourney, Setup, Target, debug_app, golden_udid, labelled, printed_pin,
    simctl,
};
use super::{holds, phase, root};
use crate::door::Control;

/// The desk and its agents: one that has answered, one that keeps a
/// recovery turn, one asking a question, and a terminal Claude whose tool
/// server opens a dialog the phone cannot answer.
const TOPOLOGY: &str = "journeys/topologies/phone-audit.json";
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

/// The app being driven, and the test auditing it.
struct Walk {
    journey: PhoneJourney,
    audit: Audit,
}

impl Walk {
    fn agent(&self, name: &str) -> String {
        self.journey.agent_id(name)
    }

    fn settle(&self) -> Result<(), String> {
        self.journey.app(json!({"kind": "settle"}))?;
        Ok(())
    }

    /// Asks the test to audit the page on screen, and waits until it has.
    fn audit(&mut self, page: &str) -> Result<(), String> {
        self.settle()?;
        let port = self.journey.port;
        let audit = &mut self.audit;
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

    // --- the way through the pages ---------------------------------------

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
        self.audit("pairing")?;
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
        self.audit("hosts")
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
        self.audit("fleet")
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
            |drawn| labelled(drawn, "share one string") && drawn.has("chat.row.turn-end"),
            "the answered chat",
        )?;
        self.audit("chat-turn")?;
        self.back()
    }

    fn question(&mut self) -> Result<(), String> {
        self.open(
            "asker",
            |drawn| drawn.has("ask") && labelled(drawn, "Only what users will notice"),
            "the question card",
        )?;
        self.audit("claude-sdk-ask-question")?;
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
        self.audit("ask-escape")?;
        self.back()
    }

    /// A draft holding two files at an accessibility text size, where each
    /// chip's remove mark must still be a thumb's target.
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
        self.audit("composer-chips")?;
        // The draft goes back as it was, and the text size with it, so the
        // pages after this one are drawn at the ordinary size.
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

    /// Every page a person reads, in an order that reaches each in the
    /// state it needs: pairing, the hosts, the fleet, an answered chat, a
    /// question, the way out of an ask the phone cannot answer, a draft
    /// holding files, Settings and New Agent.
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
pub(crate) fn run() -> ExitCode {
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
            topology: root.join(TOPOLOGY),
            output: output.join("journey"),
            goldens: output.join("journey"),
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
    let mut walk = Walk {
        journey,
        audit: Audit {
            directory: directory.path().to_owned(),
            test,
            pages: Vec::new(),
        },
    };
    let audited = (|| {
        let deadline = Instant::now() + STARTING;
        while !walk.audit.directory.join("ready").is_file() {
            if walk.audit.test.try_wait().ok().flatten().is_some() || Instant::now() > deadline {
                return Err(format!("the audit never started; see {}", log.display()));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        // Installed and launched once the test has started: the test run
        // installs the app it targets, which would end one already running.
        walk.journey.launch(Launch::plain())?;
        walk.the_audited_way()?;
        let audit = &mut walk.audit;
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
    let Walk {
        mut journey,
        mut audit,
    } = walk;
    match &audited {
        Ok(_) => {
            let pages: Vec<String> = audit
                .pages
                .iter()
                .map(|(page, controls)| format!("{page}: {controls} controls"))
                .collect();
            journey.finish(&pages);
        }
        Err(error) => journey.fail(error),
    }
    if audit.test.try_wait().ok().flatten().is_none() {
        let _ = audit.test.kill();
        let _ = audit.test.wait();
    }
    journey.close()?;
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

    #[test]
    fn the_audited_topology_is_committed() {
        assert!(root().join(TOPOLOGY).is_file());
    }
}
