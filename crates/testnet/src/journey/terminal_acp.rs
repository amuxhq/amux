//! The terminal client's stories about agents a host runs over the Agent
//! Client Protocol. Every agent here is one the desk's config adds under
//! `[agents.<id>]` (see `journeys/topologies/terminal-acp.json`), run by the
//! fake ACP agent on a script of its own, so each story starts its own
//! agent from New Agent by the name the config gives it, and reads what
//! that agent was really sent from its input log.

use std::path::PathBuf;

use serde_json::Value;

use super::common::*;
use super::terminal::{TerminalJourney, at_home, at_rest, chat_of, timed, working};
use super::terminal_stories::spaced;
use super::{Proved, negative_control, never_sent, phase, quoted, reflected_once};

/// The host every story runs on: the terminal is on it too.
const DESK: &str = "desk";

const HELLO_ACP: &str = "Say hello.";
const HELLO_REPLY: &str = "Hello from Helper.";
const MIGRATE: &str = "Apply the migrations.";
const MIGRATED: &str = "The migration ran.";
const CLEAN: &str = "Clean the build folder.";
const LEFT_ALONE: &str = "I left the build folder alone.";
const LONG_JOB: &str = "Start the long job.";
const LONG_STARTED: &str = "Starting the long job.";
const LONG_DONE: &str = "The long job finished.";
const AGAIN: &str = "Are you still there?";
const READY_AGAIN: &str = "Ready again.";
const SOAK: &str = "Run the soak.";
const SOAK_STARTED: &str = "Starting the soak.";
const SOAK_DONE: &str = "The soak finished.";
const PEAK: &str = "Then log the peak.";
const PEAK_LOGGED: &str = "Logged the peak.";
const ONCE: &str = "Do one thing.";
const DONE_ONCE: &str = "Done, once.";
const ONE_MORE: &str = "One more thing.";
const KNOCK: &str = "Are you there?";
const SIGNED_IN: &str = "Signed in and ready.";
const SECOND: &str = "Now read this too.";
const SECOND_READ: &str = "Read the second message too.";
/// The agent's `authMethods` description, as the chat draws it.
const SIGN_IN_HINT: &str = "Run fake-acp login in the terminal on desk.";

/// The lines an agent the desk's config adds read from its agent process,
/// over every agent of that id started on the desk, each as JSON.
fn sent_to(journey: &mut TerminalJourney, id: &str, label: &str) -> Result<Vec<Value>, String> {
    let path: PathBuf = journey
        .ready
        .root
        .join("scripts")
        .join(format!("{}.input", crate::net::configured_script(DESK, id)));
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).map_err(|error| format!("{line}: {error}")))
        .collect::<Result<_, _>>()?;
    journey
        .record
        .observations
        .insert(label.to_owned(), Value::Array(lines.clone()));
    Ok(lines)
}

/// The requests of `method` among what an agent was sent.
fn calls<'a>(lines: &'a [Value], method: &str) -> Vec<&'a Value> {
    lines
        .iter()
        .filter(|line| line["method"] == method)
        .collect()
}

/// The prompts an agent was sent, as the text of their blocks.
fn prompted(lines: &[Value]) -> Vec<String> {
    calls(lines, "session/prompt")
        .iter()
        .map(|call| {
            call["params"]["prompt"]
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|block| block["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default()
        })
        .collect()
}

/// The option ids the agent was answered with, in order, for the
/// permissions it asked.
fn chosen(lines: &[Value]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line.get("method").is_none() && line["result"]["outcome"].is_object())
        .map(|line| {
            let outcome = &line["result"]["outcome"];
            outcome["optionId"]
                .as_str()
                .map_or_else(|| text(&outcome["outcome"]), str::to_owned)
        })
        .collect()
}

/// The prompts an agent was sent are exactly `expected`, in order.
fn prompted_exactly(lines: &[Value], expected: &[&str]) -> Result<(), String> {
    let prompts = prompted(lines);
    if prompts == expected {
        Ok(())
    } else {
        Err(format!(
            "the agent was prompted {prompts:?}, not {expected:?}"
        ))
    }
}

