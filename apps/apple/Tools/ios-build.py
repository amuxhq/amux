#!/usr/bin/env python3
"""Generate the Xcode project and build the app for the simulator.

`--configuration Measured` builds the optimised app the performance suite
drives; the default is the Debug app every other recipe drives. `--tests`
builds the Debug app together with every suite `ios unit` and
`ios component-snapshots` run, so both can run with `--skip-build`.
`--device` builds for the phone plugged into this Mac instead, signed under
the local signing file's Team ID; the bridge's phone slice comes from
`ios-rust.py --device`.
"""

from pathlib import Path
import argparse
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
import ios_phone
import ios_project

DERIVED_DATA = Path("target/ios/DerivedData")


def build(configuration: str, tests: bool = False, phone: ios_phone.Phone | None = None) -> None:
    if phone is None:
        destination = ios_project.SIMULATOR_BUILD
    else:
        destination = [
            "-destination", f"id={phone.udid}", "-allowProvisioningUpdates",
            "CODE_SIGNING_ALLOWED=YES", "CODE_SIGN_STYLE=Automatic",
            f"DEVELOPMENT_TEAM={ios_phone.development_team()}",
        ]
    subprocess.run([
        "xcodebuild", "build-for-testing" if tests else "build",
        "-project", "apps/apple/Amux.xcodeproj",
        "-scheme", "AmuxTests" if tests else "Amux",
        "-configuration", configuration,
        *destination,
        "-derivedDataPath", str(DERIVED_DATA),
        "-quiet",
    ], check=True, timeout=2400 if phone else 1500)


def application(configuration: str, device: bool = False) -> Path:
    platform = "iphoneos" if device else "iphonesimulator"
    return DERIVED_DATA / f"Build/Products/{configuration}-{platform}/Amux.app"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--configuration", choices=["Debug", "Measured"], default="Debug")
    parser.add_argument("--tests", action="store_true")
    parser.add_argument("--device", action="store_true")
    arguments = parser.parse_args()
    if arguments.tests and arguments.configuration != "Debug":
        parser.error("--tests builds the Debug suites only")
    if arguments.tests and arguments.device:
        parser.error("--tests builds for the simulator only")
    phone = ios_phone.connected() if arguments.device else None
    ios_project.generate()
    build(arguments.configuration, tests=arguments.tests, phone=phone)
    built = application(arguments.configuration, device=phone is not None)
    if not built.is_dir():
        raise RuntimeError(f"{built} was not produced")
    print(f"built {built}", flush=True)


if __name__ == "__main__":
    main()
