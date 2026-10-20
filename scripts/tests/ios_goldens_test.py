"""The whole-screen golden driver: what it photographs and under which names."""

import importlib.util
import json
from pathlib import Path
import sys
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
spec = importlib.util.spec_from_file_location("ios_goldens", SCRIPTS / "ios-goldens.py")
goldens = importlib.util.module_from_spec(spec)
spec.loader.exec_module(goldens)
from journeys import phone  # noqa: E402

MANIFEST = {
    "screens": [
        {"id": "fleet", "shows": "the fleet"},
        {"id": "origin-rewind", "shows": "a swap", "frames": [
            {"name": "before", "shows": "before"}, {"name": "after", "shows": "after"},
        ]},
    ],
}


class Camera:
    """Stands in for the phone: records what it was asked to wear and compare."""

    def __init__(self, differs=None):
        self.calls = []
        self.differs = differs

    def appearance(self, appearance):
        self.calls.append(("wear", appearance))

    def compare(self, label, volatile=(), geometry_label=None):
        self.calls.append(("compare", label, geometry_label))
        return self.differs

    def app(self, request):
        self.calls.append(("door", request["kind"]))
        return {}

    def steady_display(self, png):
        self.calls.append(("capture", png.name))


class PhotographTests(unittest.TestCase):
    def test_the_driver_reaches_every_screen_the_committed_manifest_names(self):
        manifest = json.loads(goldens.MANIFEST.read_text())
        self.assertEqual(sorted(screen["id"] for screen in manifest["screens"]), sorted(goldens.WAY))
        self.assertTrue((goldens.ROOT / manifest["topology"]).is_file())

    def test_a_screen_is_compared_in_light_then_dark_with_one_geometry(self):
        camera = Camera()
        run = goldens.Goldens(camera, MANIFEST, {"fleet"})
        run.photograph("fleet")
        self.assertEqual(camera.calls, [
            ("wear", "light"), ("compare", "fleet.light", "fleet"),
            ("wear", "dark"), ("compare", "fleet.dark", "fleet"),
            ("wear", "light"),
        ])
        self.assertEqual(run.outcomes, [("fleet.light", None), ("fleet.dark", None)])

    def test_a_frame_is_named_after_its_screen(self):
        camera = Camera(differs="geometry differs")
        run = goldens.Goldens(camera, MANIFEST, {"origin-rewind"})
        run.photograph("origin-rewind", "after")
        self.assertIn(("compare", "origin-rewind-after.dark", "origin-rewind-after"), camera.calls)
        self.assertEqual([name for name, _ in run.outcomes], ["origin-rewind-after.light", "origin-rewind-after.dark"])

    def test_a_frame_the_manifest_does_not_declare_is_refused(self):
        run = goldens.Goldens(Camera(), MANIFEST, {"origin-rewind", "fleet"})
        with self.assertRaises(RuntimeError):
            run.photograph("origin-rewind", "during")
        with self.assertRaises(RuntimeError):
            run.photograph("origin-rewind")
        with self.assertRaises(RuntimeError):
            run.photograph("fleet", "before")

    def test_a_review_writes_each_appearance_and_compares_nothing(self):
        camera = Camera(differs="would differ")
        run = goldens.Goldens(camera, MANIFEST, {"fleet"}, review=Path("/tmp/review"))
        original, goldens.GLASS_FINISHES = goldens.GLASS_FINISHES, 0
        try:
            run.photograph("fleet")
        finally:
            goldens.GLASS_FINISHES = original
        self.assertEqual(camera.calls, [
            ("wear", "light"), ("door", "settle"), ("capture", "fleet.light.png"),
            ("wear", "dark"), ("door", "settle"), ("capture", "fleet.dark.png"),
            ("wear", "light"),
        ])
        self.assertEqual(run.outcomes, [("fleet.light", None), ("fleet.dark", None)])

    def test_a_screen_not_asked_for_is_passed_without_a_photograph(self):
        camera = Camera()
        run = goldens.Goldens(camera, MANIFEST, {"origin-rewind"})
        run.photograph("fleet")
        self.assertEqual(camera.calls, [])
        self.assertEqual(run.outcomes, [])


class CoveredTests(unittest.TestCase):
    def element(self, identifier):
        return {"identifier": identifier, "frame": {"x": 0, "y": 0, "width": 1, "height": 1}}

    def test_a_pushed_page_hides_the_tab_roots_beneath_it(self):
        drawn = [self.element(name) for name in (
            "shell", "home", "home.row.a", "hosts", "hosts.row.b", "you", "you.name",
            "chat", "chat.back", "tab.agents", "",
        )]
        kept = [element["identifier"] for element in phone.uncovered(drawn)]
        self.assertEqual(kept, ["shell", "chat", "chat.back", "tab.agents", ""])

    def test_a_tab_on_its_own_keeps_everything(self):
        drawn = [self.element(name) for name in ("shell", "home", "home.row.a", "tab.agents")]
        self.assertEqual(phone.uncovered(drawn), drawn)


if __name__ == "__main__":
    unittest.main()
