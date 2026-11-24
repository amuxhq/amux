#!/usr/bin/env python3
"""Where this machine keeps the Android SDK, its NDK and their tools.

Every Android script finds them here, so they all agree on which SDK and which
NDK a build uses. `ANDROID_HOME` (or the older `ANDROID_SDK_ROOT`) names the
SDK, and the usual install folders on macOS and Linux are tried after them.
`ANDROID_NDK_HOME` (or `ANDROID_NDK_ROOT`) names the NDK; without one, the
newest NDK installed in the SDK is used, compared by version number, so 28.2
is newer than 9.0 and 27.10 newer than 27.9.

Run as a script, `sdk` prints the SDK's folder and `ndk` the NDK's, and each
exits 1 when there is none.
"""

from __future__ import annotations

import os
from pathlib import Path
import sys
from typing import Mapping


def _lookup(env: Mapping[str, str] | None) -> Mapping[str, str]:
    return os.environ if env is None else env


def sdk(env: Mapping[str, str] | None = None) -> Path | None:
    """The Android SDK's folder, or None when this machine has none."""
    lookup = _lookup(env)
    candidates = [
        lookup.get("ANDROID_HOME"),
        lookup.get("ANDROID_SDK_ROOT"),
        str(Path.home() / "Library" / "Android" / "sdk"),
        str(Path.home() / "Android" / "Sdk"),
    ]
    for candidate in candidates:
        if candidate and Path(candidate).is_dir():
            return Path(candidate)
    return None


def require_sdk(env: Mapping[str, str] | None = None) -> Path:
    found = sdk(env)
    if found is None:
        raise RuntimeError(
            "Android SDK not found; set ANDROID_HOME or install the SDK under "
            "~/Library/Android/sdk"
        )
    return found


def version_key(name: str) -> tuple[int, ...] | None:
    """An installed version folder's name as numbers, or None for any other folder."""
    parts = name.split(".")
    if not all(part.isdigit() for part in parts):
        return None
    return tuple(int(part) for part in parts)


def newest_version(folder: Path) -> Path | None:
    """The folder under `folder` named for the highest version number."""
    if not folder.is_dir():
        return None
    versions = [
        (key, child)
        for child in folder.iterdir()
        if child.is_dir() and (key := version_key(child.name)) is not None
    ]
    return max(versions)[1] if versions else None


def ndk(env: Mapping[str, str] | None = None) -> Path | None:
    """The NDK a build uses, or None when this machine has none."""
    lookup = _lookup(env)
    for var in ("ANDROID_NDK_HOME", "ANDROID_NDK_ROOT"):
        value = lookup.get(var)
        if value and Path(value).is_dir():
            return Path(value)
    root = sdk(env)
    return newest_version(root / "ndk") if root is not None else None


def ndk_tool(ndk_root: Path | None, name: str) -> str | None:
    """One of the NDK's LLVM tools (`llvm-strip`, `llvm-readelf`, ...), when present."""
    if ndk_root is None:
        return None
    prebuilt = ndk_root / "toolchains" / "llvm" / "prebuilt"
    if not prebuilt.is_dir():
        return None
    for host in sorted(prebuilt.iterdir()):
        candidate = host / "bin" / name
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return str(candidate)
    return None


def main(argv: list[str]) -> int:
    if argv not in (["sdk"], ["ndk"]):
        print("usage: android_sdk.py sdk|ndk", file=sys.stderr)
        return 2
    found = sdk() if argv == ["sdk"] else ndk()
    if found is None:
        return 1
    print(found)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
