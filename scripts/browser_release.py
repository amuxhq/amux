"""Release the browser extension and put it on the Chrome Web Store.

  release VERSION   set the manifest's version, build, zip, commit, tag
                    `browser-v<version>`, push, and keep the zip as the
                    tag's GitHub Release asset.
  deploy VERSION    upload that asset to the store's listing and submit it
                    for review, published as soon as review approves it.
                    --staged holds it after approval until a second
                    `deploy VERSION` publishes it; --dry-run reads the
                    store, says what would happen, changes nothing.
  status            what the store holds: the published and the submitted
                    revision, and the last upload.

The store is reached as the publisher's service account, whose JSON key is
at ~/.chromewebstore/service-account.json (docs/RELEASE.md). Deploying
uploads the zip the release made, not a fresh build, so the store gets the
bytes the tag recorded.
"""

from __future__ import annotations

from pathlib import Path
import argparse
import base64
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "js/extension/static/manifest.json"
DIST = ROOT / "js/extension/dist"
OUT = ROOT / "target/browser"

# The listing's identity. The item ID is the extension's ID, which the
# manifest key in development builds also derives, so every amux host's
# native-messaging manifest already allows it.
PUBLISHER = "cd66548b-541f-40d1-a918-939b30c1ea9a"
ITEM = "omnhphmiepghpgpbfgjebeaajfppkhie"
API = "https://chromewebstore.googleapis.com"
SCOPE = "https://www.googleapis.com/auth/chromewebstore"
KEY_FILE = Path.home() / ".chromewebstore/service-account.json"


class Refusal(Exception):
    """Something this must not do, saying why."""


# ---- versions --------------------------------------------------------------

def parts(version: str) -> tuple[int, ...]:
    """A Chrome extension version: one to four numbers, each at most 65535."""
    if not re.fullmatch(r"\d+(\.\d+){0,3}", version):
        raise Refusal(f"{version!r} is not a Chrome extension version: one to four numbers joined by dots")
    numbers = tuple(int(part) for part in version.split("."))
    if any(number > 65535 for number in numbers):
        raise Refusal(f"{version!r} has a part over 65535")
    return numbers


def set_version(manifest: Path, version: str) -> str:
    """Move the manifest to `version`, which the store requires be above the
    one it holds; returns the version it had."""
    current = json.loads(manifest.read_text())["version"]
    if parts(version) <= parts(current):
        raise Refusal(f"{version} is not above the manifest's {current}")
    text = manifest.read_text()
    manifest.write_text(text.replace(f'"version": "{current}"', f'"version": "{version}"', 1))
    return current


def zipped_manifest(package: Path) -> dict:
    with zipfile.ZipFile(package) as archive:
        return json.loads(archive.read("manifest.json"))


def check_package(package: Path, version: str) -> None:
    """The zip is a store build of `version`. A store build carries no
    manifest key: the store holds the item's key, and one in the package is
    refused after the first upload."""
    manifest = zipped_manifest(package)
    if manifest.get("version") != version:
        raise Refusal(f"{package.name} holds version {manifest.get('version')}, not {version}")
    if "key" in manifest:
        raise Refusal(f"{package.name} is a development build: its manifest has a key")


