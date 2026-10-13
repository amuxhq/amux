#!/usr/bin/env python3
"""Run a shared journey story on the real terminal client.

`scripts/terminal-journey.py <story>`: one story from journeys/manifest.json
against the served topology it names, with frames compared to reviewed
goldens (UPDATE_JOURNEY_GOLDENS=1 rewrites them) and independent
observations from the hosts. Results land in target/journeys/<story>.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

from journeys.terminal import ROOT, TerminalJourney, agent_row, at_home, at_rest, chat_of, story, working

PROMPT = "Check the deployment once."
REPLY = "The deploy check passed."
DRAFT = "Keep this draft through the outage."


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


def conversation_decision(journey: TerminalJourney, agent: str, provider_logs: bool) -> list[str]:
    pane = journey.launch("terminal", "desk")
    journey.open_chat(pane, agent)
    journey.type(pane, PROMPT)
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, PROMPT)
    card = journey.read_ask(pane, "Wants to run", "deploy --check", "1. Yes")
    sent = journey.wait_chat("desk", agent, lambda chat: len(prompts(chat, PROMPT)) >= 1, "prompt-reflected")
    reflected_once(sent, PROMPT)
    control = negative_control(reflected_once, sent, PROMPT + " (a wrong prompt)")
    journey.frame(pane, "permission")
    # The first choice is Yes, this once.
    journey.keys(pane, "1")
    journey.wait(
        pane,
        lambda frame: at_rest(frame, REPLY) and "Wants to run" not in frame,
        "the settled turn",
    )
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
        "the permission ask offered its choices in the composer's box and Yes was taken",
        f"the reply {REPLY!r} arrived and the desk says idle",
    ]
    if provider_logs:
        lines = journey.provider_input(agent, "provider-input")
        answers = [line for line in lines if "allow" in line.lower() or "accept" in line.lower()]
        if len(answers) != 1:
            raise RuntimeError(f"the provider did not receive one allow: {lines!r}")
        assertions.append("the provider received exactly one allow")
    journey.frame(pane, "settled")
    del card
    journey.quit_client(pane)
    assertions.append("the client exited 0")
    return assertions


def leave_and_recover(journey: TerminalJourney) -> list[str]:
    first = "The recovery state is safely stored."
    second = "A second turn arrived while you were away."
    third = "Back after the restart."
    # A real first run on the laptop makes its replica of the desk's agent.
    pane = journey.launch("first-run", "laptop")
    journey.open_chat(pane, "keeper")
    journey.wait_terms(pane, first)
    journey.wait_chat("laptop", "keeper", lambda chat: any(first in i["text"] for i in chat["items"]), "replica-made")
    journey.quit_client(pane)

    # The desk goes out of reach; a fresh client opens from the laptop's
    # store, with the composer waiting and the draft kept.
    journey.request({"Sever": {"a": "desk", "b": "laptop"}})
    pane = journey.launch("terminal", "laptop")
    journey.open_chat(pane, "keeper")
    journey.wait_terms(pane, first, "desk away · not current")
    journey.type(pane, DRAFT)
    journey.wait_terms(pane, DRAFT, "enter holds it", "it sends when the host is back")
    journey.frame(pane, "cached-offline")

    # Work goes on at the desk while the laptop is cut off; when the link
    # returns the delta appends once and the composer is live again.
    journey.request({"Send": {"agent": "keeper", "text": "Carry on."}})
    journey.wait_chat("desk", "keeper", lambda chat: any(second in i["text"] for i in chat["items"]), "desk-moved-on")
    journey.request({"Restore": {"a": "desk", "b": "laptop"}})
    journey.wait(
        pane,
        lambda frame: second in frame and "not current" not in frame and DRAFT in frame,
        "the reconciled chat with the draft kept",
    )
    desk = journey.chat("desk", "keeper", "desk-after-restore")
    laptop = journey.wait_chat(
        "laptop",
        "keeper",
        lambda chat: [i["key"] for i in chat["items"]] == [i["key"] for i in desk["items"]],
        "laptop-matches-desk",
    )
    if sum(second in item["text"] for item in laptop["items"]) != 1:
        raise RuntimeError(f"the delta arrived other than once: {laptop!r}")
    journey.frame(pane, "reconciled")

    # The laptop's own daemon restarts under the open chat: the client
    # reconnects by itself, keeps the chat and the draft, and sends. The
    # daemon stays down until the client says so, because the live screen
    # reads the same before the outage as after it and a send pressed while
    # the client is reconnecting is not taken.
    journey.request({"KillDaemon": {"host": "laptop"}})
    journey.wait_terms(pane, "reconnecting", DRAFT, "it sends once amux is back")
    journey.request({"RestartDaemon": {"host": "laptop"}})
    journey.wait(
        pane,
        lambda frame: DRAFT in frame and "enter holds it" not in frame and second in frame,
        "the chat live again after the restart",
    )
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, third)
    after = journey.wait_chat("desk", "keeper", lambda chat: any(third in i["text"] for i in chat["items"]), "sent-after-restart")
    reflected_once(after, DRAFT)
    journey.frame(pane, "after-restart")
    # The desk rewinds: the laptop's replica is Reset, and the rows stay on
    # screen until the rebuilt transcript swaps in.
    journey.request({"Checkpoint": {"host": "desk"}})
    journey.request({"Rewind": {"host": "desk", "cuts": []}})
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        screen = journey.capture(pane)
        if first not in screen or third not in screen:
            raise RuntimeError(f"rows left the screen during the Reset:\n{screen}")
        time.sleep(0.05)
    desk = journey.chat("desk", "keeper", "desk-after-rewind")
    journey.wait_chat(
        "laptop",
        "keeper",
        lambda chat: [i["key"] for i in chat["items"]] == [i["key"] for i in desk["items"]],
        "laptop-after-reset",
    )
    # A restored drive has no running processes: the agent reads exited.
    journey.wait(pane, lambda frame: "not current" not in frame and third in frame and "enter resume" in frame, "the chat after the Reset")
    journey.frame(pane, "after-reset")

    journey.quit_client(pane)
    return [
        "a fresh client painted the cached chat from the laptop's store with the desk away and the composer waiting",
        "the draft typed while detached was kept through the outage and the daemon restart",
        "the desk's second turn appended once when the link returned; the laptop holds exactly the desk's items",
        "after the laptop's daemon restarted the client reconnected on its own and the draft was sent and answered",
        "rows stayed on screen through the desk's rewind and Reset until the rebuilt transcript swapped in",
        "the client exited 0",
    ]


def listed(agents: list[dict], agent_id: str) -> dict | None:
    return next((agent for agent in agents if agent["id"] == agent_id), None)


def lifecycle(agents: list[dict], agent_id: str, name: str, state: int) -> None:
    """The host lists `agent_id` under `name` in lifecycle `state`."""
    agent = listed(agents, agent_id)
    if agent is None or agent["name"] != name or agent["lifecycle"] != state:
        raise RuntimeError(f"the host lists {agent_id} as {agent!r}, not {name!r} in lifecycle {state}")


LIVE, EXITED = 1, 2
HELLO = "Say hello."
BACK = "Welcome back."
READY = "Ready when you are."


def manage_agent(journey: TerminalJourney) -> list[str]:
    before = {agent["id"] for agent in journey.inventory("desk", "inventory-before")}
    pane = journey.launch("terminal", "desk")
    journey.wait_terms(pane, "keeper", "n new")
    # Started from home with its first prompt: a Claude the desk runs
    # headless, whose chat opens on the first reply.
    journey.keys(pane, "n")
    journey.wait_terms(pane, "What should the new agent work on?", "Claude")
    journey.type(pane, HELLO)
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: chat_of(frame) is not None and at_rest(frame, HELLO, READY), "the first reply")
    created = journey.wait_inventory(
        "desk", lambda agents: len({a["id"] for a in agents} - before) == 1, "created"
    )
    (agent_id,) = {agent["id"] for agent in created} - before
    made = listed(created, agent_id)
    if made is None or made["lifecycle"] != LIVE:
        raise RuntimeError(f"the desk lists the new agent as {made!r}")
    # Created without a name, it has the word pair its host gave it.
    if not made.get("name"):
        raise RuntimeError(f"the desk gave the new agent no name: {made!r}")
    journey.assigned.append(made["name"])
    journey.frame(pane, "created")

    # Renamed in place on home: the host lists the same agent under the name.
    journey.home(pane)
    journey.select_agent(pane, made["name"])
    journey.keys(pane, "r", "C-u")
    journey.type(pane, "helper")
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: agent_row(frame, "helper") is not None, "helper on home")
    lifecycle(
        journey.wait_inventory("desk", lambda agents: (listed(agents, agent_id) or {}).get("name") == "helper", "renamed"),
        agent_id,
        "helper",
        LIVE,
    )

    # Stopped: exited on the host with its history kept, and resumable.
    journey.select_agent(pane, "helper")
    journey.keys(pane, "x")
    journey.wait_terms(pane, "Stop helper? It can be resumed later.")
    journey.frame(pane, "stop-asked")
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: "▸ Exited 1 " in frame and agent_row(frame, "helper") is None, "helper exited")
    stopped = journey.wait_inventory(
        "desk", lambda agents: (listed(agents, agent_id) or {}).get("lifecycle") == EXITED, "stopped"
    )
    lifecycle(stopped, agent_id, "helper", EXITED)
    control = negative_control(lifecycle, stopped, agent_id, "helper", LIVE)
    kept = journey.chat("desk", agent_id, "history-kept")
    reflected_once(kept, HELLO)
    if not any(READY in item["text"] for item in kept["items"]):
        raise RuntimeError(f"the stopped agent's history lost its reply: {kept!r}")
    journey.open_exited(pane)
    journey.open_chat(pane, "helper")
    journey.wait_terms(pane, HELLO, READY, "enter resume")
    journey.frame(pane, "exited-and-resumable")

    # Resumed from the exited composer: the same identity, live again.
    journey.type(pane, BACK)
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: frame.count(READY) == 2 and not working(frame), "the resumed reply")
    lifecycle(
        journey.wait_inventory("desk", lambda agents: (listed(agents, agent_id) or {}).get("lifecycle") == LIVE, "resumed"),
        agent_id,
        "helper",
        LIVE,
    )
    resumed = journey.wait_chat("desk", agent_id, lambda chat: len(prompts(chat, BACK)) == 1, "resumed-chat")
    reflected_once(resumed, HELLO)
    reflected_once(resumed, BACK)
    journey.frame(pane, "resumed")

    # Deleted, explicitly: only an exited agent offers it, so stopped
    # again first, then gone from home and from the host.
    journey.home(pane)
    journey.select_agent(pane, "helper")
    journey.keys(pane, "x")
    journey.wait_terms(pane, "Stop helper? It can be resumed later.")
    journey.keys(pane, "Enter")
    journey.wait_inventory(
        "desk", lambda agents: (listed(agents, agent_id) or {}).get("lifecycle") == EXITED, "stopped-again"
    )
    if "▸ Exited " in journey.wait_terms(pane, "Exited "):
        journey.open_exited(pane)
    journey.select_agent(pane, "helper")
    journey.keys(pane, "x")
    journey.wait_terms(pane, "Delete helper and its history?")
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: agent_row(frame, "helper") is None, "home without helper")
    journey.wait_inventory("desk", lambda agents: listed(agents, agent_id) is None, "deleted")
    journey.frame(pane, "deleted")
    journey.quit_client(pane)
    return [
        "n, a first prompt and enter started a headless Claude the desk lists, and its chat opened on the reply",
        "r renamed it in place; the desk lists the same id as helper",
        "x stopped it: the desk lists it exited, with its prompt and reply kept",
        control,
        "a message from the exited composer resumed the same id, and each prompt is in the desk's chat once",
        "stopped again, x on its exited row deleted it: gone from home and from the desk",
        "the client exited 0",
    ]


def pin_of(frame: str) -> str:
    found = re.search(r"Pairing PIN: (\d{3}) (\d{3})", frame)
    if found is None:
        raise RuntimeError(f"no PIN on the desk:\n{frame}")
    return found.group(1) + found.group(2)


def identified(frame: str, host_id: str) -> None:
    """The laptop names the desk by the desk's own host id."""
    if f"Paired with desk ({host_id}) on this network." not in frame:
        raise RuntimeError(f"the pairing did not name desk {host_id}:\n{frame}")


