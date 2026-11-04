"""The App Store Connect API calls the iPhone app's release tools make.

A token signed with the release Mac's App Store Connect key, whose
identifiers live in the login keychain under `amux-appstoreconnect` (accounts
`key-id` and `issuer-id`) and whose private key is under
~/.appstoreconnect/private_keys (docs/RELEASE.md); and a small client whose
network the tests replace.
"""

from __future__ import annotations

from pathlib import Path
import base64
import json
import subprocess
import time
import urllib.error
import urllib.request

API = "https://api.appstoreconnect.apple.com"
BUNDLE = "sh.amux.app"
KEYCHAIN_SERVICE = "amux-appstoreconnect"
KEY_DIRECTORY = Path.home() / ".appstoreconnect/private_keys"


class Refusal(Exception):
    """Something this must not do, saying which piece and why."""


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


def signed_in() -> Connect:
    """A client signed in with this Mac's App Store Connect key."""
    key_id, issuer = keychain("key-id"), keychain("issuer-id")
    if not (key_id and issuer):
        raise Refusal(f"the login keychain has no App Store Connect key under {KEYCHAIN_SERVICE}; "
                      "docs/RELEASE.md has the one-time setup")
    private = KEY_DIRECTORY / f"AuthKey_{key_id}.p8"
    if not private.exists():
        raise Refusal(f"no key file in {KEY_DIRECTORY} for the keychain's key id")
    return Connect(token(key_id, issuer, private, int(time.time())))
