#!/usr/bin/env python3
"""Run one XCTest bundle as a Buck2 test (build/apple.bzl's apple_xctest).

The bundle was built by Buck2; this writes the `.xctestrun` file Xcode would
have written for it and runs `xcodebuild test-without-building`, on a
simulator for an iPhone bundle and on this Mac for a Mac one. It runs inside
the test's action on a Mac's worker, so a passing result is cached like any
other and a second tree's run of the same inputs is a hit.

A simulator comes from wt's `iphone` pool, the one every recipe that drives
a simulator leases from, so a test never takes a device another tree is
using: `--pool-tree` names a tree whose .wt.toml defines the pool (the
checkout every tree of this Mac belongs to), and the lease is taken with
`wt with iphone` there. CI has no wt: its iPhone job makes and boots the
golden simulator before anything builds, and its NativeLink worker hands
every test that device's udid in AMUX_SIMULATOR_UDID, outside the action's
key (scripts/nativelink.py). Without either, as in a bare checkout, the test
looks up the first pinned device by name. Both hold a lock file on the
device, so two tests on one machine take turns.

Asking CoreSimulator anything is slow on a runner compiling on every core:
`simctl list` has taken over three minutes there, and xcodebuild has given
up on finding a device it was handed by udid. So a test handed its device
asks CoreSimulator nothing itself, and xcodebuild is run again when it gave
up on finding the device before running anything, since that says nothing
about the tests.

The verdict is xcodebuild's exit status and the result bundle's own count:
a run that executed no test fails, since a bundle whose tests were never
found would otherwise pass.
"""

from pathlib import Path
import argparse
import fcntl
import json
import os
import plistlib
import pwd
import shutil
import subprocess
import sys
import tempfile

POOL = "iphone"
LEASE = "WT_LEASE_IPHONE"
# The udid of the device CI's iPhone job prepared (.github/workflows/ci.yml).
PREPARED = "AMUX_SIMULATOR_UDID"
# What ios_simulators.py calls the first golden device outside a worktree.
FALLBACK_DEVICE = "amux-iphone-1"
RUNTIME = "com.apple.CoreSimulator.SimRuntime.iOS-26-5"
TIMEOUT = 1500
# What xcodebuild says when it gave up waiting for CoreSimulator to list the
# destination, and how many times it is asked in all.
NOT_FOUND = "Unable to find a device matching the provided destination"
ATTEMPTS = 3


def home() -> str:
    # The account's own, not the action's: CoreSimulator and xcodebuild keep
    # the devices and their state there.
    return pwd.getpwuid(os.getuid()).pw_dir


def absolute(value: str) -> str:
    """An environment value naming something in the action's inputs, as an
    absolute path: the test runs in a process that starts elsewhere."""
    if value.startswith(("buck-out/", "apps/", "crates/")) and Path(value).exists():
        return str(Path(value).resolve())
    return value


def xctestrun(arguments, scratch: Path) -> Path:
    mac = arguments.sdk == "macosx"
    platform = "MacOSX" if mac else "iPhoneSimulator"
    bundle = Path(arguments.bundle).resolve()
    environment = {
        "AMUX_SOURCE_ROOT": str(Path.cwd()),
        **{name: absolute(value) for name, value in (pair.split("=", 1) for pair in arguments.env)},
    }
    frameworks = f"__PLATFORMS__/{platform}.platform/Developer/Library/Frameworks"
    libraries = f"__PLATFORMS__/{platform}.platform/Developer/usr/lib"
    entry = {
        "TestBundlePath": str(bundle),
        "EnvironmentVariables": environment,
        "TestingEnvironmentVariables": {
            "DYLD_FRAMEWORK_PATH": frameworks,
            "DYLD_LIBRARY_PATH": libraries,
            **environment,
        },
    }
    if arguments.host:
        # Hosted: the app carries the bundle in PlugIns and loads it.
        host = Path(arguments.host).resolve()
        entry |= {
            "IsAppHostedTestBundle": True,
            "TestHostPath": str(host),
            "TestHostBundleIdentifier": arguments.host_identifier,
            "TestBundlePath": f"__TESTHOST__/PlugIns/{bundle.name}",
        }
        entry["TestingEnvironmentVariables"]["DYLD_INSERT_LIBRARIES"] = f"{libraries}/libXCTestBundleInject.dylib"
        entry["TestingEnvironmentVariables"]["XCInjectBundleInto"] = "unused"
    else:
        entry["TestHostPath"] = f"__PLATFORMS__/{platform}.platform/Developer/Library/Xcode/Agents/xctest"
    run = {arguments.name: entry, "__xctestrun_metadata__": {"FormatVersion": 1}}
    path = scratch / f"{arguments.name}.xctestrun"
    path.write_bytes(plistlib.dumps(run))
    return path


def udid_of(name: str) -> str:
    listed = json.loads(subprocess.run(
        ["xcrun", "simctl", "list", "devices", "available", "-j"],
        check=True, capture_output=True, text=True, timeout=180).stdout)
    for device in listed["devices"].get(RUNTIME, []):
        if device["name"] == name:
            return device["udid"]
    sys.exit(f"no simulator named {name} on {RUNTIME}; `just ios simulator golden` makes it")


