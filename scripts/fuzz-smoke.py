#!/usr/bin/env python3
"""Run every fuzz target in fuzz/ for a bounded time and fail on a crash.

Each target starts from seeds cut from the repository's recorded fixtures
(every line of the provider captures for the decoders and redaction, every
journal fixture whole for the journal targets), so a short run begins inside
real inputs rather than at the empty string. The corpus each run grows and
any crashing input are kept under target/fuzz/; a crash prints the command
that replays it.

`--seconds N` sets each target's time (default 30); names after the options
narrow the run to those targets.
"""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path
import shutil
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[1]
TOOLCHAIN = "+nightly-2026-08-30"
OUT = ROOT / "target" / "fuzz"

# The seeds per target: line-wise from JSONL captures, or whole files.
PROVIDER_LINES = {
    "claude_decode": ["crates/claude-specs/fixtures"],
    "codex_decode": ["crates/codex-specs/fixtures"],
    "redact_value": ["crates/claude-specs/fixtures", "crates/codex-specs/fixtures"],
    "redact_text": ["crates/claude-specs/fixtures", "crates/codex-specs/fixtures"],
}
WHOLE_FILES = {
    "journal_frame": ("crates/testnet/fixtures/journals", "*.frames"),
    "journal_reader": ("crates/testnet/fixtures/journals", "*.frames"),
}
TARGETS = [*PROVIDER_LINES, *WHOLE_FILES]
SEEDS_PER_TARGET = 500


def captured_lines(root: Path) -> list[bytes]:
    """The provider's own lines inside each recorded capture."""
    import json

    lines = []
    for path in sorted(root.rglob("*.jsonl")):
        for raw in path.read_bytes().splitlines():
            try:
                record = json.loads(raw)
            except ValueError:
                continue
            line = record.get("line") if isinstance(record, dict) else None
            lines.append(line.encode() if isinstance(line, str) else raw)
    return lines


def seeds(target: str) -> list[bytes]:
    if target in WHOLE_FILES:
        folder, pattern = WHOLE_FILES[target]
        return [path.read_bytes() for path in sorted((ROOT / folder).glob(pattern))]
    found: dict[bytes, None] = {}
    for folder in PROVIDER_LINES[target]:
        for line in captured_lines(ROOT / folder):
            found.setdefault(line, None)
    # An even spread over the captures, not only the first ones.
    unique = list(found)
    step = max(1, len(unique) // SEEDS_PER_TARGET)
    return unique[::step][:SEEDS_PER_TARGET]


def write_seeds(target: str) -> Path:
    folder = OUT / "seeds" / target
    shutil.rmtree(folder, ignore_errors=True)
    folder.mkdir(parents=True)
    for seed in seeds(target):
        (folder / hashlib.sha1(seed).hexdigest()).write_bytes(seed)
    return folder


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--seconds", type=int, default=30)
    parser.add_argument("targets", nargs="*")
    args = parser.parse_args()
    unknown = [target for target in args.targets if target not in TARGETS]
    if unknown:
        parser.error(f"unknown targets {unknown}; the targets are {TARGETS}")
    targets = args.targets or TARGETS

    print(f"building {len(targets)} fuzz targets with debug assertions", flush=True)
    for build in [[]] if targets == TARGETS else [[target] for target in targets]:
        command = ["cargo", TOOLCHAIN, "fuzz", "build", "--debug-assertions", *build]
        if subprocess.run(command, cwd=ROOT).returncode != 0:
            print("fuzz-smoke: the targets did not build", file=sys.stderr)
            return 1

    failed = []
    for target in targets:
        seed_dir = write_seeds(target)
        corpus = OUT / "corpus" / target
        corpus.mkdir(parents=True, exist_ok=True)
        artifacts = OUT / "artifacts" / target
        artifacts.mkdir(parents=True, exist_ok=True)
        seeded = len(list(seed_dir.iterdir()))
        print(f"\n== {target}: {args.seconds} s from {seeded} seeds", flush=True)
        started = time.monotonic()
        run = subprocess.run(
            [
                "cargo", TOOLCHAIN, "fuzz", "run", "--debug-assertions", target,
                str(corpus), str(seed_dir), "--",
                f"-max_total_time={args.seconds}",
                f"-artifact_prefix={artifacts}/",
                "-print_final_stats=1",
            ],
            cwd=ROOT,
            capture_output=True,
            text=True,
            errors="replace",
        )
        took = time.monotonic() - started
        output = run.stdout + run.stderr
        summary = [
            line for line in output.splitlines()
            if line.startswith(("stat::number_of_executed_units", "stat::peak_rss_mb",
                                "INFO: seed corpus", "Done "))
            or (line.startswith("#") and "DONE" in line)
        ]
        for line in summary:
            print(f"  {line}")
        if run.returncode == 0:
            print(f"  {target}: no crash in {took:.0f} s")
        else:
            failed.append(target)
            print(output[-6000:])
            crashes = sorted(artifacts.iterdir(), key=lambda path: path.stat().st_mtime)
            if crashes:
                print(f"  {target}: CRASHED; replay with "
                      f"cargo {TOOLCHAIN} fuzz run --debug-assertions {target} {crashes[-1]}")
            else:
                print(f"  {target}: FAILED (exit {run.returncode})")
    print()
    if failed:
        print(f"fuzz-smoke: {len(failed)} of {len(targets)} targets failed: {', '.join(failed)}")
        return 1
    print(f"fuzz-smoke: {len(targets)} targets ran {args.seconds} s each with no crash")
    return 0


if __name__ == "__main__":
    sys.exit(main())