def reach_host(journey: TerminalJourney) -> list[str]:
    desk_id = journey.host_id("desk")
    prompt = "Hello from the laptop."
    # Unpaired: the laptop finds the desk and says how to pair with it.
    pane = journey.launch("terminal", "laptop")
    journey.wait(pane, at_home, "home")
    journey.keys(pane, "h")
    journey.wait_terms(pane, "desk", "found nearby · amux pair desk")
    journey.frame(pane, "found-not-paired")
    peers = journey.launch("peers", "laptop", "peers")
    listing = journey.wait_terms(peers, "AMUX_EXIT_0")
    if not re.search(rf"^desk\s+{desk_id}\s+found nearby\s+no: amux pair desk$", listing, re.M):
        raise RuntimeError(f"peers does not list the desk by its id as found:\n{listing}")

    # Six digits the desk printed pair the laptop with it.
    opened = journey.launch("pairing", "desk", "pair")
    pin = pin_of(journey.wait_terms(opened, "Pairing PIN:", "Ctrl+C closes it."))
    asking = journey.launch("pair", "laptop", "pair", "desk")
    journey.wait_terms(asking, "Pairing PIN shown on the other host:")
    journey.type(asking, pin)
    journey.keys(asking, "Enter")
    paired = journey.wait_terms(asking, "Paired with desk", "AMUX_EXIT_0")
    identified(paired, desk_id)
    control = negative_control(identified, paired, "00000000-0000-0000-0000-000000000000")
    journey.wait_terms(opened, "Pairing mode ended.", "AMUX_EXIT_0")
    desk_peers = journey.launch("desk-peers", "desk", "peers")
    trusted = journey.wait_terms(desk_peers, "AMUX_EXIT_0")
    laptop_id = journey.host_id("laptop")
    if not re.search(rf"^laptop\s+{laptop_id}\s.*\syes$", trusted, re.M):
        raise RuntimeError(f"the desk does not list the laptop as paired:\n{trusted}")

    # The running client's overlay says so, and the desk's work is there.
    journey.wait(
        pane,
        lambda frame: "found nearby" not in frame and re.search(r"desk\s+direct", frame) is not None,
        "the desk paired in the overlay",
    )
    journey.frame(pane, "paired")
    journey.keys(pane, "Escape")
    journey.open_chat(pane, "desk-work")
    journey.type(pane, prompt)
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: at_rest(frame, "The desk is reachable."), "the desk's reply")
    heard = journey.wait_chat("desk", "desk-work", lambda chat: chat["phase"] == "IDLE" and len(prompts(chat, prompt)) == 1, "desk-heard")
    reflected_once(heard, prompt)
    journey.frame(pane, "usable-chat")
    journey.quit_client(pane)
    return [
        "the unpaired laptop listed the desk as found, with `amux pair desk`, and `amux peers` named it by its id",
        f"the PIN the desk printed paired the laptop, which named desk {desk_id}",
        control,
        "the desk lists the laptop as paired",
        "the running client's overlay showed the desk online and direct without a restart",
        f"the desk's agent opened from the laptop; the desk holds {prompt!r} once and its reply",
        "the client left the chat and exited 0",
    ]


