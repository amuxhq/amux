#!/usr/bin/env python3
"""Run this Mac's NativeLink: the build cache and worker Buck2 sends every
action to (docs/BUILD.md, "The build cache").

    nativelink.py install [--cores N] [--background] [--remote URL --workers HOST:PORT]
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

`--remote` names a Linux build host's NativeLink (`grpc://host:50051`)
whose cache this one fronts, and `--workers` the address on this Mac its
worker registers with this scheduler at, to run the actions only Linux can
run (build/nativelink/mac.json5). The build host's own setup says which
values those are. They are remembered, so a later `install` keeps them;
`--remote none` drops them.
"""

import argparse
import json
import os
import plistlib
import socket
import shutil
import subprocess
import sys
import time
from pathlib import Path
from urllib.parse import urlparse

ROOT = Path(__file__).resolve().parents[1]
TEMPLATE = ROOT / "build/nativelink/mac.json5"
LABEL = "sh.amux.nativelink"
CACHE = Path.home() / "Library/Caches/amux/nativelink"
CONFIG = CACHE / "config.json5"
# The same with no remote in it, for while the remote is out of reach (it
# would wait on it), or the address its worker registers at isn't this
# machine's (that address is the tailnet's, gone while it's down, and
# NativeLink won't start if it can't bind a listener).
CONFIG_LOCAL = CACHE / "config-local.json5"
# What launchd runs: picks one of the two (start_script), and records which
# in SERVING (`remote` or `local`), read by scripts/buck-config.py.
START = CACHE / "start"
SERVING = CACHE / "serving"
# The remote, read by scripts/buck-config.py too.
SETTINGS = CACHE / "settings.json"
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


def remote_stores(url: str) -> str:
    # A full build uploads thousands of blobs at once. Unbounded, they
    # arrive as more concurrent streams than the remote's HTTP/2 server
    # takes; it refuses the rest, and past a count of refusals it closes
    # the connection (GOAWAY "too_many_internal_resets") and every upload on
    # it fails. So they go over a few connections, a bounded number at a
    # time, and a failed call is tried again.
    grpc = lambda kind: (
        f'grpc: {{ instance_name: "main", endpoints: [{{ address: "{url}" }}], store_type: "{kind}", '
        "connections_per_endpoint: 4, max_concurrent_requests: 64, "
        "retry: { max_retries: 6, delay: 0.25, jitter: 0.5 } }"
    )
    return f"""    {{
      name: "REMOTE_CAS",
      {grpc("cas")},
    }},
    {{
      // The remote's results, reported missing when it has evicted their
      // outputs, as LOCAL_AC's are.
      name: "REMOTE_AC",
      completeness_checking: {{
        backend: {{ {grpc("ac")} }},
        cas_store: {{ ref_store: {{ name: "REMOTE_CAS" }} }},
      }},
    }},
    {{
      // Uploads go to both; reads fill the local layer from the remote.
      name: "CAS",
      fast_slow: {{
        fast: {{ ref_store: {{ name: "LOCAL_CAS" }} }},
        slow: {{ ref_store: {{ name: "REMOTE_CAS" }} }},
      }},
    }},
    {{
      // This machine's results, then the remote's. Nothing here writes the
      // remote's (its worker does), and a remote result isn't copied into
      // the local layer: NativeLink can't fill a fast action cache from a
      // gRPC one, whose entries carry no size.
      name: "AC",
      fast_slow: {{
        fast: {{ ref_store: {{ name: "LOCAL_AC" }} }},
        slow: {{ ref_store: {{ name: "REMOTE_AC" }} }},
        fast_direction: "update",
        slow_direction: "get",
      }},
    }},"""


LOCAL_ONLY_STORES = """    {
      name: "CAS",
      ref_store: { name: "LOCAL_ONLY_CAS" },
    },
    {
      name: "AC",
      ref_store: { name: "LOCAL_AC" },
    },"""


