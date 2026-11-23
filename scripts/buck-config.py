#!/usr/bin/env python3
"""Write this worktree's .buckconfig.local: where this machine's toolchains are.

An action's cache key covers its command line and declared inputs, not the
contents of a compiler it calls by path. So every toolchain is named here by
a path that changes with its version (rustup's toolchain directories carry
the version), and Xcode, whose path doesn't, is named by its version and
build in a platform property every Mac action carries. A toolchain upgrade
therefore changes every key it should.

scripts/buck2 runs this whenever rust-toolchain.toml, build/tools.json or
the worker's core count is newer than .buckconfig.local, so the file
follows the pins without anyone remembering to.
"""

import os
import platform
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / ".buckconfig.local"
JUSTFILE = ROOT / "justfile"
WORKER_CORES = Path.home() / "Library/Caches/amux/nativelink/cores"


def run(*args: str, env: dict | None = None) -> str:
    return subprocess.check_output(args, text=True, env=env).strip()


def sysroot(channel: str) -> str:
    env = dict(os.environ, RUSTUP_TOOLCHAIN=channel)
    try:
        return run("rustc", "--print", "sysroot", env=env)
    except (OSError, subprocess.CalledProcessError):
        sys.exit(f"buck-config: no Rust toolchain {channel}; run `rustup toolchain install {channel}`")


def nightly_channel() -> str:
    # The justfile names the dated nightly once (`nightly_cargo`).
    match = re.search(r"nightly-(\d{4}-\d{2}-\d{2})", JUSTFILE.read_text())
    if not match:
        sys.exit("buck-config: the justfile names no dated nightly")
    return f"nightly-{match.group(1)}"


def worker_cores() -> int:
    cores = WORKER_CORES.read_text().strip() if WORKER_CORES.exists() else ""
    return int(cores) if cores.isdigit() else max(1, (os.cpu_count() or 4) - 3)


def main() -> None:
    channel = tomllib.loads((ROOT / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    lines = [
        "# Written by scripts/buck-config.py; not committed. It is rewritten",
        "# when the pinned toolchains change.",
        "[rust]",
        f"  toolchain_dir = {sysroot(channel)}",
        f"  nightly_dir = {sysroot(nightly_channel())}",
    ]
    lines += [
        # The cores this machine's NativeLink worker offers
        # (scripts/nativelink.py), all of which a test that uses every core
        # reserves.
        "[amux]",
        f"  worker_cores = {worker_cores()}",
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


if __name__ == "__main__":
    main()