PASTED = "\n".join(f"deploy log {n}: rsync finished with status 0" for n in range(12))
COMMENT = "Why --delete here?"


def reviewed(chat: dict, comment: str) -> dict:
    """The one prompt carrying the paste and the review, as the desk holds it."""
    sent = [item for item in chat["items"] if item["attachments"]]
    if len(sent) != 1:
        raise RuntimeError(f"the desk holds {len(sent)} prompts with attachments")
    item = sent[0]
    kinds = [attachment["kind"] for attachment in item["attachments"]]
    if item["text"] != "Please check \ufffc against \ufffc" or kinds != ["text", "review"]:
        raise RuntimeError(f"the prompt's order is not text, paste, text, review: {item!r}")
    if item["attachments"][0]["text"] != PASTED:
        raise RuntimeError("the pasted text arrived changed")
    comments = item["attachments"][1]["comments"]
    if comments != [{"path": "deploy.sh", "line": 3, "old_line": 0, "text": comment}]:
        raise RuntimeError(f"the review's comments are {comments!r}")
    return item


def attachment_or_review(journey: TerminalJourney) -> list[str]:
    # The reviewer's working tree on the desk: one commit and an edit.
    work = Path(journey.ready["root"]) / "desk" / "work"

    # A pinned author and date keep the commit id the review page shows.
    pinned = {"GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z", "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z"}

    def git(*args: str) -> None:
        subprocess.run(
            ["git", "-C", str(work), "-c", "user.name=Journey", "-c", "user.email=journey@example.invalid", *args],
            check=True,
            capture_output=True,
            env={**os.environ, **pinned},
        )

    git("init", "-q", "-b", "main")
    (work / "deploy.sh").write_text("#!/bin/sh\necho deploying\nrsync build/ prod:/srv\n")
    git("add", "deploy.sh")
    git("commit", "-qm", "Deploy by rsync")
    (work / "deploy.sh").write_text("#!/bin/sh\necho deploying\nrsync --delete build/ prod:/srv\necho done\n")

    # On the laptop: text, a long paste as one token, text, and a review of
    # the desk's working tree with one comment.
    pane = journey.launch("terminal", "laptop")
    journey.open_chat(pane, "reviewer")
    journey.type(pane, "Please check ")
    journey.paste(pane, PASTED)
    journey.wait_terms(pane, "Please check [pasted-1 · 12 lines]")
    # The chip is followed by a space of its own.
    journey.type(pane, "against ")
    journey.keys(pane, "C-a", "r")
    journey.wait_terms(pane, "review · working tree at", "deploy.sh  +2 −1")
    for _ in range(12):
        if re.search(r"│▌\s+3 \+ rsync --delete", journey.capture(pane)):
            break
        journey.keys(pane, "j")
    else:
        raise RuntimeError(f"no line to comment on:\n{journey.capture(pane)}")
    journey.frame(pane, "review-diff")
    journey.keys(pane, "c")
    journey.type(pane, COMMENT)
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, "1 file · +2 −1 · 1 comment", f"│ {COMMENT}")
    journey.frame(pane, "review-comment")
    journey.keys(pane, "q")
    journey.wait_terms(pane, "Please check [pasted-1 · 12 lines] against [review · 1 comment]")
    journey.frame(pane, "composer-tokens")
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: at_rest(frame, "I read the review."), "the reviewer's reply")

    # The desk received the tokens in order, the paste's exact text, the
    # comment, and the patch it made itself, whose bytes are on its disk.
    received = journey.wait_chat("desk", "reviewer", lambda chat: chat["phase"] == "IDLE" and any(i["attachments"] for i in chat["items"]), "received")
    item = reviewed(received, COMMENT)
    control = negative_control(reviewed, received, COMMENT + " (a wrong comment)")
    patch = item["attachments"][1]["patch"]
    found = [path for path in (Path(journey.ready["root"]) / "desk" / "data").rglob(patch) if path.parent.name == "blobs"]
    if len(found) != 1:
        raise RuntimeError(f"the desk holds the patch {patch} {len(found)} times")
    bytes_ = found[0].read_bytes()
    if hashlib.sha256(bytes_).hexdigest() != patch or b"+rsync --delete build/ prod:/srv" not in bytes_:
        raise RuntimeError("the patch on the desk is not the reviewed diff")

    # Revisited: the chat opened again shows the paste's opening lines in
    # place and the review as its token.
    journey.home(pane)
    journey.open_chat(pane, "reviewer")
    journey.wait_terms(pane, "Please check", "deploy log 0:", "against [review · 1 comment]", "I read the review.")
    journey.frame(pane, "reopened")
    journey.quit_client(pane)
    return [
        "the prompt reached the desk as text, the paste, text and the review, in that order",
        "the paste arrived as one token holding exactly the twelve lines pasted",
        f"the review carries one comment on deploy.sh line 3: {COMMENT!r}",
        control,
        "the review names a patch the desk computed; its bytes on the desk hash to that name and hold the edit",
        "the chat opened again shows the paste in place, the review token and the reply",
        "the client exited 0",
    ]


def never_received(chat: dict, text: str) -> None:
    if prompts(chat, text):
        raise RuntimeError(f"the desk received {text!r} while access was lost")


def keep_authority(journey: TerminalJourney) -> list[str]:
    asked = "Are you there?"
    held = "Still there?"
    # The two machines are one account's, reached only through the relay.
    journey.request({"Trust": {"a": "desk", "b": "laptop"}})
    journey.request({"Trust": {"a": "laptop", "b": "desk"}})

    # A second profile on the laptop is a fleet of its own.
    created = journey.launch("profile", "laptop", "profile", "create", "work")
    journey.wait_terms(created, "Created profile work", "AMUX_EXIT_0")
    other = journey.launch("other", "laptop", "--profile", "work")
    journey.wait_terms(other, "no agents yet")
    journey.keys(other, "h")
    journey.wait(other, lambda frame: re.search(r"laptop\s+this machine", frame) is not None and "desk" not in frame, "hosts without the desk")
    journey.frame(other, "other-profile")
    journey.keys(other, "Escape")
    journey.quit_client(other)

    # The default profile reaches the desk's agent through the relay.
    pane = journey.launch("terminal", "laptop")
    journey.open_chat(pane, "guarded")
    journey.type(pane, asked)
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: at_rest(frame, "Only this account reaches me."), "the first reply")
    reflected_once(journey.wait_chat("desk", "guarded", lambda chat: chat["phase"] == "IDLE", "reached"), asked)

    # Signed out, the laptop has no way to the desk: the chat says so and
    # a message written now is held, never sent.
    signed_out = journey.launch("logout", "laptop", "logout")
    journey.wait_terms(signed_out, "Signed default out", "AMUX_EXIT_0")
    # The reason named is this machine's sign-out, never a claim about the
    # desk, and the hosts overlay says the same.
    journey.wait_terms(pane, "desk away · this machine is signed out", "it sends once this machine signs in")
    journey.home(pane)
    journey.keys(pane, "h")
    journey.wait_terms(pane, "signed out · amux login", "away · this machine is signed out")
    journey.frame(pane, "blocked-hosts")
    journey.keys(pane, "Escape")
    journey.open_chat(pane, "guarded")
    journey.wait_terms(pane, "desk away · this machine is signed out")
    journey.type(pane, held)
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, held, "sends when desk is back")
    journey.frame(pane, "blocked")
    time.sleep(2)
    blocked = journey.chat("desk", "guarded", "while-signed-out")
    never_received(blocked, held)
    control = negative_control(never_received, blocked, asked)

    # Signed in again: the chat is current and the held message goes by
    # itself, once.
    journey.request({"SignIn": {"host": "laptop", "account": "ada"}})
    journey.wait(
        pane,
        lambda frame: "not current" not in frame and at_rest(frame, held, "The relay carries us again.") and "sends when" not in frame,
        "the reply after signing in",
    )
    after = journey.wait_chat("desk", "guarded", lambda chat: chat["phase"] == "IDLE" and len(prompts(chat, held)) == 1, "sent-after-sign-in")
    reflected_once(after, asked)
    reflected_once(after, held)
    journey.frame(pane, "recovered")
    journey.quit_client(pane)
    return [
        "a second profile on the laptop opened on its own empty fleet, knowing no desk",
        f"through the relay the desk received {asked!r} once and answered",
        "signed out, the laptop named its own sign-out as the reason the desk was away, in the chat and the hosts overlay, and held the message; the desk never received it",
        control,
        f"signed in again, the chat was current and the held {held!r} went by itself, reached the desk once and was answered",
        "the clients exited 0",
    ]