def package(dist: Path, target: Path) -> None:
    """Zip the built extension with its files at the archive's root, in a
    fixed order."""
    target.parent.mkdir(parents=True, exist_ok=True)
    target.unlink(missing_ok=True)
    with zipfile.ZipFile(target, "w", zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(p for p in dist.rglob("*") if p.is_file()):
            archive.write(path, path.relative_to(dist).as_posix())


# ---- the store -------------------------------------------------------------

def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def assertion(account: dict, now: int, sign) -> str:
    """The signed JWT a service account trades for an access token."""
    header = {"alg": "RS256", "typ": "JWT"}
    claims = {"iss": account["client_email"], "scope": SCOPE,
              "aud": account.get("token_uri", "https://oauth2.googleapis.com/token"),
              "iat": now, "exp": now + 3600}
    unsigned = f"{b64url(json.dumps(header).encode())}.{b64url(json.dumps(claims).encode())}"
    return f"{unsigned}.{b64url(sign(account['private_key'], unsigned.encode()))}"


def openssl_sign(private_key: str, data: bytes) -> bytes:
    """RS256 by openssl, which every Mac and Linux this runs on has. The key
    goes through a file only this user can read, removed straight after."""
    handle, path = tempfile.mkstemp(suffix=".pem")
    try:
        with os.fdopen(handle, "w") as file:
            file.write(private_key)
        signed = subprocess.run(["openssl", "dgst", "-sha256", "-sign", path],
                                input=data, capture_output=True, timeout=60)
    finally:
        os.unlink(path)
    if signed.returncode != 0:
        raise Refusal(f"openssl could not sign with the service account's key: {signed.stderr.decode().strip()}")
    return signed.stdout


class Store:
    """The Chrome Web Store API for the one item, as the service account."""

    def __init__(self, key_file: Path):
        if not key_file.exists():
            raise Refusal(f"no service account key at {key_file} (docs/RELEASE.md, the browser extension's setup)")
        account = json.loads(key_file.read_text())
        token_uri = account.get("token_uri", "https://oauth2.googleapis.com/token")
        body = urllib.parse.urlencode({
            "grant_type": "urn:ietf:params:oauth:grant-type:jwt-bearer",
            "assertion": assertion(account, int(time.time()), openssl_sign),
        }).encode()
        self.token = self._send(urllib.request.Request(token_uri, data=body, method="POST"))["access_token"]
        self.name = f"publishers/{PUBLISHER}/items/{ITEM}"

    def _send(self, request: urllib.request.Request) -> dict:
        try:
            with urllib.request.urlopen(request, timeout=300) as response:
                text = response.read().decode()
        except urllib.error.HTTPError as error:
            raise Refusal(f"{request.get_method()} {request.full_url} answered {error.code}: "
                          f"{error.read().decode().strip()}") from None
        return json.loads(text) if text.strip() else {}

    def call(self, method: str, path: str, body: bytes | None = None,
             content_type: str = "application/json") -> dict:
        request = urllib.request.Request(f"{API}{path}", data=body, method=method)
        request.add_header("Authorization", f"Bearer {self.token}")
        if body is not None:
            request.add_header("Content-Type", content_type)
        return self._send(request)

    def status(self) -> dict:
        return self.call("GET", f"/v2/{self.name}:fetchStatus")

    def upload(self, package: Path) -> dict:
        return self.call("POST", f"/upload/v2/{self.name}:upload", package.read_bytes(), "application/zip")

    def publish(self, staged: bool) -> dict:
        body = {"publishType": "STAGED_PUBLISH" if staged else "DEFAULT_PUBLISH"}
        return self.call("POST", f"/v2/{self.name}:publish", json.dumps(body).encode())


def revision(status: dict, which: str) -> tuple[str, str] | None:
    """(state, version) of the published or submitted revision, if any."""
    found = status.get(f"{which}ItemRevisionStatus")
    if not found:
        return None
    versions = [channel.get("crxVersion", "") for channel in found.get("distributionChannels", [])]
    return found.get("state", "?"), max(versions, key=lambda v: parts(v) if v else (), default="")


def plan(status: dict, version: str) -> str:
    """What deploying `version` does against what the store holds:
    "publish-staged" when review already approved it and it waits staged,
    "upload" otherwise. Refuses what the store would refuse or what would
    throw away a submission."""
    if status.get("takenDown"):
        raise Refusal("the store has taken the extension down; the dashboard says why")
    published = revision(status, "published")
    if published and published[1] and parts(published[1]) >= parts(version):
        raise Refusal(f"the store already publishes {published[1]}")
    submitted = revision(status, "submitted")
    if submitted:
        state, held = submitted
        if state == "STAGED" and held == version:
            return "publish-staged"
        if state in ("PENDING_REVIEW", "STAGED"):
            raise Refusal(f"{held} is {state.lower().replace('_', ' ')} in the store; cancel it in the "
                          "dashboard before submitting another version")
    return "upload"


def describe(status: dict) -> str:
    lines = []
    for which in ("published", "submitted"):
        found = revision(status, which)
        lines.append(f"{which}: " + (f"{found[1] or '?'} ({found[0]})" if found else "none"))
    if status.get("lastAsyncUploadState"):
        lines.append(f"last upload: {status['lastAsyncUploadState']}")
    if status.get("warned"):
        lines.append("warned for a policy violation: see the dashboard")
    if status.get("takenDown"):
        lines.append("taken down for a policy violation: see the dashboard")
    return "\n".join(lines)


# ---- the commands ----------------------------------------------------------

def git(*args: str) -> str:
    done = subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True, timeout=300)
    if done.returncode != 0:
        raise Refusal(f"git {' '.join(args)}: {done.stderr.strip()}")
    return done.stdout


