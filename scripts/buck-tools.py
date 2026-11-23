#!/usr/bin/env python3
"""Fetch the pinned build tools and print where they are.

`buck-tools.py path NAME` prints the path of NAME's binary, fetching it first
when this machine doesn't have the pinned version yet. `buck-tools.py fetch`
fetches every tool. The pins and their SHA-256 digests live in
buck/tools.json; a download whose digest differs is refused.

Tools land in ~/.cache/amux/tools/NAME/VERSION/, so every worktree on a
machine shares one copy, and a version bump never overwrites a binary a
running daemon still uses.
"""

import hashlib
import io
import json
import os
import platform
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PINS = ROOT / "buck/tools.json"
CACHE = Path(os.environ.get("AMUX_TOOLS_CACHE", Path.home() / ".cache/amux/tools"))


def host() -> str:
    machine = platform.machine().lower()
    system = platform.system()
    if system == "Darwin" and machine in ("arm64", "aarch64"):
        return "aarch64-apple-darwin"
    if system == "Linux" and machine in ("x86_64", "amd64"):
        return "x86_64-unknown-linux-gnu"
    sys.exit(f"buck-tools: no pinned tools for {system} {machine}")


def pins() -> dict:
    return {k: v for k, v in json.loads(PINS.read_text()).items() if not k.startswith("/")}


def binary(name: str) -> Path:
    pin = pins()[name]
    return CACHE / name / pin["version"] / name


def fetch(name: str) -> Path:
    target = binary(name)
    if target.exists():
        return target
    pin = pins()[name]
    asset = pin["assets"].get(host())
    if asset is None:
        sys.exit(f"buck-tools: {name} {pin['version']} has no asset for {host()}")
    print(f"buck-tools: fetching {name} {pin['version']}", file=sys.stderr)
    with urllib.request.urlopen(asset["url"], timeout=300) as response:
        data = response.read()
    digest = hashlib.sha256(data).hexdigest()
    if digest != asset["sha256"]:
        sys.exit(
            f"buck-tools: {asset['url']} has SHA-256 {digest}, "
            f"but buck/tools.json pins {asset['sha256']}"
        )
    if asset["url"].endswith(".zst"):
        from compression import zstd

        contents = zstd.decompress(data)
    elif asset["url"].endswith(".tar.gz"):
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
            member = archive.extractfile(asset["member"])
            if member is None:
                sys.exit(f"buck-tools: {asset['url']} has no {asset['member']}")
            contents = member.read()
    else:
        contents = data
    target.parent.mkdir(parents=True, exist_ok=True)
    # Written beside the target and renamed into place, so two worktrees
    # fetching at once never see a half-written binary.
    with tempfile.NamedTemporaryFile(dir=target.parent, delete=False) as out:
        out.write(contents)
    os.chmod(out.name, 0o755)
    os.replace(out.name, target)
    return target


def main() -> None:
    args = sys.argv[1:]
    if len(args) == 2 and args[0] == "path":
        print(fetch(args[1]))
    elif args == ["fetch"]:
        for name in pins():
            print(fetch(name))
    else:
        sys.exit("usage: buck-tools.py path NAME | fetch")


if __name__ == "__main__":
    main()
