#!/usr/bin/env python3
"""Audit the shipping Android app bundle (or a release APK) and its JNI library.

Run by Buck2 as `//apps/android/app:scope-audit` (`just android
scope-audit`), on a Linux worker, with the release build's outputs and the
NDK's tools as arguments. It:
1. Reads R8's `mapping.txt` for the release build, which names every class
   that survived shrinking by its source name, and proves no debug or
   test-support class ships (`DoorServer`, `DoorHost`, `StorybookActivity`,
   `sh.amux.testsupport`, `ScriptedChat`, `ScriptedCloudService`,
   `ScriptedBillingStoreFront`). The DEX itself names classes as R8 renamed
   them, so a renamed debug class would pass a look at the DEX.
2. Extracts `lib/<abi>/libamux_app.so` from the APK, or `base/lib/<abi>/`
   from the bundle, and inspects it with the
   NDK's `llvm-readelf` and `llvm-nm` to prove no `debug-tools`,
   `amux_debug` or `testnet` symbol is present and SQLite is statically
   linked (no `libsqlite*.so` in `DT_NEEDED`, and `sqlite3_*` defined).
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from pathlib import Path
import re
import struct
import subprocess
import sys
import tempfile
import zipfile

LIB_NAME = "libamux_app.so"
PAGE_SIZE_16K = 16384
PAGE_ALIGNMENT_16K = 2
SIXTY_FOUR_BIT_ABIS = frozenset({"arm64-v8a", "x86_64"})

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
    readelf: str,
    nm: str,
) -> NativeAuditResult:
    if not so_path.is_file():
        raise RuntimeError(f"missing native library: {so_path}")
    size = so_path.stat().st_size
    if size == 0:
        raise RuntimeError(f"native library is empty: {so_path}")

    readelf_bin = readelf
    nm_bin = nm

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
    readelf: str,
    nm: str,
) -> list[NativeAuditResult]:
    if not apk_path.is_file():
        raise FileNotFoundError(f"Release APK not found at {apk_path}")

    native_results: list[NativeAuditResult] = []
    with zipfile.ZipFile(apk_path, "r") as zf:
        with tempfile.TemporaryDirectory(prefix="amux-so-audit-") as tmp:
            tmp_dir = Path(tmp)
            names = set(zf.namelist())
            for abi in abis:
                # An APK keeps it under lib/, an app bundle under base/lib/.
                member = next(
                    (m for m in (f"lib/{abi}/{LIB_NAME}", f"base/lib/{abi}/{LIB_NAME}") if m in names),
                    f"lib/{abi}/{LIB_NAME}",
                )
                if member not in names:
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


def _read_varint(buf: bytes, pos: int) -> tuple[int, int]:
    value = 0
    shift = 0
    while pos < len(buf):
        byte = buf[pos]
        pos += 1
        value |= (byte & 0x7F) << shift
        if not (byte & 0x80):
            return value, pos
        shift += 7
        if shift > 63:
            break
    raise RuntimeError("malformed protobuf varint in BundleConfig.pb")


def _iter_proto_fields(buf: bytes):
    """Yield (field_number, wire_type, value) from a protobuf message."""
    pos = 0
    end = len(buf)
    while pos < end:
        tag, pos = _read_varint(buf, pos)
        field_num = tag >> 3
        wire_type = tag & 0x07
        if wire_type == 0:
            val, pos = _read_varint(buf, pos)
            yield field_num, wire_type, val
        elif wire_type == 1:
            if pos + 8 > end:
                raise RuntimeError("truncated 64-bit field in BundleConfig.pb")
            yield field_num, wire_type, buf[pos : pos + 8]
            pos += 8
        elif wire_type == 2:
            length, pos = _read_varint(buf, pos)
            if pos + length > end:
                raise RuntimeError("truncated length-delimited field in BundleConfig.pb")
            yield field_num, wire_type, buf[pos : pos + length]
            pos += length
        elif wire_type == 5:
            if pos + 4 > end:
                raise RuntimeError("truncated 32-bit field in BundleConfig.pb")
            yield field_num, wire_type, buf[pos : pos + 4]
            pos += 4
        else:
            raise RuntimeError(f"unsupported protobuf wire type {wire_type} in BundleConfig.pb")


def bundle_config_uncompressed_alignment(bundle_config: bytes) -> tuple[bool, int]:
    """Read (enabled, alignment) from BundleConfig.optimizations.uncompress_native_libraries."""
    enabled = False
    alignment = 0
    for field_num, wire_type, value in _iter_proto_fields(bundle_config):
        # BundleConfig.optimizations = field 2
        if field_num == 2 and wire_type == 2:
            for opt_num, opt_wire, opt_val in _iter_proto_fields(value):
                # Optimizations.uncompress_native_libraries = field 2
                if opt_num == 2 and opt_wire == 2:
                    for lib_num, lib_wire, lib_val in _iter_proto_fields(opt_val):
                        if lib_num == 1 and lib_wire == 0:
                            enabled = bool(lib_val)
                        elif lib_num == 2 and lib_wire == 0:
                            alignment = int(lib_val)
    return enabled, alignment


def elf_pt_load_segments(data: bytes, label: str) -> list[tuple[int, int, int]]:
    """Return (p_offset, p_vaddr, p_align) for every PT_LOAD segment in an ELF64 LE binary."""
    if len(data) < 64 or data[:4] != b"\x7fELF":
        raise RuntimeError(f"{label} is not a valid ELF binary")
    if data[4] != 2 or data[5] != 1:
        raise RuntimeError(f"{label} is not a 64-bit little-endian ELF binary")
    e_phoff = struct.unpack_from("<Q", data, 32)[0]
    e_phentsize, e_phnum = struct.unpack_from("<HH", data, 54)
    if e_phentsize < 56 or e_phoff + e_phentsize * e_phnum > len(data):
        raise RuntimeError(f"{label} has an invalid ELF program header table")
    segments: list[tuple[int, int, int]] = []
    for index in range(e_phnum):
        offset = e_phoff + index * e_phentsize
        p_type, _flags, p_offset, p_vaddr, _paddr, _filesz, _memsz, p_align = struct.unpack_from(
            "<IIQQQQQQ", data, offset
        )
        if p_type == 1:  # PT_LOAD
            segments.append((p_offset, p_vaddr, p_align))
    if not segments:
        raise RuntimeError(f"{label} contains no PT_LOAD segments")
    return segments


def audit_page_size_16kb(apk_path: Path, *, abis: list[str]) -> tuple[int, str]:
    """Verify 16 KB ELF LOAD alignment on all 64-bit .so files and PAGE_ALIGNMENT_16K in BundleConfig.pb."""
    checked = 0
    target_abis = [abi for abi in abis if abi in SIXTY_FOUR_BIT_ABIS]
    with zipfile.ZipFile(apk_path, "r") as zf:
        names = zf.namelist()
        prefixes = tuple(
            prefix
            for abi in target_abis
            for prefix in (f"lib/{abi}/", f"base/lib/{abi}/")
        )
        for member in sorted(names):
            if member.endswith(".so") and member.startswith(prefixes):
                segments = elf_pt_load_segments(zf.read(member), member)
                for p_offset, p_vaddr, p_align in segments:
                    if p_align < PAGE_SIZE_16K or (p_offset % PAGE_SIZE_16K) != (p_vaddr % PAGE_SIZE_16K):
                        raise RuntimeError(
                            f"{member} is not 16 KB aligned: "
                            f"PT_LOAD p_align=0x{p_align:x}, p_offset=0x{p_offset:x}, p_vaddr=0x{p_vaddr:x}"
                        )
                checked += 1

        if "BundleConfig.pb" in names:
            enabled, alignment = bundle_config_uncompressed_alignment(zf.read("BundleConfig.pb"))
            if not enabled or alignment != PAGE_ALIGNMENT_16K:
                raise RuntimeError(
                    f"{apk_path.name} BundleConfig.pb does not request 16 KB native library alignment "
                    f"(uncompressNativeLibraries enabled={enabled}, alignment={alignment}; "
                    f"expected enabled=True, alignment={PAGE_ALIGNMENT_16K} / PAGE_ALIGNMENT_16K)"
                )
            config_note = "BundleConfig.pb=PAGE_ALIGNMENT_16K"
        else:
            config_note = "APK (no BundleConfig.pb)"

    if target_abis and checked == 0:
        raise RuntimeError(f"{apk_path.name} has no 64-bit .so files under {', '.join(target_abis)}")
    return checked, config_note


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apk", type=Path, required=True, help="the app bundle or release APK to audit")
    parser.add_argument("--mapping", type=Path, required=True, help="R8's mapping.txt for that APK")
    parser.add_argument("--readelf", required=True, help="the NDK's llvm-readelf")
    parser.add_argument("--nm", required=True, help="the NDK's llvm-nm")
    parser.add_argument(
        "--abi",
        action="append",
        dest="abis",
        help="Android ABI to require and audit inside the APK (defaults to arm64-v8a).",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    abis = args.abis if args.abis else ["arm64-v8a"]

    apk_path = args.apk.resolve()
    class_result = audit_classes(args.mapping.resolve())
    native_results = audit_native_libraries(apk_path, abis=abis, readelf=args.readelf, nm=args.nm)
    aligned_so_count, page_config_note = audit_page_size_16kb(apk_path, abis=abis)

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
    print(
        f"scope-audit: 16 KB page alignment: {aligned_so_count} .so file(s) ELF-aligned, {page_config_note}",
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

    print(
        "scope-audit: PASS (no debug/test classes, symbols or DWARF sections; SQLite statically linked; 16 KB page-aligned)",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
