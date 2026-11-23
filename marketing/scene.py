"""The launch scene: a small team's worth of agents on three machines.

`just scene up` writes the scene's topology into its session folder and
starts it with `testnet ctl up`; `just scene down` takes it down. Every
agent plays a script from `marketing/scripts`, so the scene is the same on
every start, and every README image and video take comes from it.

`just scene shots` takes the README's pictures; `just scene film` films the
takes the launch film is cut from (see `film()`). `just scene native` takes
the pictures of Claude Code's and Codex's own interfaces, which only the real
programs can draw, so it runs them (see `native()`).

The product the agents work on is Tally, an invoicing app whose source is
`marketing/repo`. Every host holds a clone of it.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import socket
import subprocess
import sys
import time
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
TESTNET = ROOT / "target/debug/testnet"
# Outside the checkout, as `just env`'s worlds are: inside it, git would
# find this repository above every agent's folder.
FOLDER = Path.home() / ".local/state/amux-marketing" / ROOT.name / "launch"

# Each machine keeps its clone of Tally where its owner would, on the branch
# its agents work on, in this account's home directory: the clients write a
# folder under a home as `~/...`, and the phone knows a home only as
# `/Users/<name>`. Each clone carries a mark in its .git, and the scene
# replaces or removes only folders that carry it, never a person's own.
HOME = Path.home()
MARK = ".git/amux-scene"
# Where the pictures land, light and dark beside each other.
IMAGES = FOLDER / "images"
# Where the film's takes land.
TAKES = FOLDER / "takes"
# Terminals' colours: amux's chat is photographed in each of THEMES, and the
# agents' own interfaces in a plain dark terminal.
PALETTES = HERE / "palettes"
THEMES = ("gruvbox-dark", "solarized-light", "catppuccin-mocha")
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
                  "and start a subagent to rewrite the error copy.",
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
    return clone_at(HOME / folder, branch)


def clone_at(path: Path, branch: str) -> Path:
    if path.exists() and not (path / MARK).exists():
        raise SystemExit(f"scene: {path} is not one of the scene's clones; move it and try again")
    shutil.rmtree(path, ignore_errors=True)
    shutil.copytree(HERE / "repo", path)
    git = ["git", "-C", str(path), "-c", "user.name=Ada", "-c", "user.email=ada@example.com"]
    subprocess.run([*git, "init", "-q", "-b", "main"], check=True)
    (path / MARK).write_text("A clone marketing/scene.py made, and removes on `scene down`.\n")
    subprocess.run([*git, "add", "-A"], check=True)
    subprocess.run([*git, "commit", "-q", "-m", "Tally"], check=True)
    if branch != "main":
        subprocess.run([*git, "checkout", "-q", "-b", branch], check=True)
    return path


def topology() -> dict:
    folders = {name: clone(name) for name in HOSTS}
    hosts = []
    for name in HOSTS:
        host = {
            "name": name,
            "lan": True,
            "bonjour": True,
            "account": "ada",
            "experiments": ["reviews"],
        }
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
    """The binaries the net runs, as `just env` builds them, with the probes
    a real provider runs behind; the phone app is `just ios build`'s."""
    subprocess.run(
        [str(ROOT / "scripts/bounded"), "1800", "cargo", "build", "--locked",
         "-p", "amux", "-p", "testnet", "-p", "provider-fakes", "-p", "qualification",
         "--bins"],
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


def block_to(pane: str, text: str) -> None:
    """Press j until the selected row holds `text`, where the client marks
    the row with a tinted block (a palette taken from the terminal) rather
    than a "›": the row under the block's half-filled top edge."""
    for _ in range(80):
        lines = screen(pane).splitlines()
        if any(line.strip().startswith("▄▄") and text in below for line, below in zip(lines, lines[1:])):
            return
        keys(pane, "j", settle=0.15)
    raise SystemExit(f"scene: never reached {text!r} in {pane}")


def shoot(pane: str, name: str, as_named: str | None = None) -> None:
    """A shot of `pane` kept as NAME-PANE, or as `as_named`."""
    taken = Path(output("tui", "shot", pane).splitlines()[0])
    IMAGES.mkdir(parents=True, exist_ok=True)
    kept = as_named or f"{name}-{pane}"
    shutil.copy(taken, IMAGES / f"{kept}.png")
    # The frame beside it is what compose.py redraws at twice the size.
    shutil.copy(taken.with_suffix(".frame.json"), IMAGES / f"{kept}.frame.json")
    print(f"scene: {kept}.png", flush=True)


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
    # The chat again in other people's terminals: the client takes its
    # palette from each one's colours.
    for palette in THEMES:
        tui("open", "studio", "--size", "84x26", "--palette", str(PALETTES / f"{palette}.json"), "--name", palette)
        time.sleep(2)
        block_to(palette, "checkout-redesign")
        keys(palette, "Enter", settle=2)
        shoot(palette, "theme")
        output("tui", "close", palette)
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


def containing(prefix: str, text: str) -> str:
    """The id of the element under `prefix` whose label holds `text`."""
    for _ in range(30):
        for line in output("phone", "query").splitlines():
            words = line.split()
            if words and words[0].startswith(prefix) and text in line:
                return words[0]
        time.sleep(1)
    raise SystemExit(f"scene: no {prefix} element holding {text!r} on the phone")


def store() -> int:
    """The phone's screens the App Store frames are composed from
    (`marketing/store.py`), dark: the fleet; flaky-sync-test's request;
    checkout-redesign's chat with the run that starts its subagents open;
    its review page with a comment; the new-agent sheet; and studio's page
    under Hosts. Needs the simulator lease."""
    ready()
    phone_paired()
    output("phone", "appearance", "dark")
    phone_shoot("phone-fleet-dark")
    output("phone", "tap", element("home.row.", "flaky-sync-test"))
    phone_shoot("phone-ask-dark")
    output("phone", "tap", "chat.back")
    output("phone", "tap", element("home.row.", "checkout-redesign"))
    # The first run in its chat is the one that starts its two subagents.
    output("phone", "tap", "chat.row.run")
    output("phone", "scroll", "top")
    phone_shoot("phone-chat-dark")
    output("phone", "tap", "chat.changes")
    output("phone", "perform", containing("review.line.", "MissingCard"), "Select line")
    output("phone", "tap", "review.comment")
    output("phone", "type", "Answer 422 with a message the page can show, not a 500.", "--into", "review.note")
    output("phone", "tap", "review.add")
    phone_shoot("phone-review-dark")
    output("phone", "tap", "chat.back")
    output("phone", "tap", "chat.back")
    output("phone", "tap", "home.newAgent")
    phone_shoot("phone-new-dark")
    output("phone", "tap", "new-agent.cancel")
    output("phone", "choose", "Hosts")
    output("phone", "tap", element("hosts.row.", "studio"))
    phone_shoot("phone-host-dark")
    return 0


def android_store(skip_ready: bool = False) -> int:
    """The Android phone's screens the Google Play frames are composed from
    (`marketing/play_store.py`), dark, captured on the pinned Pixel 9 emulator
    against this exact Tally scene."""
    if not skip_ready:
        ready()
    record = json.loads((FOLDER / ".testnet/net.json").read_text())
    readiness = record["readiness"]
    scope = record.get("scope", "")
    apk = ROOT / "apps/android/app/build/outputs/apk/debug/app-debug.apk"
    if not apk.is_file():
        raise SystemExit(f"scene: missing debug APK at {apk}; run `just android build` first")

    sys.path.insert(0, str(ROOT / "apps/android/tools"))
    import android_sdk  # noqa: E402
    sdk_root = android_sdk.require_sdk()
    adb = str(sdk_root / "platform-tools/adb")
    serial = subprocess.run(
        [str(ROOT / "scripts/py"), "-B", str(ROOT / "apps/android/tools/android-emulator.py"), "--serial"],
        cwd=ROOT, check=True, capture_output=True, text=True,
    ).stdout.strip()
    if not serial:
        booted = subprocess.run(
            [str(ROOT / "scripts/py"), "-B", str(ROOT / "apps/android/tools/android-emulator.py")],
            cwd=ROOT, check=True, capture_output=True, text=True,
        ).stdout
        m = re.search(r"android \(amux-android-1\):\s+(\S+)", booted)
        if not m:
            raise SystemExit(f"scene: could not parse emulator serial from:\n{booted}")
        serial = m.group(1)

    def adb_run(*args: str, check: bool = True, timeout: int = 60) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [adb, "-s", serial, *args],
            check=check, capture_output=True, text=True, timeout=timeout,
        )

    def rewrite_host(addr: str) -> str:
        return addr.replace("127.0.0.1:", "10.0.2.2:").replace("localhost:", "10.0.2.2:")

    def free_port() -> int:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.bind(("127.0.0.1", 0))
            return s.getsockname()[1]

    door_port = free_port()
    door_token = str(uuid.uuid4())
    cloud_url = readiness.get("cloud_url")
    cloud_port = None
    if cloud_url:
        m = re.search(r":(\d+)", cloud_url)
        if m:
            cloud_port = int(m.group(1))

    def door_call(payload: dict, timeout: float = 30.0) -> dict:
        body = dict(payload)
        body["door_token"] = door_token
        raw = (json.dumps(body) + "\n").encode("utf-8")
        with socket.create_connection(("127.0.0.1", door_port), timeout=timeout) as sock:
            sock.settimeout(timeout)
            sock.sendall(raw)
            buf = bytearray()
            while b"\n" not in buf:
                chunk = sock.recv(65536)
                if not chunk:
                    break
                buf.extend(chunk)
        line = bytes(buf).split(b"\n", 1)[0].decode("utf-8")
        if not line:
            raise RuntimeError(f"empty reply from Android door for {payload.get('kind')}")
        reply = json.loads(line)
        if reply.get("kind") == "error":
            raise RuntimeError(f"Android door refused {payload.get('kind')}: {reply.get('message')}")
        return reply

    def query_elements() -> list[dict]:
        return door_call({"kind": "query"})["state"]["elements"]

    def find_element(prefix: str, text: str, timeout: float = 30.0) -> dict:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            for el in query_elements():
                ident = el.get("identifier", "")
                label = el.get("label") or ""
                value = el.get("value") or ""
                if ident.startswith(prefix) and (text in label or text in value):
                    return el
            time.sleep(0.5)
        raise SystemExit(f"scene: no Android element starting with {prefix!r} holding {text!r}")

    def wait_tag(identifier: str, timeout: float = 30.0) -> dict:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            for el in query_elements():
                if el.get("identifier") == identifier and el.get("enabled", True):
                    return el
            time.sleep(0.5)
        raise SystemExit(f"scene: Android element {identifier!r} never appeared")

    def tap(identifier: str) -> None:
        wait_tag(identifier)
        door_call({"kind": "tap", "identifier": identifier})
        door_call({"kind": "settle"})

    def type_into(identifier: str, text: str) -> None:
        wait_tag(identifier)
        door_call({"kind": "type", "identifier": identifier, "text": text})
        door_call({"kind": "settle"})

    def back() -> None:
        els = {el.get("identifier") for el in query_elements()}
        if "nav.back" in els:
            tap("nav.back")
        elif "review.back" in els:
            tap("review.back")
        elif "new-agent.back" in els:
            tap("new-agent.back")
        else:
            door_call({"kind": "back"})
            door_call({"kind": "settle"})

    saved_elements: dict[str, list[dict]] = {}

    def android_shoot(name: str) -> None:
        door_call({"kind": "settle"})
        time.sleep(0.6)
        saved_elements[name] = query_elements()
        IMAGES.mkdir(parents=True, exist_ok=True)
        out = IMAGES / f"android-{name}-dark.png"
        png = subprocess.run(
            [adb, "-s", serial, "exec-out", "screencap", "-p"],
            check=True, capture_output=True, timeout=30,
        ).stdout
        if not png:
            raise SystemExit(f"scene: empty adb screencap for {name}")
        out.write_bytes(png)
        print(f"scene: {out.name} ({len(png)} bytes)", flush=True)

    def start_host_pairing(host_name: str, config_path: str) -> tuple[str, subprocess.Popen[str]]:
        amux_bin = ROOT / "target/debug/amux"
        proc = subprocess.Popen(
            [str(amux_bin), "--config", config_path, "pair"],
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
        )
        deadline = time.monotonic() + 30
        lines: list[str] = []
        assert proc.stdout is not None
        while time.monotonic() < deadline:
            line = proc.stdout.readline()
            if not line:
                if proc.poll() is not None:
                    break
                time.sleep(0.1)
                continue
            lines.append(line)
            m = re.search(r"\b(\d{3})[\s-]?(\d{3})\b", line)
            if m:
                return m.group(1) + m.group(2), proc
        proc.terminate()
        raise SystemExit(f"scene: no pairing PIN from {host_name}: {''.join(lines)}")

    try:
        # Clean status bar (9:41, 100% battery, 5G + full Wi-Fi, no stray icons).
        adb_run("shell", "settings", "put", "global", "sysui_demo_allowed", "1", check=False)
        adb_run("shell", "am", "broadcast", "-a", "com.android.systemui.demo", "-e", "command", "enter", check=False)
        adb_run("shell", "am", "broadcast", "-a", "com.android.systemui.demo", "-e", "command", "clock", "-e", "hhmm", "0941", check=False)
        adb_run("shell", "am", "broadcast", "-a", "com.android.systemui.demo", "-e", "command", "battery", "-e", "level", "100", "-e", "plugged", "false", check=False)
        adb_run("shell", "am", "broadcast", "-a", "com.android.systemui.demo", "-e", "command", "network", "-e", "wifi", "show", "-e", "level", "4", "-e", "fully", "true", check=False)
        adb_run("shell", "am", "broadcast", "-a", "com.android.systemui.demo", "-e", "command", "network", "-e", "mobile", "show", "-e", "datatype", "5g", "-e", "level", "4", "-e", "fully", "true", check=False)
        adb_run("shell", "am", "broadcast", "-a", "com.android.systemui.demo", "-e", "command", "notifications", "-e", "visible", "false", check=False)

        adb_run("shell", "am", "force-stop", "sh.amux.android", check=False)
        adb_run("install", "-r", "-t", str(apk), timeout=180)
        adb_run("shell", "pm", "clear", "sh.amux.android")
        adb_run("forward", f"tcp:{door_port}", "tcp:19876")
        if cloud_port:
            adb_run("reverse", f"tcp:{cloud_port}", f"tcp:{cloud_port}")

        start_config: dict = {
            "device_name": "Pixel 9",
            "discovery_scope": scope,
            "lan": True,
            "lan_bind": "0.0.0.0:0",
            "scripted_cloud": False,
            "use_system_discovery": False,
            "fresh_start": True,
            "notifications_dismissed": True,
        }
        if cloud_url:
            start_config["cloud_url"] = rewrite_host(cloud_url)
        if readiness.get("relay_tcp"):
            start_config["relay_tcp"] = rewrite_host(readiness["relay_tcp"])
        if readiness.get("relay_quic"):
            start_config["relay_quic"] = rewrite_host(readiness["relay_quic"])
        if readiness.get("relay_root"):
            start_config["relay_root"] = readiness["relay_root"]

        def sh_quote(s: str) -> str:
            return "'" + s.replace("'", "'\\''") + "'"

        launch_cmd = (
            f"am start -W -n sh.amux.android/.MainActivity "
            f"--ei amux_door_port 19876 "
            f"--es amux_door_token {sh_quote(door_token)} "
            f"--es amux_start_config {sh_quote(json.dumps(start_config))}"
        )
        adb_run("shell", launch_cmd, timeout=60)

        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            try:
                door_call({"kind": "ready"}, timeout=5.0)
                break
            except Exception:
                time.sleep(0.3)
        else:
            raise SystemExit("scene: Android debug door never opened")

        door_call({"kind": "awaitReconciled", "seconds": 30.0}, timeout=40.0)
        door_call({"kind": "appearance", "appearance": "dark"})

        # Inject LAN discovery for studio, build, laptop and pair each.
        hosts_meta = readiness["hosts"]
        found = [
            {
                "host_id": h["host_id"],
                "name": h["name"],
                "addrs": [f"10.0.2.2:{h['lan_port']}"],
                "scope": scope,
                "pairing_open": True,
            }
            for h in hosts_meta
        ]
        door_call({"kind": "discovered", "found": found})
        door_call({"kind": "settle"})
        for h in hosts_meta:
            pin, proc = start_host_pairing(h["name"], h["config"])
            try:
                pair_deadline = time.monotonic() + 20.0
                while True:
                    try:
                        door_call({"kind": "pairByCode", "host": h["host_id"], "pin": pin}, timeout=30.0)
                        break
                    except RuntimeError as exc:
                        if "no host named" in str(exc) and time.monotonic() < pair_deadline:
                            door_call({"kind": "discovered", "found": found})
                            time.sleep(0.5)
                            continue
                        raise
                print(f"scene: paired Android with {h['name']}", flush=True)
            finally:
                proc.terminate()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()

        if cloud_url:
            door_call({
                "kind": "connect",
                "relay": rewrite_host(cloud_url),
                "token": "refresh-ada",
                "user": "ada",
                "tier": "pro",
            }, timeout=30.0)
            door_call({"kind": "awaitReconciled", "seconds": 30.0}, timeout=40.0)

        # Wait until fleet shows checkout-redesign and flaky-sync-test.
        find_element("home.row.", "flaky-sync-test", timeout=45.0)
        find_element("home.row.", "checkout-redesign", timeout=45.0)

        # Expand checkout-redesign's subagent family.
        tap("home.family.checkout-redesign")
        android_shoot("fleet")

        # 2: flaky-sync-test ask card.
        flaky = find_element("home.row.", "flaky-sync-test")
        tap(flaky["identifier"])
        time.sleep(1.0)
        android_shoot("ask")

        # 3: checkout-redesign chat with subagent run expanded.
        back()
        checkout = find_element("home.row.", "checkout-redesign")
        tap(checkout["identifier"])
        tap("chat.row.run")
        # Scroll to top so the prompt + two subagent rows are both visible.
        adb_run("shell", "input", "swipe", "540", "650", "540", "1950", "300", check=False)
        time.sleep(0.8)
        adb_run("shell", "input", "swipe", "540", "650", "540", "1950", "300", check=False)
        time.sleep(0.8)
        android_shoot("chat")

        # 4: review diff with a comment on MissingCard.
        tap("chat.changes")
        missing_card = None
        for attempt in range(4):
            try:
                missing_card = find_element("review.line.", "MissingCard", timeout=15.0 if attempt == 0 else 4.0)
                break
            except SystemExit:
                adb_run("shell", "input", "swipe", "540", "1750", "540", "950", "300", check=False)
                time.sleep(0.5)
        if missing_card is None:
            raise SystemExit("scene: no Android review.line.* element holding 'MissingCard'")
        tap(missing_card["identifier"])
        tap("review.commentOn")
        type_into("review.note", "Answer 422 with a message the page can show, not a 500.")
        tap("review.add")
        android_shoot("review")

        # 5: new agent sheet (select build + Codex to match iOS frame 5).
        back()
        back()
        tap("home.newAgent")
        # Pick `build` host if available, then Codex provider.
        els = query_elements()
        if any(e.get("identifier") == "new-agent.host" for e in els):
            tap("new-agent.host")
            try:
                build_opt = find_element("new-agent.host.", "build", timeout=5.0)
                tap(build_opt["identifier"])
            except SystemExit:
                back()
        els = query_elements()
        if any(e.get("identifier") == "new-agent.provider" for e in els):
            tap("new-agent.provider")
            try:
                codex_opt = find_element("new-agent.provider.", "Codex", timeout=5.0)
                tap(codex_opt["identifier"])
            except SystemExit:
                back()
        android_shoot("new")

        # 6: studio host detail page.
        back()
        tap("tab.hosts")
        studio = find_element("hosts.row.", "studio")
        tap(studio["identifier"])
        android_shoot("host")

        (IMAGES / "android-store-elements.json").write_text(json.dumps(saved_elements, indent=2))
        print(f"scene: saved 6 Android screenshots in {IMAGES}", flush=True)
        return 0
    finally:
        adb_run("shell", "am", "broadcast", "-a", "com.android.systemui.demo", "-e", "command", "exit", check=False)
        adb_run("forward", "--remove", f"tcp:{door_port}", check=False)
        if cloud_port:
            adb_run("reverse", "--remove", f"tcp:{cloud_port}", check=False)


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
    its subagents, a code review comment typed into the review page, and the
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