/// From home, New Agent with the agent named `name` picked from the desk's
/// list, sent `prompt`: the new agent's chat, open on its first reply
/// `reply` (or, with none, on the prompt), and the agent the desk lists
/// that it did not before.
fn create(
    journey: &mut TerminalJourney,
    pane: &str,
    name: &str,
    prompt: &str,
    reply: Option<&str>,
    frame: Option<&str>,
) -> Result<Value, String> {
    let before = ids(&journey.inventory(DESK, Some("inventory-before"))?);
    // The desk lists each agent its config adds once it has asked it, so
    // the list is whole only once its entry offers them all.
    journey.wait_configured_agents(DESK)?;
    journey.home(pane)?;
    journey.keys(pane, &["n"])?;
    journey.wait_terms(pane, &["What should the new agent work on?"])?;
    journey.keys(pane, &["C-s", "a"])?;
    journey.wait_terms(pane, &["╭─ Agent", "type to filter", "Fake ACP Agent"])?;
    if let Some(label) = frame {
        // The entry reaches the terminal a moment after the desk lists it;
        // the open list follows it, so the picture waits for the last agent
        // that fits the list.
        journey.wait_terms(pane, &["Runner"])?;
        journey.frame(pane, label)?;
    }
    // More agents than a flyover numbers: typed, the list narrows to it.
    journey.type_text(pane, name)?;
    journey.wait(
        pane,
        |frame| frame.contains(&format!("› {name} ")) && !frame.contains("Fake ACP Agent"),
        &format!("the list narrowed to {name}"),
    )?;
    journey.keys(pane, &["Enter"])?;
    journey.wait(
        pane,
        |frame| frame.contains(&format!("│ {name} │")) && !frame.contains("╭─ Agent"),
        &format!("{name} chosen"),
    )?;
    // Named, so home's columns, which fit the longest name, stay put.
    let called = name.to_lowercase().replace(' ', "-");
    journey.keys(pane, &["C-s", "n"])?;
    journey.wait_terms(pane, &["╭─ Name"])?;
    journey.type_text(pane, &called)?;
    journey.keys(pane, &["Enter"])?;
    journey.wait(
        pane,
        |frame| frame.contains(&format!("─ {called} │")) && !frame.contains("╭─ Name"),
        "the name given",
    )?;
    journey.type_text(pane, prompt)?;
    journey.wait_terms(pane, &[&format!("› {prompt}")])?;
    journey.keys(pane, &["Enter"])?;
    journey.wait(
        pane,
        |frame| {
            chat_of(frame).is_some()
                && frame.contains(prompt)
                && reply.is_none_or(|reply| frame.contains(reply))
        },
        &format!("{name}'s chat"),
    )?;
    let made = journey.wait_inventory(
        DESK,
        |agents| ids(agents).difference(&before).count() == 1,
        "created",
    )?;
    let id = ids(&made)
        .difference(&before)
        .next()
        .cloned()
        .unwrap_or_default();
    let agent = listed(&made, &id).cloned().unwrap_or_default();
    if agent["kind"] != "ACP" || agent["host_id"] != journey.host_id(DESK) {
        return Err(format!("the desk lists the new agent as {agent}"));
    }
    if agent["name"] != called.as_str() {
        return Err(format!("the desk named the new agent {}", agent["name"]));
    }
    Ok(agent)
}

/// New Agent lists an agent the desk's config adds by the name its entry
/// gives it, beside the net's own fake; started from there, it answers, and
/// the program its entry names is the one that received the prompt.
pub(crate) fn create_from_config(journey: &mut TerminalJourney) -> Proved {
    let pane = journey.launch("terminal", DESK, &[])?;
    journey.wait(&pane, at_home, "home")?;
    let agent = create(
        journey,
        &pane,
        "Helper",
        HELLO_ACP,
        Some(HELLO_REPLY),
        Some("agents"),
    )?;
    let id = text(&agent["id"]);
    journey.wait(
        &pane,
        |frame| at_rest(frame, &[HELLO_ACP, HELLO_REPLY]),
        "the reply at rest",
    )?;
    journey.frame(&pane, "replied")?;
    let chat = journey.wait_chat(
        DESK,
        &id,
        |chat| phase(chat) == "IDLE" && super::holds(chat, HELLO_REPLY),
        "answered",
    )?;
    reflected_once(&chat, HELLO_ACP)?;
    let control = negative_control(reflected_once(&chat, "Say goodbye."))?;
    let sent = sent_to(journey, "helper", "helper-input")?;
    prompted_exactly(&sent, &[HELLO_ACP])?;
    if calls(&sent, "session/new").len() != 1 {
        return Err(format!("Helper was not opened one session: {sent:?}"));
    }
    // Home lists it among the fleet, under the words its agent gives it.
    journey.home(&pane)?;
    journey.wait_terms(&pane, &["Helper"])?;
    journey.frame(&pane, "home")?;
    journey.quit_client(&pane)?;
    Ok(vec![
        "New Agent's agent list named the desk's configured agents by the names their entries give, Helper among them".to_owned(),
        format!(
            "picked and started with {}, Helper's chat opened on its reply {}",
            quoted(HELLO_ACP),
            quoted(HELLO_REPLY)
        ),
        "the desk lists the new agent as an ACP agent of its own and holds the prompt once".to_owned(),
        control,
        "the program Helper's entry names opened one session and was sent exactly that prompt"
            .to_owned(),
        "the client exited 0".to_owned(),
    ])
}

