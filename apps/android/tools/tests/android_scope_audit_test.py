"""Unit tests for apps/android/tools/android-scope-audit.py."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import struct
import sys
import tempfile
import unittest
from unittest import mock
import zipfile

TOOLS_DIR = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "android_scope_audit",
    TOOLS_DIR / "android-scope-audit.py",
)
assert SPEC and SPEC.loader
audit = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = audit
SPEC.loader.exec_module(audit)


def _encode_uleb128(value: int) -> bytes:
    out = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        if value:
            out.append(byte | 0x80)
        else:
            out.append(byte)
            return bytes(out)


def build_minimal_dex(class_descriptors: list[str]) -> bytes:
    """Build a valid minimal DEX 035 binary containing the given class descriptors."""
    strings = sorted(set(class_descriptors))
    header_size = 112
    string_ids_off = header_size
    type_ids_off = string_ids_off + len(strings) * 4
    class_defs_off = type_ids_off + len(strings) * 4
    data_off = class_defs_off + len(strings) * 32

    string_data = bytearray()
    string_offsets: list[int] = []
    for text in strings:
        string_offsets.append(data_off + len(string_data))
        encoded = text.encode("utf-8")
        string_data.extend(_encode_uleb128(len(text)))
        string_data.extend(encoded)
        string_data.append(0)

    total_size = data_off + len(string_data)
    buf = bytearray(total_size)
    buf[0:8] = b"dex\n035\x00"
    struct.pack_into("<I", buf, 32, total_size)
    struct.pack_into("<I", buf, 36, header_size)
    struct.pack_into("<I", buf, 40, 0x12345678)
    struct.pack_into("<II", buf, 56, len(strings), string_ids_off)
    struct.pack_into("<II", buf, 64, len(strings), type_ids_off)
    struct.pack_into("<II", buf, 96, len(strings), class_defs_off)

    for idx, off in enumerate(string_offsets):
        struct.pack_into("<I", buf, string_ids_off + idx * 4, off)
        struct.pack_into("<I", buf, type_ids_off + idx * 4, idx)
        struct.pack_into("<I", buf, class_defs_off + idx * 32, idx)

    buf[data_off:] = string_data
    return bytes(buf)


def build_fake_apk(
    path: Path,
    *,
    dex_classes: list[str],
    so_bytes: bytes | None = b"\x7fELFfake-release-so",
    abi: str = "arm64-v8a",
) -> Path:
    with zipfile.ZipFile(path, "w") as zf:
        zf.writestr("classes.dex", build_minimal_dex(dex_classes))
        if so_bytes is not None:
            zf.writestr(f"lib/{abi}/libamux_app.so", so_bytes)
    return path


CLEAN_SECTIONS_OUT = (
    "Section Headers:\n"
    "  [ 0]                   NULL            0000000000000000 000000 000000\n"
    "  [ 1] .dynsym           DYNSYM          0000000000000200 000200 001000\n"
    "  [ 2] .text             PROGBITS        0000000000010000 010000 100000\n"
    "  [ 3] .rodata           PROGBITS        0000000000110000 110000 020000\n"
)


class AndroidScopeAuditTest(unittest.TestCase):
    def test_pure_python_dex_parser_extracts_class_descriptors(self):
        dex = build_minimal_dex([
            "Lsh/amux/android/MainActivity;",
            "Lsh/amux/core/runtime/NativeBridge;",
            "Lsh/amux/android/DoorServer;",
        ])
        descriptors = audit.parse_dex_descriptors(dex)
        self.assertEqual(
            descriptors,
            {
                "Lsh/amux/android/MainActivity;",
                "Lsh/amux/core/runtime/NativeBridge;",
                "Lsh/amux/android/DoorServer;",
            },
        )

    def test_audit_apk_dex_passes_for_clean_release_classes(self):
        with tempfile.TemporaryDirectory() as tmp:
            apk = build_fake_apk(
                Path(tmp) / "app-release.apk",
                dex_classes=[
                    "Lsh/amux/android/MainActivity;",
                    "Lsh/amux/core/runtime/NativeBridge;",
                    "Lsh/amux/features/AgentsHomeKt;",
                ],
            )
            result = audit.audit_apk_dex(apk, dexdump=None)
            self.assertEqual(result.forbidden_hits, ())
            self.assertEqual(result.dex_files, ("classes.dex",))
            self.assertEqual(result.tool, "python-dex-parser")

    def test_audit_apk_dex_catches_every_forbidden_debug_or_test_class(self):
        forbidden_samples = [
            "Lsh/amux/android/DoorServer;",
            "Lsh/amux/android/DoorHost;",
            "Lsh/amux/android/StorybookActivity;",
            "Lsh/amux/testsupport/Stories;",
            "Lsh/amux/testsupport/ScriptedChat;",
            "Lsh/amux/testsupport/ScriptedCloudService;",
            "Lsh/amux/testsupport/ScriptedBillingStoreFront;",
        ]
        with tempfile.TemporaryDirectory() as tmp:
            for sample in forbidden_samples:
                apk = build_fake_apk(
                    Path(tmp) / "bad.apk",
                    dex_classes=[
                        "Lsh/amux/android/MainActivity;",
                        "Lsh/amux/core/runtime/NativeBridge;",
                        sample,
                    ],
                )
                result = audit.audit_apk_dex(apk, dexdump=None)
                self.assertTrue(
                    result.forbidden_hits,
                    f"Expected forbidden DEX class {sample} to be rejected",
                )

    def test_audit_apk_dex_fails_when_expected_app_classes_missing(self):
        with tempfile.TemporaryDirectory() as tmp:
            apk = build_fake_apk(
                Path(tmp) / "empty-classes.apk",
                dex_classes=["Lcom/example/Other;"],
            )
            with self.assertRaisesRegex(RuntimeError, "missing expected app DEX descriptors"):
                audit.audit_apk_dex(apk, dexdump=None)

    def test_inspect_native_library_passes_with_static_sqlite_and_clean_symbols(self):
        readelf_out = (
            " 0x0000000000000001 (NEEDED) Shared library: [libdl.so]\n"
            " 0x0000000000000001 (NEEDED) Shared library: [libc.so]\n"
            " 0x0000000000000001 (NEEDED) Shared library: [libm.so]\n"
        )
        nm_out = (
            "0000000000123456 T Java_sh_amux_core_runtime_NativeBridge_version\n"
            "0000000000234567 t sqlite3_open_v2\n"
            "0000000000234890 t sqlite3_prepare_v2\n"
        )
        with tempfile.TemporaryDirectory() as tmp:
            so = Path(tmp) / "libamux_app.so"
            so.write_bytes(b"\x7fELFclean-release-binary")

            def fake_run(cmd, **kwargs):
                if "readelf" in cmd[0]:
                    if "--section-headers" in cmd:
                        return mock.Mock(returncode=0, stdout=CLEAN_SECTIONS_OUT, stderr="")
                    return mock.Mock(returncode=0, stdout=readelf_out, stderr="")
                return mock.Mock(returncode=0, stdout=nm_out, stderr="")

            with mock.patch.object(audit.subprocess, "run", side_effect=fake_run):
                result = audit.inspect_native_library(
                    so,
                    abi="arm64-v8a",
                    readelf="llvm-readelf",
                    nm="llvm-nm",
                )
            self.assertEqual(result.forbidden_hits, ())
            self.assertEqual(result.needed_libs, ("libdl.so", "libc.so", "libm.so"))
            self.assertIn("sqlite3_open_v2", result.sqlite_evidence)

    def test_inspect_native_library_rejects_dynamic_sqlite_needed(self):
        readelf_out = (
            " 0x0000000000000001 (NEEDED) Shared library: [libc.so]\n"
            " 0x0000000000000001 (NEEDED) Shared library: [libsqlite3.so]\n"
        )
        with tempfile.TemporaryDirectory() as tmp:
            so = Path(tmp) / "libamux_app.so"
            so.write_bytes(b"\x7fELFdynamic-sqlite")
            with mock.patch.object(
                audit.subprocess,
                "run",
                return_value=mock.Mock(returncode=0, stdout=readelf_out, stderr=""),
            ):
                with self.assertRaisesRegex(RuntimeError, "dynamically links SQLite"):
                    audit.inspect_native_library(
                        so,
                        abi="arm64-v8a",
                        readelf="llvm-readelf",
                        nm="llvm-nm",
                    )

    def test_inspect_native_library_flags_debug_tools_and_testnet_markers(self):
        readelf_out = " 0x0000000000000001 (NEEDED) Shared library: [libc.so]\n"
        nm_out = (
            "0000000000100000 T amux_debug_hook\n"
            "0000000000100100 t testnet::launch\n"
            "0000000000200000 t sqlite3_open_v2\n"
        )
        with tempfile.TemporaryDirectory() as tmp:
            so = Path(tmp) / "libamux_app.so"
            so.write_bytes(b"\x7fELF0.10.0+debug-tools\x00")

            def fake_run(cmd, **kwargs):
                if "readelf" in cmd[0]:
                    if "--section-headers" in cmd:
                        return mock.Mock(returncode=0, stdout=CLEAN_SECTIONS_OUT, stderr="")
                    return mock.Mock(returncode=0, stdout=readelf_out, stderr="")
                return mock.Mock(returncode=0, stdout=nm_out, stderr="")

            with mock.patch.object(audit.subprocess, "run", side_effect=fake_run):
                result = audit.inspect_native_library(
                    so,
                    abi="arm64-v8a",
                    readelf="llvm-readelf",
                    nm="llvm-nm",
                )
            joined = "\n".join(result.forbidden_hits)
            self.assertIn("amux_debug_hook", joined)
            self.assertIn("testnet::launch", joined)
            self.assertIn("+debug-tools", joined)

    def test_inspect_native_library_fails_when_dynamic_nm_fails(self):
        readelf_out = " 0x0000000000000001 (NEEDED) Shared library: [libc.so]\n"
        with tempfile.TemporaryDirectory() as tmp:
            so = Path(tmp) / "libamux_app.so"
            # Even with embedded static SQLite markers, a failing --dynamic nm scan must fail.
            so.write_bytes(b"\x7fELFSQLite format 3\x00sqlite_master")

            def fake_run(cmd, **kwargs):
                if "readelf" in cmd[0]:
                    if "--section-headers" in cmd:
                        return mock.Mock(returncode=0, stdout=CLEAN_SECTIONS_OUT, stderr="")
                    return mock.Mock(returncode=0, stdout=readelf_out, stderr="")
                if "--dynamic" in cmd:
                    return mock.Mock(
                        returncode=2,
                        stdout="",
                        stderr="nm: unrecognized option `--dynamic'",
                    )
                return mock.Mock(returncode=1, stdout="", stderr="nm: no symbols")

            with mock.patch.object(audit.subprocess, "run", side_effect=fake_run):
                with self.assertRaisesRegex(
                    RuntimeError,
                    r"nm --dynamic --demangle failed.*exit 2.*unrecognized option",
                ):
                    audit.inspect_native_library(
                        so,
                        abi="arm64-v8a",
                        readelf="llvm-readelf",
                        nm="nm",
                    )

    def test_inspect_native_library_allows_stripped_full_symbols_when_dynamic_nm_succeeds(self):
        readelf_out = " 0x0000000000000001 (NEEDED) Shared library: [libc.so]\n"
        dyn_nm_out = "0000000000123456 T Java_sh_amux_core_runtime_NativeBridge_version\n"
        with tempfile.TemporaryDirectory() as tmp:
            so = Path(tmp) / "libamux_app.so"
            so.write_bytes(b"\x7fELFSQLite format 3\x00sqlite_master")

            def fake_run(cmd, **kwargs):
                if "readelf" in cmd[0]:
                    if "--section-headers" in cmd:
                        return mock.Mock(returncode=0, stdout=CLEAN_SECTIONS_OUT, stderr="")
                    return mock.Mock(returncode=0, stdout=readelf_out, stderr="")
                if "--dynamic" in cmd:
                    return mock.Mock(returncode=0, stdout=dyn_nm_out, stderr="")
                return mock.Mock(returncode=1, stdout="", stderr="no symbols")

            with mock.patch.object(audit.subprocess, "run", side_effect=fake_run):
                result = audit.inspect_native_library(
                    so,
                    abi="arm64-v8a",
                    readelf="llvm-readelf",
                    nm="llvm-nm",
                )
            self.assertEqual(result.forbidden_hits, ())
            self.assertIn("stripped static SQLite engine present", result.sqlite_evidence)

    def test_inspect_native_library_rejects_dwarf_debug_sections(self):
        readelf_out = " 0x0000000000000001 (NEEDED) Shared library: [libc.so]\n"
        dwarf_sections_out = (
            CLEAN_SECTIONS_OUT
            + "  [ 4] .debug_info       PROGBITS        0000000000000000 130000 500000\n"
            + "  [ 5] .debug_line       PROGBITS        0000000000000000 630000 100000\n"
        )
        nm_out = (
            "0000000000123456 T Java_sh_amux_core_runtime_NativeBridge_version\n"
            "0000000000234567 t sqlite3_open_v2\n"
        )
        with tempfile.TemporaryDirectory() as tmp:
            so = Path(tmp) / "libamux_app.so"
            so.write_bytes(b"\x7fELFunstripped-dwarf")

            def fake_run(cmd, **kwargs):
                if "readelf" in cmd[0]:
                    if "--section-headers" in cmd:
                        return mock.Mock(returncode=0, stdout=dwarf_sections_out, stderr="")
                    return mock.Mock(returncode=0, stdout=readelf_out, stderr="")
                return mock.Mock(returncode=0, stdout=nm_out, stderr="")

            with mock.patch.object(audit.subprocess, "run", side_effect=fake_run):
                result = audit.inspect_native_library(
                    so,
                    abi="arm64-v8a",
                    readelf="llvm-readelf",
                    nm="llvm-nm",
                )
            joined = "\n".join(result.forbidden_hits)
            self.assertIn(".debug_info", joined)
            self.assertIn(".debug_line", joined)

    def test_audit_apk_fails_when_native_library_is_missing(self):
        with tempfile.TemporaryDirectory() as tmp:
            apk = build_fake_apk(
                Path(tmp) / "no-so.apk",
                dex_classes=[
                    "Lsh/amux/android/MainActivity;",
                    "Lsh/amux/core/runtime/NativeBridge;",
                ],
                so_bytes=None,
            )
            with self.assertRaisesRegex(RuntimeError, "missing required native library"):
                audit.audit_apk(apk, abis=["arm64-v8a"], dexdump=None)

    def test_locate_release_apk_picks_newest_candidate(self):
        with tempfile.TemporaryDirectory() as tmp:
            apk_dir = Path(tmp)
            unsigned = apk_dir / "app-release-unsigned.apk"
            signed = apk_dir / "app-release.apk"
            unsigned.write_bytes(b"u")
            signed.write_bytes(b"s")
            os_utime = (1_700_000_000, 1_700_000_000)
            import os
            os.utime(unsigned, os_utime)
            os.utime(signed, (1_700_000_100, 1_700_000_100))
            self.assertEqual(audit.locate_release_apk(apk_dir), signed)


if __name__ == "__main__":
    unittest.main()