def control_responses(journey: TerminalJourney, agent: str, label: str) -> list[dict]:
    """What headless Claude was answered, in order."""
    lines = [json.loads(line) for line in journey.provider_input(agent, label)]
    return [line["response"]["response"] for line in lines if line.get("type") == "control_response"]


def codex_results(journey: TerminalJourney, agent: str, label: str) -> list[dict]:
    """What Codex was answered, in order."""
    lines = [json.loads(line) for line in journey.provider_input(agent, label)]
    return [line["result"] for line in lines if "result" in line and isinstance(line.get("id"), int)]


PLAN_PROMPT = "Move the journal."
PLAN_NOTE = "Check the copy before switching."
PLAN_DONE = "Moving the journal as planned."


def sent_back_then_approved(decisions: list[dict], note: str) -> None:
    """The plan went back once with `note`, then was approved once."""
    behaviors = [decision.get("behavior") for decision in decisions]
    if behaviors != ["deny", "allow"] or not decisions[0].get("message", "").endswith(note):
        raise RuntimeError(f"Claude was answered {decisions!r}")


PLAN_FILE = ".claude/plans/"


def plans_only(chat: dict) -> None:
    """The desk holds the two plans as its only tool items: the plan file
    Claude wrote before each is no step of the chat."""
    calls = [item for item in chat["items"] if item["key"].startswith("toolu_")]
    if len(calls) != 2 or not all(item["text"].startswith("# Move the journal") for item in calls):
        raise RuntimeError(f"the desk holds {[item['text'][:40] for item in calls]!r} as tool items")


def no_plan_file(frame: str) -> None:
    """The screen shows no step writing the plan file, open or folded
    into the steps before the plan."""
    if PLAN_FILE in frame or "move-the-journal.md" in frame or re.search(r"\b\d+ edits?\b", frame):
        raise RuntimeError(f"the plan file shows as a step:\n{frame}")


def decide_plan(journey: TerminalJourney, agent: str, headless: bool) -> list[str]:
    pane = journey.launch("terminal", "laptop")
    journey.open_chat(pane, agent)
    journey.type(pane, PLAN_PROMPT)
    journey.keys(pane, "Enter")
    no_plan_file(journey.read_ask(pane, "Plan ready", "3. Delete the old journal.", "No, keep planning"))
    journey.frame(pane, "plan")
    # Esc reaches the way out; Tab opens the note it carries.
    journey.keys(pane, "Escape", "Tab")
    journey.type(pane, PLAN_NOTE)
    journey.wait_terms(pane, f"No, keep planning: {PLAN_NOTE}")
    journey.keys(pane, "Enter")
    no_plan_file(journey.read_ask(pane, "sent back", PLAN_NOTE, "4. Keep the old journal for a week.", "Plan ready"))
    journey.frame(pane, "revised")
    journey.keys(pane, "1")
    journey.wait(pane, lambda frame: at_rest(frame, "approved", PLAN_DONE), "the approved plan's work")
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
        "the plan showed in the feed with its decision in the composer's box",
        "sent back with a note, the revised plan arrived; approved, the work ran",
        "the plan file Claude wrote before each plan showed as no step, on screen or on the desk",
        negative_control(plans_only, {"items": [*settled["items"], {"key": "toolu_write", "text": ""}]}),
    ]
    if headless:
        decisions = control_responses(journey, agent, "plan-decisions")
        sent_back_then_approved(decisions, PLAN_NOTE)
        assertions.append("Claude received the plan sent back with the note, then one approval")
        assertions.append(negative_control(sent_back_then_approved, decisions, "a note never written"))
    journey.frame(pane, "approved")
    journey.quit_client(pane)
    assertions.append("the client exited 0")
    return assertions


ROTATE_PROMPT = "Rotate the logs."
ROTATE_NOTE = "Keep a month of logs, compressed."
IMPLEMENT = "Implement the plan."


def codex_turns(journey: TerminalJourney, agent: str, label: str) -> list[tuple[str, str]]:
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


def decide_plan_codex(journey: TerminalJourney) -> list[str]:
    agent = "planner-codex"
    pane = journey.launch("terminal", "laptop")
    journey.open_chat(pane, agent)
    # Shift+Tab puts Codex in plan mode before the prompt.
    journey.keys(pane, "BTab")
    journey.wait_terms(pane, "· plan ─╯")
    journey.type(pane, PLAN_PROMPT)
    journey.keys(pane, "Enter")
    # The plan is in the feed while the turn still runs.
    journey.wait(
        pane,
        lambda frame: "3. Delete the old journal." in frame and working(frame) and "Plan ready" not in frame,
        "the plan streaming",
    )
    journey.frame(pane, "plan-streaming")
    journey.request({"OpenGate": {"name": "plan-streamed"}})
    journey.read_ask(pane, "Plan ready", "1. Yes, implement this plan", "No, keep planning")
    journey.wait_chat("desk", agent, lambda chat: chat["phase"] == "NEEDS_YOU", "plan-needs-you")
    journey.frame(pane, "plan-ready")
    journey.keys(pane, "1")
    journey.wait(pane, lambda frame: at_rest(frame, "approved", PLAN_DONE), "the implemented plan's work")
    # A second plan, sent back with a note: Codex plans again on it.
    journey.keys(pane, "BTab")
    journey.wait_terms(pane, "· plan ─╯")
    journey.type(pane, ROTATE_PROMPT)
    journey.keys(pane, "Enter")
    journey.read_ask(pane, "Plan ready", "2. Delete logs older than a week.")
    journey.keys(pane, "Escape", "Tab")
    journey.type(pane, ROTATE_NOTE)
    journey.wait_terms(pane, f"No, keep planning: {ROTATE_NOTE}")
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, "sent back", ROTATE_NOTE, "3. Delete logs older than a month.", "Plan ready")
    journey.wait_chat("desk", agent, lambda chat: chat["phase"] == "NEEDS_YOU" and len(prompts(chat, ROTATE_NOTE)) == 1, "revised")
    journey.frame(pane, "sent-back")
    expected = [
        ("plan", PLAN_PROMPT),
        ("default", IMPLEMENT),
        ("plan", ROTATE_PROMPT),
        ("plan", ROTATE_NOTE),
    ]
    turns = codex_turns(journey, agent, "plan-turns")
    codex_plan_turns(turns, expected)
    control = negative_control(codex_plan_turns, turns, [*expected[:3], ("default", ROTATE_NOTE)])
    journey.quit_client(pane)
    return [
        "the plan showed in the feed while Codex's turn still ran",
        "when the turn ended the plan's decision took the composer's box and the desk said the agent needs you",
        f"implement started a turn out of plan mode with {IMPLEMENT!r}, and its work ran",
        "a second plan, sent back with a note, came back revised; the note went as the next prompt, still in plan mode",
        "Codex was started on the prompt and the note in plan mode and on the implement turn in default mode",
        control,
        "the client exited 0",
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


def answer_questions(journey: TerminalJourney) -> list[str]:
    pane = journey.launch("terminal", "laptop")
    journey.open_chat(pane, "asker")
    journey.type(pane, "Add a settings screen.")
    journey.keys(pane, "Enter")
    # The first option's preview beside the options.
    journey.read_ask(pane, "How should the settings screen be laid out?", "1. Sidebar", "│ Address  relay.amux.dev")
    journey.frame(pane, "first-question")
    journey.keys(pane, "Down")
    journey.wait_terms(pane, "Relay ──────────", "Palette  terminal")
    journey.frame(pane, "other-preview")
    journey.keys(pane, "2")
    journey.wait_terms(pane, "Where should the screen open from?", "3. Something else")
    journey.keys(pane, "3")
    journey.type(pane, OTHER)
    journey.wait_terms(pane, f"Something else: {OTHER}")
    journey.keys(pane, "Enter")
    # The third is skipped from the row under the options.
    journey.wait_terms(pane, SKIPPED, "Skip", "Reply instead")
    journey.keys(pane, "Down", "Down", "Down")
    journey.wait_terms(pane, "› Skip")
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, "Send answers", "→ Stacked", f"→ {OTHER}", "→ skipped")
    journey.frame(pane, "review")
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: at_rest(frame, "Answered 2 of 3 questions", "Thanks, I'll build it that way."), "the reply")
    journey.frame(pane, "answered")
    # A second ask, replied to instead: Esc points at Reply instead.
    journey.type(pane, PALETTE)
    journey.keys(pane, "Enter")
    journey.read_ask(pane, "Which palette should it open in?", "Reply instead")
    journey.keys(pane, "Escape")
    journey.wait_terms(pane, "› Reply instead")
    journey.keys(pane, "Enter")
    journey.type(pane, QUESTION_REPLY)
    journey.wait_terms(pane, f"Reply instead: {QUESTION_REPLY}")
    journey.frame(pane, "reply-instead")
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: at_rest(frame, QUESTION_REPLY, "I'll leave the palette for later."), "the reply to the reply")
    journey.frame(pane, "replied")
    decisions = control_responses(journey, "asker", "answers")
    answered(decisions, ANSWERS)
    replied(decisions, QUESTION_REPLY)
    controls = [
        negative_control(answered, decisions, {**ANSWERS, SKIPPED: "Relay"}),
        negative_control(replied, decisions, "Pick Terminal."),
    ]
    journey.quit_client(pane)
    return [
        "each option's preview showed beside the options as the selection moved",
        "a picked option, a typed answer under Something else and a skipped question showed together for review",
        "Claude received one answer naming Stacked and the typed text, with the skipped question left out",
        "a second ask, replied to instead, reached Claude as the question refused with the person's words",
        *controls,
        "the client exited 0",
    ]


