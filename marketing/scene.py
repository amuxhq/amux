"""The launch scene: a small team's worth of agents on three machines.

`just scene up` writes the scene's topology into its session folder and
starts it with `testnet ctl up`; `just scene down` takes it down. Every
agent plays a script from `marketing/scripts`, so the scene is the same on
every start, and every README image and video take comes from it.

`just scene shots` takes the README's pictures; `just scene film` films the
takes the launch film is cut from (see `film()`).

The product the agents work on is Tally, an invoicing app whose source is
`marketing/repo`. Every host holds a clone of it.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
TESTNET = ROOT / "target/debug/testnet"
# Outside the checkout, as `just env`'s worlds are: inside it, git would
# find this repository above every agent's folder.
FOLDER = Path.home() / ".local/state/amux-marketing" / ROOT.name / "launch"

# Each machine keeps its clone of Tally where its owner would, on the branch
# its agents work on. The clones live in a home directory of their own, for
# the clients to write them as a person's: the terminal client runs with HOME
# set to it, and the phone writes any `/Users/<name>/...` as `~/...`, so the
# home has to sit directly under /Users. /Users/Shared is the one such
# directory anyone may write to.
HOME = Path("/Users/Shared")
# Where the pictures land, light and dark beside each other.
IMAGES = FOLDER / "images"
# Where the film's takes land.
TAKES = FOLDER / "takes"
HOSTS = {
    "studio": ("src/tally", "checkout-redesign"),
    "build": ("tally", "main"),
    "laptop": ("code/tally", "main"),
}

# name, host, kind, extra fields. Declared in the order they last changed
# state, oldest first, so the fleet lists them as a day's work would.
AGENTS = [
    ("deps-bump", "build", "codex", {}),
    ("api-docs", "laptop", "claude_sdk", {"permission": "acceptEdits"}),
    ("checkout-redesign", "studio", "claude_sdk", {
        "permission": "acceptEdits",
        "prompt": "Redesign checkout as a single page that pays with the client's saved card, "
                  "with clearer errors. Have Codex on build write the payment tests while you work, "
                  "and get a helper to rewrite the error copy.",
    }),
    ("db-migration", "laptop", "claude_sdk", {
        "permission": "plan",
        "prompt": "Plan moving invoice line items out of the JSONB column into their own table.",
    }),
    ("flaky-sync-test", "build", "codex", {
        "prompt": "two_devices_converge_after_offline_edits fails about one run in twenty. Find out why and fix it.",
    }),
]

PROMPTS = {
    "deps-bump": "Bump the workspace dependencies and make sure the tests still pass.",
    "api-docs": "Update the API reference for the new checkout endpoint.",
}

# What an agent another agent starts on that host plays: checkout-redesign
# starts copy-pass beside itself and payment-tests on build.
HOST_SCRIPTS = {"studio": "copy-pass", "build": "payment-tests"}


def script(name: str) -> dict:
    text = (HERE / "scripts" / f"{name}.json").read_text()
    return json.loads(text.replace("{home}", str(HOME)))


def clone(host: str) -> Path:
    """A fresh clone of Tally for `host`: the repository committed on main,
    then checked out on the host's branch."""
    folder, branch = HOSTS[host]
    path = HOME / folder
    subprocess.run(["rm", "-rf", str(path)], check=True)
    shutil.copytree(HERE / "repo", path)
    git = ["git", "-C", str(path), "-c", "user.name=Ada", "-c", "user.email=ada@example.com"]
    subprocess.run([*git, "init", "-q", "-b", "main"], check=True)
    subprocess.run([*git, "add", "-A"], check=True)
    subprocess.run([*git, "commit", "-q", "-m", "Tally"], check=True)
    if branch != "main":
        subprocess.run([*git, "checkout", "-q", "-b", branch], check=True)
    return path


def topology() -> dict:
    folders = {name: clone(name) for name in HOSTS}
    hosts = []
    for name in HOSTS:
        host = {"name": name, "lan": True, "bonjour": True, "account": "ada"}
        if name in HOST_SCRIPTS:
            host["script"] = script(HOST_SCRIPTS[name])
        hosts.append(host)
    agents = []
    for name, host, kind, extra in AGENTS:
        agent = {
            "name": name, "host": host, "kind": kind, "cwd": str(folders[host]),
            "script": script(name),
            "prompt": PROMPTS.get(name),
        }
        agent.update(extra)
        agents.append(agent)
    return {
        "hosts": hosts,
        # One person's account on a paid plan, so the phone is signed in and
        # reaches every machine as it would away from home.
        "relay": {"accounts": [{"name": "ada", "tier": "pro"}]},
        "links": [{"a": "studio", "b": "build"}, {"a": "studio", "b": "laptop"}, {"a": "build", "b": "laptop"}],
        "agents": agents,
    }