# The agents whose own interfaces are photographed: real Claude Code in its
# terminal and real Codex, on studio, each asked what Tally's few files
# answer whole, by reading, so neither stops to ask before it is
# photographed.
NATIVE = [
    ("error-copy", "claude_pty", "sonnet",
     "Which three messages in web/src/checkout/messages.ts leave the client stuck? Suggest a clearer "
     "one for each, as a short list; don't change the file yet."),
    ("ledger-race", "codex", "gpt-6-astra",
     "Is there a race in Ledger::apply in crates/sync/src/ledger.rs? Explain in three sentences; "
     "don't change any files."),
]
# Codex runs here with a scratch home, so it writes the folder in full: a
# shared one, rather than one under this account's name.
NATIVE_CLONE = Path("/Users/Shared/tally")


def native() -> int:
    """Photographs NATIVE's agents in their own interfaces, reached the way
    a person reaches them, with `amux attach` from the terminal client."""
    folder = clone_at(NATIVE_CLONE, "main")
    topology = {
        "hosts": [{"name": "studio", "lan": True, "bonjour": True, "provider": "real"}],
        "agents": [
            {"name": name, "host": "studio", "kind": kind, "model": model, "cwd": str(folder), "prompt": prompt}
            for name, kind, model, prompt in NATIVE
        ],
    }
    path = FOLDER / "native.json"
    path.write_text(json.dumps(topology, indent=2))
    if ctl("up", str(path)):
        return 1
    tui("open", "studio", "--size", "80x30", "--palette", str(PALETTES / "studio.json"), "--name", "native")
    time.sleep(2)
    panes = {name: json.loads(output("--json", "tui", "attach", "native", name))["pane"] for name, *_ in NATIVE}
    deadline = time.monotonic() + 900
    while time.monotonic() < deadline:
        # Claude asks once whether to trust a folder it has not seen, as it
        # would ask the person; the scene says yes.
        for name, pane in panes.items():
            if "Yes, I trust this folder" in screen(pane):
                keys(pane, "Down")
                keys(pane, "Enter")
        fleet = output("fleet", "studio").splitlines()
        rows = {line.split()[0]: line for line in fleet if line and not line.startswith(" ")}
        if all("IDLE" in rows.get(name, "") for name, *_ in NATIVE):
            break
        time.sleep(5)
    else:
        raise SystemExit("scene: the agents never finished")
    time.sleep(4)
    for name, kind, *_ in NATIVE:
        shoot(panes[name], "native", as_named=f"native-{kind}")
        output("tui", "close", panes[name])
    output("tui", "close", "native")
    return 0


