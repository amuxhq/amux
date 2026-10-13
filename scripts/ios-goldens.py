#!/usr/bin/env python3
"""Photograph the phone's whole screens through the served door and compare
each with its golden, in light and in dark.

`scripts/ios-goldens.py [--only ID]... [--update] [--perturb TOKEN] [--review DIR]`

The screens are the `screens` half of apps/apple/Goldens/manifest.json. Each
is reached the way a person reaches it, on the phone driver: the manifest's
topology is served with `testnet serve`, the debug app is installed fresh on
the leased simulator, the desk is paired by the code it printed, and the app
is tapped to each screen while the served net makes the desk's agents do
what the screen needs. A screen is judged by the display's pixels and the
door's element geometry against apps/apple/Goldens/<screen>.<appearance>.png
and <screen>.elements.txt (both appearances share one geometry). A screen
with frames (origin-rewind's before and after) has a golden per frame.

Every screen is drawn with the app's reduce-transparency flag on, turned on
through the door before the first screen, so each frosted surface is flat:
a raised fill with a hairline rim, what a person who turned Reduce
Transparency on sees. Liquid Glass and material are finished by the render
server on its own schedule, out of the app's sight, so a photograph of them
is not a fixed picture a tight comparison can hold.

`--only ID` photographs just the screens named; the way through every other
screen is still taken, so each is reached in the same state. Tab pages a
pushed page covers are left out of the geometry. `--update` rewrites every
golden that differs and leaves the rest alone. `--perturb TOKEN` moves one design colour
token (or, named `needs-you-dot`, takes that one small mark away) before
anything is drawn and fails unless every photograph comes back different
with a difference image: what proves the comparison would notice.
`--review DIR` takes the same way with the flag off and writes every screen,
glass and all, to DIR as <screen>.<appearance>.png for a person to look at;
nothing is compared and no golden is read or written.

The components half of the manifest is photographed in-process by
`just ios component-snapshots`; this script only reads its screens.
"""

from __future__ import annotations

from argparse import ArgumentParser
from pathlib import Path
import base64
import json
import re
import signal
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
import ios_simulators  # noqa: E402
from journeys.phone import ROOT, DoorError, PhoneJourney  # noqa: E402

MANIFEST = ROOT / "apps/apple/Goldens/manifest.json"
GOLDENS = ROOT / "apps/apple/Goldens"
OUTPUT = ROOT / "target/ios/goldens"
PERTURBED = ROOT / "target/ios/goldens-perturb"
SIMULATOR = "golden"
APPEARANCES = ("light", "dark")
# Longer than the render server has been seen to take to finish glass after
# it first draws (under 2 s), for the review captures.
GLASS_FINISHES = 2.5
# A turn's duration and a pairing code's fingerprint and countdown are made
# fresh by every run.
TURN = ("chat.row.turn-end",)
PAIRING = ("pair-confirm.fingerprint", "pair-confirm.expiry")


def labelled(drawn: dict, text: str) -> bool:
    return any(text in (element.get("label") or "") for element in drawn.values())


