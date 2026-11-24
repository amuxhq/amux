#!/usr/bin/env python3
"""Sign a Buck2-built iPhone app for a phone or the App Store, and package it.

A device build comes out of Buck2 unsigned (build/apple.bzl): a signature
names a certificate in this Mac's login keychain and a provisioning profile
installed for Xcode, neither of which is an input a build can declare, and a
release is rare enough that caching its signature buys nothing. This runs
on the Mac, outside the build, the way `xcodebuild -exportArchive` signed an
archive: each bundle inside out, with the profile it is distributed under
embedded and the entitlements that profile allows.

    sign.py APP OUT --identity "Apple Distribution" --team TEAM \\
        --profile sh.amux.app="amux App Store" \\
        --profile sh.amux.app.notifications="amux Notifications App Store" \\
        --entitlements sh.amux.app=apps/apple/Amux/Amux.entitlements \\
        --entitlements sh.amux.app.notifications=apps/apple/AmuxNotifications/AmuxNotifications.entitlements \\
        [--revision SHA] [--ipa OUT.ipa [--symbols DIR]]

The entitlements a bundle is signed with are what its source file asks for,
with the team's prefix filled in, under the profile's values for the keys a
profile decides: the push environment (production for the App Store), the
application identifier and team, and whether a debugger may attach.

`--symbols` packs the symbols of each dSYM in DIR into the .ipa beside the app, as
Xcode's export does with `uploadSymbols`, so App Store Connect can
symbolicate the build's crash reports.
"""

from pathlib import Path
import argparse
import plistlib
import shutil
import subprocess
import sys

PROFILE_DIRECTORIES = (
    Path.home() / "Library/Developer/Xcode/UserData/Provisioning Profiles",
    Path.home() / "Library/MobileDevice/Provisioning Profiles",
)
# Signing waits on the keychain. A keychain that asks a person for access
# would wait forever on an unattended run, so a signature that takes longer
# than this is taken to be waiting on one.
SIGNING_TIMEOUT = 120
# Keys whose value the profile decides rather than the app.
FROM_PROFILE = ("aps-environment", "get-task-allow", "beta-reports-active")


def run(command: list[str], timeout: int = SIGNING_TIMEOUT) -> subprocess.CompletedProcess:
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        sys.exit(f"{command[0]} took more than {timeout} s: it is probably waiting for "
                 "someone to allow access to the signing key in the keychain")
    if result.returncode:
        sys.exit(f"{' '.join(command)} failed:\n{result.stdout}{result.stderr}")
    return result


def profiles() -> list[tuple[Path, dict]]:
    found = []
    for directory in PROFILE_DIRECTORIES:
        for path in sorted(directory.glob("*.mobileprovision")) if directory.is_dir() else []:
            decoded = subprocess.run(["security", "cms", "-D", "-i", str(path)],
                                     capture_output=True, timeout=60)
            if decoded.returncode == 0:
                found.append((path, plistlib.loads(decoded.stdout)))
    return found


def profile_named(name: str, team: str) -> tuple[Path, dict]:
    matching = [(path, profile) for path, profile in profiles()
                if profile.get("Name") == name and team in profile.get("TeamIdentifier", [])]
    if not matching:
        sys.exit(f"no provisioning profile named {name!r} for team {team} is installed "
                 f"(looked in {', '.join(str(d) for d in PROFILE_DIRECTORIES)}); docs/RELEASE.md says where it comes from")
    # The newest, as Xcode picks among profiles of one name.
    return max(matching, key=lambda item: item[1].get("CreationDate"))


def identity(kind: str, team: str) -> str:
    """The certificate of this kind for the team, by its SHA-1: a name alone
    can match two certificates, and codesign then refuses."""
    listed = run(["security", "find-identity", "-v", "-p", "codesigning"]).stdout
    for line in listed.splitlines():
        parts = line.strip().split(" ", 2)
        if len(parts) == 3 and parts[2].startswith(f'"{kind}') and f"({team})" in parts[2]:
            return parts[1]
    sys.exit(f"no valid {kind!r} identity for team {team} in the keychain")


