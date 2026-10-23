"""The phone journey driver.

Starts a served topology with `testnet serve`, installs the debug app fresh on
the leased simulator and launches it with its door, the net's discovery
scope, a loopback direct-link listener and, when the net has one, the served
relay. It then acts only as a person would, through door verbs a person's
action maps to: tap, type, paste, pair by the link or code the machine
printed, and appearance. It judges by what the hosts recorded (the served
net's Chat, Inventory and ProviderInput verbs) and by compared screens: the
simulator's display PNG against journeys/goldens/phone/<story>/<label>.png
with `xtask golden diff` under the pinned simulator's system-chrome masks,
and the door's element geometry against <label>.elements.txt. Stories supply
the acts and the assertions; UPDATE_JOURNEY_GOLDENS=1 rewrites the goldens
that differ.
"""

from __future__ import annotations

import difflib
import fcntl
import json
import os
import re
from pathlib import Path
import shutil
import socket
import subprocess
import time
from typing import Callable

from journeys.terminal import AGE, SCRATCH, DoorError, _read_readiness, door

ROOT = Path(__file__).resolve().parents[2]
TESTNET = ROOT / "target/debug/testnet"
AMUX = ROOT / "target/debug/amux"
APP = ROOT / "target/ios/DerivedData/Build/Products/Debug-iphonesimulator/Amux.app"
BUNDLE_ID = "sh.amux.app"
MANIFEST = ROOT / "journeys/manifest.json"
OUTPUT = ROOT / "target/journeys/phone"
GOLDENS = ROOT / "journeys/goldens/phone"
SCRATCH_DIR = Path("/tmp/amux-phone-journey")
SCRATCH_LOCK = Path("/tmp/amux-phone-journey.lock")
# The pinned simulator whose system-chrome masks every comparison uses.
SIMULATOR = "golden"
# The golden simulator draws three pixels to the point.
SCALE = 3
# A surface a view reports under a name ending here holds text that moves with
# the run (an age, a scratch path); its rectangle is masked in the pixel diff
# and its words are left out of the geometry.
VOLATILE = ".volatile"
# This phone's own name and key, which change with the leased simulator and
# every fresh install.
OWN_IDENTITY = ("you.name", "you.fingerprint")
UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")


# A key made fresh with every install: whole, as sixteen groups of four hex
# digits, or shortened to its ends.
# Durations measured on the run's own clock, and countdowns.
DURATION = re.compile(r"\b\d+m \d+s\b|\b\d+(?:\.\d+)?(?:ms|s|m|h)\b")
KEY = re.compile(r"\b[0-9a-f]{4}(?: [0-9a-f]{4}){15}\b|\b[0-9a-f]{4}…[0-9a-f]{4}\b")
# The leased simulator's name, which the phone shows as its own: which of the
# pool's devices a run is given is not the run's to choose.
DEVICE = re.compile(r"\bamux-[a-z]+-\d+\b")
# When a usage limit resets: a time of day on the run's wall clock, or a
# weekday.
RESETS = re.compile(r"\bresets (?:\d{1,2}:\d{2}(?:\s?[AP]M)?|[A-Z][a-z]{2})")


def normalize(text: str) -> str:
    """Ages, keys, the phone's own name, durations, reset times and the
    served net's scratch suffixes, masked as the terminal driver masks ages
    and scratch paths."""
    text = AGE.sub("<age>", text)
    text = KEY.sub("<key>", text)
    text = DEVICE.sub("<device>", text)
    text = DURATION.sub("<t>", text)
    text = RESETS.sub("resets <time>", text)
    return SCRATCH.sub(lambda match: match.group(1) + "x" * len(match.group(2)), text)


