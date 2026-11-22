import contextlib
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("sweep_script", ROOT / "scripts" / "sweep.py")
assert SPEC is not None and SPEC.loader is not None
sweep = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = sweep
SPEC.loader.exec_module(sweep)


def _write_bytes(path: Path, size: int, mtime: float | None = None) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"x" * size)
    if mtime is not None:
        os.utime(path, (mtime, mtime))


class SweepTest(unittest.TestCase):
    def setUp(self):
        self.room = tempfile.TemporaryDirectory()
        self.root = Path(self.room.name)
        self.target = self.root / "target"
        self.target.mkdir()

    def tearDown(self):
        self.room.cleanup()

    def _make_inc_dir(self, rel: str, size: int, mtime: float) -> Path:
        d = self.target / rel
        session = d / "s-session"
        _write_bytes(session / "dep-graph.bin", size, mtime=mtime)
        os.utime(session, (mtime, mtime))
        os.utime(d, (mtime, mtime))
        return d

    def test_prune_incremental_keeps_newest_within_budget(self):
        old = self._make_inc_dir("debug/incremental/node-old", 32_768, mtime=1000)
        mid = self._make_inc_dir("debug/incremental/node-mid", 32_768, mtime=2000)
        new = self._make_inc_dir("debug/incremental/node-new", 32_768, mtime=3000)
        # Budget fits 2 directories (using actual on-disk size of one dir).
        one_size = sweep.dir_bytes(new)

        kept, kept_bytes, removed = sweep.prune_incremental(self.target, one_size * 2)
        self.assertEqual(kept, 2)
        self.assertEqual(removed, 1)
        self.assertGreaterEqual(kept_bytes, one_size * 2)
        self.assertTrue(new.exists())
        self.assertTrue(mid.exists())
        self.assertFalse(old.exists())

    def test_prune_incremental_cold_removes_everything_including_triple_dirs(self):
        d1 = self._make_inc_dir("debug/incremental/node-a", 4096, mtime=1000)
        d2 = self._make_inc_dir(
            "aarch64-apple-ios/debug/incremental/wire-b", 4096, mtime=2000
        )
        # Nested ios/rust-cargo and android/rust-cargo are swept on their own passes.
        ios_d = self._make_inc_dir("ios/rust-cargo/debug/incremental/ffi-c", 4096, mtime=3000)
        android_host_d = self._make_inc_dir(
            "android/rust-cargo/host/phone-dev/incremental/jni-d", 4096, mtime=3000
        )
        android_target_d = self._make_inc_dir(
            "android/rust-cargo/android/aarch64-linux-android/phone-dev/incremental/jni-e",
            4096,
            mtime=3000,
        )

        kept, kept_bytes, removed = sweep.prune_incremental(self.target, 0)
        self.assertEqual((kept, kept_bytes, removed), (0, 0, 2))
        self.assertFalse(d1.exists())
        self.assertFalse(d2.exists())
        self.assertTrue(ios_d.exists())
        self.assertTrue(android_host_d.exists())
        self.assertTrue(android_target_d.exists())

    def test_prune_incremental_dry_run_deletes_nothing(self):
        old = self._make_inc_dir("debug/incremental/node-old", 32_768, mtime=1000)
        new = self._make_inc_dir("debug/incremental/node-new", 32_768, mtime=2000)
        one_size = sweep.dir_bytes(new)

        kept, _, removed = sweep.prune_incremental(self.target, one_size, dry_run=True)
        self.assertEqual((kept, removed), (1, 1))
        self.assertTrue(old.exists())
        self.assertTrue(new.exists())

    def test_unmanaged_bytes_excludes_cargo_managed_dirs(self):
        _write_bytes(self.target / "debug/deps/libnode.rlib", 100_000)
        _write_bytes(self.target / "debug/.fingerprint/node/lib-node", 50_000)
        _write_bytes(self.target / "debug/build/ring/out", 50_000)
        _write_bytes(self.target / "aarch64-apple-ios/debug/deps/libwire.rlib", 100_000)
        _write_bytes(self.target / "ios/DerivedData/blob", 40_000)
        _write_bytes(self.target / "android/debug-jniLibs/arm64-v8a/libamux_app.so", 40_000)
        _write_bytes(self.target / "android/rust-cargo/host/phone-dev/deps/libjni.rlib", 40_000)
        _write_bytes(self.target / "size/after-out/bin", 40_000)
        _write_bytes(self.target / "debug/incremental/node-a/dep.bin", 40_000)

        unmanaged = sweep.unmanaged_bytes(self.target)
        expected = (
            sweep.dir_bytes(self.target / "ios")
            + sweep.dir_bytes(self.target / "android")
            + sweep.dir_bytes(self.target / "size")
            + sweep.dir_bytes(self.target / "debug/incremental")
        )
        self.assertEqual(unmanaged, expected)

    def test_sweep_cargo_accounts_for_unmanaged_dirs_in_maxsize(self):
        _write_bytes(self.target / "ios/DerivedData/blob", 5_000_000)
        _write_bytes(self.target / "debug/deps/libnode.rlib", 10_000_000)
        calls: list[tuple[list[str], dict[str, str]]] = []

        def fake_runner(cmd, cwd, env, check):
            calls.append((cmd, env))
            return subprocess.CompletedProcess(cmd, 0)

        code = sweep.sweep_cargo(
            self.root,
            self.target,
            deps_budget=10_000_000,
            which=lambda _: "/usr/local/bin/cargo-sweep",
            runner=fake_runner,
        )
        self.assertEqual(code, 0)
        self.assertEqual(len(calls), 1)
        cmd, _ = calls[0]
        self.assertEqual(cmd[:3], ["cargo", "sweep", "--maxsize"])
        # Maxsize must cover both the ~5 MB unmanaged ios/ dir and the 10 MB deps budget.
        mb = int(cmd[3].removesuffix("MB"))
        self.assertGreaterEqual(mb, 15)
        self.assertLess(mb, 20)

    def test_sweep_also_sweeps_nested_ios_rust_cargo(self):
        ios_target = self.target / "ios" / "rust-cargo"
        _write_bytes(ios_target / "debug/incremental/ffi-a/s/f", 4096)
        _write_bytes(ios_target / "debug/deps/libffi.rlib", 4096)
        calls: list[tuple[list[str], str | None]] = []

        def fake_runner(cmd, cwd, env, check):
            calls.append((cmd, env.get("CARGO_TARGET_DIR")))
            return subprocess.CompletedProcess(cmd, 0)

        with contextlib.redirect_stdout(io.StringIO()):
            code = sweep.sweep(
                self.root,
                cold=True,
                which=lambda _: "/usr/local/bin/cargo-sweep",
                runner=fake_runner,
            )
        self.assertEqual(code, 0)
        self.assertFalse((ios_target / "debug/incremental/ffi-a").exists())
        self.assertEqual(len(calls), 2)
        self.assertIsNone(calls[0][1])
        self.assertEqual(calls[1][1], str(ios_target))

    def test_sweep_also_sweeps_nested_android_rust_cargo_host_and_android(self):
        android_root = self.target / "android" / "rust-cargo"
        host_target = android_root / "host"
        device_target = android_root / "android"
        host_inc = host_target / "phone-dev/incremental/jni-host"
        device_inc = device_target / "aarch64-linux-android/phone-dev/incremental/jni-dev"
        _write_bytes(host_inc / "s/f", 4096)
        _write_bytes(host_target / "phone-dev/deps/libjni.rlib", 4096)
        _write_bytes(device_inc / "s/f", 4096)
        _write_bytes(
            device_target / "aarch64-linux-android/phone-dev/deps/libjni.rlib",
            4096,
        )
        calls: list[tuple[list[str], str | None]] = []

        def fake_runner(cmd, cwd, env, check):
            calls.append((cmd, env.get("CARGO_TARGET_DIR")))
            return subprocess.CompletedProcess(cmd, 0)

        with contextlib.redirect_stdout(io.StringIO()):
            code = sweep.sweep(
                self.root,
                cold=True,
                which=lambda _: "/usr/local/bin/cargo-sweep",
                runner=fake_runner,
            )
        self.assertEqual(code, 0)
        self.assertFalse(host_inc.exists())
        self.assertFalse(device_inc.exists())
        self.assertEqual(
            [target_dir for _, target_dir in calls],
            [None, str(host_target), str(device_target)],
        )

    def test_check_target_silent_when_healthy_and_warns_when_over_threshold(self):
        healthy_disk = lambda _: SimpleNamespace(free=100 * sweep.GIB)
        self.assertEqual(
            sweep.check_target(self.target, ignore_ci=False, disk_usage=healthy_disk),
            [],
        )

        inc = self.target / "debug" / "incremental"
        inc.mkdir(parents=True)
        for i in range(5):
            (inc / f"crate-{i}").mkdir()

        deps = self.target / "debug" / "deps"
        deps.mkdir(parents=True)
        for i in range(6):
            (deps / f"f-{i}").write_text("")

        buf = io.StringIO()
        warned = sweep.emit_warning(
            self.target,
            stream=buf,
            warn_incremental_dirs=3,
            warn_deps_entries=4,
            warn_free_bytes=50 * sweep.GIB,
            ignore_ci=False,
            disk_usage=lambda _: SimpleNamespace(free=10 * sweep.GIB),
        )
        self.assertTrue(warned)
        text = buf.getvalue()
        self.assertIn("target/debug/incremental has 5 dirs", text)
        self.assertIn("target/debug/deps has 6 entries", text)
        self.assertIn("only 10.0 GiB free on disk", text)
        self.assertIn("just sweep", text)

    def test_check_target_skips_in_ci(self):
        inc = self.target / "debug" / "incremental"
        inc.mkdir(parents=True)
        for i in range(5):
            (inc / f"crate-{i}").mkdir()

        prev = os.environ.get("CI")
        os.environ["CI"] = "true"
        try:
            reasons = sweep.check_target(
                self.target,
                warn_incremental_dirs=1,
                disk_usage=lambda _: SimpleNamespace(free=1),
            )
            self.assertEqual(reasons, [])
        finally:
            if prev is None:
                os.environ.pop("CI", None)
            else:
                os.environ["CI"] = prev


if __name__ == "__main__":
    unittest.main()
