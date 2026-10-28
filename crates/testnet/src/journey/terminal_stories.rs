//! The terminal client's stories: each drives the client as a person
//! would, captures the frames its goldens review and proves what the hosts
//! recorded, one claim per line.

use std::fs::File;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::common::*;
use super::terminal::{TerminalJourney, agent_row, at_home, at_rest, chat_of, working};
use super::{
    Proved, holds, items, negative_control, never_sent, phase, prompts, quoted, reflected_once,
    root,
};
use crate::ctl::phone::copy_dir;
use crate::ctl::session::pin_in;
use crate::door::Control;

type Tell = fn(&mut TerminalJourney) -> Proved;

const STORIES: &[(&str, Tell)] = &[
    ("conversation-decision-claude-pty", |j| {
        conversation_decision(j, "decision-pty", false)
    }),
    ("conversation-decision-claude-sdk", |j| {
        conversation_decision(j, "decision-sdk", true)
    }),
    ("conversation-decision-codex", |j| {
        conversation_decision(j, "decision-codex", true)
    }),
    ("leave-and-recover", leave_and_recover),
    ("manage-agent", manage_agent),
    ("reach-host", reach_host),
    ("attachment-or-review", attachment_or_review),
    ("keep-authority", keep_authority),
    ("decide-plan-claude-sdk", |j| {
        decide_plan(j, "planner", true)
    }),
    ("decide-plan-claude-pty", |j| {
        decide_plan(j, "planner-pty", false)
    }),
    ("decide-plan-codex", decide_plan_codex),
    ("answer-questions", answer_questions),
    ("tool-server-asks", tool_server_asks),
    ("queue-and-steer", queue_and_steer),
    ("send-while-away", send_while_away),
    ("send-rejected", send_rejected),
    ("send-may-not-have-arrived", send_may_not_have_arrived),
    ("composer-limits", composer_limits),
    ("new-agent-terminal", new_agent_terminal),
    ("new-agent-worktree", new_agent_worktree),
    ("report-a-problem", report_a_problem),
];

/// The bound one story runs under.
const STORY: Duration = Duration::from_secs(600);

/// Runs one story; its result lands in `target/journeys/<story>`.
pub(crate) fn run(name: &str) -> ExitCode {
    let Some((_, tell)) = STORIES.iter().find(|(id, _)| *id == name) else {
        let names: Vec<&str> = STORIES.iter().map(|(id, _)| *id).collect();
        eprintln!(
            "usage: testnet journey terminal {{all,{}}}",
            names.join(",")
        );
        return ExitCode::from(2);
    };
    let mut journey =
        match super::story(name, "terminal").and_then(|story| TerminalJourney::start(&story)) {
            Ok(journey) => journey,
            Err(error) => {
                eprintln!("FAIL {name}: {error}");
                return ExitCode::FAILURE;
            }
        };
    let told = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tell(&mut journey)))
        .unwrap_or_else(|panic| Err(format!("panicked: {}", panic_message(&panic))));
    match &told {
        Ok(proved) => journey.finish(proved),
        Err(error) => journey.fail(error),
    }
    let closed = journey.close();
    match (told, closed) {
        (Ok(proved), Ok(())) => {
            println!("PASS {name}");
            for line in proved {
                println!("- {line}");
            }
            ExitCode::SUCCESS
        }
        (Err(error), _) | (Ok(_), Err(error)) => {
            eprintln!("FAIL {name}: {error}");
            ExitCode::FAILURE
        }
    }
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|said| (*said).to_owned()))
        .unwrap_or_default()
}

/// Every terminal story in the manifest, each in a process of its own under
/// its own bound, a failure not stopping the rest. One line per story goes
/// to the screen and to `target/journeys/summary.txt`, each story's whole
/// output to `target/journeys/<story>.log`.
pub(crate) fn run_all() -> ExitCode {
    let names: Vec<String> = match super::manifest() {
        Ok(stories) => stories
            .into_iter()
            .filter(|story| story.clients.iter().any(|client| client == "terminal"))
            .map(|story| story.id)
            .collect(),
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let output = root().join("target/journeys");
    if let Err(error) = std::fs::create_dir_all(&output) {
        eprintln!("{}: {error}", output.display());
        return ExitCode::FAILURE;
    }
    let summary = output.join("summary.txt");
    let mut lines = Vec::new();
    let mut failed = 0;
    for name in &names {
        let (passed, line) = run_bounded(name, &output);
        failed += usize::from(!passed);
        println!("{line}");
        lines.push(line);
        let _ = std::fs::write(&summary, lines.join("\n") + "\n");
    }
    let total = format!(
        "{} of {} terminal stories passed",
        names.len() - failed,
        names.len()
    );
    println!("{total}");
    lines.push(total);
    let _ = std::fs::write(&summary, lines.join("\n") + "\n");
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// One story in a child process, its output kept; how it went in one line.
fn run_bounded(name: &str, output: &Path) -> (bool, String) {
    let log = output.join(format!("{name}.log"));
    let started = Instant::now();
    let spawned = File::create(&log).and_then(|file| {
        let errors = file.try_clone()?;
        Command::new(std::env::current_exe()?)
            .args(["journey", "terminal", name])
            .current_dir(root())
            .stdin(Stdio::null())
            .stdout(file)
            .stderr(errors)
            .spawn()
    });
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => return (false, format!("FAIL {name} (0s): {error}")),
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= STORY => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(_) => break None,
        }
    };
    let seconds = started.elapsed().as_secs();
    if status.is_some_and(|status| status.success()) {
        return (true, format!("PASS {name} ({seconds}s)"));
    }
    let said = std::fs::read_to_string(&log).unwrap_or_default();
    let prefix = format!("FAIL {name}: ");
    let reason = said
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(&prefix).map(str::to_owned))
        .unwrap_or_else(|| match status.and_then(|status| status.code()) {
            None => format!("timed out after {}s", STORY.as_secs()),
            Some(code) => format!("exited {code}"),
        });
    (false, format!("FAIL {name} ({seconds}s): {reason}"))
}

// --- reading frames ------------------------------------------------------

