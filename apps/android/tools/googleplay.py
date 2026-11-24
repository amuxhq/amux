#!/usr/bin/env python3
"""Google Play Android Publisher API v3 client using a GCP service account.

Authenticates with a service account JSON key (the credential
`googleplay/service-account`, scripts/credentials.py; `AMUX_GOOGLEPLAY_KEY_FILE`
names a file instead) via RS256 JWT signed by `openssl`, using only the Python
standard library.
"""

from __future__ import annotations

import base64
from contextlib import contextmanager
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from typing import Callable, Iterator

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "scripts"))
import credentials  # noqa: E402

PACKAGE_NAME = "sh.amux.android"
SERVICE_ACCOUNT = "googleplay/service-account"
SCOPE = "https://www.googleapis.com/auth/androidpublisher"
API = "https://androidpublisher.googleapis.com/androidpublisher/v3"
UPLOAD_API = "https://androidpublisher.googleapis.com/upload/androidpublisher/v3"
DEFAULT_TOKEN_URI = "https://oauth2.googleapis.com/token"


class Refusal(Exception):
    """Something a Play release or deploy must not do, saying why."""


def key_file_path() -> Path | None:
    """The file `AMUX_GOOGLEPLAY_KEY_FILE` names, or None to ask the
    credential command."""
    override = os.environ.get("AMUX_GOOGLEPLAY_KEY_FILE", "").strip()
    return Path(override).expanduser() if override else None


def service_account() -> dict:
    """The service account, from the file the environment names or else from
    the credential command."""
    key_file = key_file_path()
    if key_file is not None:
        return load_service_account(key_file)
    try:
        text = credentials.read(SERVICE_ACCOUNT)
    except credentials.Missing as missing:
        raise Refusal(f"{missing} (docs/RELEASE.md, the Android app's setup)") from None
    return parse_service_account(text, SERVICE_ACCOUNT)


def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def openssl_sign(private_key: str, data: bytes) -> bytes:
    """RS256 signature via openssl, keeping the private key in a 0600 temp file."""
    handle, path = tempfile.mkstemp(suffix=".pem")
    try:
        os.chmod(path, 0o600)
        with os.fdopen(handle, "w", encoding="utf-8") as file:
            file.write(private_key)
        signed = subprocess.run(
            ["openssl", "dgst", "-sha256", "-sign", path],
            input=data,
            capture_output=True,
            timeout=60,
        )
    finally:
        try:
            os.unlink(path)
        except OSError:
            pass
    if signed.returncode != 0:
        detail = signed.stderr.decode("utf-8", errors="replace").strip()
        raise Refusal(f"openssl could not sign with the service account's key: {detail}")
    return signed.stdout


def jwt_assertion(
    account: dict,
    now: int,
    sign: Callable[[str, bytes], bytes] = openssl_sign,
) -> str:
    """Build the signed JWT a Google Cloud service account exchanges for a token."""
    header = {"alg": "RS256", "typ": "JWT"}
    claims = {
        "iss": account["client_email"],
        "scope": SCOPE,
        "aud": account.get("token_uri", DEFAULT_TOKEN_URI),
        "iat": now,
        "exp": now + 3600,
    }
    unsigned = f"{b64url(json.dumps(header).encode())}.{b64url(json.dumps(claims).encode())}"
    signature = sign(account["private_key"], unsigned.encode("ascii"))
    return f"{unsigned}.{b64url(signature)}"


def load_service_account(key_file: Path) -> dict:
    """Read and validate a GCP service account JSON key without hitting the network."""
    if not key_file.is_file():
        raise Refusal(
            f"no Google Play service account key at {key_file} "
            "(docs/RELEASE.md, the Android app's setup)"
        )
    try:
        text = key_file.read_text(encoding="utf-8")
    except OSError as error:
        raise Refusal(f"could not read service account key {key_file}: {error}") from None
    return parse_service_account(text, str(key_file))


def parse_service_account(text: str, where: str) -> dict:
    """A GCP service account JSON key, validated; `where` names its source."""
    try:
        account = json.loads(text)
    except json.JSONDecodeError as error:
        raise Refusal(f"could not read service account key {where}: {error}") from None
    if not isinstance(account, dict):
        raise Refusal(f"service account key {where} is not a JSON object")
    if account.get("type") != "service_account":
        raise Refusal(
            f"service account key {where} has type {account.get('type')!r}, "
            "expected 'service_account'"
        )
    for field in ("client_email", "private_key"):
        if not account.get(field):
            raise Refusal(f"service account key {where} is missing {field!r}")
    return account


def _http_send(request: urllib.request.Request, timeout: int = 300) -> dict:
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            text = response.read().decode("utf-8")
    except urllib.error.HTTPError as error:
        body = error.read().decode("utf-8", errors="replace").strip()
        raise Refusal(
            f"{request.get_method()} {request.full_url} answered {error.code}: {body}"
        ) from None
    except urllib.error.URLError as error:
        raise Refusal(f"{request.get_method()} {request.full_url} failed: {error.reason}") from None
    return json.loads(text) if text.strip() else {}


