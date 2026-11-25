#!/usr/bin/env python3
"""Vendor the pinned buck2's prelude into prelude/, with its downloads made
content-addressed.

`prelude.py update` replaces prelude/ with the prelude bundled in the buck2
build/tools.json pins and patches it; run it after bumping buck2 and commit
what changes. Nothing else edits prelude/.

Why the prelude is vendored at all: its Android toolchain builds Buck's Java
tools from source against about 120 jars it downloads (from Maven Central,
Google's Maven and buck2's GitHub releases), each declared by SHA-1 alone.
Buck2 can take a download from the build cache instead of its origin only
when the declaration gives a digest the cache uses (SHA-256 here) and the
file's size; with a SHA-1 it fetches the file from the origin in every new
daemon, so every fresh CI runner fetched all of them, and Maven Central's
rate limit on shared runner addresses failed the gate. The patch gives each
download its SHA-256 and size from prelude/toolchains/android/tools/
build_rules/digests.bzl, which this script writes: a digest it has
already recorded is kept, and only a jar new to the prelude is fetched (once,
here, and checked against its SHA-1). A download the table lacks fails the
build's loading, so the gap shows rather than the fetches coming back.
"""

import hashlib
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PRELUDE = ROOT / "prelude"
RULES = "toolchains/android/tools/build_rules.bzl"
DIGESTS = "toolchains/android/tools/build_rules/digests.bzl"

# The prelude's default for a `mvn:` URL without a repository
# (remote_file.bzl's `http.maven_repo`, which amux doesn't set).
MAVEN_CENTRAL = "https://repo1.maven.org/maven2"

CALL = re.compile(r"^(third_party_(?:jar|aar|exe)|buck_prebuilt_artifact)\(\n(.*?)^\)", re.M | re.S)
SHA1 = re.compile(r'^\s*(?:oss_)?sha1 = "([0-9a-f]{40})",$', re.M)
URL = re.compile(r'^\s*(?:oss_)?url = "([^"]+)",$', re.M)
ENTRY = re.compile(r'^    "([0-9a-f]{40})": (?:\("([0-9a-f]{64})", (\d+)\)|None),', re.M)

# Each edit to build_rules.bzl, as (what upstream has, what replaces it). An
# edit that no longer matches exactly once stops the update: upstream changed
# the wrappers, and the patch needs looking at.
EDITS = [
    (
        'load("@prelude//toolchains/android/tools/build_rules:fb_native.bzl", "fb_native")\n',
        'load("@prelude//toolchains/android/tools/build_rules:fb_native.bzl", "fb_native")\n'
        'load("@prelude//toolchains/android/tools/build_rules:digests.bzl", "DIGESTS")  # amux\n',
    ),
    (
        "def _oss_remote_file_with_wrapper(name, ext, url, sha1, **kwargs):\n",
        "# amux: a download's SHA-256 and size, by the SHA-1 its BUCK file gives,\n"
        "# so Buck2 takes it from the build cache rather than its origin\n"
        "# (amux's scripts/prelude.py writes digests.bzl and says why).\n"
        "def _sha256_and_size(sha1):\n"
        "    if sha1 not in DIGESTS:\n"
        '        fail("digests.bzl has no SHA-256 for the download with SHA-1 {}: run scripts/prelude.py update".format(sha1))\n'
        "    return DIGESTS[sha1] or (None, None)\n"
        "\n"
        "def _oss_remote_file_with_wrapper(name, ext, url, sha1, **kwargs):\n",
    ),
    (
        "    fb_native.remote_file(\n"
        "        name = remote_file_target_name,\n"
        '        out = name + "." + ext,\n'
        "        sha1 = sha1,\n",
        "    sha256, size_bytes = _sha256_and_size(sha1)  # amux\n"
        "    fb_native.remote_file(\n"
        "        name = remote_file_target_name,\n"
        '        out = name + "." + ext,\n'
        "        sha1 = sha1,\n"
        "        sha256 = sha256,  # amux\n"
        "        size_bytes = size_bytes,  # amux\n",
    ),
    (
        "        return fb_native.remote_file(sha1 = oss_sha1, url = oss_url, **kwargs)\n",
        "        sha256, size_bytes = _sha256_and_size(oss_sha1)  # amux\n"
        "        return fb_native.remote_file(sha1 = oss_sha1, sha256 = sha256, size_bytes = size_bytes, url = oss_url, **kwargs)\n",
    ),
]


def maven_url(url: str) -> str:
    """The URL a `mvn:` coordinate names, as the prelude's remote_file.bzl
    resolves it: `mvn:[repo:]group:artifact:type[:classifier]:version`, the
    repository written with its scheme (`https://maven.google.com`)."""
    if not url.startswith("mvn:"):
        return url
    parts = url.split(":")[1:]
    repo = MAVEN_CENTRAL
    if len(parts) in (6, 7):
        repo = parts[0] + ":" + parts[1]
        parts = parts[2:]
    if len(parts) == 4:
        group, artifact, kind, version = parts
        classifier = ""
    elif len(parts) == 5:
        group, artifact, kind, classifier, version = parts
        classifier = "-" + classifier
    else:
        raise ValueError(f"unsupported mvn URL: {url}")
    ext = "-sources.jar" if kind == "src" else "." + kind
    return f"{repo}/{group.replace('.', '/')}/{artifact}/{version}/{artifact}-{version}{classifier}{ext}"