/// `first`, then at least one space, then `then`, somewhere in `text`.
fn spaced(text: &str, first: &str, then: &str) -> bool {
    text.match_indices(first).any(|(at, _)| {
        let rest = &text[at + first.len()..];
        let after = rest.trim_start();
        after.len() < rest.len() && after.starts_with(then)
    })
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A count of edits drawn as a step: `3 edits`, `1 edit`.
fn counts_edits(frame: &str) -> bool {
    frame.match_indices(" edit").any(|(at, _)| {
        let before = &frame[..at];
        let digits = before.len() - before.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        let bounded_before = before[..before.len() - digits]
            .chars()
            .next_back()
            .is_none_or(|c| !word_char(c));
        let after = &frame[at + " edit".len()..];
        let after = after.strip_prefix('s').unwrap_or(after);
        digits > 0 && bounded_before && after.chars().next().is_none_or(|c| !word_char(c))
    })
}

fn answered_with(chat: &Value, words: &str) -> bool {
    phase(chat) == "IDLE" && holds(chat, words)
}

// --- the stories ---------------------------------------------------------

const DRAFT: &str = "Keep this draft through the outage.";

fn conversation_decision(
    journey: &mut TerminalJourney,
    agent: &str,
    provider_logs: bool,
) -> Proved {
    let pane = journey.launch("terminal", "desk", &[])?;
    journey.open_chat(&pane, agent)?;
    journey.type_text(&pane, PROMPT)?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait_terms(&pane, &[PROMPT])?;
    journey.read_ask(&pane, &["Wants to run", "deploy --check", "1. Yes"])?;
    let sent = journey.wait_chat(
        "desk",
        agent,
        |chat| !prompts(chat, PROMPT).is_empty(),
        "prompt-reflected",
    )?;
    reflected_once(&sent, PROMPT)?;
    let control = negative_control(reflected_once(&sent, &format!("{PROMPT} (a wrong prompt)")))?;
    journey.frame(&pane, "permission")?;
    // The first choice is Yes, this once.
    journey.keys(&pane, &["1"])?;
    journey.wait(
        &pane,
        |frame| at_rest(frame, &[REPLY]) && !frame.contains("Wants to run"),
        "the settled turn",
    )?;
    let settled = journey.wait_chat(
        "desk",
        agent,
        |chat| answered_with(chat, REPLY),
        "turn-settled",
    )?;
    reflected_once(&settled, PROMPT)?;
    let mut proved = vec![
        format!("{} reflected once in the desk's chat", quoted(PROMPT)),
        control,
        "the permission ask offered its choices in the composer's box and Yes was taken".to_owned(),
        format!("the reply {} arrived and the desk says idle", quoted(REPLY)),
    ];
    if provider_logs {
        let lines = journey.provider_input(agent, "provider-input")?;
        let answers = lines
            .iter()
            .filter(|line| {
                let line = line.to_lowercase();
                line.contains("allow") || line.contains("accept")
            })
            .count();
        if answers != 1 {
            return Err(format!("the provider did not receive one allow: {lines:?}"));
        }
        proved.push("the provider received exactly one allow".to_owned());
    }
    journey.frame(&pane, "settled")?;
    journey.quit_client(&pane)?;
    proved.push("the client exited 0".to_owned());
    Ok(proved)
}

fn leave_and_recover(journey: &mut TerminalJourney) -> Proved {
    let first = "The recovery state is safely stored.";
    let second = "A second turn arrived while you were away.";
    let third = "Back after the restart.";
    // A real first run on the laptop makes its replica of the desk's agent.
    let pane = journey.launch("first-run", "laptop", &[])?;
    journey.open_chat(&pane, "keeper")?;
    journey.wait_terms(&pane, &[first])?;
    journey.wait_chat(
        "laptop",
        "keeper",
        |chat| holds(chat, first),
        "replica-made",
    )?;
    journey.quit_client(&pane)?;

    // The desk goes out of reach; a fresh client opens from the laptop's
    // store, with the composer waiting and the draft kept.
    journey.request(sever("desk", "laptop"), None)?;
    let pane = journey.launch("terminal", "laptop", &[])?;
    journey.open_chat(&pane, "keeper")?;
    journey.wait_terms(&pane, &[first, "desk away · not current"])?;
    journey.type_text(&pane, DRAFT)?;
    journey.wait_terms(
        &pane,
        &[DRAFT, "enter holds it", "it sends when the host is back"],
    )?;
    journey.frame(&pane, "cached-offline")?;

    // Work goes on at the desk while the laptop is cut off; when the link
    // returns the delta appends once and the composer is live again.
    journey.request(
        Control::Send {
            agent: "keeper".to_owned(),
            text: "Carry on.".to_owned(),
        },
        None,
    )?;
    journey.wait_chat(
        "desk",
        "keeper",
        |chat| holds(chat, second),
        "desk-moved-on",
    )?;
    journey.request(restore("desk", "laptop"), None)?;
    journey.wait(
        &pane,
        |frame| frame.contains(second) && !frame.contains("not current") && frame.contains(DRAFT),
        "the reconciled chat with the draft kept",
    )?;
    let desk = journey.chat("desk", "keeper", Some("desk-after-restore"))?;
    let laptop = journey.wait_chat(
        "laptop",
        "keeper",
        |chat| keys_of(chat) == keys_of(&desk),
        "laptop-matches-desk",
    )?;
    let arrived = items(&laptop)
        .iter()
        .filter(|item| {
            item["text"]
                .as_str()
                .is_some_and(|said| said.contains(second))
        })
        .count();
    if arrived != 1 {
        return Err(format!("the delta arrived other than once: {laptop}"));
    }
    journey.frame(&pane, "reconciled")?;

    // The laptop's own daemon restarts under the open chat: the client
    // reconnects by itself, keeps the chat and the draft, and sends. The
    // daemon stays down until the client says so, because the live screen
    // reads the same before the outage as after it and a send pressed while
    // the client is reconnecting is not taken.
    journey.request(
        Control::KillDaemon {
            host: "laptop".to_owned(),
        },
        None,
    )?;
    journey.wait_terms(
        &pane,
        &["reconnecting", DRAFT, "it sends once amux is back"],
    )?;
    journey.request(
        Control::RestartDaemon {
            host: "laptop".to_owned(),
        },
        None,
    )?;
    journey.wait(
        &pane,
        |frame| {
            frame.contains(DRAFT) && !frame.contains("enter holds it") && frame.contains(second)
        },
        "the chat live again after the restart",
    )?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait_terms(&pane, &[third])?;
    let after = journey.wait_chat(
        "desk",
        "keeper",
        |chat| holds(chat, third),
        "sent-after-restart",
    )?;
    reflected_once(&after, DRAFT)?;
    journey.frame(&pane, "after-restart")?;
    // The desk rewinds: the laptop's replica is Reset, and the rows stay on
    // screen until the rebuilt transcript swaps in.
    journey.request(
        Control::Checkpoint {
            host: "desk".to_owned(),
        },
        None,
    )?;
    journey.request(
        Control::Rewind {
            host: "desk".to_owned(),
            cuts: Vec::new(),
        },
        None,
    )?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let screen = journey.capture(&pane)?;
        if !screen.contains(first) || !screen.contains(third) {
            return Err(format!("rows left the screen during the Reset:\n{screen}"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let desk = journey.chat("desk", "keeper", Some("desk-after-rewind"))?;
    journey.wait_chat(
        "laptop",
        "keeper",
        |chat| keys_of(chat) == keys_of(&desk),
        "laptop-after-reset",
    )?;
    // A restored drive has no running processes: the agent reads exited.
    journey.wait(
        &pane,
        |frame| {
            !frame.contains("not current")
                && frame.contains(third)
                && frame.contains("enter resume")
        },
        "the chat after the Reset",
    )?;
    journey.frame(&pane, "after-reset")?;

    journey.quit_client(&pane)?;
    Ok(lines(&[
        "a fresh client painted the cached chat from the laptop's store with the desk away and the composer waiting",
        "the draft typed while detached was kept through the outage and the daemon restart",
        "the desk's second turn appended once when the link returned; the laptop holds exactly the desk's items",
        "after the laptop's daemon restarted the client reconnected on its own and the draft was sent and answered",
        "rows stayed on screen through the desk's rewind and Reset until the rebuilt transcript swapped in",
        "the client exited 0",
    ]))
}

fn manage_agent(journey: &mut TerminalJourney) -> Proved {
    let before = ids(&journey.inventory("desk", Some("inventory-before"))?);
    let pane = journey.launch("terminal", "desk", &[])?;
    journey.wait_terms(&pane, &["keeper", "n new"])?;
    // Started from home with its first prompt: a Claude the desk runs
    // headless, whose chat opens on the first reply.
    journey.keys(&pane, &["n"])?;
    journey.wait_terms(&pane, &["What should the new agent work on?", "Claude"])?;
    journey.type_text(&pane, HELLO)?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| chat_of(frame).is_some() && at_rest(frame, &[HELLO, READY]),
        "the first reply",
    )?;
    let created = journey.wait_inventory(
        "desk",
        |agents| ids(agents).difference(&before).count() == 1,
        "created",
    )?;
    let id = ids(&created)
        .difference(&before)
        .next()
        .cloned()
        .unwrap_or_default();
    let made = listed(&created, &id).cloned().unwrap_or_default();
    if made["lifecycle"] != LIVE {
        return Err(format!("the desk lists the new agent as {made}"));
    }
    // Created without a name, it has the word pair its host gave it.
    let name = text(&made["name"]);
    if name.is_empty() {
        return Err(format!("the desk gave the new agent no name: {made}"));
    }
    journey.assigned.push(name.clone());
    journey.frame(&pane, "created")?;

    // Renamed in place on home: the host lists the same agent under the name.
    journey.home(&pane)?;
    journey.select_agent(&pane, &name)?;
    journey.keys(&pane, &["r", "C-u"])?;
    journey.type_text(&pane, "helper")?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| agent_row(frame, "helper").is_some(),
        "helper on home",
    )?;
    let renamed = journey.wait_inventory(
        "desk",
        |agents| listed(agents, &id).is_some_and(|agent| agent["name"] == "helper"),
        "renamed",
    )?;
    lifecycle(&renamed, &id, "helper", LIVE)?;

    // Stopped: exited on the host with its history kept, and resumable.
    journey.select_agent(&pane, "helper")?;
    journey.keys(&pane, &["x"])?;
    journey.wait_terms(&pane, &["Stop helper? It can be resumed later."])?;
    journey.frame(&pane, "stop-asked")?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| frame.contains("▸ Exited 1 ") && agent_row(frame, "helper").is_none(),
        "helper exited",
    )?;
    let stopped = journey.wait_inventory(
        "desk",
        |agents| listed(agents, &id).is_some_and(|agent| agent["lifecycle"] == EXITED),
        "stopped",
    )?;
    lifecycle(&stopped, &id, "helper", EXITED)?;
    let control = negative_control(lifecycle(&stopped, &id, "helper", LIVE))?;
    let kept = journey.chat("desk", &id, Some("history-kept"))?;
    reflected_once(&kept, HELLO)?;
    if !holds(&kept, READY) {
        return Err(format!(
            "the stopped agent's history lost its reply: {kept}"
        ));
    }
    journey.open_exited(&pane)?;
    journey.open_chat(&pane, "helper")?;
    journey.wait_terms(&pane, &[HELLO, READY, "enter resume"])?;
    journey.frame(&pane, "exited-and-resumable")?;

    // Resumed from the exited composer: the same identity, live again.
    journey.type_text(&pane, BACK)?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| frame.matches(READY).count() == 2 && !working(frame),
        "the resumed reply",
    )?;
    let resumed = journey.wait_inventory(
        "desk",
        |agents| listed(agents, &id).is_some_and(|agent| agent["lifecycle"] == LIVE),
        "resumed",
    )?;
    lifecycle(&resumed, &id, "helper", LIVE)?;
    let resumed = journey.wait_chat(
        "desk",
        &id,
        |chat| prompts(chat, BACK).len() == 1,
        "resumed-chat",
    )?;
    reflected_once(&resumed, HELLO)?;
    reflected_once(&resumed, BACK)?;
    journey.frame(&pane, "resumed")?;

    // Deleted, explicitly: only an exited agent offers it, so stopped
    // again first, then gone from home and from the host.
    journey.home(&pane)?;
    journey.select_agent(&pane, "helper")?;
    journey.keys(&pane, &["x"])?;
    journey.wait_terms(&pane, &["Stop helper? It can be resumed later."])?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait_inventory(
        "desk",
        |agents| listed(agents, &id).is_some_and(|agent| agent["lifecycle"] == EXITED),
        "stopped-again",
    )?;
    if journey
        .wait_terms(&pane, &["Exited "])?
        .contains("▸ Exited ")
    {
        journey.open_exited(&pane)?;
    }
    journey.select_agent(&pane, "helper")?;
    journey.keys(&pane, &["x"])?;
    journey.wait_terms(&pane, &["Delete helper and its history?"])?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| agent_row(frame, "helper").is_none(),
        "home without helper",
    )?;
    journey.wait_inventory("desk", |agents| listed(agents, &id).is_none(), "deleted")?;
    journey.frame(&pane, "deleted")?;
    journey.quit_client(&pane)?;
    let mut proved = lines(&[
        "n, a first prompt and enter started a headless Claude the desk lists, and its chat opened on the reply",
        "r renamed it in place; the desk lists the same id as helper",
        "x stopped it: the desk lists it exited, with its prompt and reply kept",
    ]);
    proved.push(control);
    proved.extend(lines(&[
        "a message from the exited composer resumed the same id, and each prompt is in the desk's chat once",
        "stopped again, x on its exited row deleted it: gone from home and from the desk",
        "the client exited 0",
    ]));
    Ok(proved)
}

