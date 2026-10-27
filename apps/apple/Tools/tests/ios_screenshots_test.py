"""Check what the screenshot upload changes on a listing, against a fake App
Store Connect: which version it picks, which set it replaces, how it uploads
a frame and the order it leaves them in. Nothing here reaches Apple."""

import hashlib
import importlib.util
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
import zlib

SCRIPTS = Path(__file__).resolve().parents[1]
specification = importlib.util.spec_from_file_location("ios_screenshots", SCRIPTS / "ios-screenshots.py")
recipe = importlib.util.module_from_spec(specification)
specification.loader.exec_module(recipe)

API = recipe.API


def png(width: int, height: int) -> bytes:
    """A real PNG of that size, one grey pixel row repeated."""
    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    rows = b"".join(b"\x00" + b"\x80" * width for _ in range(height))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 0, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def folder(size=(1320, 2868), count=7) -> Path:
    path = Path(tempfile.mkdtemp())
    for i in range(1, count + 1):
        (path / f"{i}.png").write_bytes(png(*size))
    return path


class Fake:
    """App Store Connect with one app, its versions, one en-US listing and
    its screenshot sets, answering the calls the script makes and keeping
    every write."""

    def __init__(self, states=(("1.0.0", "READY_FOR_SALE"), ("1.1.0", "PREPARE_FOR_SUBMISSION")),
                 sets=(("APP_IPHONE_67", ((1320, 2868), (1320, 2868))), ("APP_IPHONE_65", ((1242, 2688),)),
                       ("APP_IPAD_PRO_3GEN_129", ((2048, 2732),))),
                 processing=1):
        self.versions = [{"id": f"v{i}", "type": "appStoreVersions",
                          "attributes": {"versionString": number, "appStoreState": state}}
                         for i, (number, state) in enumerate(states)]
        self.shots = {}
        self.sets = []
        for i, (kind, sizes) in enumerate(sets):
            ids = []
            for j, (w, h) in enumerate(sizes):
                sid = f"old{i}{j}"
                self.shots[sid] = {"id": sid, "type": "appScreenshots",
                                   "attributes": {"imageAsset": {"width": w, "height": h}}}
                ids.append(sid)
            self.sets.append({"id": f"set{i}", "type": "appScreenshotSets",
                              "attributes": {"screenshotDisplayType": kind},
                              "relationships": {"appScreenshots": {"data": [
                                  {"type": "appScreenshots", "id": s} for s in ids]}}})
        self.processing = processing
        self.polls = {}
        self.parts = {}
        self.committed = {}
        self.deleted = []
        self.order = None
        self.created_version = None
        self.removed_sets = []

    def __call__(self, method, url, headers, body):
        if not url.startswith(API):
            return self.part(method, url, headers, body)
        assert headers["Authorization"] == "Bearer t"
        path = url[len(API):]
        data = json.loads(body) if body else None
        route = path.split("?")[0]
        if method == "GET" and route == "/v1/apps":
            assert "filter[bundleId]=sh.amux.app" in path
            return 200, json.dumps({"data": [{"id": "app", "type": "apps"}]}).encode()
        if method == "GET" and route == "/v1/apps/app/appStoreVersions":
            return 200, json.dumps({"data": self.versions}).encode()
        if method == "POST" and route == "/v1/appStoreVersions":
            self.created_version = data["data"]["attributes"]["versionString"]
            return 201, json.dumps({"data": {"id": "vnew", "type": "appStoreVersions"}}).encode()
        if method == "GET" and route.endswith("/appStoreVersionLocalizations"):
            return 200, json.dumps({"data": [
                {"id": "loc", "type": "appStoreVersionLocalizations", "attributes": {"locale": "en-US"}}]}).encode()
        if method == "GET" and route == "/v1/appStoreVersionLocalizations/loc/appScreenshotSets":
            return 200, json.dumps({"data": self.sets, "included": list(self.shots.values())}).encode()
        if method == "DELETE" and route.startswith("/v1/appScreenshots/"):
            self.deleted.append(route.rsplit("/", 1)[1])
            return 204, b""
        if method == "DELETE" and route.startswith("/v1/appScreenshotSets/"):
            self.removed_sets.append(route.rsplit("/", 1)[1])
            return 204, b""
        if method == "POST" and route == "/v1/appScreenshots":
            attributes = data["data"]["attributes"]
            sid = f"new{len(self.parts)}"
            self.parts[sid] = (attributes["fileName"], attributes["fileSize"], bytearray())
            half = attributes["fileSize"] // 2
            operations = [
                {"method": "PUT", "url": f"https://upload/{sid}/0", "offset": 0, "length": half,
                 "requestHeaders": [{"name": "Content-Type", "value": "image/png"}]},
                {"method": "PUT", "url": f"https://upload/{sid}/1", "offset": half,
                 "length": attributes["fileSize"] - half, "requestHeaders": []},
            ]
            return 201, json.dumps({"data": {"id": sid, "type": "appScreenshots",
                                             "attributes": {"uploadOperations": operations}}}).encode()
        if method == "PATCH" and route.startswith("/v1/appScreenshots/"):
            sid = route.rsplit("/", 1)[1]
            self.committed[sid] = data["data"]["attributes"]
            return 200, json.dumps({"data": {"id": sid}}).encode()
        if method == "GET" and route.startswith("/v1/appScreenshots/"):
            sid = route.rsplit("/", 1)[1]
            self.polls[sid] = self.polls.get(sid, 0) + 1
            state = "COMPLETE" if self.polls[sid] >= self.processing else "UPLOAD_COMPLETE"
            return 200, json.dumps({"data": {"id": sid, "attributes": {
                "assetDeliveryState": {"state": state}}}}).encode()
        if method == "PATCH" and route.endswith("/relationships/appScreenshots"):
            self.order = (route.split("/")[3], [item["id"] for item in data["data"]])
            return 204, b""
        if method == "POST" and route == "/v1/appScreenshotSets":
            self.sets.append({"id": "setnew", "attributes": data["data"]["attributes"]})
            return 201, json.dumps({"data": {"id": "setnew"}}).encode()
        return 404, json.dumps({"errors": [{"detail": f"no route {method} {route}"}]}).encode()

    def part(self, method, url, headers, body):
        sid, index = url.split("/")[-2:]
        assert method == "PUT"
        self.parts[sid][2].extend(body)
        return 200, b""