FORM = {"title": "Reconnect flake in e2e", "team": "FOX", "estimate": 3}


def form_sent(decisions: list[dict], content: dict) -> None:
    if len(decisions) != 1 or decisions[0] != {"action": "accept", "content": content}:
        raise RuntimeError(f"Claude was answered {decisions!r}")


def tool_server_asks(journey: TerminalJourney) -> list[str]:
    pane = journey.launch("terminal", "laptop")
    # A tool server's form, field by field in the order the server wrote.
    journey.open_chat(pane, "filer")
    journey.type(pane, "File the reconnect flake.")
    journey.keys(pane, "Enter")
    journey.read_ask(pane, "linear needs details", "Title · required")
    journey.frame(pane, "form")
    journey.type(pane, FORM["title"])
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, "Team · required", "1. FOX")
    journey.keys(pane, "1")
    journey.wait_terms(pane, "Estimate")
    journey.type(pane, str(FORM["estimate"]))
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, "Submit", f"→ {FORM['title']}", "→ FOX", "→ 3")
    journey.frame(pane, "form-review")
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: at_rest(frame, "Sent the form to linear", "Filed it in Linear."), "the filed reply")
    decisions = control_responses(journey, "filer", "form-answer")
    form_sent(decisions, FORM)
    control = negative_control(form_sent, decisions, {**FORM, "team": "CORE"})

    # A link: Codex asks to let the server's tool run, then the server
    # sends a link, worded from its own message.
    journey.home(pane)
    journey.open_chat(pane, "linker")
    journey.type(pane, "Check the relay's error rate.")
    journey.keys(pane, "Enter")
    journey.read_ask(pane, "Wants to use", "grafana ask")
    journey.keys(pane, "1")
    journey.read_ask(pane, "grafana sent a link", "https://grafana.example.com/login", "Done")
    journey.frame(pane, "link")
    journey.keys(pane, "Down", "Enter")
    journey.wait(pane, lambda frame: at_rest(frame, "Opened grafana's link", "I'll read the dashboards another way."), "the reply")
    results = codex_results(journey, "linker", "link-answers")
    if [result.get("action") for result in results] != ["accept", "accept"] or results[1].get("content") is not None:
        raise RuntimeError(f"Codex was answered {results!r}")
    journey.frame(pane, "link-opened")
    journey.quit_client(pane)
    return [
        "the form asked for title, team and estimate in the server's order and showed them for review",
        f"Claude received the form accepted with {FORM!r}",
        control,
        "Codex's ask to run the server's tool was allowed, then the link showed its message and address",
        "'Done' answered the link: Codex received two accepts, the link's with no content",
        "the client exited 0",
    ]


RUN = "Run the soak."
STEER = "Also log the peak."
WITHDRAWN = "And email me."


def never_sent(chat: dict, text: str) -> None:
    if prompts(chat, text):
        raise RuntimeError(f"the desk received {text!r}")


def queue_and_steer(journey: TerminalJourney) -> list[str]:
    pane = journey.launch("terminal", "laptop")
    journey.open_chat(pane, "worker")
    journey.type(pane, RUN)
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, "Running scripts/soak --for 1h", "ctrl+x interrupt")
    # While it works, prompts queue under the feed.
    journey.type(pane, STEER)
    journey.keys(pane, "Enter")
    journey.type(pane, WITHDRAWN)
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: re.search(rf"{re.escape(WITHDRAWN)}\s+queued", frame) is not None, "two queued")
    journey.frame(pane, "queued")
    # ↑ reaches the newest; withdrawn, it is a draft again, and cleared.
    journey.keys(pane, "Up")
    journey.wait_terms(pane, "[Send Now] [Withdraw]", "backspace withdraw")
    journey.keys(pane, "BSpace")
    journey.wait_terms(pane, f"› {WITHDRAWN}")
    journey.keys(pane, "C-c")
    journey.wait(pane, lambda frame: WITHDRAWN not in frame, "the withdrawn prompt cleared")
    # The other goes into the turn now.
    journey.keys(pane, "Up")
    journey.wait_terms(pane, "[Send Now] [Withdraw]")
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, "sending into this turn")
    journey.request({"OpenGate": {"name": "run-done"}})
    journey.wait(pane, lambda frame: at_rest(frame, "steered", "The run finished."), "the turn's end")
    settled = journey.wait_chat("desk", "worker", lambda chat: chat["phase"] == "IDLE", "settled")
    reflected_once(settled, RUN)
    reflected_once(settled, STEER)
    never_sent(settled, WITHDRAWN)
    control = negative_control(never_sent, settled, STEER)
    journey.frame(pane, "steered")
    journey.quit_client(pane)
    return [
        "two prompts sent while the agent worked queued under the feed",
        "withdrawn, the newest came back to the composer as a draft and was cleared; the desk never received it",
        f"sent now, {STEER!r} went into the running turn and the desk holds it once",
        control,
        "the client exited 0",
    ]


BACKUP = "Check last night's backup."