/// The laptop names the desk by the desk's own host id.
fn identified(frame: &str, host_id: &str) -> Result<(), String> {
    if frame.contains(&format!("Paired with desk ({host_id}) on this network.")) {
        Ok(())
    } else {
        Err(format!("the pairing did not name desk {host_id}:\n{frame}"))
    }
}

fn reach_host(journey: &mut TerminalJourney) -> Proved {
    let desk_id = journey.host_id("desk");
    let prompt = "Hello from the laptop.";
    // Unpaired: the laptop finds the desk and says how to pair with it.
    let pane = journey.launch("terminal", "laptop", &[])?;
    journey.wait(&pane, at_home, "home")?;
    journey.keys(&pane, &["h"])?;
    journey.wait_terms(&pane, &["desk", "found nearby · amux pair desk"])?;
    journey.frame(&pane, "found-not-paired")?;
    let hosts = journey.launch("hosts", "laptop", &["hosts"])?;
    let listing = journey.wait_terms(&hosts, &["AMUX_EXIT_0"])?;
    let found = [
        "desk", &desk_id, "found", "nearby", "no:", "amux", "pair", "desk",
    ];
    if !listing
        .lines()
        .any(|line| line.split_whitespace().eq(found.iter().copied()))
    {
        return Err(format!(
            "hosts does not list the desk by its id as found:\n{listing}"
        ));
    }

    // Six digits the desk printed pair the laptop with it.
    let opened = journey.launch("pairing", "desk", &["pair"])?;
    let printed = journey.wait_terms(&opened, &["Pairing PIN:", "Ctrl+C closes it."])?;
    let pin = pin_in(&printed).ok_or_else(|| format!("no PIN on the desk:\n{printed}"))?;
    let asking = journey.launch("pair", "laptop", &["pair", "desk"])?;
    journey.wait_terms(&asking, &["Pairing PIN shown on the other host:"])?;
    journey.type_text(&asking, &pin)?;
    journey.keys(&asking, &["Enter"])?;
    let paired = journey.wait_terms(&asking, &["Paired with desk", "AMUX_EXIT_0"])?;
    identified(&paired, &desk_id)?;
    let control = negative_control(identified(&paired, "00000000-0000-0000-0000-000000000000"))?;
    journey.wait_terms(&opened, &["Pairing mode ended.", "AMUX_EXIT_0"])?;
    let desk_hosts = journey.launch("desk-hosts", "desk", &["hosts"])?;
    let trusted = journey.wait_terms(&desk_hosts, &["AMUX_EXIT_0"])?;
    let laptop_id = journey.host_id("laptop");
    let lists_laptop = trusted.lines().any(|line| {
        let words: Vec<&str> = line.split_whitespace().collect();
        words.len() >= 3
            && words[0] == "laptop"
            && words[1] == laptop_id
            && words.last() == Some(&"yes")
    });
    if !lists_laptop {
        return Err(format!(
            "the desk does not list the laptop as paired:\n{trusted}"
        ));
    }

    // The running client's overlay says so, and the desk's work is there.
    journey.wait(
        &pane,
        |frame| !frame.contains("found nearby") && spaced(frame, "desk", "direct"),
        "the desk paired in the overlay",
    )?;
    journey.frame(&pane, "paired")?;
    journey.keys(&pane, &["Escape"])?;
    journey.open_chat(&pane, "desk-work")?;
    journey.type_text(&pane, prompt)?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| at_rest(frame, &["The desk is reachable."]),
        "the desk's reply",
    )?;
    let heard = journey.wait_chat(
        "desk",
        "desk-work",
        |chat| phase(chat) == "IDLE" && prompts(chat, prompt).len() == 1,
        "desk-heard",
    )?;
    reflected_once(&heard, prompt)?;
    journey.frame(&pane, "usable-chat")?;
    journey.quit_client(&pane)?;
    Ok(vec![
        "the unpaired laptop listed the desk as found, with `amux pair desk`, and `amux hosts` named it by its id"
            .to_owned(),
        format!("the PIN the desk printed paired the laptop, which named desk {desk_id}"),
        control,
        "the desk lists the laptop as paired".to_owned(),
        "the running client's overlay showed the desk online and direct without a restart".to_owned(),
        format!(
            "the desk's agent opened from the laptop; the desk holds {} once and its reply",
            quoted(prompt)
        ),
        "the client left the chat and exited 0".to_owned(),
    ])
}

/// The one prompt carrying the paste and the review, as the desk holds it.
fn reviewed(chat: &Value, comment: &str) -> Result<Value, String> {
    let sent: Vec<&Value> = items(chat)
        .iter()
        .filter(|item| {
            item["attachments"]
                .as_array()
                .is_some_and(|attached| !attached.is_empty())
        })
        .collect();
    let [item] = sent.as_slice() else {
        return Err(format!(
            "the desk holds {} prompts with attachments",
            sent.len()
        ));
    };
    let attachments = item["attachments"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let kinds: Vec<&Value> = attachments
        .iter()
        .map(|attachment| &attachment["kind"])
        .collect();
    if item["text"] != "Please check \u{fffc} against \u{fffc}"
        || kinds != [&json!("text"), &json!("review")]
    {
        return Err(format!(
            "the prompt's order is not text, paste, text, review: {item}"
        ));
    }
    if attachments[0]["text"] != pasted() {
        return Err("the pasted text arrived changed".to_owned());
    }
    let comments = &attachments[1]["comments"];
    if *comments != json!([{"path": "deploy.sh", "line": 3, "old_line": 0, "text": comment}]) {
        return Err(format!("the review's comments are {comments}"));
    }
    Ok((*item).clone())
}

fn attachment_or_review(journey: &mut TerminalJourney) -> Proved {
    // The reviewer's working tree on the desk: one commit and an edit.
    review_repository(&journey.ready.root.join("desk/work"))?;

    // On the laptop: text, a long paste as one token, text, and a review of
    // the desk's working tree with one comment.
    let pane = journey.launch("terminal", "laptop", &[])?;
    journey.open_chat(&pane, "reviewer")?;
    journey.type_text(&pane, "Please check ")?;
    journey.paste(&pane, &pasted())?;
    journey.wait_terms(&pane, &["Please check [pasted-1 · 12 lines]"])?;
    // The chip is followed by a space of its own.
    journey.type_text(&pane, "against ")?;
    journey.keys(&pane, &["C-a", "r"])?;
    journey.wait_terms(&pane, &["review · working tree at", "deploy.sh  +2 −1"])?;
    let mut on_line = false;
    for _ in 0..12 {
        if spaced(&journey.capture(&pane)?, "│▌", "3 + rsync --delete") {
            on_line = true;
            break;
        }
        journey.keys(&pane, &["j"])?;
    }
    if !on_line {
        return Err(format!(
            "no line to comment on:\n{}",
            journey.capture(&pane)?
        ));
    }
    journey.frame(&pane, "review-diff")?;
    journey.keys(&pane, &["c"])?;
    journey.type_text(&pane, COMMENT)?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait_terms(
        &pane,
        &["1 file · +2 −1 · 1 comment", &format!("│ {COMMENT}")],
    )?;
    journey.frame(&pane, "review-comment")?;
    journey.keys(&pane, &["q"])?;
    journey.wait_terms(
        &pane,
        &["Please check [pasted-1 · 12 lines] against [review · 1 comment]"],
    )?;
    journey.frame(&pane, "composer-tokens")?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| at_rest(frame, &["I read the review."]),
        "the reviewer's reply",
    )?;

    // The desk received the tokens in order, the paste's exact text, the
    // comment, and the patch it made itself, whose bytes are on its disk.
    let received = journey.wait_chat(
        "desk",
        "reviewer",
        |chat| {
            phase(chat) == "IDLE"
                && items(chat).iter().any(|item| {
                    item["attachments"]
                        .as_array()
                        .is_some_and(|attached| !attached.is_empty())
                })
        },
        "received",
    )?;
    let item = reviewed(&received, COMMENT)?;
    let control =
        negative_control(reviewed(&received, &format!("{COMMENT} (a wrong comment)")).map(drop))?;
    patch_on_disk(&journey.ready.root, &text(&item["attachments"][1]["patch"]))?;

    // Revisited: the chat opened again shows the paste's opening lines in
    // place and the review as its token.
    journey.home(&pane)?;
    journey.open_chat(&pane, "reviewer")?;
    journey.wait_terms(
        &pane,
        &[
            "Please check",
            "deploy log 0:",
            "against [review · 1 comment]",
            "I read the review.",
        ],
    )?;
    journey.frame(&pane, "reopened")?;
    journey.quit_client(&pane)?;
    Ok(vec![
        "the prompt reached the desk as text, the paste, text and the review, in that order".to_owned(),
        "the paste arrived as one token holding exactly the twelve lines pasted".to_owned(),
        format!("the review carries one comment on deploy.sh line 3: {}", quoted(COMMENT)),
        control,
        "the review names a patch the desk computed; its bytes on the desk hash to that name and hold the edit"
            .to_owned(),
        "the chat opened again shows the paste in place, the review token and the reply".to_owned(),
        "the client exited 0".to_owned(),
    ])
}