def build() -> None:
    """The binaries the net runs, as `just env` builds them; the phone app
    is `just ios build`'s."""
    subprocess.run(
        [str(ROOT / "scripts/bounded"), "1800", "cargo", "build", "--locked",
         "-p", "amux", "-p", "testnet", "-p", "provider-fakes", "--bins", "--features", "amux/bundled"],
        cwd=ROOT, check=True,
    )


def ctl(*arguments: str) -> int:
    command = [str(ROOT / "scripts/bounded"), "900", str(TESTNET), "ctl", *arguments]
    return subprocess.run(command, cwd=FOLDER).returncode


def tui(*arguments: str) -> int:
    """A `ctl tui` command with HOME at the scene's home: the pane's tmux
    server, started by the first of them, keeps it for every pane."""
    command = [str(ROOT / "scripts/bounded"), "900", str(TESTNET), "ctl", "tui", *arguments]
    return subprocess.run(command, cwd=FOLDER, env={**os.environ, "HOME": str(HOME)}).returncode


def output(*arguments: str) -> str:
    command = [str(ROOT / "scripts/bounded"), "300", str(TESTNET), "ctl", *arguments]
    return subprocess.run(command, cwd=FOLDER, check=True, capture_output=True, text=True).stdout


def screen(pane: str) -> str:
    return output("tui", "screen", pane)


def keys(pane: str, *names: str, settle: float = 0.6) -> None:
    output("tui", "keys", pane, *names)
    time.sleep(settle)


def cursor_to(pane: str, text: str, mark: str, key: str = "j") -> None:
    """Press `key` until the line holding `mark` (the client's highlight)
    also holds `text`."""
    for _ in range(80):
        if any(mark in line and text in line for line in screen(pane).splitlines()):
            return
        keys(pane, key, settle=0.15)
    raise SystemExit(f"scene: never reached {text!r} in {pane}")


def shoot(pane: str, name: str) -> None:
    taken = Path(output("tui", "shot", pane).splitlines()[0])
    IMAGES.mkdir(parents=True, exist_ok=True)
    shutil.copy(taken, IMAGES / f"{name}-{pane}.png")
    # The frame beside it is what compose.py redraws at twice the size.
    shutil.copy(taken.with_suffix(".frame.json"), IMAGES / f"{name}-{pane}.frame.json")
    print(f"scene: {name}-{pane}.png", flush=True)


def ready() -> None:
    """Until every agent has reached the state the pictures show, and the
    newest change is a minute old."""
    wanted = [
        ("flaky-sync-test", "NEEDS_YOU"), ("db-migration", "NEEDS_YOU"),
        ("checkout-redesign", "WORKING"), ("copy-pass", "EXITED"),
        ("payment-tests", "WORKING"), ("api-docs", "IDLE"), ("deps-bump", "IDLE"),
    ]
    deadline = time.monotonic() + 600
    while time.monotonic() < deadline:
        fleet = output("fleet", "studio").splitlines()
        rows = {line.split()[0]: line for line in fleet if line and not line.startswith(" ")}
        if all(state in rows.get(agent, "") for agent, state in wanted):
            time.sleep(60)
            return
        time.sleep(5)
    raise SystemExit("scene: the agents never came to rest")


def shots() -> int:
    ready()
    for theme in ("dark", "light"):
        tui("open", "studio", "--size", "120x34", "--theme", theme, "--name", theme)
        time.sleep(2)
        cursor_to(theme, "checkout-redesign", "›")
        keys(theme, "Right")
        keys(theme, "g")
        shoot(theme, "fleet")
        output("tui", "resize", theme, "120x40")
        cursor_to(theme, "checkout-redesign", "›")
        keys(theme, "Enter", settle=2)
        shoot(theme, "chat")
        output("tui", "resize", theme, "128x36")
        keys(theme, "C-a", "r", settle=2)
        cursor_to(theme, "MissingCard", "▌")
        keys(theme, "c")
        output("tui", "type", theme, "Answer 422 with a message the page can show, not a 500, when neither card is sent.")
        keys(theme, "Enter")
        shoot(theme, "review")
        output("tui", "close", theme)
    if os.environ.get("WT_LEASE_IPHONE"):
        phone_shots()
    return 0