def launch_arguments(
    ready: dict,
    scope: str,
    found: list[str],
    door_port: int,
    geometry: bool = True,
    relay_quic: str | None = None,
) -> list[str]:
    """What a driven launch is told: its door, the net's discovery scope and
    the machines its browser may report, loopback direct links and, when the
    net has one, the served relay by both its carriers. Element geometry is
    asked for unless the caller measures the app and turns it on itself only
    to tap. A `relay_quic` address stands in for the relay's own, a gate in
    front of it, and leaves the plaintext TCP carrier out: a measurement
    wants the carrier a phone away from home uses, and the fallback, dialled
    a moment later on loopback, would win the race against a gated dial."""
    arguments = ["-amux-door-port", str(door_port)]
    if geometry:
        arguments.append("-amux-element-geometry")
    arguments += [
        "-amux-discovery-scope", scope,
        "-amux-discover-only", ",".join(found),
        "-amux-lan-bind", "127.0.0.1:0",
    ]
    if ready.get("cloud_url") and (ready.get("relay_tcp") or ready.get("relay_quic")):
        arguments += ["-amux-scripted-cloud", "-amux-relay", ready["cloud_url"]]
        if ready.get("relay_tcp") and relay_quic is None:
            arguments += ["-amux-relay-tcp", ready["relay_tcp"]]
        if ready.get("relay_quic") and ready.get("relay_root"):
            arguments += [
                "-amux-relay-quic", relay_quic or ready["relay_quic"],
                "-amux-relay-root", ready["relay_root"],
            ]
    return arguments


def is_volatile(identifier: str, named: tuple[str, ...] = ()) -> bool:
    return identifier.endswith(VOLATILE) or identifier in named


# The tabs' roots by the name the door gives the tab on screen. A tab
# somebody has been to is kept and still reports its elements while another
# tab, or a page pushed over it, is on screen; one never reached for is not
# built and reports nothing.
TAB_ROOTS = {"agents": "home.", "hosts": "hosts."}


# Pages pushed over a tab, which hide the tab's root beneath them.
PAGES = ("chat", "pin", "pair-confirm")


def uncovered(elements: list[dict]) -> list[dict]:
    """What is drawn, without the tab roots a pushed page covers: they are
    still laid out underneath, where nobody can see them, and keep changing
    (a fleet re-sorting behind a chat)."""
    if not any(element["identifier"] in PAGES for element in elements):
        return elements
    roots = tuple(TAB_ROOTS.values())
    return [
        element for element in elements
        if not (element["identifier"].startswith(roots) or element["identifier"] + "." in roots)
    ]


def on_screen(identifier: str, screen: str | None) -> bool:
    root = next((root for root in TAB_ROOTS.values() if identifier.startswith(root)), None)
    return root is None or screen is None or TAB_ROOTS.get(screen) == root


# How far either side of a volatile surface its mask reaches, at least: its
# words are as wide as they are in this run, and an age that read "39s" when
# the golden was taken is wider than one reading "1m" now. The reach grows
# with the line's height, as a character's width does with the text size.
VOLATILE_SLACK = 8


def volatile_masks(
    elements: list[dict], named: tuple[str, ...] = (), scale: int = SCALE, screen: str | None = None
) -> list[str]:
    """Pixel rectangles, as `xtask golden diff --mask` reads them, of every
    surface a view declared volatile and every element a story named so (a
    key fingerprint made fresh each run, a countdown), on the page the door
    says is on screen."""
    masks = []
    for element in elements:
        if not is_volatile(element["identifier"], named) or not on_screen(element["identifier"], screen):
            continue
        frame = element["frame"]
        slack = max(VOLATILE_SLACK, frame["height"])
        x = max(0, int((frame["x"] - slack) * scale))
        y = max(0, int(frame["y"] * scale))
        width = int((frame["x"] + frame["width"] + slack) * scale + 0.999) - x
        height = int((frame["y"] + frame["height"]) * scale + 0.999) - y
        masks.append(f"{x},{y},{width},{height}")
    return masks


def pictured(elements: list[dict], frozen: list[dict], display: tuple[int, int], scale: int = SCALE) -> list[dict]:
    """The volatile surfaces of a screen the app froze, where the report
    draws its picture of that screen (`report.frame`, the whole display
    shrunk to fit). They are pixels in a picture there, not elements, so the
    picture of an age moves with the run as the age itself does."""
    frame = next((element["frame"] for element in elements if element["identifier"] == "report.frame"), None)
    if frame is None or not frozen:
        return []
    shrink = frame["width"] / (display[0] / scale)
    return [
        {
            "identifier": f"report.frame:{element['identifier']}{VOLATILE}",
            "frame": {
                "x": frame["x"] + element["frame"]["x"] * shrink,
                "y": frame["y"] + element["frame"]["y"] * shrink,
                "width": element["frame"]["width"] * shrink,
                "height": element["frame"]["height"] * shrink,
            },
        }
        for element in frozen
    ]