class Goldens:
    def __init__(self, journey: PhoneJourney, manifest: dict, wanted: set[str], review: Path | None = None):
        self.journey = journey
        self.screens = {screen["id"]: screen for screen in manifest["screens"]}
        self.wanted = wanted
        self.review = review
        self.outcomes: list[tuple[str, str | None]] = []

    def agent(self, name: str) -> str:
        return next(item["id"] for item in self.journey.ready["agents"] if item["name"] == name)

    def photograph(self, screen: str, frame: str | None = None, volatile: tuple[str, ...] = ()) -> None:
        """The screen as it is now, in every appearance, against its golden."""
        if screen not in self.wanted:
            return
        declared = [item["name"] for item in self.screens[screen].get("frames", [])]
        if (frame is None) != (not declared) or (frame is not None and frame not in declared):
            raise RuntimeError(f"the manifest does not declare {screen} with frame {frame!r}")
        label = screen if frame is None else f"{screen}-{frame}"
        for appearance in APPEARANCES:
            self.journey.appearance(appearance)
            name = f"{label}.{appearance}"
            if self.review is not None:
                self.journey.app({"kind": "settle"})
                time.sleep(GLASS_FINISHES)
                self.journey.steady_display(self.review / f"{name}.png")
                self.outcomes.append((name, None))
                print(f"wrote {name}", flush=True)
                continue
            differs = self.journey.compare(name, volatile, geometry_label=label)
            self.outcomes.append((name, differs))
            print(f"{'ok' if differs is None else 'DIFFERS'} {name}", flush=True)
        self.journey.appearance("light")

    # --- the way through the screens ---------------------------------------

    def prepare_the_desk(self) -> None:
        """The desk's agents act one turn at a time, so the fleet's order
        (most recently active first, among agents needing the same
        attention) is the same on every run."""
        journey = self.journey
        for agent, prompt, reply in (
            ("planner", "Why do the pairing errors disagree?", "share one string"),
            ("keeper", "Remember this recovery turn.", "safely stored"),
        ):
            journey.request({"Send": {"agent": agent, "text": prompt}})
            journey.wait_chat(
                "desk", agent,
                lambda chat, reply=reply: chat["phase"] == "IDLE" and any(reply in i["text"] for i in chat["items"]),
                f"{agent}-answered",
            )
        # The asker was started with its prompt, so it is asking already and
        # the fleet names what it was asked to do.
        journey.wait_chat("desk", "asker", lambda chat: chat["phase"] == "NEEDS_YOU", "asker-asking")

    def pairing(self) -> None:
        """The six digits the desk printed, typed on the keypad, reach the
        desk's own name and key before anything is trusted."""
        journey = self.journey
        desk = journey.host_id("desk")
        journey.tap("tab.hosts")
        journey.wait_for(f"hosts.pair.{desk}")
        process = journey.amux("desk", "pair")
        pin = printed_pin(process)
        journey.tap(f"hosts.pair.{desk}")
        journey.wait_for("pin.key.0")
        for digit in pin:
            journey.tap(f"pin.key.{digit}")
        journey.wait(
            lambda drawn: "pair-confirm.trust" in drawn and drawn.get("pair-confirm.name", {}).get("value") == "desk",
            "the desk's confirmation",
        )
        self.photograph("pairing", volatile=PAIRING)
        journey.tap("pair-confirm.trust")
        journey.wait_for("pair-confirm.done")
        journey.tap("pair-confirm.done")
        journey.wait(lambda drawn: f"hosts.row.{desk}" in drawn and "pair-confirm.done" not in drawn, "the desk paired")
        process.wait(timeout=30)

    def hosts(self) -> None:
        desk = self.journey.host_id("desk")
        self.journey.tap("tab.hosts")
        self.journey.wait(lambda drawn: labelled(drawn, "desk") and f"hosts.row.{desk}" in drawn, "the hosts")
        self.photograph("hosts")

    def fleet(self) -> None:
        rows = [f"home.row.{self.agent(name)}" for name in ("planner", "keeper", "asker", "dialog")]
        self.journey.tap("tab.agents")
        self.journey.wait(lambda drawn: all(row in drawn for row in rows), "every agent in the fleet")
        self.photograph("fleet")

    def open(self, agent: str, ready, description: str) -> dict:
        journey = self.journey
        journey.tap("tab.agents")
        journey.wait_for(f"home.row.{self.agent(agent)}")
        # A row redrawn by an update landing as it is tapped (a fleet row
        # takes every change to its agent) drops the tap or refuses it; a
        # person taps again.
        for attempt in range(3):
            try:
                journey.tap(f"home.row.{self.agent(agent)}")
                journey.wait(lambda drawn: "chat.back" in drawn, "the chat opened", timeout=5)
                break
            except (RuntimeError, DoorError):
                if attempt == 2:
                    raise
                journey.app({"kind": "settle"})
        return journey.wait(lambda drawn: "chat.back" in drawn and ready(drawn), description)

    def back(self) -> None:
        self.journey.tap("chat.back")
        self.journey.wait(lambda drawn: "chat.back" not in drawn, "the fleet")
        # A row tapped while the chat is still leaving does not open.
        self.journey.app({"kind": "settle"})

    def chat_turn(self) -> None:
        self.open("planner", lambda drawn: labelled(drawn, "share one string") and TURN[0] in drawn, "the answered chat")
        self.photograph("chat-turn", volatile=TURN)
        self.back()

    def question(self) -> None:
        self.open(
            "asker",
            lambda drawn: "ask" in drawn and labelled(drawn, "Only what users will notice"),
            "the question card",
        )
        self.photograph("claude-sdk-ask-question")
        self.back()

    def escape(self) -> None:
        """Terminal Claude's tool-server dialog is an ask the phone cannot
        answer; its card offers the way out."""
        journey = self.journey
        journey.request({"Send": {"agent": "dialog", "text": "File the relay bug."}})
        journey.wait_chat("desk", "dialog", lambda chat: chat["phase"] == "NEEDS_YOU", "dialog-open")
        self.open("dialog", lambda drawn: "ask" in drawn and labelled(drawn, "github"), "the unanswerable card")
        self.photograph("ask-escape")
        self.back()

    def composer_chips(self) -> None:
        """A draft holding two files at an accessibility text size: each
        name wraps rather than losing its extension, its size sits under it,
        and each remove mark is a thumb's target. The accessibility audit
        takes this screen too, and holds those marks to 44 points."""
        journey = self.journey
        self.open("planner", lambda drawn: labelled(drawn, "share one string"), "the answered chat")
        journey.app({"kind": "dynamicType", "size": "accessibility3"})
        for name, text in (
            ("pairing-errors-before-and-after.md", "# Pairing errors\n"),
            ("relay.log", "relay up\n"),
        ):
            journey.app({
                "kind": "attach", "agent": "planner", "attachment": "file", "name": name,
                "mime": "text/plain", "base64": base64.b64encode(text.encode()).decode(),
            })
        journey.wait(
            lambda drawn: "chat.attachment#2" in drawn and "chat.attachment.remove#2" in drawn,
            "both chips in the draft",
        )
        self.photograph("composer-chips", volatile=TURN)
        # The draft goes back as it was, and the text size with it, so the
        # screens after this one are drawn as they always were.
        for _ in range(2):
            journey.tap("chat.attachment.remove")
            journey.app({"kind": "settle"})
        journey.wait(lambda drawn: "chat.attachment" not in drawn, "the chips removed")
        journey.app({"kind": "dynamicType", "size": "large"})
        self.back()

    def large_text(self) -> None:
        """Every screen a person reads most, at an accessibility text size:
        nothing overprints, nothing is cut mid-word, and every control is
        on the screen. The answered chat is opened before the text grows,
        since its row is below the fold at that size. The text size goes
        back afterwards, so the screens after this one are drawn as they
        always were."""
        journey = self.journey
        desk = journey.host_id("desk")
        self.open("planner", lambda drawn: labelled(drawn, "share one string") and TURN[0] in drawn, "the answered chat")
        journey.app({"kind": "dynamicType", "size": "accessibility3"})
        journey.app({"kind": "settle"})
        self.photograph("large-text", "chat", volatile=TURN)
        self.back()
        journey.wait(lambda drawn: f"home.row.{self.agent('asker')}" in drawn, "the fleet at a large text size")
        journey.app({"kind": "settle"})
        self.photograph("large-text", "fleet")
        journey.tap("tab.hosts")
        journey.wait(lambda drawn: f"hosts.row.{desk}" in drawn, "the hosts at a large text size")
        journey.app({"kind": "settle"})
        self.photograph("large-text", "hosts")
        journey.tap("tab.you")
        journey.wait(lambda drawn: "you.appearance" in drawn, "the You tab at a large text size")
        journey.app({"kind": "settle"})
        self.photograph("large-text", "you")
        journey.tap("tab.agents")
        journey.tap("home.newAgent")
        journey.wait(
            lambda drawn: drawn.get(f"new-agent.host.{desk}", {}).get("value") == "chosen"
            and drawn.get("new-agent.directory", {}).get("value"),
            "New Agent at a large text size",
        )
        journey.app({"kind": "settle"})
        self.photograph("large-text", "new-agent")
        journey.tap("new-agent.back")
        journey.wait(lambda drawn: "new-agent" not in drawn, "the fleet again")
        journey.app({"kind": "settle"})
        self.open(
            "asker",
            lambda drawn: "ask" in drawn and labelled(drawn, "Only what users will notice"),
            "the question card at a large text size",
        )
        journey.app({"kind": "settle"})
        self.photograph("large-text", "question")
        self.back()
        journey.app({"kind": "dynamicType", "size": "large"})

    def origin_rewind(self) -> None:
        """The desk checkpoints, moves on a turn the phone sees, then loses
        power back to the checkpoint: the phone's replica is Reset and the
        rebuilt chat swaps in under the rows on screen."""
        journey = self.journey
        journey.request({"Checkpoint": {"host": "desk"}})
        journey.request({"Send": {"agent": "keeper", "text": "Carry on."}})
        journey.wait_chat(
            "desk", "keeper",
            lambda chat: chat["phase"] == "IDLE" and any("after the checkpoint" in i["text"] for i in chat["items"]),
            "keeper-moved-on",
        )
        self.open(
            "keeper", lambda drawn: labelled(drawn, "after the checkpoint") and TURN[0] in drawn, "the chat before"
        )
        self.photograph("origin-rewind", "before", volatile=TURN)
        journey.request({"Rewind": {"host": "desk", "cuts": []}})
        deadline = time.monotonic() + 60
        while True:
            drawn = journey.elements()
            if not (labelled(drawn, "safely stored") and labelled(drawn, "after the checkpoint")):
                raise RuntimeError("rows left the screen during the Reset")
            if labelled(drawn, "Exited"):
                break
            if time.monotonic() > deadline:
                raise RuntimeError("the rebuilt chat never swapped in")
            time.sleep(0.05)
        journey.actions.append("the rows stayed on screen until the rebuilt chat swapped in")
        self.photograph("origin-rewind", "after", volatile=TURN)
        self.back()


