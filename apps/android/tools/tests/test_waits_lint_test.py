"""Unit tests for apps/android/tools/test-waits-lint.sh."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO_ROOT = Path(__file__).resolve().parents[4]
SCRIPT = REPO_ROOT / "apps/android/tools/test-waits-lint.sh"


class TestWaitsLintTest(unittest.TestCase):
    def _run(self, test_dir: Path) -> subprocess.CompletedProcess[str]:
        env = dict(os.environ)
        env["AMUX_ANDROID_TEST_ROOTS"] = str(test_dir)
        return subprocess.run(
            ["sh", str(SCRIPT)],
            cwd=REPO_ROOT,
            env=env,
            capture_output=True,
            text=True,
        )

    def test_clean_and_exempted_tests_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "GoodTest.kt").write_text(
                "import kotlinx.coroutines.delay\n"
                "suspend fun good() {\n"
                "    delay(100) // test-wait-exempt: advances virtual scheduler\n"
                "    customDelay(100)\n"
                "}\n",
                encoding="utf-8",
            )
            result = self._run(root)
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)

    def test_thread_sleep_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "SleepTest.kt").write_text(
                "fun bad() { Thread.sleep(50) }\n",
                encoding="utf-8",
            )
            result = self._run(root)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("SleepTest.kt", result.stdout + result.stderr)

    def test_bare_delay_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "DelayTest.kt").write_text(
                "suspend fun bad() { delay(50) }\n",
                encoding="utf-8",
            )
            result = self._run(root)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("DelayTest.kt", result.stdout + result.stderr)

    def test_delay_with_empty_exemption_reason_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "EmptyReasonTest.kt").write_text(
                "suspend fun bad() {\n"
                "    delay(50) // test-wait-exempt:   \n"
                "}\n",
                encoding="utf-8",
            )
            result = self._run(root)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("EmptyReasonTest.kt", result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
