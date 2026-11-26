#!/usr/bin/env python3
"""Run `just premerge`: the Buck2 tests and, beside them, the checks that stay scripts.

    premerge.py [--without-windows] PATTERN...

`just test PATTERN...` runs every test and repository check in the graph,
each from the cache unless its inputs changed, and its output goes straight
to the terminal. The checks that stay scripts (docs/BUILD.md, "Repository
checks") share nothing with it but Buck2's daemon, which serves concurrent
commands, so they run at the same time, each with its output kept. A line
says when each finishes; at the end, the output of every one that failed is
printed in full, and the run fails if anything did.
"""

import os
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
JUST = os.environ.get("JUST", "just")
# The repository's Python (scripts/py picks one), for its other scripts;
# scripts/py is a sh script, which Windows can't start directly.
PY = [sys.executable] if os.name == "nt" else [str(ROOT / "scripts/py")]

# The checks that stay scripts, each a recipe of the root justfile, and why
# none is a Buck2 test: each queries Buck2's own graph, which no action of
# that graph can, or reads the checkout as a whole. None runs Cargo here:
# what they need of Cargo they read from a Linux worker's (build/cargo.bzl),
# and the checks that run Cargo are Buck2 tests in the root BUCK file
# (`fmt-check`, `shear`, `deny` and the like), so this machine needs no
# Rust toolchain for any of it.
SCRIPTS = [
    # The suite catalogue against every crate's targets (//:cargo-metadata),
    # every just recipe and every path the catalogue names.
    "tests-check",
    # Each crate's BUCK against its Cargo.toml: a query of Buck2's graph.
    "parity",
    # What ships carries no test crate or feature: a query of the configured graph.
    "shipping-check",
    # Clippy: Buck2 actions, cached like the tests, but a build of every
    # target's clippy output, which `buck2 test` doesn't build.
    "lint",
    # Which gated CI job a change runs: that the Apple apps' graphs read
    # nothing only they test outside their globs in build/ci-jobs.toml.
    "ci-jobs-check",
]

# Every Windows binary, built on a Linux worker with nothing fetched back:
# CI runs Windows' tests only when a change reaches what only Windows tests
# (build/ci-jobs.toml), so a change that stops Windows compiling is caught
# here instead, for every change. A build of the configured graph, which
# `buck2 test` over the Linux targets doesn't build; with no Linux worker in
# reach it is skipped and says so, as the linux-only tests are.
WINDOWS = "check-windows"

# On Windows, clippy for Linux too, on a Linux worker: `lint` there lints
# Windows' build, which leaves out every `cfg(unix)` item, so a warning only
# Linux's build has would pass here and fail the land. A Mac or a Linux
# machine lints as it always has, since Linux's or its own build there holds
# the Unix code. Skipped with no Linux worker in reach, as check-windows is.
LINUX_LINT = ("lint-linux", ["lint", "--target-platforms", "root//build/platforms:linux-cli-testing-dev"])

# Every Swift module of the iPhone app and the Mac app, their packages and
# their test bundles, compiled but not linked or run (`just apple-build`):
# CI builds the apps only for a change it judges theirs (build/ci-jobs.toml),
# and a change to the Rust or the protocol beneath them can break their
# Swift without touching a file of theirs. A build of the Apple graphs,
# through the Mac's own cache, so what nothing changed under is cache hits.
# Those graphs configure only on a Mac, so anywhere else it is skipped and
# says so.
APPLE = "apple-build"


