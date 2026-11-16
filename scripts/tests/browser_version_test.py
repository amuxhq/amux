"""The browser extension's release version: its form, and that it only rises."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parent.parent / "browser_version.py"
spec = importlib.util.spec_from_file_location("browser_version", SCRIPT)
browser_version = importlib.util.module_from_spec(spec)
spec.loader.exec_module(browser_version)


class BrowserVersionTest(unittest.TestCase):
    def test_chrome_versions_are_one_to_four_numbers(self):
        self.assertEqual(browser_version.parts("1"), (1,))
        self.assertEqual(browser_version.parts("1.2.3.4"), (1, 2, 3, 4))
        for wrong in ["1.x", "1.2.3.4.5", "v1.0", "1.70000", ""]:
            with self.assertRaises(SystemExit, msg=wrong):
                browser_version.parts(wrong)

    def test_the_version_only_goes_up(self):
        with tempfile.TemporaryDirectory() as folder:
            manifest = Path(folder) / "manifest.json"
            manifest.write_text('{\n  "name": "amux",\n  "version": "0.1.0"\n}\n')
            browser_version.MANIFEST = manifest
            for low in ["0.1.0", "0.0.9"]:
                with self.assertRaises(SystemExit):
                    browser_version.set_version(low)
            browser_version.set_version("0.2.0")
            self.assertEqual(json.loads(manifest.read_text())["version"], "0.2.0")


if __name__ == "__main__":
    unittest.main()
