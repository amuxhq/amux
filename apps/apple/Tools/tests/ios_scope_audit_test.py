"""The release audit must reject excluded scope without rejecting attention."""

import importlib
from pathlib import Path
import struct
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
audit = importlib.import_module("ios-scope-audit")


def png(path: Path, width: int, height: int, *, cgbi: bool = False) -> Path:
    """A PNG whose IHDR says the given size. No pixels: only the header is read.

    With `cgbi`, IHDR sits behind the proprietary chunk Apple's packaging puts
    at the front of every PNG in a device bundle — the shape the audit meets on
    an exported .ipa, where a fixed offset would read garbage."""
    def chunk(kind: bytes, body: bytes) -> bytes:
        return struct.pack(">I", len(body)) + kind + body + b"\0\0\0\0"
    header = struct.pack(">II", width, height) + bytes([8, 2, 0, 0, 0])
    lead = chunk(b"CgBI", bytes(4)) if cgbi else b""
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + lead + chunk(b"IHDR", header))
    return path


class TheAppIcon(unittest.TestCase):
    """The bundle must carry what the App Store refuses an upload without.

    Neither absence is visible in a simulator run, so the audit is the last
    place either can be caught before a validation server catches it."""

    def setUp(self):
        self.bundle = Path(tempfile.mkdtemp())
        png(self.bundle / "AppIcon60x60@2x.png", 120, 120)
        self.info = {"CFBundleIconName": "AppIcon"}

    def test_a_bundle_with_a_named_120_icon_passes(self):
        self.assertEqual(audit.icon_violations(self.info, self.bundle), [])

    def test_the_size_is_read_past_apples_own_leading_chunk(self):
        png(self.bundle / "AppIcon60x60@2x.png", 120, 120, cgbi=True)
        self.assertEqual(audit.icon_violations(self.info, self.bundle), [])
        png(self.bundle / "AppIcon60x60@2x.png", 180, 180, cgbi=True)
        self.assertIn("iPhone app icon is 180x180, not 120x120",
                      audit.icon_violations(self.info, self.bundle))

    def test_a_missing_top_level_icon_name_is_refused(self):
        # Code 90713. The catalog compiler's own copy, nested inside
        # CFBundleIcons, is not the one Apple reads.
        nested = {"CFBundleIcons": {"CFBundlePrimaryIcon": {"CFBundleIconName": "AppIcon"}}}
        self.assertIn("no top-level CFBundleIconName in Info.plist",
                      audit.icon_violations(nested, self.bundle))
        self.assertTrue(audit.icon_violations({"CFBundleIconName": ""}, self.bundle))

    def test_a_missing_or_wrongly_sized_iphone_icon_is_refused(self):
        # Code 90022: an app icon of exactly 120x120 is required.
        bare = Path(tempfile.mkdtemp())
        self.assertTrue(audit.icon_violations(self.info, bare))
        png(self.bundle / "AppIcon60x60@2x.png", 180, 180)
        self.assertIn("iPhone app icon is 180x180, not 120x120",
                      audit.icon_violations(self.info, self.bundle))


def shipping_info(**extra) -> dict:
    """An Info.plist the audit accepts, for tests about something else.

    Every key here is one the audit requires of a shipping bundle, so a test
    checking the device family does not also have to restate the local-network
    declaration."""
    return {"UIDeviceFamily": [1], "CFBundleIconName": "AppIcon",
            "NSBonjourServices": ["_amux._udp"],
            "NSLocalNetworkUsageDescription": audit.LOCAL_NETWORK_PURPOSE} | extra


