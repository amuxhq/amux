"""Unit tests for the Android Google Play release and deploy scripts."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
import urllib.request

TOOLS_DIR = Path(__file__).resolve().parents[1]
if str(TOOLS_DIR) not in sys.path:
    sys.path.insert(0, str(TOOLS_DIR))

import googleplay  # noqa: E402


def load_module(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, TOOLS_DIR / filename)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


android_release = load_module("android_release", "android-release.py")
android_deploy = load_module("android_deploy", "android-deploy.py")


class FakePlayTransport:
    def __init__(self, responses: list[tuple[str, str, dict]]) -> None:
        self.responses = list(responses)
        self.calls: list[tuple[str, str, bytes | None]] = []

    def __call__(self, request: urllib.request.Request, timeout: int = 300) -> dict:
        method = request.get_method()
        url = request.full_url
        self.calls.append((method, url, request.data))
        if not self.responses:
            raise AssertionError(f"unexpected HTTP call: {method} {url}")
        expected_method, expected_fragment, answer = self.responses.pop(0)
        assert method == expected_method, f"expected {expected_method}, got {method} ({url})"
        assert expected_fragment in url, f"expected {expected_fragment!r} in {url!r}"
        return answer


class AndroidReleaseTests(unittest.TestCase):
    SAMPLE_VERSION = """
