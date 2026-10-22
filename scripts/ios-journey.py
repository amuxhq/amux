#!/usr/bin/env python3
"""Run shared journey stories on the iPhone app.

`scripts/ios-journey.py [story...]`: each story from journeys/manifest.json
that lists the phone among its clients, against the served topology it names
for the phone (`phone_topology`, else `topology`), on the leased simulator,
driven through the app's door the way a person drives it. Screens are
compared with reviewed goldens under journeys/goldens/phone/<story>/
(UPDATE_JOURNEY_GOLDENS=1 rewrites them) and the hosts are asked what they
recorded. Results land in target/journeys/phone/<story>. With no story named,
every story written here runs; `--native` runs the phone's own stories, the
ones the manifest declares for the phone alone.
"""

from __future__ import annotations

from pathlib import Path
from urllib.parse import urlparse
import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
import ios_simulators  # noqa: E402
from journeys.phone import BUNDLE_ID, MANIFEST, ROOT, DoorError, PhoneJourney, simctl, story  # noqa: E402

SIMULATOR = "golden"


def prompts(chat: dict, text: str) -> list[dict]:
    return [item for item in chat["items"] if item["text"] == text]


def reflected_once(chat: dict, text: str) -> None:
    """The prompt the person sent is in the host's chat exactly once."""
    found = prompts(chat, text)
    if len(found) != 1:
        raise RuntimeError(f"{text!r} is in the host's chat {len(found)} times")


def negative_control(check, *args) -> str:
    """A deliberately wrong expectation must fail the same check."""
    try:
        check(*args)
    except RuntimeError as error:
        return f"a wrong expectation fails: {error}"
    raise RuntimeError(f"the check accepted a wrong outcome: {args[1:]!r}")


def pin_of(line: str) -> str | None:
    found = re.search(r"Pairing PIN: (\d{3}) (\d{3})", line)
    return found.group(1) + found.group(2) if found else None


def printed_pin(process) -> str:
    """The six digits a machine's `amux pair` printed."""
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        line = process.stdout.readline()
        if pin := pin_of(line):
            return pin
        if not line and process.poll() is not None:
            break
    raise RuntimeError("the machine printed no pairing PIN")


def trusted_as(drawn: dict, host_id: str, name: str) -> None:
    """The phone lists the machine it paired with by its own id and name."""
    row = drawn.get(f"hosts.row.{host_id}")
    if row is None or not (row.get("label") or "").startswith(f"{name}, "):
        raise RuntimeError(f"the phone does not list {name} ({host_id}) as a host: {row!r}")


def reach_host(journey: PhoneJourney) -> list[str]:
    desk = journey.host_id("desk")
    prompt = "Hello from the phone."
    reply = "The desk is reachable."
    # Unpaired: the phone's own browser finds the desk and offers to pair.
    journey.launch()
    journey.tab("Hosts")
    journey.wait_for(f"hosts.pair.{desk}", f"hosts.pair.{desk}")
    journey.screen("found-not-paired")

    # Six digits the desk printed, typed on the phone's number pad, reach the
    # desk's own name and key before anything is trusted.
    pairing = journey.amux("desk", "pair")
    pin = printed_pin(pairing)
    journey.tap(f"hosts.pair.{desk}")
    journey.wait_for("pin.code")
    journey.type("pin.code", pin)
    card = journey.wait_for("pair-confirm.trust", "pair-confirm.fingerprint")
    if card["pair-confirm.name"].get("value") != "desk":
        raise RuntimeError(f"the code reached {card['pair-confirm.name']!r}, not the desk")
    journey.screen("pair-confirm", volatile=("pair-confirm.fingerprint", "pair-confirm.expiry"))
    journey.tap("pair-confirm.trust")
    # Trusting closes the sheet, with the desk already in the list.
    drawn = journey.wait(
        lambda drawn: f"hosts.row.{desk}" in drawn and "pair-confirm" not in drawn, "the desk paired"
    )
    trusted_as(drawn, desk, "desk")
    control = negative_control(trusted_as, drawn, desk, "laptop")
    pairing.wait(timeout=30)
    journey.screen("paired")

    # The desk's agent is in the fleet; the phone opens it, writes, and the
    # desk holds the prompt once and answers.
    journey.tab("Agents")
    agent = next(item["id"] for item in journey.ready["agents"] if item["name"] == "desk-work")
    journey.wait_for(f"home.row.{agent}")
    journey.tap(f"home.row.{agent}")
    journey.wait_for("chat.field")
    journey.type("chat.field", prompt)
    journey.wait(lambda drawn: drawn.get("chat.send", {}).get("enabled") is True, "a message ready to send")
    journey.tap("chat.send")
    heard = journey.wait_chat(
        "desk", "desk-work", lambda chat: chat["phase"] == "IDLE" and len(prompts(chat, prompt)) == 1, "desk-heard"
    )
    reflected_once(heard, prompt)
    journey.wait(
        lambda drawn: any(reply in (element.get("label") or "") for element in drawn.values()),
        "the desk's reply on the phone",
    )

    # Leaving the chat comes back to the fleet; opened again, the chat holds
    # the prompt and the reply.
    journey.tap("chat.back")
    journey.wait(lambda drawn: f"home.row.{agent}" in drawn and "chat.field" not in drawn, "the fleet")
    journey.tap(f"home.row.{agent}")
    journey.wait(
        lambda drawn: "chat.row.turn-end" in drawn
        and any(reply in (element.get("label") or "") for element in drawn.values()),
        "the chat opened again",
    )
    journey.screen("usable-chat", volatile=("chat.row.turn-end",))
    journey.tap("chat.back")
    journey.wait(lambda drawn: f"home.row.{agent}" in drawn and "chat.field" not in drawn, "the fleet")
    return [
        "the phone's own browser found the desk in the net's scope and offered to pair",
        "the PIN the desk printed, typed on the keypad, reached the desk's name and key before trust",
        control,
        f"the phone lists desk {desk} as its host after Pair",
        f"the desk's agent opened on the phone; the desk holds {prompt!r} once and answered",
        "leaving the chat came back to the fleet, and the chat opened again held the prompt and the reply",
    ]


PROMPT = "Check the deployment once."
REPLY = "The deploy check passed."


def labelled(drawn: dict, text: str) -> bool:
    return any(text in (element.get("label") or "") for element in drawn.values())


def choice(drawn: dict, label: str) -> str:
    """The ask choice a person reads as `label`."""
    for name, element in drawn.items():
        if name.startswith("ask.choice.") and element.get("label") == label:
            return name
    offered = [e.get("label") for n, e in drawn.items() if n.startswith("ask.choice.")]
    raise RuntimeError(f"no choice reads {label!r}; the card offers {offered!r}")


def pair_by_code(journey: PhoneJourney, host: str) -> None:
    """The phone's browser finds `host`; the PIN it printed is typed on the
    keypad and the machine it names is trusted."""
    host_id = journey.host_id(host)
    journey.tab("Hosts")
    journey.wait_for(f"hosts.pair.{host_id}")
    pairing = journey.amux(host, "pair")
    pin = printed_pin(pairing)
    journey.tap(f"hosts.pair.{host_id}")
    journey.wait_for("pin.code")
    journey.type("pin.code", pin)
    journey.wait_for("pair-confirm.trust")
    journey.tap("pair-confirm.trust")
    journey.wait(lambda drawn: f"hosts.row.{host_id}" in drawn and "pair-confirm" not in drawn, f"{host} paired")
    pairing.wait(timeout=30)


def open_agent(journey: PhoneJourney, name: str) -> str:
    agent = next(item["id"] for item in journey.ready["agents"] if item["name"] == name)
    journey.tab("Agents")
    journey.wait_for(f"home.row.{agent}")
    journey.tap(f"home.row.{agent}")
    journey.wait_for("chat.field")
    return agent


def send(journey: PhoneJourney, text: str) -> None:
    journey.type("chat.field", text)
    journey.wait(lambda drawn: drawn.get("chat.send", {}).get("enabled") is True, "a message ready to send")
    journey.tap("chat.send")


def conversation_decision(journey: PhoneJourney, agent: str, provider_logs: bool) -> list[str]:
    journey.launch()
    pair_by_code(journey, "desk")
    open_agent(journey, agent)
    send(journey, PROMPT)
    card = journey.wait(
        lambda drawn: "ask" in drawn and labelled(drawn, "deploy --check") and labelled(drawn, "Allow"),
        "the permission card",
    )
    sent = journey.wait_chat("desk", agent, lambda chat: len(prompts(chat, PROMPT)) >= 1, "prompt-reflected")
    reflected_once(sent, PROMPT)
    control = negative_control(reflected_once, sent, PROMPT + " (a wrong prompt)")
    journey.screen("permission")
    journey.tap(choice(card, "Allow"))
    journey.wait(lambda drawn: "ask" not in drawn and labelled(drawn, REPLY), "the settled turn")
    settled = journey.wait_chat(
        "desk",
        agent,
        lambda chat: chat["phase"] == "IDLE" and any(REPLY in item["text"] for item in chat["items"]),
        "turn-settled",
    )
    reflected_once(settled, PROMPT)
    assertions = [
        f"{PROMPT!r} reflected once in the desk's chat",
        control,
        "the permission card offered its outcomes and Allow was tapped",
        f"the reply {REPLY!r} arrived and the desk says idle",
    ]
    if provider_logs:
        lines = journey.provider_input(agent, "provider-input")
        answers = [line for line in lines if "allow" in line.lower() or "accept" in line.lower()]
        if len(answers) != 1:
            raise RuntimeError(f"the provider did not receive one allow: {lines!r}")
        assertions.append("the provider received exactly one allow")
    journey.tap("chat.back")
    journey.wait(lambda drawn: "chat.field" not in drawn, "the fleet")
    journey.tap(f"home.row.{next(a['id'] for a in journey.ready['agents'] if a['name'] == agent)}")
    journey.wait(lambda drawn: "chat.row.turn-end" in drawn and labelled(drawn, REPLY), "the chat opened again")
    journey.screen("settled", volatile=("chat.row.turn-end",))
    return assertions


PLAN_PROMPT = "Move the journal."
PLAN_NOTE = "Check the copy before switching."
PLAN_DONE = "Moving the journal as planned."
PLAN_FILE = ("/.claude/plans/", "move-the-journal.md")


def control_responses(journey: PhoneJourney, agent: str, label: str) -> list[dict]:
    """What headless Claude was answered, in order."""
    lines = [json.loads(line) for line in journey.provider_input(agent, label)]
    return [line["response"]["response"] for line in lines if line.get("type") == "control_response"]


def sent_back_then_approved(decisions: list[dict], note: str) -> None:
    """The plan went back once with `note`, then was approved once."""
    behaviors = [decision.get("behavior") for decision in decisions]
    if behaviors != ["deny", "allow"] or not decisions[0].get("message", "").endswith(note):
        raise RuntimeError(f"Claude was answered {decisions!r}")


def plans_only(chat: dict) -> None:
    """The desk holds the two plans as its only tool items: the plan file
    Claude wrote before each is no step of the chat."""
    calls = [item for item in chat["items"] if item["key"].startswith("toolu_")]
    if len(calls) != 2 or not all(item["text"].startswith("# Move the journal") for item in calls):
        raise RuntimeError(f"the desk holds {[item['text'][:40] for item in calls]!r} as tool items")


