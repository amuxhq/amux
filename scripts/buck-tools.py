#!/usr/bin/env python3
"""Fetch the pinned build tools and print where they are.

`buck-tools.py path NAME` prints the path of NAME's binary, fetching it first
when this machine doesn't have the pinned version yet. `buck-tools.py fetch`
fetches every tool. The pins and their SHA-256 digests live in
build/tools.json; a download whose digest differs is refused.

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
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PINS = ROOT / "build/tools.json"
CACHE = Path(os.environ.get("AMUX_TOOLS_CACHE", Path.home() / ".cache/amux/tools"))


def host() -> str:
    machine = platform.machine().lower()
    system = platform.system()
    if system == "Darwin" and machine in ("arm64", "aarch64"):
        return "aarch64-apple-darwin"
    if system == "Linux" and machine in ("x86_64", "amd64"):
        return "x86_64-unknown-linux-gnu"
    # Windows on Arm (a Windows VM on an Apple Silicon Mac) runs the x64
    # builds under its emulation, as it runs the x64 binaries a Windows
    # test starts; the pins name only those.
    if system == "Windows" and machine in ("x86_64", "amd64", "arm64", "aarch64"):
        return "x86_64-pc-windows-msvc"
    sys.exit(f"buck-tools: no pinned tools for {system} {machine}")


def pins() -> dict:
    return {k: v for k, v in json.loads(PINS.read_text()).items() if not k.startswith("/")}


def version(name: str) -> str:
    """The pinned version for this host: an asset may name its own."""
    pin = pins()[name]
    return pin["assets"].get(host(), {}).get("version", pin["version"])


def binary(name: str) -> Path:
    return CACHE / name / version(name) / (name + (".exe" if platform.system() == "Windows" else ""))


def unzstd(data: bytes) -> bytes:
    # Python has zstd from 3.14; an older one (Debian's 3.13) uses the zstd
    # command. Inside `nix develop` nothing is fetched: the shell has them.
    try:
        from compression import zstd
    except ImportError:
        import subprocess

        try:
            return subprocess.run(["zstd", "-dc"], input=data, capture_output=True, check=True).stdout
        except (OSError, subprocess.CalledProcessError):
            sys.exit("buck-tools: unpacking needs Python 3.14 or the zstd command; or run inside `nix develop`")
    return zstd.decompress(data)


def fetch(name: str) -> Path:
    target = binary(name)
    if target.exists():
        return target
    pin = pins()[name]
    asset = pin["assets"].get(host())
    if asset is None:
        sys.exit(f"buck-tools: {name} {version(name)} has no asset for {host()}")
    print(f"buck-tools: fetching {name} {version(name)}", file=sys.stderr)
    with urllib.request.urlopen(asset["url"], timeout=300) as response:
        data = response.read()
    digest = hashlib.sha256(data).hexdigest()
    if digest != asset["sha256"]:
        sys.exit(
            f"buck-tools: {asset['url']} has SHA-256 {digest}, "
            f"but build/tools.json pins {asset['sha256']}"
        )
    if asset["url"].endswith(".zst"):
        contents = unzstd(data)
    elif asset["url"].endswith(".tar.gz"):
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
            member = archive.extractfile(asset["member"])
            if member is None:
                sys.exit(f"buck-tools: {asset['url']} has no {asset['member']}")
            contents = member.read()
    elif asset["url"].endswith(".zip"):
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            contents = archive.read(asset["member"])
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
