#!/usr/bin/env python3
"""Check that an APK holds what installing and starting it needs.

Usage: apk-contents.py APK

The debug APK (`//apps/android/app:debug`) is what the journeys install; it
is built by nothing else in the gate, so this check is what builds it there.
It must hold its manifest, its resources, dex code with the Kotlin standard
library in it, the Rust bridge for the emulator's and phones' ABI, and a
signature.
"""

from __future__ import annotations

from pathlib import Path
import sys
import zipfile

REQUIRED = ("AndroidManifest.xml", "resources.arsc", "classes.dex", "lib/arm64-v8a/libamux_app.so")


def problems(apk: Path) -> list[str]:
    with zipfile.ZipFile(apk) as zf:
        names = set(zf.namelist())
        found = [f"missing {name}" for name in REQUIRED if name not in names]
        dexes = sorted(name for name in names if name.startswith("classes") and name.endswith(".dex"))
        if not any(b"Lkotlin/Unit;" in zf.read(name) and b"Unit.kt" in zf.read(name) for name in dexes):
            found.append("no dex file defines the Kotlin standard library (kotlin.Unit)")
        if not any(name.startswith("META-INF/") and name.endswith((".RSA", ".EC", ".DSA")) for name in names):
            found.append("no signature")
    return found


def main(argv: list[str]) -> int:
    if len(argv) != 1:
        print(__doc__.split("\n\n")[1], file=sys.stderr)
        return 2
    apk = Path(argv[0])
    found = problems(apk)
    for problem in found:
        print(f"apk-contents: {apk.name}: {problem}", file=sys.stderr)
    if found:
        return 1
    print(f"apk-contents: {apk.name} ({apk.stat().st_size:,} bytes) holds its manifest, resources, code and bridge")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