def no_plan_file(drawn: dict) -> dict:
    """No row on the phone is a step writing the plan file."""
    shown = [
        f"{name}: {element.get('label')}" for name, element in drawn.items()
        if any(part in (element.get("label") or "") for part in PLAN_FILE)
    ]
    if shown:
        raise RuntimeError(f"the plan file shows as a step: {shown!r}")
    return drawn


def plan_card(drawn: dict) -> bool:
    return "ask" in drawn and "ask.approve" in drawn


def send_back(journey: PhoneJourney, card: dict, note: str) -> None:
    """Send back… opens the note the plan goes back with; the send-back
    under it is offered only once something is written."""
    journey.tap(choice(card, "Send back…"))
    journey.wait_for("ask.note")
    journey.type("ask.note", note)
    drawn = journey.wait(
        lambda drawn: any(
            name.startswith("ask.choice.") and element.get("label") == "Send back" and element.get("enabled", True)
            for name, element in drawn.items()
        ),
        "a note ready to send back",
    )
    journey.tap(choice(drawn, "Send back"))


def decide_plan(journey: PhoneJourney, agent: str, headless: bool) -> list[str]:
    journey.launch()
    pair_by_code(journey, "desk")
    agent_id = open_agent(journey, agent)
    send(journey, PLAN_PROMPT)
    card = no_plan_file(journey.wait(
        lambda drawn: plan_card(drawn) and labelled(drawn, "Delete the old journal."), "the plan and its decision"
    ))
    journey.screen("plan")
    send_back(journey, card, PLAN_NOTE)
    card = no_plan_file(journey.wait(
        lambda drawn: plan_card(drawn) and labelled(drawn, "Plan sent back") and labelled(drawn, PLAN_NOTE)
        and labelled(drawn, "Keep the old journal for a week."),
        "the revised plan",
    ))
    journey.screen("revised")
    journey.tap("ask.approve")
    journey.wait(lambda drawn: "ask" not in drawn and labelled(drawn, PLAN_DONE), "the approved plan's work")
    settled = journey.wait_chat(
        "desk",
        agent,
        lambda chat: chat["phase"] == "IDLE" and any(PLAN_DONE in item["text"] for item in chat["items"]),
        "plan-settled",
    )
    reflected_once(settled, PLAN_PROMPT)
    plans_only(settled)
    assertions = [
        f"{PLAN_PROMPT!r} reflected once in the desk's chat",
        "the plan read whole in the chat with its decision where the composer was",
        "sent back with a note, the revised plan arrived under the plan sent back; approved, the work ran",
        "the plan file Claude wrote before each plan showed as no step, on the phone or on the desk",
        negative_control(plans_only, {"items": [*settled["items"], {"key": "toolu_write", "text": ""}]}),
    ]
    if headless:
        decisions = control_responses(journey, agent, "plan-decisions")
        sent_back_then_approved(decisions, PLAN_NOTE)
        assertions.append("Claude received the plan sent back with the note, then one approval")
        assertions.append(negative_control(sent_back_then_approved, decisions, "a note never written"))
    no_plan_file(reopen(journey, agent_id, lambda drawn: labelled(drawn, PLAN_DONE) and "chat.row.turn-end" in drawn))
    journey.screen("approved", volatile=("chat.row.turn-end",))
    return assertions


ROTATE_PROMPT = "Rotate the logs."
ROTATE_NOTE = "Keep a month of logs, compressed."
IMPLEMENT = "Implement the plan."


def codex_turns(journey: PhoneJourney, agent: str, label: str) -> list[tuple[str, str]]:
    """Each turn amux started on Codex: the collaboration mode it ran in
    and its words. A turn naming no mode keeps the one before, as Codex
    does."""
    lines = [json.loads(line) for line in journey.provider_input(agent, label)]
    turns = []
    mode = "default"
    for line in lines:
        if line.get("method") != "turn/start":
            continue
        params = line["params"]
        mode = (params.get("collaborationMode") or {}).get("mode", mode)
        text = "".join(part.get("text", "") for part in params.get("input", []))
        turns.append((mode, text))
    return turns


def codex_plan_turns(turns: list[tuple[str, str]], expected: list[tuple[str, str]]) -> None:
    if turns != expected:
        raise RuntimeError(f"Codex was started on {turns!r}")


def plan_mode(journey: PhoneJourney) -> None:
    """Plan picked as Codex's mode on the settings card the model chip
    opens; the card closed once the agent reports it."""
    journey.tap("chat.model")
    journey.wait_for("chat.settings.workmode.plan")
    # Codex's card holds more than fits above the chat, and the mode is the
    # last thing on it; it is swiped once the card has finished rising.
    journey.app({"kind": "settle"})
    journey.app({"kind": "scroll", "direction": "bottom", "identifier": "chat.settings.workmode.plan"})
    journey.app({"kind": "settle"})
    journey.tap("chat.settings.workmode.plan")
    journey.wait(
        lambda drawn: drawn.get("chat.settings.workmode.plan", {}).get("value") == "current", "plan mode current"
    )
    journey.tap("chat.settings.close")
    journey.wait(lambda drawn: "chat.settings" not in drawn, "the settings card closed")


def decide_plan_codex(journey: PhoneJourney) -> list[str]:
    agent = "planner-codex"
    journey.launch()
    pair_by_code(journey, "desk")
    agent_id = open_agent(journey, agent)
    plan_mode(journey)
    send(journey, PLAN_PROMPT)
    # The plan is in the chat while the turn still runs, with no decision;
    # opened again, it is read with the keyboard down.
    def streaming(drawn: dict) -> bool:
        return labelled(drawn, "Delete the old journal.") and "ask" not in drawn and "chat.activity" in drawn

    journey.wait(streaming, "the plan streaming")
    reopen(journey, agent_id, streaming)
    # The activity bar slides while the turn runs; under reduced motion it
    # stands still, so the screen can be photographed. Its elapsed time is
    # masked.
    journey.app({"kind": "assist", "motion": True, "transparency": True})
    journey.screen("plan-streaming", volatile=("chat.activity",))
    journey.app({"kind": "assist", "motion": False, "transparency": True})
    journey.request({"OpenGate": {"name": "plan-streamed"}})
    journey.wait(lambda drawn: plan_card(drawn) and labelled(drawn, "Delete the old journal."), "the plan's decision")
    journey.wait_chat("desk", agent, lambda chat: chat["phase"] == "NEEDS_YOU", "plan-needs-you")
    journey.screen("plan-ready")
    journey.tap("ask.approve")
    journey.wait(
        lambda drawn: "ask" not in drawn and labelled(drawn, PLAN_DONE) and "chat.row.turn-end" in drawn,
        "the implemented plan's work",
    )
    # A second plan, sent back with a note: Codex plans again on it.
    plan_mode(journey)
    send(journey, ROTATE_PROMPT)
    card = journey.wait(
        lambda drawn: plan_card(drawn) and labelled(drawn, "Delete logs older than a week."), "the second plan"
    )
    send_back(journey, card, ROTATE_NOTE)
    journey.wait(
        lambda drawn: plan_card(drawn) and labelled(drawn, "Delete logs older than a month."), "the revised plan"
    )
    journey.wait_chat(
        "desk", agent, lambda chat: chat["phase"] == "NEEDS_YOU" and len(prompts(chat, ROTATE_NOTE)) == 1, "revised"
    )
    # Opened again, so it is read at rest: the decision stands where the
    # composer was.
    journey.tap("chat.back")
    journey.wait(lambda drawn: "chat" not in drawn and f"home.row.{agent_id}" in drawn, "the fleet")
    journey.tap(f"home.row.{agent_id}")
    journey.wait(lambda drawn: plan_card(drawn) and labelled(drawn, "Delete logs older than a month."), "the chat again")
    journey.screen("sent-back")
    expected = [
        ("plan", PLAN_PROMPT),
        ("default", IMPLEMENT),
        ("plan", ROTATE_PROMPT),
        ("plan", ROTATE_NOTE),
    ]
    turns = codex_turns(journey, agent, "plan-turns")
    codex_plan_turns(turns, expected)
    control = negative_control(codex_plan_turns, turns, [*expected[:3], ("default", ROTATE_NOTE)])
    return [
        "plan picked on the settings card, the plan showed in the chat while Codex's turn still ran",
        "when the turn ended the plan's decision stood where the composer was and the desk said the agent needs you",
        f"Approve started a turn out of plan mode with {IMPLEMENT!r}, and its work ran",
        "a second plan, sent back with a note, came back revised; the note went as the next prompt, still in plan mode",
        "Codex was started on the prompt and the note in plan mode and on the implement turn in default mode",
        control,
    ]


OTHER = "From the help panel"
ANSWERS = {
    "How should the settings screen be laid out?": "Stacked",
    "Where should the screen open from?": OTHER,
}
SKIPPED = "Which section should come first?"
PALETTE = "Pick the palette too."
QUESTION_REPLY = "Let me see both palettes on the desk first."


def answered(decisions: list[dict], answers: dict) -> None:
    """The first ask was answered once with exactly `answers`: the skipped
    question left out."""
    if not decisions or decisions[0].get("updatedInput", {}).get("answers") != answers:
        raise RuntimeError(f"Claude was answered {decisions!r}")


def replied(decisions: list[dict], words: str) -> None:
    """The second ask was refused with the person's own words."""
    if len(decisions) != 2 or decisions[1].get("behavior") != "deny" or decisions[1].get("message") != words:
        raise RuntimeError(f"Claude was answered {decisions!r}")


def asking(question: str):
    return lambda drawn: drawn.get("ask.question", {}).get("label") == question


def reopen_card(journey: PhoneJourney, agent_id: str, ready) -> dict:
    """Leaves the chat for the fleet and opens it again with an ask
    standing where the composer was, so it is read with the keyboard
    down."""
    journey.tap("chat.back")
    back_to_row(journey, agent_id, lambda drawn: "chat" not in drawn)
    journey.tap(f"home.row.{agent_id}")
    return journey.wait(lambda drawn: "ask" in drawn and ready(drawn), "the chat again")


def back_to_row(journey: PhoneJourney, agent_id: str, left) -> None:
    """Waits for the fleet with the agent's row on it, opening the folded
    section holding the row when the fleet keeps it folded away."""
    row = f"home.row.{agent_id}"
    folds = lambda drawn: [name for name in drawn if name.startswith("home.fold.")]
    drawn = journey.wait(lambda drawn: left(drawn) and (row in drawn or folds(drawn)), "the fleet")
    if row not in drawn:
        for fold in folds(drawn):
            journey.tap(fold)
    journey.wait(lambda drawn: left(drawn) and row in drawn, "the fleet")


