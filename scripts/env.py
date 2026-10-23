"""Start a named world of real agents and hold it until interrupted.

`just env NAME` reads `environments/NAME.json`, a topology file whose
`clients` section says which clients the world comes with, and starts it with
`testnet ctl up` in its own session folder,
`~/.local/state/amux-env/<tree>/NAME`. Any net left
there by an earlier start is taken down first, so every start is fresh. The
phone, when the world has one, runs on a simulator leased for as long as the
world is up; Ctrl-C takes the net down and gives the simulator back.

Everything after the start is `testnet ctl`, run from the session folder in
another terminal: add a host, spawn an agent, sever a link, pair the phone
with another host.
"""

from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ENVIRONMENTS = ROOT / "environments"
TESTNET = ROOT / "target/debug/testnet"
# Outside the checkout, as a QA run's nets are: inside it, git would find
# this repository above every agent's folder, and a real agent's git would
# work on it.
FOLDERS = Path.home() / ".local/state/amux-env" / ROOT.name


def say(text: str) -> None:
    print(f"env: {text}", flush=True)


def worlds() -> list[str]:
    return sorted(path.stem for path in ENVIRONMENTS.glob("*.json"))


def has_phone(world: Path) -> bool:
    return bool((json.loads(world.read_text()).get("clients") or {}).get("phone"))


def build(phone: bool) -> None:
    """The binaries a QA run builds, and the phone app when it is wanted."""
    say("building the binaries")
    subprocess.run(
        [str(ROOT / "scripts/bounded"), "1800", "cargo", "build", "--locked",
         "-p", "amux", "-p", "testnet", "--bins", "--features", "amux/bundled"],
        cwd=ROOT, check=True,
    )
    if phone:
        say("building the phone app")
        subprocess.run(["just", "ios", "build"], cwd=ROOT, check=True)


def ctl(folder: Path, *arguments: str, quiet: bool = False) -> int:
    command = [str(ROOT / "scripts/bounded"), "900", str(TESTNET), "ctl", *arguments]
    output = subprocess.DEVNULL if quiet else None
    return subprocess.run(command, cwd=folder, stdout=output, stderr=output).returncode


def guard(name: str) -> None:
    """A process of its own session that takes the net down once this one
    is gone, however it went. A Ctrl-C reaches every process in the
    terminal's job, and the wrappers holding the lease end this one before
    it can clean up after itself."""
    subprocess.Popen(
        [sys.executable, "-B", str(Path(__file__).resolve()), "--guard", str(os.getpid()), name],
        start_new_session=True,
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )


def guarding(watched: int, name: str) -> int:
    while True:
        try:
            os.kill(watched, 0)
        except ProcessLookupError:
            break
        time.sleep(1)
    return ctl(FOLDERS / name, "down", quiet=True)


def hold() -> None:
    """Until Ctrl-C, a termination, or the end of whatever started this: a
    `wt` holding the lease that is gone has given the simulator back."""
    stopping = []
    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, lambda *_: stopping.append(True))
    parent = os.getppid()
    while not stopping and os.getppid() == parent:
        time.sleep(1)


def main(arguments: list[str]) -> int:
    if len(arguments) == 3 and arguments[0] == "--guard":
        return guarding(int(arguments[1]), arguments[2])
    if len(arguments) != 1 or arguments[0] not in worlds():
        print(f"usage: just env NAME, one of: {', '.join(worlds())}", file=sys.stderr)
        return 2
    name = arguments[0]
    world = ENVIRONMENTS / f"{name}.json"
    phone = has_phone(world)
    # The lease is taken once the build is done, for as long as the world
    # is up: a simulator given back while the app still runs on it is free
    # for another tree to take.
    if phone and not os.environ.get("WT_LEASE_IPHONE"):
        build(phone)
        os.environ["AMUX_ENV_BUILT"] = "1"
        script = [str(ROOT / "scripts/py"), "-B", str(Path(__file__).resolve()), name]
        os.execv(str(ROOT / "scripts/with"), [str(ROOT / "scripts/with"), "iphone", "--", *script])
    if not os.environ.get("AMUX_ENV_BUILT"):
        build(phone)
    folder = FOLDERS / name
    folder.mkdir(parents=True, exist_ok=True)
    # A net an earlier start left behind goes first.
    ctl(folder, "down", quiet=True)
    guard(name)
    say(f"starting {name}")
    try:
        if ctl(folder, "up", str(world)) != 0:
            return 1
        print(
            f"\n{name} is up. Drive it from another terminal:\n"
            f"  cd {folder}\n"
            f"  {TESTNET} ctl status\n"
            "Ctrl-C here takes it down.",
            flush=True,
        )
        hold()
        return 0
    except KeyboardInterrupt:
        return 130
    finally:
        # Whatever ended it, the net goes with it, from a fresh signal
        # disposition so a second Ctrl-C cannot cut the shutdown short.
        for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            signal.signal(sig, signal.SIG_IGN)
        say(f"taking {name} down")
        ctl(folder, "down", quiet=True)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
