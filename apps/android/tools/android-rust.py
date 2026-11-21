#!/usr/bin/env python3
"""Build the Rust JNI bridge libraries for Android and the host JVM.

By default, builds `app-jni` for `arm64-v8a` (Android API 34) with the driving
tools feature (`debug-tools`) under the `phone-dev` profile via `cargo-ndk`,
staging `libamux_app.so` into `target/android/debug-jniLibs/`.

Flags:
- `--host`: build `app-jni` (with `debug-tools` under `phone-dev`) for the host
  OS into `target/android/host-jni/` so JVM unit/integration tests can load
  `libamux_app.dylib` / `libamux_app.so`.
- `--release`: build shipping Android libraries WITHOUT `debug-tools` under the
  `release` profile into `target/android/release-jniLibs/`.
- `--abi <abi>`: Android ABI to build (repeatable; defaults to `arm64-v8a`).
- `--fingerprint`: print the input fingerprint for the requested mode and exit.

When no Rust input has changed since the last run and every expected output
library is present, Cargo is not invoked and output timestamps are left
untouched so Gradle remains up-to-date. Stale ABI directories outside the
requested `--abi` set are always pruned, including on cache hits.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import os
from pathlib import Path
import platform
import shutil
import subprocess
from types import ModuleType

REPO_ROOT = Path(__file__).resolve().parents[3]
CRATE = "app-jni"
LIB_STEM = "amux_app"
ANDROID_PLATFORM = "34"
DRIVING_PROFILE = "phone-dev"
SHIPPING_PROFILE = "release"
DRIVING_FEATURES = ("debug-tools",)

ABI_TO_TRIPLE = {
    "arm64-v8a": "aarch64-linux-android",
    "x86_64": "x86_64-linux-android",
}

OUTPUT = Path("target/android")
RUST_TARGETS = OUTPUT / "rust-cargo"
HOST_JNI_DIR = OUTPUT / "host-jni"
RELEASE_JNI_LIBS = OUTPUT / "release-jniLibs"
DEV_JNI_LIBS = OUTPUT / "debug-jniLibs"

ROOT_SOURCE_PATHS = ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo")
SOURCE_ENVIRONMENT = (
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
    "RUSTC",
    "RUSTUP_TOOLCHAIN",
    "ANDROID_NDK_HOME",
    "ANDROID_NDK_ROOT",
)

_CI_AFFECTED: ModuleType | None = None


def _ci_affected() -> ModuleType:
    global _CI_AFFECTED
    if _CI_AFFECTED is None:
        script = REPO_ROOT / "scripts" / "ci-affected.py"
        spec = importlib.util.spec_from_file_location("amux_ci_affected", script)
        assert spec is not None and spec.loader is not None
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        _CI_AFFECTED = module
    return _CI_AFFECTED


def crate_closure(crate: str = CRATE, root: Path = Path(".")) -> list[str]:
    """Sorted folder names under `crates/` in `crate`'s transitive build graph.

    Follows normal `dependencies` and `build-dependencies` (not
    `dev-dependencies`), matching what `cargo build -p <crate> --lib` compiles.
    """
    crates = _ci_affected().workspace(root)
    if crate not in crates:
        raise RuntimeError(f"workspace crate {crate!r} was not found under {root / 'crates'}")
    seen = {crate}
    queue = list(crates[crate]["built"])
    while queue:
        dep = queue.pop()
        if dep in crates and dep not in seen:
            seen.add(dep)
            queue.extend(crates[dep]["built"])
    return sorted(crates[name]["folder"] for name in seen)


def host_library_name() -> str:
    system = platform.system()
    if system == "Darwin":
        return f"lib{LIB_STEM}.dylib"
    if system == "Windows":
        return f"{LIB_STEM}.dll"
    return f"lib{LIB_STEM}.so"


def find_cargo_ndk() -> str:
    found = shutil.which("cargo-ndk")
    if found:
        return found
    candidate = Path.home() / ".cargo" / "bin" / "cargo-ndk"
    if candidate.is_file():
        return str(candidate)
    raise RuntimeError(
        "cargo-ndk was not found on PATH or in ~/.cargo/bin; "
        "install it with `cargo install --locked cargo-ndk`"
    )


def find_android_ndk() -> str | None:
    for var in ("ANDROID_NDK_HOME", "ANDROID_NDK_ROOT"):
        value = os.environ.get(var)
        if value and Path(value).is_dir():
            return value
    sdk_candidates = [
        os.environ.get("ANDROID_HOME"),
        os.environ.get("ANDROID_SDK_ROOT"),
        str(Path.home() / "Library" / "Android" / "sdk"),
        str(Path.home() / "Android" / "Sdk"),
    ]
    for sdk in sdk_candidates:
        if not sdk:
            continue
        ndk_root = Path(sdk) / "ndk"
        if ndk_root.is_dir():
            versions = sorted((p for p in ndk_root.iterdir() if p.is_dir()), reverse=True)
            if versions:
                return str(versions[0])
    return None


def find_llvm_strip(ndk_path: str | None) -> str:
    if ndk_path:
        prebuilt = Path(ndk_path) / "toolchains" / "llvm" / "prebuilt"
        if prebuilt.is_dir():
            for host_dir in sorted(prebuilt.iterdir()):
                candidate = host_dir / "bin" / "llvm-strip"
                if candidate.is_file() and os.access(candidate, os.X_OK):
                    return str(candidate)
    found = shutil.which("llvm-strip")
    if found:
        return found
    raise RuntimeError(
        "llvm-strip was not found in the Android NDK prebuilt toolchain or on PATH"
    )


def strip_debug_sections(so_path: Path, strip_bin: str) -> None:
    subprocess.run(
        [strip_bin, "--strip-debug", str(so_path)],
        check=True,
        timeout=300,
    )


def ndk_identity(ndk_path: str | None) -> str:
    if not ndk_path:
        return "none"
    resolved = Path(ndk_path).resolve()
    props = resolved / "source.properties"
    revision = ""
    if props.is_file():
        for line in props.read_text(encoding="utf-8", errors="replace").splitlines():
            key, sep, value = line.partition("=")
            if sep and key.strip() == "Pkg.Revision":
                revision = value.strip()
                break
    return f"{resolved}:{revision or resolved.name}"


def source_fingerprint(root: Path = Path(".")) -> str:
    summary = hashlib.sha256()
    for name in SOURCE_ENVIRONMENT:
        summary.update(f"{name}={os.environ.get(name, '')}\n".encode())
    roots = [
        *(Path("crates") / folder for folder in crate_closure(CRATE, root)),
        *(Path(top) for top in ROOT_SOURCE_PATHS),
    ]
    for rel_top in roots:
        path = root / rel_top
        if not path.exists():
            continue
        entries = sorted(path.rglob("*")) if path.is_dir() else [path]
        for entry in entries:
            if not entry.is_file():
                continue
            rel = entry.relative_to(root)
            if "target" in rel.parts:
                continue
            summary.update(f"{rel.as_posix()}\0".encode())
            summary.update(hashlib.sha256(entry.read_bytes()).digest())
    return summary.hexdigest()


def build_fingerprint(
    mode: str,
    abis: list[str],
    *,
    ndk: str | None = None,
    root: Path = Path("."),
) -> str:
    if mode == "host":
        spec = (
            f"host:{platform.system()}:{platform.machine()}:"
            f"{DRIVING_PROFILE}:{','.join(DRIVING_FEATURES)}"
        )
    else:
        resolved_ndk = ndk if ndk is not None else find_android_ndk()
        ndk_id = ndk_identity(resolved_ndk)
        if mode == "release":
            spec = (
                f"release:{','.join(sorted(abis))}:api{ANDROID_PLATFORM}:"
                f"{SHIPPING_PROFILE}:strip-debug:ndk={ndk_id}"
            )
        else:
            spec = (
                f"dev:{','.join(sorted(abis))}:api{ANDROID_PLATFORM}:"
                f"{DRIVING_PROFILE}:{','.join(DRIVING_FEATURES)}:ndk={ndk_id}"
            )
    return f"{source_fingerprint(root)}:{spec}"


def copy_if_changed(src: Path, dst: Path) -> bool:
    dst.parent.mkdir(parents=True, exist_ok=True)
    src_bytes = src.read_bytes()
    if dst.is_file() and dst.read_bytes() == src_bytes:
        return False
    dst.write_bytes(src_bytes)
    return True


def prune_stale_abis(jni_root: Path, abis: list[str]) -> list[str]:
    if not jni_root.is_dir():
        return []
    keep = set(abis)
    removed: list[str] = []
    for entry in sorted(jni_root.iterdir()):
        if entry.name in keep:
            continue
        if entry.is_dir() and not entry.is_symlink():
            shutil.rmtree(entry)
        else:
            entry.unlink()
        removed.append(entry.name)
    return removed


def build_host(fingerprint: str) -> None:
    HOST_JNI_DIR.mkdir(parents=True, exist_ok=True)
    lib_name = host_library_name()
    dst = HOST_JNI_DIR / lib_name
    stamp = OUTPUT / "host-jni-stamp.txt"
    if stamp.is_file() and stamp.read_text().strip() == fingerprint and dst.is_file():
        print("Rust sources unchanged; host JNI bridge is current and cargo was not run", flush=True)
        return
    stamp.unlink(missing_ok=True)

    target_dir = (RUST_TARGETS / "host").resolve()
    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(target_dir)
    cmd = [
        "cargo", "build", "--locked", "-p", CRATE, "--lib",
        "--profile", DRIVING_PROFILE,
        "--features", ",".join(DRIVING_FEATURES),
    ]
    print(f"Building {CRATE} for host ({DRIVING_PROFILE}, {'+'.join(DRIVING_FEATURES)})", flush=True)
    subprocess.run(cmd, check=True, env=env, timeout=1500)

    built = target_dir / DRIVING_PROFILE / lib_name
    if not built.is_file():
        raise RuntimeError(f"expected host JNI library at {built}, but it was not produced")
    copy_if_changed(built, dst)
    print(f"host: {dst.stat().st_size} bytes ({dst})", flush=True)
    stamp.write_text(fingerprint + "\n")


def build_android(
    abis: list[str],
    *,
    release: bool,
    fingerprint: str,
    ndk: str | None = None,
) -> None:
    profile = SHIPPING_PROFILE if release else DRIVING_PROFILE
    features = () if release else DRIVING_FEATURES
    jni_root = RELEASE_JNI_LIBS if release else DEV_JNI_LIBS
    stamp = OUTPUT / ("release-jni-stamp.txt" if release else "dev-jni-stamp.txt")
    expected = [jni_root / abi / f"lib{LIB_STEM}.so" for abi in abis]

    if stamp.is_file() and stamp.read_text().strip() == fingerprint and all(p.is_file() for p in expected):
        prune_stale_abis(jni_root, abis)
        label = "shipping Android" if release else "Android"
        print(f"Rust sources unchanged; {label} JNI bridge is current and cargo was not run", flush=True)
        return
    stamp.unlink(missing_ok=True)

    cargo_ndk = find_cargo_ndk()
    resolved_ndk = ndk if ndk is not None else find_android_ndk()
    env = os.environ.copy()
    if resolved_ndk:
        env["ANDROID_NDK_HOME"] = resolved_ndk
    env["CARGO_TARGET_DIR"] = str((RUST_TARGETS / "android").resolve())
    # Ensure cargo-ndk's directory is on PATH so `cargo ndk` finds it.
    ndk_bin_dir = str(Path(cargo_ndk).parent)
    if ndk_bin_dir not in env.get("PATH", "").split(os.pathsep):
        env["PATH"] = f"{ndk_bin_dir}{os.pathsep}{env.get('PATH', '')}"

    staging = OUTPUT / ("release-staging" if release else "dev-staging")
    staging.mkdir(parents=True, exist_ok=True)

    cmd = ["cargo", "ndk"]
    for abi in abis:
        if abi not in ABI_TO_TRIPLE:
            raise RuntimeError(f"unsupported Android ABI {abi!r}; expected one of {sorted(ABI_TO_TRIPLE)}")
        cmd.extend(["-t", abi])
    cmd.extend([
        "-P", ANDROID_PLATFORM,
        "-o", str(staging),
        "build", "--locked", "-p", CRATE, "--lib",
        "--profile", profile,
    ])
    if features:
        cmd.extend(["--features", ",".join(features)])

    feat_note = f", {'+'.join(features)}" if features else ""
    print(f"Building {CRATE} for Android {','.join(abis)} (API {ANDROID_PLATFORM}, {profile}{feat_note})", flush=True)
    subprocess.run(cmd, check=True, env=env, timeout=1500)

    strip_bin = find_llvm_strip(resolved_ndk) if release else None

    for abi in abis:
        built = staging / abi / f"lib{LIB_STEM}.so"
        if not built.is_file():
            raise RuntimeError(f"expected Android JNI library at {built}, but it was not produced")
        if release:
            assert strip_bin is not None
            strip_debug_sections(built, strip_bin)
        dst = jni_root / abi / f"lib{LIB_STEM}.so"
        copy_if_changed(built, dst)
        print(f"{abi}: {dst.stat().st_size} bytes ({dst})", flush=True)
    prune_stale_abis(jni_root, abis)

    OUTPUT.mkdir(parents=True, exist_ok=True)
    stamp.write_text(fingerprint + "\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode_group = parser.add_mutually_exclusive_group()
    mode_group.add_argument(
        "--host",
        action="store_true",
        help="build the host JNI library for JVM tests",
    )
    mode_group.add_argument(
        "--release",
        action="store_true",
        help="build the shipping Android library without debug-tools",
    )
    parser.add_argument(
        "--abi",
        action="append",
        dest="abis",
        help="Android ABI to build (repeatable; defaults to arm64-v8a)",
    )
    parser.add_argument(
        "--fingerprint",
        action="store_true",
        help="print the fingerprint of the bridge's inputs and build nothing",
    )
    args = parser.parse_args()

    abis = args.abis if args.abis else ["arm64-v8a"]
    mode = "host" if args.host else ("release" if args.release else "dev")
    ndk = None if args.host else find_android_ndk()
    fp = build_fingerprint(mode, abis, ndk=ndk)

    if args.fingerprint:
        print(hashlib.sha256(fp.encode()).hexdigest())
        return 0

    if args.host:
        build_host(fp)
    else:
        build_android(abis, release=args.release, fingerprint=fp, ndk=ndk)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
