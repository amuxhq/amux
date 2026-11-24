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
    def test_build_capture_command_builds_the_capture_for_the_filters(self):
        cmd = storybook.build_capture_command(["fleet.busy", "hosts"])
        self.assertIn("//apps/android/app:storybook", cmd)
        self.assertIn("amux.android_storybook_filter=fleet.busy,hosts", cmd)

    def test_copy_capture_replaces_the_previous_pictures(self):
        with tempfile.TemporaryDirectory() as tmp:
            captured = Path(tmp) / "captured"
            (captured / "png").mkdir(parents=True)
            (captured / "png" / "fleet.busy.light.png").write_bytes(b"new")
            (captured / "manifest.json").write_text("{}")
            out_dir = Path(tmp) / "out"
            (out_dir / "png").mkdir(parents=True)
            (out_dir / "png" / "gone.light.png").write_bytes(b"old")
            storybook.copy_capture(captured, out_dir)
            self.assertEqual(sorted(p.name for p in (out_dir / "png").iterdir()), ["fleet.busy.light.png"])
            self.assertTrue((out_dir / "manifest.json").is_file())

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

    def test_area_tabs_order_chat_asks_review_between_fleet_and_hosts(self):
        stories = [
            {"id": "store.failure", "area": "System"},
            {"id": "report.ready", "area": "Report"},
            {"id": "settings.signed-in", "area": "Settings"},
            {"id": "pair.confirm", "area": "Pairing"},
            {"id": "review.page", "area": "Review"},
            {"id": "hosts.list", "area": "Hosts"},
            {"id": "ask.permission-command", "area": "Asks"},
            {"id": "chat.conversation", "area": "Chat"},
            {"id": "new-agent.claude", "area": "New Agent"},
            {"id": "fleet.busy", "area": "Fleet"},
        ]
        self.assertEqual(
            storybook.ordered_areas(stories),
            ["Fleet", "New Agent", "Chat", "Asks", "Review", "Hosts", "Pairing", "Settings", "Report", "System"],
        )

    def test_gallery_lists_geometry_flags_and_tolerates_missing_reports(self):
        with tempfile.TemporaryDirectory() as tmp:
            out_dir = Path(tmp)
            (out_dir / "png").mkdir()
            (out_dir / "geometry").mkdir()
            stories = []
            for sid in ("fleet.busy", "hosts.list"):
                for theme in ("light", "dark"):
                    (out_dir / "png" / f"{sid}.{theme}.png").write_bytes(b"\x89PNG\r\n\x1a\n")
                stories.append({
                    "id": sid,
                    "area": "Fleet",
                    "title": sid,
                    "description": "",
                    "fontScale": 1.0,
                    "lightPng": f"png/{sid}.light.png",
                    "darkPng": f"png/{sid}.dark.png",
                })
            stories[0]["lightGeometry"] = "geometry/fleet.busy.light.json"
            (out_dir / "geometry" / "fleet.busy.light.json").write_text(json.dumps({
                "id": "fleet.busy",
                "flags": [
                    {"rule": "tall-control", "offDp": 8.0, "detail": "\"Pair\" drawn 48.0dp tall", "elements": [3]},
                ],
                "elements": [],
            }), encoding="utf-8")
            (out_dir / "manifest.json").write_text(json.dumps({"stories": stories}), encoding="utf-8")

            index_path, _ = storybook.write_gallery(out_dir)
            html = index_path.read_text(encoding="utf-8")
            self.assertIn("1 alignment near-miss", html)
            self.assertIn("Control drawn taller than 44 dp", html)
            self.assertIn("&quot;Pair&quot; drawn 48.0dp tall", html)
            self.assertIn('href="#fleet.busy"', html)

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