fn keep_authority(journey: &mut TerminalJourney) -> Proved {
    let asked = "Are you there?";
    let held = "Still there?";
    // The two machines are one account's, reached only through the relay.
    journey.request(trust("desk", "laptop"), None)?;
    journey.request(trust("laptop", "desk"), None)?;

    // A second profile on the laptop is a fleet of its own.
    let created = journey.launch("profile", "laptop", &["profile", "new", "work"])?;
    journey.wait_terms(&created, &["Created profile work", "AMUX_EXIT_0"])?;
    let other = journey.launch("other", "laptop", &["--profile", "work"])?;
    journey.wait_terms(&other, &["no agents yet"])?;
    journey.keys(&other, &["h"])?;
    journey.wait(
        &other,
        |frame| spaced(frame, "laptop", "this machine") && !frame.contains("desk"),
        "hosts without the desk",
    )?;
    journey.frame(&other, "other-profile")?;
    journey.keys(&other, &["Escape"])?;
    journey.quit_client(&other)?;

    // The default profile reaches the desk's agent through the relay.
    let pane = journey.launch("terminal", "laptop", &[])?;
    journey.open_chat(&pane, "guarded")?;
    journey.type_text(&pane, asked)?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| at_rest(frame, &["Only this account reaches me."]),
        "the first reply",
    )?;
    let reached = journey.wait_chat("desk", "guarded", |chat| phase(chat) == "IDLE", "reached")?;
    reflected_once(&reached, asked)?;

    // Signed out, the laptop has no way to the desk: the chat says so and
    // a message written now is held, never sent.
    let signed_out = journey.launch("logout", "laptop", &["logout"])?;
    journey.wait_terms(&signed_out, &["Signed default out", "AMUX_EXIT_0"])?;
    // The reason named is this machine's sign-out, never a claim about the
    // desk, and the hosts overlay says the same.
    journey.wait_terms(
        &pane,
        &[
            "desk away · this machine is signed out",
            "it sends once this machine signs in",
        ],
    )?;
    journey.home(&pane)?;
    journey.keys(&pane, &["h"])?;
    journey.wait_terms(
        &pane,
        &[
            "signed out · amux login",
            "offline · this machine is signed out",
        ],
    )?;
    journey.frame(&pane, "blocked-hosts")?;
    journey.keys(&pane, &["Escape"])?;
    journey.open_chat(&pane, "guarded")?;
    journey.wait_terms(&pane, &["desk away · this machine is signed out"])?;
    journey.type_text(&pane, held)?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait_terms(&pane, &[held, "sends when desk is back"])?;
    journey.frame(&pane, "blocked")?;
    std::thread::sleep(Duration::from_secs(2));
    let blocked = journey.chat("desk", "guarded", Some("while-signed-out"))?;
    never_received(&blocked, held)?;
    let control = negative_control(never_received(&blocked, asked))?;

    // Signed in again: the chat is current and the held message goes by
    // itself, once.
    journey.request(
        Control::SignIn {
            host: "laptop".to_owned(),
            account: "ada".to_owned(),
        },
        None,
    )?;
    journey.wait(
        &pane,
        |frame| {
            !frame.contains("not current")
                && at_rest(frame, &[held, "The relay carries us again."])
                && !frame.contains("sends when")
        },
        "the reply after signing in",
    )?;
    let after = journey.wait_chat(
        "desk",
        "guarded",
        |chat| phase(chat) == "IDLE" && prompts(chat, held).len() == 1,
        "sent-after-sign-in",
    )?;
    reflected_once(&after, asked)?;
    reflected_once(&after, held)?;
    journey.frame(&pane, "recovered")?;
    journey.quit_client(&pane)?;
    Ok(vec![
        "a second profile on the laptop opened on its own empty fleet, knowing no desk".to_owned(),
        format!("through the relay the desk received {} once and answered", quoted(asked)),
        "signed out, the laptop named its own sign-out as the reason the desk was away, in the chat and the hosts overlay, and held the message; the desk never received it".to_owned(),
        control,
        format!(
            "signed in again, the chat was current and the held {} went by itself, reached the desk once and was answered",
            quoted(held)
        ),
        "the clients exited 0".to_owned(),
    ])
}

const PLAN_FILE: &str = ".claude/plans/";

/// The screen shows no step writing the plan file, open or folded into
/// the steps before the plan.
fn no_plan_file(frame: &str) -> Result<(), String> {
    if frame.contains(PLAN_FILE) || frame.contains("move-the-journal.md") || counts_edits(frame) {
        Err(format!("the plan file shows as a step:\n{frame}"))
    } else {
        Ok(())
    }
}

fn decide_plan(journey: &mut TerminalJourney, agent: &str, headless: bool) -> Proved {
    let pane = journey.launch("terminal", "laptop", &[])?;
    journey.open_chat(&pane, agent)?;
    journey.type_text(&pane, PLAN_PROMPT)?;
    journey.keys(&pane, &["Enter"])?;
    no_plan_file(&journey.read_ask(
        &pane,
        &[
            "Plan ready",
            "3. Delete the old journal.",
            "No, keep planning",
        ],
    )?)?;
    journey.frame(&pane, "plan")?;
    // Esc reaches the way out; Tab opens the note it carries.
    journey.keys(&pane, &["Escape", "Tab"])?;
    journey.type_text(&pane, PLAN_NOTE)?;
    journey.wait_terms(&pane, &[&format!("No, keep planning: {PLAN_NOTE}")])?;
    journey.keys(&pane, &["Enter"])?;
    no_plan_file(&journey.read_ask(
        &pane,
        &[
            "sent back",
            PLAN_NOTE,
            "4. Keep the old journal for a week.",
            "Plan ready",
        ],
    )?)?;
    journey.frame(&pane, "revised")?;
    journey.keys(&pane, &["1"])?;
    journey.wait(
        &pane,
        |frame| at_rest(frame, &["approved", PLAN_DONE]),
        "the approved plan's work",
    )?;
    let settled = journey.wait_chat(
        "desk",
        agent,
        |chat| answered_with(chat, PLAN_DONE),
        "plan-settled",
    )?;
    reflected_once(&settled, PLAN_PROMPT)?;
    plans_only(&settled)?;
    let mut extra = items(&settled).to_vec();
    extra.push(json!({"key": "toolu_write", "text": ""}));
    let mut proved = vec![
        format!("{} reflected once in the desk's chat", quoted(PLAN_PROMPT)),
        "the plan showed in the feed with its decision in the composer's box".to_owned(),
        "sent back with a note, the revised plan arrived; approved, the work ran".to_owned(),
        "the plan file Claude wrote before each plan showed as no step, on screen or on the desk"
            .to_owned(),
        negative_control(plans_only(&json!({ "items": extra })))?,
    ];
    if headless {
        let decisions = control_responses(&journey.provider_input(agent, "plan-decisions")?)?;
        sent_back_then_approved(&decisions, PLAN_NOTE)?;
        proved
            .push("Claude received the plan sent back with the note, then one approval".to_owned());
        proved.push(negative_control(sent_back_then_approved(
            &decisions,
            "a note never written",
        ))?);
    }
    journey.frame(&pane, "approved")?;
    journey.quit_client(&pane)?;
    proved.push("the client exited 0".to_owned());
    Ok(proved)
}

fn decide_plan_codex(journey: &mut TerminalJourney) -> Proved {
    let agent = "planner-codex";
    let pane = journey.launch("terminal", "laptop", &[])?;
    journey.open_chat(&pane, agent)?;
    // Shift+Tab puts Codex in plan mode before the prompt.
    journey.keys(&pane, &["BTab"])?;
    journey.wait_terms(&pane, &["· plan ─╯"])?;
    journey.type_text(&pane, PLAN_PROMPT)?;
    journey.keys(&pane, &["Enter"])?;
    // The plan is in the feed while the turn still runs.
    journey.wait(
        &pane,
        |frame| {
            frame.contains("3. Delete the old journal.")
                && working(frame)
                && !frame.contains("Plan ready")
        },
        "the plan streaming",
    )?;
    journey.frame(&pane, "plan-streaming")?;
    journey.request(open_gate("plan-streamed"), None)?;
    journey.read_ask(
        &pane,
        &[
            "Plan ready",
            "1. Yes, implement this plan",
            "No, keep planning",
        ],
    )?;
    journey.wait_chat(
        "desk",
        agent,
        |chat| phase(chat) == "NEEDS_YOU",
        "plan-needs-you",
    )?;
    journey.frame(&pane, "plan-ready")?;
    journey.keys(&pane, &["1"])?;
    journey.wait(
        &pane,
        |frame| at_rest(frame, &["approved", PLAN_DONE]),
        "the implemented plan's work",
    )?;
    // A second plan, sent back with a note: Codex plans again on it.
    journey.keys(&pane, &["BTab"])?;
    journey.wait_terms(&pane, &["· plan ─╯"])?;
    journey.type_text(&pane, ROTATE_PROMPT)?;
    journey.keys(&pane, &["Enter"])?;
    journey.read_ask(&pane, &["Plan ready", "2. Delete logs older than a week."])?;
    journey.keys(&pane, &["Escape", "Tab"])?;
    journey.type_text(&pane, ROTATE_NOTE)?;
    journey.wait_terms(&pane, &[&format!("No, keep planning: {ROTATE_NOTE}")])?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait_terms(
        &pane,
        &[
            "sent back",
            ROTATE_NOTE,
            "3. Delete logs older than a month.",
            "Plan ready",
        ],
    )?;
    journey.wait_chat(
        "desk",
        agent,
        |chat| phase(chat) == "NEEDS_YOU" && prompts(chat, ROTATE_NOTE).len() == 1,
        "revised",
    )?;
    journey.frame(&pane, "sent-back")?;
    let expected = [
        ("plan", PLAN_PROMPT),
        ("default", IMPLEMENT),
        ("plan", ROTATE_PROMPT),
        ("plan", ROTATE_NOTE),
    ];
    let turns = codex_turns(&journey.provider_input(agent, "plan-turns")?)?;
    codex_plan_turns(&turns, &expected)?;
    let wrong = [
        expected[0],
        expected[1],
        expected[2],
        ("default", ROTATE_NOTE),
    ];
    let control = negative_control(codex_plan_turns(&turns, &wrong))?;
    journey.quit_client(&pane)?;
    Ok(vec![
        "the plan showed in the feed while Codex's turn still ran".to_owned(),
        "when the turn ended the plan's decision took the composer's box and the desk said the agent needs you"
            .to_owned(),
        format!(
            "implement started a turn out of plan mode with {}, and its work ran",
            quoted(IMPLEMENT)
        ),
        "a second plan, sent back with a note, came back revised; the note went as the next prompt, still in plan mode".to_owned(),
        "Codex was started on the prompt and the note in plan mode and on the implement turn in default mode"
            .to_owned(),
        control,
        "the client exited 0".to_owned(),
    ])
}

