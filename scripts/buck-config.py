#!/usr/bin/env python3
"""Write this worktree's .buckconfig.local: where this machine's toolchains are.

An action's cache key covers its command line and declared inputs, not the
contents of a compiler it calls by path. So every toolchain is named here by
a path that changes with its version (rustup's toolchain directories carry
the version), and Xcode, whose path doesn't, is named by its version and
build in a platform property every Mac action carries. A toolchain upgrade
therefore changes every key it should.

scripts/buck2 runs this before every command. It rewrites the file when
rust-toolchain.toml, the Nix pins, build/tools.json or the worker's core
count is newer than it, so the file follows the pins without anyone
remembering to.

It also says where actions go. A Mac whose NativeLink is set up with a
remote (`scripts/nativelink.py install --remote ...`, normally nova) sends
Linux actions to the remote's worker through its own NativeLink, whose
cache is a local layer over the remote's. When the remote doesn't answer,
NativeLink is restarted without it and the tree turns Linux off
(`amux.linux = false`: no Linux execution platform, and `nova_only` tests
skipped), and back when it answers again. Buck2's daemon remembers what it
uploaded to the cache it was talking to, so a switch also restarts this
tree's daemon.
"""

import json
import os
import platform
import socket
import re
import shutil
import subprocess
import sys
import time
import tomllib
from pathlib import Path
from urllib.parse import urlparse

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / ".buckconfig.local"
NATIVELINK = Path.home() / "Library/Caches/amux/nativelink"
WORKER_CORES = NATIVELINK / "cores"
# What scripts/nativelink.py was installed with (its remote), and the last
# answer from that remote, shared by every tree on the machine.
SETTINGS = NATIVELINK / "settings.json"
REACHABLE = NATIVELINK / "reachable"
REACHABLE_SECONDS = 30
# Whether NativeLink is running in front of the remote (`remote`) or without
# it (`local`); scripts/nativelink.py's start script writes it.
SERVING = NATIVELINK / "serving"
INPUTS = [ROOT / "rust-toolchain.toml", ROOT / "build/tools.json", ROOT / "flake.lock", ROOT / "nix/toolchain.nix", Path(__file__), WORKER_CORES]


def run(*args: str, env: dict | None = None) -> str:
    return subprocess.check_output(args, text=True, env=env).strip()


def sysroot(channel: str) -> str:
    """The pinned stable toolchain's directory: rustup's for the channel, or,
    inside `nix develop` where there is no rustup, the shell's own, which
    nix/toolchain.nix reads from the same rust-toolchain.toml."""
    if shutil.which("rustup") is None:
        try:
            release = re.search(r"^release: (.+)$", run("rustc", "-vV"), re.M)
            if release and release.group(1) == channel:
                return run("rustc", "--print", "sysroot")
        except (OSError, subprocess.CalledProcessError):
            pass
        sys.exit(f"buck-config: no rustup, and no rustc {channel} on PATH; run it inside `nix develop`")
    env = dict(os.environ, RUSTUP_TOOLCHAIN=channel)
    try:
        return run("rustc", "--print", "sysroot", env=env)
    except (OSError, subprocess.CalledProcessError):
        sys.exit(f"buck-config: no Rust toolchain {channel}; run `rustup toolchain install {channel}`")


def worker_cores() -> int:
    cores = WORKER_CORES.read_text().strip() if WORKER_CORES.exists() else ""
    return int(cores) if cores.isdigit() else max(1, (os.cpu_count() or 4) - 3)


def remote() -> str | None:
    try:
        return json.loads(SETTINGS.read_text()).get("remote")
    except (OSError, ValueError):
        return None


def answers(url: str) -> bool:
    """Whether the remote's NativeLink takes a connection, remembered for a
    while so a recipe's many Buck2 commands don't each wait on a timeout."""
    try:
        when, was = REACHABLE.read_text().split()
        if time.time() - float(when) < REACHABLE_SECONDS:
            return was == "yes"
    except (OSError, ValueError):
        pass
    address = urlparse(url)
    try:
        with socket.create_connection((address.hostname, address.port or 50051), timeout=2):
            up = True
    except OSError:
        up = False
    try:
        REACHABLE.write_text(f"{time.time()} {'yes' if up else 'no'}\n")
    except OSError:
        pass
    return up


def mode() -> str:
    """`linux`: a Linux machine, whose actions all run on Linux. On a Mac,
    `remote`: Linux actions go to the remote's worker; `local`: a remote is
    set up but doesn't answer, or the address its worker registers at isn't
    this machine's (the tailnet is down); `solo`: none is set up. Only
    `remote` and `linux` run Linux actions."""
    if platform.system() != "Darwin":
        return "linux"
    url = remote()
    if url is None:
        return "solo"
    return "remote" if workers_address_is_ours() and answers(url) else "local"


def serving() -> tuple[str | None, float]:
    try:
        return SERVING.read_text().strip(), SERVING.stat().st_mtime
    except OSError:
        return None, 0.0