def run(*args: str, timeout: int) -> None:
    if subprocess.run(list(args), cwd=ROOT, timeout=timeout).returncode != 0:
        raise Refusal(f"{' '.join(args)} failed")


def release(version: str, key_file: Path) -> None:
    parts(version)
    if git("status", "--porcelain").strip():
        raise Refusal("the tree has changes; a release commits only the version")
    if not key_file.exists():
        raise Refusal(f"no service account key at {key_file}, so the release could not be deployed")
    tag = f"browser-v{version}"
    if git("tag", "--list", tag).strip():
        raise Refusal(f"{tag} already exists")
    set_version(MANIFEST, version)
    try:
        run(os.environ.get("JUST", "just"), "browser", "build", timeout=1500)
        zipped = OUT / f"amux-{version}.zip"
        package(DIST, zipped)
        check_package(zipped, version)
    except BaseException:
        git("checkout", "--", str(MANIFEST.relative_to(ROOT)))
        raise
    git("add", str(MANIFEST.relative_to(ROOT)))
    git("commit", "-q", "-m", f"Release the browser extension {version}")
    git("tag", tag)
    git("push")
    git("push", "origin", tag)
    run("gh", "release", "create", tag, str(zipped), "--latest=false",
        "--title", f"Browser extension {version}",
        "--notes", f"The amux browser extension {version}, as uploaded to the Chrome Web Store.",
        timeout=300)
    print(f"released {tag}; `just browser deploy {version}` submits it to the Chrome Web Store.")


def fetch_package(version: str) -> Path:
    folder = OUT / "deploy"
    folder.mkdir(parents=True, exist_ok=True)
    name = f"amux-{version}.zip"
    run("gh", "release", "download", f"browser-v{version}", "--pattern", name,
        "--dir", str(folder), "--clobber", timeout=300)
    zipped = folder / name
    check_package(zipped, version)
    return zipped


def wait_for_upload(store: Store, answer: dict, version: str) -> None:
    state = answer.get("uploadState")
    deadline = time.monotonic() + 600
    while state == "IN_PROGRESS":
        if time.monotonic() > deadline:
            raise Refusal("the store was still processing the upload after ten minutes; "
                          "`just browser status` shows when it finishes")
        time.sleep(5)
        state = store.status().get("lastAsyncUploadState")
    if state != "SUCCEEDED":
        raise Refusal(f"the store's upload ended {state}: {json.dumps(answer)}")
    if answer.get("crxVersion") not in (None, version):
        raise Refusal(f"the store read version {answer['crxVersion']} from the upload, not {version}")


def deploy(version: str, staged: bool, dry_run: bool, key_file: Path) -> None:
    parts(version)
    store = Store(key_file)
    status = store.status()
    print(describe(status))
    action = plan(status, version)
    if action == "publish-staged":
        if dry_run:
            print(f"would publish the staged {version}, which review has approved")
            return
        answer = store.publish(staged=False)
        print(f"published {version}: {answer.get('state')}")
        return
    zipped = fetch_package(version)
    if dry_run:
        kind = "held after approval until published" if staged else "published once approved"
        print(f"would upload {zipped.relative_to(ROOT)} and submit {version} for review, {kind}")
        return
    wait_for_upload(store, store.upload(zipped), version)
    answer = store.publish(staged)
    for warning in answer.get("warningInfo", {}).get("warnings", []):
        print(f"warning: {warning.get('reason')}: {warning.get('description')}")
    then = ("`just browser deploy " + version + "` publishes it once approved") if staged \
        else "the store publishes it once review approves it"
    print(f"submitted {version}: {answer.get('state')}; {then}.")


def main(arguments: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    cut = commands.add_parser("release")
    cut.add_argument("version")
    put = commands.add_parser("deploy")
    put.add_argument("version")
    put.add_argument("--staged", action="store_true")
    put.add_argument("--dry-run", action="store_true")
    commands.add_parser("status")
    options = parser.parse_args(arguments)
    try:
        if options.command == "release":
            release(options.version, KEY_FILE)
        elif options.command == "deploy":
            deploy(options.version, options.staged, options.dry_run, KEY_FILE)
        else:
            print(describe(Store(KEY_FILE).status()))
    except Refusal as refusal:
        print(f"browser {options.command}: {refusal}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
