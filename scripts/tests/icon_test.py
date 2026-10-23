"""Check the app has an icon the App Store will accept.

Apple rejects an upload whose bundle carries no icon — code 90022 for a
missing 120x120 image and 90713 for a missing `CFBundleIconName` — and
nothing in a simulator build notices, because the simulator shows an app
without an icon quite happily. The first time the gap appears is at
validation, after an archive, an export and a signing round trip. So it is
checked here instead, from the sources the icon is compiled out of, and again
in the release scope audit, from the bundle a build produces.

The artwork is exported from the logo's master by `just logo` and copied
in. The build never regenerates it, so these checks describe committed files
rather than a step that has to be re-run.
"""

import json
from pathlib import Path
import struct
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
CATALOG = ROOT / "apps/apple/Amux/Assets.xcassets"
ICON = CATALOG / "AppIcon.appiconset"
SPEC = ROOT / "apps/apple/project.yml"


def png_header(path: Path) -> tuple[int, int, int, int]:
    """A PNG's width, height, bit depth and colour type.

    Walk chunks as the release scope audit does: device PNGs carry Apple's
    CgBI chunk before IHDR, so the header has no fixed file offset."""
    raw = path.read_bytes()
    if raw[:8] != b"\x89PNG\r\n\x1a\n":
        raise AssertionError(f"{path} is not a PNG")
    at = 8
    while at + 8 <= len(raw):
        length, kind = struct.unpack(">I4s", raw[at:at + 8])
        if kind == b"IHDR":
            return struct.unpack(">IIBB", raw[at + 8:at + 18])
        at += 12 + length
    raise AssertionError(f"{path} has no IHDR chunk")


class PNGHeaders(unittest.TestCase):
    def test_plain_and_device_headers_preserve_dimensions_and_colour(self):
        def chunk(kind, body):
            return struct.pack(">I", len(body)) + kind + body + bytes(4)

        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "icon.png"
            for leading in (b"", chunk(b"CgBI", bytes(4))):
                for colour in (2, 6):
                    with self.subTest(device=bool(leading), colour=colour):
                        header = struct.pack(">IIBBBBB", 120, 180, 8, colour, 0, 0, 0)
                        path.write_bytes(b"\x89PNG\r\n\x1a\n" + leading + chunk(b"IHDR", header))
                        self.assertEqual((120, 180, 8, colour), png_header(path))

    def test_non_png_and_missing_header_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "icon.png"
            for raw, message in ((b"not a PNG", "is not a PNG"),
                                 (b"\x89PNG\r\n\x1a\n", "has no IHDR chunk")):
                with self.subTest(raw=raw):
                    path.write_bytes(raw)
                    with self.assertRaisesRegex(AssertionError, message):
                        png_header(path)


class TheIconSet(unittest.TestCase):
    def test_the_catalog_holds_an_app_icon(self):
        self.assertTrue((CATALOG / "Contents.json").is_file(),
                        f"{CATALOG} is not an asset catalog")
        self.assertTrue((ICON / "Contents.json").is_file(),
                        f"{ICON} is missing; the App Store rejects an app "
                        "with no icon")

    def test_every_named_image_is_there(self):
        listed = json.loads((ICON / "Contents.json").read_text())["images"]
        self.assertTrue(listed, "the icon set names no image at all")
        for image in listed:
            name = image.get("filename")
            self.assertIsNotNone(name, f"{image} names no file")
            self.assertTrue((ICON / name).is_file(),
                            f"{name} is named by the icon set but not here")

    def test_the_source_image_is_the_size_the_catalog_compiles_from(self):
        # One 1024x1024 image is the whole icon: the catalog compiler derives
        # the 120x120 the App Store asks for, and every other size, from it.
        width, height, _, _ = png_header(ICON / "AppIcon.png")
        self.assertEqual((1024, 1024), (width, height))

    def test_it_has_no_alpha_channel(self):
        # An icon with alpha is rejected outright, and a transparent pixel on
        # a home screen has nothing to show through to. Colour type 2 is RGB;
        # 6 would be RGBA and 4 grey with alpha.
        _, _, depth, colour = png_header(ICON / "AppIcon.png")
        self.assertEqual(8, depth)
        self.assertEqual(2, colour, "the icon carries an alpha channel")


class TheProject(unittest.TestCase):
    def test_the_catalog_is_compiled_into_the_app(self):
        spec = SPEC.read_text()
        self.assertIn("- path: Amux/Assets.xcassets", spec,
                      "the asset catalog is not in the app target's sources, "
                      "so nothing compiles the icon")

    def test_the_build_names_the_icon(self):
        # This setting is what makes the catalog compiler treat the set as
        # the app icon rather than as one more image.
        self.assertIn("ASSETCATALOG_COMPILER_APPICON_NAME: AppIcon",
                      SPEC.read_text())

    def test_the_bundle_carries_a_top_level_icon_name(self):
        # The App Store rejects an upload with code 90713 unless
        # CFBundleIconName sits at the top level of Info.plist. The catalog
        # compiler writes its own copy nested inside CFBundleIcons, which is
        # not the one Apple reads, so the key is declared outright.
        self.assertIn("CFBundleIconName: AppIcon", SPEC.read_text())


if __name__ == "__main__":
    unittest.main()