fn answer_questions(journey: &mut TerminalJourney) -> Proved {
    let pane = journey.launch("terminal", "laptop", &[])?;
    journey.open_chat(&pane, "asker")?;
    journey.type_text(&pane, "Add a settings screen.")?;
    journey.keys(&pane, &["Enter"])?;
    // The first option's preview beside the options.
    journey.read_ask(
        &pane,
        &[
            "How should the settings screen be laid out?",
            "1. Sidebar",
            "│ Address  relay.amux.dev",
        ],
    )?;
    journey.frame(&pane, "first-question")?;
    journey.keys(&pane, &["Down"])?;
    journey.wait_terms(&pane, &["Relay ──────────", "Palette  terminal"])?;
    journey.frame(&pane, "other-preview")?;
    journey.keys(&pane, &["2"])?;
    journey.wait_terms(
        &pane,
        &["Where should the screen open from?", "3. Something else"],
    )?;
    journey.keys(&pane, &["3"])?;
    journey.type_text(&pane, OTHER)?;
    journey.wait_terms(&pane, &[&format!("Something else: {OTHER}")])?;
    journey.keys(&pane, &["Enter"])?;
    // The third is skipped from the row under the options.
    journey.wait_terms(&pane, &[SKIPPED, "Skip", "Reply instead"])?;
    journey.keys(&pane, &["Down", "Down", "Down"])?;
    journey.wait_terms(&pane, &["› Skip"])?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait_terms(
        &pane,
        &[
            "Send answers",
            "→ Stacked",
            &format!("→ {OTHER}"),
            "→ skipped",
        ],
    )?;
    journey.frame(&pane, "review")?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| {
            at_rest(
                frame,
                &[
                    "Answered 2 of 3 questions",
                    "Thanks, I'll build it that way.",
                ],
            )
        },
        "the reply",
    )?;
    journey.frame(&pane, "answered")?;
    // A second ask, replied to instead: Esc points at Reply instead.
    journey.type_text(&pane, PALETTE)?;
    journey.keys(&pane, &["Enter"])?;
    journey.read_ask(
        &pane,
        &["Which palette should it open in?", "Reply instead"],
    )?;
    journey.keys(&pane, &["Escape"])?;
    journey.wait_terms(&pane, &["› Reply instead"])?;
    journey.keys(&pane, &["Enter"])?;
    journey.type_text(&pane, QUESTION_REPLY)?;
    journey.wait_terms(&pane, &[&format!("Reply instead: {QUESTION_REPLY}")])?;
    journey.frame(&pane, "reply-instead")?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| {
            at_rest(
                frame,
                &[QUESTION_REPLY, "I'll leave the palette for later."],
            )
        },
        "the reply to the reply",
    )?;
    journey.frame(&pane, "replied")?;
    let decisions = control_responses(&journey.provider_input("asker", "answers")?)?;
    answered(&decisions, &answers())?;
    replied(&decisions, QUESTION_REPLY)?;
    let mut wrong = answers();
    wrong[SKIPPED] = json!("Relay");
    let controls = [
        negative_control(answered(&decisions, &wrong))?,
        negative_control(replied(&decisions, "Pick Terminal."))?,
    ];
    journey.quit_client(&pane)?;
    let mut proved = lines(&[
        "each option's preview showed beside the options as the selection moved",
        "a picked option, a typed answer under Something else and a skipped question showed together for review",
        "Claude received one answer naming Stacked and the typed text, with the skipped question left out",
        "a second ask, replied to instead, reached Claude as the question refused with the person's words",
    ]);
    proved.extend(controls);
    proved.push("the client exited 0".to_owned());
    Ok(proved)
}

fn tool_server_asks(journey: &mut TerminalJourney) -> Proved {
    let pane = journey.launch("terminal", "laptop", &[])?;
    // A tool server's form, field by field in the order the server wrote.
    journey.open_chat(&pane, "filer")?;
    journey.type_text(&pane, "File the reconnect flake.")?;
    journey.keys(&pane, &["Enter"])?;
    journey.read_ask(&pane, &["linear needs details", "Title · required"])?;
    journey.frame(&pane, "form")?;
    journey.type_text(&pane, FORM_TITLE)?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait_terms(&pane, &["Team · required", "1. FOX"])?;
    journey.keys(&pane, &["1"])?;
    journey.wait_terms(&pane, &["Estimate"])?;
    journey.type_text(&pane, "3")?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait_terms(
        &pane,
        &["Submit", &format!("→ {FORM_TITLE}"), "→ FOX", "→ 3"],
    )?;
    journey.frame(&pane, "form-review")?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| at_rest(frame, &["Sent the form to linear", "Filed it in Linear."]),
        "the filed reply",
    )?;
    let decisions = control_responses(&journey.provider_input("filer", "form-answer")?)?;
    form_sent(&decisions, &form())?;
    let mut wrong = form();
    wrong["team"] = json!("CORE");
    let control = negative_control(form_sent(&decisions, &wrong))?;

    // A link: Codex asks to let the server's tool run, then the server
    // sends a link, worded from its own message.
    journey.home(&pane)?;
    journey.open_chat(&pane, "linker")?;
    journey.type_text(&pane, "Check the relay's error rate.")?;
    journey.keys(&pane, &["Enter"])?;
    journey.read_ask(&pane, &["Wants to use", "grafana ask"])?;
    journey.keys(&pane, &["1"])?;
    journey.read_ask(
        &pane,
        &[
            "grafana sent a link",
            "https://grafana.example.com/login",
            "Done",
        ],
    )?;
    journey.frame(&pane, "link")?;
    journey.keys(&pane, &["Down", "Enter"])?;
    journey.wait(
        &pane,
        |frame| {
            at_rest(
                frame,
                &[
                    "Opened grafana's link",
                    "I'll read the dashboards another way.",
                ],
            )
        },
        "the reply",
    )?;
    let results = codex_results(&journey.provider_input("linker", "link-answers")?)?;
    let actions: Vec<&Value> = results.iter().map(|result| &result["action"]).collect();
    if actions != [&json!("accept"), &json!("accept")] || !results[1]["content"].is_null() {
        return Err(format!("Codex was answered {results:?}"));
    }
    journey.frame(&pane, "link-opened")?;
    journey.quit_client(&pane)?;
    Ok(vec![
        "the form asked for title, team and estimate in the server's order and showed them for review".to_owned(),
        format!(
            "Claude received the form accepted with {{'title': {}, 'team': 'FOX', 'estimate': 3}}",
            quoted(FORM_TITLE)
        ),
        control,
        "Codex's ask to run the server's tool was allowed, then the link showed its message and address".to_owned(),
        "'Done' answered the link: Codex received two accepts, the link's with no content".to_owned(),
        "the client exited 0".to_owned(),
    ])
}

fn queue_and_steer(journey: &mut TerminalJourney) -> Proved {
    let pane = journey.launch("terminal", "laptop", &[])?;
    journey.open_chat(&pane, "worker")?;
    journey.type_text(&pane, RUN)?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait_terms(
        &pane,
        &["Running scripts/soak --for 1h", "ctrl+x interrupt"],
    )?;
    // While it works, prompts queue under the feed.
    journey.type_text(&pane, STEER)?;
    journey.keys(&pane, &["Enter"])?;
    journey.type_text(&pane, WITHDRAWN)?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| spaced(frame, WITHDRAWN, "queued"),
        "two queued",
    )?;
    journey.frame(&pane, "queued")?;
    // ↑ reaches the newest; withdrawn, it is a draft again, and cleared.
    journey.keys(&pane, &["Up"])?;
    journey.wait_terms(&pane, &["[Send Now] [Withdraw]", "backspace withdraw"])?;
    journey.keys(&pane, &["BSpace"])?;
    journey.wait_terms(&pane, &[&format!("› {WITHDRAWN}")])?;
    journey.keys(&pane, &["C-c"])?;
    journey.wait(
        &pane,
        |frame| !frame.contains(WITHDRAWN),
        "the withdrawn prompt cleared",
    )?;
    // The other goes into the turn now.
    journey.keys(&pane, &["Up"])?;
    journey.wait_terms(&pane, &["[Send Now] [Withdraw]"])?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait_terms(&pane, &["sending into this turn"])?;
    journey.request(open_gate("run-done"), None)?;
    journey.wait(
        &pane,
        |frame| at_rest(frame, &["steered", "The run finished."]),
        "the turn's end",
    )?;
    let settled = journey.wait_chat("desk", "worker", |chat| phase(chat) == "IDLE", "settled")?;
    reflected_once(&settled, RUN)?;
    reflected_once(&settled, STEER)?;
    never_sent(&settled, WITHDRAWN)?;
    let control = negative_control(never_sent(&settled, STEER))?;
    journey.frame(&pane, "steered")?;
    journey.quit_client(&pane)?;
    Ok(vec![
        "two prompts sent while the agent worked queued under the feed".to_owned(),
        "withdrawn, the newest came back to the composer as a draft and was cleared; the desk never received it"
            .to_owned(),
        format!(
            "sent now, {} went into the running turn and the desk holds it once",
            quoted(STEER)
        ),
        control,
        "the client exited 0".to_owned(),
    ])
}