class ScopeAuditTests(unittest.TestCase):
    def test_excluded_code_is_rejected(self):
        for name in (*audit.DEBUG_SYMBOLS, *audit.FORBIDDEN_APIS):
            with self.subTest(symbol=name):
                self.assertTrue(audit.binary_violations("symbol_" + name, ""))
        self.assertEqual(audit.binary_violations("NotificationCenter", "Contact Support\n3 need you"), [])

    def test_reporting_ships_and_its_absence_is_refused(self):
        symbols = "\n".join(audit.REPORT_SYMBOLS)
        strings = "\n".join(audit.REPORT_COPY)
        self.assertEqual(audit.reporting_violations(symbols, strings), [])
        # Shipping is not a debug surface: none of it trips the exclusions.
        self.assertEqual(audit.binary_violations(symbols, strings), [])
        for name in audit.REPORT_SYMBOLS:
            with self.subTest(symbol=name):
                self.assertIn(f"reporting is absent: {name}", audit.reporting_violations(
                    symbols.replace(name, ""), strings))
        for copy in audit.REPORT_COPY:
            with self.subTest(copy=copy):
                self.assertIn(f"reporting copy is absent: {copy}", audit.reporting_violations(
                    symbols, strings.replace(copy, "")))

    def test_the_legacy_bonjour_browsers_stay_refused(self):
        # Both reach the same multicast as Network.framework without the
        # system's permission prompt in front of them.
        for legacy in ("DNSServiceBrowse", "NSNetServiceBrowser"):
            with self.subTest(api=legacy):
                self.assertTrue(audit.binary_violations("_" + legacy, ""))

    def test_the_network_framework_browser_is_accepted(self):
        # The app browses the local network, so its browser is the one API in
        # the binary that says so. Refusing it would refuse the feature.
        for allowed in ("NWBrowser", "_nw_browser_create", "NWBrowserResult"):
            with self.subTest(api=allowed):
                self.assertEqual(audit.binary_violations(allowed, ""), [])

    def test_entitlements_device_family_and_destinations(self):
        bundle = Path(tempfile.mkdtemp())
        png(bundle / "AppIcon60x60@2x.png", 120, 120)
        info = shipping_info()
        settings = {"TARGETED_DEVICE_FAMILY": "1", "SUPPORTED_PLATFORMS": "iphoneos iphonesimulator",
                    "SUPPORTS_MACCATALYST": "NO", "SUPPORTS_MAC_DESIGNED_FOR_IPHONE_IPAD": "NO",
                    "SUPPORTS_XR_DESIGNED_FOR_IPHONE_IPAD": "NO"}
        self.assertEqual(audit.bundle_violations(info, {}, settings, bundle), [])
        self.assertTrue(audit.bundle_violations(info, {"aps-environment": "development"}, settings, bundle))
        self.assertTrue(audit.bundle_violations(info | {"UIDeviceFamily": [1, 2]}, {}, settings, bundle))
        for flag in ("SUPPORTS_MACCATALYST", "SUPPORTS_MAC_DESIGNED_FOR_IPHONE_IPAD",
                     "SUPPORTS_XR_DESIGNED_FOR_IPHONE_IPAD"):
            with self.subTest(flag=flag):
                self.assertTrue(audit.bundle_violations(info, {}, settings | {flag: "YES"}, bundle))
        self.assertTrue(audit.bundle_violations(info, {}, settings | {"SUPPORTED_PLATFORMS": "macosx"}, bundle))

    def test_debug_only_resources_in_the_bundle_are_rejected(self):
        # Symbols catch the code that reads these files; nothing else catches
        # the files themselves arriving through a copy phase or a package that
        # became a dependency of the app.
        bundle = Path(tempfile.mkdtemp())
        png(bundle / "AppIcon60x60@2x.png", 120, 120)
        (bundle / "AmuxDesign_AmuxDesign.bundle").mkdir()
        (bundle / "en.lproj").mkdir()
        self.assertEqual(audit.resource_violations(bundle), [])

        frozen = bundle / "AmuxDesign_AmuxDesign.bundle" / "frozen-frame.png"
        png(frozen, 10, 10)
        self.assertIn("excluded resource in the bundle: "
                      "AmuxDesign_AmuxDesign.bundle/frozen-frame.png",
                      audit.resource_violations(bundle))
        frozen.unlink()

        (bundle / "AmuxTestSupport_AmuxTestSupport.bundle").mkdir()
        self.assertIn("excluded resource in the bundle: "
                      "AmuxTestSupport_AmuxTestSupport.bundle",
                      audit.resource_violations(bundle))

    def test_the_whole_bundle_check_carries_the_resource_verdict(self):
        bundle = Path(tempfile.mkdtemp())
        png(bundle / "AppIcon60x60@2x.png", 120, 120)
        info = shipping_info()
        settings = {"TARGETED_DEVICE_FAMILY": "1",
                    "SUPPORTED_PLATFORMS": "iphoneos iphonesimulator",
                    "SUPPORTS_MACCATALYST": "NO",
                    "SUPPORTS_MAC_DESIGNED_FOR_IPHONE_IPAD": "NO",
                    "SUPPORTS_XR_DESIGNED_FOR_IPHONE_IPAD": "NO"}
        self.assertEqual(audit.bundle_violations(info, {}, settings, bundle), [])
        png(bundle / "frozen-frame.png", 10, 10)
        self.assertIn("excluded resource in the bundle: frozen-frame.png",
                      audit.bundle_violations(info, {}, settings, bundle))

    def test_transitive_cloud_and_legacy_dependencies_are_rejected(self):
        clean = {"name": "AmuxTestSupport", "dependencies": [{"name": "AmuxCore"}]}
        self.assertEqual(audit.graph_violations([clean]), [])
        for name in ("amuxcloud", "ReactNative", "react-native"):
            graph = {"name": "Amux", "dependencies": [{"name": "local", "dependencies": [{"name": name}]}]}
            with self.subTest(package=name):
                self.assertTrue(audit.graph_violations([graph]))


