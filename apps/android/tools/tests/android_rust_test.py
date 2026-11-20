"""Unit tests for apps/android/tools/android-rust.py."""

import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

REPO_ROOT = Path(__file__).resolve().parents[4]
SCRIPT = REPO_ROOT / "apps/android/tools/android-rust.py"


def load_module():
    spec = importlib.util.spec_from_file_location("android_rust", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class AndroidRustScriptTest(unittest.TestCase):
    def test_debug_and_release_staging_paths_are_isolated_under_target(self):
        mod = load_module()
        self.assertEqual(mod.DEV_JNI_LIBS, Path("target/android/debug-jniLibs"))
        self.assertEqual(mod.RELEASE_JNI_LIBS, Path("target/android/release-jniLibs"))
        self.assertNotEqual(mod.DEV_JNI_LIBS, mod.RELEASE_JNI_LIBS)

    def test_fingerprints_distinguish_modes_and_abis(self):
        mod = load_module()
        dev_arm = mod.build_fingerprint("dev", ["arm64-v8a"])
        dev_both = mod.build_fingerprint("dev", ["arm64-v8a", "x86_64"])
        dev_both_reordered = mod.build_fingerprint("dev", ["x86_64", "arm64-v8a"])
        rel_arm = mod.build_fingerprint("release", ["arm64-v8a"])
        host = mod.build_fingerprint("host", ["arm64-v8a"])

        self.assertEqual(dev_both, dev_both_reordered)
        self.assertNotEqual(dev_arm, dev_both)
        self.assertNotEqual(dev_arm, rel_arm)
        self.assertNotEqual(dev_arm, host)
        self.assertNotEqual(rel_arm, host)

    def test_fingerprint_invalidates_on_ndk_or_git_sha_change(self):
        mod = load_module()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            ndk_a = root / "ndk-a"
            ndk_b = root / "ndk-b"
            ndk_a.mkdir()
            ndk_b.mkdir()
            (ndk_a / "source.properties").write_text("Pkg.Revision = 28.1.0\n", encoding="utf-8")
            (ndk_b / "source.properties").write_text("Pkg.Revision = 28.2.0\n", encoding="utf-8")

            with mock.patch.object(mod, "source_fingerprint", return_value="src"):
                with mock.patch.dict(os.environ, {"AMUX_GIT_SHA": "aaa111"}):
                    fp_a = mod.build_fingerprint("dev", ["arm64-v8a"], ndk=str(ndk_a))
                    fp_b = mod.build_fingerprint("dev", ["arm64-v8a"], ndk=str(ndk_b))
                    self.assertNotEqual(fp_a, fp_b)

                    # Updating Pkg.Revision in place also invalidates the fingerprint.
                    (ndk_a / "source.properties").write_text("Pkg.Revision = 28.3.0\n", encoding="utf-8")
                    fp_a_updated = mod.build_fingerprint("dev", ["arm64-v8a"], ndk=str(ndk_a))
                    self.assertNotEqual(fp_a, fp_a_updated)

                with mock.patch.dict(os.environ, {"AMUX_GIT_SHA": "bbb222"}):
                    fp_sha = mod.build_fingerprint("dev", ["arm64-v8a"], ndk=str(ndk_b))
                    self.assertNotEqual(fp_b, fp_sha)

    def test_prune_stale_abis_removes_unrequested_directories_including_on_cache_hit(self):
        mod = load_module()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            dev_dir = root / "debug-jniLibs"
            (dev_dir / "arm64-v8a").mkdir(parents=True)
            (dev_dir / "x86_64").mkdir(parents=True)
            arm_so = dev_dir / "arm64-v8a" / "libamux_app.so"
            x86_so = dev_dir / "x86_64" / "libamux_app.so"
            arm_so.write_bytes(b"arm64")
            x86_so.write_bytes(b"x86_64")

            stamp = root / "dev-jni-stamp.txt"
            stamp.write_text("cached-fp\n", encoding="utf-8")

            with (
                mock.patch.object(mod, "OUTPUT", root),
                mock.patch.object(mod, "DEV_JNI_LIBS", dev_dir),
                mock.patch.object(mod.subprocess, "run") as run_mock,
            ):
                mod.build_android(["arm64-v8a"], release=False, fingerprint="cached-fp")
                run_mock.assert_not_called()

            self.assertTrue(arm_so.is_file())
            self.assertFalse((dev_dir / "x86_64").exists())

    def test_copy_if_changed_preserves_file_when_identical(self):
        mod = load_module()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            src = root / "src.so"
            dst = root / "nested" / "dst.so"
            src.write_bytes(b"amux-jni-v1")

            self.assertTrue(mod.copy_if_changed(src, dst))
            self.assertEqual(dst.read_bytes(), b"amux-jni-v1")
            self.assertFalse(mod.copy_if_changed(src, dst))

            src.write_bytes(b"amux-jni-v2")
            self.assertTrue(mod.copy_if_changed(src, dst))
            self.assertEqual(dst.read_bytes(), b"amux-jni-v2")

    def test_cli_fingerprint_prints_sha256_hex(self):
        result = subprocess.run(
            [sys.executable, "-B", str(SCRIPT), "--host", "--fingerprint"],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
            check=True,
        )
        digest = result.stdout.strip()
        self.assertEqual(len(digest), 64, digest)
        int(digest, 16)


if __name__ == "__main__":
    unittest.main()
