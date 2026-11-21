//! The phone's stories about agents a host runs over the Agent Client
//! Protocol: created from New Agent by the name the host gives the agent,
//! and driven as a person drives Claude or Codex, except where the agent
//! cannot do something (reopen a conversation, take a prompt into a
//! running turn), which the phone must not offer.
//!
//! Every host in `journeys/topologies/phone-acp.json` runs the fake ACP
//! agent, which the net offers as an agent a person added in config, under
//! the name "Fake ACP Agent"; each host's script plays one story.

use serde_json::{Value, json};

use super::common::*;
use super::phone::{Drawn, Launch, PhoneJourney, labelled};
use super::phone_stories::{
    choice, open_settled, pair_by_code, photograph_running, queued, reopen, replies, send,
};
use super::{Proved, negative_control, never_sent, phase, quoted, reflected_once};
use crate::FAKE_ACP_AGENT;

/// The name the net's hosts give the fake ACP agent, as a person who added
/// it to their config would.
const AGENT_NAME: &str = "Fake ACP Agent";

const HELLO_ACP: &str = "Say hello.";
const HELLO_REPLY: &str = "Hello from the fake agent.";
const RUN_SUITE: &str = "Run the tests.";
const SUITE_PASSED: &str = "The suite passed.";
const CLEAN: &str = "Clean the build folder.";
const LEFT_ALONE: &str = "I left the build folder alone.";

const LONG_JOB: &str = "Start the long job.";
const LONG_STARTED: &str = "Starting the long job.";
const NEVER_SAID: &str = "This is never said.";
const SOAK: &str = "Run the soak.";
const SOAK_RUNNING: &str = "Running the soak.";
const SOAK_DONE: &str = "The soak finished.";
const CHECK_LOGS: &str = "Then check the logs.";
const WITHDRAWN_ACP: &str = "And email me.";
const NOTED: &str = "Noted, checking the logs next.";

const ONE_THING: &str = "Do one thing.";
const DONE_HERE: &str = "Done here.";

const ARE_YOU_THERE: &str = "Are you there?";
const SIGNED_IN_REPLY: &str = "Signed in and ready.";

/// New Agent on `host`, the fake ACP agent picked by its name, started in
/// the directory the sheet offers under the name `name` (left unnamed, the
/// host would pick a random one, which no reviewed screen could hold): the
/// agent the host lists that it did not before, with its chat open.
fn create(journey: &mut PhoneJourney, host: &str, name: &str) -> Result<String, String> {
    let before = ids(&journey.wait_inventory(host, |_| true, "inventory-before")?);
    journey.tab("Agents")?;
    journey.tap("home.newAgent")?;
    let drawn = journey.wait_for(&["new-agent"])?;
    if drawn.value("new-agent.host") != host {
        journey.tap("new-agent.host")?;
        journey.choose(host)?;
    }
    // The agents are the host's to list; until it has, the sheet says it is
    // asking.
    let drawn = journey.wait(
        |drawn| {
            drawn.value("new-agent.host") == host
                && drawn.has("new-agent.provider")
                && !drawn.has("new-agent.agents.asking")
        },
        "the host's agents",
    )?;
    if drawn.value("new-agent.provider") != FAKE_ACP_AGENT {
        journey.tap("new-agent.provider")?;
        journey.choose(AGENT_NAME)?;
    }
    journey.wait(
        |drawn| {
            drawn.value("new-agent.provider") == FAKE_ACP_AGENT
                && drawn.label("new-agent.provider") == format!("Agent, {AGENT_NAME}")
                && !drawn.value("new-agent.directory").is_empty()
        },
        "the agent picked by its name, and a directory",
    )?;
    journey.type_text("new-agent.name", name)?;
    journey.wait(
        |drawn| drawn.value("new-agent.name") == name,
        "the name typed",
    )?;
    journey.tap("new-agent.start")?;
    let created = journey.wait_inventory(
        host,
        |agents| ids(agents).difference(&before).count() == 1,
        "created",
    )?;
    let id = ids(&created)
        .difference(&before)
        .next()
        .cloned()
        .unwrap_or_default();
    let agent = listed(&created, &id).cloned().unwrap_or_default();
    if agent["kind"] != "ACP" || agent["provider"] != FAKE_ACP_AGENT || agent["name"] != name {
        return Err(format!(
            "New Agent made something other than the fake ACP agent: {agent}"
        ));
    }
    journey.wait_for(&["chat.field"])?;
    Ok(id)
}

