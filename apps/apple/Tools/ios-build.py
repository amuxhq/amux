#!/usr/bin/env python3
"""Build the iPhone app with Buck2 and put it where the recipes look.

    ios-build.py [--configuration debug|measured|release] [--device] [--tests]

Buck2 builds the app (apps/apple/BUCK); this copies it out of buck-out to
target/ios/<Configuration>-<sdk>/Amux.app, the one path every recipe and
tool that installs it reads, since a buck-out path changes with the
configuration's hash. The default is the Debug app every other recipe
drives; `measured` is the optimised app the performance suite drives.

`--tests` also builds every iPhone test target without running one, so a
later `buck2 test` only runs them, and puts the UI tests the accessibility
journey runs, with their runner, in target/ios/AmuxUITests.

`--device` builds for a phone instead and signs the copy for development
with the local signing file's Team ID and the team's development profiles
(build/apple/sign.py): Buck2's phone builds come out unsigned, since a
signature names a certificate in this Mac's keychain.
"""

from pathlib import Path
import argparse
import plistlib
import shutil
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
import ios_phone

TARGET = "//apps/apple:Amux"
PLATFORMS = {
    ("Debug", False): "//build/platforms:ios-sim-app-shipping-phone-dev",
    ("Measured", False): "//build/platforms:ios-sim-app-shipping-measured",
    ("Release", False): "//build/platforms:ios-sim-app-shipping-release",
    ("Debug", True): "//build/platforms:ios-app-shipping-phone-dev",
    ("Measured", True): "//build/platforms:ios-app-shipping-measured",
    ("Release", True): "//build/platforms:ios-app-shipping-release",
}
# Every iPhone test target: the package suites and the hosted app tests
# `just ios unit` runs, and the UI tests the accessibility journey runs.
TESTS = [
    "//apps/apple/Packages/AmuxCore:AmuxCoreTests",
    "//apps/apple/Packages/AmuxDesign:AmuxDesignTests",
    "//apps/apple/Packages/AmuxFeatures:AmuxFeaturesTests",
    "//apps/apple/Packages/AmuxPush:AmuxPushTests",
    "//apps/apple/Packages/AmuxShell:AmuxShellTests",
    "//apps/apple/Packages/AmuxTestSupport:AmuxTestSupportTests",
    "//apps/apple:AmuxAppTests",
]
UI_TEST_TARGET = "//apps/apple:AmuxUITests"
UI_TESTS = Path("target/ios/AmuxUITests")
# The profiles Xcode's automatic signing makes for a development build on a
# registered phone: the app's own, and the wildcard the extension takes.
DEVELOPMENT_PROFILES = {
    "sh.amux.app": "iOS Team Provisioning Profile: sh.amux.app",
    "sh.amux.app.notifications": "iOS Team Provisioning Profile: *",
}
ENTITLEMENTS = {
    "sh.amux.app": "apps/apple/Amux/Amux.entitlements",
    "sh.amux.app.notifications": "apps/apple/AmuxNotifications/AmuxNotifications.entitlements",
}


def application(configuration: str, device: bool = False) -> Path:
    sdk = "iphoneos" if device else "iphonesimulator"
    return Path("target/ios") / f"{configuration}-{sdk}" / "Amux.app"


def buck_build(targets: list[str], platform: str) -> list[Path]:
    built = subprocess.run(
        ["scripts/buck2", "build", *targets, "--target-platforms", platform,
         "--show-full-simple-output"],
        check=True, stdout=subprocess.PIPE, text=True, timeout=3600)
    return [Path(line) for line in built.stdout.split()]


def place(built: Path, destination: Path) -> None:
    if destination.exists():
        shutil.rmtree(destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(built, destination, symlinks=True)
    # Buck2's outputs are read-only; a copy is reinstalled and re-signed.
    subprocess.run(["chmod", "-R", "u+w", str(destination)], check=True, timeout=120)


def revision() -> str:
    return subprocess.run(["git", "rev-parse", "HEAD"], check=True, capture_output=True,
                          text=True, timeout=60).stdout.strip()


def sign_for_development(app: Path) -> None:
    signed = app.parent / "signed"
    command = [sys.executable, "-B", "build/apple/sign.py", str(app), str(signed),
               "--identity", "Apple Development", "--team", ios_phone.development_team(),
               "--revision", revision()]
    for bundle, name in DEVELOPMENT_PROFILES.items():
        command += ["--profile", f"{bundle}={name}"]
    for bundle, path in ENTITLEMENTS.items():
        command += ["--entitlements", f"{bundle}={path}"]
    subprocess.run(command, check=True, timeout=600)
    shutil.rmtree(app)
    (signed / app.name).rename(app)
    shutil.rmtree(signed)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--configuration", type=str.capitalize,
                        choices=["Debug", "Measured", "Release"], default="Debug")
    parser.add_argument("--tests", action="store_true")
    parser.add_argument("--device", action="store_true")
    arguments = parser.parse_args()
    if arguments.tests and (arguments.configuration != "Debug" or arguments.device):
        parser.error("--tests builds the simulator's Debug suites only")
    if arguments.device:
        # Refused before building: a build for a phone nobody plugged in is
        # minutes spent on nothing.
        ios_phone.connected()
    platform = PLATFORMS[(arguments.configuration, arguments.device)]
    [built] = buck_build([TARGET], platform)
    destination = application(arguments.configuration, arguments.device)
    place(built, destination)
    if arguments.device:
        sign_for_development(destination)
    print(f"built {destination}", flush=True)
    if arguments.tests:
        buck_build(TESTS, platform)
        [ui_tests] = buck_build([UI_TEST_TARGET], platform)
        place(ui_tests, UI_TESTS)
        # xcodebuild installs the app a UI test drives before running it,
        # and refuses a plan that does not say where it is: the app just
        # copied out, the one the journeys install.
        plan = UI_TESTS / "AmuxUITests.xctestrun"
        run = plistlib.loads(plan.read_bytes())
        run["AmuxUITests"]["UITargetAppPath"] = str(destination.resolve())
        plan.write_bytes(plistlib.dumps(run))
        print(f"built every iPhone test target, and the UI tests in {UI_TESTS}", flush=True)


if __name__ == "__main__":
    main()