def answer_questions(journey: PhoneJourney) -> list[str]:
    journey.launch()
    pair_by_code(journey, "desk")
    asker = open_agent(journey, "asker")
    send(journey, "Add a settings screen.")
    first = asking("How should the settings screen be laid out?")
    journey.wait(first, "the first question")
    reopen_card(journey, asker, first)
    journey.screen("first-question")
    # Picked, an option shows its preview above the options.
    journey.tap("ask.option.1")
    journey.wait(
        lambda drawn: drawn.get("ask.option.1", {}).get("value") == "selected" and labelled(drawn, "Palette  terminal"),
        "Stacked's preview",
    )
    journey.screen("stacked-preview")
    journey.tap("ask.next")
    # The second is answered in the person's own words.
    journey.wait(asking("Where should the screen open from?"), "the second question")
    journey.tap("ask.option.other")
    journey.wait_for("ask.other")
    journey.type("ask.other", OTHER)
    journey.wait(lambda drawn: drawn.get("ask.next", {}).get("enabled") is True, "an answer typed")
    journey.tap("ask.next")
    # The third is skipped.
    journey.wait(lambda drawn: asking(SKIPPED)(drawn) and "ask.skip" in drawn, "the third question")
    journey.tap("ask.skip")
    review = journey.wait(
        lambda drawn: "ask.send" in drawn and labelled(drawn, "Stacked") and labelled(drawn, f"“{OTHER}”")
        and labelled(drawn, "Skipped"),
        "the answers for review",
    )
    if review["ask.review.2"].get("value") != "skipped":
        raise RuntimeError(f"the review does not hold the third question skipped: {review['ask.review.2']!r}")
    reopen_card(journey, asker, lambda drawn: "ask.send" in drawn)
    journey.screen("review")
    journey.tap("ask.send")
    done = "Thanks, I'll build it that way."
    journey.wait(lambda drawn: "ask" not in drawn and labelled(drawn, done), "the reply")
    journey.wait_chat("desk", "asker", lambda chat: chat["phase"] == "IDLE", "answered")
    reopen(journey, asker, lambda drawn: labelled(drawn, done) and "chat.row.turn-end" in drawn)
    journey.screen("answered", volatile=("chat.row.turn-end",))

    # A second ask, replied to instead in the person's own words.
    send(journey, PALETTE)
    palette = asking("Which palette should it open in?")
    journey.wait(lambda drawn: palette(drawn) and "ask.reply" in drawn, "the second ask")
    journey.tap("ask.reply")
    journey.wait_for("ask.reply.text")
    journey.type("ask.reply.text", QUESTION_REPLY)
    written = lambda drawn: drawn.get("ask.reply.send", {}).get("enabled") is True  # noqa: E731
    journey.wait(written, "a reply written")
    reopen_card(journey, asker, written)
    journey.screen("reply-instead")
    journey.tap("ask.reply.send")
    later = "I'll leave the palette for later."
    journey.wait(lambda drawn: "ask" not in drawn and labelled(drawn, later), "the reply to the reply")
    journey.wait_chat("desk", "asker", lambda chat: chat["phase"] == "IDLE" and len(prompts(chat, PALETTE)) == 1, "replied")
    reopen(journey, asker, lambda drawn: labelled(drawn, later) and labelled(drawn, QUESTION_REPLY))
    journey.screen("replied", volatile=("chat.row.turn-end",))
    decisions = control_responses(journey, "asker", "answers")
    answered(decisions, ANSWERS)
    replied(decisions, QUESTION_REPLY)
    return [
        "a picked option showed its preview above the options",
        "a picked option, a typed answer under Something else and a skipped question showed together for review",
        "Claude received one answer naming Stacked and the typed text, with the skipped question left out",
        "a second ask, replied to instead, reached Claude as the question refused with the person's words",
        negative_control(answered, decisions, {**ANSWERS, SKIPPED: "Relay"}),
        negative_control(replied, decisions, "Pick Terminal."),
    ]


FORM = {"title": "Reconnect flake in e2e", "team": "FOX", "estimate": 3}


def form_sent(decisions: list[dict], content: dict) -> None:
    if len(decisions) != 1 or decisions[0] != {"action": "accept", "content": content}:
        raise RuntimeError(f"Claude was answered {decisions!r}")


def codex_results(journey: PhoneJourney, agent: str, label: str) -> list[dict]:
    """What Codex was answered, in order."""
    lines = [json.loads(line) for line in journey.provider_input(agent, label)]
    return [line["result"] for line in lines if "result" in line and isinstance(line.get("id"), int)]


def link_done(results: list[dict]) -> None:
    """The server's tool allowed, then its link answered done, with no
    content."""
    if [result.get("action") for result in results] != ["accept", "accept"] or results[1].get("content") is not None:
        raise RuntimeError(f"Codex was answered {results!r}")


def tool_server_asks(journey: PhoneJourney) -> list[str]:
    journey.launch()
    pair_by_code(journey, "desk")
    # A tool server's form, its fields in the order the server wrote.
    filer = open_agent(journey, "filer")
    send(journey, "File the reconnect flake.")
    journey.wait_for("ask.field.title", "ask.field.team", "ask.field.estimate")
    reopen_card(journey, filer, lambda drawn: "ask.field.title" in drawn)
    journey.screen("form")
    journey.type("ask.field.title", FORM["title"])
    # A choice with no default starts unchosen: the person opens it and picks.
    journey.tap("ask.field.team")
    journey.choose(FORM["team"])
    journey.wait(lambda drawn: drawn.get("ask.field.team", {}).get("value") == FORM["team"], "the team picked")
    journey.type("ask.field.estimate", str(FORM["estimate"]))
    filled = lambda drawn: drawn.get("ask.submit", {}).get("enabled") is True and all(
        drawn.get(f"ask.field.{name}", {}).get("value") == str(value) for name, value in FORM.items()
    )
    journey.wait(filled, "the form filled and ready to submit")
    # Leaving the chat mid-form and coming back finds what was typed.
    reopen_card(journey, filer, filled)
    journey.screen("form-kept")
    # What goes is what the form shows.
    journey.tap("ask.submit")
    filed = "Filed it in Linear."
    journey.wait(lambda drawn: "ask" not in drawn and labelled(drawn, filed), "the filed reply")
    journey.wait_chat("desk", "filer", lambda chat: chat["phase"] == "IDLE", "filed")
    reopen(journey, filer, lambda drawn: labelled(drawn, filed) and "chat.row.turn-end" in drawn)
    journey.screen("form-sent", volatile=("chat.row.turn-end",))
    decisions = control_responses(journey, "filer", "form-answer")
    form_sent(decisions, FORM)
    control = negative_control(form_sent, decisions, {**FORM, "team": "CORE"})

    # A link: Codex asks to let the server's tool run, then the server
    # sends a link, worded from its own message.
    journey.tap("chat.back")
    journey.wait(lambda drawn: "chat.field" not in drawn, "the fleet")
    linker = open_agent(journey, "linker")
    send(journey, "Check the relay's error rate.")
    card = journey.wait(lambda drawn: "ask" in drawn and labelled(drawn, "grafana"), "the tool's ask")
    journey.tap(choice(card, "Allow"))
    journey.wait(lambda drawn: "ask.open" in drawn and labelled(drawn, "https://grafana.example.com/login"), "the link")
    reopen_card(journey, linker, lambda drawn: "ask.open" in drawn)
    journey.screen("link")
    journey.tap(choice(journey.elements(), "I’m done"))
    reply = "I'll read the dashboards another way."
    journey.wait(lambda drawn: "ask" not in drawn and labelled(drawn, reply), "the reply")
    journey.wait_chat("desk", "linker", lambda chat: chat["phase"] == "IDLE", "linked")
    results = codex_results(journey, "linker", "link-answers")
    link_done(results)
    reopen(journey, linker, lambda drawn: labelled(drawn, reply) and "chat.row.turn-end" in drawn)
    journey.screen("link-done", volatile=("chat.row.turn-end",))
    return [
        "the form asked for title, team and estimate in the server's order and showed the answers before they went",
        "leaving the chat mid-form and coming back found the values typed",
        f"Claude received the form accepted with {FORM!r}",
        control,
        "Codex's ask to run the server's tool was allowed, then the link showed its message and address",
        "I’m done answered the link: Codex received two accepts, the link's with no content",
        negative_control(link_done, [results[0], {"action": "decline"}]),
    ]


RUN = "Run the soak."
STEER = "Also log the peak."
WITHDRAWN = "And email me."


def never_sent(chat: dict, text: str) -> None:
    if prompts(chat, text):
        raise RuntimeError(f"the host received {text!r}")


def is_held(drawn: dict, text: str) -> bool:
    """`text` is a prompt the client library holds for its host: in the
    queue, pending, with what it waits for."""
    return any(
        name.startswith("chat.underway.")
        and element.get("label") == text
        and (element.get("value") or "").startswith("held · sends when ")
        for name, element in drawn.items()
    )


def current(drawn: dict, agent: str) -> bool:
    """The chat is current: the empty composer offers to message the agent,
    not to hold what is sent."""
    return drawn.get("chat.field", {}).get("label") == f"Message {agent}"


def queued(drawn: dict) -> list[tuple[str, str]]:
    """The prompts waiting under the feed, oldest first: each one's words
    and how it waits."""
    rows = sorted(
        (int(name.rsplit(".", 1)[1]), element)
        for name, element in drawn.items()
        if re.fullmatch(r"chat\.queued\.\d+", name)
    )
    return [(element.get("label") or "", element.get("value") or "") for _, element in rows]


def photograph_running(journey: PhoneJourney, label: str) -> None:
    """The activity bar slides while the turn runs; under reduced motion it
    stands still, so the screen can be photographed. Its elapsed time is
    masked."""
    journey.app({"kind": "assist", "motion": True, "transparency": True})
    journey.screen(label, volatile=("chat.activity",))
    journey.app({"kind": "assist", "motion": False, "transparency": True})


def queue_and_steer(journey: PhoneJourney) -> list[str]:
    journey.launch()
    pair_by_code(journey, "desk")
    worker = open_agent(journey, "worker")
    send(journey, RUN)
    journey.wait(lambda drawn: labelled(drawn, "scripts/soak --for 1h") and "chat.activity" in drawn, "the long run")
    # While it works, prompts queue under the feed.
    send(journey, STEER)
    send(journey, WITHDRAWN)
    both = lambda drawn: queued(drawn) == [(STEER, "queued"), (WITHDRAWN, "queued")]  # noqa: E731
    journey.wait(both, "two queued")
    reopen(journey, worker, both)
    photograph_running(journey, "queued")
    # Withdrawn, the newest is a draft again, and cleared.
    journey.tap("chat.queued.1.withdraw")
    journey.wait(
        lambda drawn: drawn.get("chat.field", {}).get("value") == WITHDRAWN and queued(drawn) == [(STEER, "queued")],
        "the withdrawn prompt back in the composer",
    )
    journey.app({"kind": "clear", "identifier": "chat.field"})
    journey.wait(lambda drawn: not drawn.get("chat.field", {}).get("value"), "the draft cleared")
    # The other goes into the turn now.
    journey.tap("chat.queued.0.sendNow")
    steered = lambda drawn: queued(drawn) == [(STEER, "steered")]  # noqa: E731
    journey.wait(steered, "the prompt steered into the turn")
    reopen(journey, worker, steered)
    photograph_running(journey, "steered")
    journey.request({"OpenGate": {"name": "run-done"}})
    journey.wait(
        lambda drawn: not queued(drawn) and labelled(drawn, "The run finished.") and "chat.activity" not in drawn,
        "the turn's end",
    )
    settled = journey.wait_chat("desk", "worker", lambda chat: chat["phase"] == "IDLE", "settled")
    reflected_once(settled, RUN)
    reflected_once(settled, STEER)
    never_sent(settled, WITHDRAWN)
    control = negative_control(never_sent, settled, STEER)
    reopen(journey, worker, lambda drawn: labelled(drawn, "The run finished.") and "chat.row.turn-end" in drawn)
    journey.screen("finished", volatile=("chat.row.turn-end",))
    return [
        "two prompts sent while the agent worked queued under the feed",
        "withdrawn, the newest came back to the composer as a draft and was cleared; the desk never received it",
        f"sent now, {STEER!r} went into the running turn and the desk holds it once",
        control,
    ]