/// The permission card for the command `subject`, once it offers exactly
/// `labels`: the agent's options, each as a button or a row, with its
/// one-time refusal behind Deny…, where the person may say why instead.
fn permission_card(
    journey: &mut PhoneJourney,
    subject: &str,
    labels: &[&str],
) -> Result<Drawn, String> {
    let offered = |drawn: &Drawn| -> Vec<String> {
        let mut choices: Vec<String> = drawn
            .iter()
            .filter(|(name, _)| name.starts_with("ask.choice.") || *name == "ask.deny")
            .map(|(_, element)| element["label"].as_str().unwrap_or_default().to_owned())
            .collect();
        choices.sort();
        choices
    };
    let card = journey.wait(
        |drawn| {
            drawn.has("ask") && drawn.label("ask.subject") == subject && !offered(drawn).is_empty()
        },
        "the permission card",
    )?;
    let got = offered(&card);
    let mut wanted: Vec<String> = labels.iter().map(|label| (*label).to_owned()).collect();
    wanted.sort();
    if got != wanted {
        return Err(format!(
            "the card for {subject:?} offers {got:?}, not the agent's {wanted:?}"
        ));
    }
    Ok(card)
}

/// The answers the agent read to its permission requests, by the option
/// each picked, in order.
fn permission_answers(journey: &mut PhoneJourney, host: &str) -> Result<Vec<String>, String> {
    let lines = journey.provider_input(host, "agent-input")?;
    Ok(lines
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|frame| frame.get("method").is_none())
        .filter_map(|frame| {
            let outcome = &frame["result"]["outcome"];
            match outcome["outcome"].as_str()? {
                "selected" => outcome["optionId"].as_str().map(str::to_owned),
                other => Some(other.to_owned()),
            }
        })
        .collect())
}

/// The methods the agent was sent, in order.
fn methods(journey: &mut PhoneJourney, host: &str) -> Result<Vec<String>, String> {
    let lines = journey.provider_input(host, "agent-methods")?;
    Ok(lines
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|frame| frame["method"].as_str().map(str::to_owned))
        .collect())
}

