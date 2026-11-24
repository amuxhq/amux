#!/usr/bin/env python3
"""Take the Android app from a clean checkout to a signed, uploaded .aab.

Everything a release needs is here and nothing beyond it: this script bumps
the version name and code in apps/android/app/version.bzl, builds the Android
App Bundle with Buck2 (`//apps/android/app:bundle`, on a Linux worker), signs
it with the upload key and verifies its signature,
uploads it to Google Play's internal testing track, and only then commits the
numbers, cuts the annotated `android-v<version>-b<build>` tag and pushes both.
It never submits anything to Production: `just android deploy` promotes a
tagged build across tracks.

Three ways to run it:

  --preflight   report every input a release needs and stop, reaching Google
                Play for nothing.
  --rehearse    build, sign and verify the .aab with the would-be version
                numbers, writing nothing to the tree and cutting no tag.
  (neither)     write the numbers, build, sign, verify, upload to internal
                testing, commit, tag and push. --no-upload and --no-push each
                stop short of their step.

The upload keystore, its alias and passwords, and the Google Play service
account's JSON key come from the operator's credential command
(scripts/credentials.py); the keystore is on disk only while a run lasts.
`AMUX_ANDROID_*` and `AMUX_GOOGLEPLAY_KEY_FILE` name them instead
(docs/RELEASE.md).
"""

from __future__ import annotations

import argparse
from contextlib import ExitStack, contextmanager
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
from googleplay import (  # noqa: E402
    PACKAGE_NAME,
    PlayPublisher,
    Refusal,
    credentials,
    service_account,
)

REPO_ROOT = Path(__file__).resolve().parents[3]
VERSION_FILE = Path("apps/android/app/version.bzl")
GOOGLE_SERVICES_FILE = Path("apps/android/app/google-services.json")
BUILT = Path("scripts/built")
BUNDLE_TARGET = "//apps/android/app:bundle"
RELEASE_DIRECTORY = Path("target/android/release")
# The upload key's credentials: the keystore, its alias and passwords.
UPLOAD_KEY = "googleplay"
TAG_RE = re.compile(r"^android-v(\d+)\.(\d+)\.(\d+)-b(\d+)$")
VERSION_CODE_RE = re.compile(r"^(VERSION_CODE\s*=\s*)(\d+)\b", re.M)
VERSION_NAME_RE = re.compile(r'^(VERSION_NAME\s*=\s*)"([^"]+)"', re.M)
LOCALE = "en-US"
MAX_PLAY_NOTES_LEN = 500


def project_numbers(version_text: str) -> tuple[str, int]:
    """Read (version name, version code) from apps/android/app/version.bzl."""
    name_match = VERSION_NAME_RE.search(version_text)
    code_match = VERSION_CODE_RE.search(version_text)
    if not name_match or not code_match:
        raise Refusal(
            f"{VERSION_FILE} sets no VERSION_NAME or VERSION_CODE this script can read"
        )
    return name_match.group(2), int(code_match.group(2))


def rewrite_numbers(version_text: str, version: str, build: int) -> str:
    """Return `version_text` with the version name and code updated."""
    updated, name_count = VERSION_NAME_RE.subn(rf'\g<1>"{version}"', version_text, count=1)
    updated, code_count = VERSION_CODE_RE.subn(rf"\g<1>{build}", updated, count=1)
    if name_count != 1 or code_count != 1:
        missing = " and ".join(
            label
            for label, count in (("versionName", name_count), ("versionCode", code_count))
            if count != 1
        )
        raise Refusal(f"{VERSION_FILE} has no {missing} line this script can write")
    return updated


def released(tags_list: list[str]) -> list[tuple[tuple[int, int, int], int]]:
    """Every (version_tuple, versionCode) recorded by android-v* tags."""
    found: list[tuple[tuple[int, int, int], int]] = []
    for name in tags_list:
        match = TAG_RE.match(name.strip())
        if match:
            major, minor, patch, build = (int(part) for part in match.groups())
            found.append(((major, minor, patch), build))
    return found