def phone_shoot(name: str) -> None:
    output("phone", "settle")
    taken = Path(output("phone", "shot").splitlines()[0])
    shutil.copy(taken, IMAGES / f"{name}.png")
    print(f"scene: {name}.png", flush=True)


def element(prefix: str, label: str) -> str:
    """The id of the element under `prefix` whose label starts with `label`,
    waiting a while for it to be drawn."""
    for _ in range(30):
        for line in output("phone", "query").splitlines():
            words = line.split()
            if words and words[0].startswith(prefix) and f'"{label}' in line:
                return words[0]
        time.sleep(1)
    raise SystemExit(f"scene: no {prefix} element labelled {label!r} on the phone")


def leased_phone() -> str:
    """The udid of the simulator this session holds the lease on."""
    devices = json.loads(subprocess.run(
        ["xcrun", "simctl", "list", "devices", "-j"], check=True, capture_output=True, text=True,
    ).stdout)["devices"]
    return next(
        device["udid"] for runtime in devices.values() for device in runtime
        if device["name"] == os.environ["WT_LEASE_IPHONE"]
    )


def phone_paired() -> str:
    """The phone freshly installed, paired with every machine and signed in,
    on its agent list with checkout-redesign's family open. Answers its udid."""
    udid = leased_phone()
    # A fresh install, so nothing an earlier scene paired lingers.
    subprocess.run(["xcrun", "simctl", "uninstall", udid, "sh.amux.app"], check=True, timeout=120)
    output("phone", "open", "--relay")
    for host in HOSTS:
        output("phone", "pair", host)
    output("phone", "connect", "ada")
    # The scene's person has notifications on already: no offer of them.
    output("phone", "tap", "home.callout.notifications.dismiss")
    output("phone", "tap", element("home.family.", "Show 2"))
    return udid


def phone_shots() -> None:
    """The phone, paired with every machine: its fleet, and the Codex agent's
    request waiting there. Needs the simulator lease (`wt exec <tree> --
    scripts/with iphone -- ...`)."""
    phone_paired()
    # Both fleets before either chat: shot after coming back from one (and
    # switching appearance), the list's large title had collapsed into the
    # bar.
    for appearance in ("dark", "light"):
        output("phone", "appearance", appearance)
        phone_shoot(f"phone-fleet-{appearance}")
    output("phone", "tap", element("home.row.", "flaky-sync-test"))
    for appearance in ("dark", "light"):
        output("phone", "appearance", appearance)
        phone_shoot(f"phone-ask-{appearance}")


def labelled(label: str) -> str:
    """The id of the element whose label is exactly `label`."""
    for _ in range(30):
        for line in output("phone", "query").splitlines():
            words = line.split()
            if words and f'"{label}"' in line:
                return words[0]
        time.sleep(1)
    raise SystemExit(f"scene: no element labelled {label!r} on the phone")


class Takes:
    """The film's recordings, each started at a moment this keeps, so the
    edit can lay them on one clock. `takes.json` lists every take and every
    action as milliseconds since the scene started."""

    def __init__(self) -> None:
        shutil.rmtree(TAKES, ignore_errors=True)
        TAKES.mkdir(parents=True)
        self.start = time.monotonic()
        self.log: dict = {"takes": {}, "events": []}
        self.running: list[subprocess.Popen] = []

    def now(self) -> int:
        return round((time.monotonic() - self.start) * 1000)

    def event(self, name: str) -> None:
        self.log["events"].append({"name": name, "ms": self.now()})
        print(f"scene: {self.now() / 1000:7.1f}s {name}", flush=True)
        self.save()

    def record(self, pane: str) -> None:
        """Film a terminal pane until `stop`."""
        path = TAKES / f"{pane}.jsonl"
        self.running.append(subprocess.Popen(
            [str(TESTNET), "ctl", "tui", "record", pane, str(path), "--every", "40ms"], cwd=FOLDER,
        ))
        self.log["takes"][pane] = {"file": path.name, "kind": "terminal", "ms": self.now()}
        self.save()

    def phone(self, name: str, udid: str) -> subprocess.Popen:
        path = TAKES / f"{name}.mp4"
        video = subprocess.Popen(
            ["xcrun", "simctl", "io", udid, "recordVideo", "--codec=h264", "--force", str(path)],
            stderr=subprocess.DEVNULL,
        )
        # It takes a moment to write its first frame.
        time.sleep(2)
        self.log["takes"][name] = {"file": path.name, "kind": "phone", "ms": self.now()}
        self.save()
        return video

    def stop(self) -> None:
        for process in self.running:
            process.terminate()
            process.wait(timeout=30)
        self.running.clear()

    def save(self) -> None:
        (TAKES / "takes.json").write_text(json.dumps(self.log, indent=2))


