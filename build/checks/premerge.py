#!/usr/bin/env python3
"""Run `just premerge`: the Buck2 tests and, beside them, the checks that stay scripts.

    premerge.py PATTERN...

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

# The checks that stay scripts, each a recipe of the root justfile, and why
# none is a Buck2 test: what each reads is the workspace as a whole, or the
# build graph itself, which no action of that graph can be given.
SCRIPTS = [
    # rustfmt over every Rust file, with the dated nightly.
    "fmt-check",
    # Cargo's resolved graph: which crate may depend on which.
    "dependency-policy",
    # cargo-shear, over every crate's code and Cargo's graph.
    "shear",
    # `cargo tree` of each state-machine core: no tokio.
    "cores-graph-check",
    # The suite catalogue against every crate's targets (Cargo's view),
    # every just recipe and every path the catalogue names.
    "tests-check",
    # Each crate's BUCK against its Cargo.toml: a query of Buck2's graph.
    "parity",
    # What ships carries no test crate or feature: a query of the configured graph.
    "shipping-check",
    # Clippy: Buck2 actions, cached like the tests, but a build of every
    # target's clippy output, which `buck2 test` doesn't build.
    "lint",
]


def main(patterns: list[str]) -> int:
    started = time.monotonic()
    logs = Path(tempfile.mkdtemp(prefix="premerge-"))
    running: dict[str, tuple[subprocess.Popen, Path, float]] = {}
    for name in SCRIPTS:
        log = logs / f"{name}.log"
        with log.open("wb") as out:
            child = subprocess.Popen([JUST, name], cwd=ROOT, stdout=out, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
        running[name] = (child, log, time.monotonic())
    tests = subprocess.Popen([JUST, "test", *patterns], cwd=ROOT, stdin=subprocess.DEVNULL)

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
