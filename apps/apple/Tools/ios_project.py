#!/usr/bin/env python3
"""Generating apps/apple/Amux.xcodeproj, in one place.

Every recipe that builds the app regenerates the project first, and every one
of them has to apply the same repair afterwards: XcodeGen writes the StoreKit
configuration into the launch action alone. A recipe that generated the
project without repairing it would leave the committed scheme changed on disk
and hand the next test run an App Store with nothing in it.
"""

from pathlib import Path
import subprocess

# How every recipe builds for the simulator: for any device, so a build
# holds no lease and waits for none, and for arm64 alone. The project pins
# ARCHS for its own targets, but a local package's targets take the
# platform's defaults, and an Intel slice of a package's tests cannot link
# against the bridge, which is assembled for arm64 alone. One spelling, so
# every build shares the same products.
SIMULATOR_BUILD = ["-destination", "generic/platform=iOS Simulator", "ARCHS=arm64"]

SCHEME = Path("apps/apple/Amux.xcodeproj/xcshareddata/xcschemes/Amux.xcscheme")
STOREKIT = """      <StoreKitConfigurationFileReference
         identifier = "../../Amux/Amux.storekit">
      </StoreKitConfigurationFileReference>
"""


def generate() -> None:
    # The project is generated from apps/apple/project.yml and committed, so a
    # regeneration that changes it shows up in the diff like any other change.
    subprocess.run(
        ["xcodegen", "generate", "--spec", "project.yml", "--quiet"],
        cwd="apps/apple", check=True, timeout=300,
    )
    store_kit_in_tests()


def store_kit_in_tests() -> None:
    """Give the test action the same subscriptions the run action has.

    XcodeGen writes a StoreKit configuration into the launch action only, and
    Xcode reads the two actions' settings separately: without this, a test that
    launches the app gets an App Store with nothing in it, and every purchase
    path fails for a reason that has nothing to do with the app. Written here
    rather than by hand because the project is generated.
    """
    scheme = SCHEME.read_text()
    if "StoreKitConfigurationFileReference" not in scheme:
        raise RuntimeError(f"{SCHEME} has no StoreKit configuration to copy")
    if scheme.count("StoreKitConfigurationFileReference") > 2:
        return
    patched = scheme.replace("   </TestAction>", STOREKIT + "   </TestAction>", 1)
    if patched == scheme:
        raise RuntimeError(f"{SCHEME} has no test action to give a StoreKit configuration")
    SCHEME.write_text(patched)
