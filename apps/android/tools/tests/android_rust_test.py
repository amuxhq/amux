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
        dev_arm = mod.build_fingerprint("dev", ["arm64-v8a"], root=REPO_ROOT)
        dev_both = mod.build_fingerprint("dev", ["arm64-v8a", "x86_64"], root=REPO_ROOT)
        dev_both_reordered = mod.build_fingerprint("dev", ["x86_64", "arm64-v8a"], root=REPO_ROOT)
        rel_arm = mod.build_fingerprint("release", ["arm64-v8a"], root=REPO_ROOT)
        host = mod.build_fingerprint("host", ["arm64-v8a"], root=REPO_ROOT)

        self.assertEqual(dev_both, dev_both_reordered)
        self.assertNotEqual(dev_arm, dev_both)
        self.assertNotEqual(dev_arm, rel_arm)
        self.assertNotEqual(dev_arm, host)
        self.assertNotEqual(rel_arm, host)

    def test_fingerprint_invalidates_on_ndk_change_but_ignores_git_sha(self):
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
                    fp_rel_b = mod.build_fingerprint("release", ["arm64-v8a"], ndk=str(ndk_b))
                    fp_host = mod.build_fingerprint("host", ["arm64-v8a"])
                    self.assertNotEqual(fp_a, fp_b)

                    # Updating Pkg.Revision in place also invalidates the fingerprint.
                    (ndk_a / "source.properties").write_text("Pkg.Revision = 28.3.0\n", encoding="utf-8")
                    fp_a_updated = mod.build_fingerprint("dev", ["arm64-v8a"], ndk=str(ndk_a))
                    self.assertNotEqual(fp_a, fp_a_updated)

                with mock.patch.dict(os.environ, {"AMUX_GIT_SHA": "bbb222"}):
                    self.assertEqual(fp_b, mod.build_fingerprint("dev", ["arm64-v8a"], ndk=str(ndk_b)))
                    self.assertEqual(fp_rel_b, mod.build_fingerprint("release", ["arm64-v8a"], ndk=str(ndk_b)))
                    self.assertEqual(fp_host, mod.build_fingerprint("host", ["arm64-v8a"]))

    def test_source_fingerprint_scopes_to_app_jni_transitive_closure(self):
        mod = load_module()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "Cargo.toml").write_text("[workspace]\nmembers = []\n", encoding="utf-8")
            (root / "Cargo.lock").write_text("version = 4\n", encoding="utf-8")
            (root / "rust-toolchain.toml").write_text("[toolchain]\n", encoding="utf-8")
            (root / ".cargo").mkdir()
            (root / ".cargo/config.toml").write_text("[build]\n", encoding="utf-8")

            def make_crate(folder: str, name: str, deps: list[str], body: str) -> Path:
                crate_dir = root / "crates" / folder
                (crate_dir / "src").mkdir(parents=True)
                dep_lines = "\n".join(f'{dep} = {{ workspace = true }}' for dep in deps)
                (crate_dir / "Cargo.toml").write_text(
                    f'[package]\nname = "{name}"\nversion = "0.1.0"\n\n[dependencies]\n{dep_lines}\n',
                    encoding="utf-8",
                )
                src = crate_dir / "src" / "lib.rs"
                src.write_text(body, encoding="utf-8")
                return src

            make_crate("app-jni", "app-jni", ["app-bridge"], "pub fn jni() {}")
            bridge_src = make_crate("app-bridge", "app-bridge", ["node"], "pub fn bridge() {}")
            node_src = make_crate("node", "node", [], "pub fn node() {}")
            tui_src = make_crate("tui", "tui", [], "pub fn tui() {}")

            with mock.patch.dict(os.environ, {"AMUX_GIT_SHA": "commit-1"}):
                base = mod.source_fingerprint(root)
            with mock.patch.dict(os.environ, {"AMUX_GIT_SHA": "commit-2"}):
                self.assertEqual(base, mod.source_fingerprint(root))

            # Editing an unrelated workspace crate does not bust the fingerprint.
            tui_src.write_text("pub fn tui() { let _ = 1; }", encoding="utf-8")
            self.assertEqual(base, mod.source_fingerprint(root))

            # Editing a direct dependency (app-bridge) invalidates the fingerprint.
            bridge_src.write_text("pub fn bridge() { let _ = 2; }", encoding="utf-8")
            after_bridge = mod.source_fingerprint(root)
            self.assertNotEqual(base, after_bridge)

            # Editing a transitive dependency (node) invalidates the fingerprint.
            node_src.write_text("pub fn node() { let _ = 3; }", encoding="utf-8")
            after_node = mod.source_fingerprint(root)
            self.assertNotEqual(after_bridge, after_node)

    def test_real_workspace_closure_excludes_unrelated_crates(self):
        mod = load_module()
        closure = set(mod.crate_closure("app-jni", REPO_ROOT))
        for expected in ("app-jni", "app-bridge", "app-embedded", "app-runtime", "node", "wire"):
            self.assertIn(expected, closure)
        for unrelated in ("tui", "amux", "models", "testnet", "ios-tools", "app-ffi"):
            self.assertNotIn(unrelated, closure)

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