/// A permission the agent asks offers exactly its own choices; one allowed
/// and one rejected each reach the agent as the option it offered.
pub(crate) fn permission(journey: &mut TerminalJourney) -> Proved {
    let pane = journey.launch("terminal", DESK, &[])?;
    journey.wait(&pane, at_home, "home")?;
    let agent = create(journey, &pane, "Gatekeeper", MIGRATE, None, None)?;
    let id = text(&agent["id"]);
    // Antigravity's two choices: Allow and Deny.
    let asked = journey.read_ask(&pane, &["scripts/migrate --apply", "1. Yes", "2. No"])?;
    if asked.contains("Always") {
        return Err(format!(
            "the ask offers a choice the agent did not:\n{asked}"
        ));
    }
    journey.frame(&pane, "allow-asked")?;
    journey.keys(&pane, &["1"])?;
    journey.wait(
        &pane,
        |frame| at_rest(frame, &[MIGRATED]) && !frame.contains("1. Yes"),
        "the migration's end",
    )?;
    // OpenCode's three: allow once, always, reject.
    journey.type_text(&pane, CLEAN)?;
    journey.keys(&pane, &["Enter"])?;
    journey.read_ask(
        &pane,
        &["rm -rf build", "1. Yes", "2. Always allow", "3. No"],
    )?;
    journey.frame(&pane, "reject-asked")?;
    journey.keys(&pane, &["3"])?;
    journey.wait(
        &pane,
        |frame| at_rest(frame, &[LEFT_ALONE]) && !frame.contains("Always allow"),
        "the rejected call's end",
    )?;
    journey.frame(&pane, "answered")?;
    let chat = journey.wait_chat(
        DESK,
        &id,
        |chat| phase(chat) == "IDLE" && super::holds(chat, LEFT_ALONE),
        "answered",
    )?;
    reflected_once(&chat, MIGRATE)?;
    reflected_once(&chat, CLEAN)?;
    let sent = sent_to(journey, "gatekeeper", "gatekeeper-input")?;
    let answers = chosen(&sent);
    if answers != ["allow", "reject"] {
        return Err(format!(
            "Gatekeeper was answered {answers:?}, not allow then reject"
        ));
    }
    let control = negative_control(if chosen(&sent) == ["allow", "always"] {
        Ok(())
    } else {
        Err("Gatekeeper was not answered allow then always".to_owned())
    })?;
    journey.quit_client(&pane)?;
    Ok(vec![
        "the first ask offered only the agent's own two choices, Yes and No, and Yes was taken"
            .to_owned(),
        "the second offered its three, Yes, Always allow and No, and No was taken".to_owned(),
        "the agent was answered with the options it offered: allow, then reject; each turn went on to its reply".to_owned(),
        control,
        "the desk holds both prompts once".to_owned(),
        "the client exited 0".to_owned(),
    ])
}

/// Interrupt ends a running turn: the agent is sent a cancel, the words
/// after it are never said, and the agent answers the next prompt.
pub(crate) fn interrupt(journey: &mut TerminalJourney) -> Proved {
    let pane = journey.launch("terminal", DESK, &[])?;
    journey.wait(&pane, at_home, "home")?;
    let agent = create(journey, &pane, "Runner", LONG_JOB, Some(LONG_STARTED), None)?;
    let id = text(&agent["id"]);
    journey.wait(
        &pane,
        |frame| working(frame) && timed(frame) && frame.contains(LONG_STARTED),
        "the long job running",
    )?;
    journey.frame(&pane, "running")?;
    journey.keys(&pane, &["C-x"])?;
    journey.wait(
        &pane,
        |frame| at_rest(frame, &[LONG_STARTED]) && frame.contains("You stopped it"),
        "the turn interrupted",
    )?;
    journey.frame(&pane, "interrupted")?;
    journey.type_text(&pane, AGAIN)?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| at_rest(frame, &[AGAIN, READY_AGAIN]),
        "the next reply",
    )?;
    let chat = journey.wait_chat(
        DESK,
        &id,
        |chat| phase(chat) == "IDLE" && super::holds(chat, READY_AGAIN),
        "answered-after",
    )?;
    if super::holds(&chat, LONG_DONE) {
        return Err(format!("the interrupted turn went on: {chat}"));
    }
    let sent = sent_to(journey, "runner", "runner-input")?;
    let cancels = calls(&sent, "session/cancel").len();
    if cancels != 1 {
        return Err(format!("Runner was sent {cancels} cancels: {sent:?}"));
    }
    prompted_exactly(&sent, &[LONG_JOB, AGAIN])?;
    let control = negative_control(prompted_exactly(&sent, &[LONG_JOB]))?;
    journey.quit_client(&pane)?;
    Ok(vec![
        "ctrl+x interrupted the running turn, which ended saying You stopped it".to_owned(),
        "the agent was sent one session/cancel and never said what came after".to_owned(),
        format!("the next prompt {} was answered", quoted(AGAIN)),
        control,
        "the client exited 0".to_owned(),
    ])
}

