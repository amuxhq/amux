#!/usr/bin/env python3
"""Building the Rust bridge the iPhone app links, in one place.

Two recipes build it and they must agree on what it is called, where its
archives go and how an XCFramework is assembled from them, so both import this
module rather than repeating it. `ios-rust.py` builds the one slice a
development build links; `ios-package.py` builds every shipping slice.
"""

from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

# The bridge crate and the names its build produces. Renaming the crate is a
# change here and nowhere else in the recipes.
CRATE = "app-ffi"
LIBRARY = "libamux_app.a"
HEADER = "amux_app.h"
MODULE = "AmuxApp"
# The library the Debug and Measured app configurations force-load: the
# bridge with its driving tools compiled in. Its simulator slice is always
# built; its phone slice only when a build for a plugged-in phone asks for it.
DRIVING_FRAMEWORK = "AmuxAppDebugTools.xcframework"
DRIVING_SLICE = "ios-arm64-simulator"
DRIVING_DEVICE_SLICE = "ios-arm64"
# The library the app packages ship with, and that the Swift package's binary
# target names, so it has to exist before any configuration can resolve.
FRAMEWORK = "AmuxApp.xcframework"
DEBUG_TOOLS_FEATURE = "debug-tools"
# Every development-time simulator consumer uses this optimized bridge. A
# Debug Swift build still carries symbols and testability of its own; giving a
# standalone smoke a different Cargo profile only recompiles the same Rust
# source and features without covering a different binary boundary. It is the
# release build without whole-program optimisation, which would make every
# Rust edit's rebuild several times slower for a library nobody ships.
DRIVING_PROFILE = "phone-dev"
# What the app packages ship with: the profile every release builds under.
SHIPPING_PROFILE = "release"
DRIVING_FEATURES = (DEBUG_TOOLS_FEATURE,)

# The served test network a simulator connects to, as `just ios tools` builds
# it. Every recipe that starts one starts it through this, with
# TESTNET_BINARIES naming where the same build put amux and the fakes, so the
# net builds nothing of its own.
TESTNET_SERVE = ("target/debug/testnet", "serve")
TESTNET_BINARIES = Path("target/debug")

SIMULATOR_TRIPLE = "aarch64-apple-ios-sim"
DEVICE_TRIPLE = "aarch64-apple-ios"
SHIPPING_TRIPLES = (SIMULATOR_TRIPLE, DEVICE_TRIPLE)
DEPLOYMENT_TARGET = "26.0"

OUTPUT = Path("target/ios")
# One Cargo target directory per target triple. Simulator and device builds
# share no host artifacts through it, so alternating between them cannot
# invalidate each other, and the development and shipping profiles of one
# triple live side by side inside it.
RUST_TARGETS = OUTPUT / "rust-cargo"
SIZE_REPORT = OUTPUT / "size.txt"

# What the fingerprint of the Rust sources is taken over. Anything cargo would
# read to decide whether the bridge is stale.
SOURCE_ROOTS = ("crates", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo")
SOURCE_ENVIRONMENT = ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS",
                      "RUSTC", "RUSTUP_TOOLCHAIN", "DEVELOPER_DIR")


@dataclass(frozen=True)
class Slice:
    triple: str
    library: Path
    header: Path


def build_environment(triple: str) -> dict[str, str]:
    environment = os.environ.copy()
    # Static archives embed native objects whose contents can change without
    # changing Rust metadata. Cargo tracks those inputs; wrapper caches may not.
    environment["RUSTC_WRAPPER"] = ""
    environment["CARGO_TARGET_DIR"] = str((RUST_TARGETS / triple).resolve())
    environment["IPHONEOS_DEPLOYMENT_TARGET"] = DEPLOYMENT_TARGET
    # SDKROOT is deliberately absent: cargo fingerprints host-side build
    # scripts with it, so exporting the simulator and device SDKs in turn
    # rebuilds the shared host graph every time. The C compiler selects the SDK
    # from the target triple on its own.
    environment.pop("SDKROOT", None)
    return environment


