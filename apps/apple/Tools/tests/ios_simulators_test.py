"""The pinned-simulator helpers that can be checked without a device."""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import ios_simulators  # noqa: E402


class AppsListed(unittest.TestCase):
    def test_reads_bundle_identifiers_from_launchd_labels(self):
        listing = (
            "81486\t0\tUIKitApplication:com.apple.mobilecal[6208][rb-legacy]\n"
            "32027\t0\tUIKitApplication:sh.amux.app[8d52][rb-legacy]\n"
            "1\t0\tcom.apple.SpringBoard\n"
        )
        self.assertEqual(
            ios_simulators.apps_listed(listing), ["com.apple.mobilecal", "sh.amux.app"]
        )

    def test_an_empty_listing_names_nothing(self):
        self.assertEqual(ios_simulators.apps_listed(""), [])


class Settle(unittest.TestCase):
    def test_a_slow_setting_is_asked_again_and_a_lasting_one_still_fails(self):
        from unittest.mock import patch
        import subprocess

        expired = subprocess.TimeoutExpired(["xcrun"], 120)
        with patch.object(ios_simulators, "run", side_effect=[expired, "applied"]) as run, \
                patch("builtins.print"):
            ios_simulators.settle("xcrun", "simctl", "ui", "udid", "appearance", "light")
        self.assertEqual(run.call_count, 2)
        with patch.object(ios_simulators, "run", side_effect=expired), \
                patch("builtins.print"), self.assertRaises(subprocess.TimeoutExpired):
            ios_simulators.settle("xcrun", "simctl", "ui", "udid", "appearance", "light")


class SeedRegion(unittest.TestCase):
    def test_a_new_device_is_given_the_pinned_region_beside_what_it_had(self):
        import plistlib
        import tempfile

        with tempfile.TemporaryDirectory() as data:
            path = Path(data) / "Library/Preferences/.GlobalPreferences.plist"
            path.parent.mkdir(parents=True)
            path.write_bytes(plistlib.dumps({"AppleLocale": "en_GB", "Other": 1}))
            ios_simulators.seed_region(Path(data))
            written = plistlib.loads(path.read_bytes())
        self.assertEqual(written, {
            "AppleLanguages": ["en-US"],
            "AppleLocale": "en_US",
            "AppleICUForce24HourTime": False,
            "Other": 1,
        })

    def test_the_seeded_values_are_what_pin_reads_back_as_settled(self):
        # `defaults read` prints a bool as 0 or 1 and an array's one value
        # bare; pin skips its second boot only if these agree.
        printed = {True: "1", False: "0"}
        for key, value in ios_simulators.region_values().items():
            shown = printed[value] if isinstance(value, bool) else (value[0] if isinstance(value, list) else value)
            self.assertEqual(shown, ios_simulators.REGION[key][2], key)


if __name__ == "__main__":
    unittest.main()