def run(fake, paths, **kwargs):
    said = []
    connect = recipe.Connect("t", send=fake)
    recipe.replace(connect, paths, kwargs.get("version"), "en-US", kwargs.get("display_type"),
                   kwargs.get("dry_run", False), say=said.append, sleep=lambda _: None)
    return connect, said


class Frames(unittest.TestCase):
    def test_seven_frames_at_the_six_point_nine_inch_size_are_taken_in_order(self):
        paths = recipe.frames(folder())
        self.assertEqual([p.name for p in paths], [f"{i}.png" for i in range(1, 8)])

    def test_a_missing_frame_or_a_wrong_size_is_refused(self):
        with self.assertRaisesRegex(recipe.Refusal, "has no 7.png"):
            recipe.frames(folder(count=6))
        with self.assertRaisesRegex(recipe.Refusal, "1206x2622"):
            recipe.frames(folder(size=(1206, 2622)))


class Signing(unittest.TestCase):
    def test_openssls_der_signature_becomes_es256s_raw_pair(self):
        r, s = (1 << 255) + 5, 7
        r_bytes = b"\x00" + r.to_bytes(32, "big")
        der = (b"\x30" + bytes([4 + len(r_bytes) + 1]) + b"\x02" + bytes([len(r_bytes)]) + r_bytes
               + b"\x02\x01\x07")
        self.assertEqual(recipe.raw_signature(der), r.to_bytes(32, "big") + s.to_bytes(32, "big"))

    def test_a_token_is_signed_by_the_key_and_verifies(self):
        work = Path(tempfile.mkdtemp())
        key, public = work / "key.p8", work / "public.pem"
        subprocess.run(["openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", str(key)],
                       check=True, capture_output=True)
        subprocess.run(["openssl", "ec", "-in", str(key), "-pubout", "-out", str(public)],
                       check=True, capture_output=True)
        made = recipe.token("KEY", "ISSUER", key, now=1_000)
        header, claims, signature = made.split(".")
        pad = lambda text: text + "=" * (-len(text) % 4)
        self.assertEqual(json.loads(recipe.base64.urlsafe_b64decode(pad(header))),
                         {"alg": "ES256", "kid": "KEY", "typ": "JWT"})
        self.assertEqual(json.loads(recipe.base64.urlsafe_b64decode(pad(claims))),
                         {"iss": "ISSUER", "iat": 1_000, "exp": 2_200, "aud": "appstoreconnect-v1"})
        raw = recipe.base64.urlsafe_b64decode(pad(signature))
        r, s = int.from_bytes(raw[:32], "big"), int.from_bytes(raw[32:], "big")

        def integer(n):
            body = n.to_bytes((n.bit_length() + 8) // 8, "big")
            return b"\x02" + bytes([len(body)]) + body
        der = integer(r) + integer(s)
        (work / "sig.der").write_bytes(b"\x30" + bytes([len(der)]) + der)
        verified = subprocess.run(["openssl", "dgst", "-sha256", "-verify", str(public), "-signature",
                                   str(work / "sig.der")], input=f"{header}.{claims}".encode(),
                                  capture_output=True)
        self.assertEqual(verified.returncode, 0, verified.stderr)


class Listing(unittest.TestCase):
    def test_the_open_versions_six_point_nine_inch_set_is_replaced_in_order(self):
        fake, paths = Fake(processing=2), recipe.frames(folder())
        connect, said = run(fake, paths)
        self.assertEqual(sorted(fake.deleted), ["old00", "old01"])
        self.assertEqual(len(fake.parts), 7)
        for i, (sid, (name, size, sent)) in enumerate(sorted(fake.parts.items(), key=lambda kv: int(kv[0][3:]))):
            data = paths[i].read_bytes()
            self.assertEqual((name, size, bytes(sent)), (paths[i].name, len(data), data))
            self.assertEqual(fake.committed[sid],
                             {"uploaded": True, "sourceFileChecksum": hashlib.md5(data).hexdigest()})
        self.assertEqual(fake.order, ("set0", [f"new{i}" for i in range(7)]))
        self.assertIn("version 1.1.0's en-US listing has 7 APP_IPHONE_67 screenshots, in order", said[-1])
        # The smaller iPhone's set goes, so it shows these scaled; the iPad's stays.
        self.assertEqual(fake.removed_sets, ["set1"])
        self.assertEqual(said[-2], "removed the APP_IPHONE_65 set")

    def test_a_dry_run_says_what_it_would_replace_and_writes_nothing(self):
        fake = Fake()
        connect, said = run(fake, recipe.frames(folder()), dry_run=True)
        self.assertEqual(connect.writes, [])
        self.assertEqual(said, ["would replace the 2 APP_IPHONE_67 screenshots of version 1.1.0's en-US "
                                "listing with 7: 1.png, 2.png, 3.png, 4.png, 5.png, 6.png, 7.png; would remove "
                                "the APP_IPHONE_65 set (1 screenshots), so smaller iPhones show these scaled"])

    def test_with_no_open_version_it_stops_and_names_each_versions_state(self):
        fake = Fake(states=(("1.0.0", "READY_FOR_SALE"),))
        with self.assertRaisesRegex(recipe.Refusal, r"no version can take new screenshots \(1.0.0 READY_FOR_SALE\)"):
            run(fake, recipe.frames(folder()))

    def test_a_named_version_is_created_when_the_app_has_none_of_that_number(self):
        # A new version starts with the live one's listing, which the fake's
        # sets stand for.
        fake = Fake(states=(("1.0.0", "READY_FOR_SALE"),))
        run(fake, recipe.frames(folder()), version="1.2.0")
        self.assertEqual(fake.created_version, "1.2.0")
        self.assertEqual(fake.order, ("set0", [f"new{i}" for i in range(7)]))

    def test_a_dry_run_for_a_new_version_reads_the_live_listing_it_would_start_as(self):
        fake = Fake(states=(("1.0.0", "READY_FOR_SALE"),))
        connect, said = run(fake, recipe.frames(folder()), version="1.2.0", dry_run=True)
        self.assertEqual(connect.writes, [])
        self.assertEqual(fake.created_version, None)
        self.assertTrue(said[0].startswith("would create version 1.2.0, whose listing starts as the live "
                                           "one's; would replace the 2 APP_IPHONE_67 screenshots"), said)

    def test_a_named_version_already_live_is_refused(self):
        with self.assertRaisesRegex(recipe.Refusal, "1.0.0 is READY_FOR_SALE"):
            run(Fake(), recipe.frames(folder()), version="1.0.0")

    def test_a_listing_with_only_a_smaller_set_gets_a_six_point_nine_inch_one_in_its_place(self):
        fake = Fake(sets=(("APP_IPHONE_65", ((1242, 2688),)),))
        run(fake, recipe.frames(folder()))
        self.assertEqual(fake.sets[-1]["attributes"], {"screenshotDisplayType": "APP_IPHONE_67"})
        self.assertEqual(fake.order[0], "setnew")
        self.assertEqual(fake.deleted, [])
        self.assertEqual(fake.removed_sets, ["set0"])

    def test_a_failed_upload_leaves_the_smaller_iphones_set_in_place(self):
        fake = Fake()
        original = fake.part

        def refusing(method, url, headers, body):
            return 500, b""
        fake.part = refusing
        with self.assertRaises(recipe.Refusal):
            run(fake, recipe.frames(folder()))
        self.assertEqual(fake.removed_sets, [])

    def test_a_named_display_type_without_a_set_gets_one_made(self):
        fake = Fake(sets=())
        run(fake, recipe.frames(folder()), display_type="APP_IPHONE_69")
        self.assertEqual(fake.sets[-1]["attributes"], {"screenshotDisplayType": "APP_IPHONE_69"})
        self.assertEqual(fake.order[0], "setnew")

    def test_a_refusal_from_apple_says_its_detail(self):
        fake = Fake()
        original = fake.__call__

        def refusing(method, url, headers, body):
            if method == "DELETE":
                return 409, json.dumps({"errors": [{"detail": "The version is in review"}]}).encode()
            return original(method, url, headers, body)
        with self.assertRaisesRegex(recipe.Refusal, "refused DELETE /v1/appScreenshots/old00 \\(409\\): The version is in review"):
            run(refusing, recipe.frames(folder()))


if __name__ == "__main__":
    unittest.main()
