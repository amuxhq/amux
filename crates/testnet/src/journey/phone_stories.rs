//! The phone app's stories: the shared ones as a person tells them on the
//! phone, and the phone's own (signing in, buying, reporting, a large text
//! size, the local-network permission, a push, resuming links), each
//! compared with its reviewed screens and proved by what the hosts
//! recorded.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::common::*;
use super::phone::{
    AppError, BUNDLE_ID, Drawn, Launch, PhoneJourney, golden_udid, labelled, printed_pin, simctl,
};
use super::{
    Proved, items, negative_control, never_sent, phase, prompts, quoted, reflected_once, root,
};
use crate::TierDecl;
use crate::door::Control;

type Tell = fn(&mut PhoneJourney) -> Proved;

const STORIES: &[(&str, Tell)] = &[
    ("reach-host", reach_host),
    ("conversation-decision-claude-pty", |j| {
        conversation_decision(j, "decision-pty", false)
    }),
    ("conversation-decision-claude-sdk", |j| {
        conversation_decision(j, "decision-sdk", true)
    }),
    ("conversation-decision-codex", |j| {
        conversation_decision(j, "decision-codex", true)
    }),
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
    ("composer-limits", composer_limits),
    ("keep-authority", keep_authority),
    ("manage-agent", manage_agent),
    ("attachment-or-review", attachment_or_review),
    ("leave-and-recover", leave_and_recover),
    ("account-sign-in", account_sign_in),
    ("purchase-restore", purchase_restore),
    ("report", report),
    ("accessibility", accessibility),
    ("local-network", local_network),
    ("push-wake", push_wake),
    ("resume", resume),
];