BACKUP = "Check last night's backup."
BACKUP_DONE = "Last night's backup finished with no errors."


def send_while_away(journey: PhoneJourney) -> list[str]:
    journey.launch()
    pair_by_code(journey, "cabin")
    backups = open_agent(journey, "backups")
    # The cabin goes out of reach: its daemon stops, its agent kept.
    journey.request({"StopDaemon": {"host": "cabin"}})
    journey.wait(lambda drawn: labelled(drawn, "out of reach"), "the cabin out of reach")
    # Send stays open: the client library holds the prompt, pending with
    # Cancel, until the cabin can be reached.
    send(journey, BACKUP)
    journey.wait(lambda drawn: is_held(drawn, BACKUP) and not drawn.get("chat.field", {}).get("value"), "the prompt held")
    reopen(journey, backups, lambda drawn: is_held(drawn, BACKUP))
    journey.screen("away")

    # The cabin back: the held prompt goes by itself.
    journey.request({"RestartDaemon": {"host": "cabin"}})
    journey.wait(
        lambda drawn: labelled(drawn, BACKUP_DONE) and "chat.row.turn-end" in drawn and not is_held(drawn, BACKUP),
        "the reply",
    )
    heard = journey.wait_chat(
        "cabin", "backups", lambda chat: chat["phase"] == "IDLE" and len(prompts(chat, BACKUP)) == 1, "sent"
    )
    reflected_once(heard, BACKUP)
    control = negative_control(never_sent, heard, BACKUP)
    reopen(journey, backups, lambda drawn: labelled(drawn, BACKUP_DONE) and "chat.row.turn-end" in drawn)
    journey.screen("sent", volatile=("chat.row.turn-end",))
    return [
        "with the cabin out of reach, Send held the prompt, pending until the cabin is back, and it stayed held across leaving the chat",
        f"with the cabin back, the held prompt went by itself and the cabin holds {BACKUP!r} once",
        control,
    ]


LIMIT = "5-hour limit reached · resets "
SIGN_IN = "Run claude and sign in with /login on desk."


def limit_named(drawn: dict) -> bool:
    return (drawn.get("chat.strip", {}).get("label") or "").startswith(LIMIT)


def composer_limits(journey: PhoneJourney) -> list[str]:
    journey.launch()
    pair_by_code(journey, "desk")
    # At a usage limit the strip over the composer names the window and
    # when it resets, and sending still works. The reset is a time of day,
    # so the strip is masked and its words checked here.
    limited = open_agent(journey, "limited")
    journey.wait(lambda drawn: limit_named(drawn) and "chat.field" in drawn, "the limit named over the composer")
    reopen(journey, limited, limit_named)
    journey.screen("limit", volatile=("chat.strip",))
    send(journey, "Try again.")
    journey.wait(lambda drawn: labelled(drawn, "Done after the limit.") and "chat.row.turn-end" in drawn, "the reply past the limit")
    past = journey.wait_chat(
        "desk", "limited", lambda chat: chat["phase"] == "IDLE" and len(prompts(chat, "Try again.")) == 1, "limited-sent"
    )
    reflected_once(past, "Try again.")
    control = negative_control(reflected_once, past, "Try again. (a wrong prompt)")

    # A refused credential takes the composer's place: Claude must be
    # signed in again on the host it runs on.
    journey.tap("chat.back")
    journey.wait(lambda drawn: "chat.field" not in drawn, "the fleet")
    expired = open_agent(journey, "expired")
    send(journey, "Check the links.")
    asked = lambda drawn: (  # noqa: E731
        drawn.get("chat.foot.sign-in", {}).get("label") == "Claude needs you to sign in"
        and SIGN_IN in (drawn.get("chat.foot.sign-in", {}).get("value") or "")
        and "chat.field" not in drawn
    )
    journey.wait(asked, "signing in in the composer's place")
    refused = journey.wait_chat("desk", "expired", lambda chat: chat["phase"] == "IDLE", "refused")
    reflected_once(refused, "Check the links.")
    journey.tap("chat.back")
    journey.wait(lambda drawn: "chat" not in drawn and f"home.row.{expired}" in drawn, "the fleet")
    journey.tap(f"home.row.{expired}")
    journey.wait(asked, "the chat again")
    journey.screen("sign-in", volatile=("chat.row.turn-end",))
    return [
        "at its usage limit the strip over the composer named the 5-hour window and when it resets",
        "a prompt still went past the limit and was answered; the desk holds it once",
        control,
        "refused its credential, Claude's chat put signing in in the composer's place, naming desk to sign in on",
        "the desk holds the refused turn's prompt once",
    ]


LIVE, EXITED = 1, 2
HELLO = "Say hello."
BACK = "Welcome back."
READY = "Ready when you are."


def listed(agents: list[dict], agent_id: str) -> dict | None:
    return next((agent for agent in agents if agent["id"] == agent_id), None)


def lifecycle(agents: list[dict], agent_id: str, name: str | None, state: int) -> None:
    """The host lists `agent_id` under `name` in lifecycle `state`."""
    agent = listed(agents, agent_id)
    if agent is None or agent.get("name") != name or agent["lifecycle"] != state:
        raise RuntimeError(f"the desk lists {agent!r}, not {name!r} in lifecycle {state}")


def reopen(journey: PhoneJourney, agent_id: str, ready) -> dict:
    """Leaves the chat for the fleet and opens it again, so it is read with
    the keyboard down."""
    journey.tap("chat.back")
    back_to_row(journey, agent_id, lambda drawn: "chat.field" not in drawn)
    journey.tap(f"home.row.{agent_id}")
    return journey.wait(lambda drawn: "chat.field" in drawn and ready(drawn), "the chat again")


def replies(drawn: dict, text: str) -> int:
    return sum(text in (element.get("label") or "") for name, element in drawn.items() if name.startswith("chat.row"))


def manage_agent(journey: PhoneJourney) -> list[str]:
    desk = journey.host_id("desk")
    journey.launch()
    pair_by_code(journey, "desk")
    before = {agent["id"] for agent in journey.wait_inventory("desk", lambda agents: True, "inventory-before")}

    # Created from the fleet: Claude on the desk, whose chat opens.
    journey.tab("Agents")
    journey.tap("home.newAgent")
    drawn = journey.wait_for("new-agent")
    if drawn.get("new-agent.host", {}).get("value") != "desk":
        journey.tap("new-agent.host")
        journey.choose("desk")
    # New Agent starts on the kind most recently at work, which on this
    # desk may be Codex.
    if drawn.get("new-agent.provider", {}).get("value") != "claude":
        journey.tap("new-agent.provider")
        journey.choose("Claude")
    drawn = journey.wait(lambda drawn: drawn.get("new-agent.directory", {}).get("value"), "a directory chosen")
    # Unnamed, an agent is called after the directory it starts in.
    named = Path(drawn["new-agent.directory"]["value"]).name
    journey.tap("new-agent.start")
    created = journey.wait_inventory("desk", lambda agents: len({a["id"] for a in agents} - before) == 1, "created")
    (agent_id,) = {agent["id"] for agent in created} - before
    lifecycle(created, agent_id, named, LIVE)
    journey.wait_for("chat.field")
    send(journey, HELLO)
    journey.wait(lambda drawn: replies(drawn, READY) == 1 and "chat.row.turn-end" in drawn, "the first reply")
    reopen(journey, agent_id, lambda drawn: replies(drawn, READY) == 1)
    journey.screen("created", volatile=("chat.row.turn-end",))

    # Renamed from the chat's menu: the desk lists the same agent under it.
    journey.tap("chat.more")
    journey.choose("Rename")
    journey.wait_for("chat.rename.field")
    journey.app({"kind": "clear", "identifier": "chat.rename.field"})
    journey.type("chat.rename.field", "helper")
    journey.tap("chat.rename.confirm")
    renamed = journey.wait_inventory(
        "desk", lambda agents: (listed(agents, agent_id) or {}).get("name") == "helper", "renamed"
    )
    lifecycle(renamed, agent_id, "helper", LIVE)
    journey.wait(lambda drawn: drawn.get("chat.title", {}).get("value") == "helper", "the new name on the phone")

    # Stopped: exited on the desk with its history kept, and resumable.
    journey.tap("chat.more")
    journey.choose("Stop Agent")
    journey.wait_for("chat.stop.confirm")
    journey.tap("chat.stop.confirm")
    stopped = journey.wait_inventory(
        "desk", lambda agents: (listed(agents, agent_id) or {}).get("lifecycle") == EXITED, "stopped"
    )
    lifecycle(stopped, agent_id, "helper", EXITED)
    control = negative_control(lifecycle, stopped, agent_id, "helper", LIVE)
    kept = journey.request({"Chat": {"host": "desk", "agent": agent_id}}, "history-kept")
    reflected_once(kept, HELLO)
    if not any(READY in item["text"] for item in kept["items"]):
        raise RuntimeError(f"the stopped agent's history lost its reply: {kept!r}")
    journey.wait(lambda drawn: labelled(drawn, "Exited"), "the chat saying it exited")
    reopen(journey, agent_id, lambda drawn: labelled(drawn, "Exited") and replies(drawn, READY) == 1)
    journey.screen("exited-and-resumable")

    # Resumed from the exited composer: the same identity, live again.
    journey.type("chat.field", BACK)
    journey.wait(lambda drawn: drawn.get("chat.resume", {}).get("enabled") is True, "a message ready to resume with")
    journey.tap("chat.resume")
    resumed = journey.wait_inventory(
        "desk", lambda agents: (listed(agents, agent_id) or {}).get("lifecycle") == LIVE, "resumed"
    )
    lifecycle(resumed, agent_id, "helper", LIVE)
    chat = journey.wait_chat("desk", agent_id, lambda chat: len(prompts(chat, BACK)) == 1 and chat["phase"] == "IDLE", "resumed-chat")
    reflected_once(chat, HELLO)
    reflected_once(chat, BACK)
    journey.wait(lambda drawn: replies(drawn, READY) == 2, "the resumed reply")
    reopen(journey, agent_id, lambda drawn: replies(drawn, READY) == 2 and "chat.row.turn-end" in drawn)
    journey.screen("resumed", volatile=("chat.row.turn-end",))

    # Deleted, explicitly: only an exited agent offers it, so stopped
    # again first, then gone from the fleet and from the desk.
    journey.tap("chat.more")
    journey.choose("Stop Agent")
    journey.wait_for("chat.stop.confirm")
    journey.tap("chat.stop.confirm")
    journey.wait_inventory(
        "desk", lambda agents: (listed(agents, agent_id) or {}).get("lifecycle") == EXITED, "stopped-again"
    )
    journey.wait(lambda drawn: labelled(drawn, "Exited"), "the chat saying it exited again")
    journey.tap("chat.more")
    journey.choose("Delete Agent")
    journey.wait_for("chat.delete.confirm")
    journey.tap("chat.delete.confirm")
    journey.wait(lambda drawn: "chat.field" not in drawn and f"home.row.{agent_id}" not in drawn, "the fleet without it")
    journey.wait_inventory("desk", lambda agents: listed(agents, agent_id) is None, "deleted")
    journey.screen("deleted")
    return [
        "New Agent on the desk created a Claude agent the desk lists under its directory's name, and its chat answered",
        "Rename from the chat's menu: the desk lists the same id as helper",
        "Stop Agent: the desk lists it exited, with its prompt and reply kept",
        control,
        "a message from the exited composer resumed the same id, and each prompt is in the desk's chat once",
        "stopped again, Delete Agent: gone from the fleet and from the desk",
    ]


