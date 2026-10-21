#!/usr/bin/env python3
"""Audit every control on the app's live pages for a name and a hit area.

Links a markdown parser made out of a run of an agent's prose are judged on
their name but not on their size — a span of a sentence is not a control this
app draws and has no rectangle to grow. Each one is named here and in the
record so nothing small goes unreported.

The check itself is a UI test — XCUITest is the only accessibility client an
app cannot be for itself, so element kinds, VoiceOver names and rectangles are
only real from over there. The pages are reached here, the way the whole-screen
goldens reach theirs: the goldens' topology is served, the debug app installed
fresh and paired with the desk by the code it printed, and the app tapped to
each page while the desk's agents act. At each page this asks the running test
to audit what is on screen and waits for its answer; the two take turns
through files in a directory both can see. The record the test leaves in its
container is copied out to somewhere a person can read it.
"""

from pathlib import Path
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).parent))
sys.path.insert(0, str(Path("apps/apple/Tools").resolve()))
import ios_simulators  # noqa: E402
from journeys.phone import ROOT, PhoneJourney  # noqa: E402

DERIVED_DATA = Path("target/ios/DerivedData")
OUTPUT = Path("target/ios/accessibility")
SIMULATOR = "golden"
TEST = "AmuxUITests/AccessibilityAuditTests"
RECORD = "accessibility-audit.json"
UI_TESTS = "sh.amux.AmuxUITests.xctrunner"
# How long the test may take to build and start, and to audit one page.
STARTING = 900
AUDITING = 300


def goldens_module():
    spec = importlib.util.spec_from_file_location("ios_goldens", Path(__file__).with_name("ios-goldens.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


GOLDENS = goldens_module()


class Audit(GOLDENS.Goldens):
    """The goldens' way through the app, auditing each page where the goldens
    photograph it."""

    def __init__(self, journey: PhoneJourney, manifest: dict, directory: Path, test: subprocess.Popen):
        super().__init__(journey, manifest, set())
        self.directory = directory
        self.test = test
        self.pages: list[tuple[str, int]] = []

    def photograph(self, screen: str, frame: str | None = None, volatile: tuple[str, ...] = ()) -> None:
        self.audit(screen)

    def audit(self, page: str) -> None:
        """Asks the test to audit the page on screen, and waits until it has."""
        self.journey.app({"kind": "settle"})
        index = len(self.pages)
        written = self.directory / f".page-{index}.json"
        written.write_text(json.dumps({"page": page, "port": self.journey.port}))
        written.rename(self.directory / f"page-{index}.json")
        done = self.directory / f"page-{index}.done"
        deadline = time.monotonic() + AUDITING
        while not done.is_file():
            if self.test.poll() is not None:
                raise RuntimeError(f"the audit ended before it answered for {page}")
            if time.monotonic() > deadline:
                raise RuntimeError(f"the audit never answered for {page}")
            time.sleep(0.2)
        controls = json.loads(done.read_text())["controls"]
        self.pages.append((page, controls))
        self.journey.actions.append(f"audited {page}: {controls} controls")
        print(f"audited {page}: {controls} controls", flush=True)

    def you(self) -> None:
        self.journey.settings()
        self.audit("you")
        self.journey.tap("you.close")
        self.journey.wait(lambda drawn: "you" not in drawn, "Settings put away")

    def new_agent(self) -> None:
        self.journey.tab("Agents")
        self.journey.wait_for("home.newAgent")
        self.journey.tap("home.newAgent")
        self.journey.wait_for("new-agent")
        self.audit("new-agent")


def main() -> int:
    manifest = json.loads(GOLDENS.MANIFEST.read_text())
    udid = ios_simulators.ready(SIMULATOR)
    OUTPUT.mkdir(parents=True, exist_ok=True)
    stale = subprocess.run(
        ["xcrun", "simctl", "get_app_container", udid, UI_TESTS, "data"],
        text=True, capture_output=True, timeout=120)
    if stale.returncode == 0:
        (Path(stale.stdout.strip()) / "tmp" / RECORD).unlink(missing_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="amux-audit-", dir="/tmp"))
    log = OUTPUT / "audit.log"
    journey = PhoneJourney({"id": "accessibility"}, ROOT / manifest["topology"], udid, output=OUTPUT / "journey")
    test = None
    audit = None
    try:
        with log.open("w") as sink:
            test = subprocess.Popen([
                "xcodebuild", "test",
                "-project", "apps/apple/Amux.xcodeproj",
                "-scheme", "Amux",
                "-configuration", "Debug",
                "-destination", f"id={udid}",
                "-derivedDataPath", str(DERIVED_DATA.resolve()),
                "-only-testing", TEST,
                "-quiet",
            ], stdout=sink, stderr=subprocess.STDOUT, env=os.environ | {
                "TEST_RUNNER_AMUX_AUDIT_DIRECTORY": str(directory),
            })
        deadline = time.monotonic() + STARTING
        while not (directory / "ready").is_file():
            if test.poll() is not None or time.monotonic() > deadline:
                raise RuntimeError(f"the audit never started; see {log}")
            time.sleep(0.5)
        # Installed and launched once the test has started: the test run
        # installs the app it targets, which would end one already running.
        journey.launch()
        audit = Audit(journey, manifest, directory, test)
        audit.prepare_the_desk()
        audit.pairing()
        audit.hosts()
        audit.fleet()
        audit.chat_turn()
        audit.question()
        audit.escape()
        audit.composer_chips()
        audit.you()
        audit.new_agent()
        (directory / "end").touch()
        returned = test.wait(timeout=AUDITING)
        journey.finish([f"{page}: {controls} controls" for page, controls in audit.pages])
    except BaseException as error:
        journey.fail(error)
        raise
    finally:
        if test is not None and test.poll() is None:
            test.kill()
            test.wait(timeout=60)
        journey.close()
        shutil.rmtree(directory, ignore_errors=True)

    written = collect(udid)
    if returned != 0:
        print(f"the audit failed; what it found is in {log}", file=sys.stderr)
        if written is not None:
            for fault in json.loads(written.read_text()).get("faults", []):
                print(f"  {fault}", file=sys.stderr)
        return 1
    if written is None:
        print("the audit passed but left no record behind", file=sys.stderr)
        return 1
    found = json.loads(written.read_text())
    inline = found.get("inlineLinks", [])
    print(f"{found['controls']} controls across {len(found['pages'])} live pages "
          f"({', '.join(found['pages'])}): every one named, and every one the app draws "
          f"at least 44 pt (links inside agent prose, judged on their names alone: "
          f"{len(inline)})")
    for link in inline:
        print(f"  inline link in {link['state']}: "
              f"{link['label']!r} -> {link['url']} ({link['size']})")
    print(f"record: {written}")
    return 0


def collect(udid: str) -> Path | None:
    """Copies the record out of the test process's own container."""
    found = subprocess.run(
        ["xcrun", "simctl", "get_app_container", udid, UI_TESTS, "data"],
        text=True, capture_output=True, timeout=120)
    if found.returncode != 0:
        return None
    source = Path(found.stdout.strip()) / "tmp" / RECORD
    if not source.is_file():
        return None
    destination = OUTPUT / RECORD
    shutil.copyfile(source, destination)
    source.unlink()
    return destination


if __name__ == "__main__":
    raise SystemExit(main())