def png_size(png: Path) -> tuple[int, int]:
    header = png.read_bytes()[16:24]
    return int.from_bytes(header[:4], "big"), int.from_bytes(header[4:], "big")


def named_ids(text: str, ids: dict[str, str]) -> str:
    """Every id the net made this run, as the name it was declared by; any
    other id as <id>."""
    return UUID.sub(lambda match: "<" + ids.get(match.group(0), "id") + ">", text)


def geometry(elements: list[dict], named: tuple[str, ...] = (), ids: dict[str, str] | None = None) -> str:
    """One line per element: its name, its words and where it is drawn, in
    whole points. Volatile surfaces keep their place and lose their words;
    ids read as the names the topology gave them."""
    ids = ids or {}
    lines = []
    for element in elements:
        frame = element["frame"]
        where = ",".join(str(round(frame[key])) for key in ("x", "y", "width", "height"))
        if is_volatile(element["identifier"], named):
            # Its size follows its words (a duration's width), so only that
            # it is drawn is recorded.
            lines.append(named_ids(f"{element['identifier']} | <volatile>", ids))
            continue
        words = normalize(f"{element.get('label') or ''} | {element.get('value') or ''}")
        state = "" if element.get("enabled", True) else " (disabled)"
        lines.append(named_ids(f"{element['identifier']} | {words} | {where}{state}", ids))
    return "\n".join(lines) + "\n"


# Words `normalize` masked, whose drawn width still follows what they said.
MASKED = re.compile(r"<(?:age|key|device|t|time)>")
# How far a line with masked words may move sideways or change width: "Worked
# 9s" is narrower than "Worked 12s", and trailing text moves its left edge.
MASKED_SLACK = 16


def same_line(approved: str, drawn: str) -> bool:
    if approved == drawn:
        return True
    if not MASKED.search(approved):
        return False
    approved_words, _, approved_where = approved.rpartition(" | ")
    drawn_words, _, drawn_where = drawn.rpartition(" | ")
    approved_frame, _, approved_state = approved_where.partition(" ")
    drawn_frame, _, drawn_state = drawn_where.partition(" ")
    if (approved_words, approved_state) != (drawn_words, drawn_state):
        return False
    try:
        x, y, width, height = (int(value) for value in approved_frame.split(","))
        x2, y2, width2, height2 = (int(value) for value in drawn_frame.split(","))
    except ValueError:
        return False
    return (y, height) == (y2, height2) and abs(x - x2) <= MASKED_SLACK and abs(width - width2) <= MASKED_SLACK


def same_geometry(approved: str, drawn: str) -> bool:
    """Whether two geometry records hold the same lines, in any order.

    Each line carries its own frame, so the order says nothing about where
    anything is drawn; it is the order SwiftUI reduced the screen's reports
    in and the system walked its accessibility tree, and both move between
    runs of one build. An exact line is matched before a near one, so a near
    match never takes the line another needed exactly."""
    left = drawn.splitlines()
    near = []
    for line in approved.splitlines():
        if line in left:
            left.remove(line)
        else:
            near.append(line)
    for line in near:
        match = next((index for index, candidate in enumerate(left) if same_line(line, candidate)), None)
        if match is None:
            return False
        del left[match]
    return not left


def by_name(elements: list[dict]) -> dict[str, dict]:
    named: dict[str, dict] = {}
    for element in elements:
        name, count = element["identifier"], 1
        while name in named:
            count += 1
            name = f"{element['identifier']}#{count}"
        named[name] = element
    return named


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def simctl(*arguments: str, timeout: float = 120, env: dict[str, str] | None = None) -> str:
    return subprocess.run(
        ["xcrun", "simctl", *arguments],
        check=True, text=True, capture_output=True, timeout=timeout, env=env,
    ).stdout