PASTED = "\n".join(f"deploy log {n}: rsync finished with status 0" for n in range(12))
COMMENT = "Why --delete here?"


def reviewed(chat: dict, comment: str) -> dict:
    """The one prompt carrying the paste and the review, as the desk holds
    it. The phone's composer keeps a paste and a review as chips after the
    text, so they follow it in the order they were added."""
    sent = [item for item in chat["items"] if item["attachments"]]
    if len(sent) != 1:
        raise RuntimeError(f"the desk holds {len(sent)} prompts with attachments")
    item = sent[0]
    kinds = [attachment["kind"] for attachment in item["attachments"]]
    words = item["text"].replace("\ufffc", " ").split()
    if words != ["Please", "check", "against"] or kinds != ["text", "review"]:
        raise RuntimeError(f"the prompt is not the text, then the paste, then the review: {item!r}")
    if item["attachments"][0]["text"] != PASTED:
        raise RuntimeError("the pasted text arrived changed")
    comments = item["attachments"][1]["comments"]
    if comments != [{"path": "deploy.sh", "line": 3, "old_line": 0, "text": comment}]:
        raise RuntimeError(f"the review's comments are {comments!r}")
    return item


def chips(drawn: dict) -> list[str]:
    return [
        element.get("label") or ""
        for name, element in drawn.items()
        if name.startswith("chat.tray.") or "Pasted text" in (element.get("label") or "")
    ]


def on_branch(drawn: dict, branch: str) -> None:
    """The chat's overview names the branch the agent's folder is on."""
    said = drawn.get("chat.overview.branch", {}).get("value")
    if said != branch:
        raise RuntimeError(f"the overview does not say it is on {branch!r}: {said!r}")


def attachment_or_review(journey: PhoneJourney) -> list[str]:
    # The reviewer's working tree on the desk: one commit and an edit.
    work = Path(journey.ready["root"]) / "desk" / "work"
    pinned = {"GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z", "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z"}

    def git(*args: str) -> None:
        subprocess.run(
            ["git", "-C", str(work), "-c", "user.name=Journey", "-c", "user.email=journey@example.invalid", *args],
            check=True, capture_output=True, env={**os.environ, **pinned},
        )

    git("init", "-q", "-b", "main")
    (work / "deploy.sh").write_text("#!/bin/sh\necho deploying\nrsync build/ prod:/srv\n")
    git("add", "deploy.sh")
    git("commit", "-qm", "Deploy by rsync")
    (work / "deploy.sh").write_text("#!/bin/sh\necho deploying\nrsync --delete build/ prod:/srv\necho done\n")
    # An agent reads its folder's git facts when it starts and when a turn
    # ends, and the reviewer started before its folder was a repository: start
    # it again so the phone has changes to offer for review.
    journey.request({"Stop": {"agent": "reviewer"}})
    journey.request({"Resume": {"agent": "reviewer"}})

    journey.launch()
    pair_by_code(journey, "desk")
    reviewer = open_agent(journey, "reviewer")

    # Text, a long paste that becomes one chip, more text.
    journey.type("chat.field", "Please check")
    journey.paste("chat.field", PASTED)
    journey.wait(lambda drawn: labelled(drawn, "Pasted text"), "the paste as one chip")
    # The field keeps the space it put between the words and the paste.
    journey.type("chat.field", "against")

    # A review of the desk's working tree, with a comment on one line.
    journey.wait_for("chat.changes")
    journey.tap("chat.changes")
    drawn = journey.wait(lambda drawn: any(name.startswith("review.line.") for name in drawn), "the review's lines")
    line = next(
        name for name, element in drawn.items()
        if name.startswith("review.line.") and "rsync --delete" in (element.get("label") or "")
    )
    journey.perform(line, "Select line")
    journey.wait_for("review.comment")
    journey.screen("review-diff")
    journey.tap("review.comment")
    journey.wait_for("review.note")
    journey.type("review.note", COMMENT)
    journey.wait(lambda drawn: drawn.get("review.add", {}).get("enabled") is True, "a comment ready to add")
    journey.tap("review.add")
    journey.wait(lambda drawn: labelled(drawn, COMMENT) and "review.attach" in drawn, "the comment on the line")
    journey.screen("review-comment")
    journey.tap("review.attach")
    journey.wait(lambda drawn: "chat.field" in drawn and "review.attach" not in drawn, "the chat again")

    # Read with the keyboard down: the draft keeps its text and both chips.
    drawn = reopen(journey, reviewer, lambda drawn: labelled(drawn, "Pasted text"))
    journey.screen("composer-tokens")
    journey.wait(lambda drawn: drawn.get("chat.send", {}).get("enabled") is True, "the draft ready to send")
    journey.tap("chat.send")
    journey.wait(lambda drawn: labelled(drawn, "I read the review."), "the reviewer's reply")

    # The desk received the text, the paste's exact text, the comment, and
    # the patch it made itself, whose bytes are on its disk.
    received = journey.wait_chat(
        "desk", "reviewer",
        lambda chat: chat["phase"] == "IDLE" and any(i["attachments"] for i in chat["items"]), "received",
    )
    item = reviewed(received, COMMENT)
    control = negative_control(reviewed, received, COMMENT + " (a wrong comment)")
    patch = item["attachments"][1]["patch"]
    found = [path for path in (Path(journey.ready["root"]) / "desk" / "data").rglob(patch) if path.parent.name == "blobs"]
    if len(found) != 1:
        raise RuntimeError(f"the desk holds the patch {patch} {len(found)} times")
    held = found[0].read_bytes()
    if hashlib.sha256(held).hexdigest() != patch or b"+rsync --delete build/ prod:/srv" not in held:
        raise RuntimeError("the patch on the desk is not the reviewed diff")

    # Opened again, the chat shows what was sent and the reply.
    reopen(journey, reviewer, lambda drawn: labelled(drawn, "I read the review.") and "chat.row.turn-end" in drawn)
    journey.screen("reopened", volatile=("chat.row.turn-end",))

    # The chat's overview names the branch its folder is on.
    journey.tap("chat.more")
    journey.choose("Overview")
    drawn = journey.wait(lambda drawn: "chat.overview.branch" in drawn, "the overview's branch")
    on_branch(drawn, "main")
    branch_control = negative_control(on_branch, drawn, "develop")
    journey.screen("overview-branch")
    return [
        "the prompt reached the desk as the text, then the paste, then the review, as the composer showed them",
        "the paste arrived as one attachment holding exactly the twelve lines pasted",
        f"the review carries one comment on deploy.sh line 3: {COMMENT!r}",
        control,
        "the review names a patch the desk computed; its bytes on the desk hash to that name and hold the edit",
        "the chat opened again shows what was sent and the reply",
        "the chat's overview names the branch its folder is on, main",
        branch_control,
    ]


FIRST = "The recovery state is safely stored."
SECOND = "A second turn arrived while you were away."
THIRD = "Back after the restart."
DRAFT = "Pick up where we left off."


def leave_and_recover(journey: PhoneJourney) -> list[str]:
    # A real first run makes the phone's replica of the desk's agent.
    journey.launch()
    pair_by_code(journey, "desk")
    keeper = open_agent(journey, "keeper")
    journey.wait(lambda drawn: labelled(drawn, FIRST) and "chat.row.turn-end" in drawn, "the first turn")
    journey.quit()

    # The desk moves on while the phone is closed, then goes out of reach:
    # its daemon stops, and its agent keeps its journal.
    journey.request({"Send": {"agent": "keeper", "text": "Carry on."}})
    journey.wait_chat(
        "desk", "keeper", lambda chat: chat["phase"] == "IDLE" and any(SECOND in i["text"] for i in chat["items"]),
        "desk-moved-on",
    )
    journey.request({"StopDaemon": {"host": "desk"}})
    away = time.monotonic()

    # Opened again, the app paints the chat from its own store: the cached
    # tail, the desk out of reach, and a composer that keeps a draft.
    journey.relaunch()
    journey.tab("Agents")
    journey.wait_for(f"home.row.{keeper}")
    journey.tap(f"home.row.{keeper}")
    drawn = journey.wait(
        lambda drawn: labelled(drawn, FIRST) and labelled(drawn, "out of reach"), "the cached chat, out of reach"
    )
    if labelled(drawn, SECOND):
        raise RuntimeError("the cached chat shows a turn the phone never received")
    journey.type("chat.field", DRAFT)
    drawn = journey.wait(lambda drawn: drawn.get("chat.field", {}).get("value") == DRAFT, "the draft written")
    if current(drawn, "keeper"):
        raise RuntimeError("the chat reads current with the desk out of reach")
    detached = reopen(journey, keeper, lambda drawn: drawn.get("chat.field", {}).get("value") == DRAFT)
    if not labelled(detached, "out of reach") or labelled(detached, SECOND):
        raise RuntimeError("the chat opened again is not the cached tail out of reach")
    journey.screen("cached-offline", volatile=("chat.row.turn-end",))
    journey.observations["seconds-out-of-reach"] = round(time.monotonic() - away, 1)

    # The desk comes back: the missed turn appends once, and the chat says
    # it is current only once it has.
    journey.request({"RestartDaemon": {"host": "desk"}})
    deadline = time.monotonic() + 60
    while True:
        drawn = journey.elements()
        if current(drawn, "keeper") and not labelled(drawn, SECOND):
            raise RuntimeError("the chat read current before the missed turn arrived")
        if current(drawn, "keeper"):
            break
        if time.monotonic() > deadline:
            raise RuntimeError("the chat did not come back after the desk did")
        time.sleep(0.1)
    journey.actions.append("reached the missed turn, then a current chat")
    if replies(drawn, SECOND) != 1 or drawn.get("chat.field", {}).get("value") != DRAFT:
        raise RuntimeError("the missed turn did not append once with the draft kept")
    journey.tap("chat.send")
    journey.wait(lambda drawn: labelled(drawn, THIRD), "the reply to the kept draft")
    after = journey.wait_chat(
        "desk", "keeper", lambda chat: chat["phase"] == "IDLE" and any(THIRD in i["text"] for i in chat["items"]),
        "sent-after-return",
    )
    reflected_once(after, DRAFT)
    control = negative_control(reflected_once, after, DRAFT + " (a wrong draft)")
    reopen(journey, keeper, lambda drawn: labelled(drawn, THIRD))
    journey.screen("reconciled", volatile=("chat.row.turn-end",))

    # The desk loses power back to a checkpoint: the phone's replica is
    # Reset, and its rows stay on screen until the rebuilt chat swaps in.
    journey.request({"Checkpoint": {"host": "desk"}})
    journey.request({"Rewind": {"host": "desk", "cuts": []}})
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        drawn = journey.elements()
        if not (labelled(drawn, FIRST) and labelled(drawn, THIRD)):
            raise RuntimeError("rows left the screen during the Reset")
        time.sleep(0.05)
    journey.actions.append("the rows stayed on screen through the Reset")
    # A restored drive has no running processes: the agent reads exited.
    reopen(journey, keeper, lambda drawn: labelled(drawn, THIRD) and labelled(drawn, "Exited"))
    journey.screen("after-reset", volatile=("chat.row.turn-end",))
    return [
        "opened again with the desk out of reach, the app painted the cached chat from its own store, "
        "without the turn it never received, and did not read current",
        "the draft typed while out of reach was kept",
        "when the desk came back the missed turn appeared once, and only then did the chat read current",
        f"the kept draft reached the desk once and was answered",
        control,
        "rows stayed on screen through the desk's rewind and Reset, and the chat reads the agent exited",
    ]


