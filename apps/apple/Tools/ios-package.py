#!/usr/bin/env python3
"""Build every shipping slice of the bridge and assemble the XCFramework.

Simulator and device, under the `release` profile every shipped binary uses, without the
driving tools. The result is what the Swift package's binary target links and
what a Release archive ships, so the linkage smoke runs against it here and
the release recipes depend on this one.
"""

from pathlib import Path
import subprocess
import sys
import tomllib

sys.path.insert(0, str(Path(__file__).parent))
import ios_bridge as bridge

LINKAGE_SMOKE = Path("apps/apple/Tools/linkage_smoke.py")


def main() -> None:
    bridge.OUTPUT.mkdir(parents=True, exist_ok=True)
    framework = bridge.OUTPUT / bridge.FRAMEWORK
    linkage = bridge.OUTPUT / "simulator-linkage.txt"

    lines = []
    staged_dirs = []
    for triple in bridge.SHIPPING_TRIPLES:
        built = bridge.cargo_build(triple, profile=bridge.SHIPPING_PROFILE, features=(),
                                   log=bridge.OUTPUT / f"{triple}-build.jsonl")
        directory = bridge.OUTPUT / triple
        staged_dirs.append(directory)
        lines.append(bridge.size_line(triple, bridge.stage(built, directory)))
    headers = {(directory / "include" / bridge.HEADER).read_bytes() for directory in staged_dirs}
    if len(headers) != 1:
        raise RuntimeError("Device and simulator C headers differ")
    if bridge.package_if_changed(framework, staged_dirs, bridge.OUTPUT / "framework.sha256"):
        # A fresh framework has not been linked yet; the previous result says
        # nothing about it.
        linkage.unlink(missing_ok=True)
    # A successful shipping assembly owns this path now, even when its digest
    # was already current. Only the development recipe creates this marker.
    bridge.stand_in_marker(framework).unlink(missing_ok=True)

    profile = tomllib.loads(Path("Cargo.toml").read_text())["profile"][bridge.SHIPPING_PROFILE]
    text = bridge.write_size_report(lines, {"name": bridge.SHIPPING_PROFILE, **profile})
    print(text, end="", flush=True)
    if not linkage.is_file():
        # The smoke runs on a device, so it takes the phone lease for its own
        # short run rather than the whole package build holding one.
        subprocess.run(["scripts/with", "iphone", "--", sys.executable, str(LINKAGE_SMOKE), str(framework)],
                       check=True, timeout=600)


if __name__ == "__main__":
    main()