def next_version(tags_list: list[str], current: str, override: str = "") -> str:
    """Derive the next marketing versionName.

    Before any android-v* tag exists, defaults to the project's versionName
    (or `--version`). Once tags exist, bumps the patch of the highest known
    version. `--version` may equal the highest known version (for a second
    build of an unreleased version), but may never move backwards.
    """
    issued = [version for version, _ in released(tags_list)]
    if not re.fullmatch(r"\d+\.\d+\.\d+", current):
        raise Refusal(f"{current!r} is not a three-part version")
    here = tuple(int(part) for part in current.split("."))
    highest = max(issued + [here])
    if override:
        if not re.fullmatch(r"\d+\.\d+\.\d+", override):
            raise Refusal(f"--version {override} is not a three-part version")
        wanted = tuple(int(part) for part in override.split("."))
        if wanted < highest:
            high_str = ".".join(str(p) for p in highest)
            raise Refusal(
                f"--version {override} is below {high_str}, which the project already "
                "carries or a tag already records"
            )
        return override
    if not issued:
        return current
    major, minor, patch = highest
    return f"{major}.{minor}.{patch + 1}"


def next_build(tags_list: list[str], current: int, override: int = 0) -> int:
    """Derive the next monotonic Play Store versionCode.

    Google Play permanently binds each versionCode once uploaded. Before any
    android-v* tag exists, starts from the project's versionCode (or `--build`).
    Once tags exist, strictly increments above max(tagged, project).
    """
    issued = [build for _, build in released(tags_list)]
    if not issued:
        if override:
            if override < 1:
                raise Refusal(f"--build {override} must be at least 1")
            return override
        if current < 1:
            raise Refusal(f"project versionCode {current} must be at least 1")
        return current
    highest = max(issued + [current])
    if override:
        if override <= highest:
            raise Refusal(
                f"--build {override} is not above {highest}, the highest versionCode "
                "already issued; a versionCode is permanent and can only move up"
            )
        return override
    return highest + 1


def tag_name(version: str, build: int) -> str:
    return f"android-v{version}-b{build}"


