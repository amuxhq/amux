#!/usr/bin/env python3
"""Walk the testnet's simulated stories by seed.

Runs every test target of every suite the test catalogue declares a
simulated mode for, under TESTNET_MODE=simulated: a few seeds per story by
default, `--seeds N` for N of them, `--seed N` for exactly one (how a
failing seed is reproduced). Names after the options narrow the run to the
stories whose test names contain them.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import subprocess
import sys
import tomllib

import suites as selections


ROOT = Path(__file__).resolve().parents[1]
CATALOG = ROOT / "tests" / "catalog.toml"
SIMULATED = "simulated"


def simulated_suites() -> list[dict[str, object]]:
    """Every suite of the catalogue with a simulated mode."""
    with CATALOG.open("rb") as source:
        suites = tomllib.load(source)["suite"]
    return [suite for suite in suites if SIMULATED in suite.get("modes", [])]


def walk(seed: int | None, seeds: int | None, filters: list[str]) -> int:
    """Runs the simulated stories; returns nextest's exit status."""
    suites = simulated_suites()
    if not suites:
        print("no suite in tests/catalog.toml declares a simulated mode", file=sys.stderr)
        return 1
    env = dict(os.environ, TESTNET_MODE=SIMULATED)
    env.pop("TESTNET_SEED", None)
    env.pop("TESTNET_SEEDS", None)
    if seed is not None:
        env["TESTNET_SEED"] = str(seed)
    if seeds is not None:
        env["TESTNET_SEEDS"] = str(seeds)
    # The workspace scope `just test` builds with, so the stories
    # reuse its build: naming the package instead unifies the dependencies'
    # features differently and rebuilds every workspace crate beneath it.
    command = ["cargo", "nextest", "run", "--locked", "--no-fail-fast", "--workspace"]
    command += ["-E", " | ".join(selections.filterset(selector) for suite in suites for selector in suite["cargo"])]
    # Each story's output whole as it ends, unindented, so its walked line
    # reads as the story printed it; the stories still run side by side.
    command += ["--success-output", "immediate", "--failure-output", "immediate", "--no-output-indent"]
    command += ["--", *filters]
    shown = ["TESTNET_MODE=simulated"] + [f"{k}={env[k]}" for k in ("TESTNET_SEED", "TESTNET_SEEDS") if k in env]
    print("$", *shown, *command, flush=True)
    return subprocess.run(command, cwd=ROOT, env=env).returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    which = parser.add_mutually_exclusive_group()
    which.add_argument("--seed", type=int, help="run exactly this seed")
    which.add_argument("--seeds", type=int, help="run this many seeds from 0")
    parser.add_argument("filters", nargs="*", help="run only stories whose test names contain one of these")
    arguments = parser.parse_args()
    return walk(arguments.seed, arguments.seeds, arguments.filters)


if __name__ == "__main__":
    sys.exit(main())