/// Runs the named stories, the phone's own with `native`, or every story
/// written here, one after another on the leased golden simulator.
pub(crate) fn run(native: bool, named: Vec<String>) -> ExitCode {
    let wanted = match selected(native, named) {
        Ok(wanted) => wanted,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let udid = match golden_udid() {
        Ok(udid) => udid,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let mut failed = 0;
    for name in &wanted {
        let (_, tell) = STORIES.iter().find(|(id, _)| id == name).expect("known");
        let began = Instant::now();
        let mut journey = match super::story(name, "phone")
            .and_then(|story| PhoneJourney::start(&story, &udid))
        {
            Ok(journey) => journey,
            Err(error) => {
                eprintln!("FAIL {name}: {error}");
                failed += 1;
                continue;
            }
        };
        let told = tell(&mut journey);
        match &told {
            Ok(proved) => journey.finish(proved),
            Err(error) => journey.fail(error),
        }
        let closed = journey.close();
        match (told, closed) {
            (Ok(proved), Ok(())) => {
                for line in proved {
                    println!("- {line}");
                }
                println!("{name}: journey took {:.1}s", began.elapsed().as_secs_f64());
                // The verifier recognizes this exact final line as a pass.
                println!("{name}: passed");
            }
            (Err(error), _) | (Ok(_), Err(error)) => {
                eprintln!("FAIL {name}: {error}");
                failed += 1;
            }
        }
    }
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The stories a run tells: the phone's own with `native` (those the
/// manifest declares for the phone and no other client), else those named,
/// else every story written here.
fn selected(native: bool, named: Vec<String>) -> Result<Vec<String>, String> {
    let wanted = if native {
        super::manifest()?
            .into_iter()
            .filter(|story| story.clients == ["phone"])
            .map(|story| story.id)
            .collect()
    } else if named.is_empty() {
        STORIES.iter().map(|(id, _)| (*id).to_owned()).collect()
    } else {
        named
    };
    let unknown: Vec<&str> = wanted
        .iter()
        .filter(|name| !STORIES.iter().any(|(id, _)| id == name))
        .map(String::as_str)
        .collect();
    if unknown.is_empty() {
        Ok(wanted)
    } else {
        let known: Vec<&str> = STORIES.iter().map(|(id, _)| *id).collect();
        Err(format!(
            "no phone story written for {}; one of {}",
            unknown.join(", "),
            known.join(", ")
        ))
    }
}

// --- reading the phone ---------------------------------------------------

/// The ask choice a person reads as `label`.
fn choice(drawn: &Drawn, label: &str) -> Result<String, String> {
    let choices = || {
        drawn
            .iter()
            .filter(|(name, _)| name.starts_with("ask.choice."))
    };
    choices()
        .find(|(_, element)| element["label"] == label)
        .map(|(name, _)| name.to_owned())
        .ok_or_else(|| {
            let offered: Vec<&Value> = choices().map(|(_, element)| &element["label"]).collect();
            format!("no choice reads {label:?}; the card offers {offered:?}")
        })
}

/// The phone lists the machine it paired with by its own id and name.
fn trusted_as(drawn: &Drawn, host_id: &str, name: &str) -> Result<(), String> {
    let row = format!("hosts.row.{host_id}");
    if drawn.has(&row) && drawn.label(&row).starts_with(&format!("{name}, ")) {
        Ok(())
    } else {
        Err(format!(
            "the phone does not list {name} ({host_id}) as a host: {:?}",
            drawn.get(&row)
        ))
    }
}

/// `text` is a prompt the client library holds for its host: in the queue,
/// pending, with what it waits for.
fn is_held(drawn: &Drawn, text: &str) -> bool {
    drawn.iter().any(|(name, element)| {
        name.starts_with("chat.underway.")
            && element["label"] == text
            && element["value"]
                .as_str()
                .is_some_and(|value| value.starts_with("held · sends when "))
    })
}

/// The chat is current: the empty composer offers to message the agent,
/// not to hold what is sent.
fn current(drawn: &Drawn, agent: &str) -> bool {
    drawn.label("chat.field") == format!("Message {agent}")
}

/// The prompts waiting under the feed, oldest first: each one's words and
/// how it waits.
fn queued(drawn: &Drawn) -> Vec<(String, String)> {
    let mut rows: Vec<(u32, &Value)> = drawn
        .iter()
        .filter_map(|(name, element)| {
            let index = name.strip_prefix("chat.queued.")?;
            index
                .bytes()
                .all(|byte| byte.is_ascii_digit())
                .then(|| index.parse().ok())
                .flatten()
                .map(|index| (index, element))
        })
        .collect();
    rows.sort_by_key(|(index, _)| *index);
    rows.into_iter()
        .map(|(_, element)| {
            (
                element["label"].as_str().unwrap_or_default().to_owned(),
                element["value"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

fn asking(drawn: &Drawn, question: &str) -> bool {
    drawn.label("ask.question") == question
}

/// How many of the chat's rows hold `text`.
fn replies(drawn: &Drawn, text: &str) -> usize {
    drawn
        .iter()
        .filter(|(name, element)| {
            name.starts_with("chat.row")
                && element["label"]
                    .as_str()
                    .is_some_and(|label| label.contains(text))
        })
        .count()
}

fn plan_card(drawn: &Drawn) -> bool {
    drawn.has("ask") && drawn.has("ask.approve")
}

/// No row on the phone is a step writing the plan file.
fn no_plan_file(drawn: Drawn) -> Result<Drawn, String> {
    let shown: Vec<String> = drawn
        .iter()
        .filter(|(_, element)| {
            let label = element["label"].as_str().unwrap_or_default();
            ["/.claude/plans/", "move-the-journal.md"]
                .iter()
                .any(|part| label.contains(part))
        })
        .map(|(name, element)| format!("{name}: {}", element["label"]))
        .collect();
    if shown.is_empty() {
        Ok(drawn)
    } else {
        Err(format!("the plan file shows as a step: {shown:?}"))
    }
}

/// The chat's overview names the branch the agent's folder is on.
fn on_branch(drawn: &Drawn, branch: &str) -> Result<(), String> {
    match drawn.field("chat.overview.branch", "value") {
        said if *said == branch => Ok(()),
        said => Err(format!(
            "the overview does not say it is on {branch:?}: {said}"
        )),
    }
}

/// The server's tool allowed, then its link answered done, with no content.
fn link_done(results: &[Value]) -> Result<(), String> {
    let actions: Vec<&Value> = results.iter().map(|result| &result["action"]).collect();
    if actions == [&json!("accept"), &json!("accept")] && results[1]["content"].is_null() {
        Ok(())
    } else {
        Err(format!("Codex was answered {results:?}"))
    }
}

/// The one prompt carrying the paste and the review, as the desk holds it.
/// The phone's composer keeps a paste and a review as chips after the text,
/// so they follow it in the order they were added.
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
    let words: Vec<String> = text(&item["text"])
        .replace('\u{fffc}', " ")
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    if words != ["Please", "check", "against"] || kinds != [&json!("text"), &json!("review")] {
        return Err(format!(
            "the prompt is not the text, then the paste, then the review: {item}"
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

// --- acts ----------------------------------------------------------------

/// The phone's browser finds `host`; the PIN it printed is typed on the
/// keypad and the machine it names is trusted.
fn pair_by_code(journey: &mut PhoneJourney, host: &str) -> Result<(), String> {
    let host_id = journey.host_id(host);
    journey.tab("Hosts")?;
    journey.wait_for(&[&format!("hosts.pair.{host_id}")])?;
    let mut pairing = journey.amux(host, &["pair"])?;
    let pin = printed_pin(&mut pairing)?;
    journey.tap(&format!("hosts.pair.{host_id}"))?;
    journey.wait_for(&["pin.code"])?;
    journey.type_text("pin.code", &pin)?;
    journey.wait_for(&["pair-confirm.trust"])?;
    journey.tap("pair-confirm.trust")?;
    let row = format!("hosts.row.{host_id}");
    journey.wait(
        |drawn| drawn.has(&row) && !drawn.has("pair-confirm"),
        &format!("{host} paired"),
    )?;
    pairing.wait(Duration::from_secs(30))
}

fn open_agent(journey: &mut PhoneJourney, name: &str) -> Result<String, String> {
    let agent = journey.agent_id(name);
    journey.tab("Agents")?;
    journey.wait_for(&[&format!("home.row.{agent}")])?;
    journey.tap(&format!("home.row.{agent}"))?;
    journey.wait_for(&["chat.field"])?;
    Ok(agent)
}

fn send(journey: &mut PhoneJourney, text: &str) -> Result<(), String> {
    journey.type_text("chat.field", text)?;
    journey.wait(
        |drawn| drawn.enabled("chat.send"),
        "a message ready to send",
    )?;
    journey.tap("chat.send")
}

/// Waits for the fleet with the agent's row on it, opening the folded
/// section holding the row when the fleet keeps it folded away.
fn back_to_row(
    journey: &mut PhoneJourney,
    agent: &str,
    left: impl Fn(&Drawn) -> bool,
) -> Result<(), String> {
    let row = format!("home.row.{agent}");
    let folds = |drawn: &Drawn| -> Vec<String> {
        drawn
            .names()
            .filter(|name| name.starts_with("home.fold."))
            .map(str::to_owned)
            .collect()
    };
    let drawn = journey.wait(
        |drawn| left(drawn) && (drawn.has(&row) || !folds(drawn).is_empty()),
        "the fleet",
    )?;
    if !drawn.has(&row) {
        for fold in folds(&drawn) {
            journey.tap(&fold)?;
        }
    }
    journey.wait(|drawn| left(drawn) && drawn.has(&row), "the fleet")?;
    Ok(())
}

/// Leaves the chat for the fleet and opens it again, so it is read with the
/// keyboard down.
fn reopen(
    journey: &mut PhoneJourney,
    agent: &str,
    ready: impl Fn(&Drawn) -> bool,
) -> Result<Drawn, String> {
    journey.tap("chat.back")?;
    back_to_row(journey, agent, |drawn| !drawn.has("chat.field"))?;
    journey.tap(&format!("home.row.{agent}"))?;
    journey.wait(
        |drawn| drawn.has("chat.field") && ready(drawn),
        "the chat again",
    )
}

/// Leaves the chat for the fleet and opens it again with an ask standing
/// where the composer was, so it is read with the keyboard down.
fn reopen_card(
    journey: &mut PhoneJourney,
    agent: &str,
    ready: impl Fn(&Drawn) -> bool,
) -> Result<Drawn, String> {
    journey.tap("chat.back")?;
    back_to_row(journey, agent, |drawn| !drawn.has("chat"))?;
    journey.tap(&format!("home.row.{agent}"))?;
    journey.wait(|drawn| drawn.has("ask") && ready(drawn), "the chat again")
}

/// Back from a chat to the fleet, once the page has finished leaving.
fn back_to_fleet(journey: &mut PhoneJourney) -> Result<(), String> {
    journey.tap("chat.back")?;
    journey.wait(|drawn| !drawn.has("chat.field"), "the fleet")?;
    journey.app(json!({"kind": "settle"}))?;
    Ok(())
}

/// Back from a chat to the fleet.
fn leave_chat(journey: &mut PhoneJourney) -> Result<(), String> {
    journey.tap("chat.back")?;
    journey.wait(|drawn| !drawn.has("chat.field"), "the fleet")?;
    Ok(())
}

/// Send back… opens the note the plan goes back with; the send-back under
/// it is offered only once something is written.
fn send_back(journey: &mut PhoneJourney, card: &Drawn, note: &str) -> Result<(), String> {
    journey.tap(&choice(card, "Send back…")?)?;
    journey.wait_for(&["ask.note"])?;
    journey.type_text("ask.note", note)?;
    let drawn = journey.wait(
        |drawn| {
            drawn.iter().any(|(name, element)| {
                name.starts_with("ask.choice.")
                    && element["label"] == "Send back"
                    && enabled_or_unsaid(element)
            })
        },
        "a note ready to send back",
    )?;
    journey.tap(&choice(&drawn, "Send back")?)
}

/// An element that does not say it is disabled.
fn enabled_or_unsaid(element: &Value) -> bool {
    element
        .get("enabled")
        .is_none_or(|enabled| *enabled != false && !enabled.is_null())
}

/// Plan picked as Codex's mode on the settings card the model chip opens;
/// the card closed once the agent reports it.
fn plan_mode(journey: &mut PhoneJourney) -> Result<(), String> {
    journey.tap("chat.model")?;
    journey.wait_for(&["chat.settings.workmode.plan"])?;
    // Codex's card holds more than fits above the chat, and the mode is the
    // last thing on it; it is swiped once the card has finished rising.
    journey.app(json!({"kind": "settle"}))?;
    journey.app(json!({"kind": "scroll", "direction": "bottom", "identifier": "chat.settings.workmode.plan"}))?;
    journey.app(json!({"kind": "settle"}))?;
    journey.tap("chat.settings.workmode.plan")?;
    journey.wait(
        |drawn| drawn.value("chat.settings.workmode.plan") == "current",
        "plan mode current",
    )?;
    journey.tap("chat.settings.close")?;
    journey.wait(
        |drawn| !drawn.has("chat.settings"),
        "the settings card closed",
    )?;
    Ok(())
}

/// The activity bar slides while the turn runs; under reduced motion it
/// stands still, so the screen can be photographed. Its elapsed time is
/// masked.
fn photograph_running(journey: &mut PhoneJourney, label: &str) -> Result<(), String> {
    journey.app(json!({"kind": "assist", "motion": true, "transparency": true}))?;
    journey.screen(label, &["chat.activity"])?;
    journey.app(json!({"kind": "assist", "motion": false, "transparency": true}))?;
    Ok(())
}

// --- the shared stories --------------------------------------------------

fn reach_host(journey: &mut PhoneJourney) -> Proved {
    let desk = journey.host_id("desk");
    let prompt = "Hello from the phone.";
    let reply = "The desk is reachable.";
    // Unpaired: the phone's own browser finds the desk and offers to pair.
    journey.launch(Launch::plain())?;
    journey.tab("Hosts")?;
    let offer = format!("hosts.pair.{desk}");
    journey.wait_for(&[&offer, &offer])?;
    journey.screen("found-not-paired", &[])?;

    // Six digits the desk printed, typed on the phone's number pad, reach
    // the desk's own name and key before anything is trusted.
    let mut pairing = journey.amux("desk", &["pair"])?;
    let pin = printed_pin(&mut pairing)?;
    journey.tap(&offer)?;
    journey.wait_for(&["pin.code"])?;
    journey.type_text("pin.code", &pin)?;
    let card = journey.wait_for(&["pair-confirm.trust", "pair-confirm.fingerprint"])?;
    if card.value("pair-confirm.name") != "desk" {
        return Err(format!(
            "the code reached {:?}, not the desk",
            card.get("pair-confirm.name")
        ));
    }
    journey.screen(
        "pair-confirm",
        &["pair-confirm.fingerprint", "pair-confirm.expiry"],
    )?;
    journey.tap("pair-confirm.trust")?;
    // Trusting closes the sheet, with the desk already in the list.
    let row = format!("hosts.row.{desk}");
    let drawn = journey.wait(
        |drawn| drawn.has(&row) && !drawn.has("pair-confirm"),
        "the desk paired",
    )?;
    trusted_as(&drawn, &desk, "desk")?;
    let control = negative_control(trusted_as(&drawn, &desk, "laptop"))?;
    pairing.wait(Duration::from_secs(30))?;
    journey.screen("paired", &[])?;

    // The desk's agent is in the fleet; the phone opens it, writes, and the
    // desk holds the prompt once and answers.
    journey.tab("Agents")?;
    let agent = journey.agent_id("desk-work");
    let agent_row = format!("home.row.{agent}");
    journey.wait_for(&[&agent_row])?;
    journey.tap(&agent_row)?;
    journey.wait_for(&["chat.field"])?;
    send(journey, prompt)?;
    let heard = journey.wait_chat(
        "desk",
        "desk-work",
        |chat| phase(chat) == "IDLE" && prompts(chat, prompt).len() == 1,
        "desk-heard",
    )?;
    reflected_once(&heard, prompt)?;
    journey.wait(
        |drawn| labelled(drawn, reply),
        "the desk's reply on the phone",
    )?;

    // Leaving the chat comes back to the fleet; opened again, the chat holds
    // the prompt and the reply.
    journey.tap("chat.back")?;
    journey.wait(
        |drawn| drawn.has(&agent_row) && !drawn.has("chat.field"),
        "the fleet",
    )?;
    journey.tap(&agent_row)?;
    journey.wait(
        |drawn| drawn.has("chat.row.turn-end") && labelled(drawn, reply),
        "the chat opened again",
    )?;
    journey.screen("usable-chat", &["chat.row.turn-end"])?;
    journey.tap("chat.back")?;
    journey.wait(
        |drawn| drawn.has(&agent_row) && !drawn.has("chat.field"),
        "the fleet",
    )?;
    Ok(vec![
        "the phone's own browser found the desk in the net's scope and offered to pair".to_owned(),
        "the PIN the desk printed, typed on the keypad, reached the desk's name and key before trust".to_owned(),
        control,
        format!("the phone lists desk {desk} as its host after Pair"),
        format!(
            "the desk's agent opened on the phone; the desk holds {} once and answered",
            quoted(prompt)
        ),
        "leaving the chat came back to the fleet, and the chat opened again held the prompt and the reply".to_owned(),
    ])
}

fn conversation_decision(journey: &mut PhoneJourney, agent: &str, provider_logs: bool) -> Proved {
    journey.launch(Launch::plain())?;
    pair_by_code(journey, "desk")?;
    let agent_id = open_agent(journey, agent)?;
    send(journey, PROMPT)?;
    let card = journey.wait(
        |drawn| drawn.has("ask") && labelled(drawn, "deploy --check") && labelled(drawn, "Allow"),
        "the permission card",
    )?;
    let sent = journey.wait_chat(
        "desk",
        agent,
        |chat| !prompts(chat, PROMPT).is_empty(),
        "prompt-reflected",
    )?;
    reflected_once(&sent, PROMPT)?;
    let control = negative_control(reflected_once(&sent, &format!("{PROMPT} (a wrong prompt)")))?;
    journey.screen("permission", &[])?;
    journey.tap(&choice(&card, "Allow")?)?;
    journey.wait(
        |drawn| !drawn.has("ask") && labelled(drawn, REPLY),
        "the settled turn",
    )?;
    let settled = journey.wait_chat(
        "desk",
        agent,
        |chat| phase(chat) == "IDLE" && super::holds(chat, REPLY),
        "turn-settled",
    )?;
    reflected_once(&settled, PROMPT)?;
    let mut proved = vec![
        format!("{} reflected once in the desk's chat", quoted(PROMPT)),
        control,
        "the permission card offered its outcomes and Allow was tapped".to_owned(),
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
    leave_chat(journey)?;
    journey.tap(&format!("home.row.{agent_id}"))?;
    journey.wait(
        |drawn| drawn.has("chat.row.turn-end") && labelled(drawn, REPLY),
        "the chat opened again",
    )?;
    journey.screen("settled", &["chat.row.turn-end"])?;
    Ok(proved)
}

fn decide_plan(journey: &mut PhoneJourney, agent: &str, headless: bool) -> Proved {
    journey.launch(Launch::plain())?;
    pair_by_code(journey, "desk")?;
    let agent_id = open_agent(journey, agent)?;
    send(journey, PLAN_PROMPT)?;
    let card = no_plan_file(journey.wait(
        |drawn| plan_card(drawn) && labelled(drawn, "Delete the old journal."),
        "the plan and its decision",
    )?)?;
    journey.screen("plan", &[])?;
    send_back(journey, &card, PLAN_NOTE)?;
    no_plan_file(journey.wait(
        |drawn| {
            plan_card(drawn)
                && labelled(drawn, "Plan sent back")
                && labelled(drawn, PLAN_NOTE)
                && labelled(drawn, "Keep the old journal for a week.")
        },
        "the revised plan",
    )?)?;
    journey.screen("revised", &[])?;
    journey.tap("ask.approve")?;
    journey.wait(
        |drawn| !drawn.has("ask") && labelled(drawn, PLAN_DONE),
        "the approved plan's work",
    )?;
    let settled = journey.wait_chat(
        "desk",
        agent,
        |chat| phase(chat) == "IDLE" && super::holds(chat, PLAN_DONE),
        "plan-settled",
    )?;
    reflected_once(&settled, PLAN_PROMPT)?;
    plans_only(&settled)?;
    let mut extra = items(&settled).to_vec();
    extra.push(json!({"key": "toolu_write", "text": ""}));
    let mut proved = vec![
        format!("{} reflected once in the desk's chat", quoted(PLAN_PROMPT)),
        "the plan read whole in the chat with its decision where the composer was".to_owned(),
        "sent back with a note, the revised plan arrived under the plan sent back; approved, the work ran".to_owned(),
        "the plan file Claude wrote before each plan showed as no step, on the phone or on the desk".to_owned(),
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
    no_plan_file(reopen(journey, &agent_id, |drawn| {
        labelled(drawn, PLAN_DONE) && drawn.has("chat.row.turn-end")
    })?)?;
    journey.screen("approved", &["chat.row.turn-end"])?;
    Ok(proved)
}

fn decide_plan_codex(journey: &mut PhoneJourney) -> Proved {
    let agent = "planner-codex";
    journey.launch(Launch::plain())?;
    pair_by_code(journey, "desk")?;
    let agent_id = open_agent(journey, agent)?;
    plan_mode(journey)?;
    send(journey, PLAN_PROMPT)?;
    // The plan is in the chat while the turn still runs, with no decision;
    // opened again, it is read with the keyboard down.
    let streaming = |drawn: &Drawn| {
        labelled(drawn, "Delete the old journal.")
            && !drawn.has("ask")
            && drawn.has("chat.activity")
    };
    journey.wait(streaming, "the plan streaming")?;
    reopen(journey, &agent_id, streaming)?;
    photograph_running(journey, "plan-streaming")?;
    journey.request(open_gate("plan-streamed"), None)?;
    journey.wait(
        |drawn| plan_card(drawn) && labelled(drawn, "Delete the old journal."),
        "the plan's decision",
    )?;
    journey.wait_chat(
        "desk",
        agent,
        |chat| phase(chat) == "NEEDS_YOU",
        "plan-needs-you",
    )?;
    journey.screen("plan-ready", &[])?;
    journey.tap("ask.approve")?;
    journey.wait(
        |drawn| !drawn.has("ask") && labelled(drawn, PLAN_DONE) && drawn.has("chat.row.turn-end"),
        "the implemented plan's work",
    )?;
    // A second plan, sent back with a note: Codex plans again on it.
    plan_mode(journey)?;
    send(journey, ROTATE_PROMPT)?;
    let card = journey.wait(
        |drawn| plan_card(drawn) && labelled(drawn, "Delete logs older than a week."),
        "the second plan",
    )?;
    send_back(journey, &card, ROTATE_NOTE)?;
    journey.wait(
        |drawn| plan_card(drawn) && labelled(drawn, "Delete logs older than a month."),
        "the revised plan",
    )?;
    journey.wait_chat(
        "desk",
        agent,
        |chat| phase(chat) == "NEEDS_YOU" && prompts(chat, ROTATE_NOTE).len() == 1,
        "revised",
    )?;
    // Opened again, so it is read at rest: the decision stands where the
    // composer was.
    let row = format!("home.row.{agent_id}");
    journey.tap("chat.back")?;
    journey.wait(|drawn| !drawn.has("chat") && drawn.has(&row), "the fleet")?;
    journey.tap(&row)?;
    journey.wait(
        |drawn| plan_card(drawn) && labelled(drawn, "Delete logs older than a month."),
        "the chat again",
    )?;
    journey.screen("sent-back", &[])?;
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
    Ok(vec![
        "plan picked on the settings card, the plan showed in the chat while Codex's turn still ran".to_owned(),
        "when the turn ended the plan's decision stood where the composer was and the desk said the agent needs you".to_owned(),
        format!(
            "Approve started a turn out of plan mode with {}, and its work ran",
            quoted(IMPLEMENT)
        ),
        "a second plan, sent back with a note, came back revised; the note went as the next prompt, still in plan mode".to_owned(),
        "Codex was started on the prompt and the note in plan mode and on the implement turn in default mode".to_owned(),
        control,
    ])
}

fn answer_questions(journey: &mut PhoneJourney) -> Proved {
    journey.launch(Launch::plain())?;
    pair_by_code(journey, "desk")?;
    let asker = open_agent(journey, "asker")?;
    send(journey, "Add a settings screen.")?;
    let first = |drawn: &Drawn| asking(drawn, "How should the settings screen be laid out?");
    journey.wait(first, "the first question")?;
    reopen_card(journey, &asker, first)?;
    journey.screen("first-question", &[])?;
    // Picked, an option shows its preview above the options.
    journey.tap("ask.option.1")?;
    journey.wait(
        |drawn| drawn.value("ask.option.1") == "selected" && labelled(drawn, "Palette  terminal"),
        "Stacked's preview",
    )?;
    journey.screen("stacked-preview", &[])?;
    journey.tap("ask.next")?;
    // The second is answered in the person's own words.
    journey.wait(
        |drawn| asking(drawn, "Where should the screen open from?"),
        "the second question",
    )?;
    journey.tap("ask.option.other")?;
    journey.wait_for(&["ask.other"])?;
    journey.type_text("ask.other", OTHER)?;
    journey.wait(|drawn| drawn.enabled("ask.next"), "an answer typed")?;
    journey.tap("ask.next")?;
    // The third is skipped.
    journey.wait(
        |drawn| asking(drawn, SKIPPED) && drawn.has("ask.skip"),
        "the third question",
    )?;
    journey.tap("ask.skip")?;
    let review = journey.wait(
        |drawn| {
            drawn.has("ask.send")
                && labelled(drawn, "Stacked")
                && labelled(drawn, &format!("“{OTHER}”"))
                && labelled(drawn, "Skipped")
        },
        "the answers for review",
    )?;
    if review.value("ask.review.2") != "skipped" {
        return Err(format!(
            "the review does not hold the third question skipped: {:?}",
            review.get("ask.review.2")
        ));
    }
    reopen_card(journey, &asker, |drawn| drawn.has("ask.send"))?;
    journey.screen("review", &[])?;
    journey.tap("ask.send")?;
    let done = "Thanks, I'll build it that way.";
    journey.wait(
        |drawn| !drawn.has("ask") && labelled(drawn, done),
        "the reply",
    )?;
    journey.wait_chat("desk", "asker", |chat| phase(chat) == "IDLE", "answered")?;
    reopen(journey, &asker, |drawn| {
        labelled(drawn, done) && drawn.has("chat.row.turn-end")
    })?;
    journey.screen("answered", &["chat.row.turn-end"])?;

    // A second ask, replied to instead in the person's own words.
    send(journey, PALETTE)?;
    journey.wait(
        |drawn| asking(drawn, "Which palette should it open in?") && drawn.has("ask.reply"),
        "the second ask",
    )?;
    journey.tap("ask.reply")?;
    journey.wait_for(&["ask.reply.text"])?;
    journey.type_text("ask.reply.text", QUESTION_REPLY)?;
    let written = |drawn: &Drawn| drawn.enabled("ask.reply.send");
    journey.wait(written, "a reply written")?;
    reopen_card(journey, &asker, written)?;
    journey.screen("reply-instead", &[])?;
    journey.tap("ask.reply.send")?;
    let later = "I'll leave the palette for later.";
    journey.wait(
        |drawn| !drawn.has("ask") && labelled(drawn, later),
        "the reply to the reply",
    )?;
    journey.wait_chat(
        "desk",
        "asker",
        |chat| phase(chat) == "IDLE" && prompts(chat, PALETTE).len() == 1,
        "replied",
    )?;
    reopen(journey, &asker, |drawn| {
        labelled(drawn, later) && labelled(drawn, QUESTION_REPLY)
    })?;
    journey.screen("replied", &["chat.row.turn-end"])?;
    let decisions = control_responses(&journey.provider_input("asker", "answers")?)?;
    answered(&decisions, &answers())?;
    replied(&decisions, QUESTION_REPLY)?;
    let mut wrong = answers();
    wrong[SKIPPED] = json!("Relay");
    Ok(vec![
        "a picked option showed its preview above the options".to_owned(),
        "a picked option, a typed answer under Something else and a skipped question showed together for review".to_owned(),
        "Claude received one answer naming Stacked and the typed text, with the skipped question left out".to_owned(),
        "a second ask, replied to instead, reached Claude as the question refused with the person's words".to_owned(),
        negative_control(answered(&decisions, &wrong))?,
        negative_control(replied(&decisions, "Pick Terminal."))?,
    ])
}

fn tool_server_asks(journey: &mut PhoneJourney) -> Proved {
    journey.launch(Launch::plain())?;
    pair_by_code(journey, "desk")?;
    // A tool server's form, its fields in the order the server wrote.
    let filer = open_agent(journey, "filer")?;
    send(journey, "File the reconnect flake.")?;
    journey.wait_for(&["ask.field.title", "ask.field.team", "ask.field.estimate"])?;
    reopen_card(journey, &filer, |drawn| drawn.has("ask.field.title"))?;
    journey.screen("form", &[])?;
    journey.type_text("ask.field.title", FORM_TITLE)?;
    // A choice with no default starts unchosen: the person opens it and picks.
    journey.tap("ask.field.team")?;
    journey.choose("FOX")?;
    journey.wait(
        |drawn| drawn.value("ask.field.team") == "FOX",
        "the team picked",
    )?;
    journey.type_text("ask.field.estimate", "3")?;
    let filled = |drawn: &Drawn| {
        drawn.enabled("ask.submit")
            && [("title", FORM_TITLE), ("team", "FOX"), ("estimate", "3")]
                .iter()
                .all(|(name, value)| drawn.value(&format!("ask.field.{name}")) == *value)
    };
    journey.wait(filled, "the form filled and ready to submit")?;
    // Leaving the chat mid-form and coming back finds what was typed.
    reopen_card(journey, &filer, filled)?;
    journey.screen("form-kept", &[])?;
    // What goes is what the form shows.
    journey.tap("ask.submit")?;
    let filed = "Filed it in Linear.";
    journey.wait(
        |drawn| !drawn.has("ask") && labelled(drawn, filed),
        "the filed reply",
    )?;
    journey.wait_chat("desk", "filer", |chat| phase(chat) == "IDLE", "filed")?;
    reopen(journey, &filer, |drawn| {
        labelled(drawn, filed) && drawn.has("chat.row.turn-end")
    })?;
    journey.screen("form-sent", &["chat.row.turn-end"])?;
    let decisions = control_responses(&journey.provider_input("filer", "form-answer")?)?;
    form_sent(&decisions, &form())?;
    let mut wrong = form();
    wrong["team"] = json!("CORE");
    let control = negative_control(form_sent(&decisions, &wrong))?;

    // A link: Codex asks to let the server's tool run, then the server
    // sends a link, worded from its own message.
    leave_chat(journey)?;
    let linker = open_agent(journey, "linker")?;
    send(journey, "Check the relay's error rate.")?;
    let card = journey.wait(
        |drawn| drawn.has("ask") && labelled(drawn, "grafana"),
        "the tool's ask",
    )?;
    journey.tap(&choice(&card, "Allow")?)?;
    journey.wait(
        |drawn| drawn.has("ask.open") && labelled(drawn, "https://grafana.example.com/login"),
        "the link",
    )?;
    reopen_card(journey, &linker, |drawn| drawn.has("ask.open"))?;
    journey.screen("link", &[])?;
    let drawn = journey.elements()?;
    journey.tap(&choice(&drawn, "I’m done")?)?;
    let reply = "I'll read the dashboards another way.";
    journey.wait(
        |drawn| !drawn.has("ask") && labelled(drawn, reply),
        "the reply",
    )?;
    journey.wait_chat("desk", "linker", |chat| phase(chat) == "IDLE", "linked")?;
    let results = codex_results(&journey.provider_input("linker", "link-answers")?)?;
    link_done(&results)?;
    reopen(journey, &linker, |drawn| {
        labelled(drawn, reply) && drawn.has("chat.row.turn-end")
    })?;
    journey.screen("link-done", &["chat.row.turn-end"])?;
    Ok(vec![
        "the form asked for title, team and estimate in the server's order and showed the answers before they went".to_owned(),
        "leaving the chat mid-form and coming back found the values typed".to_owned(),
        format!(
            "Claude received the form accepted with {{'title': {}, 'team': 'FOX', 'estimate': 3}}",
            quoted(FORM_TITLE)
        ),
        control,
        "Codex's ask to run the server's tool was allowed, then the link showed its message and address".to_owned(),
        "I’m done answered the link: Codex received two accepts, the link's with no content".to_owned(),
        negative_control(link_done(&[results[0].clone(), json!({"action": "decline"})]))?,
    ])
}

fn queue_and_steer(journey: &mut PhoneJourney) -> Proved {
    journey.launch(Launch::plain())?;
    pair_by_code(journey, "desk")?;
    let worker = open_agent(journey, "worker")?;
    send(journey, RUN)?;
    journey.wait(
        |drawn| labelled(drawn, "scripts/soak --for 1h") && drawn.has("chat.activity"),
        "the long run",
    )?;
    // While it works, prompts queue under the feed.
    send(journey, STEER)?;
    send(journey, WITHDRAWN)?;
    let both = |drawn: &Drawn| {
        queued(drawn)
            == [
                (STEER.to_owned(), "queued".to_owned()),
                (WITHDRAWN.to_owned(), "queued".to_owned()),
            ]
    };
    journey.wait(both, "two queued")?;
    reopen(journey, &worker, both)?;
    photograph_running(journey, "queued")?;
    // Withdrawn, the newest is a draft again, and cleared.
    journey.tap("chat.queued.1.withdraw")?;
    journey.wait(
        |drawn| {
            drawn.value("chat.field") == WITHDRAWN
                && queued(drawn) == [(STEER.to_owned(), "queued".to_owned())]
        },
        "the withdrawn prompt back in the composer",
    )?;
    journey.app(json!({"kind": "clear", "identifier": "chat.field"}))?;
    journey.wait(
        |drawn| drawn.value("chat.field").is_empty(),
        "the draft cleared",
    )?;
    // The other goes into the turn now.
    journey.tap("chat.queued.0.sendNow")?;
    let steered = |drawn: &Drawn| queued(drawn) == [(STEER.to_owned(), "steered".to_owned())];
    journey.wait(steered, "the prompt steered into the turn")?;
    reopen(journey, &worker, steered)?;
    photograph_running(journey, "steered")?;
    journey.request(open_gate("run-done"), None)?;
    journey.wait(
        |drawn| {
            queued(drawn).is_empty()
                && labelled(drawn, "The run finished.")
                && !drawn.has("chat.activity")
        },
        "the turn's end",
    )?;
    let settled = journey.wait_chat("desk", "worker", |chat| phase(chat) == "IDLE", "settled")?;
    reflected_once(&settled, RUN)?;
    reflected_once(&settled, STEER)?;
    never_sent(&settled, WITHDRAWN)?;
    let control = negative_control(never_sent(&settled, STEER))?;
    reopen(journey, &worker, |drawn| {
        labelled(drawn, "The run finished.") && drawn.has("chat.row.turn-end")
    })?;
    journey.screen("finished", &["chat.row.turn-end"])?;
    Ok(vec![
        "two prompts sent while the agent worked queued under the feed".to_owned(),
        "withdrawn, the newest came back to the composer as a draft and was cleared; the desk never received it".to_owned(),
        format!(
            "sent now, {} went into the running turn and the desk holds it once",
            quoted(STEER)
        ),
        control,
    ])
}

fn send_while_away(journey: &mut PhoneJourney) -> Proved {
    journey.launch(Launch::plain())?;
    pair_by_code(journey, "cabin")?;
    let backups = open_agent(journey, "backups")?;
    // The cabin goes out of reach: its daemon stops, its agent kept.
    journey.request(stop_daemon("cabin"), None)?;
    journey.wait(
        |drawn| labelled(drawn, "out of reach"),
        "the cabin out of reach",
    )?;
    // Send stays open: the client library holds the prompt, pending with
    // Cancel, until the cabin can be reached.
    send(journey, BACKUP)?;
    journey.wait(
        |drawn| is_held(drawn, BACKUP) && drawn.value("chat.field").is_empty(),
        "the prompt held",
    )?;
    reopen(journey, &backups, |drawn| is_held(drawn, BACKUP))?;
    journey.screen("away", &[])?;

    // The cabin back: the held prompt goes by itself.
    journey.request(restart_daemon("cabin"), None)?;
    journey.wait(
        |drawn| {
            labelled(drawn, BACKUP_DONE)
                && drawn.has("chat.row.turn-end")
                && !is_held(drawn, BACKUP)
        },
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
    reopen(journey, &backups, |drawn| {
        labelled(drawn, BACKUP_DONE) && drawn.has("chat.row.turn-end")
    })?;
    journey.screen("sent", &["chat.row.turn-end"])?;
    Ok(vec![
        "with the cabin out of reach, Send held the prompt, pending until the cabin is back, and it stayed held across leaving the chat".to_owned(),
        format!(
            "with the cabin back, the held prompt went by itself and the cabin holds {} once",
            quoted(BACKUP)
        ),
        control,
    ])
}

const LIMIT: &str = "5-hour limit reached · resets ";
const SIGN_IN: &str = "Run claude and sign in with /login on desk.";

fn composer_limits(journey: &mut PhoneJourney) -> Proved {
    journey.launch(Launch::plain())?;
    pair_by_code(journey, "desk")?;
    // At a usage limit the strip over the composer names the window and
    // when it resets, and sending still works. The reset is a time of day,
    // so the strip is masked and its words checked here.
    let limit_named = |drawn: &Drawn| drawn.label("chat.strip").starts_with(LIMIT);
    let limited = open_agent(journey, "limited")?;
    journey.wait(
        |drawn| limit_named(drawn) && drawn.has("chat.field"),
        "the limit named over the composer",
    )?;
    reopen(journey, &limited, limit_named)?;
    journey.screen("limit", &["chat.strip"])?;
    send(journey, "Try again.")?;
    journey.wait(
        |drawn| labelled(drawn, "Done after the limit.") && drawn.has("chat.row.turn-end"),
        "the reply past the limit",
    )?;
    let past = journey.wait_chat(
        "desk",
        "limited",
        |chat| phase(chat) == "IDLE" && prompts(chat, "Try again.").len() == 1,
        "limited-sent",
    )?;
    reflected_once(&past, "Try again.")?;
    let control = negative_control(reflected_once(&past, "Try again. (a wrong prompt)"))?;

    // A refused credential takes the composer's place: Claude must be
    // signed in again on the host it runs on.
    leave_chat(journey)?;
    let expired = open_agent(journey, "expired")?;
    send(journey, "Check the links.")?;
    let asked = |drawn: &Drawn| {
        drawn.label("chat.foot.sign-in") == "Claude needs you to sign in"
            && drawn.value("chat.foot.sign-in").contains(SIGN_IN)
            && !drawn.has("chat.field")
    };
    journey.wait(asked, "signing in in the composer's place")?;
    let refused = journey.wait_chat("desk", "expired", |chat| phase(chat) == "IDLE", "refused")?;
    reflected_once(&refused, "Check the links.")?;
    let row = format!("home.row.{expired}");
    journey.tap("chat.back")?;
    journey.wait(|drawn| !drawn.has("chat") && drawn.has(&row), "the fleet")?;
    journey.tap(&row)?;
    journey.wait(asked, "the chat again")?;
    journey.screen("sign-in", &["chat.row.turn-end"])?;
    Ok(vec![
        "at its usage limit the strip over the composer named the 5-hour window and when it resets".to_owned(),
        "a prompt still went past the limit and was answered; the desk holds it once".to_owned(),
        control,
        "refused its credential, Claude's chat put signing in in the composer's place, naming desk to sign in on".to_owned(),
        "the desk holds the refused turn's prompt once".to_owned(),
    ])
}

fn manage_agent(journey: &mut PhoneJourney) -> Proved {
    journey.launch(Launch::plain())?;
    pair_by_code(journey, "desk")?;
    let before = ids(&journey.wait_inventory("desk", |_| true, "inventory-before")?);

    // Created from the fleet: Claude on the desk, whose chat opens.
    journey.tab("Agents")?;
    journey.tap("home.newAgent")?;
    let drawn = journey.wait_for(&["new-agent"])?;
    if drawn.value("new-agent.host") != "desk" {
        journey.tap("new-agent.host")?;
        journey.choose("desk")?;
    }
    // New Agent starts on the kind most recently at work, which on this
    // desk may be Codex.
    if drawn.value("new-agent.provider") != "claude" {
        journey.tap("new-agent.provider")?;
        journey.choose("Claude")?;
    }
    let drawn = journey.wait(
        |drawn| !drawn.value("new-agent.directory").is_empty(),
        "a directory chosen",
    )?;
    // Unnamed, an agent is called after the directory it starts in.
    let named = Path::new(drawn.value("new-agent.directory"))
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    journey.tap("new-agent.start")?;
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
    lifecycle(&created, &id, &named, LIVE)?;
    journey.wait_for(&["chat.field"])?;
    send(journey, HELLO)?;
    journey.wait(
        |drawn| replies(drawn, READY) == 1 && drawn.has("chat.row.turn-end"),
        "the first reply",
    )?;
    reopen(journey, &id, |drawn| replies(drawn, READY) == 1)?;
    journey.screen("created", &["chat.row.turn-end"])?;

    // Renamed from the chat's menu: the desk lists the same agent under it.
    journey.tap("chat.more")?;
    journey.choose("Rename")?;
    journey.wait_for(&["chat.rename.field"])?;
    journey.app(json!({"kind": "clear", "identifier": "chat.rename.field"}))?;
    journey.type_text("chat.rename.field", "helper")?;
    journey.tap("chat.rename.confirm")?;
    let renamed = journey.wait_inventory(
        "desk",
        |agents| listed(agents, &id).is_some_and(|agent| agent["name"] == "helper"),
        "renamed",
    )?;
    lifecycle(&renamed, &id, "helper", LIVE)?;
    journey.wait(
        |drawn| drawn.value("chat.title") == "helper",
        "the new name on the phone",
    )?;

    // Stopped: exited on the desk with its history kept, and resumable.
    stop_from_menu(journey)?;
    let stopped = journey.wait_inventory(
        "desk",
        |agents| listed(agents, &id).is_some_and(|agent| agent["lifecycle"] == EXITED),
        "stopped",
    )?;
    lifecycle(&stopped, &id, "helper", EXITED)?;
    let control = negative_control(lifecycle(&stopped, &id, "helper", LIVE))?;
    let kept = journey.chat("desk", &id, "history-kept")?;
    reflected_once(&kept, HELLO)?;
    if !super::holds(&kept, READY) {
        return Err(format!(
            "the stopped agent's history lost its reply: {kept}"
        ));
    }
    journey.wait(
        |drawn| labelled(drawn, "Exited"),
        "the chat saying it exited",
    )?;
    reopen(journey, &id, |drawn| {
        labelled(drawn, "Exited") && replies(drawn, READY) == 1
    })?;
    journey.screen("exited-and-resumable", &[])?;

    // Resumed from the exited composer: the same identity, live again.
    journey.type_text("chat.field", BACK)?;
    journey.wait(
        |drawn| drawn.enabled("chat.resume"),
        "a message ready to resume with",
    )?;
    journey.tap("chat.resume")?;
    let resumed = journey.wait_inventory(
        "desk",
        |agents| listed(agents, &id).is_some_and(|agent| agent["lifecycle"] == LIVE),
        "resumed",
    )?;
    lifecycle(&resumed, &id, "helper", LIVE)?;
    let chat = journey.wait_chat(
        "desk",
        &id,
        |chat| prompts(chat, BACK).len() == 1 && phase(chat) == "IDLE",
        "resumed-chat",
    )?;
    reflected_once(&chat, HELLO)?;
    reflected_once(&chat, BACK)?;
    journey.wait(|drawn| replies(drawn, READY) == 2, "the resumed reply")?;
    reopen(journey, &id, |drawn| {
        replies(drawn, READY) == 2 && drawn.has("chat.row.turn-end")
    })?;
    journey.screen("resumed", &["chat.row.turn-end"])?;

    // Deleted, explicitly: only an exited agent offers it, so stopped
    // again first, then gone from the fleet and from the desk.
    stop_from_menu(journey)?;
    journey.wait_inventory(
        "desk",
        |agents| listed(agents, &id).is_some_and(|agent| agent["lifecycle"] == EXITED),
        "stopped-again",
    )?;
    journey.wait(
        |drawn| labelled(drawn, "Exited"),
        "the chat saying it exited again",
    )?;
    journey.tap("chat.more")?;
    journey.choose("Delete Agent")?;
    journey.wait_for(&["chat.delete.confirm"])?;
    journey.tap("chat.delete.confirm")?;
    let row = format!("home.row.{id}");
    journey.wait(
        |drawn| !drawn.has("chat.field") && !drawn.has(&row),
        "the fleet without it",
    )?;
    journey.wait_inventory("desk", |agents| listed(agents, &id).is_none(), "deleted")?;
    journey.screen("deleted", &[])?;
    Ok(vec![
        "New Agent on the desk created a Claude agent the desk lists under its directory's name, and its chat answered".to_owned(),
        "Rename from the chat's menu: the desk lists the same id as helper".to_owned(),
        "Stop Agent: the desk lists it exited, with its prompt and reply kept".to_owned(),
        control,
        "a message from the exited composer resumed the same id, and each prompt is in the desk's chat once".to_owned(),
        "stopped again, Delete Agent: gone from the fleet and from the desk".to_owned(),
    ])
}

fn stop_from_menu(journey: &mut PhoneJourney) -> Result<(), String> {
    journey.tap("chat.more")?;
    journey.choose("Stop Agent")?;
    journey.wait_for(&["chat.stop.confirm"])?;
    journey.tap("chat.stop.confirm")
}

fn attachment_or_review(journey: &mut PhoneJourney) -> Proved {
    review_repository(&journey.ready.root.join("desk/work"))?;
    // An agent reads its folder's git facts when it starts and when a turn
    // ends, and the reviewer started before its folder was a repository:
    // start it again so the phone has changes to offer for review.
    journey.request(
        Control::Stop {
            agent: "reviewer".to_owned(),
        },
        None,
    )?;
    journey.request(
        Control::Resume {
            agent: "reviewer".to_owned(),
            text: None,
        },
        None,
    )?;

    journey.launch(Launch::plain())?;
    pair_by_code(journey, "desk")?;
    let reviewer = open_agent(journey, "reviewer")?;

    // Text, a long paste that becomes one chip, more text.
    journey.type_text("chat.field", "Please check")?;
    journey.paste("chat.field", &pasted())?;
    journey.wait(
        |drawn| labelled(drawn, "Pasted text"),
        "the paste as one chip",
    )?;
    // The field keeps the space it put between the words and the paste.
    journey.type_text("chat.field", "against")?;

    // A review of the desk's working tree, with a comment on one line.
    journey.wait_for(&["chat.changes"])?;
    journey.tap("chat.changes")?;
    let drawn = journey.wait(
        |drawn| drawn.names().any(|name| name.starts_with("review.line.")),
        "the review's lines",
    )?;
    let line = drawn
        .iter()
        .find(|(name, element)| {
            name.starts_with("review.line.")
                && element["label"]
                    .as_str()
                    .is_some_and(|label| label.contains("rsync --delete"))
        })
        .map(|(name, _)| name.to_owned())
        .ok_or("no review line holds the edit")?;
    journey.perform(&line, "Select line")?;
    journey.wait_for(&["review.comment"])?;
    journey.screen("review-diff", &[])?;
    journey.tap("review.comment")?;
    journey.wait_for(&["review.note"])?;
    journey.type_text("review.note", COMMENT)?;
    journey.wait(
        |drawn| drawn.enabled("review.add"),
        "a comment ready to add",
    )?;
    journey.tap("review.add")?;
    journey.wait(
        |drawn| labelled(drawn, COMMENT) && drawn.has("review.attach"),
        "the comment on the line",
    )?;
    journey.screen("review-comment", &[])?;
    journey.tap("review.attach")?;
    journey.wait(
        |drawn| drawn.has("chat.field") && !drawn.has("review.attach"),
        "the chat again",
    )?;

    // Read with the keyboard down: the draft keeps its text and both chips.
    reopen(journey, &reviewer, |drawn| labelled(drawn, "Pasted text"))?;
    journey.screen("composer-tokens", &[])?;
    journey.wait(
        |drawn| drawn.enabled("chat.send"),
        "the draft ready to send",
    )?;
    journey.tap("chat.send")?;
    journey.wait(
        |drawn| labelled(drawn, "I read the review."),
        "the reviewer's reply",
    )?;

    // The desk received the text, the paste's exact text, the comment, and
    // the patch it made itself, whose bytes are on its disk.
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

    // Opened again, the chat shows what was sent and the reply.
    reopen(journey, &reviewer, |drawn| {
        labelled(drawn, "I read the review.") && drawn.has("chat.row.turn-end")
    })?;
    journey.screen("reopened", &["chat.row.turn-end"])?;

    // The chat's overview names the branch its folder is on.
    journey.tap("chat.more")?;
    journey.choose("Overview")?;
    let drawn = journey.wait(
        |drawn| drawn.has("chat.overview.branch"),
        "the overview's branch",
    )?;
    on_branch(&drawn, "main")?;
    let branch_control = negative_control(on_branch(&drawn, "develop"))?;
    journey.screen("overview-branch", &[])?;
    Ok(vec![
        "the prompt reached the desk as the text, then the paste, then the review, as the composer showed them".to_owned(),
        "the paste arrived as one attachment holding exactly the twelve lines pasted".to_owned(),
        format!("the review carries one comment on deploy.sh line 3: {}", quoted(COMMENT)),
        control,
        "the review names a patch the desk computed; its bytes on the desk hash to that name and hold the edit".to_owned(),
        "the chat opened again shows what was sent and the reply".to_owned(),
        "the chat's overview names the branch its folder is on, main".to_owned(),
        branch_control,
    ])
}

const FIRST: &str = "The recovery state is safely stored.";
const SECOND: &str = "A second turn arrived while you were away.";
const THIRD: &str = "Back after the restart.";
const DRAFT: &str = "Pick up where we left off.";

fn leave_and_recover(journey: &mut PhoneJourney) -> Proved {
    // A real first run makes the phone's replica of the desk's agent.
    journey.launch(Launch::plain())?;
    pair_by_code(journey, "desk")?;
    let keeper = open_agent(journey, "keeper")?;
    journey.wait(
        |drawn| labelled(drawn, FIRST) && drawn.has("chat.row.turn-end"),
        "the first turn",
    )?;
    journey.quit()?;

    // The desk moves on while the phone is closed, then goes out of reach:
    // its daemon stops, and its agent keeps its journal.
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
        |chat| phase(chat) == "IDLE" && super::holds(chat, SECOND),
        "desk-moved-on",
    )?;
    journey.request(stop_daemon("desk"), None)?;
    let away = Instant::now();

    // Opened again, the app paints the chat from its own store: the cached
    // tail, the desk out of reach, and a composer that keeps a draft.
    journey.relaunch(Launch::plain())?;
    journey.tab("Agents")?;
    let row = format!("home.row.{keeper}");
    journey.wait_for(&[&row])?;
    journey.tap(&row)?;
    let drawn = journey.wait(
        |drawn| labelled(drawn, FIRST) && labelled(drawn, "out of reach"),
        "the cached chat, out of reach",
    )?;
    if labelled(&drawn, SECOND) {
        return Err("the cached chat shows a turn the phone never received".to_owned());
    }
    journey.type_text("chat.field", DRAFT)?;
    let drawn = journey.wait(
        |drawn| drawn.value("chat.field") == DRAFT,
        "the draft written",
    )?;
    if current(&drawn, "keeper") {
        return Err("the chat reads current with the desk out of reach".to_owned());
    }
    let detached = reopen(journey, &keeper, |drawn| drawn.value("chat.field") == DRAFT)?;
    if !labelled(&detached, "out of reach") || labelled(&detached, SECOND) {
        return Err("the chat opened again is not the cached tail out of reach".to_owned());
    }
    journey.screen("cached-offline", &["chat.row.turn-end"])?;
    journey.record.observations.insert(
        "seconds-out-of-reach".to_owned(),
        json!((away.elapsed().as_secs_f64() * 10.0).round() / 10.0),
    );

    // The desk comes back: the missed turn appends once, and the chat says
    // it is current only once it has.
    journey.request(restart_daemon("desk"), None)?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let drawn = loop {
        let drawn = journey.elements()?;
        if current(&drawn, "keeper") && !labelled(&drawn, SECOND) {
            return Err("the chat read current before the missed turn arrived".to_owned());
        }
        if current(&drawn, "keeper") {
            break drawn;
        }
        if Instant::now() > deadline {
            return Err("the chat did not come back after the desk did".to_owned());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    journey
        .record
        .actions
        .push("reached the missed turn, then a current chat".to_owned());
    if replies(&drawn, SECOND) != 1 || drawn.value("chat.field") != DRAFT {
        return Err("the missed turn did not append once with the draft kept".to_owned());
    }
    journey.tap("chat.send")?;
    journey.wait(
        |drawn| labelled(drawn, THIRD),
        "the reply to the kept draft",
    )?;
    let after = journey.wait_chat(
        "desk",
        "keeper",
        |chat| phase(chat) == "IDLE" && super::holds(chat, THIRD),
        "sent-after-return",
    )?;
    reflected_once(&after, DRAFT)?;
    let control = negative_control(reflected_once(&after, &format!("{DRAFT} (a wrong draft)")))?;
    reopen(journey, &keeper, |drawn| labelled(drawn, THIRD))?;
    journey.screen("reconciled", &["chat.row.turn-end"])?;

    // The desk loses power back to a checkpoint: the phone's replica is
    // Reset, and its rows stay on screen until the rebuilt chat swaps in.
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
    // A restored drive has no running processes, so the rebuilt chat reads
    // the agent exited: it swaps in under the rows on screen, which never
    // leave in between.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let drawn = journey.elements()?;
        if !(labelled(&drawn, FIRST) && labelled(&drawn, THIRD)) {
            return Err("rows left the screen during the Reset".to_owned());
        }
        if labelled(&drawn, "Exited") {
            break;
        }
        if Instant::now() > deadline {
            return Err("the rebuilt chat never swapped in under the open chat".to_owned());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    journey
        .record
        .actions
        .push("the rows stayed on screen until the rebuilt chat swapped in".to_owned());
    reopen(journey, &keeper, |drawn| {
        labelled(drawn, THIRD) && labelled(drawn, "Exited")
    })?;
    journey.screen("after-reset", &["chat.row.turn-end"])?;
    Ok(vec![
        "opened again with the desk out of reach, the app painted the cached chat from its own store, without the turn it never received, and did not read current".to_owned(),
        "the draft typed while out of reach was kept".to_owned(),
        "when the desk came back the missed turn appeared once, and only then did the chat read current".to_owned(),
        "the kept draft reached the desk once and was answered".to_owned(),
        control,
        "rows stayed on screen through the desk's rewind and Reset until the rebuilt chat swapped in, reading the agent exited".to_owned(),
    ])
}

/// The account a person picks from the account disc's menu.
fn switch_to(journey: &mut PhoneJourney, account: &str) -> Result<(), String> {
    journey.tab("Agents")?;
    journey.tap("home.accounts")?;
    journey.choose(account)?;
    journey.wait(
        |drawn| drawn.value("home.subtitle").split(" · ").next() == Some(account),
        &format!("{account} on screen"),
    )?;
    Ok(())
}

/// The link put to the machine again, until the relay link that has just
/// come up (or re-read the account) carries it.
fn pair_by_link(journey: &mut PhoneJourney, link: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match journey.pair(link) {
            Ok(_) => return Ok(()),
            Err(AppError::Refused(error)) => {
                if Instant::now() > deadline {
                    return Err(error);
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Pairs by the link `host` printed. The phone's relay link comes up a
/// moment after signing in, and until it has, the link reaches nobody; the
/// person scans the same code again.
fn pair_through_the_relay(journey: &mut PhoneJourney, host: &str) -> Result<(), String> {
    let (mut pairing, link) = journey.pairing_link(host)?;
    if let Err(error) = pair_by_link(journey, &link) {
        pairing.kill();
        return Err(error);
    }
    pairing.wait(Duration::from_secs(30))
}

/// Signed in to `account` on the served relay, through the door.
fn connect(journey: &mut PhoneJourney, account: &str) -> Result<(), String> {
    let relay = journey.ready.cloud_url.clone().unwrap_or_default();
    journey.app(json!({"kind": "connect", "relay": relay, "token": format!("refresh-{account}"), "user": account}))?;
    Ok(())
}

fn keep_authority(journey: &mut PhoneJourney) -> Proved {
    let asked = "Are you there?";
    let held = "Still there?";
    let desk = journey.host_id("desk");
    let guarded = journey.agent_id("guarded");
    let row = format!("home.row.{guarded}");
    journey.launch(Launch::plain())?;

    // Signed in to the desk's account, the phone pairs with the desk by the
    // link it printed; there is no local network between them, only the
    // relay.
    connect(journey, "ada")?;
    pair_through_the_relay(journey, "desk")?;
    journey.tab("Agents")?;
    journey.wait_for(&[&row])?;

    // A second account is a profile of its own, on its own empty fleet.
    journey.app(json!({"kind": "addAccount", "user": "bob", "token": "refresh-bob"}))?;
    journey.wait(
        |drawn| !drawn.has(&row) && drawn.has("home"),
        "bob's empty fleet",
    )?;
    journey.tab("Hosts")?;
    let drawn = journey.wait(|drawn| drawn.has("hosts"), "bob's hosts")?;
    if drawn.has(&format!("hosts.row.{desk}")) {
        return Err("bob's profile knows the desk".to_owned());
    }
    journey.screen("other-account", &[])?;
    switch_to(journey, "ada")?;
    journey.wait_for(&[&row])?;

    // Through the relay the desk's agent answers.
    journey.tap(&row)?;
    journey.wait_for(&["chat.field"])?;
    send(journey, asked)?;
    journey.wait(
        |drawn| labelled(drawn, "Only this account reaches me."),
        "the first reply",
    )?;
    let reached = journey.wait_chat("desk", "guarded", |chat| phase(chat) == "IDLE", "reached")?;
    reflected_once(&reached, asked)?;
    leave_chat(journey)?;

    // Signed out, the phone has no way to the desk: the chat says so and a
    // message written now never reaches it.
    journey.settings()?;
    journey.wait_for(&["you.signOut"])?;
    journey.tap("you.signOut")?;
    journey.wait(
        |drawn| {
            drawn
                .label("account.ada@example.com")
                .contains("Signed out")
        },
        "signed out",
    )?;
    journey.tap("you.close")?;
    journey.wait(|drawn| !drawn.has("you"), "Settings put away")?;
    journey.tab("Hosts")?;
    journey.wait(
        |drawn| labelled(drawn, "signed out"),
        "the hosts tab naming the sign-out",
    )?;
    journey.screen("blocked-hosts", &[])?;
    journey.tab("Agents")?;
    journey.tap(&row)?;
    journey.wait(
        |drawn| drawn.has("chat.field") && labelled(drawn, "this phone is signed out"),
        "the chat naming the sign-out",
    )?;
    // Send holds the message, pending until the phone can reach the desk.
    send(journey, held)?;
    journey.wait(|drawn| is_held(drawn, held), "the message held")?;
    std::thread::sleep(Duration::from_secs(2));
    // It stays held across leaving the chat; read with the keyboard down.
    leave_chat(journey)?;
    journey.tap(&row)?;
    journey.wait(|drawn| is_held(drawn, held), "the message still held")?;
    journey.screen("blocked", &[])?;
    let blocked = journey.chat("desk", "guarded", "while-signed-out")?;
    never_received(&blocked, held)?;
    let control = negative_control(never_received(&blocked, asked))?;

    // Signed in again: the chat is current and the held message goes by
    // itself, once.
    connect(journey, "ada")?;
    journey.wait(
        |drawn| {
            !labelled(drawn, "this phone is signed out")
                && labelled(drawn, "The relay carries us again.")
                && !is_held(drawn, held)
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
    leave_chat(journey)?;
    journey.tap(&row)?;
    journey.wait(
        |drawn| labelled(drawn, "The relay carries us again.") && drawn.has("chat.row.turn-end"),
        "the chat again",
    )?;
    journey.screen("recovered", &["chat.row.turn-end"])?;
    Ok(vec![
        "a second account opened on its own profile, with an empty fleet that knows no desk".to_owned(),
        format!("through the relay the desk received {} once and answered", quoted(asked)),
        "signed out, the phone named its own sign-out in the hosts tab and the chat, and held the message, across leaving the chat; the desk never received it".to_owned(),
        control,
        format!(
            "signed in again, the held {} went by itself, reached the desk once and was answered",
            quoted(held)
        ),
    ])
}

// --- the phone's own stories ---------------------------------------------
//
// What only a phone does: signing in through the account service, buying
// and restoring at the App Store, reporting a problem, being read at an
// accessibility text size, the local-network permission and a pairing link
// the system hands over, and a push that wakes the app while it is put away.

const RELAYED: &str = "Are you there?";
const THROUGH_RELAY: &str = "Reached through the relay.";

/// What the scripted account service answers: this account, whose relay
/// credential is the served relay's own login for it, at the served relay.
/// `fields` add to or override the script.
fn relay_cloud(journey: &mut PhoneJourney, fields: Value) -> Result<Value, String> {
    let cloud = journey.ready.cloud_url.clone().unwrap_or_default();
    let authority = cloud
        .split_once("://")
        .map_or(cloud.as_str(), |(_, rest)| rest);
    let authority = authority.split('/').next().unwrap_or_default();
    let (host, port) = authority.rsplit_once(':').unwrap_or((authority, ""));
    let mut fields = fields;
    let account = fields
        .as_object_mut()
        .and_then(|fields| fields.remove("account"))
        .and_then(|account| account.as_str().map(str::to_owned))
        .unwrap_or_else(|| "ada".to_owned());
    let mut script = json!({
        "account": account,
        "email": format!("{account}@example.com"),
        "token": format!("refresh-{account}"),
        "relayHost": host,
        "relayPort": port.parse::<u16>().ok(),
    });
    if let (Some(script), Some(fields)) = (script.as_object_mut(), fields.as_object()) {
        script.extend(fields.clone());
    }
    journey.app(json!({"kind": "cloud", "cloud": script}))?;
    journey
        .record
        .actions
        .push(format!("the account service answers {script}"));
    Ok(script)
}

/// The app started by a link, as a code scanned with the camera starts it.
/// (`simctl openurl` would put the system's own Open in "Amux"? sheet in
/// front, which is outside the app and no door can press.) A scripted
/// account service is said again, for the launch to begin from it.
fn open_link(journey: &mut PhoneJourney, link: &str, cloud: Option<&Value>) -> Result<(), String> {
    let mut extra = vec!["-amux-link".to_owned(), link.to_owned()];
    if let Some(cloud) = cloud {
        extra.extend(["-amux-cloud-script".to_owned(), cloud.to_string()]);
    }
    journey.relaunch(Launch {
        extra,
        ..Launch::plain()
    })?;
    journey
        .record
        .actions
        .push("the app opened by the link".to_owned());
    Ok(())
}

fn calls(journey: &mut PhoneJourney, label: &str) -> Result<Value, String> {
    let reply = journey.app(json!({"kind": "calls"}))?;
    journey.record.observations.insert(
        label.to_owned(),
        json!({"cloud": reply["cloud"], "store": reply["store"]}),
    );
    Ok(reply)
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Sign In in Settings, handed off to the account service's page and back,
/// and Settings put away.
fn sign_in(journey: &mut PhoneJourney, photograph: bool) -> Result<(), String> {
    journey.settings()?;
    journey.wait_for(&["you.signIn"])?;
    if photograph {
        journey.screen("signed-out", &[])?;
    }
    journey.tap("you.signIn")?;
    journey.wait(
        |drawn| drawn.enabled("sign-in.continue"),
        "the hand-off offered",
    )?;
    if photograph {
        journey.screen("hand-off", &[])?;
    }
    journey.tap("sign-in.continue")?;
    journey.wait_for(&["sign-in.signed-in"])?;
    if photograph {
        journey.screen("signed-in", &[])?;
    }
    journey.tap("sign-in.continue")?;
    journey.wait(
        |drawn| drawn.value("you") == "ada@example.com" && !drawn.has("sign-in"),
        "ada in Settings",
    )?;
    journey.tap("you.close")?;
    journey.wait(|drawn| !drawn.has("you"), "Settings put away")?;
    Ok(())
}

/// Opens `agent`, sends `prompt` and waits for `reply` on the phone and
/// the host; the chat is opened again for its photograph.
fn round_trip(
    journey: &mut PhoneJourney,
    agent: &str,
    prompt: &str,
    reply: &str,
) -> Result<String, String> {
    let agent_id = open_agent(journey, agent)?;
    send(journey, prompt)?;
    journey.wait(
        |drawn| labelled(drawn, reply) && drawn.has("chat.row.turn-end"),
        &format!("{reply:?} on the phone"),
    )?;
    let heard = journey.wait_chat(
        "desk",
        agent,
        |chat| phase(chat) == "IDLE" && prompts(chat, prompt).len() == 1,
        &format!("{agent}-heard"),
    )?;
    reflected_once(&heard, prompt)?;
    reopen(journey, &agent_id, |drawn| {
        labelled(drawn, reply) && drawn.has("chat.row.turn-end")
    })?;
    Ok(agent_id)
}

fn account_sign_in(journey: &mut PhoneJourney) -> Proved {
    journey.launch(Launch::plain())?;
    relay_cloud(journey, json!({}))?;
    sign_in(journey, true)?;
    let asked = strings(&calls(journey, "account-service-calls")?["cloud"]);
    for expected in ["signIn select", "handOver ada"] {
        if !asked.iter().any(|call| call == expected) {
            return Err(format!(
                "the account service was never asked {expected:?}: {asked:?}"
            ));
        }
    }

    // Signed in, the phone reaches the desk only through the relay the
    // account service named: the desk has no network in common with it.
    pair_through_the_relay(journey, "desk")?;
    journey.tab("Hosts")?;
    let desk = journey.host_id("desk");
    let drawn = journey.wait_for(&[&format!("hosts.row.{desk}")])?;
    trusted_as(&drawn, &desk, "desk")?;
    journey.screen("hosts", &[])?;
    round_trip(journey, "desk-work", RELAYED, THROUGH_RELAY)?;
    journey.screen("reached", &["chat.row.turn-end"])?;
    leave_chat(journey)?;
    Ok(vec![
        "signed out, the You tab offered Sign In; the hand-off named the account service and came back signed in as ada".to_owned(),
        format!("the account service was asked to sign in and to hand over ada's session: {asked:?}"),
        "the relay the hand-over named carried the pairing to the desk, which the phone lists by its own name".to_owned(),
        format!("through that relay the desk received {} once and answered", quoted(RELAYED)),
    ])
}

fn purchase_restore(journey: &mut PhoneJourney) -> Proved {
    let desk = journey.host_id("desk");
    journey.launch(Launch::plain())?;
    let unpaid = relay_cloud(journey, json!({"entitlement": "none"}))?;
    sign_in(journey, false)?;

    // Nothing bought: the relay carries nothing to the desk, so the link it
    // printed, scanned with the camera, is answered with the offer of a
    // subscription rather than called a bad invitation.
    let (pairing, link) = journey.pairing_link("desk")?;
    open_link(journey, &link, Some(&unpaid))?;
    journey.wait_for(&["pair-confirm.subscribe"])?;
    journey.screen("needs-subscription", &[])?;

    // Bought at the App Store while amux.sh cannot be reached: the purchase
    // is kept and the paywall says it is not confirmed.
    journey.app(
        json!({"kind": "store", "store": {"purchase": "bought", "restore": "nothingToRestore"}}),
    )?;
    relay_cloud(
        journey,
        json!({"entitlement": "none", "recordPurchase": "network"}),
    )?;
    journey.tap("pair-confirm.subscribe.buy")?;
    journey.wait(
        |drawn| drawn.has("paywall.restore") && drawn.enabled("paywall.buy"),
        "the paywall with a plan to buy",
    )?;
    journey.screen("paywall", &[])?;
    journey.tap("paywall.buy")?;
    journey.wait_for(&["paywall.unconfirmed"])?;
    journey.screen("unconfirmed", &[])?;

    // amux.sh answers again and takes it; Restore Purchases finds the
    // subscription the App Store holds for this Apple Account and records
    // it. The relay learns what the account bought from amux.sh.
    relay_cloud(journey, json!({"entitlement": "none"}))?;
    journey.request(
        Control::SetTier {
            account: "ada".to_owned(),
            tier: TierDecl::Pro,
        },
        None,
    )?;
    journey.app(json!({"kind": "store", "store": {"purchase": "bought", "restore": "bought"}}))?;
    journey.tap("paywall.restore")?;
    journey.wait_for(&["paywall.subscribed"])?;
    journey.screen("restored", &[])?;
    let record = calls(journey, "purchase-calls")?;
    let (cloud, store) = (strings(&record["cloud"]), strings(&record["store"]));
    let recorded = cloud
        .iter()
        .filter(|call| call.starts_with("recordPurchase"))
        .count();
    if recorded != 2 || !store.iter().any(|call| call == "restore") {
        return Err(format!(
            "the purchase was not offered to amux.sh again by a restore: {record}"
        ));
    }
    journey.tap("paywall.back")?;
    journey.wait(|drawn| !drawn.has("paywall"), "the paywall closed")?;

    // The relay link reads what the account buys again, and the same
    // invitation, still offered by the desk, pairs it through the relay.
    pair_by_link(journey, &link)?;
    pairing.wait(Duration::from_secs(30))?;
    journey.wait(
        |drawn| !drawn.has("pair-confirm"),
        "the pairing sheet closed",
    )?;
    journey.screen("paired", &[])?;
    journey.tab("Hosts")?;
    let drawn = journey.wait_for(&[&format!("hosts.row.{desk}")])?;
    trusted_as(&drawn, &desk, "desk")?;
    round_trip(journey, "desk-work", RELAYED, THROUGH_RELAY)?;
    journey.screen("reached", &["chat.row.turn-end"])?;
    leave_chat(journey)?;
    Ok(vec![
        "signed in with nothing bought, the link the desk printed was answered with the offer of a subscription".to_owned(),
        "bought while amux.sh could not be reached, the paywall kept the purchase and said it was not confirmed".to_owned(),
        format!(
            "Restore Purchases offered it to amux.sh again, which took it: store {store:?}, account service {cloud:?}"
        ),
        format!(
            "subscribed, the same link paired the desk through the relay and the desk received {} once and answered",
            quoted(RELAYED)
        ),
    ])
}

const REPORT_NOTE: &str = "The desk's agent is listed twice.";
const REFUSED_UPLOAD: &str = "amux.sh could not take this report";

/// The app's own data container, which a Mac reads directly.
fn app_data(journey: &PhoneJourney) -> Result<PathBuf, String> {
    Ok(PathBuf::from(
        simctl(
            &["get_app_container", &journey.udid, BUNDLE_ID, "data"],
            Duration::from_secs(120),
        )?
        .trim(),
    ))
}

fn report(journey: &mut PhoneJourney) -> Proved {
    journey.launch(Launch::plain())?;
    connect(journey, "ada")?;
    pair_through_the_relay(journey, "desk")?;
    let agent = journey.agent_id("desk-work");
    journey.tab("Agents")?;
    journey.wait_for(&[&format!("home.row.{agent}")])?;
    journey.app(
        json!({"kind": "cloud", "cloud": {"upload": "refused", "uploadReason": REFUSED_UPLOAD}}),
    )?;

    // The system photographs the app, and the app offers to report what was
    // on screen, over the frame it froze.
    journey.system_screenshot()?;
    journey.wait_for(&["report.prompt"])?;
    journey.screen("offer", &[])?;
    journey.tap("report.prompt")?;
    journey.wait_for(&["report.screen", "report.note"])?;
    journey.type_text("report.note", REPORT_NOTE)?;
    journey.wait(
        |drawn| drawn.value("report.note") == REPORT_NOTE,
        "the note written",
    )?;

    // amux.sh turns it down in its own words; nothing written is lost. Sent
    // once the keyboard has finished rising, so the page the refusal lands
    // on rests at one place.
    journey.app(json!({"kind": "settle"}))?;
    journey.tap("report.send")?;
    let refused = journey.wait_for(&["report.refusal"])?;
    if refused.value("report.refusal") != REFUSED_UPLOAD {
        return Err(format!(
            "the refusal says {:?}",
            refused.get("report.refusal")
        ));
    }
    if journey.elements()?.value("report.note") != REPORT_NOTE {
        return Err("the refusal lost the note".to_owned());
    }
    journey.screen("refused", &[])?;

    // Retry hands over the same report, and it is taken.
    journey
        .app(json!({"kind": "cloud", "cloud": {"upload": "accepted", "receipt": "report-7"}}))?;
    journey.tap("report.send")?;
    let sent = journey.wait_for(&["report.sent"])?;
    if sent.value("report.sent") != "report-7" {
        return Err(format!("the receipt reads {:?}", sent.get("report.sent")));
    }
    journey.screen("sent", &[])?;
    let uploads: Vec<String> = strings(&calls(journey, "report-calls")?["cloud"])
        .into_iter()
        .filter(|call| call.starts_with("uploadReport"))
        .collect();
    if uploads.len() != 2 || uploads[0] != uploads[1] {
        return Err(format!(
            "the retry did not hand over the same report: {uploads:?}"
        ));
    }

    // What left the phone, read where it crossed into the account service.
    let inside = app_data(journey)?.join("tmp/journey-report");
    let bundle = journey.app(json!({"kind": "uploaded", "path": inside}))?;
    let header: Value = serde_json::from_str(bundle["reportJSON"].as_str().unwrap_or_default())
        .map_err(|error| format!("report.json: {error}"))?;
    let parts = strings(&bundle["parts"]);
    journey.record.observations.insert(
        "uploaded-report".to_owned(),
        json!({"parts": bundle["parts"], "report.json": header}),
    );
    if !header.to_string().contains(REPORT_NOTE) {
        return Err(format!("the report does not carry the note: {header}"));
    }
    let screen = inside.join("frame.png");
    if std::fs::metadata(&screen).map_or(true, |metadata| metadata.len() == 0) {
        return Err(format!(
            "the report carries no picture of the screen: {parts:?}"
        ));
    }
    std::fs::copy(&screen, journey.output.join("uploaded-frame.png"))
        .map_err(|error| error.to_string())?;
    // The dump inside it carries this phone's copy of the agent the desk
    // lists, the one whose row the report froze.
    if !parts.contains(&format!("dump/agents/{agent}/row.pb")) {
        return Err(format!(
            "the report's dump holds no copy of the desk's agent: {parts:?}"
        ));
    }
    journey.wait_inventory(
        "desk",
        |agents| agents.iter().any(|item| item["id"] == agent.as_str()),
        "desk-lists-the-reported-agent",
    )?;
    if journey.elements()?.has("report.cancel") {
        journey.tap("report.cancel")?;
    }
    Ok(vec![
        "a screenshot offered Report over the frozen frame; the report opened on it and took a note".to_owned(),
        format!(
            "amux.sh refused the report in its own words ({}) and the note was kept",
            quoted(REFUSED_UPLOAD)
        ),
        format!(
            "Retry handed over the same report, {} parts both times, and came back with receipt report-7",
            parts.len()
        ),
        "the report that left the phone carries the note, the frozen screen and a dump holding the agent the desk lists".to_owned(),
    ])
}

const ACCESSIBLE_SIZE: &str = "accessibility3";

fn accessibility(journey: &mut PhoneJourney) -> Proved {
    journey.launch(Launch::plain())?;
    journey.app(json!({"kind": "dynamicType", "size": ACCESSIBLE_SIZE}))?;
    let size = journey.query()?["typeSize"].clone();
    if size != ACCESSIBLE_SIZE {
        return Err(format!("the app says it draws at {size}"));
    }
    pair_by_code(journey, "desk")?;
    let agent = open_agent(journey, "decision-sdk")?;
    journey.screen("chat", &[])?;
    send(journey, PROMPT)?;
    let card = journey.wait(
        |drawn| drawn.has("ask") && labelled(drawn, "deploy --check") && labelled(drawn, "Allow"),
        "the permission card",
    )?;
    journey.screen("permission", &[])?;
    journey.tap(&choice(&card, "Allow")?)?;
    journey.wait(
        |drawn| !drawn.has("ask") && labelled(drawn, REPLY),
        "the settled turn",
    )?;
    let settled = journey.wait_chat(
        "desk",
        "decision-sdk",
        |chat| phase(chat) == "IDLE" && super::holds(chat, REPLY),
        "turn-settled",
    )?;
    reflected_once(&settled, PROMPT)?;
    let lines = journey.provider_input("decision-sdk", "provider-input")?;
    if lines
        .iter()
        .filter(|line| line.to_lowercase().contains("allow"))
        .count()
        != 1
    {
        return Err(format!("the provider did not receive one allow: {lines:?}"));
    }
    reopen(journey, &agent, |drawn| {
        labelled(drawn, REPLY) && drawn.has("chat.row.turn-end")
    })?;
    // The command's row says how long it ran, which at this size is wide
    // enough to move more than the comparison allows; the desk's record of
    // the call is checked above.
    journey.screen("settled", &["chat.row.turn-end", "chat.row.command"])?;

    // Hosts and Settings at the same size: every control whole across the
    // screen, and the Appearance control's choices clear of one another.
    leave_chat(journey)?;
    journey.tab("Hosts")?;
    let row = format!("hosts.row.{}", journey.host_id("desk"));
    let hosts = journey.wait_for(&[&row])?;
    across_the_screen(&hosts, "hosts.")?;
    journey.screen("hosts", &[])?;
    journey.settings()?;
    let choices = [
        "you.appearance.light",
        "you.appearance.dark",
        "you.appearance.system",
    ];
    let settings = journey.wait_for(&choices)?;
    across_the_screen(&settings, "you.")?;
    apart(&settings, &choices)?;
    let control = negative_control(apart(&settings, &["you.appearance", choices[0]]))?;
    journey.screen("you", &[])?;
    Ok(vec![
        format!("the app says it draws at {ACCESSIBLE_SIZE}"),
        "at that size the desk was paired on the keypad and the agent's chat opened".to_owned(),
        format!(
            "the permission card offered Allow, which was tapped; the desk holds {} once and answered",
            quoted(PROMPT)
        ),
        "the provider received exactly one allow".to_owned(),
        "at that size every control on Hosts and Settings lies whole across the screen, and the Appearance choices are clear of one another".to_owned(),
        control,
    ])
}

/// An element's rectangle as its left, top, right and bottom edges, in
/// points; None when it reports no frame.
fn rectangle(drawn: &Drawn, name: &str) -> Option<[f64; 4]> {
    let frame = drawn.field(name, "frame");
    let (x, y) = (frame["x"].as_f64()?, frame["y"].as_f64()?);
    Some([
        x,
        y,
        x + frame["width"].as_f64()?,
        y + frame["height"].as_f64()?,
    ])
}

/// No element named under `prefix` reaches past either side of the screen,
/// as a control pushed off the edge by large text would. The screen is the
/// shell's width.
fn across_the_screen(drawn: &Drawn, prefix: &str) -> Result<(), String> {
    let width = rectangle(drawn, "shell").ok_or("the shell reports no frame")?[2];
    let cut: Vec<String> = drawn
        .names()
        .filter(|name| name.starts_with(prefix))
        .filter_map(|name| {
            let [left, _, right, _] = rectangle(drawn, name)?;
            (left < -0.5 || right > width + 0.5).then(|| format!("{name} at {left}..{right}"))
        })
        .collect();
    if cut.is_empty() {
        Ok(())
    } else {
        Err(format!("past the {width} pt screen: {}", cut.join(", ")))
    }
}

/// None of the named elements is drawn over another, beyond the point
/// their shared edges round to.
fn apart(drawn: &Drawn, names: &[&str]) -> Result<(), String> {
    for (at, first) in names.iter().enumerate() {
        for second in &names[at + 1..] {
            let (Some(a), Some(b)) = (rectangle(drawn, first), rectangle(drawn, second)) else {
                return Err(format!("{first} or {second} reports no frame"));
            };
            let across = a[2].min(b[2]) - a[0].max(b[0]);
            let down = a[3].min(b[3]) - a[1].max(b[1]);
            if across > 1.0 && down > 1.0 {
                return Err(format!("{first} is drawn over {second}"));
            }
        }
    }
    Ok(())
}

fn local_network(journey: &mut PhoneJourney) -> Proved {
    let desk = journey.host_id("desk");
    let offer = format!("hosts.pair.{desk}");
    // Refused, the system's browser finds nothing, which the launch stands
    // in for by letting it report no machine.
    journey.launch(Launch {
        found: Some(Vec::new()),
        ..Launch::plain()
    })?;

    // Refused: the Hosts tab says so and where it is undone.
    journey.app(json!({"kind": "localNetwork", "permission": "denied"}))?;
    journey
        .record
        .actions
        .push("the system says the local network was refused".to_owned());
    journey.tab("Hosts")?;
    let drawn = journey.wait_for(&["hosts.localNetwork.refused", "hosts.localNetwork.settings"])?;
    if drawn.has(&offer) {
        return Err("the phone offers the desk with the local network refused".to_owned());
    }
    journey.screen("refused", &[])?;

    // Turned on in Settings, which ends the app, and opened again: the desk
    // is found and offered.
    journey.relaunch(Launch::plain())?;
    journey
        .record
        .actions
        .push("the local network granted in Settings and the app opened again".to_owned());
    journey.tab("Hosts")?;
    journey.wait(
        |drawn| drawn.has(&offer) && !drawn.has("hosts.localNetwork.refused"),
        "the desk found",
    )?;
    journey.screen("granted", &[])?;

    // The link the desk printed, scanned with the camera, starts the app on
    // the desk's name and key before anything is trusted.
    let (pairing, link) = journey.pairing_link("desk")?;
    open_link(journey, &link, None)?;
    // The relaunch the link starts the app with finds the desk again behind
    // the sheet; the screen is read once it has.
    let card = journey.wait(
        |drawn| {
            drawn.has("pair-confirm.trust")
                && drawn.has("pair-confirm.fingerprint")
                && labelled(drawn, "desk is nearby")
        },
        "the link's card over the desk found",
    )?;
    if card.value("pair-confirm.name") != "desk" {
        return Err(format!(
            "the link reached {:?}, not the desk",
            card.get("pair-confirm.name")
        ));
    }
    journey.screen(
        "link-confirm",
        &["pair-confirm.fingerprint", "pair-confirm.expiry"],
    )?;
    journey.tap("pair-confirm.trust")?;
    journey.wait(
        |drawn| !drawn.has("pair-confirm"),
        "the pairing sheet closed",
    )?;
    pairing.wait(Duration::from_secs(30))?;
    journey.tab("Hosts")?;
    let drawn = journey.wait_for(&[&format!("hosts.row.{desk}")])?;
    trusted_as(&drawn, &desk, "desk")?;
    round_trip(
        journey,
        "desk-work",
        "Hello from the phone.",
        "The desk is reachable.",
    )?;
    journey.screen("reached", &["chat.row.turn-end"])?;
    leave_chat(journey)?;
    Ok(lines(&[
        "with the local network refused the Hosts tab said so, offered Settings and listed no machine",
        "granted, the phone's browser found the desk and offered it",
        "the desk's amux://pair link started the app on the desk's name and key before trust, and paired it",
        "the desk received 'Hello from the phone.' once and answered",
    ]))
}

/// A push brings its chat current within this long, or gives up.
const WARM_LIMIT: Duration = Duration::from_secs(25);

/// The committed needs-you payload, addressed to `agent` on the desk.
fn needs_you_push(journey: &PhoneJourney, agent: &str) -> Result<PathBuf, String> {
    let mut payload = read_json(&root().join("journeys/fixtures/needs-you.apns"))?;
    payload["amux"]["host"] = json!(journey.host_id("desk"));
    payload["amux"]["agent"] = json!(journey.agent_id(agent));
    let built = journey.output.join("needs-you.apns");
    std::fs::write(
        &built,
        serde_json::to_string_pretty(&payload).unwrap_or_default() + "\n",
    )
    .map_err(|error| error.to_string())?;
    Ok(built)
}

/// Deliver a push to the app put away, the notification and the payload.
///
/// `xcrun simctl push` shows the notification a person sees. It never
/// reaches the app's didReceiveRemoteNotification on this simulator (iOS
/// 26.5): a backgrounded app only gets a payload-less background-fetch
/// launch. So the debug door hands the same file to that handler, with the
/// app in the background as iOS would call it, and answers when the handler
/// returns.
fn push_to_handler(journey: &mut PhoneJourney, payload: &Path) -> Result<(), String> {
    simctl(
        &[
            "push",
            &journey.udid,
            BUNDLE_ID,
            payload.to_string_lossy().as_ref(),
        ],
        Duration::from_secs(120),
    )?;
    journey
        .record
        .actions
        .push("xcrun simctl push needs-you.apns: the notification shows".to_owned());
    journey.app_within(
        json!({"kind": "push", "path": payload}),
        WARM_LIMIT + Duration::from_secs(15),
    )?;
    journey.record.actions.push(
        "the debug door handed the same needs-you.apns payload to AppDelegate's didReceiveRemoteNotification with the app in the background".to_owned(),
    );
    Ok(())
}

fn signposts(journey: &PhoneJourney) -> Result<Vec<String>, String> {
    Ok(journey.app(json!({"kind": "signposts"}))?["marks"]
        .as_array()
        .map(|marks| {
            marks
                .iter()
                .filter_map(|mark| mark["signpost"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default())
}

fn push_wake(journey: &mut PhoneJourney) -> Proved {
    let asker = journey.agent_id("asker");
    let bystander = journey.agent_id("bystander");
    let (asker_row, bystander_row) = (format!("home.row.{asker}"), format!("home.row.{bystander}"));
    journey.launch(Launch::plain())?;
    pair_by_code(journey, "desk")?;
    journey.tab("Agents")?;
    journey.wait_for(&[&asker_row, &bystander_row])?;

    // Put away. Both agents move on at the desk: one comes to need the
    // person, the other answers. The app asks for background time first, as
    // an app finishing a piece of work does, so it is still there to hand
    // the push to: this simulator never passes a push's payload to an app
    // put away (see push_to_handler).
    journey.app(json!({"kind": "holdBackground"}))?;
    simctl(
        &["launch", &journey.udid, "com.apple.Preferences"],
        Duration::from_secs(120),
    )?;
    journey.app(json!({"kind": "awaitBackground", "seconds": 10}))?;
    journey
        .record
        .actions
        .push("the app put away behind Settings, holding background time for the push".to_owned());
    journey.request(
        Control::Send {
            agent: "asker".to_owned(),
            text: "Deploy it.".to_owned(),
        },
        None,
    )?;
    journey.request(
        Control::Send {
            agent: "bystander".to_owned(),
            text: "Anything new?".to_owned(),
        },
        None,
    )?;
    journey.wait_chat(
        "desk",
        "asker",
        |chat| phase(chat) == "NEEDS_YOU",
        "asker-needs-you",
    )?;
    journey.wait_chat(
        "desk",
        "bystander",
        |chat| phase(chat) == "IDLE" && super::holds(chat, "The logs are quiet."),
        "bystander-answered",
    )?;

    // The push names the asker. Handled in the background, it brings that
    // one chat current and nothing else.
    let payload = needs_you_push(journey, "asker")?;
    push_to_handler(journey, &payload)?;

    // With the desk gone, what the phone holds is what the push fetched.
    journey.request(stop_daemon("desk"), None)?;
    simctl(
        &["launch", &journey.udid, BUNDLE_ID],
        Duration::from_secs(120),
    )?;
    journey
        .record
        .actions
        .push("the app brought back to the foreground".to_owned());
    let woke: Vec<String> = signposts(journey)?
        .into_iter()
        .filter(|mark| mark.starts_with("push"))
        .collect();
    journey
        .record
        .observations
        .insert("push-signposts".to_owned(), json!(woke));
    if woke != ["pushWoke", "pushCurrent"] {
        return Err(format!(
            "the push did not bring its chat current: the app marked {woke:?}"
        ));
    }
    journey.wait(
        |drawn| labelled(drawn, "desk is offline"),
        "the desk offline on the phone",
    )?;
    journey.tap(&asker_row)?;
    journey.wait(
        |drawn| drawn.has("ask") && labelled(drawn, "deploy --prod"),
        "the asker's ask, fetched by the push",
    )?;
    journey.screen("warmed", &["chat.row.turn-end"])?;
    back_to_fleet(journey)?;
    journey.tap(&bystander_row)?;
    let drawn = journey.wait(
        |drawn| labelled(drawn, "Watching the logs.") && drawn.has("chat.field"),
        "the bystander's chat",
    )?;
    if labelled(&drawn, "The logs are quiet.") {
        return Err(
            "the bystander's chat was brought current by a push that named the asker".to_owned(),
        );
    }
    journey.screen("not-warmed", &["chat.row.turn-end"])?;
    leave_chat(journey)?;

    // In the foreground every agent is listed again: the desk back, the
    // bystander's answer arrives, and the ask is answered from the phone.
    journey.request(restart_daemon("desk"), None)?;
    journey.tap(&bystander_row)?;
    journey.wait_within(
        |drawn| labelled(drawn, "The logs are quiet."),
        "the bystander current in the foreground",
        Duration::from_secs(90),
    )?;
    back_to_fleet(journey)?;
    journey.tap(&asker_row)?;
    let card = journey.wait(
        |drawn| drawn.has("ask") && labelled(drawn, "Allow"),
        "the ask again",
    )?;
    journey.tap(&choice(&card, "Allow")?)?;
    journey.wait(
        |drawn| labelled(drawn, "Deployed to production."),
        "the deploy reply",
    )?;
    journey.wait_chat(
        "desk",
        "asker",
        |chat| phase(chat) == "IDLE" && super::holds(chat, "Deployed to production."),
        "asker-answered",
    )?;
    reopen(journey, &asker, |drawn| {
        labelled(drawn, "Deployed to production.") && drawn.has("chat.row.turn-end")
    })?;
    // The command row's duration is how long the ask waited on the phone.
    journey.screen("answered", &["chat.row.turn-end", "chat.row.command"])?;
    journey.tap("chat.back")?;
    Ok(lines(&[
        "put away, the desk's asker came to need the person and the bystander answered",
        "a push built from journeys/fixtures/needs-you.apns showed its notification, and its payload, handed to the app's remote-notification handler through the debug door with the app in the background, brought the asker's chat current: with the desk stopped it held the ask",
        "the bystander's chat did not hold the answer it gave while the app was away: only the named chat was brought current",
        "back in the foreground with the desk running again, the bystander caught up without being named",
        "Allow from the phone reached the desk, which finished the deploy",
    ]))
}

// The resume story: three launches of one installation, each read back
// from the link log the phone's runtime writes.

const RESUME_HOSTS: [&str; 2] = ["desk", "laptop"];
/// Where each launch's link log is kept, named by the launch.
const LINK_LOGS: &str = "target/ios/journey/resume";
const LINK_LINE: &str = "amux::link: link ";

/// The log the phone's runtime writes, in the installation's directory.
fn runtime_log(journey: &PhoneJourney) -> Result<PathBuf, String> {
    Ok(app_data(journey)?.join("Library/Application Support/amux/installation/runtime.log"))
}

/// The link log's lines written since `offset`, the log's length before the
/// launch. A log cut to its newest part since then is read whole.
fn logged_since(journey: &PhoneJourney, offset: u64) -> Result<Vec<String>, String> {
    let Ok(data) = std::fs::read(runtime_log(journey)?) else {
        return Ok(Vec::new());
    };
    let offset = usize::try_from(offset).unwrap_or(usize::MAX);
    let offset = if data.len() < offset { 0 } else { offset };
    Ok(String::from_utf8_lossy(&data[offset..])
        .lines()
        .filter(|line| line.contains(LINK_LINE))
        .map(str::to_owned)
        .collect())
}

/// A link line's `key=value` fields.
fn link_fields(line: &str) -> BTreeMap<String, String> {
    let said = line.split_once(LINK_LINE).map_or("", |(_, said)| said);
    said.split_whitespace()
        .filter_map(|field| field.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

/// The lines for links to the machines that say `event`, by machine name in
/// the order written: the relay's own link is not a machine's.
fn machine_lines(
    journey: &PhoneJourney,
    lines: &[String],
    event: &str,
) -> Vec<(String, BTreeMap<String, String>)> {
    let names: BTreeMap<String, &str> = RESUME_HOSTS
        .iter()
        .map(|host| (journey.host_id(host), *host))
        .collect();
    lines
        .iter()
        .map(|line| link_fields(line))
        .filter_map(|fields| {
            let name = names.get(fields.get("peer")?)?;
            (fields.get("event").map(String::as_str) == Some(event))
                .then(|| ((*name).to_owned(), fields))
        })
        .collect()
}

type Ups = BTreeMap<String, BTreeMap<String, String>>;

/// The first link to come up to each machine, by machine name, with the
/// milliseconds its catch-up took from the first packet where it carried
/// one.
fn first_ups(journey: &PhoneJourney, lines: &[String]) -> Ups {
    let mut ups = Ups::new();
    for (name, fields) in machine_lines(journey, lines, "up") {
        ups.entry(name).or_insert(fields);
    }
    for (name, fields) in machine_lines(journey, lines, "caught_up") {
        if let Some(up) = ups.get_mut(&name) {
            up.insert(
                "first_packet_to_catch_up_ms".to_owned(),
                fields
                    .get("first_packet_to_catch_up_ms")
                    .cloned()
                    .unwrap_or_default(),
            );
        }
    }
    ups
}

fn log_length(journey: &PhoneJourney) -> Result<u64, String> {
    Ok(std::fs::metadata(runtime_log(journey)?).map_or(0, |metadata| metadata.len()))
}

/// Waits for the launch to reconcile with every machine and for its link
/// log to name a link to each, then keeps that log as `<launch>.log`.
fn reconciled_launch(journey: &mut PhoneJourney, launch: &str, offset: u64) -> Result<Ups, String> {
    journey.app_within(
        json!({"kind": "awaitReconciled", "seconds": 60}),
        Duration::from_secs(70),
    )?;
    let deadline = Instant::now() + Duration::from_secs(60);
    while !signposts(journey)?.iter().any(|mark| mark == "reconciled") {
        if Instant::now() > deadline {
            return Err(format!(
                "the {launch} launch never read every machine current"
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    journey
        .record
        .actions
        .push(format!("the {launch} launch read every machine current"));
    let deadline = Instant::now() + Duration::from_secs(30);
    let (lines, ups) = loop {
        let lines = logged_since(journey, offset)?;
        let ups = first_ups(journey, &lines);
        if ups.len() == RESUME_HOSTS.len() {
            break (lines, ups);
        }
        if Instant::now() > deadline {
            let named: Vec<&String> = ups.keys().collect();
            return Err(format!(
                "the {launch} launch logged links to {named:?} only: {lines:?}"
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    std::fs::write(
        root().join(LINK_LOGS).join(format!("{launch}.log")),
        lines.join("\n") + "\n",
    )
    .map_err(|error| error.to_string())?;
    journey
        .record
        .observations
        .insert(format!("{launch}-links"), json!(ups));
    Ok(ups)
}

/// The machine restarted, as a person restarts amux on it: its daemon
/// killed and started again on the same disk.
fn restart(journey: &mut PhoneJourney, host: &str) -> Result<(), String> {
    journey.request(restart_daemon(host), None)?;
    journey.wait_inventory(host, |agents| !agents.is_empty(), &format!("{host}-back"))?;
    Ok(())
}

fn field<'a>(fields: &'a BTreeMap<String, String>, key: &str) -> &'a str {
    fields.get(key).map_or("", String::as_str)
}

/// The launch's links in words: each machine, its path and how it resumed.
fn spelled(launch: &Ups) -> String {
    launch
        .iter()
        .map(|(host, fields)| {
            let caught_up = match field(fields, "first_packet_to_catch_up_ms") {
                "none" => String::new(),
                ms => format!(", caught up {ms} ms after its first packet"),
            };
            format!(
                "{host} over {} (resumed={}, early data {}{caught_up})",
                field(fields, "path"),
                field(fields, "resumed"),
                field(fields, "early_data")
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn resume(journey: &mut PhoneJourney) -> Proved {
    let logs = root().join(LINK_LOGS);
    let _ = std::fs::remove_dir_all(&logs);
    std::fs::create_dir_all(&logs).map_err(|error| error.to_string())?;

    // The first launch ever: nothing kept, so every link is a full
    // handshake.
    journey.launch(Launch::plain())?;
    connect(journey, "ada")?;
    journey.record.actions.push("signed in as ada".to_owned());
    for host in RESUME_HOSTS {
        pair_by_code(journey, host)?;
    }
    let first = reconciled_launch(journey, "first", 0)?;
    for (host, fields) in machine_lines(journey, &logged_since(journey, 0)?, "up") {
        if field(&fields, "resumed") != "false" || field(&fields, "early_data") != "none" {
            return Err(format!(
                "the first launch ever reached {host} on a resumed session: {fields:?}"
            ));
        }
    }

    // Closed and opened again: every machine resumed, its early data taken.
    journey.quit()?;
    let offset = log_length(journey)?;
    journey.relaunch(Launch::plain())?;
    let relaunch = reconciled_launch(journey, "relaunch", offset)?;
    for (host, fields) in &relaunch {
        if field(fields, "resumed") != "true" || field(fields, "early_data") != "accepted" {
            return Err(format!(
                "the relaunch did not resume {host} with its early data accepted: {fields:?}"
            ));
        }
    }

    // One machine restarted while the phone is closed: its session tickets
    // are kept on its disk, so the next launch still resumes it.
    journey.quit()?;
    restart(journey, "desk")?;
    let offset = log_length(journey)?;
    journey.relaunch(Launch::plain())?;
    let after = reconciled_launch(journey, "after-host-restart", offset)?;
    for (host, fields) in &after {
        if field(fields, "resumed") != "true" {
            return Err(format!(
                "after the desk restarted the launch did not resume {host}: {fields:?}"
            ));
        }
    }
    Ok(vec![
        format!(
            "the first launch ever reached both machines by full handshakes on every link: {}",
            spelled(&first)
        ),
        format!(
            "the relaunch resumed both with their early data accepted: {}",
            spelled(&relaunch)
        ),
        format!(
            "with the desk restarted while the phone was closed, the next launch still resumed both: {}",
            spelled(&after)
        ),
        format!("each launch's link log is kept in {LINK_LOGS}"),
    ])
}

fn stop_daemon(host: &str) -> Control {
    Control::StopDaemon {
        host: host.to_owned(),
    }
}

fn restart_daemon(host: &str) -> Control {
    Control::RestartDaemon {
        host: host.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::phone::tests::{DESK, element};
    use super::*;

    const NATIVE: [&str; 7] = [
        "account-sign-in",
        "purchase-restore",
        "report",
        "accessibility",
        "local-network",
        "push-wake",
        "resume",
    ];

    #[test]
    fn a_paired_host_is_recognised_by_id_and_name_and_a_wrong_name_fails() {
        let drawn = Drawn::of(&[element(
            &format!("hosts.row.{DESK}"),
            [0.0; 4],
            Some("desk, reachable on this network"),
        )]);
        assert!(trusted_as(&drawn, DESK, "desk").is_ok());
        assert!(trusted_as(&drawn, DESK, "laptop").is_err());
        assert!(trusted_as(&Drawn::default(), DESK, "desk").is_err());
    }

    #[test]
    fn a_review_is_read_as_text_then_paste_then_review_with_its_one_comment() {
        let comment = json!({"path": "deploy.sh", "line": 3, "old_line": 0, "text": COMMENT});
        let sent = json!({
            "text": "Please check \u{fffc} against",
            "attachments": [
                {"kind": "text", "text": pasted()},
                {"kind": "review", "comments": [comment], "patch": "ab"},
            ],
        });
        let chat = json!({"items": [{"text": "earlier", "attachments": []}, sent]});
        assert_eq!(reviewed(&chat, COMMENT), Ok(sent.clone()));
        assert!(reviewed(&chat, "another comment").is_err());
        let mut swapped = sent.clone();
        swapped["attachments"]
            .as_array_mut()
            .expect("attachments")
            .reverse();
        assert!(reviewed(&json!({"items": [swapped]}), COMMENT).is_err());
        assert!(reviewed(&json!({"items": [sent, sent]}), COMMENT).is_err());
    }

    #[test]
    fn queued_prompts_read_oldest_first() {
        let drawn = Drawn::of(&[
            json!({"identifier": "chat.queued.1", "label": "b", "value": "queued"}),
            json!({"identifier": "chat.queued.0", "label": "a", "value": "steered"}),
            json!({"identifier": "chat.queued.0.withdraw", "label": "Withdraw"}),
        ]);
        assert_eq!(
            queued(&drawn),
            [
                ("a".to_owned(), "steered".to_owned()),
                ("b".to_owned(), "queued".to_owned())
            ]
        );
    }

    #[test]
    fn a_link_line_names_its_fields() {
        let fields = link_fields(
            "2026 INFO amux::link: link peer=abc event=up resumed=true early_data=accepted",
        );
        assert_eq!(field(&fields, "peer"), "abc");
        assert_eq!(field(&fields, "early_data"), "accepted");
    }

    #[test]
    fn every_phone_story_written_is_declared_for_the_phone() {
        for (name, _) in STORIES {
            let declared = super::super::story(name, "phone").expect("declared for the phone");
            let topology = declared
                .phone_topology
                .or(declared.topology)
                .expect("a topology");
            assert!(root().join(&topology).exists(), "{topology}");
        }
    }

    #[test]
    fn native_runs_exactly_the_stories_declared_for_the_phone_alone() {
        assert_eq!(
            selected(true, Vec::new()),
            Ok(NATIVE.map(str::to_owned).to_vec())
        );
    }

    #[test]
    fn names_run_those_and_nothing_runs_every_story() {
        assert_eq!(
            selected(false, vec!["reach-host".to_owned()]),
            Ok(vec!["reach-host".to_owned()])
        );
        assert_eq!(
            selected(false, Vec::new()),
            Ok(STORIES.iter().map(|(id, _)| (*id).to_owned()).collect())
        );
        assert!(selected(false, vec!["no-such-story".to_owned()]).is_err());
    }

    fn retired() -> Vec<Value> {
        let manifest = read_json(&root().join("journeys/manifest.json")).expect("the manifest");
        manifest["retired_phone_journeys"]["journeys"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    #[test]
    fn every_retired_phone_journey_names_stories_that_carry_it() {
        let declared: Vec<String> = super::super::manifest()
            .expect("manifest")
            .into_iter()
            .map(|story| story.id)
            .collect();
        let retired = retired();
        assert!(!retired.is_empty());
        for journey in retired {
            let id = text(&journey["id"]);
            // A story written again under its old name carries its own claim.
            if declared.contains(&id) {
                assert_eq!(journey["carried_by"], json!([id]));
            }
            assert!(!text(&journey["how"]).is_empty(), "{id}");
            for carrier in journey["carried_by"].as_array().into_iter().flatten() {
                assert!(STORIES.iter().any(|(name, _)| carrier == name), "{id}");
            }
        }
    }

    #[test]
    fn every_retired_claim_is_carried_by_a_story_step_or_dropped_with_why() {
        let goldens = root().join("journeys/goldens/phone");
        for journey in retired() {
            let id = text(&journey["id"]);
            let claims = journey["claims"].as_array().cloned().unwrap_or_default();
            let dropped = journey["dropped"].as_array().cloned().unwrap_or_default();
            assert!(!claims.is_empty() || !dropped.is_empty(), "{id}");
            let mut carriers: Vec<String> = Vec::new();
            for claim in &claims {
                assert!(!text(&claim["claim"]).is_empty(), "{id}");
                let step = text(&claim["step"]);
                let (story, screen) = step.split_once('/').expect("story/screen");
                assert!(STORIES.iter().any(|(name, _)| *name == story), "{step}");
                // A step is a screen the story compares.
                assert!(
                    goldens
                        .join(story)
                        .join(format!("{screen}.elements.txt"))
                        .exists(),
                    "{step}"
                );
                if !carriers.iter().any(|carrier| carrier == story) {
                    carriers.push(story.to_owned());
                }
            }
            assert_eq!(journey["carried_by"], json!(carriers), "{id}");
            for dropped in dropped {
                assert!(
                    !text(&dropped["claim"]).is_empty() && !text(&dropped["why"]).is_empty(),
                    "{id}"
                );
            }
        }
    }

    #[test]
    fn the_needs_you_push_carries_what_the_app_reads() {
        let payload =
            read_json(&root().join("journeys/fixtures/needs-you.apns")).expect("the payload");
        assert_eq!(payload["aps"]["content-available"], 1);
        let keys: Vec<&String> = payload["amux"].as_object().expect("amux").keys().collect();
        assert_eq!(keys, ["agent", "host"]);
    }
}