pub(super) fn conversation(journey: &mut PhoneJourney) -> Proved {
    journey.launch(Launch::plain())?;
    pair_by_code(journey, "desk")?;
    let id = create(journey, "desk", "tester")?;

    send(journey, HELLO_ACP)?;
    journey.wait(
        |drawn| replies(drawn, HELLO_REPLY) == 1 && drawn.has("chat.row.turn-end"),
        "the first reply",
    )?;
    reopen(journey, &id, |drawn| replies(drawn, HELLO_REPLY) == 1)?;
    journey.screen("replied", &["chat.row.turn-end"])?;

    // Antigravity's two choices: allow once or deny.
    send(journey, RUN_SUITE)?;
    let card = permission_card(journey, "cargo test", &["Allow", "Deny…"])?;
    journey.screen("asked-once", &[])?;
    journey.tap(&choice(&card, "Allow")?)?;
    journey.wait(
        |drawn| !drawn.has("ask") && labelled(drawn, SUITE_PASSED),
        "the turn after Allow",
    )?;

    // OpenCode's three: allow once, always allow, or reject.
    send(journey, CLEAN)?;
    permission_card(journey, "rm -rf build", &["Allow", "Always allow", "Deny…"])?;
    journey.screen("asked-three", &[])?;
    // Denied with nothing said: the agent's own refusal.
    journey.tap("ask.deny")?;
    let noting = journey.wait_for(&["ask.note"])?;
    journey.tap(&choice(&noting, "Deny")?)?;
    journey.wait(
        |drawn| !drawn.has("ask") && labelled(drawn, LEFT_ALONE),
        "the turn after Reject",
    )?;

    let settled = journey.wait_chat(
        "desk",
        &id,
        |chat| phase(chat) == "IDLE" && super::holds(chat, LEFT_ALONE),
        "settled",
    )?;
    for prompt in [HELLO_ACP, RUN_SUITE, CLEAN] {
        reflected_once(&settled, prompt)?;
    }
    for reply in [HELLO_REPLY, SUITE_PASSED, LEFT_ALONE] {
        if !super::holds(&settled, reply) {
            return Err(format!("the desk does not hold {reply:?}: {settled}"));
        }
    }
    let control = negative_control(reflected_once(&settled, "A prompt nobody sent."))?;
    let answers = permission_answers(journey, "desk")?;
    if answers != ["allow", "reject"] {
        return Err(format!(
            "the agent was told {answers:?}, not allow then reject"
        ));
    }
    reopen(journey, &id, |drawn| {
        labelled(drawn, LEFT_ALONE) && drawn.has("chat.row.turn-end")
    })?;
    journey.screen("answered", &["chat.row.turn-end"])?;
    Ok(vec![
        format!("New Agent listed the host's added agent as {AGENT_NAME:?}; picked by that name, it made an ACP agent the desk lists, and its chat opened"),
        format!("{} was answered {}", quoted(HELLO_ACP), quoted(HELLO_REPLY)),
        "the first permission card offered the agent's two choices, Allow and Deny; Allow reached the agent as allow and the turn went on".to_owned(),
        "the second offered its three, Allow (once), Always allow and Deny; Deny with nothing said reached the agent as its reject option and the turn went on without the command".to_owned(),
        "the desk holds each prompt once and each reply".to_owned(),
        control,
    ])
}