def send_while_away(journey: TerminalJourney) -> list[str]:
    pane = journey.launch("terminal", "laptop")
    journey.open_chat(pane, "backups")
    journey.request({"Sever": {"a": "cabin", "b": "laptop"}})
    journey.wait_terms(pane, "cabin away")
    journey.type(pane, BACKUP)
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, BACKUP, "sends when cabin is back")
    journey.frame(pane, "away")
    time.sleep(2)
    away = journey.chat("cabin", "backups", "while-away")
    never_sent(away, BACKUP)
    journey.request({"Restore": {"a": "cabin", "b": "laptop"}})
    journey.wait(
        pane,
        lambda frame: at_rest(frame, BACKUP, "Last night's backup finished with no errors.") and "sends when" not in frame,
        "the reply",
    )
    heard = journey.wait_chat("cabin", "backups", lambda chat: chat["phase"] == "IDLE" and len(prompts(chat, BACKUP)) == 1, "sent")
    reflected_once(heard, BACKUP)
    control = negative_control(never_sent, heard, BACKUP)
    journey.frame(pane, "sent")
    journey.quit_client(pane)
    return [
        "with the cabin away, Enter held the prompt to send when the cabin is back, and the cabin never received it",
        f"with the cabin back, the held prompt went by itself and the cabin holds {BACKUP!r} once",
        control,
        "the client exited 0",
    ]


NOT_SENT = "Rotate the backup keys."
MAYBE = "Prune the old snapshots."
BACKUP_DONE = "Last night's backup finished with no errors."


def send_rejected(journey: TerminalJourney) -> list[str]:
    pane = journey.launch("terminal", "laptop")
    journey.open_chat(pane, "backups")
    # The laptop can no longer open a call to the cabin while the cabin's
    # chat stays live: a send fails before it leaves the laptop.
    journey.request({"RefuseCalls": {"a": "laptop", "b": "cabin", "refuse": True}})
    journey.type(pane, NOT_SENT)
    journey.wait_terms(pane, NOT_SENT)
    journey.keys(pane, "Enter")
    # The failed call takes the cabin's route down, so the cabin reads as
    # away at once rather than staying listed while nothing reaches it.
    journey.wait_terms(pane, "cabin away", NOT_SENT, "sends when cabin is back")
    journey.frame(pane, "not-sent")
    time.sleep(2)
    refused = journey.chat("cabin", "backups", "refused")
    never_sent(refused, NOT_SENT)
    # Once calls reach the cabin again it reads as reachable, and the held
    # prompt goes by itself.
    journey.request({"RefuseCalls": {"a": "laptop", "b": "cabin", "refuse": False}})
    journey.wait(pane, lambda frame: at_rest(frame, NOT_SENT, BACKUP_DONE) and "sends when" not in frame, "the reply")
    heard = journey.wait_chat("cabin", "backups", lambda chat: chat["phase"] == "IDLE" and len(prompts(chat, NOT_SENT)) == 1, "sent")
    reflected_once(heard, NOT_SENT)
    control = negative_control(never_sent, heard, NOT_SENT)
    journey.frame(pane, "sent")
    journey.quit_client(pane)
    return [
        "a send that could not leave the laptop was held, to send when the cabin is back, and the cabin read as away",
        "the cabin never received it",
        f"once the cabin was back the held prompt went by itself, and the cabin holds {NOT_SENT!r} once",
        control,
        "the client exited 0",
    ]


def send_may_not_have_arrived(journey: TerminalJourney) -> list[str]:
    pane = journey.launch("terminal", "laptop")
    journey.open_chat(pane, "backups")
    # The cabin's agent takes the prompt in and answers nothing while
    # frozen, so the send waits on the cabin when the link is cut.
    journey.request({"Freeze": {"agent": "backups"}})
    journey.type(pane, MAYBE)
    journey.wait_terms(pane, MAYBE)
    journey.keys(pane, "Enter")
    # Sent: the composer is empty again. The forwarded call reaches the
    # cabin at once and waits there for an answer that does not come; it
    # would give up after 30 seconds, so everything until the prompt reads
    # may not have arrived happens well inside that.
    journey.wait(pane, lambda frame: "› Message backups" in frame, "the prompt on its way", timeout=5)
    sent_at = time.monotonic()
    time.sleep(2)
    journey.request({"Sever": {"a": "cabin", "b": "laptop"}})
    journey.wait_terms(pane, "cabin away", timeout=5)
    time.sleep(1)
    # The link returns with the agent still frozen: catching up finds the
    # prompt neither queued nor in the chat, so it may not have arrived.
    journey.request({"Restore": {"a": "cabin", "b": "laptop"}})
    journey.wait(
        pane,
        lambda frame: re.search(rf"{re.escape(MAYBE)}\s+may not have arrived", frame) is not None and "cabin away" not in frame,
        "may not have arrived",
        timeout=10,
    )
    if time.monotonic() - sent_at >= 25:
        raise RuntimeError("the prompt read may not have arrived only near the answer's time limit, not from the cut")
    journey.frame(pane, "may-not-have-arrived")
    # The agent goes on with what it took in, and the prompt settles with
    # no resend: the cabin had it, once.
    journey.request({"Thaw": {"agent": "backups"}})
    journey.wait(
        pane,
        lambda frame: at_rest(frame, MAYBE, BACKUP_DONE) and "may not have arrived" not in frame,
        "the prompt settled and answered",
    )
    heard = journey.wait_chat("cabin", "backups", lambda chat: chat["phase"] == "IDLE" and len(prompts(chat, MAYBE)) == 1, "settled")
    reflected_once(heard, MAYBE)
    control = negative_control(never_sent, heard, MAYBE)
    journey.frame(pane, "settled")
    journey.quit_client(pane)
    return [
        "with the link cut while the cabin held the prompt unanswered, the send came back uncertain, not as not sent",
        "with the link back and the cabin's agent still frozen, the prompt read may not have arrived",
        f"once the agent went on it settled without a resend, and the cabin holds {MAYBE!r} once",
        control,
        "the client exited 0",
    ]


def composer_limits(journey: TerminalJourney) -> list[str]:
    pane = journey.launch("terminal", "laptop")
    # At a usage limit the composer says so, and sending still works.
    journey.open_chat(pane, "limited")
    journey.wait_terms(pane, "5-hour limit reached · resets")
    journey.frame(pane, "limit")
    journey.type(pane, "Try again.")
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: at_rest(frame, "Done after the limit."), "the reply past the limit")
    reflected_once(journey.wait_chat("desk", "limited", lambda chat: chat["phase"] == "IDLE" and len(prompts(chat, "Try again.")) == 1, "limited-sent"), "Try again.")

    # A refused credential takes the composer's box: Claude must be signed
    # in again where it runs.
    journey.home(pane)
    journey.open_chat(pane, "expired")
    journey.type(pane, "Check the links.")
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, "Claude needs you to sign in", "Run claude and sign in with /login on desk.")
    journey.frame(pane, "sign-in")
    refused = journey.wait_chat("desk", "expired", lambda chat: chat["phase"] == "IDLE", "refused")
    reflected_once(refused, "Check the links.")
    journey.quit_client(pane)
    return [
        "at its usage limit the composer named the window and when it resets, and a prompt still went and was answered",
        "refused its credential, Claude's chat put sign-in in the composer's box, naming the host to sign in on",
        "the desk holds each prompt once",
        "the client exited 0",
    ]


