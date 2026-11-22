"""Unit tests for apps/android/tools/android_sdk.py."""

from __future__ import annotations

from pathlib import Path
import sys
import tempfile
import unittest

TOOLS_DIR = Path(__file__).resolve().parents[1]
if str(TOOLS_DIR) not in sys.path:
    sys.path.insert(0, str(TOOLS_DIR))

import android_sdk  # noqa: E402


class AndroidSdkTest(unittest.TestCase):
    def test_android_home_names_the_sdk_before_the_older_variable(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / "home"
            root = Path(tmp) / "root"
            home.mkdir()
            root.mkdir()
            env = {"ANDROID_HOME": str(home), "ANDROID_SDK_ROOT": str(root)}
            self.assertEqual(android_sdk.sdk(env), home)
            self.assertEqual(android_sdk.sdk({"ANDROID_SDK_ROOT": str(root)}), root)

    def test_the_newest_ndk_is_chosen_by_version_number_not_by_name(self):
        with tempfile.TemporaryDirectory() as tmp:
            sdk = Path(tmp)
            for name in ("9.0.0", "27.9.1", "27.10.0", "partial-download"):
                (sdk / "ndk" / name).mkdir(parents=True)
            self.assertEqual(android_sdk.ndk({"ANDROID_HOME": str(sdk)}), sdk / "ndk" / "27.10.0")
            (sdk / "ndk" / "28.2.13676358").mkdir()
            self.assertEqual(
                android_sdk.ndk({"ANDROID_HOME": str(sdk)}), sdk / "ndk" / "28.2.13676358"
            )

    def test_a_named_ndk_wins_over_the_sdks(self):
        with tempfile.TemporaryDirectory() as tmp:
            sdk = Path(tmp) / "sdk"
            (sdk / "ndk" / "28.2.0").mkdir(parents=True)
            named = Path(tmp) / "pinned-ndk"
            named.mkdir()
            env = {"ANDROID_HOME": str(sdk), "ANDROID_NDK_HOME": str(named)}
            self.assertEqual(android_sdk.ndk(env), named)

    def test_no_ndk_folder_means_no_ndk(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.assertIsNone(android_sdk.newest_version(Path(tmp) / "ndk"))

    def test_ndk_tool_finds_an_executable_in_the_prebuilt_toolchain(self):
        with tempfile.TemporaryDirectory() as tmp:
            ndk = Path(tmp)
            bin_dir = ndk / "toolchains" / "llvm" / "prebuilt" / "darwin-x86_64" / "bin"
            bin_dir.mkdir(parents=True)
            tool = bin_dir / "llvm-strip"
            tool.write_text("#!/bin/sh\n", encoding="utf-8")
            tool.chmod(0o755)
            self.assertEqual(android_sdk.ndk_tool(ndk, "llvm-strip"), str(tool))
            self.assertIsNone(android_sdk.ndk_tool(ndk, "llvm-nm"))
            self.assertIsNone(android_sdk.ndk_tool(None, "llvm-strip"))


if __name__ == "__main__":
    unittest.main()