def cargo_build(triple: str, *, profile: str, features: tuple[str, ...], log: Path) -> Slice:
    """Build the bridge for one triple and answer where cargo put its outputs.

    Cargo reports the artifact and build-script directories even on a cached
    build, so the header is never selected by globbing a possibly stale tree.
    """
    command = [
        "cargo", "build", "--locked", "-p", CRATE, "--lib",
        "--profile", profile, "--target", triple, "--message-format=json-render-diagnostics",
    ]
    if features:
        command.extend(["--features", ",".join(features)])
    print(f"Building {CRATE} for {triple} ({profile} profile"
          + (f", {'+'.join(features)}" if features else "") + ")", flush=True)
    log.parent.mkdir(parents=True, exist_ok=True)
    with log.open("w") as messages:
        subprocess.run(command, stdout=messages, check=True, timeout=1500,
                       env=build_environment(triple))
    messages = [json.loads(line) for line in log.read_text().splitlines()]
    crate_name = CRATE.replace("-", "_")
    artifact, = [
        message for message in messages
        if message.get("reason") == "compiler-artifact"
        and message["target"]["name"] == crate_name
        and "staticlib" in message["target"]["crate_types"]
    ]
    library, = [Path(name) for name in artifact["filenames"] if name.endswith(".a")]
    build_script, = [
        message for message in messages
        if message.get("reason") == "build-script-executed"
        and message["package_id"] == artifact["package_id"]
    ]
    return Slice(triple, library, Path(build_script["out_dir"]) / HEADER)


def stage(built: Slice, destination: Path) -> Path:
    """Copy one built slice into the layout `xcodebuild -create-xcframework` reads."""
    includes = destination / "include"
    includes.mkdir(parents=True, exist_ok=True)
    shutil.copy2(built.header, includes / HEADER)
    (includes / "module.modulemap").write_text(
        f'module {MODULE} {{\n  header "{HEADER}"\n  export *\n}}\n'
    )
    staged = destination / LIBRARY
    shutil.copy2(built.library, staged)
    return staged


def package(framework: Path, slices: list[Path]) -> None:
    if framework.exists():
        shutil.rmtree(framework)
    command = ["xcodebuild", "-create-xcframework"]
    for directory in slices:
        command.extend([
            "-library", str((directory / LIBRARY).resolve()),
            "-headers", str((directory / "include").resolve()),
        ])
    command.extend(["-output", str(framework.resolve())])
    subprocess.run(command, check=True, timeout=180)


def stand_in_marker(framework: Path) -> Path:
    """Marker owned by the development recipe for a shipping-path stand-in."""
    return framework.with_name(f"{framework.name}.stand-in")


def digest(paths: list[Path]) -> str:
    """One hash over the archives and headers a framework is assembled from."""
    summary = hashlib.sha256()
    for path in paths:
        summary.update(path.name.encode())
        summary.update(path.read_bytes())
    return summary.hexdigest()


def package_if_changed(framework: Path, slices: list[Path], stamp: Path) -> bool:
    """Assemble the framework only when its inputs differ from the last one.

    Xcode treats a rewritten framework directory as a changed input and
    rebuilds every Swift target that imports it, so a bridge that cargo left
    untouched must leave the framework untouched too.
    """
    current = digest([path for directory in slices
                      for path in (directory / LIBRARY, directory / "include" / HEADER)])
    if framework.is_dir() and stamp.is_file() and stamp.read_text().strip() == current:
        print(f"{framework.name} is current", flush=True)
        return False
    package(framework, slices)
    stamp.write_text(current + "\n")
    return True


def source_fingerprint(root: Path = Path(".")) -> str:
    """A cheap answer to "could cargo have anything to do?".

    Walks every path cargo reads and records name, size and modification time,
    so a build recipe can skip invoking cargo at all when no Rust input moved.
    Cargo remains the authority whenever anything did.
    """
    summary = hashlib.sha256()
    for name in SOURCE_ENVIRONMENT:
        summary.update(f"{name}={os.environ.get(name, '')}\n".encode())
    for top in SOURCE_ROOTS:
        path = root / top
        entries = sorted(path.rglob("*")) if path.is_dir() else [path]
        for entry in entries:
            if not entry.is_file() or "target" in entry.relative_to(root).parts[:2]:
                continue
            info = entry.stat()
            summary.update(f"{entry}\0{info.st_size}\0{info.st_mtime_ns}\n".encode())
    return summary.hexdigest()


def write_size_report(lines: list[str], profile: dict) -> str:
    text = "\n".join([
        f"{CRATE} static archives (not the linked application size)",
        f"iOS deployment target: {DEPLOYMENT_TARGET}",
        "profile: " + ", ".join(f"{key}={value}" for key, value in profile.items()),
        *lines,
        "",
    ])
    SIZE_REPORT.write_text(text)
    return text


def size_line(triple: str, staged: Path, note: str = "") -> str:
    return f"{triple}: {staged.stat().st_size} bytes ({staged}){note}"
