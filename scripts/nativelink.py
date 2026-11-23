#!/usr/bin/env python3
"""Run this Mac's NativeLink: the build cache and worker Buck2 sends every
action to (docs/BUILD.md, "The build cache").

    nativelink.py install [--cores N] [--background]
        Write the config and a launchd agent, and (re)start it.
    nativelink.py uninstall
        Stop it and remove the agent. The cache directory is kept.
    nativelink.py status
        Say whether it answers on 127.0.0.1:50051.
    nativelink.py run [--cores N] [--background]
        Run it in the foreground, for debugging.

`--cores` is how many cores the worker offers (default: the machine's
count less three), so builds never take the whole laptop. `--background`
runs every action at macOS background priority (`taskpolicy -b`: mostly
efficiency cores, yielding to foreground apps); it is off by default.
One NativeLink serves every worktree on the machine.
"""

import argparse
import os
import plistlib
import socket
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TEMPLATE = ROOT / "build/nativelink/mac.json5"
LABEL = "sh.amux.nativelink"
CACHE = Path.home() / "Library/Caches/amux/nativelink"
CONFIG = CACHE / "config.json5"
PLIST = Path.home() / f"Library/LaunchAgents/{LABEL}.plist"
LOG = Path.home() / "Library/Logs/amux/nativelink.log"
GIB = 1024**3


def binary() -> str:
    return subprocess.check_output(
        [str(ROOT / "scripts/py"), str(ROOT / "scripts/buck-tools.py"), "path", "nativelink"],
        text=True,
    ).strip()


def tool_bin() -> Path:
    """A directory holding only the tools actions find by name beyond the
    operating system's own: python3, which the prelude's helper scripts
    call and which must be 3.12 or newer, while macOS's /usr/bin/python3
    is 3.9 (the interpreter scripts/py picks), and tmux."""
    bin = CACHE / "bin"
    bin.mkdir(parents=True, exist_ok=True)
    python = subprocess.check_output(
        [str(ROOT / "scripts/py"), "-c", "import sys, os; print(os.path.realpath(sys.executable))"],
        text=True,
    ).strip()
    tools = {"python3": python}
    # tmux, for the harness's terminal sessions (testnet's `ctl tui`), as
    # the tests find it on a developer's PATH under Cargo.
    if tmux := shutil.which("tmux"):
        tools["tmux"] = os.path.realpath(tmux)
    for name, target in tools.items():
        link = bin / name
        if not link.is_symlink() or os.readlink(link) != target:
            link.unlink(missing_ok=True)
            link.symlink_to(target)
    return bin


def render(cores: int, background: bool) -> None:
    # The cores the worker offers, for scripts/buck-config.py: a test that
    # uses every core reserves them all (build/toolchains/BUCK).
    CACHE.mkdir(parents=True, exist_ok=True)
    (CACHE / "cores").write_text(f"{cores}\n")
    values = {
        "tool_bin": str(tool_bin()),
        "cache": str(CACHE),
        "cas_bytes": str(40 * GIB),
        "ac_bytes": str(4 * GIB),
        "cores": str(cores),
        # Three quarters of the machine's memory, in KiB.
        "memory_kb": str(memory_bytes() * 3 // 4 // 1024),
        "entrypoint": 'entrypoint: "/usr/sbin/taskpolicy -b",' if background else "",
    }
    text = TEMPLATE.read_text()
    for key, value in values.items():
        text = text.replace("{{" + key + "}}", value)
    CACHE.mkdir(parents=True, exist_ok=True)
    CONFIG.write_text(text)


def memory_bytes() -> int:
    return int(subprocess.check_output(["sysctl", "-n", "hw.memsize"], text=True))


def answering() -> bool:
    try:
        with socket.create_connection(("127.0.0.1", 50051), timeout=1):
            return True
    except OSError:
        return False


def launchctl(*args: str, check: bool = True) -> None:
    subprocess.run(["launchctl", *args], check=check, capture_output=not check)


def install(cores: int, background: bool) -> None:
    render(cores, background)
    LOG.parent.mkdir(parents=True, exist_ok=True)
    PLIST.parent.mkdir(parents=True, exist_ok=True)
    plist = {
        "Label": LABEL,
        "ProgramArguments": [binary(), str(CONFIG)],
        "RunAtLoad": True,
        "KeepAlive": True,
        "StandardOutPath": str(LOG),
        "StandardErrorPath": str(LOG),
        "EnvironmentVariables": {"RUST_LOG": "warn"},
        # NativeLink drains on SIGTERM; a restart shouldn't wait for that.
        "ExitTimeOut": 2,
    }
    PLIST.write_bytes(plistlib.dumps(plist))
    domain = f"gui/{os.getuid()}"
    stop(domain)
    launchctl("bootstrap", domain, str(PLIST))
    print(f"nativelink: running under launchd as {LABEL}, {cores} cores, log {LOG}")
    # macOS's Developer Tools exemption belongs to one binary, and without
    # it Gatekeeper scans every binary a build links before it first runs
    # (docs/BUILD.md, "The build cache").
    exempted = CACHE / "exempted-binary"
    if not exempted.exists() or exempted.read_text() != binary():
        print(
            "nativelink: a new nativelink binary. Add it under System Settings > Privacy & Security >"
            f" Developer Tools ({binary()}), then run this again so it starts exempt.",
        )
        exempted.write_text(binary())


def stop(domain: str) -> None:
    """Stop the agent and wait until launchd has let it go: bootout returns
    before the service is gone, and bootstrapping it again before then fails."""
    launchctl("bootout", f"{domain}/{LABEL}", check=False)
    for _ in range(100):
        loaded = subprocess.run(
            ["launchctl", "print", f"{domain}/{LABEL}"], capture_output=True
        ).returncode == 0
        if not loaded:
            return
        time.sleep(0.1)
    sys.exit(f"nativelink: launchd still holds {LABEL} after 10 s")


def uninstall() -> None:
    stop(f"gui/{os.getuid()}")
    PLIST.unlink(missing_ok=True)
    print(f"nativelink: stopped; the cache under {CACHE} is kept")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("command", choices=["install", "uninstall", "status", "run"])
    parser.add_argument("--cores", type=int, default=max(1, (os.cpu_count() or 4) - 3))
    parser.add_argument("--background", action="store_true")
    args = parser.parse_args()
    if args.command == "install":
        install(args.cores, args.background)
    elif args.command == "uninstall":
        uninstall()
    elif args.command == "status":
        if answering():
            print("nativelink: answering on 127.0.0.1:50051")
        else:
            sys.exit("nativelink: nothing answers on 127.0.0.1:50051; run `just nativelink install`")
    else:
        render(args.cores, args.background)
        os.execv(binary(), [binary(), str(CONFIG)])


if __name__ == "__main__":
    main()
