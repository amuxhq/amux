#!/usr/bin/env python3
"""Build and run the fast, app-hosted SwiftUI component snapshot suite."""

from argparse import ArgumentParser, Namespace
from contextlib import contextmanager
from pathlib import Path
import json
import re
import shutil
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).parent))
import ios_project
import ios_simulators

DERIVED_DATA = Path("target/ios/DerivedData").resolve()
ARTIFACTS = Path("target/ios/component-snapshots").resolve()
RESULT = Path("target/ios/ComponentSnapshots.xcresult").resolve()
TIMINGS = ARTIFACTS / "timing.json"
PROJECT = "apps/apple/Amux.xcodeproj"
SCHEME = "AmuxTests"
TARGET = "AmuxComponentSnapshotTests"
NEGATIVE_DEFAULT = "row.prompt"
MISMATCH = "does not match reference"
# Hang detector for one test-without-building batch (startup and every
# example), sized by docs/CI.md's rule: the slowest clean run times 1.5,
# rounded up to the hundred. An ordinary batch, where only the examples
# marked to settle late wait, takes about 330 s on a CI runner for 416
# examples (one cut off at 300 s had started in 37 s and drawn 379); one that
# settles every example (--settle-all, and every recording) took 487 s in the
# nightly captures run, whose test host took 180 s of that to start.
BATCH_BOUND = 500
SETTLED_BOUND = 800
# The same for a batch of review captures, every frosted example held 2.5 s
# for its glass: 372 s for all of them on a local Mac.
REVIEW_BOUND = 600


def arguments(argv: list[str]) -> Namespace:
    parser = ArgumentParser(
        description="Render stable component examples in-process and compare their PNG baselines."
    )
    parser.add_argument("components", nargs="*", metavar="COMPONENT_ID")
    parser.add_argument(
        "--record", action="store_true",
        help="deliberately replace the selected committed baselines",
    )
    parser.add_argument(
        "--skip-build", action="store_true",
        help="run the already-built test bundle (the warm iteration path)",
    )
    parser.add_argument(
        "--settle-all", action="store_true",
        help="wait for every example to stop changing, not only those marked settlesLate, and "
        "fail any unmarked example that changed after it reported ready",
    )
    parser.add_argument(
        "--negative-control", action="store_true",
        help="first verify, then perturb a selected component and require an image mismatch",
    )
    parser.add_argument(
        "--review", metavar="DIR", type=Path,
        help="write the selected components that wear a frosted surface to DIR with their glass "
        "on, in light and dark, for a person to look at; nothing is compared or recorded",
    )
    parsed = parser.parse_args(argv)
    if parsed.record and parsed.negative_control:
        parser.error("--record and --negative-control cannot be combined")
    if parsed.review and (parsed.record or parsed.negative_control):
        parser.error("--review writes pictures nothing compares; it takes neither --record nor --negative-control")
    return parsed


def command(action: str, destination: list[str]) -> list[str]:
    return [
        "xcodebuild", action,
        "-project", PROJECT,
        "-scheme", SCHEME,
        "-configuration", "Debug",
        *destination,
        "-derivedDataPath", str(DERIVED_DATA),
        f"-only-testing:{TARGET}",
        "-enableCodeCoverage", "NO",
        "-enableAddressSanitizer", "NO",
        "-enableThreadSanitizer", "NO",
        "-enableUndefinedBehaviorSanitizer", "NO",
    ]


def build() -> float:
    started = time.monotonic()
    # Built as `ios build` and `ios test-build` build, so the three share one
    # set of build products and none waits for a device.
    subprocess.run(
        command("build-for-testing", ios_project.SIMULATOR_BUILD), check=True, timeout=1800)
    elapsed = time.monotonic() - started
    print(f"component snapshot timing: build={elapsed:.3f}s", flush=True)
    return elapsed


@contextmanager
def forwarded(udid: str, variables: dict[str, str]):
    """Give the hosted test process an exact, temporary launchd environment."""
    installed = []
    try:
        for name, value in variables.items():
            subprocess.run(
                ["xcrun", "simctl", "spawn", udid, "launchctl", "setenv", name, value],
                check=True, timeout=120,
            )
            installed.append(name)
        yield
    finally:
        for name in reversed(installed):
            subprocess.run(
                ["xcrun", "simctl", "spawn", udid, "launchctl", "unsetenv", name],
                check=True, timeout=120,
            )


