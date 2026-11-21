#!/usr/bin/env python3
"""Build and audit the shipping Release Android APK and JNI library.

Invoked via `just android scope-audit`. By default this script:
1. Builds the shipping Rust JNI bridge (`android-rust.py --release`, without
   `debug-tools`).
2. Builds the Release Android APK (`:app:assembleRelease --no-daemon`).
3. Inspects the APK's DEX files (via Android SDK `dexdump` when available, with
   a pure-Python DEX header/string-table parser fallback) and proves no debug
   or test-support classes ship (`DoorServer`, `DoorHost`, `StorybookActivity`,
   `sh/amux/testsupport/`, `ScriptedChat`, `ScriptedCloudService`,
   `ScriptedBillingStoreFront`).
4. Extracts `lib/<abi>/libamux_app.so` from the APK and inspects it with NDK
   `llvm-readelf` / `llvm-nm` (falling back to `readelf` / `nm` / `strings`) to
   prove zero `debug-tools` / `amux_debug` / `testnet` symbols are present and
   SQLite is statically linked (no `libsqlite*.so` in `DT_NEEDED`, with static
   SQLite symbols/linkage present).
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import zipfile

REPO_ROOT = Path(__file__).resolve().parents[3]
ANDROID_RUST = REPO_ROOT / "apps" / "android" / "tools" / "android-rust.py"
GRADLE_WRAPPER = REPO_ROOT / "apps" / "android" / "tools" / "gradle-run.sh"
PYTHON = REPO_ROOT / "scripts" / "py"
RELEASE_APK_DIR = REPO_ROOT / "apps" / "android" / "app" / "build" / "outputs" / "apk" / "release"
APK_CANDIDATES = ("app-release.apk", "app-release-unsigned.apk")
LIB_NAME = "libamux_app.so"

FORBIDDEN_DEX_PATTERNS: tuple[str, ...] = (
    "DoorServer",
    "DoorHost",
    "StorybookActivity",
    "sh/amux/testsupport/",
    "ScriptedChat",
    "ScriptedCloudService",
    "ScriptedBillingStoreFront",
)

REQUIRED_DEX_DESCRIPTORS: tuple[str, ...] = (
    "Lsh/amux/android/MainActivity;",
    "Lsh/amux/core/runtime/NativeBridge;",
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

DEX_CLASS_DESCRIPTOR_RE = re.compile(r"Class descriptor\s*:\s*'([^']+)'")
DT_NEEDED_RE = re.compile(r"\(NEEDED\)\s+Shared library:\s*\[([^\]]+)\]")
ELF_SECTION_NAME_RE = re.compile(r"^\s*\[\s*\d+\]\s+(\S+)", re.MULTILINE)
SQLITE_LIB_RE = re.compile(r"libsqlite", re.IGNORECASE)


@dataclass(frozen=True)
class DexAuditResult:
    dex_files: tuple[str, ...]
    descriptor_count: int
    tool: str
    forbidden_hits: tuple[str, ...]


@dataclass(frozen=True)
class NativeAuditResult:
    abi: str
    size_bytes: int
    needed_libs: tuple[str, ...]
    sqlite_evidence: str
    forbidden_hits: tuple[str, ...]


def find_android_sdk() -> Path | None:
    candidates = [
        os.environ.get("ANDROID_HOME"),
        os.environ.get("ANDROID_SDK_ROOT"),
        str(Path.home() / "Library" / "Android" / "sdk"),
        str(Path.home() / "Android" / "Sdk"),
    ]
    for candidate in candidates:
        if candidate and Path(candidate).is_dir():
            return Path(candidate)
    return None


def find_android_ndk() -> Path | None:
    for var in ("ANDROID_NDK_HOME", "ANDROID_NDK_ROOT"):
        value = os.environ.get(var)
        if value and Path(value).is_dir():
            return Path(value)
    sdk = find_android_sdk()
    if sdk is None:
        return None
    ndk_root = sdk / "ndk"
    if ndk_root.is_dir():
        versions = sorted((p for p in ndk_root.iterdir() if p.is_dir()), reverse=True)
        if versions:
            return versions[0]
    return None


def find_dexdump() -> str | None:
    sdk = find_android_sdk()
    if sdk is not None:
        build_tools = sdk / "build-tools"
        if build_tools.is_dir():
            versions = sorted((p for p in build_tools.iterdir() if p.is_dir()), reverse=True)
            for version_dir in versions:
                candidate = version_dir / "dexdump"
                if candidate.is_file() and os.access(candidate, os.X_OK):
                    return str(candidate)
    return shutil.which("dexdump")


def find_ndk_tool(name: str, fallbacks: tuple[str, ...] = ()) -> str | None:
    ndk = find_android_ndk()
    if ndk is not None:
        prebuilt = ndk / "toolchains" / "llvm" / "prebuilt"
        if prebuilt.is_dir():
            for host_dir in sorted(prebuilt.iterdir()):
                candidate = host_dir / "bin" / name
                if candidate.is_file() and os.access(candidate, os.X_OK):
                    return str(candidate)
    for candidate_name in (name, *fallbacks):
        found = shutil.which(candidate_name)
        if found:
            return found
    return None


def _read_uleb128(data: bytes, offset: int) -> tuple[int, int]:
    result = 0
    shift = 0
    while offset < len(data):
        byte = data[offset]
        offset += 1
        result |= (byte & 0x7F) << shift
        if (byte & 0x80) == 0:
            return result, offset
        shift += 7
        if shift > 35:
            raise ValueError("invalid ULEB128 sequence in DEX string table")
    raise ValueError("unexpected EOF reading ULEB128 in DEX string table")


def parse_dex_descriptors(data: bytes) -> set[str]:
    """Pure-Python DEX parser extracting all defined and referenced type descriptors."""
    if len(data) < 112:
        raise ValueError(f"DEX file too short ({len(data)} bytes)")
    if data[:4] != b"dex\n" or data[7:8] != b"\x00":
        raise ValueError(f"invalid DEX magic: {data[:8]!r}")

    string_ids_size, string_ids_off = struct.unpack_from("<II", data, 56)
    type_ids_size, type_ids_off = struct.unpack_from("<II", data, 64)
    class_defs_size, class_defs_off = struct.unpack_from("<II", data, 96)

    def read_string(string_idx: int) -> str:
        if string_idx >= string_ids_size:
            raise ValueError(f"DEX string index {string_idx} out of range ({string_ids_size})")
        (string_data_off,) = struct.unpack_from("<I", data, string_ids_off + string_idx * 4)
        _, text_start = _read_uleb128(data, string_data_off)
        text_end = data.find(b"\x00", text_start)
        if text_end == -1:
            raise ValueError(f"unterminated DEX string at offset {string_data_off}")
        return data[text_start:text_end].decode("utf-8", errors="replace")

    descriptors: set[str] = set()
    type_strings: list[str] = []
    for i in range(type_ids_size):
        (descriptor_idx,) = struct.unpack_from("<I", data, type_ids_off + i * 4)
        desc = read_string(descriptor_idx)
        type_strings.append(desc)
        if desc.startswith("L") and desc.endswith(";"):
            descriptors.add(desc)

    for i in range(class_defs_size):
        (class_idx,) = struct.unpack_from("<I", data, class_defs_off + i * 32)
        if class_idx < len(type_strings):
            descriptors.add(type_strings[class_idx])

    return descriptors


def extract_dex_descriptors_dexdump(dex_paths: list[Path], dexdump: str) -> set[str]:
    descriptors: set[str] = set()
    for dex_path in dex_paths:
        proc = subprocess.run(
            [dexdump, str(dex_path)],
            check=True,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=120,
        )
        for match in DEX_CLASS_DESCRIPTOR_RE.finditer(proc.stdout):
            descriptors.add(match.group(1))
    return descriptors


def find_forbidden_dex_descriptors(descriptors: set[str]) -> list[str]:
    hits: list[str] = []
    for desc in sorted(descriptors):
        for pattern in FORBIDDEN_DEX_PATTERNS:
            if pattern in desc:
                hits.append(f"{desc} (matched {pattern!r})")
                break
    return hits


def audit_apk_dex(
    apk_path: Path,
    *,
    dexdump: str | None = None,
    require_app_classes: bool = True,
) -> DexAuditResult:
    with zipfile.ZipFile(apk_path, "r") as zf:
        dex_names = sorted(
            name for name in zf.namelist()
            if re.fullmatch(r"classes\d*\.dex", name)
        )
        if not dex_names:
            raise RuntimeError(f"APK {apk_path} contains no classes*.dex entries")

        descriptors: set[str] = set()
        tool = "python-dex-parser"
        with tempfile.TemporaryDirectory(prefix="amux-dex-audit-") as tmp:
            tmp_dir = Path(tmp)
            dex_paths: list[Path] = []
            for name in dex_names:
                raw = zf.read(name)
                descriptors.update(parse_dex_descriptors(raw))
                dex_file = tmp_dir / name
                dex_file.write_bytes(raw)
                dex_paths.append(dex_file)

            if dexdump:
                descriptors.update(extract_dex_descriptors_dexdump(dex_paths, dexdump))
                tool = f"dexdump+python ({Path(dexdump).name})"

    if require_app_classes:
        missing = [desc for desc in REQUIRED_DEX_DESCRIPTORS if desc not in descriptors]
        if missing:
            raise RuntimeError(
                f"APK {apk_path} is missing expected app DEX descriptors: {', '.join(missing)}"
            )

    forbidden = tuple(find_forbidden_dex_descriptors(descriptors))
    return DexAuditResult(
        dex_files=tuple(dex_names),
        descriptor_count=len(descriptors),
        tool=tool,
        forbidden_hits=forbidden,
    )


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

    readelf_bin = readelf or find_ndk_tool("llvm-readelf", ("readelf",))
    if not readelf_bin:
        raise RuntimeError("neither NDK llvm-readelf nor system readelf was found")
    nm_bin = nm or find_ndk_tool("llvm-nm", ("nm",))
    if not nm_bin:
        raise RuntimeError("neither NDK llvm-nm nor system nm was found")

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

    # Verify SQLite is statically linked: no undefined sqlite3_* imports AND defined
    # sqlite3_open / sqlite3_open_v2 / sqlite3_prepare_v2 (or embedded SQLite signature
    # if symbols were stripped).
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
    if defined_sqlite:
        sqlite_evidence = f"defined static symbols: {', '.join(defined_sqlite)}"
    elif b"SQLite format 3\x00" in raw_bytes and b"sqlite_master" in raw_bytes:
        sqlite_evidence = "stripped static SQLite engine present (no DT_NEEDED libsqlite, no undefined sqlite3_*)"
    else:
        raise RuntimeError(
            f"{so_path} shows neither defined sqlite3_open/sqlite3_prepare_v2 symbols "
            "nor embedded static SQLite engine markers"
        )

    # Deduplicate forbidden hits while preserving order.
    deduped_hits = tuple(dict.fromkeys(forbidden_hits))
    return NativeAuditResult(
        abi=abi,
        size_bytes=size,
        needed_libs=tuple(needed),
        sqlite_evidence=sqlite_evidence,
        forbidden_hits=deduped_hits,
    )


def audit_apk(
    apk_path: Path,
    *,
    abis: list[str],
    dexdump: str | None = None,
    readelf: str | None = None,
    nm: str | None = None,
    require_app_classes: bool = True,
) -> tuple[DexAuditResult, list[NativeAuditResult]]:
    if not apk_path.is_file():
        raise FileNotFoundError(f"Release APK not found at {apk_path}")

    dex_result = audit_apk_dex(
        apk_path,
        dexdump=dexdump,
        require_app_classes=require_app_classes,
    )

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

    return dex_result, native_results


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

    dexdump = find_dexdump()
    dex_result, native_results = audit_apk(
        apk_path,
        abis=abis,
        dexdump=dexdump,
    )

    print(
        f"scope-audit: APK {apk_path.name} ({apk_path.stat().st_size:,} bytes): "
        f"{len(dex_result.dex_files)} DEX file(s), {dex_result.descriptor_count} class descriptors "
        f"[{dex_result.tool}]",
        flush=True,
    )
    for nr in native_results:
        print(
            f"scope-audit: lib/{nr.abi}/{LIB_NAME} ({nr.size_bytes:,} bytes): "
            f"DT_NEEDED=[{', '.join(nr.needed_libs)}], SQLite={nr.sqlite_evidence}",
            flush=True,
        )

    failures: list[str] = []
    for hit in dex_result.forbidden_hits:
        failures.append(f"DEX forbidden class: {hit}")
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