class PlayPublisher:
    """Thin client for Google Play Android Publisher API v3 for one package."""

    def __init__(
        self,
        account: dict | None = None,
        *,
        package_name: str = PACKAGE_NAME,
        token: str | None = None,
        send: Callable[[urllib.request.Request, int], dict] = _http_send,
        sign: Callable[[str, bytes], bytes] = openssl_sign,
    ) -> None:
        self.package_name = package_name
        self._send = send
        if token is not None:
            self.token = token
        else:
            account = account or service_account()
            token_uri = account.get("token_uri", DEFAULT_TOKEN_URI)
            body = urllib.parse.urlencode(
                {
                    "grant_type": "urn:ietf:params:oauth:grant-type:jwt-bearer",
                    "assertion": jwt_assertion(account, int(time.time()), sign),
                }
            ).encode("ascii")
            req = urllib.request.Request(token_uri, data=body, method="POST")
            req.add_header("Content-Type", "application/x-www-form-urlencoded")
            answer = self._send(req, 60)
            access_token = answer.get("access_token")
            if not access_token:
                raise Refusal("Google OAuth token exchange returned no access_token")
            self.token = str(access_token)

    def _call(
        self,
        method: str,
        url: str,
        *,
        body: bytes | None = None,
        content_type: str | None = None,
        timeout: int = 300,
    ) -> dict:
        request = urllib.request.Request(url, data=body, method=method)
        request.add_header("Authorization", f"Bearer {self.token}")
        if content_type is not None:
            request.add_header("Content-Type", content_type)
        return self._send(request, timeout)

    def _json_call(
        self,
        method: str,
        path: str,
        payload: dict | None = None,
        *,
        query: dict[str, str] | None = None,
        timeout: int = 300,
    ) -> dict:
        url = f"{API}/applications/{self.package_name}{path}"
        if query:
            url = f"{url}?{urllib.parse.urlencode(query)}"
        if payload is not None:
            body = json.dumps(payload).encode("utf-8")
            return self._call(method, url, body=body, content_type="application/json", timeout=timeout)
        return self._call(method, url, timeout=timeout)

    def insert_edit(self) -> str:
        answer = self._json_call("POST", "/edits", {})
        edit_id = answer.get("id")
        if not edit_id:
            raise Refusal(f"Google Play returned no edit id: {answer}")
        return str(edit_id)

    def delete_edit(self, edit_id: str) -> None:
        self._json_call("DELETE", f"/edits/{edit_id}")

    def validate_edit(self, edit_id: str) -> dict:
        return self._json_call("POST", f"/edits/{edit_id}:validate", {})

    def commit_edit(self, edit_id: str, *, changes_not_sent_for_review: bool = False) -> dict:
        query = {"changesNotSentForReview": "true"} if changes_not_sent_for_review else None
        return self._json_call("POST", f"/edits/{edit_id}:commit", {}, query=query)

    @contextmanager
    def edit(
        self,
        *,
        commit: bool = False,
        changes_not_sent_for_review: bool = False,
    ) -> Iterator[str]:
        """Open a Play Console edit, committing on clean exit or deleting otherwise."""
        edit_id = self.insert_edit()
        committed = False
        try:
            yield edit_id
            if commit:
                self.commit_edit(
                    edit_id,
                    changes_not_sent_for_review=changes_not_sent_for_review,
                )
                committed = True
        finally:
            if not committed:
                try:
                    self.delete_edit(edit_id)
                except Refusal:
                    pass

    def list_bundles(self, edit_id: str) -> list[dict]:
        return list(self._json_call("GET", f"/edits/{edit_id}/bundles").get("bundles", []))

    def upload_bundle(self, edit_id: str, aab_path: Path) -> dict:
        if not aab_path.is_file():
            raise Refusal(f"bundle not found at {aab_path}")
        url = (
            f"{UPLOAD_API}/applications/{self.package_name}"
            f"/edits/{edit_id}/bundles?uploadType=media"
        )
        return self._call(
            "POST",
            url,
            body=aab_path.read_bytes(),
            content_type="application/octet-stream",
            timeout=1800,
        )

    def list_tracks(self, edit_id: str) -> list[dict]:
        return list(self._json_call("GET", f"/edits/{edit_id}/tracks").get("tracks", []))

    def get_track(self, edit_id: str, track: str) -> dict:
        return self._json_call("GET", f"/edits/{edit_id}/tracks/{track}")

    def update_track(self, edit_id: str, track: str, releases: list[dict]) -> dict:
        return self._json_call(
            "PUT",
            f"/edits/{edit_id}/tracks/{track}",
            {"track": track, "releases": releases},
        )