def never_received(chat: dict, text: str) -> None:
    if prompts(chat, text):
        raise RuntimeError(f"the desk received {text!r} while access was lost")


def switch_to(journey: PhoneJourney, account: str) -> None:
    """The account a person picks from the account disc's menu."""
    journey.tab("Agents")
    journey.tap("home.accounts")
    journey.choose(account)
    journey.wait(
        lambda drawn: (drawn.get("home.subtitle", {}).get("value") or "").split(" · ")[0] == account,
        f"{account} on screen",
    )


def pair_through_the_relay(journey: PhoneJourney, host: str) -> None:
    """Pairs by the link `host` printed. The phone's relay link comes up a
    moment after signing in, and until it has, the link reaches nobody; the
    person scans the same code again."""
    pairing, link = journey.pairing_link(host)
    deadline = time.monotonic() + 60
    while True:
        try:
            journey.pair(link)
            break
        except DoorError:
            if time.monotonic() > deadline:
                pairing.kill()
                raise
            time.sleep(2)
    pairing.wait(timeout=30)


def keep_authority(journey: PhoneJourney) -> list[str]:
    asked = "Are you there?"
    held = "Still there?"
    desk = journey.host_id("desk")
    guarded = next(item["id"] for item in journey.ready["agents"] if item["name"] == "guarded")
    relay = journey.ready["cloud_url"]
    journey.launch()

    # Signed in to the desk's account, the phone pairs with the desk by the
    # link it printed; there is no local network between them, only the
    # relay.
    journey.app({"kind": "connect", "relay": relay, "token": "refresh-ada", "user": "ada"})
    pair_through_the_relay(journey, "desk")
    journey.tab("Agents")
    journey.wait_for(f"home.row.{guarded}")

    # A second account is a profile of its own, on its own empty fleet.
    journey.app({"kind": "addAccount", "user": "bob", "token": "refresh-bob"})
    journey.wait(lambda drawn: f"home.row.{guarded}" not in drawn and "home" in drawn, "bob's empty fleet")
    journey.tab("Hosts")
    drawn = journey.wait(lambda drawn: "hosts" in drawn, "bob's hosts")
    if f"hosts.row.{desk}" in drawn:
        raise RuntimeError("bob's profile knows the desk")
    journey.screen("other-account")
    switch_to(journey, "ada")
    journey.wait_for(f"home.row.{guarded}")

    # Through the relay the desk's agent answers.
    journey.tap(f"home.row.{guarded}")
    journey.wait_for("chat.field")
    send(journey, asked)
    journey.wait(lambda drawn: labelled(drawn, "Only this account reaches me."), "the first reply")
    reflected_once(journey.wait_chat("desk", "guarded", lambda chat: chat["phase"] == "IDLE", "reached"), asked)
    journey.tap("chat.back")
    journey.wait(lambda drawn: "chat.field" not in drawn, "the fleet")

    # Signed out, the phone has no way to the desk: the chat says so and a
    # message written now never reaches it.
    journey.settings()
    journey.wait_for("you.signOut")
    journey.tap("you.signOut")
    journey.wait(
        lambda drawn: "Signed out" in (drawn.get("account.ada@example.com", {}).get("label") or ""), "signed out"
    )
    journey.tap("you.close")
    journey.wait(lambda drawn: "you" not in drawn, "Settings put away")
    journey.tab("Hosts")
    journey.wait(lambda drawn: labelled(drawn, "signed out"), "the hosts tab naming the sign-out")
    journey.screen("blocked-hosts")
    journey.tab("Agents")
    journey.tap(f"home.row.{guarded}")
    journey.wait(lambda drawn: "chat.field" in drawn and labelled(drawn, "this phone is signed out"), "the chat naming the sign-out")
    # Send holds the message, pending until the phone can reach the desk.
    send(journey, held)
    journey.wait(lambda drawn: is_held(drawn, held), "the message held")
    time.sleep(2)
    # It stays held across leaving the chat; read with the keyboard down.
    journey.tap("chat.back")
    journey.wait(lambda drawn: "chat.field" not in drawn, "the fleet")
    journey.tap(f"home.row.{guarded}")
    journey.wait(lambda drawn: is_held(drawn, held), "the message still held")
    journey.screen("blocked")
    blocked = journey.request({"Chat": {"host": "desk", "agent": "guarded"}}, "while-signed-out")
    never_received(blocked, held)
    control = negative_control(never_received, blocked, asked)

    # Signed in again: the chat is current and the held message goes by
    # itself, once.
    journey.app({"kind": "connect", "relay": relay, "token": "refresh-ada", "user": "ada"})
    journey.wait(
        lambda drawn: not labelled(drawn, "this phone is signed out")
        and labelled(drawn, "The relay carries us again.")
        and not is_held(drawn, held),
        "the reply after signing in",
    )
    after = journey.wait_chat(
        "desk", "guarded", lambda chat: chat["phase"] == "IDLE" and len(prompts(chat, held)) == 1, "sent-after-sign-in"
    )
    reflected_once(after, asked)
    reflected_once(after, held)
    journey.tap("chat.back")
    journey.wait(lambda drawn: "chat.field" not in drawn, "the fleet")
    journey.tap(f"home.row.{guarded}")
    journey.wait(lambda drawn: labelled(drawn, "The relay carries us again.") and "chat.row.turn-end" in drawn, "the chat again")
    journey.screen("recovered", volatile=("chat.row.turn-end",))
    return [
        "a second account opened on its own profile, with an empty fleet that knows no desk",
        f"through the relay the desk received {asked!r} once and answered",
        "signed out, the phone named its own sign-out in the hosts tab and the chat, and held the message, across leaving the chat; the desk never received it",
        control,
        f"signed in again, the held {held!r} went by itself, reached the desk once and was answered",
    ]


# --- The phone's own stories -------------------------------------------------
#
# What only a phone does: signing in through the account service, buying and
# restoring at the App Store, reporting a problem, being read at an
# accessibility text size, the local-network permission and a pairing link
# the system hands over, and a push that wakes the app while it is put away.

RELAYED = "Are you there?"
THROUGH_RELAY = "Reached through the relay."


def relay_cloud(journey: PhoneJourney, **fields: object) -> dict:
    """What the scripted account service answers: this account, whose relay
    credential is the served relay's own login for it, at the served relay."""
    relay = urlparse(journey.ready["cloud_url"])
    account = fields.pop("account", "ada")
    script = {
        "account": account,
        "email": f"{account}@example.com",
        "token": f"refresh-{account}",
        "relayHost": relay.hostname,
        "relayPort": relay.port,
    }
    journey.app({"kind": "cloud", "cloud": script | fields})
    journey.actions.append(f"the account service answers {script | fields}")
    return script | fields


def open_link(journey: PhoneJourney, link: str, cloud: dict | None = None) -> None:
    """The app started by a link, as a code scanned with the camera starts
    it. (`simctl openurl` would put the system's own Open in "Amux"? sheet
    in front, which is outside the app and no door can press.) A scripted
    account service is said again, for the launch to begin from it."""
    extra = ["-amux-link", link]
    if cloud is not None:
        extra += ["-amux-cloud-script", json.dumps(cloud)]
    journey.relaunch(*extra)
    journey.actions.append("the app opened by the link")


def pair_by_link(journey: PhoneJourney, link: str) -> None:
    """The link put to the machine again, until the relay link that has just
    re-read the account carries it."""
    deadline = time.monotonic() + 60
    while True:
        try:
            journey.pair(link)
            return
        except DoorError:
            if time.monotonic() > deadline:
                raise
            time.sleep(2)


def calls(journey: PhoneJourney, label: str) -> dict:
    reply = journey.app({"kind": "calls"})
    journey.observations[label] = {"cloud": reply["cloud"], "store": reply["store"]}
    return reply


def sign_in(journey: PhoneJourney, photograph: bool = False) -> None:
    """Sign In in Settings, handed off to the account service's page and
    back, and Settings put away."""
    journey.settings()
    journey.wait_for("you.signIn")
    if photograph:
        journey.screen("signed-out")
    journey.tap("you.signIn")
    journey.wait(lambda drawn: drawn.get("sign-in.continue", {}).get("enabled") is True, "the hand-off offered")
    if photograph:
        journey.screen("hand-off")
    journey.tap("sign-in.continue")
    journey.wait_for("sign-in.signed-in")
    if photograph:
        journey.screen("signed-in")
    journey.tap("sign-in.continue")
    journey.wait(
        lambda drawn: drawn.get("you", {}).get("value") == "ada@example.com" and "sign-in" not in drawn,
        "ada in Settings",
    )
    journey.tap("you.close")
    journey.wait(lambda drawn: "you" not in drawn, "Settings put away")


def round_trip(journey: PhoneJourney, agent: str, prompt: str, reply: str, host: str = "desk") -> str:
    """Opens `agent`, sends `prompt` and waits for `reply` on the phone and
    the host; the chat is opened again for its photograph."""
    agent_id = open_agent(journey, agent)
    send(journey, prompt)
    journey.wait(lambda drawn: labelled(drawn, reply) and "chat.row.turn-end" in drawn, f"{reply!r} on the phone")
    heard = journey.wait_chat(
        host, agent, lambda chat: chat["phase"] == "IDLE" and len(prompts(chat, prompt)) == 1, f"{agent}-heard"
    )
    reflected_once(heard, prompt)
    reopen(journey, agent_id, lambda drawn: labelled(drawn, reply) and "chat.row.turn-end" in drawn)
    return agent_id