def new_agent_terminal(journey: TerminalJourney) -> list[str]:
    journey.configure("laptop", "ui:\n  chat_in: terminal")
    before = {agent["id"] for agent in journey.inventory("laptop", "inventory-before")}
    pane = journey.launch("terminal", "laptop")
    journey.wait(pane, at_home, "home")
    # On this machine, Start hands the terminal to the new agent.
    journey.keys(pane, "n")
    journey.wait_terms(pane, "New Agent", "named automatically", "[Start]")
    journey.type(pane, "release-notes")
    journey.wait_terms(pane, "Name      release-notes")
    journey.frame(pane, "form")
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, "enter start")
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, "Claude Code (scripted)")
    journey.type(pane, "Write the notes.")
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, "Ready to write the release notes.")
    journey.keys(pane, "C-a", "s")
    journey.wait(pane, lambda frame: at_home(frame) and agent_row(frame, "release-notes") is not None, "home with release-notes")
    made = journey.wait_inventory("laptop", lambda agents: any(a["name"] == "release-notes" for a in agents), "created-here")
    (local,) = [agent for agent in made if agent["name"] == "release-notes"]
    if local["id"] in before or local["host_id"] != journey.host_id("laptop"):
        raise RuntimeError(f"the laptop lists {local!r}")
    reflected_once(journey.wait_chat("laptop", local["id"], lambda chat: len(prompts(chat, "Write the notes.")) == 1, "typed-there"), "Write the notes.")

    # On another machine there is no terminal to hand over: its chat opens.
    journey.keys(pane, "n")
    journey.wait_terms(pane, "New Agent")
    journey.type(pane, "desk-notes")
    journey.keys(pane, "Down", "Down", "Down")
    journey.wait_terms(pane, "←→ choose")
    # The cabin's providers are signed out: asked what it offers, the
    # cabin says so, and the form names it on each agent.
    journey.keys(pane, "Left", "Left")
    journey.wait_terms(pane, "Claude · not signed in", "Codex · not signed in")
    journey.frame(pane, "signed-out-form")
    journey.keys(pane, "Right")
    journey.wait(pane, lambda frame: "not signed in" not in frame, "the desk's signed-in agents")
    journey.frame(pane, "remote-form")
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: chat_of(frame) == "desk-notes" and "its terminal is on another machine" in frame, "desk-notes' chat")
    journey.frame(pane, "remote-chat")
    remote = journey.wait_inventory("desk", lambda agents: any(a["name"] == "desk-notes" for a in agents), "created-there")
    (there,) = [agent for agent in remote if agent["name"] == "desk-notes"]
    if there["host_id"] != journey.host_id("desk"):
        raise RuntimeError(f"desk-notes runs on {there['host_id']}, not the desk")
    journey.quit_client(pane)
    return [
        "for someone who chats in each agent's own terminal, n opened the New Agent form",
        "Start on this machine handed the terminal to the new Claude; what was typed there reached it once; leader s came back home",
        "on the cabin, whose providers are signed out, the form said so on Claude and Codex; on the desk it did not",
        "Start on the desk created the agent there and opened its chat, saying its terminal is on another machine",
        "the client exited 0",
    ]


def worktrees(repository: Path) -> list[dict]:
    """`git worktree list --porcelain` of `repository`, one dict per tree."""
    listed = subprocess.run(
        ["git", "-C", str(repository), "worktree", "list", "--porcelain"],
        check=True, text=True, stdout=subprocess.PIPE, timeout=30,
    ).stdout
    trees = []
    for block in listed.strip().split("\n\n"):
        fields = dict(line.split(" ", 1) for line in block.splitlines() if " " in line)
        trees.append(fields)
    return trees


def pick(journey: TerminalJourney, pane: str, label: str) -> None:
    """Picks `label` in an open flyover by the number it is listed under."""
    numbered = re.search(rf"(\d)\. {re.escape(label)}\b", journey.capture(pane))
    if numbered is None:
        raise RuntimeError(f"no flyover lists {label!r}")
    journey.type(pane, numbered.group(1))


def new_agent_worktree(journey: TerminalJourney) -> list[str]:
    name = "site-redesign"
    pane = journey.launch("terminal", "laptop")
    journey.wait(pane, at_home, "home")
    journey.keys(pane, "n")
    journey.wait_terms(pane, "What should the new agent work on?")
    # On the desk, which offers a model the laptop does not.
    journey.keys(pane, "C-s", "h")
    journey.wait_terms(pane, "╭─ Host", ". desk")
    pick(journey, pane, "desk")
    journey.wait(pane, lambda frame: "│ desk ─╯" in frame and "╭─ Host" not in frame, "the desk chosen")
    journey.keys(pane, "C-s", "m")
    journey.wait_terms(pane, "╭─ Model", ". Fable 5")
    journey.frame(pane, "desk-models")
    pick(journey, pane, "Fable 5")
    # Another model starts at its own default effort; Claude names none, so
    # the effort is left to the model.
    journey.wait(pane, lambda frame: "Claude · Fable 5 · " in frame and "╭─ Model" not in frame, "Fable 5 chosen")
    # In the desk's repository, which its agent already works in.
    journey.keys(pane, "C-s", "d")
    journey.wait_terms(pane, "╭─ Folder")
    journey.type(pane, "repositories/site")
    journey.wait_terms(pane, "desk/repositories/site")
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: "╭─ Folder" not in frame and "repositories/site" in frame, "the repository chosen")
    journey.keys(pane, "C-s", "n")
    journey.wait_terms(pane, "╭─ Name")
    journey.type(pane, name)
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: f"─ {name} │" in frame, "the name given")
    journey.keys(pane, "C-s", "w")
    journey.wait_terms(pane, "· new worktree │")
    journey.type(pane, "Redesign the landing page.")
    journey.wait_terms(pane, "› Redesign the landing page.")
    journey.frame(pane, "worktree-set")
    journey.keys(pane, "Enter")
    # The branch reaches the header with the turn's end, which can land just
    # after the reply does.
    journey.wait(
        pane,
        lambda frame: chat_of(frame) == name
        and at_rest(frame, "Working in the new worktree.")
        and f"{name} │ {name} " in frame,
        "the new agent's reply, on its branch",
    )
    journey.frame(pane, "worktree-chat")

    # The desk made it there, on the chosen model, in a worktree of its
    # repository on a branch named after the agent.
    made = journey.wait_inventory(
        "desk", lambda agents: any(a["name"] == name and a["branch"] == name for a in agents), "created-in-worktree"
    )
    (agent,) = [a for a in made if a["name"] == name]
    if agent["host_id"] != journey.host_id("desk"):
        raise RuntimeError(f"{name} runs on {agent['host_id']}, not the desk")
    chat = journey.wait_chat("desk", agent["id"], lambda chat: chat["phase"] == "IDLE", "answered")
    reflected_once(chat, "Redesign the landing page.")
    if chat["model"] != "claude-fable-5":
        raise RuntimeError(f"{name} runs {chat['model']!r}, not the desk's Fable 5")
    repository = Path(journey.ready["root"]) / "desk" / "repositories" / "site"
    trees = worktrees(repository)
    journey.observations["desk-worktrees"] = trees
    folder = os.path.realpath(agent["cwd"])
    listed = [tree for tree in trees if os.path.realpath(tree["worktree"]) == folder]
    if len(listed) != 1 or listed[0].get("branch") != f"refs/heads/{name}":
        raise RuntimeError(f"the desk's repository lists {trees!r}, not {folder} on {name}")
    if folder == os.path.realpath(repository) or Path(folder).name != name:
        raise RuntimeError(f"{name} works in {folder}, not a worktree of its own")

    # The same name again: names are unique on a host, so the second agent
    # gets the next free one and a worktree and branch of its own.
    journey.home(pane)
    journey.keys(pane, "n")
    journey.wait_terms(pane, "What should the new agent work on?", "· new worktree │")
    journey.keys(pane, "C-s", "n")
    journey.wait_terms(pane, "╭─ Name")
    journey.type(pane, name)
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: f"─ {name} │" in frame, "the name given again")
    journey.type(pane, "Redesign it again.")
    journey.wait_terms(pane, "› Redesign it again.")
    journey.keys(pane, "Enter")
    second = f"{name}-2"
    journey.wait(
        pane,
        lambda frame: chat_of(frame) == second
        and at_rest(frame, "Redesign it again.", "Working in the new worktree.")
        and f"{second} │ {second} " in frame,
        "the second agent's reply, on its branch",
    )
    journey.frame(pane, "name-taken")
    again = journey.wait_inventory("desk", lambda agents: any(a["name"] == second for a in agents), "after-second")
    if [a["name"] for a in again].count(name) != 1:
        raise RuntimeError(f"the desk lists {again!r}")
    (other,) = [a for a in again if a["name"] == second]
    other_folder = os.path.realpath(other["cwd"])
    trees = worktrees(repository)
    listed = [tree for tree in trees if os.path.realpath(tree["worktree"]) == other_folder]
    if len(listed) != 1 or listed[0].get("branch") != f"refs/heads/{second}" or other_folder == folder:
        raise RuntimeError(f"the desk's repository lists {trees!r}, not {other_folder} on {second}")
    journey.home(pane)
    journey.quit_client(pane)
    return [
        "the new agent was set to run on the desk, on Fable 5 from the desk's own catalogue, in the desk's repository, in a new worktree",
        "it started on the desk on claude-fable-5 and answered; its prompt reached the desk once",
        f"the desk's repository lists the agent's folder as a worktree on branch {name}, named after the agent",
        f"started again as {name}, the desk named the second agent {name}-2, in a worktree of its own on branch {name}-2",
        "the client exited 0",
    ]