const NOT_SENT: &str = "Rotate the backup keys.";
const MAYBE: &str = "Prune the old snapshots.";
fn send_while_away(journey: &mut TerminalJourney) -> Proved {
    let pane = journey.launch("terminal", "laptop", &[])?;
    journey.open_chat(&pane, "backups")?;
    journey.request(sever("cabin", "laptop"), None)?;
    journey.wait_terms(&pane, &["cabin away"])?;
    journey.type_text(&pane, BACKUP)?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait_terms(&pane, &[BACKUP, "sends when cabin is back"])?;
    journey.frame(&pane, "away")?;
    std::thread::sleep(Duration::from_secs(2));
    let away = journey.chat("cabin", "backups", Some("while-away"))?;
    never_sent(&away, BACKUP)?;
    journey.request(restore("cabin", "laptop"), None)?;
    journey.wait(
        &pane,
        |frame| at_rest(frame, &[BACKUP, BACKUP_DONE]) && !frame.contains("sends when"),
        "the reply",
    )?;
    let heard = journey.wait_chat(
        "cabin",
        "backups",
        |chat| phase(chat) == "IDLE" && prompts(chat, BACKUP).len() == 1,
        "sent",
    )?;
    reflected_once(&heard, BACKUP)?;
    let control = negative_control(never_sent(&heard, BACKUP))?;
    journey.frame(&pane, "sent")?;
    journey.quit_client(&pane)?;
    Ok(vec![
        "with the cabin away, Enter held the prompt to send when the cabin is back, and the cabin never received it"
            .to_owned(),
        format!(
            "with the cabin back, the held prompt went by itself and the cabin holds {} once",
            quoted(BACKUP)
        ),
        control,
        "the client exited 0".to_owned(),
    ])
}

fn send_rejected(journey: &mut TerminalJourney) -> Proved {
    let pane = journey.launch("terminal", "laptop", &[])?;
    journey.open_chat(&pane, "backups")?;
    // The laptop can no longer open a call to the cabin while the cabin's
    // chat stays live: a send fails before it leaves the laptop.
    journey.request(refuse_calls("laptop", "cabin", true), None)?;
    journey.type_text(&pane, NOT_SENT)?;
    journey.wait_terms(&pane, &[NOT_SENT])?;
    journey.keys(&pane, &["Enter"])?;
    // The failed call closes the link it could not open a stream on, so
    // the cabin reads offline at once (the chat words it "cabin away")
    // rather than online while nothing reaches it.
    journey.wait_terms(&pane, &["cabin away", NOT_SENT, "sends when cabin is back"])?;
    journey.frame(&pane, "not-sent")?;
    std::thread::sleep(Duration::from_secs(2));
    let refused = journey.chat("cabin", "backups", Some("refused"))?;
    never_sent(&refused, NOT_SENT)?;
    // Once a link to the cabin is up again it reads online, and the held
    // prompt goes by itself.
    journey.request(refuse_calls("laptop", "cabin", false), None)?;
    journey.wait(
        &pane,
        |frame| at_rest(frame, &[NOT_SENT, BACKUP_DONE]) && !frame.contains("sends when"),
        "the reply",
    )?;
    let heard = journey.wait_chat(
        "cabin",
        "backups",
        |chat| phase(chat) == "IDLE" && prompts(chat, NOT_SENT).len() == 1,
        "sent",
    )?;
    reflected_once(&heard, NOT_SENT)?;
    let control = negative_control(never_sent(&heard, NOT_SENT))?;
    journey.frame(&pane, "sent")?;
    journey.quit_client(&pane)?;
    Ok(vec![
        "a send that could not leave the laptop was held, to send when the cabin is back, and the cabin read as out of reach".to_owned(),
        "the cabin never received it".to_owned(),
        format!(
            "once the cabin was back the held prompt went by itself, and the cabin holds {} once",
            quoted(NOT_SENT)
        ),
        control,
        "the client exited 0".to_owned(),
    ])
}

fn send_may_not_have_arrived(journey: &mut TerminalJourney) -> Proved {
    let pane = journey.launch("terminal", "laptop", &[])?;
    journey.open_chat(&pane, "backups")?;
    // The cabin's agent takes the prompt in and answers nothing while
    // frozen, so the send waits on the cabin when the link is cut.
    journey.request(
        Control::Freeze {
            agent: "backups".to_owned(),
        },
        None,
    )?;
    journey.type_text(&pane, MAYBE)?;
    journey.wait_terms(&pane, &[MAYBE])?;
    journey.keys(&pane, &["Enter"])?;
    // Sent: the composer is empty again. The forwarded call reaches the
    // cabin at once and waits there for an answer that does not come; it
    // would give up after 30 seconds, so everything until the prompt reads
    // may not have arrived happens well inside that.
    journey.wait_for(
        &pane,
        |frame| frame.contains("› Message backups"),
        "the prompt on its way",
        Duration::from_secs(5),
    )?;
    let sent_at = Instant::now();
    std::thread::sleep(Duration::from_secs(2));
    journey.request(sever("cabin", "laptop"), None)?;
    journey.wait_terms_for(&pane, &["cabin away"], Duration::from_secs(5))?;
    std::thread::sleep(Duration::from_secs(1));
    // The link returns with the agent still frozen: catching up finds the
    // prompt neither queued nor in the chat, so it may not have arrived.
    journey.request(restore("cabin", "laptop"), None)?;
    journey.wait_for(
        &pane,
        |frame| spaced(frame, MAYBE, "may not have arrived") && !frame.contains("cabin away"),
        "may not have arrived",
        Duration::from_secs(10),
    )?;
    if sent_at.elapsed() >= Duration::from_secs(25) {
        return Err(
            "the prompt read may not have arrived only near the answer's time limit, not from the cut".to_owned(),
        );
    }
    journey.frame(&pane, "may-not-have-arrived")?;
    // The agent goes on with what it took in, and the prompt settles with
    // no resend: the cabin had it, once.
    journey.request(
        Control::Thaw {
            agent: "backups".to_owned(),
        },
        None,
    )?;
    journey.wait(
        &pane,
        |frame| at_rest(frame, &[MAYBE, BACKUP_DONE]) && !frame.contains("may not have arrived"),
        "the prompt settled and answered",
    )?;
    let heard = journey.wait_chat(
        "cabin",
        "backups",
        |chat| phase(chat) == "IDLE" && prompts(chat, MAYBE).len() == 1,
        "settled",
    )?;
    reflected_once(&heard, MAYBE)?;
    let control = negative_control(never_sent(&heard, MAYBE))?;
    journey.frame(&pane, "settled")?;
    journey.quit_client(&pane)?;
    Ok(vec![
        "with the link cut while the cabin held the prompt unanswered, the send came back uncertain, not as not sent"
            .to_owned(),
        "with the link back and the cabin's agent still frozen, the prompt read may not have arrived".to_owned(),
        format!(
            "once the agent went on it settled without a resend, and the cabin holds {} once",
            quoted(MAYBE)
        ),
        control,
        "the client exited 0".to_owned(),
    ])
}

fn composer_limits(journey: &mut TerminalJourney) -> Proved {
    let pane = journey.launch("terminal", "laptop", &[])?;
    // At a usage limit the composer says so, and sending still works.
    journey.open_chat(&pane, "limited")?;
    journey.wait_terms(&pane, &["5-hour limit reached · resets"])?;
    journey.frame(&pane, "limit")?;
    journey.type_text(&pane, "Try again.")?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| at_rest(frame, &["Done after the limit."]),
        "the reply past the limit",
    )?;
    let limited = journey.wait_chat(
        "desk",
        "limited",
        |chat| phase(chat) == "IDLE" && prompts(chat, "Try again.").len() == 1,
        "limited-sent",
    )?;
    reflected_once(&limited, "Try again.")?;

    // A refused credential takes the composer's box: Claude must be signed
    // in again where it runs.
    journey.home(&pane)?;
    journey.open_chat(&pane, "expired")?;
    journey.type_text(&pane, "Check the links.")?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait_terms(
        &pane,
        &[
            "Claude needs you to sign in",
            "Run claude and sign in with /login on desk.",
        ],
    )?;
    journey.frame(&pane, "sign-in")?;
    let refused = journey.wait_chat("desk", "expired", |chat| phase(chat) == "IDLE", "refused")?;
    reflected_once(&refused, "Check the links.")?;
    journey.quit_client(&pane)?;
    Ok(lines(&[
        "at its usage limit the composer named the window and when it resets, and a prompt still went and was answered",
        "refused its credential, Claude's chat put sign-in in the composer's box, naming the host to sign in on",
        "the desk holds each prompt once",
        "the client exited 0",
    ]))
}