class PhoneJourney:
    def __init__(
        self,
        story: dict,
        topology: Path,
        udid: str,
        output: Path | None = None,
        goldens: Path | None = None,
        app: Path = APP,
    ):
        self.story = story
        # The build driven: the debug app, or the optimised one the
        # performance suite measures.
        self.build = app
        self.name = story["id"]
        self.udid = udid
        self.output = output or OUTPUT / self.name
        # Where this run's screens are compared, and whether a screen that
        # no longer matches is rewritten there instead.
        self.goldens = goldens or GOLDENS / self.name
        self.update = os.environ.get("UPDATE_JOURNEY_GOLDENS") == "1"
        # Whether every launch turns the app's reduce-transparency setting on,
        # so each frosted surface is drawn flat. The render server finishes
        # glass after the app has drawn, on its own schedule, and a photograph
        # at any fixed moment shows one stage or another of that work; drawn
        # flat, the same screens repeat pixel for pixel. A run that measures
        # the app as it ships, or photographs glass for a person to look at,
        # turns this off before it launches.
        self.flat = True
        if self.output.exists():
            shutil.rmtree(self.output)
        self.output.mkdir(parents=True)
        self.topology = json.loads(topology.read_text())
        # One fixed place, so every path the phone draws (an agent's working
        # directory, an ask's scope) is the same on every run and every
        # machine. Short: daemons bind Unix sockets under it. Journeys in
        # other checkouts wait for it on the lock.
        self.lock = open(SCRATCH_LOCK, "w")
        fcntl.flock(self.lock, fcntl.LOCK_EX)
        shutil.rmtree(SCRATCH_DIR, ignore_errors=True)
        SCRATCH_DIR.mkdir()
        self.scratch = SCRATCH_DIR
        self.actions: list[str] = []
        self.observations: dict[str, object] = {}
        self.process: subprocess.Popen[bytes] | None = None
        self.ready: dict = {}
        # The volatile surfaces on screen when the system last photographed
        # the app, masked again wherever a report shows that photograph.
        self.frozen: list[dict] = []
        self.port = 0
        # The process id the app was launched as on this Mac, for a profile
        # of the Mac to be filtered by.
        self.pid = 0
        self.env = {k: v for k, v in os.environ.items() if k not in ("AMUX_LOG", "AMUX_CONFIG")}
        self.env.update({key: str(self.scratch) for key in ("TMPDIR", "TMP", "TEMP")})
        self.env["AMUX_TEST_DISCOVERY_MODE"] = "disabled"
        # The served machines say what they did with each link and stream, so
        # a reconciliation that took longer than its round trips can be read
        # from their side too; a caller's own filter wins.
        self.env.setdefault("RUST_LOG", "info,node::link=debug,node::services::reachability=debug,node::routing=debug,node::sources=debug,node::dispatcher=debug,node::edge::peer=debug")
        self.process = subprocess.Popen(
            [str(TESTNET), "serve", str(topology), "--root-in", str(self.scratch)],
            cwd=ROOT,
            env=self.env,
            stdout=subprocess.PIPE,
            stderr=open(self.output / "testnet.log", "wb"),
        )
        try:
            self.ready = _read_readiness(self.process)
        except BaseException:
            self.process.kill()
            self.process.wait(timeout=30)
            raise
        self.actions.append(f"ready control={self.ready['control']}")

    # --- the served net ---------------------------------------------------

    def request(self, request: object, label: str | None = None) -> object:
        self.actions.append("net " + json.dumps(request, separators=(",", ":")))
        reply = door(self.ready["control"], request)
        if label is not None:
            self.observations[label] = reply
        return reply

    def wait_chat(
        self, host: str, agent: str, predicate: Callable[[dict], bool], description: str, timeout: float = 60.0
    ) -> dict:
        deadline = time.monotonic() + timeout
        last: dict = {}
        while time.monotonic() < deadline:
            last = door(self.ready["control"], {"Chat": {"host": host, "agent": agent}})
            if predicate(last):
                self.observations[description] = last
                self.actions.append(f"observed at {host}: {description}")
                return last
            time.sleep(0.2)
        raise RuntimeError(f"timed out waiting for {description}; {host} holds {last!r}")

    def wait_inventory(
        self, host: str, predicate: Callable[[list[dict]], bool], description: str, timeout: float = 60.0
    ) -> list[dict]:
        deadline = time.monotonic() + timeout
        last: list[dict] = []
        while time.monotonic() < deadline:
            last = door(self.ready["control"], {"Inventory": {"host": host}})["agents"]
            if predicate(last):
                self.observations[description] = last
                self.actions.append(f"observed at {host}: {description}")
                return last
            time.sleep(0.2)
        raise RuntimeError(f"timed out waiting for {description}; {host} lists {last!r}")

    def provider_input(self, agent: str, label: str) -> list[str]:
        reply = self.request({"ProviderInput": {"agent": agent}}, label)
        assert isinstance(reply, dict)
        return reply["lines"]

    def host_id(self, host: str) -> str:
        return next(item["host_id"] for item in self.ready["hosts"] if item["name"] == host)

    def config(self, host: str) -> str:
        return next(item["config"] for item in self.ready["hosts"] if item["name"] == host)

    def amux(self, host: str, *arguments: str) -> subprocess.Popen[str]:
        """`amux` on a served machine, as a person at that machine types it."""
        self.actions.append(f"at {host}: amux {' '.join(arguments)}")
        return subprocess.Popen(
            [str(AMUX), "--config", self.config(host), *arguments],
            env=self.env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )

    def pairing_link(self, host: str) -> tuple[subprocess.Popen[str], str]:
        """The link `amux pair --qr --print-link` prints on `host`; the
        process stays open, as the machine keeps offering it."""
        process = self.amux(host, "pair", "--qr", "--print-link")
        assert process.stdout is not None
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            line = process.stdout.readline()
            if line.startswith("Pairing link: "):
                return process, line.removeprefix("Pairing link: ").strip()
            if not line and process.poll() is not None:
                break
        process.kill()
        raise RuntimeError(f"{host} printed no pairing link")

    # --- the phone --------------------------------------------------------

    def launch(
        self,
        *extra: str,
        found: list[str] | None = None,
        geometry: bool = True,
        relay_quic: str | None = None,
    ) -> None:
        """The debug app, installed fresh so no earlier run's identity or
        trust is on the phone, launched against this net."""
        simctl("terminate", self.udid, BUNDLE_ID, timeout=60) if self._running() else None
        subprocess.run(["xcrun", "simctl", "uninstall", self.udid, BUNDLE_ID], capture_output=True, timeout=120)
        simctl("install", self.udid, str(self.build), timeout=300)
        self.relaunch(*extra, found=found, geometry=geometry, relay_quic=relay_quic)

    def relaunch(
        self,
        *extra: str,
        found: list[str] | None = None,
        geometry: bool = True,
        relay_quic: str | None = None,
    ) -> None:
        """The same installation launched again, as a person reopens it. Its
        browser reports the machines named in `found`, every one of the net's
        when nothing is said. Without `geometry` the app reports what is
        drawn but not where, which a measurement wants: see `geometry()`. A
        `relay_quic` address is dialled in the relay's place, QUIC only.
        When the topology puts any host behind a router, on a network of its
        own, the phone dials the relay through the net's gate unless told
        otherwise: the relay then sees the phone at the gate's socket, which
        takes datagrams from the relay alone, so no host punches to it."""
        if relay_quic is None and any(host.get("router") for host in self.topology.get("hosts", [])):
            reply = self.request({"RelayGate": {}})
            assert isinstance(reply, dict)
            relay_quic = reply["addr"]
        scope = self.topology.get("scope", "")
        if found is None:
            found = [host["host_id"] for host in self.ready["hosts"]]
        self.port = free_port()
        arguments = launch_arguments(self.ready, scope, found, self.port, geometry, relay_quic) + list(extra)
        # The phone's runtime logs under the same filter as the served hosts,
        # so one run's two logs can be read side by side.
        launched = simctl(
            "launch", "--terminate-running-process", self.udid, BUNDLE_ID, *arguments,
            env={**os.environ, "SIMCTL_CHILD_RUST_LOG": self.env["RUST_LOG"]},
        )
        # `simctl launch` prints `<bundle id>: <pid>`.
        self.pid = int(launched.rsplit(":", 1)[-1].strip() or 0)
        self.actions.append("launch the app")
        # The door answering is the launch; what a capture needs settled it
        # settles itself. Settling here drew the window to an image over and
        # over on the main thread while the app was still building its first
        # frame, which a cold-launch measurement then counted.
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            try:
                self.app({"kind": "signposts"})
                break
            except (OSError, DoorError):
                time.sleep(0.5)
        else:
            raise RuntimeError("the app's door did not open within a minute")
        if self.flat:
            # Motion stays as the run has it: the door's own default, off.
            self.app({"kind": "assist", "motion": False, "transparency": True})
            self.actions.append("reduce transparency on")

    def quit(self) -> None:
        """The app closed, as a person swipes it away."""
        simctl("terminate", self.udid, BUNDLE_ID, timeout=60)
        self.actions.append("quit the app")

    def _running(self) -> bool:
        listed = subprocess.run(
            ["xcrun", "simctl", "spawn", self.udid, "launchctl", "list"], capture_output=True, text=True, timeout=60
        ).stdout
        return BUNDLE_ID in listed

    def app(self, request: dict, timeout: float = 90.0) -> dict:
        """One request to the app's door; its reply, or DoorError."""
        with socket.create_connection(("127.0.0.1", self.port), timeout=timeout) as connection:
            connection.settimeout(timeout)
            connection.sendall((json.dumps(request) + "\n").encode())
            with connection.makefile("rb") as stream:
                encoded = stream.readline()
        if not encoded:
            raise DoorError(f"the app's door closed on {request!r}")
        reply = json.loads(encoded)
        if reply.get("kind") == "error":
            raise DoorError(f"the app refused {request!r}: {reply.get('message')}")
        return reply

    def query(self) -> dict:
        return self.app({"kind": "query"})["state"]

    def system_screenshot(self) -> None:
        """The system photographs the app, as a person's screenshot does.
        What on screen moves with the run is remembered, for a report that
        shows the photograph."""
        state = self.query()
        self.frozen = [
            element for element in uncovered(state["elements"])
            if is_volatile(element["identifier"], OWN_IDENTITY) and on_screen(element["identifier"], state.get("screen"))
        ]
        self.app({"kind": "screenshot"})
        self.actions.append("the system took a screenshot")

    def elements(self) -> dict[str, dict]:
        """What is drawn by name; a name drawn again (every prompt row is
        chat.row.prompt) gets #2, #3 in drawing order."""
        return by_name(self.query()["elements"])

    def wait(self, predicate: Callable[[dict[str, dict]], bool], description: str, timeout: float = 60.0) -> dict[str, dict]:
        """Until what is drawn satisfies `predicate`, by element name."""
        deadline = time.monotonic() + timeout
        last: dict[str, dict] = {}
        while time.monotonic() < deadline:
            last = self.elements()
            if predicate(last):
                self.actions.append(f"reached {description}")
                return last
            time.sleep(0.2)
        drawn = "\n".join(f"{name} | {e.get('label')} | {e.get('value')}" for name, e in last.items())
        raise RuntimeError(f"timed out waiting for {description}; the phone draws:\n{drawn}")

    def wait_for(self, *identifiers: str, timeout: float = 60.0) -> dict[str, dict]:
        return self.wait(lambda drawn: all(name in drawn for name in identifiers), repr(identifiers), timeout)

    def tap(self, identifier: str, patience: float = 10.0) -> None:
        """A tap as a person makes one, as `ctl phone tap` does: on a control
        that is there and enabled, and again while the app has not yet drawn
        a just-enabled control able to take it."""
        self.wait(
            lambda drawn: drawn.get(identifier, {}).get("enabled", False) is not False,
            f"{identifier} there to tap",
            timeout=patience,
        )
        deadline = time.monotonic() + patience
        while True:
            try:
                self.app({"kind": "tap", "identifier": identifier})
                break
            except DoorError as error:
                if " did not activate" not in str(error) or time.monotonic() > deadline:
                    raise
                self.app({"kind": "settle"})
        self.actions.append(f"tap {identifier}")

    def geometry(self, on: bool) -> None:
        """Whether every identified element reports its frame. A tap needs
        the frames; producing them costs a real share of the main thread,
        so a measurement launches without them and asks only to tap."""
        self.app({"kind": "geometry", "on": on})
        self.actions.append(f"element geometry {'on' if on else 'off'}")

    def choose(self, label: str) -> None:
        """The item a person reads as `label` in a menu the app presented,
        once the menu is drawn."""
        self.wait(
            lambda drawn: any(element.get("label") == label for element in drawn.values()), f"{label!r} presented"
        )
        self.app({"kind": "choose", "label": label})
        self.actions.append(f"choose {label!r}")

    def search(self, text: str) -> None:
        """The screen's search field made to hold `text`, as a person pulls
        it into view, clears it and types."""
        self.app({"kind": "search", "text": text})
        self.actions.append(f"search {text!r}")

    def tab(self, title: str) -> None:
        """The tab reading `title` in the system's tab bar, whose tabs carry
        their titles and no names of their own."""
        self.choose(title)

    def settings(self) -> None:
        """Settings, as a person reaches them: the account disc on the agent
        list, then Settings in its menu. They open as a sheet over the tabs."""
        self.tab("Agents")
        self.wait_for("home.accounts")
        self.tap("home.accounts")
        self.choose("Settings")
        self.wait_for("you")

    def perform(self, identifier: str, action: str) -> None:
        """An element's named accessibility action, as VoiceOver offers it."""
        self.app({"kind": "perform", "identifier": identifier, "action": action})
        self.actions.append(f"{action} on {identifier}")

    def type(self, identifier: str, text: str) -> None:
        self.app({"kind": "type", "identifier": identifier, "text": text})
        self.actions.append(f"type {text!r} into {identifier}")

    def paste(self, identifier: str, text: str) -> None:
        self.app({"kind": "paste", "identifier": identifier, "text": text})
        self.actions.append(f"paste {len(text.splitlines())} lines into {identifier}")

    def pair(self, link: str) -> str:
        """The link the machine printed, as a scanned code hands it over."""
        reply = self.app({"kind": "pair", "qr": link})
        self.actions.append(f"pair by the link, answered by {reply.get('host')}")
        return reply["host"]

    def appearance(self, appearance: str) -> None:
        self.app({"kind": "appearance", "appearance": appearance})
        self.actions.append(f"appearance {appearance}")

    # --- screens ----------------------------------------------------------

    def screen(self, label: str, volatile: tuple[str, ...] = (), geometry: str | None = None) -> None:
        """What the phone shows now, compared with its reviewed golden: the
        display's pixels and the door's element geometry, with `volatile`
        elements masked."""
        differs = self.compare(label, volatile, geometry)
        if differs:
            raise RuntimeError(differs)

    def compare(self, label: str, volatile: tuple[str, ...] = (), geometry_label: str | None = None) -> str | None:
        """What differs between the phone now and the golden `label`, or
        None when nothing does. The element geometry is read from
        `geometry_label` when several pictures share one layout (the same
        screen in light and dark). With updating on, a golden that differs
        is rewritten and nothing is reported; one that matches is left
        alone, so a masked region never churns."""
        self.app({"kind": "settle"})
        state = self.query()
        # A tab not on screen still reports what it holds, and its bar's items
        # come and go from that report while it is hidden.
        elements = [
            element for element in uncovered(state["elements"])
            if on_screen(element["identifier"], state.get("screen"))
        ]
        actual = self.output / "actual"
        actual.mkdir(parents=True, exist_ok=True)
        png = actual / f"{label}.png"
        self.steady_display(png)
        volatile = volatile + OWN_IDENTITY
        ids = {host["host_id"]: host["name"] for host in self.ready["hosts"]}
        ids |= {agent["id"]: agent["name"] for agent in self.ready["agents"]}
        drawn = geometry(elements, volatile, ids)
        geometry_label = geometry_label or label
        (actual / f"{geometry_label}.elements.txt").write_text(drawn)
        # A volatile surface is masked where it stood before the photograph
        # and after it: an age crossing into another digit while the display
        # settles (9s to 19s) grows past the frame read first.
        after = self.query()
        masks = volatile_masks(elements, volatile, screen=state.get("screen"))
        masks += volatile_masks(uncovered(after["elements"]), volatile, screen=after.get("screen"))
        masks += volatile_masks(pictured(elements, self.frozen, png_size(png)))
        self.actions.append(f"captured {label}")
        if self.update and os.environ.get("CI"):
            raise RuntimeError("rewriting goldens is refused in CI")
        golden_png = self.goldens / f"{label}.png"
        expected = self.goldens / f"{geometry_label}.elements.txt"
        differs = self._differs(png, golden_png, expected, drawn, masks, label)
        if differs and self.update:
            self.goldens.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(png, golden_png)
            expected.write_text(drawn)
            self.actions.append(f"rewrote {label}: {differs.splitlines()[0]}")
            return None
        return differs

    def _differs(
        self,
        png: Path,
        golden_png: Path,
        expected: Path,
        drawn: str,
        masks: list[str],
        label: str,
    ) -> str | None:
        if not expected.exists() or not golden_png.exists():
            return f"missing golden {golden_png}; review a rewritten one"
        approved = expected.read_text()
        if not same_geometry(approved, drawn):
            diff = "\n".join(
                difflib.unified_diff(
                    approved.splitlines(), drawn.splitlines(), str(expected), f"actual/{expected.name}", lineterm=""
                )
            )
            return f"geometry differs: {expected}\n{diff}"
        compared = subprocess.run(
            [
                "cargo", "run", "-q", "-p", "xtask", "--", "golden", "diff",
                "--expected", str(golden_png),
                "--actual", str(png),
                "--out", str(self.output / "diff" / label),
                "--simulator", SIMULATOR,
                *[argument for mask in masks for argument in ("--mask", mask)],
            ],
            cwd=ROOT, text=True, capture_output=True, timeout=600,
        )
        if compared.returncode != 0:
            return (
                f"screen {label} differs: {compared.stdout}{compared.stderr}"
                f"; expected, actual and diff are under {self.output / 'diff' / label}"
            )
        # A match that spent some of the comparison's allowance for stray
        # pixels says how much, so the allowance can be judged from the runs.
        verdict = compared.stdout.strip()
        if verdict != "same":
            print(f"AMUX_PICTURE_STRAYS screen={label} {verdict}", flush=True)
            self.actions.append(f"compared {label}: {verdict}")
        return None

    def steady_display(self, png: Path) -> None:
        """The display once two photographs a moment apart agree: a
        transition or the glass settling behind it is over."""
        previous = b""
        for _ in range(20):
            simctl("io", self.udid, "screenshot", "--type=png", str(png))
            taken = png.read_bytes()
            if taken == previous:
                return
            previous = taken
            time.sleep(0.3)
        raise RuntimeError(f"the display never held still for {png.stem}")

    # --- the end ----------------------------------------------------------

    def finish(self, assertions: list[str]) -> None:
        self._write("PASS\n" + "".join(f"- {item}\n" for item in assertions))

    def fail(self, error: BaseException) -> None:
        # The display as the story stopped, beside what it says it drew.
        try:
            simctl("io", self.udid, "screenshot", "--type=png", str(self.output / "failed.png"))
        except Exception as missed:  # the failure itself matters more
            self.actions.append(f"no picture of the failure: {missed}")
        self._write(f"FAIL\n- {type(error).__name__}: {error}\n")

    def _write(self, result: str) -> None:
        (self.output / "actions.txt").write_text("\n".join(self.actions) + "\n")
        (self.output / "observations.json").write_text(
            json.dumps(self.observations, indent=2, sort_keys=True) + "\n"
        )
        (self.output / "result.txt").write_text(result)

    def close(self) -> None:
        try:
            subprocess.run(["xcrun", "simctl", "terminate", self.udid, BUNDLE_ID], capture_output=True, timeout=60)
            if self.process is not None and self.process.poll() is None:
                try:
                    host, port = self.ready["control"].rsplit(":", 1)
                    with socket.create_connection((host, int(port)), timeout=10) as connection:
                        connection.sendall(b'"Shutdown"\n')
                        connection.recv(4096)
                except (OSError, KeyError):
                    pass
                try:
                    self.process.wait(timeout=60)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=30)
                    raise RuntimeError("testnet did not shut down within a minute")
            (self.output / "actions.txt").write_text("\n".join(self.actions) + "\n")
        finally:
            shutil.rmtree(self.scratch, ignore_errors=True)
            self.lock.close()


def story(name: str) -> dict:
    manifest = json.loads(MANIFEST.read_text())
    matches = [item for item in manifest["journeys"] if item["id"] == name]
    if len(matches) != 1 or "phone" not in matches[0].get("clients", []):
        raise RuntimeError(f"no phone journey named {name!r}")
    return matches[0]
