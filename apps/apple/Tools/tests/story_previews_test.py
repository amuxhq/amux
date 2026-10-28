"""Every story the storybook list gives the phone has its Xcode preview.

The component snapshot suite holds the catalogue to the list; this holds the
previews to it, so the canvas a person browses shows the same stories.
"""

import json
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[4]
STORIES = ROOT / "tests/stories.json"
PREVIEWS = ROOT / "apps/apple/Amux/Debug"


class StoryPreviewTests(unittest.TestCase):
    def test_each_phone_story_has_a_preview_and_no_preview_names_another(self):
        stories = [
            story["name"]
            for story in json.loads(STORIES.read_text())["stories"]
            if "phone" in story["clients"]
        ]
        previewed = [
            name
            for path in sorted(PREVIEWS.glob("*Previews.swift"))
            for name in re.findall(r'StoryScreen\("([^"]+)"\)', path.read_text())
        ]
        self.assertEqual(sorted(set(stories) - set(previewed)), [], "stories without a preview")
        self.assertEqual(sorted(set(previewed) - set(stories)), [], "previews of no story")


if __name__ == "__main__":
    unittest.main()
