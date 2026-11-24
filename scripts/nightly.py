#!/usr/bin/env python3
"""Run one of amux's nightly checks: what no push or landing waits for.

    nightly.py [--nix] KIND

KIND is one of:

    models    every protocol model at its nightly bounds (`just
              models-nightly`, about three and a half hours on a build
              host's cores), then every simulated testnet story under 2000
              seeds (`scripts/sim.py --seeds 2000`)
    nightly   the shipping binary under the release profile and the release
              dependency policy (`just release-check`), and both sanitizers
              over every crate whose tests they run (`just sanitize address`,
              `just sanitize thread`): a landing runs the sanitizers only
              when one of those crates itself changed, not the crates
              beneath them
    offline   the workspace's tests with no network but loopback and empty
              configuration homes (`just offline-test`), after fetching the
              crates while the network is there

Every step runs whichever failed before it, and the run ends with one line
naming the kind and the steps that failed, exiting non-zero if any did.
Each step runs from this checkout, and with --nix inside its own `nix
develop`, the toolchain the checkout's flake pins.

A machine runs these on a timer from a checkout of main that it keeps,
target/ included, so each night's Cargo build starts warm; nothing here
names the machine. Standard library only, and no newer Python than
Debian's, as scripts/landing.py says: the timer runs this from main with no
build step.
"""

from __future__ import annotations

import argparse
import datetime
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# Each kind's steps: a name and the command, run from the checkout.
KINDS: dict[str, list[tuple[str, list[str]]]] = {
    "models": [
        ("the models at their nightly bounds", ["just", "models-nightly"]),
        ("the simulated stories under 2000 seeds", ["scripts/bounded", "3000", "scripts/py", "scripts/sim.py", "--seeds", "2000"]),
    ],
    "nightly": [
        ("the release build and its dependency policy", ["just", "release-check"]),
        ("the address sanitizer", ["just", "sanitize", "address"]),
        ("the thread sanitizer", ["just", "sanitize", "thread"]),
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


def run(kind: str, nix: bool) -> int:
    commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip()
    say(f"{kind} at {commit[:12] or 'an unknown commit'}")
    # tmux, which the tests drive, starts each pane with $SHELL, which a
    # service account may have set to nologin.
    env = dict(os.environ, SHELL="/bin/sh")
    failed = []
    for name, command in KINDS[kind]:
        argv = ["nix", "develop", str(ROOT), "--command", *command] if nix else command
        say(f"{name}: {' '.join(command)}")
        started = datetime.datetime.now()
        status = subprocess.run(argv, cwd=ROOT, env=env, stdin=subprocess.DEVNULL).returncode
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
    parser.add_argument("--nix", action="store_true", help="run each step inside this checkout's `nix develop`")
    parser.add_argument("kind", choices=sorted(KINDS))
    args = parser.parse_args()
    return run(args.kind, args.nix)


if __name__ == "__main__":
    sys.exit(main())