fn new_agent_terminal(journey: &mut TerminalJourney) -> Proved {
    journey.configure("laptop", "[ui]\nchat_in = \"native\"")?;
    let before = ids(&journey.inventory("laptop", Some("inventory-before"))?);
    let pane = journey.launch("terminal", "laptop", &[])?;
    journey.wait(&pane, at_home, "home")?;
    // On this machine, Start hands the terminal to the new agent.
    journey.keys(&pane, &["n"])?;
    journey.wait_terms(&pane, &["New Agent", "named automatically", "[Start]"])?;
    journey.type_text(&pane, "release-notes")?;
    journey.wait_terms(&pane, &["Name      release-notes"])?;
    journey.frame(&pane, "form")?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait_terms(&pane, &["enter start"])?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait_terms(&pane, &["Claude Code (scripted)"])?;
    journey.type_text(&pane, "Write the notes.")?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait_terms(&pane, &["Ready to write the release notes."])?;
    journey.keys(&pane, &["C-a", "s"])?;
    journey.wait(
        &pane,
        |frame| at_home(frame) && agent_row(frame, "release-notes").is_some(),
        "home with release-notes",
    )?;
    let made = journey.wait_inventory(
        "laptop",
        |agents| agents.iter().any(|agent| agent["name"] == "release-notes"),
        "created-here",
    )?;
    let local = named(&made, "release-notes")?;
    let local_id = text(&local["id"]);
    if before.contains(&local_id) || local["host_id"] != journey.host_id("laptop") {
        return Err(format!("the laptop lists {local}"));
    }
    let typed = journey.wait_chat(
        "laptop",
        &local_id,
        |chat| prompts(chat, "Write the notes.").len() == 1,
        "typed-there",
    )?;
    reflected_once(&typed, "Write the notes.")?;

    // On another machine there is no terminal to hand over: its chat opens.
    journey.keys(&pane, &["n"])?;
    journey.wait_terms(&pane, &["New Agent"])?;
    journey.type_text(&pane, "desk-notes")?;
    journey.keys(&pane, &["Down", "Down", "Down"])?;
    journey.wait_terms(&pane, &["←→ choose"])?;
    // The cabin's providers are signed out: asked what it offers, the
    // cabin says so, and the form names it on each agent.
    journey.keys(&pane, &["Left", "Left"])?;
    journey.wait_terms(&pane, &["Claude · not signed in", "Codex · not signed in"])?;
    journey.frame(&pane, "signed-out-form")?;
    journey.keys(&pane, &["Right"])?;
    journey.wait(
        &pane,
        |frame| !frame.contains("not signed in"),
        "the desk's signed-in agents",
    )?;
    journey.frame(&pane, "remote-form")?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| {
            chat_of(frame).as_deref() == Some("desk-notes")
                && frame.contains("its terminal is on another machine")
        },
        "desk-notes' chat",
    )?;
    journey.frame(&pane, "remote-chat")?;
    let remote = journey.wait_inventory(
        "desk",
        |agents| agents.iter().any(|agent| agent["name"] == "desk-notes"),
        "created-there",
    )?;
    let there = named(&remote, "desk-notes")?;
    if there["host_id"] != journey.host_id("desk") {
        return Err(format!(
            "desk-notes runs on {}, not the desk",
            there["host_id"]
        ));
    }
    journey.quit_client(&pane)?;
    Ok(lines(&[
        "for someone who chats in each agent's own terminal, n opened the New Agent form",
        "Start on this machine handed the terminal to the new Claude; what was typed there reached it once; leader s came back home",
        "on the cabin, whose providers are signed out, the form said so on Claude and Codex; on the desk it did not",
        "Start on the desk created the agent there and opened its chat, saying its terminal is on another machine",
        "the client exited 0",
    ]))
}

/// `git worktree list --porcelain` of `repository`, one map per tree.
fn worktrees(repository: &Path) -> Result<Vec<serde_json::Map<String, Value>>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .map_err(|error| format!("git worktree list: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git worktree list: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim()
        .split("\n\n")
        .map(|block| {
            block
                .lines()
                .filter_map(|line| line.split_once(' '))
                .map(|(key, value)| (key.to_owned(), Value::String(value.to_owned())))
                .collect()
        })
        .collect())
}

/// Picks `label` in an open flyover by the number it is listed under.
fn pick(journey: &mut TerminalJourney, pane: &str, label: &str) -> Result<(), String> {
    let frame = journey.capture(pane)?;
    let number = frame
        .match_indices(&format!(". {label}"))
        .find_map(|(at, found)| {
            let digit = frame[..at]
                .chars()
                .next_back()
                .filter(char::is_ascii_digit)?;
            let bounded = frame[at + found.len()..]
                .chars()
                .next()
                .is_none_or(|c| !word_char(c));
            bounded.then_some(digit)
        });
    let number = number.ok_or_else(|| format!("no flyover lists {}", quoted(label)))?;
    journey.type_text(pane, &number.to_string())
}

fn real(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

/// The one tree of `trees` at `folder`, on `branch`.
fn on_branch(trees: &[serde_json::Map<String, Value>], folder: &Path, branch: &str) -> bool {
    let at: Vec<_> = trees
        .iter()
        .filter(|tree| {
            tree.get("worktree")
                .and_then(Value::as_str)
                .is_some_and(|path| real(Path::new(path)) == folder)
        })
        .collect();
    at.len() == 1
        && at[0].get("branch").and_then(Value::as_str) == Some(&format!("refs/heads/{branch}"))
}

fn new_agent_worktree(journey: &mut TerminalJourney) -> Proved {
    let name = "site-redesign";
    let pane = journey.launch("terminal", "laptop", &[])?;
    journey.wait(&pane, at_home, "home")?;
    journey.keys(&pane, &["n"])?;
    journey.wait_terms(&pane, &["What should the new agent work on?"])?;
    // On the desk, which offers a model the laptop does not.
    journey.keys(&pane, &["C-s", "h"])?;
    journey.wait_terms(&pane, &["╭─ Host", ". desk"])?;
    pick(journey, &pane, "desk")?;
    journey.wait(
        &pane,
        |frame| frame.contains("│ desk ─╯") && !frame.contains("╭─ Host"),
        "the desk chosen",
    )?;
    journey.keys(&pane, &["C-s", "m"])?;
    journey.wait_terms(&pane, &["╭─ Model", ". Fable 5"])?;
    journey.frame(&pane, "desk-models")?;
    pick(journey, &pane, "Fable 5")?;
    // Another model starts at its own default effort; Claude names none, so
    // the effort is left to the model.
    journey.wait(
        &pane,
        |frame| frame.contains("Claude · Fable 5 · ") && !frame.contains("╭─ Model"),
        "Fable 5 chosen",
    )?;
    // In the desk's repository, which its agent already works in.
    journey.keys(&pane, &["C-s", "d"])?;
    journey.wait_terms(&pane, &["╭─ Folder"])?;
    journey.type_text(&pane, "repositories/site")?;
    journey.wait_terms(&pane, &["desk/repositories/site"])?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| !frame.contains("╭─ Folder") && frame.contains("repositories/site"),
        "the repository chosen",
    )?;
    journey.keys(&pane, &["C-s", "n"])?;
    journey.wait_terms(&pane, &["╭─ Name"])?;
    journey.type_text(&pane, name)?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| frame.contains(&format!("─ {name} │")),
        "the name given",
    )?;
    journey.keys(&pane, &["C-s", "w"])?;
    journey.wait_terms(&pane, &["· new worktree │"])?;
    journey.type_text(&pane, "Redesign the landing page.")?;
    journey.wait_terms(&pane, &["› Redesign the landing page."])?;
    journey.frame(&pane, "worktree-set")?;
    journey.keys(&pane, &["Enter"])?;
    // The branch reaches the header with the turn's end, which can land just
    // after the reply does.
    journey.wait(
        &pane,
        |frame| {
            chat_of(frame).as_deref() == Some(name)
                && at_rest(frame, &["Working in the new worktree."])
                && frame.contains(&format!("{name} │ {name} "))
        },
        "the new agent's reply, on its branch",
    )?;
    journey.frame(&pane, "worktree-chat")?;

    // The desk made it there, on the chosen model, in a worktree of its
    // repository on a branch named after the agent.
    let made = journey.wait_inventory(
        "desk",
        |agents| {
            agents
                .iter()
                .any(|agent| agent["name"] == name && agent["branch"] == name)
        },
        "created-in-worktree",
    )?;
    let agent = named(&made, name)?.clone();
    if agent["host_id"] != journey.host_id("desk") {
        return Err(format!("{name} runs on {}, not the desk", agent["host_id"]));
    }
    let chat = journey.wait_chat(
        "desk",
        &text(&agent["id"]),
        |chat| phase(chat) == "IDLE",
        "answered",
    )?;
    reflected_once(&chat, "Redesign the landing page.")?;
    if chat["model"] != "claude-fable-5" {
        return Err(format!(
            "{name} runs {}, not the desk's Fable 5",
            chat["model"]
        ));
    }
    let repository = journey.ready.root.join("desk/repositories/site");
    let trees = worktrees(&repository)?;
    journey.record.observations.insert(
        "desk-worktrees".to_owned(),
        Value::Array(trees.iter().cloned().map(Value::Object).collect()),
    );
    let folder = real(Path::new(&text(&agent["cwd"])));
    if !on_branch(&trees, &folder, name) {
        return Err(format!(
            "the desk's repository lists {trees:?}, not {} on {name}",
            folder.display()
        ));
    }
    if folder == real(&repository) || folder.file_name().is_none_or(|file| file != name) {
        return Err(format!(
            "{name} works in {}, not a worktree of its own",
            folder.display()
        ));
    }

    // The same name again: names are unique on a host, so the second agent
    // gets the next free one and a worktree and branch of its own.
    journey.home(&pane)?;
    journey.keys(&pane, &["n"])?;
    journey.wait_terms(
        &pane,
        &["What should the new agent work on?", "· new worktree │"],
    )?;
    journey.keys(&pane, &["C-s", "n"])?;
    journey.wait_terms(&pane, &["╭─ Name"])?;
    journey.type_text(&pane, name)?;
    journey.keys(&pane, &["Enter"])?;
    journey.wait(
        &pane,
        |frame| frame.contains(&format!("─ {name} │")),
        "the name given again",
    )?;
    journey.type_text(&pane, "Redesign it again.")?;
    journey.wait_terms(&pane, &["› Redesign it again."])?;
    journey.keys(&pane, &["Enter"])?;
    let second = format!("{name}-2");
    journey.wait(
        &pane,
        |frame| {
            chat_of(frame).as_deref() == Some(second.as_str())
                && at_rest(
                    frame,
                    &["Redesign it again.", "Working in the new worktree."],
                )
                && frame.contains(&format!("{second} │ {second} "))
        },
        "the second agent's reply, on its branch",
    )?;
    journey.frame(&pane, "name-taken")?;
    let again = journey.wait_inventory(
        "desk",
        |agents| agents.iter().any(|agent| agent["name"] == second.as_str()),
        "after-second",
    )?;
    if again.iter().filter(|agent| agent["name"] == name).count() != 1 {
        return Err(format!("the desk lists {again:?}"));
    }
    let other = named(&again, &second)?;
    let other_folder = real(Path::new(&text(&other["cwd"])));
    let trees = worktrees(&repository)?;
    if !on_branch(&trees, &other_folder, &second) || other_folder == folder {
        return Err(format!(
            "the desk's repository lists {trees:?}, not {} on {second}",
            other_folder.display()
        ));
    }
    journey.home(&pane)?;
    journey.quit_client(&pane)?;
    Ok(vec![
        "the new agent was set to run on the desk, on Fable 5 from the desk's own catalogue, in the desk's repository, in a new worktree".to_owned(),
        "it started on the desk on claude-fable-5 and answered; its prompt reached the desk once".to_owned(),
        format!("the desk's repository lists the agent's folder as a worktree on branch {name}, named after the agent"),
        format!(
            "started again as {name}, the desk named the second agent {name}-2, in a worktree of its own on branch {name}-2"
        ),
        "the client exited 0".to_owned(),
    ])
}

