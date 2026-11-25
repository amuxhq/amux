#!/usr/bin/env python3
"""Re-record the goldens: `just goldens [TARGET...]`.

A golden test compares what the code draws or emits with a file committed
beside it, and rewrites the file instead when its update switch is set. Under
`just test` the tests run in NativeLink's sandbox, where nothing they write
reaches the checkout, and no switch is set (scripts/no-update-flags.sh). This
runs the named test targets with `buck2 run` instead: the same runner, here,
from the checkout's root, with every update switch on, so each golden is
rewritten where it is committed. Review the diff, then commit it; the next
`just test` reruns only the tests that read what changed.

With no targets it re-records the goldens of the interpreters, the terminal
client and the views. The phone and terminal journeys keep their own
recipes (`just journey`, `just ios`).
"""

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
# scripts/buck2 is a shell script, which Windows can't start itself.
BUCK2 = (["sh"] if os.name == "nt" else []) + [str(ROOT / "scripts/buck2")]

# Each crate's switch; setting them all is harmless, since each test reads
# only its own.
SWITCHES = {
    "INTERPRET_UPDATE_GOLDENS": "1",
    "UPDATE_GOLDENS": "1",
    "UI_VIEW_UPDATE_GOLDENS": "1",
}

DEFAULT_TARGETS = [
    "//crates/interpret:unit",
    "//crates/interpret:integration",
    "//crates/tui:unit",
    "//crates/tui:integration",
    "//crates/ui-view:unit",
    "//crates/ui-view:integration",
]


def main() -> int:
    targets = sys.argv[1:] or DEFAULT_TARGETS
    env = dict(os.environ, **SWITCHES)
    failed = []
    for target in targets:
        print(f"goldens: {target}", flush=True)
        if subprocess.run([*BUCK2, "run", target], cwd=ROOT, env=env).returncode != 0:
            failed.append(target)
    changed = subprocess.run(
        ["git", "status", "--short", "--", "crates"], cwd=ROOT, capture_output=True, text=True
    ).stdout
    print("\nchanged:\n" + (changed or "  nothing\n"), end="")
    if failed:
        # A test that re-records still fails when what it checks beside the
        # golden is wrong.
        print(f"goldens: failed in {', '.join(failed)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