def printed_pin(process) -> str:
    """The six digits a machine's `amux pair` printed."""
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        line = process.stdout.readline()
        if found := re.search(r"Pairing PIN: (\d{3}) (\d{3})", line):
            return found.group(1) + found.group(2)
        if not line and process.poll() is not None:
            break
    raise RuntimeError("the machine printed no pairing PIN")


# Every screen the manifest may declare, in the order the way through them
# passes it. origin-rewind comes last: it takes the desk's power.
WAY = {
    "pairing": None,
    "hosts": Goldens.hosts,
    "fleet": Goldens.fleet,
    "chat-turn": Goldens.chat_turn,
    "claude-sdk-ask-question": Goldens.question,
    "ask-escape": Goldens.escape,
    "composer-chips": Goldens.composer_chips,
    "large-text": Goldens.large_text,
    "origin-rewind": Goldens.origin_rewind,
}


def main() -> int:
    parser = ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--only", action="append", default=[], metavar="ID")
    parser.add_argument("--update", action="store_true")
    parser.add_argument("--perturb", metavar="TOKEN")
    parser.add_argument("--review", metavar="DIR", type=Path)
    options = parser.parse_args()
    manifest = json.loads(MANIFEST.read_text())
    declared = [screen["id"] for screen in manifest["screens"]]
    if sorted(declared) != sorted(WAY):
        raise SystemExit(f"{MANIFEST} declares {declared}; this driver reaches {list(WAY)}")
    unknown = [name for name in options.only if name not in declared]
    if unknown:
        raise SystemExit(f"{MANIFEST} has no screen named {', '.join(unknown)}")
    if options.perturb and options.update:
        raise SystemExit("--perturb moves a token on purpose; its photographs are never goldens")
    if options.review and (options.perturb or options.update):
        raise SystemExit("--review writes pictures nothing compares; it takes neither --update nor --perturb")
    wanted = set(options.only or declared)

    udid = ios_simulators.ready(SIMULATOR)
    began = time.monotonic()
    output = PERTURBED / options.perturb if options.perturb else OUTPUT
    if options.review:
        output = OUTPUT.with_name("goldens-review")
        options.review = options.review.resolve()
        options.review.mkdir(parents=True, exist_ok=True)
    journey = PhoneJourney(
        {"id": "goldens"}, ROOT / manifest["topology"], udid, output=output, goldens=GOLDENS
    )
    journey.update = options.update
    # Review captures are of the glass itself.
    journey.flat = not options.review
    goldens = Goldens(journey, manifest, wanted, options.review)
    try:
        journey.launch()
        if options.perturb:
            journey.app({"kind": "perturb", "token": options.perturb})
        goldens.prepare_the_desk()
        goldens.pairing()
        # The whole way every time, photographing only what was asked: a
        # screen's element geometry includes the pages under it, which hold
        # whatever the earlier steps left there.
        for reach in WAY.values():
            if reach is not None:
                reach(goldens)
        journey.finish([name for name, _ in goldens.outcomes])
    except BaseException as error:
        journey.fail(error)
        raise
    finally:
        journey.close()
    if options.review:
        print(f"{len(goldens.outcomes)} review captures with glass on in {time.monotonic() - began:.1f}s under {options.review}")
        return 0
    print(f"{len(goldens.outcomes)} photographs in {time.monotonic() - began:.1f}s; captures under {output}")
    if options.perturb:
        missed = [
            name for name, differs in goldens.outcomes
            if differs is None or not (output / "diff" / name / "diff.png").is_file()
        ]
        if missed or not goldens.outcomes:
            print(f"the {options.perturb} token was moved and nothing noticed on {', '.join(missed)}", file=sys.stderr)
            return 1
        print(f"every photograph differs with {options.perturb} moved; difference images under {output / 'diff'}")
        return 0
    failed = [(name, differs) for name, differs in goldens.outcomes if differs is not None]
    for name, differs in failed:
        print(f"FAILED {name}: {differs}", file=sys.stderr)
    return 1 if failed else 0


if __name__ == "__main__":
    # Let the recipe's timeout unwind the served net and the app.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    sys.exit(main())
