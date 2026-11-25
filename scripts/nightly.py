#!/usr/bin/env python3
"""Run one of amux's nightly checks: what no push or landing waits for.

    nightly.py KIND [MODEL...]

KIND is one of:

    models    the protocol models at their CI and nightly bounds (`just
              models-nightly`); model names, the modules of the models'
              integration tests, narrow it to those models
    seeds     every simulated testnet story under 2000 seeds
              (`scripts/sim.py --seeds 2000`)
    pointers  the phone's C ABI tests under Miri, which checks every pointer
              handed across the boundary (`just miri`)
    address   the tests that reach unsafe code and C under the address
              sanitizer (`just sanitize address`)
    thread    the same under the thread sanitizer (`just sanitize thread`)
    release   the shipping binary under the release profile and the release
              dependency policy (`just release-check`)
    offline   the workspace's tests with no network but loopback and empty
              configuration homes (`just offline-test`), after fetching the
              crates while the network is there

These look for faults rarer than a change should wait on, and take longer:
the models are exhaustive searches that run for hours. The nightly workflow
(.github/workflows/nightly.yml) runs each kind in a job of its own, and the
models one job per model, since all of them in one would not fit a hosted
runner's six hours. Run one here with the same command to reproduce a job.

Every step runs whichever failed before it, and the run ends with one line
naming the kind and the steps that failed, exiting non-zero if any did.
"""

from __future__ import annotations

import argparse
import datetime
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
# The repository's sh scripts, which Windows can't start itself.
SH = ["sh"] if os.name == "nt" else []

# Each kind's steps: a name and the command, run from the checkout.
KINDS: dict[str, list[tuple[str, list[str]]]] = {
    "models": [
        ("the models at their CI and nightly bounds", ["just", "models-nightly"]),
    ],
    "seeds": [
        # The walk builds the workspace's tests first, which a cold cache
        # compiles from nothing, so the bound holds both.
        ("the simulated stories under 2000 seeds", [*SH, "scripts/bounded", "5400", "scripts/py", "scripts/sim.py", "--seeds", "2000"]),
    ],
    "pointers": [
        ("the C ABI's pointer checks", ["just", "miri"]),
    ],
    "address": [
        ("the address sanitizer", ["just", "sanitize", "address"]),
    ],
    "thread": [
        ("the thread sanitizer", ["just", "sanitize", "thread"]),
    ],
    "release": [
        ("the release build and its dependency policy", ["just", "release-check"]),
    ],
    "offline": [
        # The claim is that the tests need no network, not that fetching
        # their dependencies needs none.
        ("fetching the crates", ["cargo", "fetch", "--locked"]),
        ("the tests without a network", ["just", "offline-test"]),
    ],
}


def say(what: str) -> None:
    print(f"{datetime.datetime.now().isoformat(timespec='seconds')} nightly: {what}", flush=True)


def run(kind: str, models: list[str]) -> int:
    commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip()
    say(f"{kind} at {commit[:12] or 'an unknown commit'}")
    failed = []
    for name, command in KINDS[kind]:
        command = command + models
        say(f"{name}: {' '.join(command)}")
        started = datetime.datetime.now()
        status = subprocess.run(command, cwd=ROOT, stdin=subprocess.DEVNULL).returncode
        took = datetime.datetime.now() - started
        say(f"{name}: {'passed' if status == 0 else f'failed (exit {status})'} after {str(took).split('.')[0]}")
        if status != 0:
            failed.append(name)
    if failed:
        say(f"{kind} FAILED: {'; '.join(failed)}")
        return 1
    say(f"{kind} passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("kind", choices=sorted(KINDS))
    parser.add_argument("models", nargs="*", metavar="MODEL", help="with models, the models to check")
    args = parser.parse_args()
    if args.models and args.kind != "models":
        parser.error("only the models kind takes model names")
    return run(args.kind, args.models)


if __name__ == "__main__":
    sys.exit(main())