fn private(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o077 == 0)
}

fn report_a_problem(journey: &mut TerminalJourney) -> Proved {
    let pane = journey.launch("terminal", "laptop", &[])?;
    journey.open_chat(&pane, "limited")?;
    journey.wait_terms(&pane, &["The FAQ is rewritten."])?;
    let before = journey.capture(&pane)?;
    journey.keys(&pane, &["C-a", "b"])?;
    journey.wait_terms(
        &pane,
        &["Report a problem", "What went wrong?", "enter send"],
    )?;
    // A drag over the reply, as the terminal reports a mouse: press, move,
    // let go, in cells counted from one.
    let screen: Vec<&str> = before.lines().collect();
    let row = screen
        .iter()
        .position(|line| line.contains("The FAQ is rewritten."))
        .ok_or("the reply left the screen")?;
    let col = screen[row]
        .find("The FAQ")
        .map(|at| screen[row][..at].chars().count())
        .unwrap_or_default();
    let press = format!("\x1b[<0;{};{}M", col + 1, row + 1);
    let drag = format!("\x1b[<32;{};{}M", col + 21, row + 1);
    let release = format!("\x1b[<0;{};{}m", col + 21, row + 1);
    for sequence in [press, drag, release] {
        journey.type_text(&pane, &sequence)?;
        std::thread::sleep(Duration::from_millis(150));
    }
    journey.wait_terms(&pane, &["Mark 1: what is wrong here?", "1 mark"])?;
    journey.type_text(&pane, "This says rewritten but nothing changed.")?;
    journey.keys(&pane, &["Enter"])?;
    journey.type_text(&pane, "The reply claims work it did not do.")?;
    journey.wait_terms(&pane, &["▎ The reply claims work it did not do."])?;
    journey.frame(&pane, "report")?;
    journey.keys(&pane, &["Enter"])?;
    // The laptop is signed in to no account, so the report is kept and the
    // client says why it did not go.
    journey.wait_terms(&pane, &["report saved in", "not sent"])?;
    let reports = journey.ready.root.join("laptop/data/reports");
    let bundles: Vec<PathBuf> = std::fs::read_dir(&reports)
        .map_err(|error| format!("{}: {error}", reports.display()))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("report-"))
        })
        .collect();
    let [bundle] = bundles.as_slice() else {
        return Err(format!("the laptop holds {} reports", bundles.len()));
    };
    // Kept with the run's evidence, to read.
    copy_dir(bundle, &journey.output.join("bundle"))?;
    let header = read_json(&bundle.join("report.json"))?;
    let expected_mark = json!({
        "x": col, "y": row, "width": 21, "height": 1,
        "note": "This says rewritten but nothing changed.",
    });
    if header["marks"] != json!([expected_mark])
        || header["note"] != "The reply claims work it did not do."
    {
        return Err(format!("report.json holds {header}"));
    }
    if header["viewport"] != json!([110, 34])
        || header["parts"]["frame"] != "present"
        || header["parts"]["dump"] != "present"
    {
        return Err(format!("report.json declares {header}"));
    }
    if header["schema_version"] != 3 || header["kind"] != "report" {
        return Err(format!("report.json is not a schema 3 report: {header}"));
    }
    let reason = &header["parts"]["trace"]["absent"]["reason"];
    if reason.is_null() || *reason == "" {
        return Err(format!(
            "the trace is not declared absent with a reason: {}",
            header["parts"]
        ));
    }
    let frame =
        std::fs::read_to_string(bundle.join("frame.txt")).map_err(|error| error.to_string())?;
    if !frame.contains("The FAQ is rewritten.")
        || !frame.contains("limited │")
        || frame.contains("Report a problem")
    {
        return Err(format!("frame.txt is not the frozen chat:\n{frame}"));
    }
    let cells = read_json(&bundle.join("frame.json"))?;
    let rows = cells["rows"].as_array().map_or(&[][..], Vec::as_slice);
    let runs: Vec<&Value> = rows
        .iter()
        .flat_map(|row| row.as_array().map_or(&[][..], Vec::as_slice))
        .collect();
    if cells["width"] != 110 || cells["height"] != 34 || rows.len() != 34 {
        return Err(format!(
            "frame.json is not the 110x34 screen: {}x{}",
            cells["width"], cells["height"]
        ));
    }
    if !runs.iter().any(|run| {
        run["text"]
            .as_str()
            .is_some_and(|said| said.contains("The FAQ is rewritten."))
    }) {
        return Err("frame.json does not hold the reply".to_owned());
    }
    let coloured = runs.iter().any(|run| run["fg"] != "default");
    let bold = runs.iter().any(|run| {
        run["styles"]
            .as_array()
            .is_some_and(|styles| styles.iter().any(|style| style == "bold"))
    });
    if !coloured || !bold {
        return Err(format!(
            "frame.json carries no colours or styles: {:?}",
            &runs[..runs.len().min(5)]
        ));
    }
    let dump = bundle.join("dump");
    let parts = walk(&dump);
    let named_parts = || -> Vec<String> {
        parts
            .iter()
            .filter_map(|path| path.strip_prefix(&dump).ok())
            .map(|path| path.display().to_string())
            .collect()
    };
    let has_rows = parts
        .iter()
        .any(|path| path.file_name().is_some_and(|name| name == "row.pb"));
    if !dump.join("manifest.json").is_file() || !has_rows {
        return Err(format!("the dump is not whole: {:?}", named_parts()));
    }
    if !named_parts().iter().any(|part| part.contains("client")) {
        return Err(format!("the dump has no client parts: {:?}", named_parts()));
    }
    if !private(bundle) || !private(&bundle.join("report.json")) {
        return Err("the report is readable by others".to_owned());
    }
    journey.quit_client(&pane)?;
    Ok(vec![
        "leader b froze the chat under the report's panel".to_owned(),
        "a mouse drag marked the reply, and its note and the overall note were written".to_owned(),
        format!("the bundle holds report.json with the mark in cells ({col},{row} 21×1) and both notes"),
        "frame.txt is the chat as it was at the key, without the report's panel".to_owned(),
        "frame.json holds the same 110x34 cells with their colours and styles".to_owned(),
        "the dump is whole: its manifest, the agents' rows and this client's parts".to_owned(),
        "the parts not captured are declared absent with their reasons, and only this user can read the report"
            .to_owned(),
        "signed in to no account, the client said the report was kept and not sent".to_owned(),
        "the client exited 0".to_owned(),
    ])
}

// --- door requests -------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_readers_match_what_the_stories_look_for() {
        assert!(spaced("And email me.   queued", WITHDRAWN, "queued"));
        assert!(!spaced("And email me.queued", WITHDRAWN, "queued"));
        assert!(counts_edits("  ▸ 3 edits\n"));
        assert!(counts_edits("1 edit"));
        assert!(!counts_edits("a3 edits"));
        assert!(!counts_edits("3 editor"));
        assert!(!counts_edits("no edits"));
    }
}
