#!/usr/bin/env python3
"""Fail when a state-machine core could block, wait or share state.

The daemon, relay, agent process and supervisor each decide everything in one
`step` over typed inputs and outputs; sockets, timers, codecs and the async
runtime belong to the drivers around them. Two facts hold that shape, and
this check states both:

- no core's normal dependency graph, on any target, contains tokio or one of
  its own crates, so nothing a core links can await or spawn for it;
- no core's source, comments aside, names `Mutex` or `RwLock`, calls `spawn`,
  or writes `async` or `.await`.
"""

from __future__ import annotations

from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
# The ring the cores record their inputs in is held to the same rule.
CORES = ["daemon-core", "relay-core", "agent-core", "supervisor-core", "core-ring"]
RUNTIME = re.compile(r"^(tokio(-[\w-]+)?) v")
FORBIDDEN = [
    (re.compile(r"\bMutex\b"), "a Mutex"),
    (re.compile(r"\bRwLock\b"), "an RwLock"),
    (re.compile(r"\bspawn\b"), "a spawn"),
    (re.compile(r"\basync\b"), "async"),
    (re.compile(r"\.await\b"), "an await"),
]
COMMENT = re.compile(r"//.*$")


def runtime_in_graph(core: str) -> list[str]:
    tree = subprocess.run(
        [
            "cargo", "tree", "--locked", "-p", core, "-e", "normal",
            "--target", "all", "--prefix", "none",
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if tree.returncode != 0:
        return [f"cargo tree failed: {tree.stderr.strip()}"]
    return sorted({match.group(1) for line in tree.stdout.splitlines() if (match := RUNTIME.match(line))})


def forbidden_in_source(core: str) -> list[str]:
    found = []
    for path in sorted((ROOT / "crates" / core).rglob("*.rs")):
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            code = COMMENT.sub("", line)
            for pattern, what in FORBIDDEN:
                if pattern.search(code):
                    found.append(f"{path.relative_to(ROOT)}:{number}: {what}: {line.strip()}")
    return found


def main() -> int:
    failures = []
    for core in CORES:
        if not (ROOT / "crates" / core / "Cargo.toml").is_file():
            failures.append(f"{core}: no such crate")
            continue
        if runtime := runtime_in_graph(core):
            failures.append(f"{core}: its dependency graph contains {', '.join(runtime)}")
        failures.extend(f"{core}: {finding}" for finding in forbidden_in_source(core))
    if failures:
        print("cores check failed:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print(f"cores check passed: {', '.join(CORES)} carry no tokio, lock, spawn or await")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
