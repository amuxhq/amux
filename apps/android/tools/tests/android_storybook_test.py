"""Unit tests for apps/android/tools/android-storybook.py."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

TOOLS_DIR = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "android_storybook",
    TOOLS_DIR / "android-storybook.py",
)
storybook = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(storybook)


class AndroidStorybookTest(unittest.TestCase):
    def test_build_gradle_command_includes_filter_and_no_daemon(self):
        out_dir = Path("/tmp/amux-storybook-test")
        cmd = storybook.build_gradle_command(
            out_dir=out_dir,
            filters=["fleet.busy", "hosts"],
            no_daemon=True,
        )
        self.assertIn(":app:testDebugUnitTest", cmd)
        self.assertIn("-Damux.storybook.capture=true", cmd)
        self.assertIn("-Damux.storybook.filter=fleet.busy,hosts", cmd)
        self.assertIn("--no-daemon", cmd)

    def test_write_gallery_validates_pngs_and_generates_html(self):
        with tempfile.TemporaryDirectory() as tmp:
            out_dir = Path(tmp)
            png_dir = out_dir / "png"
            png_dir.mkdir()
            (png_dir / "fleet.busy.light.png").write_bytes(b"\x89PNG\r\n\x1a\nlight")
            (png_dir / "fleet.busy.dark.png").write_bytes(b"\x89PNG\r\n\x1a\ndark")
            (png_dir / "store.failure.light.png").write_bytes(b"\x89PNG\r\n\x1a\nlight")
            (png_dir / "store.failure.dark.png").write_bytes(b"\x89PNG\r\n\x1a\ndark")
            manifest = {
                "stories": [
                    {
                        "id": "fleet.busy",
                        "area": "Fleet",
                        "title": "Busy fleet",
                        "description": "Agents across machines.",
                        "fontScale": 1.0,
                        "lightPng": "png/fleet.busy.light.png",
                        "darkPng": "png/fleet.busy.dark.png",
                    },
                    {
                        "id": "store.failure",
                        "area": "System",
                        "title": "Store unavailable",
                        "description": "Recovery screen.",
                        "fontScale": 1.5,
                        "lightPng": "png/store.failure.light.png",
                        "darkPng": "png/store.failure.dark.png",
                    },
                ]
            }
            (out_dir / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")

            index_path, stories = storybook.write_gallery(out_dir)
            self.assertEqual(len(stories), 2)
            html = index_path.read_text(encoding="utf-8")
            self.assertIn("fleet.busy", html)
            self.assertIn("store.failure", html)
            self.assertIn('data-area="Fleet"', html)
            self.assertIn('data-area="System"', html)
            self.assertIn("1.5x text", html)

    def test_load_manifest_fails_on_missing_png(self):
        with tempfile.TemporaryDirectory() as tmp:
            out_dir = Path(tmp)
            manifest = {
                "stories": [
                    {
                        "id": "fleet.busy",
                        "area": "Fleet",
                        "title": "Busy",
                        "description": "",
                        "fontScale": 1.0,
                        "lightPng": "png/missing.light.png",
                        "darkPng": "png/missing.dark.png",
                    }
                ]
            }
            (out_dir / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            with self.assertRaises(ValueError):
                storybook.load_manifest(out_dir)


if __name__ == "__main__":
    unittest.main()
