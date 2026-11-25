#!/usr/bin/env python3
"""Run clippy over the workspace's own crates, failing on any finding.

What `cargo clippy --workspace --all-targets -- -D warnings` does under
Cargo: every library, binary and test binary under crates/ is checked
(each target's `[clippy.txt]` subtarget, with the toolchain's clippy.toml
and denied lints), and any warning fails the run. Third-party crates are
not checked. Results are cached like any other action, so a warm run
checks only what changed.

    clippy.py [buck2 build arguments...]
"""

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
# scripts/buck2 is a shell script, which Windows can't start itself.
BUCK2 = (["sh"] if os.name == "nt" else []) + [str(ROOT / "scripts/buck2")]


def main() -> int:
    targets = subprocess.check_output(
        [*BUCK2, "uquery", "kind('^rust_(library|binary)$', //crates/...)"],
        cwd=ROOT,
        text=True,
        stderr=subprocess.DEVNULL,
    ).split()
    built = subprocess.run(
        # A denied lint fails its target's clippy action
        # (build/toolchains/bootstrap-guard.py), so every target is built
        # whatever fails, to report them all.
        [*BUCK2, "build", "--keep-going", *sys.argv[1:], "--show-full-output", *[t + "[clippy.txt]" for t in targets]],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if built.returncode != 0:
        sys.stderr.write(built.stderr)
        return built.returncode
    findings = 0
    for line in built.stdout.splitlines():
        _, _, path = line.partition(" ")
        text = Path(path).read_text() if path else ""
        if text.strip():
            findings += 1
            print(text.rstrip())
    if findings:
        print(f"\nclippy: findings in {findings} of {len(targets)} targets")
        return 1
    print(f"clippy: {len(targets)} targets clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())
