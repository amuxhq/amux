"""Unit tests for apps/android/tools/feature-lint.sh."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO_ROOT = Path(__file__).resolve().parents[4]
SCRIPT = REPO_ROOT / "apps/android/tools/feature-lint.sh"


class FeatureLintTest(unittest.TestCase):
    def _run(self, sources_dir: Path) -> subprocess.CompletedProcess[str]:
        env = dict(os.environ)
        env["AMUX_ANDROID_FEATURES_SOURCES"] = str(sources_dir)
        return subprocess.run(
            ["sh", str(SCRIPT)],
            cwd=REPO_ROOT,
            env=env,
            capture_output=True,
            text=True,
        )

    def test_clean_composable_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "Home.kt").write_text(
                "package sh.amux.features\n"
                "import androidx.compose.material3.Text\n"
                "import androidx.compose.runtime.Composable\n"
                "@Composable fun Home() { Text(\"amux\") }\n",
                encoding="utf-8",
            )
            result = self._run(root)
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)

    def test_forbidden_imports_and_spinners_fail(self):
        cases = [
            ("ActivityImport.kt", "import android.app.Activity\nclass X\n"),
            ("NavigationImport.kt", "import androidx.navigation.NavHostController\nclass X\n"),
            ("OkHttpImport.kt", "import okhttp3.OkHttpClient\nclass X\n"),
            ("ToastImport.kt", "import android.widget.Toast\nclass X\n"),
            ("CircularSpinner.kt", "fun X() { CircularProgressIndicator() }\n"),
            ("LinearSpinner.kt", "fun X() { LinearProgressIndicator() }\n"),
        ]
        for filename, source in cases:
            with self.subTest(filename=filename), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                (root / filename).write_text(source, encoding="utf-8")
                result = self._run(root)
                self.assertNotEqual(result.returncode, 0, f"expected failure for {filename}")
                self.assertIn(filename, result.stdout + result.stderr)

    def test_comment_mentioning_spinner_does_not_fail(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "Notes.kt").write_text(
                "// Never use CircularProgressIndicator or LinearProgressIndicator here.\n"
                "fun Clean() {}\n",
                encoding="utf-8",
            )
            result = self._run(root)
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)


if __name__ == "__main__":
    unittest.main()