def git_tags() -> list[str]:
    listed = subprocess.run(
        ["git", "tag", "--list", "android-v*"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        timeout=60,
    )
    if listed.returncode != 0:
        raise Refusal(f"git tag --list failed: {listed.stderr.strip()}")
    return listed.stdout.splitlines()


def tree() -> str:
    status = subprocess.run(
        ["git", "status", "--porcelain"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        timeout=120,
    )
    if status.returncode != 0:
        raise Refusal(f"git status failed: {status.stderr.strip()}")
    return status.stdout


def tree_changes(before: str, after: str) -> list[str]:
    return sorted(
        {line for line in after.splitlines() if line.strip()}
        - {line for line in before.splitlines() if line.strip()}
    )


@contextmanager
def upload_keystore():
    """The upload keystore for the run: the file `AMUX_ANDROID_KEYSTORE` names,
    or else the credential command's, on disk until the run ends. None when
    neither has one."""
    override = os.environ.get("AMUX_ANDROID_KEYSTORE", "").strip()
    if override:
        yield Path(override).expanduser()
        return
    with ExitStack() as stack:
        try:
            path = stack.enter_context(
                credentials.file(f"{UPLOAD_KEY}/upload-keystore", "upload-keystore.jks"))
        except credentials.Missing:
            path = None
        yield path


def secret(env_var: str, field: str) -> str:
    value = os.environ.get(env_var, "").strip()
    if value:
        return value
    try:
        return credentials.read(f"{UPLOAD_KEY}/{field}").strip()
    except credentials.Missing:
        return ""


def find_jdk_tool(name: str) -> str | None:
    candidates = [
        os.environ.get("JAVA_HOME"),
        "/opt/homebrew/opt/openjdk@21/libexec/openjdk.jdk/Contents/Home",
        "/usr/local/opt/openjdk@21/libexec/openjdk.jdk/Contents/Home",
        "/usr/lib/jvm/java-21-openjdk-amd64",
        "/usr/lib/jvm/java-21-openjdk-arm64",
    ]
    for home in candidates:
        if not home:
            continue
        candidate = Path(home) / "bin" / name
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return str(candidate)
    return shutil.which(name)


def check_keystore_credentials(
    keytool: str,
    store: Path,
    alias: str,
    store_password: str,
) -> tuple[bool, str]:
    """Prove locally that the alias and store password open the keystore."""
    env = os.environ.copy()
    env["AMUX_KEYTOOL_STOREPASS"] = store_password
    result = subprocess.run(
        [
            keytool,
            "-list",
            "-keystore",
            str(store),
            "-alias",
            alias,
            "-storepass:env",
            "AMUX_KEYTOOL_STOREPASS",
        ],
        env=env,
        capture_output=True,
        text=True,
        timeout=60,
    )
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip().splitlines()
        summary = detail[-1] if detail else "keytool refused the keystore or alias"
        return False, summary
    for line in result.stdout.splitlines():
        if "SHA-256:" in line:
            return True, f"alias {alias!r} opens {store.name} ({line.strip()})"
    return True, f"alias {alias!r} opens {store.name}"


class Check:
    def __init__(self, what: str, held: bool, detail: str) -> None:
        self.what = what
        self.held = held
        self.detail = detail

    def line(self) -> str:
        return f"  [{'present' if self.held else 'MISSING'}] {self.what}: {self.detail}"


def inputs(store: Path | None, *, require_upload_key: bool = True) -> tuple[list[Check], dict]:
    checks: list[Check] = []
    facts: dict = {}

    dirty = tree().strip()
    facts["clean"] = not dirty
    facts["tree"] = dirty
    checks.append(
        Check(
            "a clean tree",
            not dirty,
            "nothing uncommitted"
            if not dirty
            else f"{len(dirty.splitlines())} uncommitted paths. A release tags the commit it builds",
        )
    )

    facts["keystore"] = store
    held = bool(store and store.is_file())
    checks.append(
        Check(
            "the upload keystore",
            held,
            "read from the environment's AMUX_ANDROID_KEYSTORE"
            if held and os.environ.get("AMUX_ANDROID_KEYSTORE", "").strip()
            else f"{UPLOAD_KEY}/upload-keystore, from the credential command"
            if held
            else f"{store or 'the credential command'} has no upload keystore "
            "(docs/RELEASE.md, Android one-time setup)",
        )
    )

    alias = secret("AMUX_ANDROID_KEY_ALIAS", "key-alias")
    store_pw = secret("AMUX_ANDROID_STORE_PASSWORD", "store-password")
    key_pw = secret("AMUX_ANDROID_KEY_PASSWORD", "key-password")
    facts["key_alias"] = alias
    facts["store_password"] = store_pw
    facts["key_password"] = key_pw
    missing_creds = [
        name
        for name, val in (
            ("key-alias", alias),
            ("store-password", store_pw),
            ("key-password", key_pw),
        )
        if not val
    ]

    keytool = find_jdk_tool("keytool")
    jarsigner = find_jdk_tool("jarsigner")
    facts["jarsigner"] = jarsigner

    if missing_creds:
        checks.append(
            Check(
                "the keystore credentials",
                False,
                "the credential command (or AMUX_ANDROID_* env) has no "
                + ", ".join(f"{UPLOAD_KEY}/{name}" for name in missing_creds),
            )
        )
    elif held and keytool:
        ok, detail = check_keystore_credentials(keytool, store, alias, store_pw)
        checks.append(Check("the keystore credentials", ok, detail))
    else:
        checks.append(
            Check(
                "the keystore credentials",
                bool(keytool),
                f"key-alias={alias!r} (keystore not present to verify)"
                if keytool
                else "keytool not found (install JDK 21)",
            )
        )

    checks.append(
        Check(
            "jarsigner",
            bool(jarsigner),
            jarsigner or "jarsigner not found (install JDK 21)",
        )
    )

    openssl = shutil.which("openssl")
    checks.append(
        Check(
            "openssl",
            bool(openssl),
            openssl or "openssl not found on PATH",
        )
    )

    facts["service_account"] = None
    try:
        account = service_account()
        facts["service_account"] = account
        checks.append(
            Check(
                "the Google Play service account key",
                True,
                account["client_email"],
            )
        )
    except Refusal as error:
        if require_upload_key:
            checks.append(Check("the Google Play service account key", False, str(error)))
        else:
            checks.append(
                Check(
                    "the Google Play service account key",
                    True,
                    f"skipped (--rehearse / --no-upload); {error}",
                )
            )

    gs_path = REPO_ROOT / GOOGLE_SERVICES_FILE
    checks.append(
        Check(
            "the Firebase google-services.json",
            gs_path.is_file(),
            str(GOOGLE_SERVICES_FILE)
            if gs_path.is_file()
            else f"{GOOGLE_SERVICES_FILE} is missing (download from Firebase Console for FCM push)",
        )
    )

    return checks, facts


def draft_notes(version: str, notes_file: str) -> str:
    if notes_file:
        path = Path(notes_file)
        if not path.is_file():
            raise Refusal(f"--notes-file {path} does not exist")
        text = path.read_text(encoding="utf-8").strip()
        if not text:
            raise Refusal(f"--notes-file {path} is empty")
        return text
    prior = subprocess.run(
        ["git", "describe", "--tags", "--abbrev=0", "--match", "android-v*"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        timeout=60,
    )
    rev_range = f"{prior.stdout.strip()}..HEAD" if prior.returncode == 0 else "HEAD"
    log_cmd = ["git", "log", "--pretty=format:- %s", "-n", "20", rev_range]
    listed = subprocess.run(
        log_cmd,
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        timeout=60,
    )
    subjects = listed.stdout.strip()
    header = f"Android {version}"
    return f"{header}\n\n{subjects}" if subjects else header


def play_notes(text: str) -> str:
    """Trim notes cleanly to Google Play's 500-character per-locale limit."""
    trimmed = text.strip()
    if len(trimmed) <= MAX_PLAY_NOTES_LEN:
        return trimmed
    return trimmed[: MAX_PLAY_NOTES_LEN - 1].rstrip() + "…"


def version_config(version: str, build: int) -> list[str]:
    """The Buck2 options that build `version` (`build`) without writing them."""
    return ["-c", f"amux.android_version_name={version}", "-c", f"amux.android_version_code={build}"]


def run_scope_audit(version: str, build: int) -> None:
    """Prove the bundle ships no debug/test surface or dynamic SQLite."""
    print("running release scope audit", flush=True)
    subprocess.run(
        ["scripts/buck2", "test", "//apps/android/app:scope-audit", *version_config(version, build)],
        cwd=REPO_ROOT,
        check=True,
        timeout=3000,
    )


def write_numbers(version: str, build: int) -> None:
    path = REPO_ROOT / VERSION_FILE
    path.write_text(rewrite_numbers(path.read_text(encoding="utf-8"), version, build), encoding="utf-8")


def built(target: str, version: str, build: int) -> Path:
    """What Buck2 built for `target` at `version` (`build`), fetched from the cache."""
    out = subprocess.run(
        ["sh", str(BUILT), target, *version_config(version, build)],
        cwd=REPO_ROOT,
        check=True,
        capture_output=True,
        text=True,
        timeout=3000,
    )
    return Path(out.stdout.strip())


def sign_command(jarsigner: str, unsigned: Path, signed: Path, facts: dict) -> list[str]:
    """jarsigner signing `unsigned` into `signed` with the upload key, its
    passwords read from the environment rather than the command line."""
    return [
        jarsigner,
        "-keystore",
        str(facts["keystore"]),
        "-storepass:env",
        "AMUX_ANDROID_STORE_PASSWORD",
        "-keypass:env",
        "AMUX_ANDROID_KEY_PASSWORD",
        "-sigalg",
        "SHA256withRSA",
        "-digestalg",
        "SHA-256",
        "-signedjar",
        str(signed),
        str(unsigned),
        facts["key_alias"],
    ]


def build_bundle(version: str, build: int, facts: dict) -> tuple[Path, Path]:
    """Build the bundle at `version` (`build`) and sign it with the upload key.

    Buck2's bundle is unsigned: the upload key never enters the build, so it
    is signed here, into target/android/release. Returns the signed bundle
    and R8's mapping for it."""
    print(f"building signed release bundle for {version} ({build})", flush=True)
    unsigned = built(BUNDLE_TARGET, version, build)
    if not unsigned.is_file():
        raise Refusal(f"Buck2 succeeded but wrote no bundle at {unsigned}")
    mapping = built(BUNDLE_TARGET + "[proguard_text_output]", version, build) / "mapping.txt"
    destination = REPO_ROOT / RELEASE_DIRECTORY
    destination.mkdir(parents=True, exist_ok=True)
    signed = destination / "signed.aab"
    signed.unlink(missing_ok=True)
    env = os.environ.copy()
    env["AMUX_ANDROID_STORE_PASSWORD"] = facts["store_password"]
    env["AMUX_ANDROID_KEY_PASSWORD"] = facts["key_password"]
    subprocess.run(sign_command(facts["jarsigner"], unsigned, signed, facts), env=env, check=True, timeout=600)
    return signed, mapping


def verify_signature(aab: Path, jarsigner: str) -> None:
    print(f"verifying bundle signature ({aab.name})", flush=True)
    # `-strict` rejects self-signed Google Play upload keys and AGP's bundle
    # entry ordering (BundleConfig.pb precedes META-INF/MANIFEST.MF). Plain
    # `-verify` still verifies every signed digest and fails on tampering, and
    # checking for `jar verified.` refuses unsigned bundles (which exit 0).
    result = subprocess.run(
        [jarsigner, "-verify", str(aab)],
        capture_output=True,
        text=True,
        timeout=300,
    )
    if result.returncode != 0 or "jar verified." not in result.stdout:
        detail = (result.stderr or result.stdout).strip()
        raise Refusal(f"bundle signature verification failed: {detail}")


def stage_release_artifacts(aab: Path, mapping: Path, version: str, build: int, message: str) -> Path:
    destination = REPO_ROOT / RELEASE_DIRECTORY / tag_name(version, build)
    if destination.exists():
        shutil.rmtree(destination)
    destination.mkdir(parents=True, exist_ok=True)
    copied_aab = destination / f"amux-android-{version}-b{build}.aab"
    shutil.copy2(aab, copied_aab)
    if mapping.is_file():
        shutil.copy2(mapping, destination / "mapping.txt")
    (destination / "ReleaseNotes.txt").write_text(message + "\n", encoding="utf-8")
    return copied_aab


def upload_to_play(
    aab: Path,
    version: str,
    build: int,
    notes_text: str | None,
    facts: dict,
    *,
    track: str = "internal",
    draft: bool = False,
    changes_not_sent_for_review: bool = False,
) -> None:
    """Upload the signed .aab to Google Play's internal testing track."""
    print(f"uploading {aab.name} to Google Play ({track} track)", flush=True)
    publisher = PlayPublisher(facts["service_account"], package_name=PACKAGE_NAME)
    with publisher.edit(
        commit=True,
        changes_not_sent_for_review=changes_not_sent_for_review,
    ) as edit_id:
        uploaded = publisher.upload_bundle(edit_id, aab)
        returned_code = int(uploaded.get("versionCode", 0))
        if returned_code != build:
            raise Refusal(
                f"Google Play read versionCode {returned_code} from {aab.name}, expected {build}"
            )
        release_entry: dict = {
            "name": f"{version} ({build})",
            "versionCodes": [str(build)],
            "status": "draft" if draft else "completed",
        }
        if notes_text:
            release_entry["releaseNotes"] = [
                {"language": LOCALE, "text": play_notes(notes_text)}
            ]
        publisher.update_track(edit_id, track, [release_entry])
    state = "draft" if draft else "completed"
    print(f"uploaded versionCode {build} ({version}) to Play {track} track ({state})", flush=True)


def commit_and_tag(version: str, build: int, message: str) -> None:
    tag = tag_name(version, build)
    subprocess.run(["git", "add", str(VERSION_FILE)], cwd=REPO_ROOT, check=True, timeout=120)
    subprocess.run(
        ["git", "commit", "-m", f"Android version {version} (build {build})"],
        cwd=REPO_ROOT,
        check=True,
        timeout=120,
    )
    subprocess.run(
        ["git", "tag", "-a", tag, "-m", message],
        cwd=REPO_ROOT,
        check=True,
        timeout=120,
    )
    print(f"tagged {tag}", flush=True)


def publish(version: str, build: int) -> None:
    tag = tag_name(version, build)
    print("pushing the release commit and its tag", flush=True)
    subprocess.run(["git", "push", "origin", "HEAD:main"], cwd=REPO_ROOT, check=True, timeout=300)
    subprocess.run(["git", "push", "origin", tag], cwd=REPO_ROOT, check=True, timeout=300)
    print(f"pushed the release commit and {tag}", flush=True)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--preflight", action="store_true", help="report every input and stop")
    parser.add_argument(
        "--rehearse",
        action="store_true",
        help="build, sign and verify the would-be .aab without writing the tree or tagging",
    )
    parser.add_argument("--version", default="", help="marketing versionName (X.Y.Z)")
    parser.add_argument("--build", type=int, default=0, help="monotonic versionCode")
    parser.add_argument("--notes-file", default="", help="release notes file instead of git log")
    parser.add_argument(
        "--draft",
        action="store_true",
        help="upload the internal-track release as a draft (required on unpublished draft apps)",
    )
    parser.add_argument(
        "--changes-not-sent-for-review",
        action="store_true",
        help="commit the Play edit with changesNotSentForReview=true",
    )
    parser.add_argument(
        "--no-upload",
        action="store_true",
        help="build, sign, verify, commit and tag without uploading to Google Play",
    )
    parser.add_argument(
        "--no-push",
        action="store_true",
        help="commit and tag without pushing HEAD:main or the tag",
    )
    args = parser.parse_args(argv)
    if args.preflight and args.rehearse:
        parser.error("--preflight and --rehearse are different runs")

    # The keystore is on disk for the whole run: it is checked first and
    # signs the bundle at the end.
    with upload_keystore() as store:
        return run(args, store)


def run(args: argparse.Namespace, store: Path | None) -> int:
    require_upload_key = not (args.rehearse or args.no_upload)
    checks, facts = inputs(store, require_upload_key=require_upload_key)

    version_text = (REPO_ROOT / VERSION_FILE).read_text(encoding="utf-8")
    all_tags = git_tags()
    try:
        here_version, here_build = project_numbers(version_text)
        version = next_version(all_tags, here_version, args.version)
        build = next_build(all_tags, here_build, args.build)
        numbering = Check("the next version and build number", True, f"{version} ({build})")
    except Refusal as refused:
        version, build = "", 0
        numbering = Check("the next version and build number", False, str(refused))
    checks.append(numbering)

    print("just android release, on this machine:")
    for check in checks:
        print(check.line())

    missing = [check for check in checks if not check.held and check.what != "a clean tree"]
    if missing:
        print(
            f"{len(missing)} of {len(checks)} not satisfied: "
            + "; ".join(check.what for check in missing)
        )
    elif args.preflight:
        print("everything a release needs is here")
    if args.preflight:
        return 1 if missing else 0
    if missing:
        return 1

    if not args.rehearse and not facts["clean"]:
        print(
            "the tree is not clean; a release tags the commit it builds",
            file=sys.stderr,
        )
        return 1

    message = draft_notes(version, args.notes_file)
    play_release_notes = Path(args.notes_file).read_text(encoding="utf-8").strip() if args.notes_file else None

    # Always audit shipping Rust + DEX scope before touching version numbers or
    # signing an .aab, so a debug surface or dynamic SQLite stops the run early.
    try:
        run_scope_audit(version, build)
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as failed:
        reason = "timed out" if isinstance(failed, subprocess.TimeoutExpired) else "failed"
        print(f"scope-audit {reason}; nothing was modified", file=sys.stderr)
        return 1

    if not args.rehearse:
        write_numbers(version, build)
    step = "bundle"
    try:
        built_aab, mapping = build_bundle(version, build, facts)
        step = "verify"
        verify_signature(built_aab, facts["jarsigner"])
        step = "stage"
        staged_aab = stage_release_artifacts(built_aab, mapping, version, build, message)
        print(f"staged {staged_aab.relative_to(REPO_ROOT)}")
        print(f"release notes beside it in {(staged_aab.parent / 'ReleaseNotes.txt').relative_to(REPO_ROOT)}")
        if not args.rehearse and not args.no_upload:
            step = "upload"
            upload_to_play(
                staged_aab,
                version,
                build,
                play_release_notes,
                facts,
                draft=args.draft,
                changes_not_sent_for_review=args.changes_not_sent_for_review,
            )
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as failed:
        reason = "timed out" if isinstance(failed, subprocess.TimeoutExpired) else "failed"
        if args.rehearse:
            print(f"{step} {reason}; the rehearsal wrote nothing", file=sys.stderr)
        else:
            print(
                f"{step} {reason}, so nothing was committed and no tag was cut. "
                f"{version} ({build}) is written into {VERSION_FILE}; "
                f"undo with `git checkout -- {VERSION_FILE}`",
                file=sys.stderr,
            )
        return 1

    if not args.rehearse:
        commit_and_tag(version, build, message)
        if not args.no_push:
            publish(version, build)
    else:
        changed = tree_changes(facts["tree"], tree())
        if changed:
            print(
                "rehearsal changed the tree, which it must not: " + "; ".join(changed),
                file=sys.stderr,
            )
            return 1
        print(
            "rehearsal: git status --porcelain is what it was before the run, "
            "so nothing was written to the tree, and no tag was cut"
        )

    if args.rehearse:
        print("nothing was uploaded and nothing was pushed")
    elif args.no_upload:
        print(
            "nothing was uploaded"
            + ("" if args.no_push else "; the commit and tag were pushed")
        )
    else:
        print(
            "uploaded to Google Play internal testing"
            + ("" if args.no_push else ", and the commit and tag pushed")
            + f"; nothing was submitted to production (`just android deploy {version}` does that)"
        )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Refusal as refused:
        print(f"refused: {refused}", file=sys.stderr)
        sys.exit(1)
