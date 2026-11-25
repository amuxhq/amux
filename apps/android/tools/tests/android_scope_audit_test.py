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


def build_fake_apk(
    path: Path,
    *,
    so_bytes: bytes | None = b"\x7fELFfake-release-so",
    abi: str = "arm64-v8a",
) -> Path:
    with zipfile.ZipFile(path, "w") as zf:
        zf.writestr("classes.dex", b"dex\n035\x00")
        if so_bytes is not None:
            zf.writestr(f"lib/{abi}/libamux_app.so", so_bytes)
    return path


def write_mapping(path: Path, classes: list[str]) -> Path:
    """A mapping in R8's shape: a header, then each class with a member under it."""
    lines = ["# compiler: R8", "# pg_map_id: 0123456"]
    for index, name in enumerate(classes):
        lines.append(f"{name} -> a{index}:")
        lines.append("    void <init>() -> <init>")
        lines.append("    1:1:void run():12:12 -> a")
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return path


APP_CLASSES = ["sh.amux.android.MainActivity", "sh.amux.core.runtime.NativeBridge"]


CLEAN_SECTIONS_OUT = (
    "Section Headers:\n"
    "  [ 0]                   NULL            0000000000000000 000000 000000\n"
    "  [ 1] .dynsym           DYNSYM          0000000000000200 000200 001000\n"
    "  [ 2] .text             PROGBITS        0000000000010000 010000 100000\n"
    "  [ 3] .rodata           PROGBITS        0000000000110000 110000 020000\n"
)


