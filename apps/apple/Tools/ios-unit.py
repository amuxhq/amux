#!/usr/bin/env python3
"""Run package and app-hosted unit suites on the golden simulator.

Every suite runs from the AmuxTests scheme, which holds each package's tests
beside the app-hosted ones, so one build serves them all. A `-only-testing:`
selector picks suites and the other arguments are handed to xcodebuild
unchanged; with none, every package suite and AmuxAppTests run. The component
pictures share the scheme but are `just ios component-snapshots`'s to run.
Tests that need the running application's UIKit, such as the report's frozen
frame, run in AmuxAppTests, hosted by the app rather than the package test
runner.

`--skip-build` runs the bundles the last build left behind, which is how the
gate runs them after building every suite once; it never verifies a source
edit.
"""

from contextlib import contextmanager
from pathlib import Path
import os
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
import ios_simulators
import ios_project

PACKAGES = Path("apps/apple/Packages")
DERIVED_DATA = Path("target/ios/DerivedData")
PROJECT = "apps/apple/Amux.xcodeproj"
SCHEME = "AmuxTests"
APP_HOSTED = "AmuxAppTests"


def suites() -> list[str]:
    """Every unit test target: the app-hosted one, then each package's."""
    return [APP_HOSTED] + [
        tests.name
        for package in sorted(PACKAGES.iterdir()) if package.is_dir()
        for tests in sorted((package / "Tests").glob("*")) if tests.is_dir()
    ]


def selected(arguments: list[str]) -> list[str]:
    """xcodebuild's arguments, selecting every suite when none was named."""
    known = suites()
    named = False
    for argument in arguments:
        if not argument.startswith("-only-testing"):
            continue
        target = argument.split(":", 1)[1].split("/", 1)[0]
        if target not in known:
            raise SystemExit(
                f"No suite owns the test target {target}; known targets: "
                + ", ".join(sorted(known))
            )
        named = True
    if named:
        return arguments
    return [*arguments, *(f"-only-testing:{suite}" for suite in known)]


def requested_updates() -> dict[str, str]:
    """Deliberate baseline rewrites, named by the person asking for one.

    A suite that pins a baseline rewrites it only when told to, and xcodebuild
    hands a simulator test process none of the shell's environment, so the
    request would otherwise never arrive. Anything named `AMUX_UPDATE_*` is
    forwarded; nothing else is, because the rest of the environment is the
    Mac's business and not the app's.
    """
    return {
        name: value for name, value in os.environ.items()
        if name.startswith("AMUX_UPDATE_")
    }


@contextmanager
def forwarded(udid: str, variables: dict[str, str]):
    """Put the requests where a process on the device will inherit them.

    The device's own launchd is the only place a test host reads them from,
    and it keeps them until told otherwise, so they are removed afterwards
    rather than left to change the meaning of the next run.
    """
    if variables:
        subprocess.run(
            ["xcrun", "simctl", "bootstatus", udid, "-b"], check=True, timeout=600)
    for name, value in variables.items():
        subprocess.run(
            ["xcrun", "simctl", "spawn", udid, "launchctl", "setenv", name, value],
            check=True, timeout=120)
        print(f"Asking for {name}={value}", flush=True)
    try:
        yield
    finally:
        for name in variables:
            subprocess.run(
                ["xcrun", "simctl", "spawn", udid, "launchctl", "unsetenv", name],
                check=True, timeout=120)


def command(udid: str, arguments: list[str], skip_build: bool) -> list[str]:
    return [
        "xcodebuild", "test-without-building" if skip_build else "test",
        "-project", PROJECT, "-scheme", SCHEME,
        "-configuration", "Debug", "-destination", f"id={udid}",
        "-derivedDataPath", str(DERIVED_DATA.resolve()), *arguments,
    ]


def main(argv: list[str]) -> None:
    skip_build = "--skip-build" in argv
    arguments = selected([argument for argument in argv if argument != "--skip-build"])
    if not skip_build:
        ios_project.generate()
    udid = ios_simulators.ready("golden")
    with forwarded(udid, requested_updates()):
        subprocess.run(command(udid, arguments, skip_build), check=True, timeout=1500)


if __name__ == "__main__":
    main(sys.argv[1:])
