#!/usr/bin/env python3
"""Run the tests that reach amux's unsafe code and C under a sanitizer.

`address` fails on a read or write outside an allocation, a use after free
and a double free; `thread` fails on a data race. Either builds on the pinned
nightly with the standard library rebuilt under the same sanitizer, into
target/sanitize/<kind>.

The suites are where amux has unsafe code or calls C: the phone's C ABI and
runtime, the store over SQLite, the journal, the pty host, the agent socket,
and the library tests of the node and the terminal client. The C (SQLite,
zstd, ring) is compiled as usual, not instrumented: Apple's clang cannot share
a sanitizer runtime with rustc's LLVM. The address sanitizer still replaces
the allocator C uses, so a bad or double free inside C fails the run; a race
inside C does not.

Tests that start a net take amux and the fake providers built under the same
sanitizer (TESTNET_BINARIES), so the agents they start are checked too.

`thread` reads scripts/sanitize-thread.supp, the reports that are not races,
and runs on Linux only. On macOS it reported races on `errno` between a
finished thread and a later one given the same memory, which the sanitizer
cannot see being handed over, and the phone test's agent stop went
unanswered every time it ran alone (71 ms under `address`); the daemon learns
of an agent's exit from SIGCHLD there, and the sanitizer holds signals back.

`--cargo` is the nightly's cargo command; crate names after the kind narrow
the run.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import shlex
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
KINDS = ["address", "thread"]
# Every test target of these crates.
WHOLE = ["app-ffi", "app-runtime", "app-embedded", "store", "journal", "pty-host", "agent-dir"]
# Only the library tests: the node's and the terminal client's integration
# suites build amux mid-run with a cargo of their own, which takes the
# sanitizer flags without the target that keeps them off build scripts and
# proc macros, and fails to build.
LIBRARY = ["node", "tui"]


def host(cargo: list[str]) -> str:
    version = subprocess.run([*cargo, "-vV"], capture_output=True, text=True, check=True)
    for line in version.stdout.splitlines():
        if line.startswith("host: "):
            return line.removeprefix("host: ")
    raise SystemExit(f"sanitize: `{shlex.join(cargo)} -vV` names no host")


def packages(crates: list[str]) -> list[str]:
    return [arg for crate in crates for arg in ("-p", crate)]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--cargo", required=True)
    parser.add_argument("kind", choices=KINDS)
    parser.add_argument("crates", nargs="*")
    args = parser.parse_args()
    unknown = [crate for crate in args.crates if crate not in WHOLE + LIBRARY]
    if unknown:
        parser.error(f"unknown crates {unknown}; the crates are {WHOLE + LIBRARY}")
    chosen = args.crates or WHOLE + LIBRARY

    if args.kind == "thread" and sys.platform == "darwin":
        print("sanitize: the thread sanitizer runs on Linux only; see scripts/sanitize.py",
              file=sys.stderr)
        return 2

    cargo = shlex.split(args.cargo)
    triple = host(cargo)
    target_dir = ROOT / "target" / "sanitize" / args.kind
    # An explicit --target keeps the flags off build scripts and proc macros,
    # which run in the compiler's own uninstrumented process.
    build = ["--locked", "-Zbuild-std", "--target", triple, "--target-dir", str(target_dir)]
    flags = f"-Zsanitizer={args.kind}"
    env = dict(os.environ, RUSTFLAGS=flags, RUSTDOCFLAGS=flags,
               TESTNET_BINARIES=str(target_dir / triple / "debug"))
    if args.kind == "thread":
        suppressions = ROOT / "scripts" / "sanitize-thread.supp"
        env["TSAN_OPTIONS"] = f"suppressions={suppressions} {env.get('TSAN_OPTIONS', '')}".strip()

    runs = [
        ["build", *build, "-p", "amux", "-p", "provider-fakes", "--bins"],
    ]
    whole = [crate for crate in chosen if crate in WHOLE]
    if whole:
        runs.append(["test", *build, "--no-fail-fast", *packages(whole), "--lib", "--tests"])
    library = [crate for crate in chosen if crate in LIBRARY]
    if library:
        runs.append(["test", *build, "--no-fail-fast", *packages(library), "--lib"])

    for run in runs:
        print(f"sanitize {args.kind}: cargo {shlex.join(run)}", flush=True)
        if subprocess.run([*cargo, *run], cwd=ROOT, env=env).returncode != 0:
            print(f"sanitize: the {args.kind} sanitizer run failed", file=sys.stderr)
            return 1
    print(f"sanitize: {', '.join(chosen)} passed under the {args.kind} sanitizer")
    return 0


if __name__ == "__main__":
    sys.exit(main())