pub(super) fn interrupt_and_queue(journey: &mut PhoneJourney) -> Proved {
    journey.launch(Launch::plain())?;
    pair_by_code(journey, "cabin")?;
    let id = create(journey, "cabin", "soaker")?;

    // Interrupted: the turn ends, and what it would have said next never
    // comes.
    send(journey, LONG_JOB)?;
    journey.wait(
        |drawn| labelled(drawn, LONG_STARTED) && drawn.has("chat.stop"),
        "the long job under way",
    )?;
    journey.tap("chat.stop")?;
    journey.wait(
        |drawn| drawn.has("chat.row.stopped") && !drawn.has("chat.activity"),
        "the turn stopped",
    )?;
    let stopped = journey.wait_chat("cabin", &id, |chat| phase(chat) == "IDLE", "stopped")?;
    if super::holds(&stopped, NEVER_SAID) {
        return Err("the interrupted turn went on".to_owned());
    }
    if !methods(journey, "cabin")?.contains(&"session/cancel".to_owned()) {
        return Err("the agent was never sent session/cancel".to_owned());
    }
    reopen(journey, &id, |drawn| drawn.has("chat.row.stopped"))?;
    journey.screen("interrupted", &[])?;

    // Queued while it works: Withdraw, and no Send now, since this agent
    // cannot take a prompt into a running turn.
    send(journey, SOAK)?;
    journey.wait(
        |drawn| labelled(drawn, SOAK_RUNNING) && drawn.has("chat.activity"),
        "the soak under way",
    )?;
    send(journey, CHECK_LOGS)?;
    send(journey, WITHDRAWN_ACP)?;
    let both = |drawn: &Drawn| {
        queued(drawn)
            == [
                (CHECK_LOGS.to_owned(), "queued".to_owned()),
                (WITHDRAWN_ACP.to_owned(), "queued".to_owned()),
            ]
    };
    let drawn = journey.wait(both, "two queued")?;
    let offered_now: Vec<&str> = drawn
        .names()
        .filter(|name| name.starts_with("chat.queued.") && name.ends_with(".sendNow"))
        .collect();
    if !offered_now.is_empty() || !drawn.has("chat.queued.1.withdraw") {
        return Err(format!(
            "the queued prompts offer {offered_now:?} to send now, or no Withdraw"
        ));
    }
    reopen(journey, &id, both)?;
    photograph_running(journey, "queued")?;
    journey.tap("chat.queued.1.withdraw")?;
    journey.wait(
        |drawn| {
            drawn.value("chat.field") == WITHDRAWN_ACP
                && queued(drawn) == [(CHECK_LOGS.to_owned(), "queued".to_owned())]
        },
        "the withdrawn prompt back in the composer",
    )?;
    journey.app(json!({"kind": "clear", "identifier": "chat.field"}))?;
    journey.wait(
        |drawn| drawn.value("chat.field").is_empty(),
        "the draft cleared",
    )?;
    journey.request(open_gate("soak-done"), None)?;
    journey.wait(
        |drawn| {
            queued(drawn).is_empty()
                && labelled(drawn, SOAK_DONE)
                && labelled(drawn, NOTED)
                && !drawn.has("chat.activity")
        },
        "both turns' ends",
    )?;
    let settled = journey.wait_chat(
        "cabin",
        &id,
        |chat| phase(chat) == "IDLE" && super::holds(chat, NOTED),
        "settled",
    )?;
    reflected_once(&settled, SOAK)?;
    reflected_once(&settled, CHECK_LOGS)?;
    never_sent(&settled, WITHDRAWN_ACP)?;
    let control = negative_control(never_sent(&settled, CHECK_LOGS))?;
    // amux holds a queued prompt until the turn ends, so the agent read it
    // only after the soak's turn was answered.
    let prompts_sent = methods(journey, "cabin")?
        .iter()
        .filter(|method| *method == "session/prompt")
        .count();
    if prompts_sent != 3 {
        return Err(format!(
            "the agent was sent {prompts_sent} prompts, not the long job, the soak and the queued one"
        ));
    }
    reopen(journey, &id, |drawn| {
        labelled(drawn, NOTED) && drawn.has("chat.row.turn-end")
    })?;
    journey.screen("finished", &["chat.row.turn-end"])?;
    Ok(vec![
        "Interrupt ended the running turn: the agent was sent session/cancel, the chat shows it stopped and the host never holds what came after".to_owned(),
        "two prompts sent while the agent worked queued under the feed with Withdraw and without Send now".to_owned(),
        "withdrawn, the newest came back to the composer as a draft and was cleared; the host never received it".to_owned(),
        format!(
            "{} was sent once the soak's turn ended, and answered {}",
            quoted(CHECK_LOGS),
            quoted(NOTED)
        ),
        control,
    ])
}

/// What the chat says of settling an agent that cannot reopen its
/// conversation, and where its composer stood once it has.
const ENDS_FOR_GOOD: &str =
    "Fake ACP Agent can’t reopen this conversation, so settling ends it for good.";
const ENDED: &str = "Ended · Fake ACP Agent can’t reopen this conversation";