def main(patterns: list[str]) -> int:
    # Each check by name, with the recipe and arguments that run it.
    scripts = [(name, [name]) for name in SCRIPTS]
    # Where actions go (whether the build host is in reach, on a Mac) is
    # decided once, here, and every recipe below is told it rather than
    # asking again: one that found the host gone mid-run would switch the
    # mode and restart Buck2's daemon under the rest (scripts/buck-config.py).
    mode = subprocess.run(
        [*PY, str(ROOT / "scripts/buck-config.py"), "decide"],
        cwd=ROOT, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, text=True, check=True,
    ).stdout.strip()
    env = dict(os.environ, AMUX_BUCK_MODE=mode)
    reach: list[bool] = []

    def linux() -> bool:
        """Whether a Linux worker is in reach, asked once and only if needed."""
        if not reach:
            asked = subprocess.run([*PY, str(ROOT / "scripts/buck-config.py"), "linux"], cwd=ROOT, env=env, stdin=subprocess.DEVNULL)
            reach.append(asked.returncode == 0)
        return reach[0]

    if patterns[:1] == ["--without-windows"]:
        patterns = patterns[1:]
    elif linux():
        scripts.append((WINDOWS, [WINDOWS]))
    else:
        print(f"premerge: {WINDOWS} skipped: no Linux worker is in reach to build Windows on", flush=True)
    if os.name == "nt":
        if linux():
            scripts.append(LINUX_LINT)
        else:
            print(f"premerge: {LINUX_LINT[0]} skipped: no Linux worker is in reach to lint Linux's build on", flush=True)
    if sys.platform == "darwin":
        scripts.append((APPLE, [APPLE]))
    else:
        print(f"premerge: {APPLE} skipped: the Apple apps' Swift compiles only on a Mac", flush=True)
    started = time.monotonic()
    wall_start = time.time()
    # What the check targets read from outside the checkout is written now,
    # before any Buck2 command starts, so `just test` below, which writes it
    # too, finds nothing left to change. Buck2's daemon runs two commands at
    # once only while they see the same files: a write after clippy's build
    # has started holds the tests until that build ends.
    subprocess.run([*PY, str(ROOT / "build/checks/inputs.py")], cwd=ROOT, env=env, check=True, stdin=subprocess.DEVNULL)
    logs = Path(tempfile.mkdtemp(prefix="premerge-"))
    running: dict[str, tuple[subprocess.Popen, Path, float]] = {}
    for name, recipe in scripts:
        log = logs / f"{name}.log"
        with log.open("wb") as out:
            child = subprocess.Popen([JUST, *recipe], cwd=ROOT, env=env, stdout=out, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
        running[name] = (child, log, time.monotonic())
    tests = subprocess.Popen([JUST, "test", *patterns], cwd=ROOT, env=env, stdin=subprocess.DEVNULL)

    def stop(signum, _frame):
        for child, _, _ in running.values():
            child.terminate()
        tests.terminate()
        sys.exit(128 + signum)

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)

    failed: list[tuple[str, Path | None]] = []
    while running:
        for name, (child, log, since) in list(running.items()):
            if child.poll() is None:
                continue
            del running[name]
            took = time.monotonic() - since
            if child.returncode == 0:
                print(f"premerge: {name} passed ({took:.1f} s)", flush=True)
            else:
                print(f"premerge: {name} FAILED (exit {child.returncode}, {took:.1f} s); its output follows the tests", flush=True)
                failed.append((name, log))
        time.sleep(0.1)
    if tests.wait() != 0:
        failed.append((f"just test {' '.join(patterns)}", None))
    # Where the tests' time went (build/checks/timing.py), so a landing's
    # log says it for the build host too.
    ours = [log for log in (ROOT / "buck-out/v2/log").glob("*_test_*_events.pb.zst") if log.stat().st_mtime >= wall_start]
    if ours:
        newest = max(ours, key=lambda log: log.stat().st_mtime)
        try:
            subprocess.run([*PY, str(ROOT / "build/checks/timing.py"), str(newest)], cwd=ROOT, env=env, stdin=subprocess.DEVNULL, timeout=300)
        except subprocess.TimeoutExpired:
            print("timing: no summary (it took over five minutes)", flush=True)

    for name, log in failed:
        if log is not None:
            print(f"\n==== premerge: {name} failed ====", flush=True)
            sys.stdout.write(log.read_text(errors="replace"))
    took = time.monotonic() - started
    if failed:
        print(f"\npremerge: FAILED after {took:.0f} s: {', '.join(name for name, _ in failed)}", flush=True)
        return 1
    print(f"premerge: passed in {took:.0f} s", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