def entitlements(source: Path, team: str, profile: dict) -> dict:
    text = source.read_text().replace("$(AppIdentifierPrefix)", team + ".") \
        .replace("$(TeamIdentifierPrefix)", team + ".")
    wanted = plistlib.loads(text.encode())
    allowed = profile["Entitlements"]
    granted = {
        "application-identifier": allowed["application-identifier"],
        "com.apple.developer.team-identifier": allowed.get("com.apple.developer.team-identifier", team),
    }
    for key, value in wanted.items():
        granted[key] = allowed[key] if key in FROM_PROFILE and key in allowed else value
    for key in FROM_PROFILE:
        if key in allowed and key not in ("aps-environment",):
            granted[key] = allowed[key]
    return granted


def bundle_identifier(bundle: Path) -> str:
    return plistlib.loads((bundle / "Info.plist").read_bytes())["CFBundleIdentifier"]


def sign(bundle: Path, certificate: str, grants: Path | None) -> None:
    command = ["codesign", "--force", "--sign", certificate, "--timestamp=none",
               "--generate-entitlement-der"]
    if grants:
        command += ["--entitlements", str(grants)]
    run([*command, str(bundle)])


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("app")
    parser.add_argument("out", help="the directory the signed app is written to")
    parser.add_argument("--identity", required=True, help='"Apple Distribution" or "Apple Development"')
    parser.add_argument("--team", required=True)
    parser.add_argument("--profile", action="append", default=[], help="BUNDLE_ID=PROFILE NAME")
    parser.add_argument("--entitlements", action="append", default=[], help="BUNDLE_ID=FILE")
    parser.add_argument("--revision", help="the commit, stamped into the app's Info.plist")
    parser.add_argument("--ipa", help="also package the signed app as this .ipa")
    parser.add_argument("--symbols", help="a directory of dSYMs whose symbols the .ipa carries")
    arguments = parser.parse_args()

    wanted_profiles = dict(pair.split("=", 1) for pair in arguments.profile)
    wanted_grants = dict(pair.split("=", 1) for pair in arguments.entitlements)
    out = Path(arguments.out)
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    app = out / Path(arguments.app).name
    # Buck2's outputs are read-only, and the copy is signed in place.
    shutil.copytree(arguments.app, app, symlinks=True)
    subprocess.run(["chmod", "-R", "u+w", str(app)], check=True, timeout=120)

    if arguments.revision:
        # Which commit a report from this build came from (Composition.swift).
        info = app / "Info.plist"
        values = plistlib.loads(info.read_bytes())
        values["AmuxGitSHA"] = arguments.revision
        info.write_bytes(plistlib.dumps(values))

    certificate = identity(arguments.identity, arguments.team)
    plugins = sorted((app / "PlugIns").glob("*.appex")) if (app / "PlugIns").is_dir() else []
    for nested in sorted(app.glob("*.bundle")):
        sign(nested, certificate, None)
    for bundle in [*plugins, app]:
        identifier = bundle_identifier(bundle)
        if identifier not in wanted_profiles:
            sys.exit(f"no profile named for {identifier} (--profile {identifier}=NAME)")
        path, profile = profile_named(wanted_profiles[identifier], arguments.team)
        shutil.copyfile(path, bundle / "embedded.mobileprovision")
        grants = None
        if identifier in wanted_grants:
            grants = out / f"{identifier}.entitlements"
            grants.write_bytes(plistlib.dumps(
                entitlements(Path(wanted_grants[identifier]), arguments.team, profile)))
        sign(bundle, certificate, grants)
    run(["codesign", "--verify", "--deep", "--strict", str(app)])
    print(f"signed {app} as {arguments.identity} ({arguments.team})")

    if arguments.ipa:
        # The .ipa's root holds Payload/ with the app, and Symbols/ when
        # there are any.
        staging = out / "ipa"
        payload = staging / "Payload"
        payload.mkdir(parents=True)
        shutil.move(str(app), payload / app.name)
        dsyms = sorted(Path(arguments.symbols).glob("*.dSYM")) if arguments.symbols else []
        if dsyms:
            symbols = staging / "Symbols"
            symbols.mkdir()
            for dsym in dsyms:
                run(["xcrun", "symbols", "-noTextInSOD", "-noDaemon", "-arch", "all",
                     "-symbolsPackageDir", str(symbols), str(dsym)], timeout=600)
        ipa = Path(arguments.ipa)
        ipa.unlink(missing_ok=True)
        run(["ditto", "-c", "-k", "--sequesterRsrc", str(staging), str(ipa)], timeout=600)
        shutil.move(str(payload / app.name), app)
        shutil.rmtree(staging)
        print(f"packaged {ipa}" + (f" with the symbols of {len(dsyms)} binaries" if dsyms else ""))


if __name__ == "__main__":
    main()