def restart_nativelink(now: str) -> None:
    """Start NativeLink again so its start script picks the rendering that
    fits (with the remote or without), and wait until it answers."""
    subprocess.run(["launchctl", "kickstart", "-k", f"gui/{os.getuid()}/sh.amux.nativelink"], capture_output=True)
    deadline = time.time() + 20
    while time.time() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", 50051), timeout=1):
                break
        except OSError:
            time.sleep(0.2)
    print(
        "buck-config: restarted NativeLink "
        + ("in front of the remote; Linux actions wait until its worker registers (up to a minute)" if now == "remote" else "without the remote"),
        file=sys.stderr,
    )


def workers_address_is_ours() -> bool:
    try:
        host = json.loads(SETTINGS.read_text())["workers"].rsplit(":", 1)[0]
    except (OSError, ValueError, KeyError):
        return False
    return f"inet {host} " in subprocess.run(["/sbin/ifconfig"], capture_output=True, text=True).stdout


def current_mode() -> str | None:
    try:
        for line in OUT.read_text().splitlines():
            if line.startswith("# mode: "):
                return line.removeprefix("# mode: ")
    except OSError:
        pass
    return None


def stale() -> bool:
    if not OUT.exists():
        return True
    written = OUT.stat().st_mtime
    return any(path.exists() and path.stat().st_mtime > written for path in INPUTS)


def write(mode: str) -> None:
    channel = tomllib.loads((ROOT / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    lines = [
        "# Written by scripts/buck-config.py; not committed. It is rewritten",
        "# when the pinned toolchains change, or where actions go does.",
        f"# mode: {mode}",
        "[rust]",
        f"  toolchain_dir = {sysroot(channel)}",
    ]
    lines += [
        # The cores this machine's NativeLink worker offers
        # (scripts/nativelink.py), all of which a test that uses every core
        # reserves; and whether a Linux worker is in reach.
        "[amux]",
        f"  worker_cores = {worker_cores()}",
        f"  linux = {'true' if mode in ('linux', 'remote') else 'false'}",
    ]
    host = {"Darwin": "macos", "Linux": "linux", "Windows": "windows"}[platform.system()]
    lines += [
        # A target that doesn't name its own platform is a development
        # build of the desktop product for this machine.
        "[parser]",
        "  target_platform_detector_spec = "
        f"target:root//...->root//build/platforms:{host}-cli-shipping-dev "
        "target:prelude//...->prelude//platforms:default "
        "target:toolchains//...->prelude//platforms:default",
    ]
    if platform.system() == "Darwin":
        version = run("xcodebuild", "-version").replace("\n", " ")
        lines += [
            "[apple]",
            f"  xcode = {version}",
        ]
    OUT.write_text("\n".join(lines) + "\n")


def skipped(patterns: list[str]) -> None:
    """With no Linux worker in reach, name the `nova-only` targets among
    `patterns` that a run therefore skipped."""
    if mode() in ("linux", "remote"):
        return
    buck2 = run(str(ROOT / "scripts/py"), str(ROOT / "scripts/buck-tools.py"), "path", "buck2")
    query = subprocess.run(
        [buck2, "uquery", f"attrfilter(labels, nova-only, set({' '.join(patterns)}))"],
        cwd=ROOT, capture_output=True, text=True,
    )
    targets = query.stdout.split()
    if targets:
        why = f"{remote()} is out of reach" if remote() else "no Linux worker is set up (scripts/nativelink.py install --remote)"
        print(f"skipped, since {why}: {' '.join(targets)} (nova-only: they run only on a Linux worker)", file=sys.stderr)


def main() -> None:
    if sys.argv[1:2] == ["skipped"]:
        skipped(sys.argv[2:])
        return
    was = current_mode()
    now = mode()
    # NativeLink serves every tree on the machine, so whichever tree sees
    # the change first restarts it; not again within a minute of a start,
    # in case its start script and this disagree about the remote.
    running, since = serving()
    if now in ("remote", "local") and running is not None and running != now and time.time() - since > 60:
        restart_nativelink(now)
    if now != was or stale():
        write(now)
    if was is not None and now != was:
        # The running daemon still holds the old endpoints.
        buck2 = run(str(ROOT / "scripts/py"), str(ROOT / "scripts/buck-tools.py"), "path", "buck2")
        subprocess.run([buck2, "kill"], cwd=ROOT, capture_output=True)
        print(f"buck-config: actions now go {DESCRIBE[now]}; restarted this tree's Buck2 daemon", file=sys.stderr)
    if now == "local":
        print(f"buck-config: {remote()} is out of reach: Linux actions are off and nova-only tests are skipped", file=sys.stderr)


DESCRIBE = {
    "linux": "to this machine's NativeLink",
    "remote": "to this Mac's worker, and Linux ones to the remote's",
    "local": "to this Mac's worker only (the remote doesn't answer)",
    "solo": "to this Mac's worker only",
}


if __name__ == "__main__":
    main()