# The version the app ships as.
VERSION_NAME = "0.1.0"
VERSION_CODE = 1
"""

    def test_project_numbers_and_rewrite(self) -> None:
        self.assertEqual(
            android_release.project_numbers((android_release.REPO_ROOT / android_release.VERSION_FILE).read_text()),
            android_release.project_numbers(self.SAMPLE_VERSION),
        )
        self.assertEqual(android_release.project_numbers(self.SAMPLE_VERSION), ("0.1.0", 1))
        rewritten = android_release.rewrite_numbers(self.SAMPLE_VERSION, "1.0.0", 4)
        self.assertEqual(android_release.project_numbers(rewritten), ("1.0.0", 4))
        self.assertIn("VERSION_CODE = 4", rewritten)
        self.assertIn('VERSION_NAME = "1.0.0"', rewritten)

    def test_next_version_and_build_initial_and_subsequent(self) -> None:
        # Before any android-v* tags exist, start from project numbers or explicit overrides.
        self.assertEqual(android_release.next_version([], "0.1.0"), "0.1.0")
        self.assertEqual(android_release.next_version([], "0.1.0", "1.0.0"), "1.0.0")
        self.assertEqual(android_release.next_build([], 1), 1)
        self.assertEqual(android_release.next_build([], 1, 5), 5)

        # Once tags exist, patch and build advance monotonically.
        tags = ["android-v1.0.0-b1", "android-v1.0.1-b3", "ios-v9.9.9-b99"]
        self.assertEqual(android_release.next_version(tags, "1.0.1"), "1.0.2")
        self.assertEqual(android_release.next_version(tags, "1.0.1", "1.0.1"), "1.0.1")
        self.assertEqual(android_release.next_build(tags, 3), 4)
        self.assertEqual(android_release.next_build(tags, 3, 10), 10)

    def test_next_version_and_build_refuse_backwards(self) -> None:
        tags = ["android-v1.2.0-b7"]
        with self.assertRaises(googleplay.Refusal):
            android_release.next_version(tags, "1.2.0", "1.1.9")
        with self.assertRaises(googleplay.Refusal):
            android_release.next_build(tags, 7, 7)
        with self.assertRaises(googleplay.Refusal):
            android_release.next_build(tags, 7, 6)

    def test_play_notes_truncation(self) -> None:
        short = "Bug fixes."
        self.assertEqual(android_release.play_notes(short), short)
        long_text = "a" * 600
        trimmed = android_release.play_notes(long_text)
        self.assertEqual(len(trimmed), 500)
        self.assertTrue(trimmed.endswith("…"))

    def test_load_service_account_validation(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            missing = Path(tmp) / "missing.json"
            with self.assertRaises(googleplay.Refusal):
                googleplay.load_service_account(missing)

            bad = Path(tmp) / "bad.json"
            bad.write_text(json.dumps({"type": "authorized_user"}), encoding="utf-8")
            with self.assertRaises(googleplay.Refusal):
                googleplay.load_service_account(bad)

            good = Path(tmp) / "sa.json"
            good.write_text(
                json.dumps(
                    {
                        "type": "service_account",
                        "client_email": "release@demo.iam.gserviceaccount.com",
                        "private_key": "-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----\n",
                    }
                ),
                encoding="utf-8",
            )
            loaded = googleplay.load_service_account(good)
            self.assertEqual(loaded["client_email"], "release@demo.iam.gserviceaccount.com")


class AndroidDeployTests(unittest.TestCase):
    def test_tagged_build_picks_highest(self) -> None:
        tags = [
            "android-v1.0.0-b1",
            "android-v1.0.0-b4",
            "android-v1.0.1-b5",
            "ios-v1.0.0-b99",
        ]
        self.assertEqual(android_deploy.tagged_build(tags, "1.0.0"), 4)
        with self.assertRaises(googleplay.Refusal):
            android_deploy.tagged_build(tags, "2.0.0")

    def test_rollout_and_notes_validation(self) -> None:
        self.assertIsNone(android_deploy.validate_rollout(None))
        self.assertIsNone(android_deploy.validate_rollout(100))
        self.assertEqual(android_deploy.validate_rollout(25), 0.25)
        with self.assertRaises(googleplay.Refusal):
            android_deploy.validate_rollout(0)
        with self.assertRaises(googleplay.Refusal):
            android_deploy.validate_rollout(101)
        with self.assertRaises(googleplay.Refusal):
            android_deploy.validate_notes("x" * 501)
        with self.assertRaises(googleplay.Refusal):
            android_deploy.build_release_entry("1.0.0", 2, rollout_fraction=0.1, notes=None, draft=True)

    def test_describe_tracks_orders_standard_tracks(self) -> None:
        text = android_deploy.describe_tracks(
            [
                {
                    "track": "production",
                    "releases": [{"name": "1.0.0 (2)", "versionCodes": ["2"], "status": "inProgress", "userFraction": 0.2}],
                },
                {
                    "track": "internal",
                    "releases": [{"name": "1.0.1 (3)", "versionCodes": ["3"], "status": "completed"}],
                },
            ]
        )
        lines = text.splitlines()
        self.assertTrue(lines[0].startswith("internal: 1.0.1 (3) [3] (completed)"))
        self.assertTrue(lines[1].startswith("production: 1.0.0 (2) [2] (inProgress 20%)"))

    def test_deploy_dry_run_deletes_edit_without_commit(self) -> None:
        transport = FakePlayTransport(
            [
                ("POST", "/edits", {"id": "edit-1"}),
                ("GET", "/edits/edit-1/bundles", {"bundles": [{"versionCode": 4}]}),
                ("GET", "/edits/edit-1/tracks", {"tracks": [{"track": "internal", "releases": [{"versionCodes": ["4"], "status": "completed"}]}]}),
                ("DELETE", "/edits/edit-1", {}),
            ]
        )
        publisher = googleplay.PlayPublisher(token="tok", send=transport)
        output: list[str] = []
        android_deploy.deploy(
            publisher,
            "1.0.0",
            4,
            track="production",
            rollout=25,
            notes="Initial Android release.",
            dry_run=True,
            say=output.append,
        )
        self.assertIn("would promote 1.0.0 (build 4) to production at 25% rollout with en-US release notes", output[-1])
        self.assertEqual(len(transport.responses), 0)

    def test_deploy_commits_and_supports_changes_not_sent_for_review(self) -> None:
        transport = FakePlayTransport(
            [
                ("POST", "/edits", {"id": "edit-2"}),
                ("GET", "/edits/edit-2/bundles", {"bundles": [{"versionCode": 4}]}),
                ("GET", "/edits/edit-2/tracks", {"tracks": []}),
                ("PUT", "/edits/edit-2/tracks/production", {"track": "production"}),
                ("POST", "/edits/edit-2:validate", {}),
                ("POST", "/edits/edit-2:commit?changesNotSentForReview=true", {}),
            ]
        )
        publisher = googleplay.PlayPublisher(token="tok", send=transport)
        output: list[str] = []
        android_deploy.deploy(
            publisher,
            "1.0.0",
            4,
            track="production",
            notes="First production release.",
            changes_not_sent_for_review=True,
            say=output.append,
        )
        put_payload = json.loads(transport.calls[3][2].decode("utf-8"))
        self.assertEqual(put_payload["releases"][0]["status"], "completed")
        self.assertEqual(put_payload["releases"][0]["versionCodes"], ["4"])
        self.assertEqual(put_payload["releases"][0]["releaseNotes"][0]["text"], "First production release.")
        self.assertEqual(len(transport.responses), 0)

    def test_deploy_refuses_unknown_version_code_and_cleans_up_edit(self) -> None:
        transport = FakePlayTransport(
            [
                ("POST", "/edits", {"id": "edit-3"}),
                ("GET", "/edits/edit-3/bundles", {"bundles": [{"versionCode": 1}]}),
                ("GET", "/edits/edit-3/tracks", {"tracks": []}),
                ("DELETE", "/edits/edit-3", {}),
            ]
        )
        publisher = googleplay.PlayPublisher(token="tok", send=transport)
        with self.assertRaises(googleplay.Refusal):
            android_deploy.deploy(publisher, "1.0.0", 99, track="production")
        self.assertEqual(len(transport.responses), 0)

    def test_verify_signature_accepts_verified_and_refuses_unsigned(self) -> None:
        from unittest import mock
        import subprocess

        fake_aab = Path("/tmp/fake.aab")
        with mock.patch.object(
            subprocess,
            "run",
            return_value=subprocess.CompletedProcess(["jarsigner"], 0, stdout="\njar verified.\n", stderr=""),
        ) as patched:
            android_release.verify_signature(fake_aab, "/usr/bin/jarsigner")
            self.assertEqual(patched.call_args[0][0], ["/usr/bin/jarsigner", "-verify", str(fake_aab)])

        with mock.patch.object(
            subprocess,
            "run",
            return_value=subprocess.CompletedProcess(["jarsigner"], 0, stdout="jar is unsigned.\n", stderr=""),
        ):
            with self.assertRaises(googleplay.Refusal):
                android_release.verify_signature(fake_aab, "/usr/bin/jarsigner")


class SignCommandTest(unittest.TestCase):
    def test_passwords_come_from_the_environment_not_the_command_line(self) -> None:
        facts = {"keystore": Path("/keys/upload.jks"), "key_alias": "upload", "store_password": "s3cret", "key_password": "k3y"}
        cmd = android_release.sign_command("jarsigner", Path("in.aab"), Path("out.aab"), facts)
        self.assertNotIn("s3cret", cmd)
        self.assertNotIn("k3y", cmd)
        self.assertEqual(cmd[-3:], ["out.aab", "in.aab", "upload"])


if __name__ == "__main__":
    unittest.main()