def run(udid: str, selected: list[str], *, record: bool = False, perturb: bool = False,
        review: Path | None = None, settle_all: bool = False):
    shutil.rmtree(ARTIFACTS, ignore_errors=True)
    shutil.rmtree(RESULT, ignore_errors=True)
    ARTIFACTS.mkdir(parents=True)
    values = {
        "AMUX_COMPONENT_SNAPSHOTS": "1",
        "AMUX_SNAPSHOT_ONLY": ",".join(selected),
        "AMUX_RECORD_SNAPSHOTS": "1" if record else "0",
        "AMUX_SNAPSHOT_PERTURB": "1" if perturb else "0",
        "AMUX_SNAPSHOT_SETTLE_ALL": "1" if settle_all else "0",
        # The simulator shares the Mac's file system, so the test writes the
        # review captures straight into the directory asked for.
        "AMUX_SNAPSHOT_REVIEW": str(review.resolve()) if review else "",
        # SnapshotTesting writes the newly rendered image here on a mismatch;
        # its reference/failure/difference attachments are exported below.
        "SNAPSHOT_ARTIFACTS": str(ARTIFACTS),
    }
    expected = (
        f"AMUX_SNAPSHOT_CONFIGURATION selected={','.join(sorted(selected)) or 'all'} "
        f"record={values['AMUX_RECORD_SNAPSHOTS']} "
        f"perturb={values['AMUX_SNAPSHOT_PERTURB']} "
        f"review={1 if review else 0} "
        f"settle-all={1 if settle_all or record else 0} host=1"
    )
    bound = REVIEW_BOUND if review else SETTLED_BOUND if settle_all or record else BATCH_BOUND
    with forwarded(udid, values | {"AMUX_SNAPSHOT_RUNNER_STARTED": str(time.monotonic())}):
        started = time.monotonic()
        try:
            completed = subprocess.run(
                [*command("test-without-building", ["-destination", f"id={udid}"]),
                 "-resultBundlePath", str(RESULT)],
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                timeout=bound,
            )
        except subprocess.TimeoutExpired as expired:
            # What the batch printed up to the bound (each example's timing,
            # each mismatch) is the evidence of where it stood.
            output = expired.stdout or ""
            output = output.decode(errors="replace") if isinstance(output, bytes) else output
            completed = subprocess.CompletedProcess(expired.cmd, 124, output)
            print(f"component snapshot batch passed its {bound}s bound", file=sys.stderr)
    elapsed = time.monotonic() - started
    if completed.returncode:
        export_failure_attachments()
    shutil.rmtree(RESULT, ignore_errors=True)
    print(completed.stdout, end="")
    print(f"component snapshot timing: test-and-startup={elapsed:.3f}s", flush=True)
    if completed.returncode == 0 and expected not in completed.stdout:
        print("test host did not echo the requested snapshot configuration", file=sys.stderr)
        completed.returncode = 2
    completed.amux_seconds = elapsed
    completed.amux_startup_seconds = timing(completed.stdout, "startup")
    completed.amux_batch_seconds = timing(completed.stdout, "batch")
    return completed


def export_failure_attachments() -> None:
    destination = ARTIFACTS / "attachments"
    exported = subprocess.run([
        "xcrun", "xcresulttool", "export", "attachments",
        "--path", str(RESULT),
        "--output-path", str(destination),
    ], text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=120)
    if exported.returncode:
        print("could not export SnapshotTesting failure attachments:", file=sys.stderr)
        print(exported.stdout, file=sys.stderr)


def timing(output: str, name: str) -> float | None:
    match = re.search(rf"AMUX_SNAPSHOT_TIMING {name}(?:=\d+)? seconds=([0-9.]+)", output)
    if match is None and name == "startup":
        match = re.search(r"AMUX_SNAPSHOT_TIMING startup=([0-9.]+)s", output)
    return float(match.group(1)) if match else None


def write_timings(options: Namespace, stages: dict) -> None:
    TIMINGS.parent.mkdir(parents=True, exist_ok=True)
    TIMINGS.write_text(json.dumps({
        "schema_version": 1,
        "components": options.components or "all",
        "recording": options.record,
        "warm_without_build": options.skip_build,
        "settle_all": options.settle_all,
        "negative_control": options.negative_control,
        "review": str(options.review) if options.review else None,
        **stages,
    }, indent=2) + "\n")
    print(f"component snapshot timings: {TIMINGS}", flush=True)


def main(argv: list[str] | None = None) -> int:
    total_started = time.monotonic()
    options = arguments(sys.argv[1:] if argv is None else argv)
    generated_started = time.monotonic()
    ios_project.generate()
    generated_seconds = time.monotonic() - generated_started
    ready_started = time.monotonic()
    udid = ios_simulators.ready("golden")
    ready_seconds = time.monotonic() - ready_started
    build_seconds = None
    if not options.skip_build:
        build_seconds = build()

    selected = options.components
    if options.negative_control and not selected:
        selected = [NEGATIVE_DEFAULT]
    if options.review:
        shutil.rmtree(options.review, ignore_errors=True)
    ordinary = run(udid, selected, record=options.record, review=options.review,
                   settle_all=options.settle_all)
    stages = {
        "components": selected or "all",
        "project_generation_seconds": generated_seconds,
        "simulator_ready_seconds": ready_seconds,
        "build_seconds": build_seconds,
        "startup_seconds": ordinary.amux_startup_seconds,
        "render_batch_seconds": ordinary.amux_batch_seconds,
        "test_and_startup_seconds": ordinary.amux_seconds,
        "total_seconds": time.monotonic() - total_started,
    }
    if ordinary.returncode:
        write_timings(options, stages)
        return ordinary.returncode
    if not options.negative_control:
        write_timings(options, stages)
        if options.review:
            written = sorted(options.review.glob("*.png")) if options.review.is_dir() else []
            print(f"{len(written)} review captures with glass on under {options.review}", flush=True)
        return 0

    changed = run(udid, selected, perturb=True)
    stages["negative_control_test_and_startup_seconds"] = changed.amux_seconds
    stages["total_seconds"] = time.monotonic() - total_started
    write_timings(options, stages)
    if changed.returncode == 0:
        print("negative control unexpectedly matched its baseline", file=sys.stderr)
        return 1
    if MISMATCH not in changed.stdout:
        print("negative control failed for a reason other than an image mismatch", file=sys.stderr)
        return changed.returncode
    print("negative control detected the deliberate visual change", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