/// Prompts sent while the agent works wait under the feed, with Withdraw
/// and no Send Now, since this agent cannot take one into a running turn.
/// The newest, withdrawn, never reaches the agent; the other is sent once
/// the turn ends, as a turn of its own.
pub(crate) fn queue_and_withdraw(journey: &mut TerminalJourney) -> Proved {
    let pane = journey.launch("terminal", DESK, &[])?;
    journey.wait(&pane, at_home, "home")?;
    let agent = create(journey, &pane, "Soaker", SOAK, Some(SOAK_STARTED), None)?;
    let id = text(&agent["id"]);
    journey.wait(
        &pane,
        |frame| working(frame) && frame.contains(SOAK_STARTED),
        "the soak running",
    )?;
    journey.type_text(&pane, PEAK)?;
    journey.keys(&pane, &["Enter"])?;
    journey.type_text(&pane, WITHDRAWN)?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| spaced(frame, PEAK, "queued") && spaced(frame, WITHDRAWN, "queued") && timed(frame),
        "two queued",
    )?;
    journey.frame(&pane, "queued")?;
    journey.keys(&pane, &["Up"])?;
    let selected = journey.wait(
        &pane,
        |frame| {
            frame.contains("[Withdraw]") && frame.contains("backspace withdraw") && timed(frame)
        },
        "the newest selected",
    )?;
    if selected.contains("Send Now") {
        return Err(format!(
            "a queued prompt offers Send Now on an agent that cannot take one:\n{selected}"
        ));
    }
    journey.frame(&pane, "selected")?;
    journey.keys(&pane, &["BSpace"])?;
    journey.wait_terms(&pane, &[&format!("› {WITHDRAWN}")])?;
    journey.keys(&pane, &["C-c"])?;
    journey.wait(
        &pane,
        |frame| !frame.contains(WITHDRAWN),
        "the withdrawn prompt cleared",
    )?;
    journey.request(open_gate("soak-done"), None)?;
    journey.wait(
        &pane,
        |frame| at_rest(frame, &[SOAK_DONE, PEAK, PEAK_LOGGED]),
        "both turns' ends",
    )?;
    journey.frame(&pane, "sent-after")?;
    let chat = journey.wait_chat(
        DESK,
        &id,
        |chat| phase(chat) == "IDLE" && super::holds(chat, PEAK_LOGGED),
        "settled",
    )?;
    reflected_once(&chat, SOAK)?;
    reflected_once(&chat, PEAK)?;
    never_sent(&chat, WITHDRAWN)?;
    let sent = sent_to(journey, "soaker", "soaker-input")?;
    prompted_exactly(&sent, &[SOAK, PEAK])?;
    let control = negative_control(never_sent(&chat, PEAK))?;
    journey.quit_client(&pane)?;
    Ok(vec![
        "two prompts sent while the agent worked queued under the feed, with Withdraw and no Send Now".to_owned(),
        "withdrawn, the newest came back to the composer as a draft and was cleared; neither the desk nor the agent received it".to_owned(),
        format!(
            "once the turn ended, {} was sent as a turn of its own and answered; the desk holds it once",
            quoted(PEAK)
        ),
        control,
        "the client exited 0".to_owned(),
    ])
}

