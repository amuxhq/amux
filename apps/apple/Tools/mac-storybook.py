#!/usr/bin/env python3
"""Build the Mac storybook, and watch its sources or capture its stories."""

from argparse import ArgumentParser
from pathlib import Path
import os
import re
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[3]
DERIVED = ROOT / "target/mac/StorybookData"
STATE = ROOT / "target/mac/storybook"
LOG = STATE / "build.log"
STATUS = STATE / "status"
PLACE = STATE / "place.json"
LIST = ROOT / "tests/stories.json"
APP = DERIVED / "Build/Products/Debug/AmuxStorybook.app"
BINARY = APP / "Contents/MacOS/AmuxStorybook"
MAC = ROOT / "apps/apple/Mac"
SPEC = MAC / "project.yml"
PROJECT = MAC / "AmuxMac.xcodeproj"
FRAMEWORK = ROOT / "target/ios/AmuxApp.xcframework"
WATCHED = [
    MAC / "Storybook",
    ROOT / "apps/apple/Packages/AmuxMacScreens",
    ROOT / "apps/apple/Packages/AmuxCore/Sources/AmuxValues",
    SPEC,
    LIST,
]
# Hang detectors, not expectations: on a desk a build with an empty cache took
# 15 s and a capture of every story 250 s.
BUILD_BOUND = 300
CAPTURE_BOUND = 600
POLL_SECONDS = 0.4


def newest(paths: list[Path]) -> float:
    """The latest modification time among `paths` and what they hold."""
    latest = 0.0
    for path in paths:
        files = path.rglob("*") if path.is_dir() else [path]
        for file in files:
            if ".build" not in file.parts and file.is_file():
                latest = max(latest, file.stat().st_mtime)
    return latest


def first_error(log: str) -> str:
    """The compiler's first complaint, or where to read the whole log."""
    for line in log.splitlines():
        if re.search(r"\berror:", line):
            return line.replace(f"{ROOT}/", "").strip()
    return f"Build failed; see {LOG.relative_to(ROOT)}"


def build() -> str | None:
    """Builds the storybook, answering what went wrong or None."""
    STATE.mkdir(parents=True, exist_ok=True)
    # The package graph needs the framework to exist, though nothing the
    # storybook links uses it.
    if not FRAMEWORK.exists():
        subprocess.run(
            [sys.executable, "-B", "apps/apple/Tools/ios-rust.py", "--mac"],
            cwd=ROOT, check=True, timeout=1800)
    pbxproj = PROJECT / "project.pbxproj"
    if not pbxproj.exists() or SPEC.stat().st_mtime > pbxproj.stat().st_mtime:
        subprocess.run(
            ["xcodegen", "generate", "--spec", "project.yml", "--quiet"],
            cwd=MAC, check=True, timeout=300)
    with LOG.open("w") as log:
        done = subprocess.run(
            ["xcodebuild", "-project", str(PROJECT), "-scheme", "AmuxMacStorybook",
             "-configuration", "Debug", "-destination", "platform=macOS,arch=arm64",
             "-derivedDataPath", str(DERIVED), "-quiet",
             "COMPILER_INDEX_STORE_ENABLE=NO", "build"],
            cwd=ROOT, stdout=log, stderr=subprocess.STDOUT, timeout=BUILD_BOUND)
    return None if done.returncode == 0 else first_error(LOG.read_text())


def running() -> list[int]:
    """The storybooks this checkout has running."""
    found = subprocess.run(
        ["pgrep", "-f", f"^{re.escape(str(BINARY))}( |$)"], capture_output=True, text=True)
    return [int(pid) for pid in found.stdout.split()]


def stop() -> None:
    for pid in running():
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    deadline = time.monotonic() + 5
    while running() and time.monotonic() < deadline:
        time.sleep(0.05)


def launch(story: str | None, background: bool) -> None:
    command = ["open", "-n"] + (["-g"] if background else []) + [str(APP), "--args"]
    command += ["--list", str(LIST), "--place", str(PLACE), "--status", str(STATUS)]
    command += ["--story", story] if story else []
    command += ["--background"] if background else []
    subprocess.run(command, check=True, timeout=30)
    deadline = time.monotonic() + 10
    while not running() and time.monotonic() < deadline:
        time.sleep(0.05)


def say(status: str) -> None:
    STATE.mkdir(parents=True, exist_ok=True)
    STATUS.write_text(status + "\n")


def capture(out: str, only: str | None) -> int:
    failure = build()
    if failure:
        print(failure, file=sys.stderr)
        return 1
    command = [str(BINARY), "--list", str(LIST), "--capture", str(Path(out).resolve())]
    command += ["--only", only] if only else []
    return subprocess.run(command, timeout=CAPTURE_BOUND).returncode


def watch(story: str | None, background: bool) -> int:
    say("")
    stamp = newest(WATCHED)
    failure = build()
    if failure:
        print(failure, file=sys.stderr)
    else:
        stop()
        launch(story, background)
    launched = not failure
    print("Watching the Mac screens; Ctrl-C or quitting the storybook stops.")
    try:
        # Waits out a first build that failed; after that, the viewer closing
        # ends the watch, even while the latest build is broken.
        while running() or not launched:
            time.sleep(POLL_SECONDS)
            now = newest(WATCHED)
            if now == stamp:
                continue
            stamp = now
            say("Building…")
            started = time.monotonic()
            failure = build()
            if failure:
                say(f"Build failed: {failure}")
                print(failure, file=sys.stderr)
                continue
            say("")
            stop()
            launch(None, background=True)
            launched = True
            print(f"Rebuilt in {time.monotonic() - started:.1f} s.")
    except KeyboardInterrupt:
        pass
    finally:
        stop()
    return 0


def main() -> int:
    parser = ArgumentParser(description=__doc__)
    parser.add_argument("--story", help="the story to open on; the last one looked at by default")
    parser.add_argument("--background", action="store_true", help="open behind what is in front")
    commands = parser.add_subparsers(dest="command")
    drawing = commands.add_parser("capture", help="draw every story to PNG files and an index")
    drawing.add_argument("--out", required=True)
    drawing.add_argument("--only", help="only stories whose names contain this")
    arguments = parser.parse_args()
    if arguments.command == "capture":
        return capture(arguments.out, arguments.only)
    return watch(arguments.story, arguments.background)


if __name__ == "__main__":
    sys.exit(main())
