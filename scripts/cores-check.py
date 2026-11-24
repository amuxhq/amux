#!/usr/bin/env python3
"""Fail when a state-machine core could block, wait or share state.

The daemon, relay, agent process and supervisor, and each link's path
machine, decide everything in one
`step` over typed inputs and outputs; sockets, timers, codecs and the async
runtime belong to the drivers around them. Two facts hold that shape, and
this check states both:

- no core's normal dependency graph, on any target, contains tokio or one of
  its own crates, so nothing a core links can await or spawn for it;
- no core's source, comments aside, names `Mutex` or `RwLock`, calls `spawn`,
  or writes `async` or `.await`.

    cores-check.py [--graph | --sources]

With no option it checks both. `--sources` checks only the sources, which
is how it runs as a Buck2 test over the cores' files (the root BUCK file);
`--graph` only the dependency graphs, which needs Cargo and the whole
workspace, so `just premerge` runs that half as a script.
"""

from __future__ import annotations

from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
# The ring the cores record their inputs in is held to the same rule.
CORES = ["daemon-core", "relay-core", "path-core", "socket-core", "agent-core", "supervisor-core", "workspace-core", "core-ring"]
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
    graph = sys.argv[1:] != ["--sources"]
    sources = sys.argv[1:] != ["--graph"]
    failures = []
    for core in CORES:
        if not (ROOT / "crates" / core / ("Cargo.toml" if graph else "src")).exists():
            failures.append(f"{core}: no such crate")
            continue
        if graph and (runtime := runtime_in_graph(core)):
            failures.append(f"{core}: its dependency graph contains {', '.join(runtime)}")
        if sources:
            failures.extend(f"{core}: {finding}" for finding in forbidden_in_source(core))
    if failures:
        print("cores check failed:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    carry = {(True, True): "tokio, lock, spawn or await", (True, False): "tokio", (False, True): "lock, spawn or await"}[(graph, sources)]
    print(f"cores check passed: {', '.join(CORES)} carry no {carry}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
