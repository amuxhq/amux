"""The bridge build: one target directory per triple, no SDK in the recipe
environment, and no cargo at all when nothing Rust changed."""

import importlib.util
import os
from pathlib import Path
import tempfile
from unittest import mock
import unittest


ROOT = Path(__file__).resolve().parents[2]


def load(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


import sys

bridge = load("ios_bridge", "ios_bridge.py")
# The build script imports the bridge module by name; hand it this instance so
# patching one patches what the script runs.
sys.modules["ios_bridge"] = bridge
ios_rust = load("ios_rust", "ios-rust.py")
ios_package = load("ios_package", "ios-package.py")


class BridgeEnvironmentTests(unittest.TestCase):
    def test_each_triple_owns_one_cargo_target_directory(self):
        simulator = bridge.build_environment(bridge.SIMULATOR_TRIPLE)
        device = bridge.build_environment(bridge.DEVICE_TRIPLE)
        self.assertNotEqual(simulator["CARGO_TARGET_DIR"], device["CARGO_TARGET_DIR"])
        for environment in (simulator, device):
            self.assertEqual(Path(environment["CARGO_TARGET_DIR"]).parent,
                             bridge.RUST_TARGETS.resolve())
            self.assertEqual(environment["RUSTC_WRAPPER"], "")
            self.assertEqual(environment["IPHONEOS_DEPLOYMENT_TARGET"], bridge.DEPLOYMENT_TARGET)

    def test_the_sdk_root_is_never_exported(self):
        with mock.patch.dict(os.environ, {"SDKROOT": "/sdk"}):
            self.assertNotIn("SDKROOT", bridge.build_environment(bridge.SIMULATOR_TRIPLE))

    def test_source_fingerprint_follows_rust_inputs_only(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "crates/a/src").mkdir(parents=True)
            (root / "crates/a/src/lib.rs").write_text("fn a() {}")
            (root / "Cargo.toml").write_text("[workspace]")
            (root / "apps/apple").mkdir(parents=True)
            (root / "apps/apple/View.swift").write_text("struct View {}")
            before = bridge.source_fingerprint(root)
            (root / "apps/apple/View.swift").write_text("struct View { var x = 1 }")
            self.assertEqual(bridge.source_fingerprint(root), before)
            (root / "crates/a/src/lib.rs").write_text("fn a() { let _ = 1; }")
            self.assertNotEqual(bridge.source_fingerprint(root), before)

    def test_framework_is_repackaged_only_when_its_inputs_change(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            staged = root / "slice"
            (staged / "include").mkdir(parents=True)
            (staged / bridge.LIBRARY).write_bytes(b"archive")
            (staged / "include" / bridge.HEADER).write_text("int f(void);")
            framework = root / bridge.FRAMEWORK
            stamp = root / "framework.sha256"
            with mock.patch.object(bridge, "package", side_effect=lambda f, s: f.mkdir(exist_ok=True)) as assemble:
                self.assertTrue(bridge.package_if_changed(framework, [staged], stamp))
                self.assertFalse(bridge.package_if_changed(framework, [staged], stamp))
                (staged / bridge.LIBRARY).write_bytes(b"archive v2")
                self.assertTrue(bridge.package_if_changed(framework, [staged], stamp))
            self.assertEqual(assemble.call_count, 2)


class DevelopmentBuildTests(unittest.TestCase):
    def test_unchanged_sources_with_both_frameworks_present_run_no_cargo(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "ios"
            driving = output / bridge.DRIVING_FRAMEWORK / bridge.DRIVING_SLICE
            driving.mkdir(parents=True)
            (driving / bridge.LIBRARY).write_bytes(b"a")
            shipping = output / bridge.FRAMEWORK / bridge.DRIVING_SLICE
            shipping.mkdir(parents=True)
            (shipping / bridge.LIBRARY).write_bytes(b"a")
            bridge.stand_in_marker(output / bridge.FRAMEWORK).write_text("same:phone-dev:debug-tools\n")
            stamp = output / "rust-stamp.json"
            stamp.write_text("same:phone-dev:debug-tools\n")
            with mock.patch.object(bridge, "OUTPUT", output), \
                    mock.patch.object(ios_rust, "STAMP", stamp), \
                    mock.patch.object(bridge, "source_fingerprint", return_value="same"), \
                    mock.patch.object(bridge, "cargo_build") as cargo:
                ios_rust.main()
            cargo.assert_not_called()

    def test_a_simulator_build_after_a_phone_build_runs_no_cargo(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "ios"
            for framework in (bridge.DRIVING_FRAMEWORK, bridge.FRAMEWORK):
                for name in (bridge.DRIVING_SLICE, bridge.DRIVING_DEVICE_SLICE):
                    (output / framework / name).mkdir(parents=True)
                    (output / framework / name / bridge.LIBRARY).write_bytes(b"a")
            phone = f"same:phone-dev:debug-tools:{bridge.DEVICE_TRIPLE}\n"
            bridge.stand_in_marker(output / bridge.FRAMEWORK).write_text(phone)
            stamp = output / "rust-stamp.json"
            stamp.write_text(phone)
            with mock.patch.object(bridge, "OUTPUT", output), \
                    mock.patch.object(ios_rust, "STAMP", stamp), \
                    mock.patch.object(bridge, "source_fingerprint", return_value="same"), \
                    mock.patch.object(bridge, "cargo_build") as cargo, \
                    mock.patch("builtins.print"):
                ios_rust.main()
            cargo.assert_not_called()

    def test_a_phone_build_puts_its_slice_in_both_frameworks(self):
        """The app force-loads the driving framework's phone slice, and the
        Swift packages resolve the shipping framework for the phone too, so a
        build for a phone that left either without it would fail at link."""
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "ios"
            output.mkdir()
            stamp = output / "rust-stamp.json"
            stamp.write_text("same\n")

            def build(triple, **_):
                built = bridge.Slice(triple, output / f"{triple}.a", output / f"{triple}.h")
                built.library.write_bytes(triple.encode())
                built.header.write_text("h")
                return built

            def package(framework, slices):
                for staged in slices:
                    name = (bridge.DRIVING_DEVICE_SLICE if staged.name == bridge.DEVICE_TRIPLE
                            else bridge.DRIVING_SLICE)
                    (framework / name).mkdir(parents=True, exist_ok=True)
                    (framework / name / bridge.LIBRARY).write_bytes(b"a")

            with mock.patch.object(bridge, "OUTPUT", output), \
                    mock.patch.object(ios_rust, "STAMP", stamp), \
                    mock.patch.object(bridge, "SIZE_REPORT", output / "size.txt"), \
                    mock.patch.object(bridge, "source_fingerprint", return_value="same"), \
                    mock.patch.object(bridge, "cargo_build", side_effect=build) as cargo, \
                    mock.patch.object(bridge, "package", side_effect=package) as packaged, \
                    mock.patch.object(ios_rust.Path, "read_text", return_value="[profile.dev]\ndebug = 1\n"), \
                    mock.patch("builtins.print"):
                ios_rust.main(device=True)
            self.assertEqual([call.args[0] for call in cargo.call_args_list],
                             [bridge.SIMULATOR_TRIPLE, bridge.DEVICE_TRIPLE])
            self.assertEqual(
                {call.args[0].name: sorted(path.name for path in call.args[1])
                 for call in packaged.call_args_list},
                {name: sorted([bridge.SIMULATOR_TRIPLE, bridge.DEVICE_TRIPLE])
                 for name in (bridge.DRIVING_FRAMEWORK, bridge.FRAMEWORK)})

    def test_a_shipping_framework_holding_no_library_is_rebuilt(self):
        """A restored build cache can leave an xcframework's directories behind
        without the archives inside them. The shape alone must not count as
        current, or xcodebuild reports a missing binary artifact two stages
        later and names neither the cache nor the bridge."""
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "ios"
            driving = output / bridge.DRIVING_FRAMEWORK / bridge.DRIVING_SLICE
            driving.mkdir(parents=True)
            (driving / bridge.LIBRARY).write_bytes(b"a")
            (output / bridge.FRAMEWORK / bridge.DRIVING_SLICE).mkdir(parents=True)
            stamp = output / "rust-stamp.json"
            stamp.write_text("same\n")
            built = bridge.Slice("t", output / "lib.a", output / "h.h")
            built.library.write_bytes(b"a")
            built.header.write_text("h")
            with mock.patch.object(bridge, "OUTPUT", output), \
                    mock.patch.object(ios_rust, "STAMP", stamp), \
                    mock.patch.object(bridge, "SIZE_REPORT", output / "size.txt"), \
                    mock.patch.object(bridge, "source_fingerprint", return_value="same"), \
                    mock.patch.object(bridge, "cargo_build", return_value=built) as cargo, \
                    mock.patch.object(bridge, "package") as package, \
                    mock.patch.object(ios_rust.Path, "read_text", return_value="[profile.dev]\ndebug = 1\n"), \
                    mock.patch("builtins.print"):
                ios_rust.main()
            cargo.assert_called_once()
            self.assertEqual(package.call_args.args[0], output / bridge.FRAMEWORK)

    def test_a_missing_shipping_framework_is_stood_in_for_by_the_development_slice(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "ios"
            output.mkdir()
            stamp = output / "rust-stamp.json"
            built = bridge.Slice("t", output / "lib.a", output / "h.h")
            built.library.write_bytes(b"a")
            built.header.write_text("h")
            packaged = []

            def package(framework, slices):
                framework.mkdir(parents=True)
                packaged.append(framework.name)
                if framework.name == bridge.DRIVING_FRAMEWORK:
                    (framework / bridge.DRIVING_SLICE).mkdir()
                    (framework / bridge.DRIVING_SLICE / bridge.LIBRARY).write_bytes(b"a")

            with mock.patch.object(bridge, "OUTPUT", output), \
                    mock.patch.object(ios_rust, "STAMP", stamp), \
                    mock.patch.object(bridge, "SIZE_REPORT", output / "size.txt"), \
                    mock.patch.object(bridge, "source_fingerprint", return_value="v1"), \
                    mock.patch.object(bridge, "cargo_build", return_value=built), \
                    mock.patch.object(bridge, "package", side_effect=package), \
                    mock.patch.object(
                        ios_rust.Path, "read_text",
                        return_value="[profile.release]\npanic = 'abort'\n"), \
                    mock.patch("builtins.print"):
                ios_rust.main()
            self.assertEqual(packaged, [bridge.DRIVING_FRAMEWORK, bridge.FRAMEWORK])
            self.assertEqual(stamp.read_text().strip(), "v1:phone-dev:debug-tools")
            self.assertEqual(
                bridge.stand_in_marker(output / bridge.FRAMEWORK).read_text().strip(),
                "v1:phone-dev:debug-tools",
            )
            self.assertIn("phone-dev, debug tools", (output / "size.txt").read_text())

    def test_a_changed_bridge_restages_a_shipping_stand_in(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "ios"
            output.mkdir()
            shipping = output / bridge.FRAMEWORK
            shipping_slice = shipping / bridge.DRIVING_SLICE
            shipping_slice.mkdir(parents=True)
            (shipping_slice / bridge.LIBRARY).write_bytes(b"old")
            marker = bridge.stand_in_marker(shipping)
            marker.write_text("old:phone-dev:debug-tools\n")
            stamp = output / "rust-stamp.json"
            stamp.write_text("old:phone-dev:debug-tools\n")
            built = bridge.Slice("t", output / "lib.a", output / "h.h")
            built.library.write_bytes(b"new")
            built.header.write_text("void new_symbol(void);")
            packaged = []

            def package(framework, slices):
                header = (slices[0] / "include" / bridge.HEADER).read_text()
                packaged.append((framework.name, header))
                if framework.name == bridge.DRIVING_FRAMEWORK:
                    (framework / bridge.DRIVING_SLICE).mkdir(parents=True, exist_ok=True)
                    (framework / bridge.DRIVING_SLICE / bridge.LIBRARY).write_bytes(b"new")

            with mock.patch.object(bridge, "OUTPUT", output), \
                    mock.patch.object(ios_rust, "STAMP", stamp), \
                    mock.patch.object(bridge, "SIZE_REPORT", output / "size.txt"), \
                    mock.patch.object(bridge, "source_fingerprint", return_value="new"), \
                    mock.patch.object(bridge, "cargo_build", return_value=built), \
                    mock.patch.object(bridge, "package", side_effect=package), \
                    mock.patch("builtins.print"):
                ios_rust.main()

            self.assertEqual(
                packaged,
                [
                    (bridge.DRIVING_FRAMEWORK, "void new_symbol(void);"),
                    (bridge.FRAMEWORK, "void new_symbol(void);"),
                ],
            )
            self.assertEqual(marker.read_text().strip(), "new:phone-dev:debug-tools")

    def test_a_changed_bridge_replaces_older_shipping_rust_for_package_tests(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "ios"
            output.mkdir()
            shipping = output / bridge.FRAMEWORK
            shipping_slice = shipping / bridge.DRIVING_SLICE
            shipping_slice.mkdir(parents=True)
            library = shipping_slice / bridge.LIBRARY
            library.write_bytes(b"shipping")
            stamp = output / "rust-stamp.json"
            stamp.write_text("old:phone-dev:debug-tools\n")
            built = bridge.Slice("t", output / "lib.a", output / "h.h")
            built.library.write_bytes(b"new")
            built.header.write_text("void new_symbol(void);")
            packaged = []

            def package(framework, _slices):
                packaged.append(framework.name)
                (framework / bridge.DRIVING_SLICE).mkdir(parents=True, exist_ok=True)
                (framework / bridge.DRIVING_SLICE / bridge.LIBRARY).write_bytes(b"new")

            with mock.patch.object(bridge, "OUTPUT", output), \
                    mock.patch.object(ios_rust, "STAMP", stamp), \
                    mock.patch.object(bridge, "SIZE_REPORT", output / "size.txt"), \
                    mock.patch.object(bridge, "source_fingerprint", return_value="new"), \
                    mock.patch.object(bridge, "cargo_build", return_value=built), \
                    mock.patch.object(bridge, "package", side_effect=package), \
                    mock.patch("builtins.print"):
                ios_rust.main()

            self.assertEqual(packaged, [bridge.DRIVING_FRAMEWORK, bridge.FRAMEWORK])
            self.assertEqual(library.read_bytes(), b"new")
            self.assertEqual(bridge.stand_in_marker(shipping).read_text().strip(),
                             "new:phone-dev:debug-tools")


    def test_a_shipping_build_in_the_stand_in_place_is_replaced_although_the_stamp_is_current(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "ios"
            output.mkdir()
            driving = output / bridge.DRIVING_FRAMEWORK
            (driving / bridge.DRIVING_SLICE).mkdir(parents=True)
            (driving / bridge.DRIVING_SLICE / bridge.LIBRARY).write_bytes(b"dev")
            shipping = output / bridge.FRAMEWORK
            shipping_slice = shipping / bridge.DRIVING_SLICE
            shipping_slice.mkdir(parents=True)
            library = shipping_slice / bridge.LIBRARY
            library.write_bytes(b"shipping")
            # `ios package` removed the marker when it put its library here.
            self.assertFalse(bridge.stand_in_marker(shipping).is_file())
            stamp = output / "rust-stamp.json"
            stamp.write_text("v1:phone-dev:debug-tools\n")
            built = bridge.Slice("t", output / "lib.a", output / "h.h")
            built.library.write_bytes(b"dev")
            built.header.write_text("void dev_symbol(void);")
            packaged = []

            def package(framework, _slices):
                packaged.append(framework.name)
                (framework / bridge.DRIVING_SLICE).mkdir(parents=True, exist_ok=True)
                (framework / bridge.DRIVING_SLICE / bridge.LIBRARY).write_bytes(b"dev")

            with mock.patch.object(bridge, "OUTPUT", output), \
                    mock.patch.object(ios_rust, "STAMP", stamp), \
                    mock.patch.object(bridge, "SIZE_REPORT", output / "size.txt"), \
                    mock.patch.object(bridge, "source_fingerprint", return_value="v1"), \
                    mock.patch.object(bridge, "cargo_build", return_value=built), \
                    mock.patch.object(bridge, "package", side_effect=package), \
                    mock.patch.object(
                        ios_rust.Path, "read_text",
                        return_value="[profile.release]\npanic = 'abort'\n"), \
                    mock.patch("builtins.print"):
                ios_rust.main()

            self.assertIn(bridge.FRAMEWORK, packaged)
            self.assertEqual(library.read_bytes(), b"dev")
            self.assertEqual(bridge.stand_in_marker(shipping).read_text().strip(),
                             "v1:phone-dev:debug-tools")


class ShippingBuildTests(unittest.TestCase):
    def test_shipping_output_removes_the_development_stand_in_marker(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "ios"
            output.mkdir()
            framework = output / bridge.FRAMEWORK
            marker = bridge.stand_in_marker(framework)
            marker.write_text("old:phone-dev:debug-tools\n")
            built = bridge.Slice("t", output / "lib.a", output / "h.h")
            built.library.write_bytes(b"shipping")
            built.header.write_text("void shipping_symbol(void);")

            def stage(_built, destination):
                (destination / "include").mkdir(parents=True)
                (destination / bridge.LIBRARY).write_bytes(b"shipping")
                (destination / "include" / bridge.HEADER).write_text(
                    "void shipping_symbol(void);"
                )
                return destination / bridge.LIBRARY

            with mock.patch.object(bridge, "OUTPUT", output), \
                    mock.patch.object(bridge, "SIZE_REPORT", output / "size.txt"), \
                    mock.patch.object(bridge, "cargo_build", return_value=built), \
                    mock.patch.object(bridge, "stage", side_effect=stage), \
                    mock.patch.object(bridge, "package_if_changed", return_value=False), \
                    mock.patch.object(bridge, "write_size_report", return_value=""), \
                    mock.patch.object(ios_package.subprocess, "run"), \
                    mock.patch("builtins.print"):
                ios_package.main()

            self.assertFalse(marker.exists())

    def test_a_shipping_build_from_older_rust_is_replaced_by_the_new_slice(self):
        """The Swift packages' unit tests link the shipping framework. One
        packaged before a Rust change must not survive the rebuild, or those
        tests run against the old Rust and pass or fail for the wrong reason."""
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "ios"
            for name in (bridge.DRIVING_FRAMEWORK, bridge.FRAMEWORK):
                (output / name / bridge.DRIVING_SLICE).mkdir(parents=True)
                (output / name / bridge.DRIVING_SLICE / bridge.LIBRARY).write_bytes(b"old")
            (output / "framework.sha256").write_text("digest of the shipping slices\n")
            stamp = output / "rust-stamp.json"
            stamp.write_text("before the change\n")
            built = bridge.Slice("t", output / "lib.a", output / "h.h")
            built.library.write_bytes(b"new")
            built.header.write_text("h")
            packaged = []
            fingerprint = mock.Mock(return_value="after the change")
            real_read_text = Path.read_text

            def read_text(path, *args, **kwargs):
                if path == Path("Cargo.toml"):
                    return "[profile.dev]\ndebug = 1\n"
                return real_read_text(path, *args, **kwargs)

            with mock.patch.object(bridge, "OUTPUT", output), \
                    mock.patch.object(ios_rust, "STAMP", stamp), \
                    mock.patch.object(bridge, "SIZE_REPORT", output / "size.txt"), \
                    mock.patch.object(bridge, "source_fingerprint", fingerprint), \
                    mock.patch.object(bridge, "cargo_build", return_value=built), \
                    mock.patch.object(bridge, "package",
                                      side_effect=lambda framework, _: packaged.append(framework.name)), \
                    mock.patch.object(ios_rust.Path, "read_text", read_text), \
                    mock.patch("builtins.print"):
                ios_rust.main()
                self.assertEqual(packaged, [bridge.DRIVING_FRAMEWORK, bridge.FRAMEWORK])

                # A Rust input that moved without changing what cargo built
                # leaves both frameworks alone, so Xcode rebuilds nothing.
                fingerprint.return_value = "touched, not changed"
                ios_rust.main()
                self.assertEqual(packaged, [bridge.DRIVING_FRAMEWORK, bridge.FRAMEWORK])


if __name__ == "__main__":
    unittest.main()