def report_a_problem(journey: TerminalJourney) -> list[str]:
    pane = journey.launch("terminal", "laptop")
    journey.open_chat(pane, "limited")
    journey.wait_terms(pane, "The FAQ is rewritten.")
    before = journey.capture(pane)
    journey.keys(pane, "C-a", "b")
    journey.wait_terms(pane, "Report a problem", "What went wrong?", "enter save")
    # A drag over the reply, as the terminal reports a mouse: press, move,
    # let go, in cells counted from one.
    row = next(i for i, line in enumerate(before.splitlines()) if "The FAQ is rewritten." in line)
    col = before.splitlines()[row].index("The FAQ")
    press = f"\x1b[<0;{col + 1};{row + 1}M"
    drag = f"\x1b[<32;{col + 21};{row + 1}M"
    release = f"\x1b[<0;{col + 21};{row + 1}m"
    for sequence in (press, drag, release):
        journey.type(pane, sequence)
        time.sleep(0.15)
    journey.wait_terms(pane, "Mark 1: what is wrong here?", "1 mark")
    journey.type(pane, "This says rewritten but nothing changed.")
    journey.keys(pane, "Enter")
    journey.type(pane, "The reply claims work it did not do.")
    journey.wait_terms(pane, "▎ The reply claims work it did not do.")
    journey.frame(pane, "report")
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, "report saved in")
    reports = Path(journey.ready["root"]) / "laptop" / "data" / "reports"
    (bundle,) = [path for path in reports.iterdir() if path.name.startswith("report-")]
    # Kept with the run's evidence, to read.
    shutil.copytree(bundle, journey.output / "bundle")
    header = json.loads((bundle / "report.json").read_text())
    expected_mark = {"x": col, "y": row, "width": 21, "height": 1, "note": "This says rewritten but nothing changed."}
    if header["marks"] != [expected_mark] or header["note"] != "The reply claims work it did not do.":
        raise RuntimeError(f"report.json holds {header!r}")
    if header["viewport"] != [110, 34] or header["parts"]["frame"] != "present" or header["parts"]["dump"] != "present":
        raise RuntimeError(f"report.json declares {header!r}")
    for part in ("frame_png", "trace", "log"):
        if not header["parts"][part].get("absent", {}).get("reason"):
            raise RuntimeError(f"{part} is not declared absent with a reason: {header['parts']!r}")
    frame = (bundle / "frame.txt").read_text()
    if "The FAQ is rewritten." not in frame or "limited │" not in frame or "Report a problem" in frame:
        raise RuntimeError(f"frame.txt is not the frozen chat:\n{frame}")
    cells = json.loads((bundle / "frame.json").read_text())
    runs = [run for row in cells["rows"] for run in row]
    if (cells["width"], cells["height"]) != (110, 34) or len(cells["rows"]) != 34:
        raise RuntimeError(f"frame.json is not the 110x34 screen: {cells['width']}x{cells['height']}")
    if not any("The FAQ is rewritten." in run["text"] for run in runs):
        raise RuntimeError("frame.json does not hold the reply")
    if not any(run["fg"] != "default" for run in runs) or not any("bold" in run.get("styles", []) for run in runs):
        raise RuntimeError(f"frame.json carries no colours or styles: {runs[:5]!r}")
    dump = bundle / "dump"
    if not (dump / "manifest.json").is_file() or not any(dump.rglob("row.pb")):
        raise RuntimeError(f"the dump is not whole: {sorted(str(p.relative_to(dump)) for p in dump.rglob('*'))}")
    client_parts = [p for p in dump.rglob("*") if p.is_file() and "client" in str(p.relative_to(dump))]
    if not client_parts:
        raise RuntimeError(f"the dump has no client parts: {sorted(str(p.relative_to(dump)) for p in dump.rglob('*'))}")
    if os.stat(bundle).st_mode & 0o077 or os.stat(bundle / "report.json").st_mode & 0o077:
        raise RuntimeError("the report is readable by others")
    journey.quit_client(pane)
    return [
        "leader b froze the chat under the report's panel",
        "a mouse drag marked the reply, and its note and the overall note were written",
        f"the bundle holds report.json with the mark in cells ({expected_mark['x']},{expected_mark['y']} 21×1) and both notes",
        "frame.txt is the chat as it was at the key, without the report's panel",
        "frame.json holds the same 110x34 cells with their colours and styles",
        "the dump is whole: its manifest, the agents' rows and this client's parts",
        "the parts not captured are declared absent with their reasons, and only this user can read the report",
        "the client exited 0",
    ]


STORIES = {
    "conversation-decision-claude-pty": lambda j: conversation_decision(j, "decision-pty", False),
    "conversation-decision-claude-sdk": lambda j: conversation_decision(j, "decision-sdk", True),
    "conversation-decision-codex": lambda j: conversation_decision(j, "decision-codex", True),
    "leave-and-recover": leave_and_recover,
    "manage-agent": manage_agent,
    "reach-host": reach_host,
    "attachment-or-review": attachment_or_review,
    "keep-authority": keep_authority,
    "decide-plan-claude-sdk": lambda j: decide_plan(j, "planner", True),
    "decide-plan-claude-pty": lambda j: decide_plan(j, "planner-pty", False),
    "decide-plan-codex": decide_plan_codex,
    "answer-questions": answer_questions,
    "tool-server-asks": tool_server_asks,
    "queue-and-steer": queue_and_steer,
    "send-while-away": send_while_away,
    "send-rejected": send_rejected,
    "send-may-not-have-arrived": send_may_not_have_arrived,
    "composer-limits": composer_limits,
    "new-agent-terminal": new_agent_terminal,
    "new-agent-worktree": new_agent_worktree,
    "report-a-problem": report_a_problem,
}


def main() -> int:
    if len(sys.argv) != 2 or sys.argv[1] not in STORIES:
        print(f"usage: terminal-journey.py {{{','.join(STORIES)}}}", file=sys.stderr)
        return 2
    name = sys.argv[1]
    declared = story(name)
    journey = TerminalJourney(declared, ROOT / declared["topology"])
    try:
        assertions = STORIES[name](journey)
        journey.finish(assertions)
        print(f"PASS {name}")
        for assertion in assertions:
            print(f"- {assertion}")
        return 0
    except BaseException as error:
        journey.fail(error)
        print(f"FAIL {name}: {error}", file=sys.stderr)
        return 1
    finally:
        journey.close()


if __name__ == "__main__":
    sys.exit(main())
