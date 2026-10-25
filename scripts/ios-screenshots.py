#!/usr/bin/env python3
"""Put the App Store's screenshots on the version being prepared.

`just store` composes seven 1320x2868 frames; this replaces the iPhone
screenshots of one App Store version's listing with them, in order, through
the App Store Connect API. It never creates a build or submits anything for
review: a version still needs a build attached and a person to submit it.

  --dry-run         find the app, the version, the listing and the
                    screenshot set, say what would change, and change
                    nothing.
  --version X.Y.Z   the version to put them on, created if the app has no
                    version of that number yet. Without it, the one version
                    that can still be edited.

Only a version that has not gone to review, or that review sent back, takes
new screenshots: a live version's listing is fixed until the next one.

The frames go in the 6.9-inch set, and every other iPhone set of the
listing is removed, so that smaller iPhones show these frames scaled rather
than an older set made for them. iPad sets are left alone. The key is the one
`just ios release` signs in with: its identifiers in the login keychain, its
private key under ~/.appstoreconnect/private_keys (docs/RELEASE.md).
"""

from __future__ import annotations

from pathlib import Path
import argparse
import base64
import hashlib
import json
import struct
import subprocess
import sys
import time
import urllib.error
import urllib.request

API = "https://api.appstoreconnect.apple.com"
BUNDLE = "sh.amux.app"
KEYCHAIN_SERVICE = "amux-appstoreconnect"
KEY_DIRECTORY = Path.home() / ".appstoreconnect/private_keys"
ROOT = Path(__file__).resolve().parents[1]
FRAMES = Path.home() / ".local/state/amux-marketing" / ROOT.name / "store"
COUNT = 7
# The 6.9-inch iPhone's two accepted portrait sizes.
SIZES = {(1320, 2868), (1290, 2796)}
# The listing's 6.9-inch set. App Store Connect calls it 6.9-inch and takes
# 1320x2868 in it; the API's reference lists no larger iPhone type.
# --display-type names another if Apple adds one.
DISPLAY_TYPE = "APP_IPHONE_67"
# The states in which a version's listing can still be changed.
EDITABLE = {"PREPARE_FOR_SUBMISSION", "DEVELOPER_REJECTED", "REJECTED",
            "METADATA_REJECTED", "INVALID_BINARY"}
# How long Apple takes to process an uploaded screenshot, at most.
PROCESSING = 300


class Refusal(Exception):
    """Something this must not do, saying which piece and why."""


def png_size(path: Path) -> tuple[int, int]:
    head = path.read_bytes()[:24]
    if head[:8] != b"\x89PNG\r\n\x1a\n":
        raise Refusal(f"{path.name} is not a PNG")
    return struct.unpack(">II", head[16:24])


def frames(folder: Path) -> list[Path]:
    """The seven frames, 1.png to 7.png, each at a 6.9-inch size."""
    paths = [folder / f"{i}.png" for i in range(1, COUNT + 1)]
    missing = [path.name for path in paths if not path.exists()]
    if missing:
        raise Refusal(f"{folder} has no {', '.join(missing)}; `just store` composes them")
    for path in paths:
        if png_size(path) not in SIZES:
            width, height = png_size(path)
            raise Refusal(f"{path.name} is {width}x{height}, not a 6.9-inch iPhone's 1320x2868")
    return paths


def raw_signature(der: bytes) -> bytes:
    """An ECDSA signature as JWT's ES256 wants it, r and s at 32 bytes each,
    from the DER sequence openssl writes."""
    def integer(at: int) -> tuple[int, int]:
        if der[at] != 0x02:
            raise ValueError("not a DER integer")
        length = der[at + 1]
        return int.from_bytes(der[at + 2:at + 2 + length], "big"), at + 2 + length

    if der[0] != 0x30:
        raise ValueError("not a DER sequence")
    at = 3 if der[1] & 0x80 else 2
    r, at = integer(at)
    s, _ = integer(at)
    return r.to_bytes(32, "big") + s.to_bytes(32, "big")


