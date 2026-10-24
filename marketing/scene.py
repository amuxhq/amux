"""The launch scene: a small team's worth of agents on three machines.

`just scene up` writes the scene's topology into its session folder and
starts it with `testnet ctl up`; `just scene down` takes it down. Every
agent plays a script from `marketing/scripts`, so the scene is the same on
every start, and every README image and video take comes from it.

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


def phone_shots() -> None:
    """The phone, paired with every machine: its fleet, and the Codex agent's
    request waiting there. Needs the simulator lease (`wt exec <tree> --
    scripts/with iphone -- ...`)."""
    # A fresh install, so nothing an earlier scene paired lingers.
    devices = json.loads(subprocess.run(
        ["xcrun", "simctl", "list", "devices", "-j"], check=True, capture_output=True, text=True,
    ).stdout)["devices"]
    udid = next(
        device["udid"] for runtime in devices.values() for device in runtime
        if device["name"] == os.environ["WT_LEASE_IPHONE"]
    )
    subprocess.run(["xcrun", "simctl", "uninstall", udid, "sh.amux.app"], check=True, timeout=120)
    output("phone", "open", "--relay")
    for host in HOSTS:
        output("phone", "pair", host)
    output("phone", "connect", "ada")
    output("phone", "tap", element("home.family.", "Show 2"))
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


def main(arguments: list[str]) -> int:
    verb = arguments[0] if arguments else "up"
    FOLDER.mkdir(parents=True, exist_ok=True)
    if verb in ("up", "shots"):
        build()
        ctl("down")
        path = FOLDER / "topology.json"
        path.write_text(json.dumps(topology(), indent=2))
        print(f"scene: {FOLDER}", flush=True)
        if ctl("up", str(path)):
            return 1
        return shots() if verb == "shots" else 0
    if verb == "down":
        code = ctl("down")
        for folder, _ in HOSTS.values():
            shutil.rmtree(HOME / folder, ignore_errors=True)
            parent = (HOME / folder).parent
            if parent != HOME and parent.exists() and not any(parent.iterdir()):
                parent.rmdir()
        return code
    print("usage: scene.py up|shots|down", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