def film() -> int:
    """The takes the launch film is cut from, every one real UI: the fleet
    filling as the scene starts, checkout-redesign's chat as it hands work to
    its helpers, a code review comment typed into the review page, and the
    phone opening flaky-sync-test's request and allowing it, filmed with the
    terminal's fleet at the same moment. Terminal takes are frame files
    (`ctl tui record`) the film draws itself; the phone's is the simulator's
    own screen recording. Needs the simulator lease."""
    takes = Takes()
    try:
        for pane, size in (("fleet", "120x34"), ("chat", "120x40")):
            tui("open", "studio", "--size", size, "--theme", "dark", "--name", pane)
        time.sleep(2)
        takes.record("fleet")
        cursor_to("chat", "checkout-redesign", "›")
        takes.record("chat")
        takes.event("chat.open")
        keys("chat", "Enter", settle=2)
        udid = phone_paired() if os.environ.get("WT_LEASE_IPHONE") else None
        if udid:
            output("phone", "appearance", "dark")
        ready()
        # The fleet as the README shows it: checkout-redesign's family open, and
        # the selection back on New Agent.
        keys("fleet", "g")
        cursor_to("fleet", "checkout-redesign", "›")
        keys("fleet", "Right")
        keys("fleet", "g")
        takes.event("rest")

        tui("open", "studio", "--size", "128x36", "--theme", "dark", "--name", "review")
        time.sleep(2)
        takes.record("review")
        cursor_to("review", "checkout-redesign", "›")
        time.sleep(1.5)
        takes.event("review.chat")
        keys("review", "Enter", settle=2.5)
        takes.event("review.open")
        keys("review", "C-a", "r", settle=3)
        for _ in range(80):
            if any("▌" in line and "MissingCard" in line for line in screen("review").splitlines()):
                break
            keys("review", "j", settle=0.12)
        time.sleep(1.5)
        takes.event("review.comment")
        keys("review", "c", settle=1)
        # Typed as a person types: a key at a time, with the pauses a sentence has.
        comment = "Answer 422 with a message the page can show, not a 500, when neither card is sent."
        for index, char in enumerate(comment):
            output("tui", "type", "review", char)
            time.sleep(0.11 if char == " " else 0.05 + (index * 7 % 5) * 0.012)
            if char in ",.":
                time.sleep(0.25)
        time.sleep(1)
        takes.event("review.send")
        keys("review", "Enter", settle=3)
        takes.event("review.done")

        if udid:
            video = takes.phone("phone", udid)
            time.sleep(2)
            takes.event("phone.open")
            output("phone", "tap", element("home.row.", "flaky-sync-test"))
            allow = labelled("Allow")
            takes.event("phone.ask")
            # Long enough to read the request.
            time.sleep(4)
            takes.event("phone.allow")
            output("phone", "tap", allow)
            for _ in range(60):
                if "IDLE" in next((line for line in output("fleet", "studio").splitlines()
                                   if line.startswith("flaky-sync-test")), ""):
                    break
                time.sleep(0.5)
            takes.event("phone.done")
            time.sleep(4)
            video.send_signal(2)
            video.wait(timeout=60)
    finally:
        # A take that fails part-way still stops its recorders.
        takes.stop()
    for pane in ("fleet", "chat", "review"):
        output("tui", "close", pane)
    print(f"scene: takes in {TAKES}", flush=True)
    return 0


def main(arguments: list[str]) -> int:
    verb = arguments[0] if arguments else "up"
    FOLDER.mkdir(parents=True, exist_ok=True)
    if verb in ("up", "shots", "film"):
        build()
        ctl("down")
        path = FOLDER / "topology.json"
        path.write_text(json.dumps(topology(), indent=2))
        print(f"scene: {FOLDER}", flush=True)
        if ctl("up", str(path)):
            return 1
        return {"shots": shots, "film": film}.get(verb, lambda: 0)()
    if verb == "down":
        code = ctl("down")
        for folder, _ in HOSTS.values():
            shutil.rmtree(HOME / folder, ignore_errors=True)
            parent = (HOME / folder).parent
            if parent != HOME and parent.exists() and not any(parent.iterdir()):
                parent.rmdir()
        return code
    print("usage: scene.py up|shots|film|down", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