def downloads(prelude: Path) -> dict[str, str]:
    """Every download the Android toolchain's wrappers declare: SHA-1 to URL."""
    found: dict[str, str] = {}
    for path in sorted(p for p in prelude.rglob("BUCK") if p.is_file()):
        text = path.read_text()
        calls = list(CALL.finditer(text))
        mentions = len(re.findall(r"^(?:third_party_(?:jar|aar|exe)|buck_prebuilt_artifact)\(", text, re.M))
        if len(calls) != mentions:
            sys.exit(f"prelude: {path} has a wrapper call this script can't read")
        for call in calls:
            sha1, url = SHA1.findall(call.group(2)), URL.findall(call.group(2))
            if call.group(1) == "buck_prebuilt_artifact" and not sha1:
                continue  # Meta's own CAS artifact only, nothing to fetch
            if len(sha1) != 1 or len(url) != 1:
                sys.exit(f"prelude: {path}: a {call.group(1)} without one sha1 and one url")
            found[sha1[0]] = maven_url(url[0])
    return found


Digest = tuple[str, int] | None


def read_digests(path: Path) -> dict[str, Digest]:
    if not path.exists():
        return {}
    return {
        sha1: (sha256, int(size)) if sha256 else None
        for sha1, sha256, size in ENTRY.findall(path.read_text())
    }


def render_digests(digests: dict[str, Digest], urls: dict[str, str]) -> str:
    lines = [
        "# Written by amux's scripts/prelude.py, which says why: each Android",
        "# toolchain download's SHA-256 and size, by the SHA-1 its BUCK file gives.",
        "# None is a download its origin doesn't serve (it 404s), which nothing",
        "# amux builds uses: it keeps its SHA-1 alone, as upstream declares it.",
        "DIGESTS = {",
    ]
    for sha1 in sorted(digests):
        lines.append(f"    # {urls[sha1]}")
        digest = digests[sha1]
        lines.append(f'    "{sha1}": ("{digest[0]}", {digest[1]}),' if digest else f'    "{sha1}": None,')
    lines.append("}")
    return "\n".join(lines) + "\n"


def fetch_digest(sha1: str, url: str) -> Digest:
    print(f"prelude: fetching {url}", file=sys.stderr)
    try:
        with urllib.request.urlopen(url, timeout=300) as response:
            data = response.read()
    except urllib.error.HTTPError as error:
        if error.code != 404:
            raise
        print(f"prelude: {url} is not there (404); it keeps its SHA-1 alone", file=sys.stderr)
        return None
    if hashlib.sha1(data).hexdigest() != sha1:
        sys.exit(f"prelude: {url} doesn't have the SHA-1 the prelude declares, {sha1}")
    return hashlib.sha256(data).hexdigest(), len(data)


def patch(rules: str) -> str:
    for old, new in EDITS:
        if rules.count(old) != 1:
            sys.exit(f"prelude: {RULES} no longer has, exactly once:\n{old}")
        rules = rules.replace(old, new)
    return rules


def expand(into: Path) -> None:
    """The prelude bundled in the pinned buck2, written to `into`."""
    buck2 = subprocess.run(
        [sys.executable, str(ROOT / "scripts/buck-tools.py"), "path", "buck2"],
        check=True, capture_output=True, text=True,
    ).stdout.strip()
    with tempfile.TemporaryDirectory() as scratch:
        project = Path(scratch)
        (project / ".buckroot").write_text("")
        (project / ".buckconfig").write_text(
            "[cells]\n  root = .\n  prelude = prelude\n[external_cells]\n  prelude = bundled\n"
        )
        try:
            subprocess.run([buck2, "expand-external-cell", "prelude"], cwd=project, check=True)
        finally:
            subprocess.run([buck2, "kill"], cwd=project, capture_output=True)
        shutil.copytree(project / "prelude", into)


def update() -> None:
    kept = read_digests(PRELUDE / DIGESTS)
    with tempfile.TemporaryDirectory() as scratch:
        fresh = Path(scratch) / "prelude"
        expand(fresh)
        urls = downloads(fresh)
        digests = {sha1: kept[sha1] if sha1 in kept else fetch_digest(sha1, url) for sha1, url in urls.items()}
        (fresh / DIGESTS).write_text(render_digests(digests, urls))
        (fresh / RULES).write_text(patch((fresh / RULES).read_text()))
        shutil.rmtree(PRELUDE, ignore_errors=True)
        shutil.copytree(fresh, PRELUDE)
    print(f"prelude: {len(urls)} downloads, {len(urls.keys() - kept.keys())} newly fetched", file=sys.stderr)


def main() -> None:
    if sys.argv[1:] == ["update"]:
        update()
    else:
        sys.exit("usage: prelude.py update")


if __name__ == "__main__":
    main()