def main(arguments: list[str]) -> int:
    verb = arguments[0] if arguments else "up"
    reuse = "--reuse" in arguments[1:]
    FOLDER.mkdir(parents=True, exist_ok=True)
    if verb == "android-store" and reuse:
        return android_store(skip_ready=True)
    if verb in ("up", "shots", "film", "store", "android-store"):
        if verb == "android-store":
            subprocess.run(
                [str(ROOT / "scripts/bounded"), "1800", "cargo", "build", "--locked",
                 "-p", "amux", "-p", "testnet", "-p", "provider-fakes", "--bins"],
                cwd=ROOT, check=True,
            )
        else:
            build()
        ctl("down")
        path = FOLDER / "topology.json"
        path.write_text(json.dumps(topology(), indent=2))
        print(f"scene: {FOLDER}", flush=True)
        if ctl("up", str(path)):
            return 1
        return {
            "shots": shots,
            "film": film,
            "store": store,
            "android-store": android_store,
        }.get(verb, lambda: 0)()
    if verb == "native":
        build()
        ctl("down")
        return native()
    if verb == "down":
        code = ctl("down")
        if (NATIVE_CLONE / MARK).exists():
            shutil.rmtree(NATIVE_CLONE)
        for folder, _ in HOSTS.values():
            if (HOME / folder / MARK).exists():
                shutil.rmtree(HOME / folder)
            parent = (HOME / folder).parent
            if parent != HOME and parent.exists() and not any(parent.iterdir()):
                parent.rmdir()
        return code
    print("usage: scene.py up|shots|film|store|android-store|native|down", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
