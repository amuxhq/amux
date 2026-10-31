#!/usr/bin/env python3
"""Walk the testnet's simulated stories by seed, and run the nightly walk.

`sim` runs every test target of every suite the test catalogue declares a
simulated mode for, under TESTNET_MODE=simulated: a few seeds per story by
default, `--seeds N` for N of them, `--seed N` for exactly one (how a
failing seed is reproduced). Names after the options narrow the run to the
stories whose test names contain them.

`nightly` walks `--seeds N` (thousands) the same way, then checks the
protocol models at `--bounds nightly` (`just models-nightly`), at `ci`
bounds (`just test-crate models`), or not at all (`none`, for a workflow
whose own matrix runs the models), and prints a report: seeds walked per
story, time, and every failing seed.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import tomllib

import suites as selections


ROOT = Path(__file__).resolve().parents[1]
CATALOG = ROOT / "tests" / "catalog.toml"
SIMULATED = "simulated"
# The line testnet::modes prints when a story has walked its seeds.
WALKED = re.compile(
    r"^simulated story (?P<story>\S+): (?P<seeds>\d+) seeds walked in (?P<seconds>[\d.]+)s, "
    r"failing seeds \[(?P<failing>[\d, ]*)\]$"
)


def simulated_suites() -> list[dict[str, object]]:
    """Every suite of the catalogue with a simulated mode."""
    with CATALOG.open("rb") as source:
        suites = tomllib.load(source)["suite"]
    return [suite for suite in suites if SIMULATED in suite.get("modes", [])]


def walk(seed: int | None, seeds: int | None, filters: list[str]) -> tuple[int, list[dict[str, object]]]:
    """Runs the simulated stories, streaming their output; returns
    nextest's exit status and each story's walked line."""
    suites = simulated_suites()
    if not suites:
        print("no suite in tests/catalog.toml declares a simulated mode", file=sys.stderr)
        return 1, []
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
    walked: list[dict[str, object]] = []
    # nextest writes the tests' output, with its own, to stderr.
    process = subprocess.Popen(command, cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    assert process.stdout is not None
    for line in process.stdout:
        sys.stdout.write(line)
        match = WALKED.match(line.strip())
        if match:
            failing = [int(s) for s in match["failing"].replace(",", " ").split()]
            walked.append(
                {
                    "story": match["story"],
                    "seeds": int(match["seeds"]),
                    "seconds": float(match["seconds"]),
                    "failing": failing,
                }
            )
    return process.wait(), walked


def report(walked: list[dict[str, object]], seconds: float) -> None:
    print("\n## Simulated walk\n")
    print("| Story | Seeds walked | Seconds | Failing seeds |")
    print("| --- | ---: | ---: | --- |")
    for story in walked:
        failing = ", ".join(str(s) for s in story["failing"]) or "none"  # type: ignore[union-attr]
        print(f"| {story['story']} | {story['seeds']} | {story['seconds']:.2f} | {failing} |")
    total = sum(int(story["seeds"]) for story in walked)  # type: ignore[arg-type]
    failing = sum(len(story["failing"]) for story in walked)  # type: ignore[arg-type]
    print(f"\n{len(walked)} stories, {total} seeded runs, {failing} failing, {seconds:.1f}s with the build.")
    for story in walked:
        for seed in story["failing"]:  # type: ignore[union-attr]
            print(f"reproduce: just sim --seed {seed} {story['story']}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    sim = commands.add_parser("sim", help="walk the simulated stories by seed")
    which = sim.add_mutually_exclusive_group()
    which.add_argument("--seed", type=int, help="run exactly this seed")
    which.add_argument("--seeds", type=int, help="run this many seeds from 0")
    sim.add_argument("filters", nargs="*", help="run only stories whose test names contain one of these")
    nightly = commands.add_parser("nightly", help="walk thousands of seeds and check the models")
    nightly.add_argument("--seeds", type=int, required=True, help="seeds per simulated story")
    nightly.add_argument("--bounds", choices=("nightly", "ci", "none"), default="nightly")
    arguments = parser.parse_args()

    if arguments.command == "sim":
        status, _ = walk(arguments.seed, arguments.seeds, arguments.filters)
        return status

    started = time.monotonic()
    status, walked = walk(None, arguments.seeds, [])
    report(walked, time.monotonic() - started)
    models = {
        "nightly": ["just", "models-nightly"],
        "ci": ["just", "test-crate", "models"],
        "none": None,
    }[arguments.bounds]
    if models is not None:
        print(f"\n## Models at {arguments.bounds} bounds\n\n$ {' '.join(models)}", flush=True)
        checked = time.monotonic()
        models_status = subprocess.run(models, cwd=ROOT).returncode
        verdict = "held" if models_status == 0 else f"failed (exit {models_status})"
        print(f"\nThe models at {arguments.bounds} bounds {verdict} in {time.monotonic() - checked:.0f}s.")
        status = status or models_status
    return status


if __name__ == "__main__":
    sys.exit(main())