def b64(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def token(key_id: str, issuer: str, private_key: Path, now: int) -> str:
    """A twenty-minute token for the App Store Connect API, signed with the
    key by openssl, which every Mac this runs on has."""
    header = b64(json.dumps({"alg": "ES256", "kid": key_id, "typ": "JWT"}).encode())
    claims = b64(json.dumps({"iss": issuer, "iat": now, "exp": now + 1200,
                             "aud": "appstoreconnect-v1"}).encode())
    signing = f"{header}.{claims}".encode()
    signed = subprocess.run(["openssl", "dgst", "-sha256", "-sign", str(private_key)],
                            input=signing, capture_output=True, timeout=60)
    if signed.returncode != 0:
        raise Refusal("openssl could not sign with the App Store Connect key: "
                      + signed.stderr.decode(errors="replace").strip())
    return f"{header}.{claims}.{b64(raw_signature(signed.stdout))}"


def keychain(account: str) -> str:
    found = subprocess.run(["security", "find-generic-password", "-s", KEYCHAIN_SERVICE,
                            "-a", account, "-w"], capture_output=True, text=True, timeout=60)
    return found.stdout.strip() if found.returncode == 0 else ""


def send(method: str, url: str, headers: dict, body: bytes | None) -> tuple[int, bytes]:
    request = urllib.request.Request(url, data=body, method=method, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()


class Connect:
    """The few App Store Connect calls this needs. `send` is the network,
    which the tests replace."""

    def __init__(self, bearer: str, send=send) -> None:
        self.bearer = bearer
        self.send = send
        self.writes: list[str] = []

    def call(self, method: str, path: str, body: dict | None = None) -> dict:
        headers = {"Authorization": f"Bearer {self.bearer}"}
        data = None
        if body is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(body).encode()
        if method != "GET":
            self.writes.append(f"{method} {path.split('?')[0]}")
        status, reply = self.send(method, API + path, headers, data)
        if status >= 400:
            try:
                errors = json.loads(reply).get("errors", [])
                detail = "; ".join(e.get("detail") or e.get("title", "") for e in errors)
            except ValueError:
                detail = reply[:200].decode(errors="replace")
            raise Refusal(f"App Store Connect refused {method} {path.split('?')[0]} ({status}): {detail}")
        return json.loads(reply) if reply else {}

    def upload(self, operation: dict, data: bytes) -> None:
        """One of the parts Apple asked for, sent where it said."""
        part = data[operation["offset"]:operation["offset"] + operation["length"]]
        headers = {h["name"]: h["value"] for h in operation.get("requestHeaders", [])}
        status, reply = self.send(operation["method"], operation["url"], headers, part)
        if status >= 400:
            raise Refusal(f"the upload of a screenshot's part was refused ({status})")


def app(connect: Connect) -> str:
    found = connect.call("GET", f"/v1/apps?filter[bundleId]={BUNDLE}")["data"]
    if not found:
        raise Refusal(f"App Store Connect has no app with bundle id {BUNDLE} on this key's team")
    return found[0]["id"]


def version(connect: Connect, app_id: str, wanted: str | None, dry_run: bool) -> tuple[str, str, bool]:
    """The version to change: its id, its number, and whether it is yet to
    be created. A dry run creates nothing; for a version it would create, it
    answers the live version's id instead, whose listing the new one starts
    as."""
    versions = connect.call(
        "GET", f"/v1/apps/{app_id}/appStoreVersions?filter[platform]=IOS&limit=50")["data"]
    if wanted:
        for found in versions:
            if found["attributes"]["versionString"] == wanted:
                state = found["attributes"]["appStoreState"]
                if state not in EDITABLE:
                    raise Refusal(f"version {wanted} is {state}; its screenshots can no longer change")
                return found["id"], wanted, False
        if dry_run:
            live = next((v for v in versions if v["attributes"]["appStoreState"] == "READY_FOR_SALE"), None)
            return (live or {}).get("id", ""), wanted, True
        created = connect.call("POST", "/v1/appStoreVersions", {"data": {
            "type": "appStoreVersions",
            "attributes": {"platform": "IOS", "versionString": wanted},
            "relationships": {"app": {"data": {"type": "apps", "id": app_id}}}}})
        return created["data"]["id"], wanted, False
    open_ = [found for found in versions if found["attributes"]["appStoreState"] in EDITABLE]
    if not open_:
        states = ", ".join(f"{v['attributes']['versionString']} {v['attributes']['appStoreState']}"
                           for v in versions) or "none"
        raise Refusal("no version can take new screenshots (" + states + "). Name the next one "
                      "with --version, or create it in App Store Connect")
    found = open_[0]
    return found["id"], found["attributes"]["versionString"], False


def localization(connect: Connect, version_id: str, locale: str) -> str:
    found = connect.call(
        "GET", f"/v1/appStoreVersions/{version_id}/appStoreVersionLocalizations?limit=50")["data"]
    for item in found:
        if item["attributes"]["locale"] == locale:
            return item["id"]
    have = ", ".join(item["attributes"]["locale"] for item in found) or "none"
    raise Refusal(f"the version has no {locale} listing (it has {have}); --locale names another")


def screenshot_sets(connect: Connect, localization_id: str) -> list[dict]:
    return connect.call(
        "GET", f"/v1/appStoreVersionLocalizations/{localization_id}/appScreenshotSets"
        "?include=appScreenshots&limit=50")["data"]


def held(screenshot_set: dict | None) -> list[str]:
    """The ids of a set's screenshots."""
    return [s["id"] for s in (screenshot_set or {}).get("relationships", {})
            .get("appScreenshots", {}).get("data", [])]


def put(connect: Connect, set_id: str, path: Path) -> str:
    """One frame uploaded into the set, as Apple asks: reserve it, send its
    parts, then commit it with its checksum. Answers its id."""
    data = path.read_bytes()
    reserved = connect.call("POST", "/v1/appScreenshots", {"data": {
        "type": "appScreenshots",
        "attributes": {"fileName": path.name, "fileSize": len(data)},
        "relationships": {"appScreenshotSet": {"data": {"type": "appScreenshotSets", "id": set_id}}}}})
    screenshot = reserved["data"]
    for operation in screenshot["attributes"]["uploadOperations"]:
        connect.upload(operation, data)
    connect.call("PATCH", f"/v1/appScreenshots/{screenshot['id']}", {"data": {
        "type": "appScreenshots", "id": screenshot["id"],
        "attributes": {"uploaded": True, "sourceFileChecksum": hashlib.md5(data).hexdigest()}}})
    return screenshot["id"]


def processed(connect: Connect, ids: list[str], clock=time.monotonic, sleep=time.sleep) -> None:
    """Until Apple has processed every uploaded screenshot."""
    deadline = clock() + PROCESSING
    waiting = list(ids)
    while waiting:
        for screenshot_id in list(waiting):
            state = connect.call("GET", f"/v1/appScreenshots/{screenshot_id}")["data"][
                "attributes"].get("assetDeliveryState") or {}
            if state.get("state") == "COMPLETE":
                waiting.remove(screenshot_id)
            elif state.get("state") == "FAILED":
                errors = "; ".join(e.get("description", "") for e in state.get("errors", []))
                raise Refusal(f"Apple could not process a screenshot: {errors}")
        if waiting:
            if clock() > deadline:
                raise Refusal(f"{len(waiting)} screenshots were still processing after {PROCESSING} s")
            sleep(3)


def replace(connect: Connect, paths: list[Path], version_wanted: str | None, locale: str,
            named_type: str | None, dry_run: bool, say=print, sleep=time.sleep) -> None:
    app_id = app(connect)
    version_id, number, to_create = version(connect, app_id, version_wanted, dry_run)
    if to_create and not version_id:
        say(f"would create version {number}, then put {len(paths)} screenshots on its {locale} listing")
        return
    localization_id = localization(connect, version_id, locale)
    sets = screenshot_sets(connect, localization_id)
    kind = named_type or DISPLAY_TYPE
    target = next((s for s in sets if s["attributes"]["screenshotDisplayType"] == kind), None)
    old = held(target)
    others = [s for s in sets if s is not target
              and s["attributes"]["screenshotDisplayType"].startswith("APP_IPHONE_")]
    if dry_run:
        created = f"would create version {number}, whose listing starts as the live one's; " if to_create else ""
        removed = "".join(f"; would remove the {s['attributes']['screenshotDisplayType']} set "
                          f"({len(held(s))} screenshots), so smaller iPhones show these scaled"
                          for s in others)
        say(f"{created}would replace the {len(old)} {kind} screenshots of version {number}'s {locale} "
            f"listing with {len(paths)}: {', '.join(path.name for path in paths)}{removed}")
        return
    if target is None:
        target = connect.call("POST", "/v1/appScreenshotSets", {"data": {
            "type": "appScreenshotSets", "attributes": {"screenshotDisplayType": kind},
            "relationships": {"appStoreVersionLocalization": {"data": {
                "type": "appStoreVersionLocalizations", "id": localization_id}}}}})["data"]
    for screenshot_id in old:
        connect.call("DELETE", f"/v1/appScreenshots/{screenshot_id}")
    ids = []
    for path in paths:
        ids.append(put(connect, target["id"], path))
        say(f"uploaded {path.name}")
    processed(connect, ids, sleep=sleep)
    connect.call("PATCH", f"/v1/appScreenshotSets/{target['id']}/relationships/appScreenshots",
                 {"data": [{"type": "appScreenshots", "id": i} for i in ids]})
    # Only once the new set is whole: a failed upload leaves the listing
    # with the screenshots it had for smaller iPhones.
    for other in others:
        connect.call("DELETE", f"/v1/appScreenshotSets/{other['id']}")
        say(f"removed the {other['attributes']['screenshotDisplayType']} set")
    say(f"version {number}'s {locale} listing has {len(ids)} {kind} screenshots, in order")


def main(arguments: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--frames", type=Path, default=FRAMES)
    parser.add_argument("--version")
    parser.add_argument("--locale", default="en-US")
    parser.add_argument("--display-type")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(arguments)
    try:
        paths = frames(args.frames)
        key_id, issuer = keychain("key-id"), keychain("issuer-id")
        if not (key_id and issuer):
            raise Refusal(f"the login keychain has no App Store Connect key under {KEYCHAIN_SERVICE}; "
                          "docs/RELEASE.md has the one-time setup")
        private = KEY_DIRECTORY / f"AuthKey_{key_id}.p8"
        if not private.exists():
            raise Refusal(f"no key file in {KEY_DIRECTORY} for the keychain's key id")
        connect = Connect(token(key_id, issuer, private, int(time.time())))
        replace(connect, paths, args.version, args.locale, args.display_type, args.dry_run)
    except Refusal as refusal:
        print(f"ios-screenshots: {refusal}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
