"""Cargo's metadata of the workspace's members (`cargo metadata --no-deps`).

For the checks that stay scripts and compare something with Cargo's view of
the crates (build/checks/parity.py, `tests-catalog.py check`). With a Linux
worker in reach it is //:cargo-metadata, which the worker's Cargo writes
(build/cargo.bzl), so the machine running the check needs no Rust toolchain
of its own: CI's Linux gate has none. Without one, this machine's Cargo is
asked, as before there was a worker to ask.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PY = str(ROOT / "scripts/py")


def read(path: Path) -> dict:
    """The metadata //:cargo-metadata wrote, with this checkout's root in
    place of the `@ROOT@` it writes for the worker's."""
    root = json.dumps(str(ROOT))[1:-1]
    return json.loads(path.read_text().replace("@ROOT@", root))


def load() -> dict:
    linux = subprocess.run(
        [PY, str(ROOT / "scripts/buck-config.py"), "linux"], cwd=ROOT, stdin=subprocess.DEVNULL
    ).returncode == 0
    if not linux:
        return json.loads(
            subprocess.check_output(
                ["cargo", "metadata", "--locked", "--format-version", "1", "--no-deps"], cwd=ROOT, text=True
            )
        )
    built = subprocess.run(
        [str(ROOT / "scripts/built"), "//:cargo-metadata"],
        cwd=ROOT, stdin=subprocess.DEVNULL, capture_output=True, text=True,
    )
    if built.returncode != 0:
        raise SystemExit(f"cargo metadata: building //:cargo-metadata failed:\n{built.stderr[-4000:]}")
    return read(Path(built.stdout.strip()))
