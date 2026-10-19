#!/usr/bin/env python3
"""Measure line coverage per workspace crate and write it to tests/coverage.md.

Runs every workspace test target under cargo-llvm-cov with the desktop
features, as `just test` does, then sums the per-file line counts of each
crate's own sources. The report names the commit it measured, so a reader
can tell how stale it is. Coverage is a reading aid for judging which tests
earn their keep, not a gate: nothing fails on a low number.

A test that fails under instrumentation still leaves a report, but the
report lists the failing targets and the recipe exits non-zero, because the
numbers of a partial run understate what the suite covers.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[1]
REPORT = ROOT / "tests" / "coverage.md"
# Kept apart from target/debug: instrumented builds would invalidate it.
TARGET = ROOT / "target" / "coverage"
SUMMARY = TARGET / "summary.json"
# Cargo's closing list of the targets whose tests failed, one per line.
FAILED_TARGET = re.compile(r"^\s+`(?P<target>.+)`$")


def git(*arguments: str) -> str:
    return subprocess.run(
        ["git", *arguments], cwd=ROOT, check=True, capture_output=True, text=True
    ).stdout.strip()


def coverage_env() -> dict[str, str]:
    """The environment cargo-llvm-cov instruments a plain cargo build under.

    Tests that build binaries of their own mid-run (the fake providers,
    fake-amux) call cargo again and look for the result beside their own
    executable. `cargo llvm-cov test` builds into a target directory it names
    only on its own command line, so those nested builds went to
    target/debug and the tests ran stale or missing binaries. Running plain
    `cargo test` under the variables `show-env` prints, with
    CARGO_TARGET_DIR set, sends every build to one instrumented directory
    that the developer's own target/debug never shares.
    """
    env = dict(os.environ, CARGO_TARGET_DIR=str(TARGET))
    shown = subprocess.run(
        ["cargo", "llvm-cov", "show-env", "--sh"],
        cwd=ROOT, env=env, check=True, capture_output=True, text=True,
    ).stdout
    for line in shown.splitlines():
        if line.startswith("export "):
            name, _, value = line.removeprefix("export ").partition("=")
            env[name] = shlex.split(value)[0] if value else ""
    return env


def run_tests(env: dict[str, str]) -> tuple[list[str], float]:
    """Run the workspace's tests under instrumentation; return the failed targets."""
    subprocess.run(["cargo", "llvm-cov", "clean", "--workspace"], cwd=ROOT, env=env, check=True)
    command = [
        "cargo", "test", "--locked", "--workspace", "--all-targets",
        "--features", "bundled", "--no-fail-fast",
    ]
    started = time.monotonic()
    process = subprocess.Popen(
        command, cwd=ROOT, env=env, stderr=subprocess.PIPE, text=True, bufsize=1
    )
    failed: list[str] = []
    listing = False
    assert process.stderr is not None
    for line in process.stderr:
        sys.stderr.write(line)
        if "target failed" in line or "targets failed" in line:
            listing = True
            continue
        if listing:
            match = FAILED_TARGET.match(line.rstrip("\n"))
            if match:
                failed.append(match["target"])
            else:
                listing = False
    status = process.wait()
    if status != 0 and not failed:
        sys.exit(f"coverage: cargo test exited {status} without naming a failed target")
    subprocess.run(
        ["cargo", "llvm-cov", "report", "--json", "--summary-only", "--output-path", str(SUMMARY)],
        cwd=ROOT, env=env, check=True,
    )
    return failed, time.monotonic() - started


def crate_of(path: str) -> str | None:
    """The workspace crate a source file belongs to, or None for anything else."""
    try:
        relative = Path(path).resolve().relative_to(ROOT)
    except ValueError:
        return None
    parts = relative.parts
    if len(parts) >= 3 and parts[0] == "crates":
        return parts[1]
    return None


def per_crate(summary: dict) -> dict[str, tuple[int, int]]:
    totals: dict[str, tuple[int, int]] = {}
    for data in summary["data"]:
        for entry in data["files"]:
            crate = crate_of(entry["filename"])
            if crate is None:
                continue
            lines = entry["summary"]["lines"]
            covered, count = totals.get(crate, (0, 0))
            totals[crate] = (covered + lines["covered"], count + lines["count"])
    return totals


def percent(covered: int, count: int) -> str:
    return f"{100 * covered / count:.1f}%" if count else "n/a"


def write_report(
    totals: dict[str, tuple[int, int]], failed: list[str], commit: str, dirty: bool, seconds: float
) -> None:
    measured = f"`{commit}`" + (" with uncommitted changes" if dirty else "")
    lines = [
        "# Line coverage per crate",
        "",
        "Written by `just coverage`: every workspace test target run under",
        "cargo-llvm-cov with the desktop features. Lines are counted in each",
        "crate's own sources (`crates/<crate>/src`), whichever crate's tests ran",
        "them, so a core's figure includes what the protocol models and the",
        "testnet reach in it. Regenerate it rather than editing it.",
        "",
        f"Measured at commit {measured}, in {seconds / 60:.0f} minutes.",
        "",
    ]
    if failed:
        lines += [
            "These targets failed under instrumentation, so the figures understate",
            "what the suite covers:",
            "",
            *[f"- `{target}`" for target in failed],
            "",
        ]
    lines += ["| crate | lines | covered | line coverage |", "|---|---:|---:|---:|"]
    all_covered = all_count = 0
    for crate in sorted(totals):
        covered, count = totals[crate]
        all_covered += covered
        all_count += count
        lines.append(f"| {crate} | {count} | {covered} | {percent(covered, count)} |")
    lines.append(f"| **workspace** | {all_count} | {all_covered} | {percent(all_covered, all_count)} |")
    REPORT.write_text("\n".join(lines) + "\n")


def main() -> int:
    commit = git("rev-parse", "HEAD")
    dirty = bool(git("status", "--porcelain", "--untracked-files=no"))
    failed, seconds = run_tests(coverage_env())
    totals = per_crate(json.loads(SUMMARY.read_text()))
    write_report(totals, failed, commit, dirty, seconds)
    print(f"coverage: wrote {REPORT.relative_to(ROOT)} ({len(totals)} crates)")
    if failed:
        print(f"coverage: {len(failed)} targets failed under instrumentation", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