class TheLocalNetworkDeclaration(unittest.TestCase):
    """Bonjour is declared for amux's own service and explained, or refused."""

    def setUp(self):
        self.bundle = Path(tempfile.mkdtemp())
        png(self.bundle / "AppIcon60x60@2x.png", 120, 120)
        self.info = shipping_info()
        self.settings = {"TARGETED_DEVICE_FAMILY": "1",
                         "SUPPORTED_PLATFORMS": "iphoneos iphonesimulator",
                         "SUPPORTS_MACCATALYST": "NO",
                         "SUPPORTS_MAC_DESIGNED_FOR_IPHONE_IPAD": "NO",
                         "SUPPORTS_XR_DESIGNED_FOR_IPHONE_IPAD": "NO"}

    def violations(self, info):
        return audit.bundle_violations(info, {}, self.settings, self.bundle)

    def test_a_bundle_that_declares_nothing_cannot_browse_and_is_refused(self):
        # iOS answers an undeclared browser with nothing, so the app would find
        # no machines on a network full of them and look merely empty.
        bare = dict(self.info)
        del bare["NSBonjourServices"]
        self.assertTrue(self.violations(bare))

    def test_amuxs_own_service_with_the_agreed_sentence_passes(self):
        self.assertEqual(self.violations(self.info), [])

    def test_a_second_service_is_refused(self):
        # A declaration is the scope: a service listed here is one this app may
        # look for on somebody's network whether or not it ever does.
        self.assertTrue(self.violations(
            self.info | {"NSBonjourServices": ["_amux._udp", "_ssh._tcp"]}))

    def test_browsing_without_the_agreed_explanation_is_refused(self):
        # The sentence is what a person reads before answering; an absent or
        # reworded one is a different question being asked.
        without = dict(self.info)
        del without["NSLocalNetworkUsageDescription"]
        self.assertTrue(self.violations(without))
        self.assertTrue(self.violations(
            self.info | {"NSLocalNetworkUsageDescription": "amux needs your network."}))


if __name__ == "__main__":
    unittest.main()
