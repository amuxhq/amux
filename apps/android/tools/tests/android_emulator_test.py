"""Unit tests for apps/android/tools/android-emulator.py."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest

TOOLS_DIR = Path(__file__).resolve().parents[1]


def load_module():
    spec = importlib.util.spec_from_file_location(
        "android_emulator", TOOLS_DIR / "android-emulator.py",
    )
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class AndroidEmulatorTests(unittest.TestCase):
    def setUp(self) -> None:
        self.mod = load_module()

    def test_parse_adb_devices_skips_headers_and_daemon_lines(self) -> None:
        sample = """
* daemon not running; starting now at tcp:5037
* daemon started successfully
List of devices attached
emulator-5554\tdevice
emulator-5556\toffline
R5CR123456\tdevice

"""
        self.assertEqual(
            self.mod.parse_adb_devices(sample),
            [
                ("emulator-5554", "device"),
                ("emulator-5556", "offline"),
                ("R5CR123456", "device"),
            ],
        )

    def test_parse_avd_name_output(self) -> None:
        self.assertEqual(
            self.mod.parse_avd_name_output("amux-android-1\r\nOK\r\n"),
            "amux-android-1",
        )
        self.assertIsNone(self.mod.parse_avd_name_output("OK\n"))

    def test_parse_emulator_serial_line(self) -> None:
        line = "android (amux-android-1): emulator-5554 (booted, animations=0)\n"
        self.assertEqual(self.mod.parse_emulator_serial_line(line), "emulator-5554")
        self.assertIsNone(self.mod.parse_emulator_serial_line(line, avd="other-avd"))

    def test_emulator_launch_command_uses_headless_flags(self) -> None:
        cmd = self.mod.emulator_launch_command("/sdk/emulator/emulator", "amux-android-1", gpu="auto")
        self.assertEqual(
            cmd,
            [
                "/sdk/emulator/emulator",
                "-avd",
                "amux-android-1",
                "-no-window",
                "-no-audio",
                "-no-boot-anim",
                "-no-snapshot-save",
                "-gpu",
                "auto",
            ],
        )


if __name__ == "__main__":
    unittest.main()