def remote_workers(address: str) -> str:
    return f"""    {{
      // Where the remote's worker registers, over the network.
      name: "remote-workers",
      listener: {{ http: {{ socket_address: "{address}" }} }},
      services: {{
        worker_api: {{ scheduler: "SCHEDULER" }},
        health: {{}},
      }},
    }},"""


def settings() -> dict:
    try:
        return json.loads(SETTINGS.read_text())
    except (OSError, ValueError):
        return {}


def remember(remote: str | None, workers: str | None) -> dict:
    """The remote this NativeLink fronts: the one given, else the one it was
    last installed with."""
    saved = settings()
    if remote == "none":
        saved = {}
    elif remote is not None:
        if workers is None:
            sys.exit("nativelink: --remote needs --workers, the address on this Mac the remote's worker registers at")
        saved = {"remote": remote, "workers": workers}
    CACHE.mkdir(parents=True, exist_ok=True)
    SETTINGS.write_text(json.dumps(saved) + "\n")
    return saved


def render(cores: int, background: bool, remote: dict) -> None:
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
    template = TEMPLATE.read_text()
    for key, value in values.items():
        template = template.replace("{{" + key + "}}", value)
    local = {"remote_stores": LOCAL_ONLY_STORES, "worker_slow": "{ noop: {} }", "remote_workers": ""}
    full = local if not remote else {
        "remote_stores": remote_stores(remote["remote"]),
        "worker_slow": '{ ref_store: { name: "REMOTE_CAS" } }',
        "remote_workers": remote_workers(remote["workers"]),
    }
    for path, fills in ((CONFIG, full), (CONFIG_LOCAL, local)):
        text = template
        for key, value in fills.items():
            text = text.replace("{{" + key + "}}", value)
        path.write_text(text)
    START.write_text(start_script(remote))


def start_script(remote: dict) -> str:
    if not remote:
        return f"""#!/bin/sh
# Written by scripts/nativelink.py.
echo local > "{SERVING}"
exec "{binary()}" "{CONFIG_LOCAL}"
"""
    workers = remote["workers"].rsplit(":", 1)[0]
    url = urlparse(remote["remote"])
    return f"""#!/bin/sh
# Written by scripts/nativelink.py. In front of the remote only while it
# answers and {workers} is this machine's; scripts/buck-config.py restarts
# this when that changes.
if /sbin/ifconfig | grep -q "inet {workers} " \\
        && /usr/bin/nc -z -G 2 {url.hostname} {url.port or 50051} > /dev/null 2>&1; then
    echo remote > "{SERVING}"
    exec "{binary()}" "{CONFIG}"
fi
echo local > "{SERVING}"
exec "{binary()}" "{CONFIG_LOCAL}"
"""


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


def install(cores: int, background: bool, remote: dict) -> None:
    render(cores, background, remote)
    LOG.parent.mkdir(parents=True, exist_ok=True)
    PLIST.parent.mkdir(parents=True, exist_ok=True)
    plist = {
        "Label": LABEL,
        "ProgramArguments": ["/bin/sh", str(START)],
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
    if remote:
        print(f"nativelink: in front of {remote['remote']}; its worker registers at {remote['workers']}")
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
    parser.add_argument("--remote", help="a Linux build host's NativeLink, grpc://HOST:PORT, or `none`")
    parser.add_argument("--workers", help="HOST:PORT on this Mac where the remote's worker registers")
    args = parser.parse_args()
    if args.command == "install":
        install(args.cores, args.background, remember(args.remote, args.workers))
    elif args.command == "uninstall":
        uninstall()
    elif args.command == "status":
        if answering():
            print("nativelink: answering on 127.0.0.1:50051")
        else:
            sys.exit("nativelink: nothing answers on 127.0.0.1:50051; run `just nativelink install`")
    else:
        render(args.cores, args.background, remember(args.remote, args.workers))
        os.execv(binary(), [binary(), str(CONFIG)])


if __name__ == "__main__":
    main()