def account_sign_in(journey: PhoneJourney) -> list[str]:
    journey.launch()
    relay_cloud(journey)
    sign_in(journey, photograph=True)
    asked = calls(journey, "account-service-calls")["cloud"]
    for expected in ("signIn select", "handOver ada"):
        if expected not in asked:
            raise RuntimeError(f"the account service was never asked {expected!r}: {asked!r}")

    # Signed in, the phone reaches the desk only through the relay the
    # account service named: the desk has no network in common with it.
    pair_through_the_relay(journey, "desk")
    journey.tab("Hosts")
    desk = journey.host_id("desk")
    trusted_as(journey.wait_for(f"hosts.row.{desk}"), desk, "desk")
    journey.screen("hosts")
    round_trip(journey, "desk-work", RELAYED, THROUGH_RELAY)
    journey.screen("reached", volatile=("chat.row.turn-end",))
    journey.tap("chat.back")
    journey.wait(lambda drawn: "chat.field" not in drawn, "the fleet")
    return [
        "signed out, the You tab offered Sign In; the hand-off named the account service and came back signed in as ada",
        f"the account service was asked to sign in and to hand over ada's session: {asked!r}",
        "the relay the hand-over named carried the pairing to the desk, which the phone lists by its own name",
        f"through that relay the desk received {RELAYED!r} once and answered",
    ]


def purchase_restore(journey: PhoneJourney) -> list[str]:
    desk = journey.host_id("desk")
    journey.launch()
    unpaid = relay_cloud(journey, entitlement="none")
    sign_in(journey)

    # Nothing bought: the relay carries nothing to the desk, so the link it
    # printed, scanned with the camera, is answered with the offer of a
    # subscription rather than called a bad invitation.
    pairing, link = journey.pairing_link("desk")
    open_link(journey, link, unpaid)
    journey.wait_for("pair-confirm.subscribe")
    journey.screen("needs-subscription")

    # Bought at the App Store while amux.sh cannot be reached: the purchase
    # is kept and the paywall says it is not confirmed.
    journey.app({"kind": "store", "store": {"purchase": "bought", "restore": "nothingToRestore"}})
    relay_cloud(journey, entitlement="none", recordPurchase="network")
    journey.tap("pair-confirm.subscribe.buy")
    journey.wait(
        lambda drawn: "paywall.restore" in drawn and drawn.get("paywall.buy", {}).get("enabled") is True,
        "the paywall with a plan to buy",
    )
    journey.screen("paywall")
    journey.tap("paywall.buy")
    journey.wait_for("paywall.unconfirmed")
    journey.screen("unconfirmed")

    # amux.sh answers again and takes it; Restore Purchases finds the
    # subscription the App Store holds for this Apple Account and records it.
    # The relay learns what the account bought from amux.sh.
    relay_cloud(journey, entitlement="none")
    journey.request({"SetTier": {"account": "ada", "tier": "pro"}})
    journey.app({"kind": "store", "store": {"purchase": "bought", "restore": "bought"}})
    journey.tap("paywall.restore")
    journey.wait_for("paywall.subscribed")
    journey.screen("restored")
    record = calls(journey, "purchase-calls")
    recorded = [call for call in record["cloud"] if call.startswith("recordPurchase")]
    if len(recorded) != 2 or "restore" not in record["store"]:
        raise RuntimeError(f"the purchase was not offered to amux.sh again by a restore: {record!r}")
    journey.tap("paywall.back")
    journey.wait(lambda drawn: "paywall" not in drawn, "the paywall closed")

    # The relay link reads what the account buys again, and the same
    # invitation, still offered by the desk, pairs it through the relay.
    pair_by_link(journey, link)
    pairing.wait(timeout=30)
    journey.wait(lambda drawn: "pair-confirm" not in drawn, "the pairing sheet closed")
    journey.screen("paired")
    journey.tab("Hosts")
    trusted_as(journey.wait_for(f"hosts.row.{desk}"), desk, "desk")
    round_trip(journey, "desk-work", RELAYED, THROUGH_RELAY)
    journey.screen("reached", volatile=("chat.row.turn-end",))
    journey.tap("chat.back")
    journey.wait(lambda drawn: "chat.field" not in drawn, "the fleet")
    return [
        "signed in with nothing bought, the link the desk printed was answered with the offer of a subscription",
        "bought while amux.sh could not be reached, the paywall kept the purchase and said it was not confirmed",
        f"Restore Purchases offered it to amux.sh again, which took it: store {record['store']!r}, account service {record['cloud']!r}",
        f"subscribed, the same link paired the desk through the relay and the desk received {RELAYED!r} once and answered",
    ]


REPORT_NOTE = "The desk's agent is listed twice."
REFUSED_UPLOAD = "amux.sh could not take this report"


def app_data(journey: PhoneJourney) -> Path:
    """The app's own data container, which a Mac reads directly."""
    return Path(simctl("get_app_container", journey.udid, BUNDLE_ID, "data").strip())


def report(journey: PhoneJourney) -> list[str]:
    journey.launch()
    journey.app({"kind": "connect", "relay": journey.ready["cloud_url"], "token": "refresh-ada", "user": "ada"})
    pair_through_the_relay(journey, "desk")
    agent = next(item["id"] for item in journey.ready["agents"] if item["name"] == "desk-work")
    journey.tab("Agents")
    journey.wait_for(f"home.row.{agent}")
    journey.app({"kind": "cloud", "cloud": {"upload": "refused", "uploadReason": REFUSED_UPLOAD}})

    # The system photographs the app, and the app offers to report what was
    # on screen, over the frame it froze.
    journey.system_screenshot()
    journey.wait_for("report.prompt")
    journey.screen("offer")
    journey.tap("report.prompt")
    journey.wait_for("report.screen", "report.note")
    journey.type("report.note", REPORT_NOTE)
    journey.wait(lambda drawn: drawn.get("report.note", {}).get("value") == REPORT_NOTE, "the note written")

    # amux.sh turns it down in its own words; nothing written is lost. Sent
    # once the keyboard has finished rising, so the page the refusal lands on
    # rests at one place.
    journey.app({"kind": "settle"})
    journey.tap("report.send")
    refusal = journey.wait_for("report.refusal")["report.refusal"]
    if refusal.get("value") != REFUSED_UPLOAD:
        raise RuntimeError(f"the refusal says {refusal!r}")
    if journey.elements().get("report.note", {}).get("value") != REPORT_NOTE:
        raise RuntimeError("the refusal lost the note")
    journey.screen("refused")

    # Retry hands over the same report, and it is taken.
    journey.app({"kind": "cloud", "cloud": {"upload": "accepted", "receipt": "report-7"}})
    journey.tap("report.send")
    sent = journey.wait_for("report.sent")["report.sent"]
    if sent.get("value") != "report-7":
        raise RuntimeError(f"the receipt reads {sent!r}")
    journey.screen("sent")
    uploads = [call for call in calls(journey, "report-calls")["cloud"] if call.startswith("uploadReport")]
    if len(uploads) != 2 or uploads[0] != uploads[1]:
        raise RuntimeError(f"the retry did not hand over the same report: {uploads!r}")

    # What left the phone, read where it crossed into the account service.
    inside = app_data(journey) / "tmp" / "journey-report"
    bundle = journey.app({"kind": "uploaded", "path": str(inside)})
    header = json.loads(bundle["reportJSON"])
    journey.observations["uploaded-report"] = {"parts": bundle["parts"], "report.json": header}
    if REPORT_NOTE not in json.dumps(header):
        raise RuntimeError(f"the report does not carry the note: {header!r}")
    screen = inside / "frame.png"
    if not screen.exists() or screen.stat().st_size == 0:
        raise RuntimeError(f"the report carries no picture of the screen: {bundle['parts']!r}")
    shutil.copyfile(screen, journey.output / "uploaded-frame.png")
    # The dump inside it carries this phone's copy of the agent the desk
    # lists, the one whose row the report froze.
    if f"dump/agents/{agent}/row.pb" not in bundle["parts"]:
        raise RuntimeError(f"the report's dump holds no copy of the desk's agent: {bundle['parts']!r}")
    journey.wait_inventory(
        "desk", lambda agents: any(item["id"] == agent for item in agents), "desk-lists-the-reported-agent"
    )
    journey.tap("report.cancel") if "report.cancel" in journey.elements() else None
    return [
        "a screenshot offered Report over the frozen frame; the report opened on it and took a note",
        f"amux.sh refused the report in its own words ({REFUSED_UPLOAD!r}) and the note was kept",
        f"Retry handed over the same report, {len(bundle['parts'])} parts both times, and came back with receipt report-7",
        "the report that left the phone carries the note, the frozen screen and a dump holding the agent the desk lists",
    ]


ACCESSIBLE_SIZE = "accessibility3"


def accessibility(journey: PhoneJourney) -> list[str]:
    journey.launch()
    journey.app({"kind": "dynamicType", "size": ACCESSIBLE_SIZE})
    size = journey.query().get("typeSize")
    if size != ACCESSIBLE_SIZE:
        raise RuntimeError(f"the app says it draws at {size!r}")
    pair_by_code(journey, "desk")
    agent = open_agent(journey, "decision-sdk")
    journey.screen("chat")
    send(journey, PROMPT)
    card = journey.wait(
        lambda drawn: "ask" in drawn and labelled(drawn, "deploy --check") and labelled(drawn, "Allow"),
        "the permission card",
    )
    journey.screen("permission")
    journey.tap(choice(card, "Allow"))
    journey.wait(lambda drawn: "ask" not in drawn and labelled(drawn, REPLY), "the settled turn")
    settled = journey.wait_chat(
        "desk",
        "decision-sdk",
        lambda chat: chat["phase"] == "IDLE" and any(REPLY in item["text"] for item in chat["items"]),
        "turn-settled",
    )
    reflected_once(settled, PROMPT)
    lines = journey.provider_input("decision-sdk", "provider-input")
    allowed = [line for line in lines if "allow" in line.lower()]
    if len(allowed) != 1:
        raise RuntimeError(f"the provider did not receive one allow: {lines!r}")
    reopen(journey, agent, lambda drawn: labelled(drawn, REPLY) and "chat.row.turn-end" in drawn)
    # The command's row says how long it ran, which at this size is wide
    # enough to move more than the comparison allows; the desk's record of
    # the call is checked above.
    journey.screen("settled", volatile=("chat.row.turn-end", "chat.row.command"))
    return [
        f"the app says it draws at {ACCESSIBLE_SIZE}",
        "at that size the desk was paired on the keypad and the agent's chat opened",
        f"the permission card offered Allow, which was tapped; the desk holds {PROMPT!r} once and answered",
        "the provider received exactly one allow",
    ]


