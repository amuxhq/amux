#!/usr/bin/env python3
"""Build and audit the shipping Release Android APK and JNI library.

Invoked via `just android scope-audit`. By default this script:
1. Builds the shipping Rust JNI bridge (`android-rust.py --release`, without
   `debug-tools`).
2. Builds the Release Android APK (`:app:assembleRelease --no-daemon`).
3. Reads R8's `mapping.txt` for the release build, which names every class
   that survived shrinking by its source name, and proves no debug or
   test-support class ships (`DoorServer`, `DoorHost`, `StorybookActivity`,
   `sh.amux.testsupport`, `ScriptedChat`, `ScriptedCloudService`,
   `ScriptedBillingStoreFront`). The DEX itself names classes as R8 renamed
   them, so a renamed debug class would pass a look at the DEX.
4. Extracts `lib/<abi>/libamux_app.so` from the APK and inspects it with the
   NDK's `llvm-readelf` and `llvm-nm` to prove no `debug-tools`,
   `amux_debug` or `testnet` symbol is present and SQLite is statically
   linked (no `libsqlite*.so` in `DT_NEEDED`, and `sqlite3_*` defined).
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import zipfile

sys.path.insert(0, str(Path(__file__).parent))
import android_sdk  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parents[3]
ANDROID_RUST = REPO_ROOT / "apps" / "android" / "tools" / "android-rust.py"
GRADLE_WRAPPER = REPO_ROOT / "apps" / "android" / "tools" / "gradle-run.sh"
PYTHON = REPO_ROOT / "scripts" / "py"
RELEASE_APK_DIR = REPO_ROOT / "apps" / "android" / "app" / "build" / "outputs" / "apk" / "release"
APK_CANDIDATES = ("app-release.apk", "app-release-unsigned.apk")
RELEASE_MAPPING = (
    REPO_ROOT / "apps" / "android" / "app" / "build" / "outputs" / "mapping" / "release" / "mapping.txt"
)
LIB_NAME = "libamux_app.so"

FORBIDDEN_CLASS_PATTERNS: tuple[str, ...] = (
    "DoorServer",
    "DoorHost",
    "StorybookActivity",
    "sh.amux.testsupport.",
    "ScriptedChat",
    "ScriptedCloudService",
    "ScriptedBillingStoreFront",
)

# Classes the release build must hold, so an empty or unrelated mapping fails
# rather than passes.
REQUIRED_CLASSES: tuple[str, ...] = (
    "sh.amux.android.MainActivity",
    "sh.amux.core.runtime.NativeBridge",
)

FORBIDDEN_NATIVE_SYMBOL_PATTERNS: tuple[str, ...] = (
    "debug-tools",
    "amux_debug",
    "testnet",
)

FORBIDDEN_DEBUG_SECTIONS: tuple[str, ...] = (
    ".debug_info",
    ".debug_line",
)

SQLITE_DEFINED_SYMBOLS: tuple[str, ...] = (
    "sqlite3_open",
    "sqlite3_open_v2",
    "sqlite3_prepare_v2",
)

# A class line in R8's mapping: `original.Name -> renamed.Name:`, unindented;
# its members follow indented.
MAPPING_CLASS_RE = re.compile(r"^(\S+) -> (\S+):$")
DT_NEEDED_RE = re.compile(r"\(NEEDED\)\s+Shared library:\s*\[([^\]]+)\]")
ELF_SECTION_NAME_RE = re.compile(r"^\s*\[\s*\d+\]\s+(\S+)", re.MULTILINE)
SQLITE_LIB_RE = re.compile(r"libsqlite", re.IGNORECASE)


@dataclass(frozen=True)
class ClassAuditResult:
    class_count: int
    forbidden_hits: tuple[str, ...]


@dataclass(frozen=True)
class NativeAuditResult:
    abi: str
    size_bytes: int
    needed_libs: tuple[str, ...]
    sqlite_evidence: str
    forbidden_hits: tuple[str, ...]


def mapped_classes(mapping_text: str) -> set[str]:
    """The source names of the classes R8 kept, from its mapping file."""
    classes: set[str] = set()
    for line in mapping_text.splitlines():
        if line.startswith(("#", " ", "\t")):
            continue
        match = MAPPING_CLASS_RE.match(line)
        if match:
            classes.add(match.group(1))
    return classes


def find_forbidden_classes(classes: set[str]) -> list[str]:
    hits: list[str] = []
    for name in sorted(classes):
        for pattern in FORBIDDEN_CLASS_PATTERNS:
            if pattern in name:
                hits.append(f"{name} (matched {pattern!r})")
                break
    return hits


def audit_classes(mapping_path: Path) -> ClassAuditResult:
    if not mapping_path.is_file():
        raise RuntimeError(
            f"R8 mapping not found at {mapping_path}; the release build must be minified"
        )
    classes = mapped_classes(mapping_path.read_text(encoding="utf-8", errors="replace"))
    missing = [name for name in REQUIRED_CLASSES if name not in classes]
    if missing:
        raise RuntimeError(
            f"R8 mapping {mapping_path} is missing expected app classes: {', '.join(missing)}"
        )
    return ClassAuditResult(
        class_count=len(classes),
        forbidden_hits=tuple(find_forbidden_classes(classes)),
    )


def ndk_tool(name: str) -> str:
    found = android_sdk.ndk_tool(android_sdk.ndk(), name)
    if not found:
        raise RuntimeError(f"{name} was not found in the Android NDK's prebuilt toolchain")
    return found


def parse_dt_needed(readelf_output: str) -> list[str]:
    return DT_NEEDED_RE.findall(readelf_output)


def parse_section_headers(readelf_output: str) -> list[str]:
    return ELF_SECTION_NAME_RE.findall(readelf_output)


def parse_nm_symbols(nm_output: str) -> list[tuple[str, str]]:
    """Return (symbol_type, symbol_name) pairs from llvm-nm/nm output."""
    parsed: list[tuple[str, str]] = []
    for line in nm_output.splitlines():
        parts = line.split()
        if len(parts) == 2:
            parsed.append((parts[0], parts[1]))
        elif len(parts) >= 3:
            parsed.append((parts[1], parts[2]))
    return parsed


def inspect_native_library(
    so_path: Path,
    *,
    abi: str,
    readelf: str | None = None,
    nm: str | None = None,
) -> NativeAuditResult:
    if not so_path.is_file():
        raise RuntimeError(f"missing native library: {so_path}")
    size = so_path.stat().st_size
    if size == 0:
        raise RuntimeError(f"native library is empty: {so_path}")

    readelf_bin = readelf or ndk_tool("llvm-readelf")
    nm_bin = nm or ndk_tool("llvm-nm")

    dynamic_out = subprocess.run(
        [readelf_bin, "-d", str(so_path)],
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=120,
    ).stdout
    needed = parse_dt_needed(dynamic_out)
    if not needed:
        raise RuntimeError(
            f"{so_path} reported no DT_NEEDED libraries; readelf output may be invalid"
        )

    dynamic_sqlite = [lib for lib in needed if SQLITE_LIB_RE.search(lib)]
    if dynamic_sqlite:
        raise RuntimeError(
            f"{so_path} dynamically links SQLite ({', '.join(dynamic_sqlite)}); "
            "SQLite must be statically linked"
        )

    sections_out = subprocess.run(
        [readelf_bin, "--section-headers", str(so_path)],
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=120,
    ).stdout
    sections = parse_section_headers(sections_out)
    if not sections:
        raise RuntimeError(
            f"{so_path} reported no ELF section headers; readelf output may be invalid"
        )
    dwarf_sections = [sec for sec in FORBIDDEN_DEBUG_SECTIONS if sec in sections]

    # Read full symbol table when present (optional for stripped libraries),
    # and require the dynamic symbol table (always present in ELF cdylibs).
    nm_lines: list[str] = []
    full_proc = subprocess.run(
        [nm_bin, "--demangle", str(so_path)],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=120,
    )
    if full_proc.returncode == 0 and full_proc.stdout:
        nm_lines.append(full_proc.stdout)

    dyn_proc = subprocess.run(
        [nm_bin, "--dynamic", "--demangle", str(so_path)],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=120,
    )
    if dyn_proc.returncode != 0:
        detail = (dyn_proc.stderr or dyn_proc.stdout).strip()
        raise RuntimeError(
            f"{nm_bin} --dynamic --demangle failed on {so_path} "
            f"(exit {dyn_proc.returncode}): {detail or 'no error output'}"
        )
    if not dyn_proc.stdout.strip():
        raise RuntimeError(
            f"{nm_bin} --dynamic --demangle produced no symbols for {so_path}"
        )
    nm_lines.append(dyn_proc.stdout)

    symbols = parse_nm_symbols("\n".join(nm_lines))
    forbidden_hits: list[str] = [
        f"unstripped DWARF section {sec} present" for sec in dwarf_sections
    ]

    for sym_type, sym_name in symbols:
        lower = sym_name.lower()
        for pattern in FORBIDDEN_NATIVE_SYMBOL_PATTERNS:
            if pattern in lower:
                forbidden_hits.append(f"symbol {sym_type} {sym_name} (matched {pattern!r})")
                break

    # Also verify the binary does not embed the `+debug-tools` build marker from app-bridge::version().
    raw_bytes = so_path.read_bytes()
    if b"+debug-tools" in raw_bytes:
        forbidden_hits.append("rodata contains '+debug-tools' version marker")

    # SQLite is statically linked when no sqlite3_* symbol is left for the
    # loader to find and the engine's own entry points are defined here. The
    # release library is stripped of debug sections only, so its symbol table
    # still names them.
    undefined_sqlite = [
        name for sym_type, name in symbols
        if sym_type.upper() == "U" and name.startswith("sqlite3_")
    ]
    if undefined_sqlite:
        raise RuntimeError(
            f"{so_path} has undefined dynamic SQLite symbol(s): "
            f"{', '.join(sorted(set(undefined_sqlite))[:10])}"
        )

    defined_sqlite = sorted({
        name for sym_type, name in symbols
        if sym_type.upper() != "U" and name in SQLITE_DEFINED_SYMBOLS
    })
    if not defined_sqlite:
        raise RuntimeError(
            f"{so_path} defines none of {', '.join(SQLITE_DEFINED_SYMBOLS)}"
        )
    sqlite_evidence = f"defined static symbols: {', '.join(defined_sqlite)}"

    # Deduplicate forbidden hits while preserving order.
    deduped_hits = tuple(dict.fromkeys(forbidden_hits))
    return NativeAuditResult(
        abi=abi,
        size_bytes=size,
        needed_libs=tuple(needed),
        sqlite_evidence=sqlite_evidence,
        forbidden_hits=deduped_hits,
    )


def audit_native_libraries(
    apk_path: Path,
    *,
    abis: list[str],
    readelf: str | None = None,
    nm: str | None = None,
) -> list[NativeAuditResult]:
    if not apk_path.is_file():
        raise FileNotFoundError(f"Release APK not found at {apk_path}")

    native_results: list[NativeAuditResult] = []
    with zipfile.ZipFile(apk_path, "r") as zf:
        with tempfile.TemporaryDirectory(prefix="amux-so-audit-") as tmp:
            tmp_dir = Path(tmp)
            for abi in abis:
                member = f"lib/{abi}/{LIB_NAME}"
                if member not in zf.namelist():
                    raise RuntimeError(
                        f"Release APK {apk_path} is missing required native library {member}"
                    )
                out_path = tmp_dir / abi / LIB_NAME
                out_path.parent.mkdir(parents=True, exist_ok=True)
                out_path.write_bytes(zf.read(member))
                native_results.append(
                    inspect_native_library(
                        out_path,
                        abi=abi,
                        readelf=readelf,
                        nm=nm,
                    )
                )

    return native_results


def locate_release_apk(apk_dir: Path = RELEASE_APK_DIR) -> Path:
    found = [apk_dir / name for name in APK_CANDIDATES if (apk_dir / name).is_file()]
    if not found:
        expected = ", ".join(str(apk_dir / name) for name in APK_CANDIDATES)
        raise FileNotFoundError(f"no release APK found (checked {expected})")
    return max(found, key=lambda p: p.stat().st_mtime)


def build_release_artifacts(abis: list[str], *, no_daemon: bool = True) -> Path:
    rust_cmd = [str(PYTHON), "-B", str(ANDROID_RUST), "--release"]
    for abi in abis:
        rust_cmd.extend(["--abi", abi])
    print(f"scope-audit: building release JNI bridge ({', '.join(abis)})", flush=True)
    subprocess.run(rust_cmd, cwd=REPO_ROOT, check=True, timeout=1800)

    gradle_cmd = ["sh", str(GRADLE_WRAPPER), ":app:assembleRelease"]
    if no_daemon:
        gradle_cmd.append("--no-daemon")
    print("scope-audit: assembling Release Android APK", flush=True)
    subprocess.run(gradle_cmd, cwd=REPO_ROOT, check=True, timeout=1800)

    return locate_release_apk()


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--apk",
        type=Path,
        default=None,
        help="Path to an existing Release APK to audit (implies --skip-build).",
    )
    parser.add_argument(
        "--mapping",
        type=Path,
        default=RELEASE_MAPPING,
        help="R8's mapping.txt for that APK (defaults to the release build's).",
    )
    parser.add_argument(
        "--skip-build",
        action="store_true",
        help="Audit the already-built Release APK without running Cargo or Gradle.",
    )
    parser.add_argument(
        "--abi",
        action="append",
        dest="abis",
        help="Android ABI to require and audit inside the APK (defaults to arm64-v8a).",
    )
    parser.add_argument(
        "--allow-daemon",
        action="store_true",
        help="Do not pass --no-daemon to Gradle when building :app:assembleRelease.",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    abis = args.abis if args.abis else ["arm64-v8a"]

    if args.apk is not None:
        apk_path = args.apk.resolve()
    elif args.skip_build:
        apk_path = locate_release_apk()
    else:
        apk_path = build_release_artifacts(abis, no_daemon=not args.allow_daemon)

    class_result = audit_classes(args.mapping.resolve())
    native_results = audit_native_libraries(apk_path, abis=abis)

    print(
        f"scope-audit: APK {apk_path.name} ({apk_path.stat().st_size:,} bytes): "
        f"{class_result.class_count} classes kept by R8",
        flush=True,
    )
    for nr in native_results:
        print(
            f"scope-audit: lib/{nr.abi}/{LIB_NAME} ({nr.size_bytes:,} bytes): "
            f"DT_NEEDED=[{', '.join(nr.needed_libs)}], SQLite={nr.sqlite_evidence}",
            flush=True,
        )

    failures: list[str] = []
    for hit in class_result.forbidden_hits:
        failures.append(f"forbidden class: {hit}")
    for nr in native_results:
        for hit in nr.forbidden_hits:
            failures.append(f"lib/{nr.abi}/{LIB_NAME} forbidden native marker: {hit}")

    if failures:
        print("scope-audit: FAILED:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1

    print("scope-audit: PASS (no debug/test classes, symbols or DWARF sections; SQLite statically linked)", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