/// An agent that cannot reopen a conversation answers, then its program
/// ends: the agent has ended for good, its chat keeps the conversation but
/// takes no message and offers no resume.
pub(crate) fn ends_for_good(journey: &mut TerminalJourney) -> Proved {
    let pane = journey.launch("terminal", DESK, &[])?;
    journey.wait(&pane, at_home, "home")?;
    let agent = create(journey, &pane, "One Shot", ONCE, Some(DONE_ONCE), None)?;
    let id = text(&agent["id"]);
    journey.wait(&pane, |frame| at_rest(frame, &[DONE_ONCE]), "the reply")?;
    // The next prompt finds the program ending.
    journey.type_text(&pane, ONE_MORE)?;
    journey.keys(&pane, &["Enter"])?;
    let ended = journey.wait_terms(&pane, &["Ended", "can't reopen this conversation"])?;
    if ended.contains("Resume") || ended.contains("resume") {
        return Err(format!("the ended agent offers to resume:\n{ended}"));
    }
    journey.frame(&pane, "ended")?;
    // Typing reaches no composer.
    journey.type_text(&pane, "Wake up.")?;
    std::thread::sleep(std::time::Duration::from_millis(500));
    let after = journey.capture(&pane)?;
    if after.contains("Wake up.") {
        return Err(format!("the ended agent's chat took a message:\n{after}"));
    }
    let exited = journey.wait_inventory(
        DESK,
        |agents| listed(agents, &id).is_some_and(|agent| agent["lifecycle"] == EXITED),
        "exited",
    )?;
    let row = listed(&exited, &id).cloned().unwrap_or_default();
    let chat = journey.chat(DESK, &id, Some("kept"))?;
    reflected_once(&chat, ONCE)?;
    if !super::holds(&chat, DONE_ONCE) {
        return Err(format!("the ended agent's chat lost its reply: {chat}"));
    }
    let sent = sent_to(journey, "one-shot", "one-shot-input")?;
    let reopened = calls(&sent, "session/load").len() + calls(&sent, "session/resume").len();
    if reopened != 0 || calls(&sent, "session/new").len() != 1 {
        return Err(format!(
            "One Shot was opened more than once, or asked to reopen: {sent:?}"
        ));
    }
    let control = negative_control(if row["lifecycle"] == LIVE {
        Ok(())
    } else {
        Err(format!("the desk lists it {}", row["lifecycle_name"]))
    })?;
    journey.quit_client(&pane)?;
    Ok(vec![
        format!("One Shot answered {} and its program then ended", quoted(ONCE)),
        "its chat kept the conversation and said it had ended, the agent unable to reopen it, with no resume offered".to_owned(),
        "typing in its chat reached no composer".to_owned(),
        "the desk lists it exited, and its program was opened once and never asked to reopen".to_owned(),
        control,
        "the client exited 0".to_owned(),
    ])
}

/// An agent signed out on the desk says so in its chat, in the agent's own
/// words for how to sign in; once signed in there, the next message opens
/// its conversation and every message waiting is answered in order.
pub(crate) fn sign_in(journey: &mut TerminalJourney) -> Proved {
    let pane = journey.launch("terminal", DESK, &[])?;
    journey.wait(&pane, at_home, "home")?;
    let agent = create(journey, &pane, "Locked", KNOCK, None, None)?;
    let id = text(&agent["id"]);
    journey.wait_terms(&pane, &[SIGN_IN_HINT])?;
    journey.frame(&pane, "signed-out")?;
    let waiting = journey.chat(DESK, &id, Some("signed-out"))?;
    if super::holds(&waiting, SIGNED_IN) {
        return Err(format!("the signed-out agent answered: {waiting}"));
    }
    journey.request(open_gate("locked-signed-in"), None)?;
    journey.type_text(&pane, SECOND)?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| at_rest(frame, &[SIGNED_IN, SECOND_READ]) && !frame.contains(SIGN_IN_HINT),
        "both answered",
    )?;
    journey.frame(&pane, "signed-in")?;
    let chat = journey.wait_chat(
        DESK,
        &id,
        |chat| phase(chat) == "IDLE" && super::holds(chat, SECOND_READ),
        "answered",
    )?;
    reflected_once(&chat, KNOCK)?;
    reflected_once(&chat, SECOND)?;
    let sent = sent_to(journey, "locked", "locked-input")?;
    prompted_exactly(&sent, &[KNOCK, SECOND])?;
    let control = negative_control(prompted_exactly(&sent, &[SECOND, KNOCK]))?;
    journey.quit_client(&pane)?;
    Ok(vec![
        format!(
            "started signed out, Locked's chat said so with the agent's own words: {}",
            quoted(SIGN_IN_HINT)
        ),
        "the message sent then waited; nothing answered it".to_owned(),
        "signed in on the desk, the next message opened its conversation".to_owned(),
        "both messages reached the agent in the order they were sent and each was answered"
            .to_owned(),
        control,
        "the client exited 0".to_owned(),
    ])
}