pub(super) fn unresumable(journey: &mut PhoneJourney) -> Proved {
    journey.launch(Launch::plain())?;
    pair_by_code(journey, "shed")?;
    let id = create(journey, "shed", "oneshot")?;
    let name = "oneshot";
    send(journey, ONE_THING)?;
    journey.wait(
        |drawn| replies(drawn, DONE_HERE) == 1 && drawn.has("chat.row.turn-end"),
        "the reply",
    )?;
    // Settling it ends the conversation for good, so the chat asks first,
    // in those words, even with nothing under way to cut short.
    journey.tap("chat.more")?;
    journey.choose("Settle")?;
    let card = journey.wait_for(&["chat.settle.card"])?;
    let asked = card.label("chat.settle.card").to_owned();
    if !asked.contains(ENDS_FOR_GOOD) {
        return Err(format!(
            "the settle card said {asked:?}, not that it ends for good"
        ));
    }
    journey.screen("settle-asked", &[])?;
    journey.tap("chat.settle.confirm")?;
    journey.wait(|drawn| !drawn.has("chat"), "the chat closed")?;
    let settled = journey.wait_inventory(
        "shed",
        |agents| {
            listed(agents, &id)
                .is_some_and(|agent| agent["lifecycle"] == EXITED && agent["filing"] == SETTLED)
        },
        "settled",
    )?;
    lifecycle(&settled, &id, name, EXITED)?;
    let control = negative_control(lifecycle(&settled, &id, name, LIVE))?;
    open_settled(journey, &id)?;
    journey.tap(&format!("home.row.{id}"))?;
    let drawn = journey.wait(
        |drawn| drawn.has("chat.ended") && replies(drawn, DONE_HERE) == 1,
        "the ended chat",
    )?;
    if drawn.label("chat.ended") != ENDED {
        return Err(format!(
            "the ended chat said {:?}",
            drawn.label("chat.ended")
        ));
    }
    if drawn.has("chat.field") || drawn.has("chat.resume") {
        return Err("the ended chat still offers a composer or Resume".to_owned());
    }
    let kept = journey.chat("shed", &id, "history-kept")?;
    reflected_once(&kept, ONE_THING)?;
    journey.screen("ended", &[])?;
    leave_chat_from_settled(journey)?;
    Ok(vec![
        format!("an agent that cannot reopen a conversation answered {}", quoted(ONE_THING)),
        "settled from its menu after the chat asked first, saying it can't reopen the conversation and settling ends it for good, the host lists it exited and settled".to_owned(),
        control,
        format!("its chat on the Settled page keeps the conversation and offers no composer and no Resume, only {}", quoted(ENDED)),
    ])
}

/// Back from a chat opened on the Settled page.
fn leave_chat_from_settled(journey: &mut PhoneJourney) -> Result<(), String> {
    journey.back()?;
    journey.wait(|drawn| !drawn.has("chat"), "the Settled page")?;
    Ok(())
}

pub(super) fn signed_out(journey: &mut PhoneJourney) -> Proved {
    journey.launch(Launch::plain())?;
    pair_by_code(journey, "attic")?;
    let id = create(journey, "attic", "signer")?;
    // Its conversation could not open: the chat says why, in the agent's
    // words for signing in, on the computer it runs on.
    let hint = "fake-acp login";
    journey.wait(
        |drawn| labelled(drawn, "needs you to sign in") && labelled(drawn, hint),
        "the sign-in notice",
    )?;
    reopen(journey, &id, |drawn| labelled(drawn, hint))?;
    journey.screen("signed-out", &[])?;
    let inventory = journey.wait_inventory("attic", |_| true, "signed-out")?;
    let agent = listed(&inventory, &id).cloned().unwrap_or_default();

    // Signed in at the machine, the next message opens it.
    journey.request(open_gate("attic-signed-in"), None)?;
    send(journey, ARE_YOU_THERE)?;
    journey.wait(
        |drawn| replies(drawn, SIGNED_IN_REPLY) == 1 && !labelled(drawn, "needs you to sign in"),
        "the reply once signed in",
    )?;
    let answered = journey.wait_chat(
        "attic",
        &id,
        |chat| phase(chat) == "IDLE" && super::holds(chat, SIGNED_IN_REPLY),
        "answered",
    )?;
    reflected_once(&answered, ARE_YOU_THERE)?;
    let opened = methods(journey, "attic")?
        .iter()
        .filter(|method| *method == "session/new")
        .count();
    if opened < 2 {
        return Err(format!(
            "the agent was asked to open a session {opened} times, not once signed out and again signed in"
        ));
    }
    reopen(journey, &id, |drawn| replies(drawn, SIGNED_IN_REPLY) == 1)?;
    journey.screen("signed-in", &["chat.row.turn-end"])?;
    Ok(vec![
        format!(
            "created signed out, the chat said {AGENT_NAME} needs signing in, with its own words {:?}, and the host kept it {}",
            hint, agent["lifecycle_name"]
        ),
        format!(
            "signed in at the machine, {} from the same chat opened the conversation and was answered once",
            quoted(ARE_YOU_THERE)
        ),
    ])
}