def local_network(journey: PhoneJourney) -> list[str]:
    desk = journey.host_id("desk")
    # Refused, the system's browser finds nothing, which the launch stands in
    # for by letting it report no machine.
    journey.launch(found=[])

    # Refused: the Hosts tab says so and where it is undone.
    journey.app({"kind": "localNetwork", "permission": "denied"})
    journey.actions.append("the system says the local network was refused")
    journey.tab("Hosts")
    drawn = journey.wait_for("hosts.localNetwork.refused", "hosts.localNetwork.settings")
    if f"hosts.pair.{desk}" in drawn:
        raise RuntimeError("the phone offers the desk with the local network refused")
    journey.screen("refused")

    # Turned on in Settings, which ends the app, and opened again: the desk
    # is found and offered.
    journey.relaunch()
    journey.actions.append("the local network granted in Settings and the app opened again")
    journey.tab("Hosts")
    journey.wait(
        lambda drawn: f"hosts.pair.{desk}" in drawn and "hosts.localNetwork.refused" not in drawn,
        "the desk found",
    )
    journey.screen("granted")

    # The link the desk printed, scanned with the camera, starts the app on
    # the desk's name and key before anything is trusted.
    pairing, link = journey.pairing_link("desk")
    open_link(journey, link)
    card = journey.wait_for("pair-confirm.trust", "pair-confirm.fingerprint")
    if card["pair-confirm.name"].get("value") != "desk":
        raise RuntimeError(f"the link reached {card['pair-confirm.name']!r}, not the desk")
    journey.screen("link-confirm", volatile=("pair-confirm.fingerprint", "pair-confirm.expiry"))
    journey.tap("pair-confirm.trust")
    journey.wait(lambda drawn: "pair-confirm" not in drawn, "the pairing sheet closed")
    pairing.wait(timeout=30)
    journey.tab("Hosts")
    trusted_as(journey.wait_for(f"hosts.row.{desk}"), desk, "desk")
    round_trip(journey, "desk-work", "Hello from the phone.", "The desk is reachable.")
    journey.screen("reached", volatile=("chat.row.turn-end",))
    journey.tap("chat.back")
    journey.wait(lambda drawn: "chat.field" not in drawn, "the fleet")
    return [
        "with the local network refused the Hosts tab said so, offered Settings and listed no machine",
        "granted, the phone's browser found the desk and offered it",
        "the desk's amux://pair link started the app on the desk's name and key before trust, and paired it",
        "the desk received 'Hello from the phone.' once and answered",
    ]


NEEDS_YOU = ROOT / "journeys/fixtures/needs-you.apns"
# A push brings its chat current within this long, or gives up.
WARM_LIMIT = 25


def needs_you_push(journey: PhoneJourney, agent: str) -> Path:
    """The committed needs-you payload, addressed to `agent` on the desk."""
    payload = json.loads(NEEDS_YOU.read_text())
    payload["amux"]["host"] = journey.host_id("desk")
    payload["amux"]["agent"] = next(item["id"] for item in journey.ready["agents"] if item["name"] == agent)
    built = journey.output / "needs-you.apns"
    built.write_text(json.dumps(payload, indent=2) + "\n")
    return built


def back_to_fleet(journey: PhoneJourney) -> None:
    """Back from a chat to the fleet, once the page has finished leaving."""
    journey.tap("chat.back")
    journey.wait(lambda drawn: "chat.field" not in drawn, "the fleet")
    journey.app({"kind": "settle"})


def push_to_handler(journey: PhoneJourney, payload: Path) -> None:
    """Deliver a push to the app put away, the notification and the payload.

    xcrun simctl push shows the notification a person sees. It never reaches
    the app's didReceiveRemoteNotification on this simulator (iOS 26.5): a
    backgrounded app only gets a payload-less background-fetch launch. So the
    debug door hands the same file to that handler, with the app in the
    background as iOS would call it, and answers when the handler returns.
    """
    simctl("push", journey.udid, BUNDLE_ID, str(payload))
    journey.actions.append("xcrun simctl push needs-you.apns: the notification shows")
    journey.app({"kind": "push", "path": str(payload)}, timeout=WARM_LIMIT + 15)
    journey.actions.append(
        "the debug door handed the same needs-you.apns payload to AppDelegate's didReceiveRemoteNotification"
        " with the app in the background"
    )


def push_wake(journey: PhoneJourney) -> list[str]:
    asker = next(item["id"] for item in journey.ready["agents"] if item["name"] == "asker")
    bystander = next(item["id"] for item in journey.ready["agents"] if item["name"] == "bystander")
    journey.launch()
    pair_by_code(journey, "desk")
    journey.tab("Agents")
    journey.wait_for(f"home.row.{asker}", f"home.row.{bystander}")

    # Put away. Both agents move on at the desk: one comes to need the
    # person, the other answers. The app asks for background time first, as
    # an app finishing a piece of work does, so it is still there to hand
    # the push to: this simulator never passes a push's payload to an app
    # put away (see push_to_handler).
    journey.app({"kind": "holdBackground"})
    simctl("launch", journey.udid, "com.apple.Preferences")
    journey.app({"kind": "awaitBackground", "seconds": 10})
    journey.actions.append("the app put away behind Settings, holding background time for the push")
    journey.request({"Send": {"agent": "asker", "text": "Deploy it."}})
    journey.request({"Send": {"agent": "bystander", "text": "Anything new?"}})
    journey.wait_chat("desk", "asker", lambda chat: chat["phase"] == "NEEDS_YOU", "asker-needs-you")
    journey.wait_chat(
        "desk", "bystander",
        lambda chat: chat["phase"] == "IDLE" and any("The logs are quiet." in item["text"] for item in chat["items"]),
        "bystander-answered",
    )

    # The push names the asker. Handled in the background, it brings that
    # one chat current and nothing else.
    push_to_handler(journey, needs_you_push(journey, "asker"))

    # With the desk gone, what the phone holds is what the push fetched.
    journey.request({"StopDaemon": {"host": "desk"}})
    simctl("launch", journey.udid, BUNDLE_ID)
    journey.actions.append("the app brought back to the foreground")
    marks = [mark["signpost"] for mark in journey.app({"kind": "signposts"})["marks"]]
    woke = [mark for mark in marks if mark.startswith("push")]
    journey.observations["push-signposts"] = woke
    if woke != ["pushWoke", "pushCurrent"]:
        raise RuntimeError(f"the push did not bring its chat current: the app marked {woke!r}")
    journey.wait(lambda drawn: labelled(drawn, "desk is offline"), "the desk offline on the phone")
    journey.tap(f"home.row.{asker}")
    journey.wait(lambda drawn: "ask" in drawn and labelled(drawn, "deploy --prod"), "the asker's ask, fetched by the push")
    journey.screen("warmed", volatile=("chat.row.turn-end",))
    back_to_fleet(journey)
    journey.tap(f"home.row.{bystander}")
    drawn = journey.wait(lambda drawn: labelled(drawn, "Watching the logs.") and "chat.field" in drawn, "the bystander's chat")
    if labelled(drawn, "The logs are quiet."):
        raise RuntimeError("the bystander's chat was brought current by a push that named the asker")
    journey.screen("not-warmed", volatile=("chat.row.turn-end",))
    journey.tap("chat.back")
    journey.wait(lambda drawn: "chat.field" not in drawn, "the fleet")

    # In the foreground every agent is listed again: the desk back, the
    # bystander's answer arrives, and the ask is answered from the phone.
    journey.request({"RestartDaemon": {"host": "desk"}})
    journey.tap(f"home.row.{bystander}")
    journey.wait(lambda drawn: labelled(drawn, "The logs are quiet."), "the bystander current in the foreground", timeout=90)
    back_to_fleet(journey)
    journey.tap(f"home.row.{asker}")
    card = journey.wait(lambda drawn: "ask" in drawn and labelled(drawn, "Allow"), "the ask again")
    journey.tap(choice(card, "Allow"))
    journey.wait(lambda drawn: labelled(drawn, "Deployed to production."), "the deploy reply")
    journey.wait_chat(
        "desk", "asker",
        lambda chat: chat["phase"] == "IDLE" and any("Deployed to production." in item["text"] for item in chat["items"]),
        "asker-answered",
    )
    reopen(journey, asker, lambda drawn: labelled(drawn, "Deployed to production.") and "chat.row.turn-end" in drawn)
    # The command row's duration is how long the ask waited on the phone.
    journey.screen("answered", volatile=("chat.row.turn-end", "chat.row.command"))
    journey.tap("chat.back")
    return [
        "put away, the desk's asker came to need the person and the bystander answered",
        "a push built from journeys/fixtures/needs-you.apns showed its notification, and its payload, handed to"
        " the app's remote-notification handler through the debug door with the app in the background, brought"
        " the asker's chat current: with the desk stopped it held the ask",
        "the bystander's chat did not hold the answer it gave while the app was away: only the named chat was brought current",
        "back in the foreground with the desk running again, the bystander caught up without being named",
        "Allow from the phone reached the desk, which finished the deploy",
    ]


STORIES = {
    "reach-host": reach_host,
    "conversation-decision-claude-pty": lambda j: conversation_decision(j, "decision-pty", False),
    "conversation-decision-claude-sdk": lambda j: conversation_decision(j, "decision-sdk", True),
    "conversation-decision-codex": lambda j: conversation_decision(j, "decision-codex", True),
    "decide-plan-claude-sdk": lambda j: decide_plan(j, "planner", True),
    "decide-plan-claude-pty": lambda j: decide_plan(j, "planner-pty", False),
    "decide-plan-codex": decide_plan_codex,
    "answer-questions": answer_questions,
    "tool-server-asks": tool_server_asks,
    "queue-and-steer": queue_and_steer,
    "send-while-away": send_while_away,
    "composer-limits": composer_limits,
    "keep-authority": keep_authority,
    "manage-agent": manage_agent,
    "attachment-or-review": attachment_or_review,
    "leave-and-recover": leave_and_recover,
    "account-sign-in": account_sign_in,
    "purchase-restore": purchase_restore,
    "report": report,
    "accessibility": accessibility,
    "local-network": local_network,
    "push-wake": push_wake,
}


def native() -> list[str]:
    """The stories the manifest declares for the phone and no other client."""
    manifest = json.loads(MANIFEST.read_text())
    return [item["id"] for item in manifest["journeys"] if item.get("clients") == ["phone"]]


def selected(arguments: list[str]) -> list[str]:
    if arguments == ["--native"]:
        return native()
    if "--native" in arguments:
        raise SystemExit("--native runs the phone's own stories and takes no story names")
    return arguments or list(STORIES)


def main() -> int:
    wanted = selected(sys.argv[1:])
    unknown = [name for name in wanted if name not in STORIES]
    if unknown:
        print(f"no phone story written for {', '.join(unknown)}; one of {', '.join(STORIES)}", file=sys.stderr)
        return 2
    udid = ios_simulators.ready(SIMULATOR)
    failed = 0
    for name in wanted:
        declared = story(name)
        topology = ROOT / declared.get("phone_topology", declared["topology"])
        began = time.monotonic()
        journey = PhoneJourney(declared, topology, udid)
        try:
            assertions = STORIES[name](journey)
            journey.finish(assertions)
            for assertion in assertions:
                print(f"- {assertion}")
            print(f"{name}: journey took {time.monotonic() - began:.1f}s", flush=True)
            # The verifier recognizes this exact final line as a pass.
            print(f"{name}: passed", flush=True)
        except BaseException as error:
            journey.fail(error)
            print(f"FAIL {name}: {error}", file=sys.stderr, flush=True)
            failed += 1
        finally:
            journey.close()
    return 1 if failed else 0


if __name__ == "__main__":
    # Let the recipe's timeout unwind the served net and the app.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    sys.exit(main())
