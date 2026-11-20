"""Unit tests for apps/android/tools/android-rust.py."""

import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

REPO_ROOT = Path(__file__).resolve().parents[4]
SCRIPT = REPO_ROOT / "apps/android/tools/android-rust.py"


def load_module():
    spec = importlib.util.spec_from_file_location("android_rust", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class AndroidRustScriptTest(unittest.TestCase):
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
