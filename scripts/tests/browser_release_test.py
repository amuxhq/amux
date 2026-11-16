"""The browser extension's release: its versions, its package, and what a
deploy does against what the Chrome Web Store holds."""

import base64
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
import zipfile

SCRIPT = Path(__file__).resolve().parent.parent / "browser_release.py"
spec = importlib.util.spec_from_file_location("browser_release", SCRIPT)
browser_release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(browser_release)
Refusal = browser_release.Refusal


def store(published=None, submitted=None, **rest):
    def revision(found):
        state, version = found
        return {"state": state, "distributionChannels": [{"deployPercentage": 100, "crxVersion": version}]}
    status = dict(rest)
    if published:
        status["publishedItemRevisionStatus"] = revision(published)
    if submitted:
        status["submittedItemRevisionStatus"] = revision(submitted)
    return status


class VersionTest(unittest.TestCase):
    def test_chrome_versions_are_one_to_four_numbers(self):
        self.assertEqual(browser_release.parts("1"), (1,))
        self.assertEqual(browser_release.parts("1.2.3.4"), (1, 2, 3, 4))
        for wrong in ["1.x", "1.2.3.4.5", "v1.0", "1.70000", ""]:
            with self.assertRaises(Refusal, msg=wrong):
                browser_release.parts(wrong)

    def test_the_version_only_goes_up(self):
        with tempfile.TemporaryDirectory() as folder:
            manifest = Path(folder) / "manifest.json"
            manifest.write_text('{\n  "name": "amux",\n  "version": "0.1.0"\n}\n')
            for low in ["0.1.0", "0.0.9"]:
                with self.assertRaises(Refusal):
                    browser_release.set_version(manifest, low)
            self.assertEqual(browser_release.set_version(manifest, "0.1.1"), "0.1.0")
            self.assertEqual(json.loads(manifest.read_text())["version"], "0.1.1")
            self.assertIn('"name": "amux"', manifest.read_text())


class PackageTest(unittest.TestCase):
    def test_the_zip_holds_the_build_at_its_root_and_a_dev_build_is_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            dist = Path(folder) / "dist"
            (dist / "panel").mkdir(parents=True)
            (dist / "manifest.json").write_text(json.dumps({"version": "0.2.0"}))
            (dist / "panel/index.html").write_text("<p>")
            zipped = Path(folder) / "out/amux-0.2.0.zip"
            browser_release.package(dist, zipped)
            self.assertEqual(zipfile.ZipFile(zipped).namelist(), ["manifest.json", "panel/index.html"])
            browser_release.check_package(zipped, "0.2.0")
            with self.assertRaises(Refusal):
                browser_release.check_package(zipped, "0.3.0")
            (dist / "manifest.json").write_text(json.dumps({"version": "0.2.0", "key": "MIIB"}))
            browser_release.package(dist, zipped)
            with self.assertRaises(Refusal):
                browser_release.check_package(zipped, "0.2.0")


class PlanTest(unittest.TestCase):
    def test_a_new_version_is_uploaded(self):
        self.assertEqual(browser_release.plan(store(), "0.1.0"), "upload")
        self.assertEqual(browser_release.plan(store(published=("PUBLISHED", "0.1.0")), "0.2.0"), "upload")
        # A submission review turned down, or one cancelled, is replaced.
        for state in ["REJECTED", "CANCELLED"]:
            self.assertEqual(browser_release.plan(store(submitted=(state, "0.2.0")), "0.2.0"), "upload")

    def test_an_approved_staged_version_is_published_without_uploading_again(self):
        self.assertEqual(browser_release.plan(store(published=("PUBLISHED", "0.1.0"),
                                                    submitted=("STAGED", "0.2.0")), "0.2.0"),
                         "publish-staged")

    def test_what_would_be_thrown_away_or_refused_is_refused(self):
        for status in [
            store(published=("PUBLISHED", "0.2.0")),
            store(published=("PUBLISHED", "0.3.0")),
            store(submitted=("PENDING_REVIEW", "0.2.0")),
            store(submitted=("STAGED", "0.1.9")),
            store(takenDown=True),
        ]:
            with self.assertRaises(Refusal, msg=json.dumps(status)):
                browser_release.plan(status, "0.2.0")


class TokenTest(unittest.TestCase):
    def test_the_assertion_is_an_rs256_jwt_openssl_signs_and_verifies(self):
        with tempfile.TemporaryDirectory() as folder:
            key = Path(folder) / "key.pem"
            subprocess.run(["openssl", "genrsa", "-out", str(key), "2048"], check=True,
                           capture_output=True, timeout=60)
            public = Path(folder) / "public.pem"
            subprocess.run(["openssl", "rsa", "-in", str(key), "-pubout", "-out", str(public)],
                           check=True, capture_output=True, timeout=60)
            account = {"client_email": "release@amux.iam.gserviceaccount.com",
                       "private_key": key.read_text(), "token_uri": "https://oauth2.googleapis.com/token"}
            jwt = browser_release.assertion(account, 1_000, browser_release.openssl_sign)
            header, claims, signature = jwt.split(".")

            def decode(part):
                return base64.urlsafe_b64decode(part + "=" * (-len(part) % 4))

            self.assertEqual(json.loads(decode(header)), {"alg": "RS256", "typ": "JWT"})
            self.assertEqual(json.loads(decode(claims)), {
                "iss": "release@amux.iam.gserviceaccount.com", "scope": browser_release.SCOPE,
                "aud": "https://oauth2.googleapis.com/token", "iat": 1_000, "exp": 4_600})
            (Path(folder) / "signature").write_bytes(decode(signature))
            verified = subprocess.run(
                ["openssl", "dgst", "-sha256", "-verify", str(public), "-signature",
                 str(Path(folder) / "signature")],
                input=f"{header}.{claims}".encode(), capture_output=True, timeout=60)
            self.assertEqual(verified.returncode, 0, verified.stderr)


if __name__ == "__main__":
    unittest.main()
