#!/usr/bin/env python3
"""Build and run the complete Rust store suite on the pinned iOS simulator."""

import json
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
sys.path.insert(1, str(Path(__file__).resolve().parents[3] / "scripts"))
import ios_simulators
import sqlite_linkage


TRIPLE = "aarch64-apple-ios-sim"
# The unit tests (named after the crate) and the integration test binary,
# which holds every integration suite.
EXPECTED_TESTS = {"store", "integration"}


def build_tests() -> dict[str, Path]:
    command = [
        "cargo", "test", "--locked", "-p", "store", "--target", TRIPLE,
        "--no-run", "--message-format=json-render-diagnostics",
    ]
    completed = subprocess.run(
        command, capture_output=True, text=True, timeout=1800,
    )
    if completed.stderr:
        print(completed.stderr, file=sys.stderr, end="")
    if completed.returncode != 0:
        if completed.stdout:
            print(completed.stdout, file=sys.stderr, end="")
        completed.check_returncode()

    executables: dict[str, Path] = {}
    for line in completed.stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if message.get("reason") != "compiler-artifact" or not message.get("executable"):
            continue
        if "/crates/store#" not in message.get("package_id", ""):
            continue
        target = message["target"]
        if not target.get("test"):
            continue
        features = set(message.get("features", []))
        if "bundled" not in features:
            raise RuntimeError(
                f"store test {target['name']} was built without the bundled feature"
            )
        executables[target["name"]] = Path(message["executable"])
    missing = EXPECTED_TESTS - executables.keys()
    unexpected = executables.keys() - EXPECTED_TESTS
    if missing or unexpected:
        raise RuntimeError(
            f"store test inventory changed; missing={sorted(missing)}, "
            f"unexpected={sorted(unexpected)}"
        )
    return executables


def run_on_simulator(udid: str, executable: Path) -> None:
    command = [
        "xcrun", "simctl", "spawn", udid, str(executable.resolve()), "--nocapture",
    ]
    print(f"\nRunning {executable.name} on {udid}", flush=True)
    completed = subprocess.run(command, capture_output=True, text=True, timeout=900)
    if completed.stdout:
        print(completed.stdout, end="", flush=True)
    if completed.stderr:
        print(completed.stderr, file=sys.stderr, end="", flush=True)
    completed.check_returncode()


def main() -> None:
    executables = build_tests()
    # Every suite opens real databases, so each binary must carry SQLite
    # itself rather than load the system's; the integration binary's is shown.
    print(sqlite_linkage.inspect(executables["integration"]).render(), end="", flush=True)

    udid = ios_simulators.ensure("golden")
    ios_simulators.run("xcrun", "simctl", "bootstatus", udid, "-b", timeout=600)
    print(f"iOS store suite: {ios_simulators.device_name('golden')} ({udid})", flush=True)
    for name in sorted(executables):
        run_on_simulator(udid, executables[name])


if __name__ == "__main__":
    main()
