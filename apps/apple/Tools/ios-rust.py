#!/usr/bin/env python3
"""Build the bridge slices a driven build of the app links.

The simulator slice, with the driving tools compiled in, under the
`phone-dev` profile: what `ios build`, `ios unit` and every recipe that drives
an app need. Debug app configurations still carry symbols and testability on
the Swift side; the Rust bridge is built optimised, but without the
whole-program optimisation a release gets. `--device` adds the same library
for a plugged-in phone, which a driven build for that phone links; only the
recipes that build for one ask for it, so a simulator recipe never pays for a
second triple. The shipping library is `ios-package.py`'s.

When no Rust input has changed since the last run, cargo is not invoked and the
framework is left untouched, so a Swift-only edit costs no Rust work at all.
"""

from pathlib import Path
import argparse
import sys

try:
    import tomllib
except ModuleNotFoundError:  # Apple's command-line tools still ship Python 3.9.
    from pip._vendor import tomli as tomllib

sys.path.insert(0, str(Path(__file__).parent))
import ios_bridge as bridge

STAMP = bridge.OUTPUT / "rust-stamp.json"
PROFILE = bridge.DRIVING_PROFILE
FEATURES = bridge.DRIVING_FEATURES


def build_fingerprint(device: bool = False) -> str:
    """Everything that determines which driving archives may be reused."""
    simulator = f"{bridge.source_fingerprint()}:{PROFILE}:{','.join(FEATURES)}"
    return f"{simulator}:{bridge.DEVICE_TRIPLE}" if device else simulator


def packaged(framework: Path) -> bool:
    """Whether an xcframework holds the library every slice of it should."""
    slices = [entry for entry in framework.glob("*") if entry.is_dir()]
    return bool(slices) and all(
        (entry / bridge.LIBRARY).is_file() for entry in slices
    )


def main(device: bool = False) -> None:
    # Each triple and the slice xcodebuild names it in a framework. A build
    # with the phone's slice is still current for a simulator build, so a
    # simulator recipe after a phone one does not rebuild the bridge.
    slices = [(bridge.SIMULATOR_TRIPLE, bridge.DRIVING_SLICE)]
    if device:
        slices.append((bridge.DEVICE_TRIPLE, bridge.DRIVING_DEVICE_SLICE))
    bridge.OUTPUT.mkdir(parents=True, exist_ok=True)
    driving = bridge.OUTPUT / bridge.DRIVING_FRAMEWORK
    shipping = bridge.OUTPUT / bridge.FRAMEWORK
    stand_in = bridge.stand_in_marker(shipping)
    linked = [driving / name / bridge.LIBRARY for _, name in slices]
    triples = [triple for triple, _ in slices]
    fingerprint = build_fingerprint(device)
    accepted = {fingerprint, build_fingerprint(device=True)}
    # Both frameworks are asked for a library, not for a directory. A build
    # cache can restore an xcframework's shape without the archives inside it
    # -- they are the large part -- and a stamp alone would then call the
    # bridge current and leave xcodebuild to report `does not contain a binary
    # artifact` two stages later, which names neither the cache nor this
    # check. What was actually built is the only thing worth trusting.
    # `ios package` removes the marker when it puts the shipping library in
    # the stand-in's place, so a missing marker means the package tests
    # would link the shipping Rust: not current.
    def current(marker: Path) -> bool:
        return marker.is_file() and marker.read_text().strip() in accepted

    if (current(STAMP) and all(library.is_file() for library in linked)
            and packaged(shipping) and current(stand_in)):
        print("Rust sources unchanged; the bridge is current and cargo was not run", flush=True)
        return
    STAMP.unlink(missing_ok=True)

    staging = bridge.OUTPUT / "debug-tools"
    staged = {}
    for triple in triples:
        built = bridge.cargo_build(
            triple, profile=PROFILE, features=FEATURES,
            log=staging / f"{triple}-build.jsonl")
        staged[triple] = bridge.stage(built, staging / triple)
    bridge.package_if_changed(driving, [staging / triple for triple in triples],
                              staging / "framework.sha256")
    for library in linked:
        if not library.is_file():
            raise RuntimeError(
                f"{library} is missing. The Debug and Measured configurations of the app "
                "link this exact path (apps/apple/project.yml), so a change in how "
                "xcodebuild names the slice has to fail here rather than at link time.")
    # The Swift packages name the shipping framework as a binary target, so
    # their unit tests link whatever sits there, not the driving library the
    # app force-loads. A shipping build from before a Rust change would have
    # them test old Rust, so this slice replaces whatever sits there unless it
    # already is this slice. `ios package` records a digest over its own
    # slices in the same stamp, which never matches one development slice, so
    # a real shipping build is replaced by the next rebuild here, and
    # `ios package` rebuilds it over this stand-in before any shipping recipe
    # uses it. A restored build cache that left the framework's shape without
    # its archives is replaced the same way.
    if not packaged(shipping):
        (bridge.OUTPUT / "framework.sha256").unlink(missing_ok=True)
    if bridge.package_if_changed(shipping, [staging / triple for triple in triples],
                                 bridge.OUTPUT / "framework.sha256"):
        print(f"{shipping.name} now holds this development slice, so the package unit "
              "tests link the current Rust; `just ios package` builds the shipping library",
              flush=True)

    stand_in.write_text(fingerprint + "\n")

    profile = tomllib.loads(Path("Cargo.toml").read_text())["profile"].get(PROFILE, {})
    text = bridge.write_size_report(
        [bridge.size_line(triple, staged[triple], f" ({PROFILE}, debug tools)")
         for triple in triples],
        {"name": PROFILE, **profile})
    print(text, end="", flush=True)
    STAMP.write_text(fingerprint + "\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--device", action="store_true",
                        help="also build the slice a driven build for a phone links")
    main(device=parser.parse_args().device)