class AndroidScopeAuditTest(unittest.TestCase):
    def test_mapping_lists_source_names_not_members_or_comments(self):
        text = (
            "# compiler: R8\n"
            "sh.amux.android.MainActivity -> sh.amux.android.MainActivity:\n"
            "    sh.amux.android.DoorServer door -> a\n"
            "sh.amux.features.AgentsHomeKt -> b.c:\n"
            "# {\"id\":\"sourceFile\",\"fileName\":\"AgentsHome.kt\"}\n"
        )
        self.assertEqual(
            audit.mapped_classes(text),
            {"sh.amux.android.MainActivity", "sh.amux.features.AgentsHomeKt"},
        )

    def test_audit_classes_passes_for_clean_release_classes(self):
        with tempfile.TemporaryDirectory() as tmp:
            mapping = write_mapping(
                Path(tmp) / "mapping.txt",
                APP_CLASSES + ["sh.amux.features.AgentsHomeKt", "sh.amux.push.SealedPush"],
            )
            result = audit.audit_classes(mapping)
            self.assertEqual(result.forbidden_hits, ())
            self.assertEqual(result.class_count, 4)

    def test_audit_classes_catches_every_forbidden_debug_or_test_class_even_renamed(self):
        forbidden_samples = [
            "sh.amux.android.DoorServer",
            "sh.amux.android.DoorHost",
            "sh.amux.android.StorybookActivity",
            "sh.amux.testsupport.Stories",
            "sh.amux.testsupport.ScriptedChat",
            "sh.amux.testsupport.ScriptedCloudService",
            "sh.amux.testsupport.ScriptedBillingStoreFront",
        ]
        with tempfile.TemporaryDirectory() as tmp:
            for sample in forbidden_samples:
                mapping = write_mapping(Path(tmp) / "mapping.txt", APP_CLASSES + [sample])
                result = audit.audit_classes(mapping)
                self.assertTrue(
                    result.forbidden_hits,
                    f"Expected forbidden class {sample} to be rejected",
                )

    def test_audit_classes_fails_when_expected_app_classes_missing(self):
        with tempfile.TemporaryDirectory() as tmp:
            mapping = write_mapping(Path(tmp) / "mapping.txt", ["com.example.Other"])
            with self.assertRaisesRegex(RuntimeError, "missing expected app classes"):
                audit.audit_classes(mapping)

    def test_audit_classes_fails_without_a_mapping(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaisesRegex(RuntimeError, "must be minified"):
                audit.audit_classes(Path(tmp) / "mapping.txt")

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

    def test_inspect_native_library_refuses_sqlite_it_cannot_see_defined(self):
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
                with self.assertRaisesRegex(RuntimeError, "defines none of"):
                    audit.inspect_native_library(
                        so,
                        abi="arm64-v8a",
                        readelf="llvm-readelf",
                        nm="llvm-nm",
                    )

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

    def test_audit_native_libraries_fails_when_native_library_is_missing(self):
        with tempfile.TemporaryDirectory() as tmp:
            apk = build_fake_apk(Path(tmp) / "no-so.apk", so_bytes=None)
            with self.assertRaisesRegex(RuntimeError, "missing required native library"):
                audit.audit_native_libraries(apk, abis=["arm64-v8a"], readelf="llvm-readelf", nm="llvm-nm")

    @staticmethod
    def _synthetic_elf64(p_align: int, *, p_offset: int = 0x4000, p_vaddr: int = 0x4000) -> bytes:
        header = bytearray(64)
        header[0:4] = b"\x7fELF"
        header[4] = 2  # ELFCLASS64
        header[5] = 1  # ELFDATA2LSB
        struct.pack_into("<Q", header, 32, 64)  # e_phoff
        struct.pack_into("<HH", header, 54, 56, 1)  # e_phentsize, e_phnum
        phdr = struct.pack("<IIQQQQQQ", 1, 5, p_offset, p_vaddr, p_vaddr, 0x100, 0x100, p_align)
        return bytes(header) + phdr

    def test_audit_page_size_16kb_passes_for_16k_aligned_bundle(self):
        with tempfile.TemporaryDirectory() as tmp:
            aab = Path(tmp) / "release.aab"
            with zipfile.ZipFile(aab, "w") as zf:
                zf.writestr("BundleConfig.pb", b"\n\x08\x12\x061.16.0\x12\x06\x12\x04\x08\x01\x10\x02")
                zf.writestr("base/lib/arm64-v8a/libamux_app.so", self._synthetic_elf64(0x4000))
                zf.writestr("base/lib/arm64-v8a/libother.so", self._synthetic_elf64(0x4000))
            count, note = audit.audit_page_size_16kb(aab, abis=["arm64-v8a"])
            self.assertEqual(count, 2)
            self.assertEqual(note, "BundleConfig.pb=PAGE_ALIGNMENT_16K")

    def test_audit_page_size_16kb_rejects_4k_aligned_so(self):
        with tempfile.TemporaryDirectory() as tmp:
            aab = Path(tmp) / "release.aab"
            with zipfile.ZipFile(aab, "w") as zf:
                zf.writestr("BundleConfig.pb", b"\n\x08\x12\x061.16.0\x12\x06\x12\x04\x08\x01\x10\x02")
                zf.writestr("base/lib/arm64-v8a/libbad.so", self._synthetic_elf64(0x1000))
            with self.assertRaisesRegex(RuntimeError, r"libbad\.so is not 16 KB aligned"):
                audit.audit_page_size_16kb(aab, abis=["arm64-v8a"])

    def test_audit_page_size_16kb_rejects_bundle_missing_page_alignment_16k(self):
        with tempfile.TemporaryDirectory() as tmp:
            aab = Path(tmp) / "release.aab"
            with zipfile.ZipFile(aab, "w") as zf:
                zf.writestr("BundleConfig.pb", b"\n\x08\x12\x061.16.0")
                zf.writestr("base/lib/arm64-v8a/libamux_app.so", self._synthetic_elf64(0x4000))
            with self.assertRaisesRegex(RuntimeError, "PAGE_ALIGNMENT_16K"):
                audit.audit_page_size_16kb(aab, abis=["arm64-v8a"])

    def test_audit_page_size_16kb_rejects_bundle_with_page_alignment_4k(self):
        with tempfile.TemporaryDirectory() as tmp:
            aab = Path(tmp) / "release.aab"
            with zipfile.ZipFile(aab, "w") as zf:
                zf.writestr("BundleConfig.pb", b"\n\x08\x12\x061.16.0\x12\x06\x12\x04\x08\x01\x10\x01")
                zf.writestr("base/lib/arm64-v8a/libamux_app.so", self._synthetic_elf64(0x4000))
            with self.assertRaisesRegex(RuntimeError, "alignment=1"):
                audit.audit_page_size_16kb(aab, abis=["arm64-v8a"])


if __name__ == "__main__":
    unittest.main()
