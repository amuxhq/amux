"""Check the app's default icon has no alpha channel.

The App Store rejects an icon with one outright, and nothing in a simulator
build notices. The release scope audit checks the built bundle for the
other two rules validation enforces (a 120x120 iPhone icon and a top-level
`CFBundleIconName`); this one is read from the committed source image, which
`just logo` exports and the build never regenerates.
"""

from pathlib import Path
import struct
import unittest

ICON = Path(__file__).resolve().parents[2] / "apps/apple/Amux/Assets.xcassets/AppIcon.appiconset/AppIcon.png"


class TheDefaultIcon(unittest.TestCase):
    def test_it_has_no_alpha_channel(self):
        # A transparent pixel on a home screen has nothing to show through
        # to. IHDR is a plain PNG's first chunk; colour type 2 is RGB, where 6
        # would be RGBA and 4 grey with alpha.
        raw = ICON.read_bytes()
        self.assertEqual(b"\x89PNG\r\n\x1a\n", raw[:8], f"{ICON} is not a PNG")
        self.assertEqual(b"IHDR", raw[12:16], f"{ICON} does not start with IHDR")
        _, _, depth, colour = struct.unpack(">IIBB", raw[16:26])
        self.assertEqual(8, depth)
        self.assertEqual(2, colour, "the icon carries an alpha channel")


if __name__ == "__main__":
    unittest.main()
