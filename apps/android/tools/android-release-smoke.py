#!/usr/bin/env python3
"""Run the release launch smoke test on a device.

Invoked via `just android release-smoke`. Buck2 builds the minified release
APK with the test's R8 rules (`//apps/android/app:release-smoke`, signed with
the debug key) and the test APK that instruments it
(`//apps/android/app:release-smoke-test`) on a Linux worker; this installs
both on the device ANDROID_SERIAL names, which the recipe boots the pinned
emulator for when none is named, and runs the test with `am instrument`.
Nothing is built here: the APKs come from the cache.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
import android_sdk  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parents[3]
BUILT = REPO_ROOT / "scripts" / "built"
APP_TARGET = "//apps/android/app:release-smoke"
TEST_TARGET = "//apps/android/app:release-smoke-test"
APP_PACKAGE = "sh.amux.android"
RUNNER = "sh.amux.android.test/androidx.test.runner.AndroidJUnitRunner"

# `am instrument -r`'s summary of a run whose every test passed.
PASSED = re.compile(r"^OK \((\d+) tests?\)$", re.M)


def built(target: str) -> Path:
    out = subprocess.run(["sh", str(BUILT), target], cwd=REPO_ROOT, check=True, capture_output=True, text=True, timeout=3000)
    return Path(out.stdout.strip())


def passed(output: str) -> int | None:
    """How many tests `am instrument` ran when all passed, else None."""
    if "FAILURES!!!" in output or "INSTRUMENTATION_FAILED" in output:
        return None
    match = PASSED.search(output)
    return int(match.group(1)) if match else None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.parse_args(argv)
    serial = os.environ.get("ANDROID_SERIAL")
    if not serial:
        print("release-smoke: ANDROID_SERIAL names no device", file=sys.stderr)
        return 2
    sdk_root = android_sdk.sdk()
    if sdk_root is None:
        print("release-smoke: no Android SDK on this machine (ANDROID_HOME)", file=sys.stderr)
        return 2
    adb = str(sdk_root / "platform-tools" / "adb")

    app, test = built(APP_TARGET), built(TEST_TARGET)
    print(f"release-smoke: installing {app.name} and {test.name} on {serial}", flush=True)
    # The release build replaces whatever build of the app is there: one
    # signed with another key can't be updated in place.
    subprocess.run([adb, "-s", serial, "uninstall", APP_PACKAGE], capture_output=True, timeout=120)
    for apk in (app, test):
        subprocess.run([adb, "-s", serial, "install", "-r", "-t", str(apk)], check=True, timeout=300)
    result = subprocess.run(
        [adb, "-s", serial, "shell", "am", "instrument", "-w", "-r", RUNNER],
        capture_output=True,
        text=True,
        timeout=900,
    )
    output = result.stdout + result.stderr
    print(output, flush=True)
    count = passed(output)
    if count is None:
        print("release-smoke: FAILED", file=sys.stderr)
        return 1
    print(f"release-smoke: PASS ({count} test{'s' if count != 1 else ''} on the minified release build)", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
