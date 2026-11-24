#!/usr/bin/env python3
"""Build the shipping amux binary for this machine, as a release ships it.

    release-build.py [--target TRIPLE]

Buck2 builds `//crates/amux:amux` in the release configuration with every
action on this machine (`--local-only`) and nothing read from or written to
a cache (`--no-remote-cache`), with this machine's own toolchains: rustup's
pinned Rust and the system's C compiler (build/constraints:local-build), not
the build host's Nix ones, whose C library a shipped Linux binary must not
need. So what ships is built from scratch where it is built (docs/RELEASE.md).

The binary and the symbols a release keeps beside it go where Cargo's
release build left them, `target/<triple>/release/` (`target/release/`
without --target), which scripts/release-symbols.sh and
scripts/release-policy-check.sh read: `amux` (`amux.exe` and `amux.pdb` on
Windows, and `amux.dSYM` on macOS). The release keys come from
AMUX_RELEASE_PUBLIC_KEYS, as for Cargo; with none, the binary trusts only the
test key. TRIPLE must be this machine's: a release is built natively on each
platform.
"""

import argparse
import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BUCK2 = ROOT / "scripts/buck2"

HOSTS = {
    ("Darwin", "arm64"): ("aarch64-apple-darwin", "macos"),
    ("Linux", "x86_64"): ("x86_64-unknown-linux-gnu", "linux"),
    ("Windows", "AMD64"): ("x86_64-pc-windows-msvc", "windows"),
}


def buck2(*args: str) -> list[str]:
    # scripts/buck2 is a shell script, which Windows can't start itself.
    command = [str(BUCK2), *args]
    return (["sh"] if os.name == "nt" else []) + command


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--target")
    args, _ = parser.parse_known_args()
    host = HOSTS.get((platform.system(), platform.machine()))
    if host is None:
        sys.exit(f"release-build: no release is built on {platform.system()} {platform.machine()}")
    triple, os_name = host
    if args.target not in (None, triple):
        sys.exit(f"release-build: a release is built natively; this machine builds {triple}, not {args.target}")
    out = ROOT / "target" / (args.target or "") / "release"
    out.mkdir(parents=True, exist_ok=True)
    exe = ".exe" if os_name == "windows" else ""
    env = dict(os.environ, AMUX_LOCAL_BUILD="1")
    common = [
        "--target-platforms", f"root//build/platforms:{os_name}-cli-shipping-release",
        "--local-only", "--no-remote-cache",
        "-c", f"amux.release_public_keys={os.environ.get('AMUX_RELEASE_PUBLIC_KEYS', '')}",
    ]
    outputs = {"//crates/amux:amux": out / f"amux{exe}"}
    if os_name == "windows":
        outputs["//crates/amux:amux[pdb]"] = out / "amux.pdb"
    for target, path in outputs.items():
        if path.is_dir():
            shutil.rmtree(path)
        else:
            path.unlink(missing_ok=True)
        subprocess.run(buck2("build", target, *common, "--out", str(path)), check=True, cwd=ROOT, env=env)
    if os_name == "macos":
        dsym(common, env, out / "amux")
    print(f"release-build: {out}")
    return 0


def dsym(common: list[str], env: dict[str, str], binary: Path) -> None:
    """The macOS symbols: rustc's own dSYM, written beside the binary in
    Buck2's output directory when it linked (-Csplit-debuginfo=packed,
    build/toolchains/BUCK), as Cargo's was in target/. Nothing else can make
    it: with fat LTO all of the program's code is one object, which rustc
    deletes once it has run dsymutil over it. Buck2 doesn't know the bundle
    is an output, so it is taken only if it is this binary's."""
    built = subprocess.run(
        buck2("build", "//crates/amux:amux", *common, "--show-full-simple-output"),
        check=True, cwd=ROOT, env=env, capture_output=True, text=True,
    ).stdout.strip().splitlines()[-1]
    bundle = Path(built + ".dSYM")
    if uuid(bundle) != uuid(binary):
        sys.exit(f"release-build: {bundle} is not the symbols of {binary}")
    target = binary.with_name(binary.name + ".dSYM")
    shutil.rmtree(target, ignore_errors=True)
    shutil.copytree(bundle, target, symlinks=True)


def uuid(path: Path) -> str:
    """The Mach-O UUID that matches a binary to its symbols."""
    out = subprocess.run(["dwarfdump", "--uuid", str(path)], check=True, capture_output=True, text=True).stdout
    return " ".join(line.split()[1] for line in out.splitlines() if line.startswith("UUID:"))


if __name__ == "__main__":
    sys.exit(main())