def summary(result: Path) -> dict:
    found = subprocess.run(
        ["xcrun", "xcresulttool", "get", "test-results", "summary", "--path", str(result)],
        capture_output=True, text=True, timeout=300)
    if found.returncode:
        return {}
    return json.loads(found.stdout)


def failures(result: Path) -> list[str]:
    found = subprocess.run(
        ["xcrun", "xcresulttool", "get", "test-results", "tests", "--path", str(result)],
        capture_output=True, text=True, timeout=300)
    if found.returncode:
        return []
    lines = []

    def visit(node, path):
        name = node.get("name", "")
        if node.get("nodeType") == "Test Case" and node.get("result") == "Failed":
            lines.append("/".join(path + [name]))
            for child in node.get("children", []):
                if child.get("nodeType") == "Failure Message":
                    lines.append("    " + child.get("name", ""))
        for child in node.get("children", []):
            visit(child, path + ([name] if node.get("nodeType") in ("Test Suite", "Unit test bundle") else []))

    for node in json.loads(found.stdout).get("testNodes", []):
        visit(node, [])
    return lines


def run_tests(arguments, destination: str) -> int:
    scratch = Path(tempfile.mkdtemp(prefix="amux-xctest-", dir="/tmp"))
    try:
        plan = xctestrun(arguments, scratch)
        environment = os.environ | {"HOME": home(), "TMPDIR": str(scratch)}
        for attempt in range(1, ATTEMPTS + 1):
            # xcodebuild refuses a result bundle path that exists.
            result = scratch / f"result-{attempt}.xcresult"
            log = scratch / f"xcodebuild-{attempt}.log"
            command = ["xcodebuild", "test-without-building", "-xctestrun", str(plan),
                       "-destination", destination, "-resultBundlePath", str(result)]
            with log.open("w") as sink:
                try:
                    status = subprocess.run(command, stdout=sink, stderr=subprocess.STDOUT,
                                            stdin=subprocess.DEVNULL, env=environment,
                                            timeout=TIMEOUT).returncode
                except subprocess.TimeoutExpired:
                    status = None
            counts = summary(result)
            total = counts.get("totalTestCount", 0)
            if (status not in (0, None) and total == 0 and attempt < ATTEMPTS
                    and NOT_FOUND in log.read_text(errors="replace")):
                print(f"{arguments.name}: xcodebuild didn't find {destination}; asking again",
                      flush=True)
                continue
            break
        passed = status == 0 and counts.get("result") == "Passed" and total > 0
        if passed:
            print(f"{arguments.name}: {counts.get('passedTests', total)} passed, "
                  f"{counts.get('skippedTests', 0)} skipped")
            return 0
        print(f"{arguments.name}: FAILED "
              + ("(xcodebuild timed out)" if status is None else f"(xcodebuild exited {status})")
              + (f", {counts.get('failedTests', 0)} of {total} tests failed" if counts else ""))
        for line in failures(result):
            print(line)
        if total == 0:
            print("no test ran")
        print("--- xcodebuild's log, last 80 lines ---")
        print("\n".join(log.read_text(errors="replace").splitlines()[-80:]))
        return 1
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def on_device(udid: str, arguments, boot: bool) -> int:
    with open(f"/tmp/amux-simulator-{udid}.lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if boot:
            subprocess.run(["xcrun", "simctl", "bootstatus", udid, "-b"], check=True,
                           capture_output=True, timeout=900)
        return run_tests(arguments, f"id={udid}")


def on_simulator(arguments) -> int:
    prepared = os.environ.get(PREPARED)
    if prepared:
        # Made, booted and pinned by the job before anything built.
        return on_device(prepared, arguments, boot=False)
    leased = os.environ.get(LEASE)
    if leased:
        return run_tests(arguments, f"id={udid_of(leased)}")
    wt = shutil.which("wt") or str(Path(home()) / ".cargo/bin/wt")
    if arguments.pool_tree and Path(wt).exists() and Path(arguments.pool_tree).is_dir():
        # Leased in the pool's tree, then back here to run: wt's pool is
        # defined by that tree's .wt.toml, and `acquire` boots the device.
        return subprocess.run(
            [wt, "with", POOL, "--", sys.executable, str(Path(__file__).resolve()),
             "--cwd", str(Path.cwd()), *sys.argv[1:]],
            cwd=arguments.pool_tree, env=os.environ | {"HOME": home()},
        ).returncode
    return on_device(udid_of(FALLBACK_DEVICE), arguments, boot=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--sdk", required=True)
    parser.add_argument("--name", required=True)
    parser.add_argument("--bundle", required=True)
    parser.add_argument("--host")
    parser.add_argument("--host-identifier")
    parser.add_argument("--pool-tree")
    parser.add_argument("--env", action="append", default=[])
    parser.add_argument("--cwd")
    arguments = parser.parse_args()
    if arguments.cwd:
        os.chdir(arguments.cwd)
    if arguments.sdk == "macosx":
        sys.exit(run_tests(arguments, "platform=macOS,arch=arm64"))
    sys.exit(on_simulator(arguments))


if __name__ == "__main__":
    main()
