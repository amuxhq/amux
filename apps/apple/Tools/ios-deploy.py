#!/usr/bin/env python3
"""Put a released iPhone build in front of people.

`just ios release` uploads a build and tags it `ios-v<version>-b<build>`;
this takes that build to the people a channel names, through the App Store
Connect API:

  stable (the default)  the version's App Store listing: the build attached,
                        released as soon as review approves it, with no
                        phased release, and submitted for review.
  preview               the external TestFlight group, after Apple's beta
                        review when the build needs one.

What's New (or TestFlight's What to Test) comes from --notes-file, else
whatever App Store Connect already holds; a stable deploy refuses a listing
with none, since the tag's commit list is not written for the App Store.

  --dry-run   read App Store Connect, say what would change, change nothing.

The key is the one `just ios release` signs in with (docs/RELEASE.md).
"""

from __future__ import annotations

from pathlib import Path
import argparse
import re
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
from appstoreconnect import Connect, Refusal, app, signed_in  # noqa: E402

TAG = re.compile(r"^ios-v(\d+\.\d+\.\d+)-b(\d+)$")
# The states in which a version can still take a build and be submitted.
EDITABLE = {"PREPARE_FOR_SUBMISSION", "DEVELOPER_REJECTED", "REJECTED",
            "METADATA_REJECTED", "INVALID_BINARY"}
LOCALE = "en-US"


def tagged_build(tags: list[str], version: str) -> int:
    """The highest build number the tags record for `version`."""
    numbers = [int(match.group(2)) for match in map(TAG.match, tags)
               if match and match.group(1) == version]
    if not numbers:
        raise Refusal(f"no ios-v{version}-b<build> tag records a build of {version}; "
                      "`just ios release` cuts one")
    return max(numbers)


def tags() -> list[str]:
    listed = subprocess.run(["git", "tag", "--list", "ios-v*"],
                            capture_output=True, text=True, timeout=60)
    return listed.stdout.splitlines()


def build(connect: Connect, app_id: str, version: str, number: int) -> str:
    """The id of the uploaded build, once Apple has processed it."""
    found = connect.call(
        "GET", f"/v1/builds?filter[app]={app_id}&filter[version]={number}"
               f"&filter[preReleaseVersion.version]={version}&limit=1")["data"]
    if not found:
        raise Refusal(f"App Store Connect has no build {number} of {version}; "
                      "an upload shows up once Apple has received it")
    attributes = found[0]["attributes"]
    if attributes.get("processingState") != "VALID":
        raise Refusal(f"build {number} of {version} is {attributes.get('processingState')}; "
                      "deploy it once Apple has processed it")
    if attributes.get("expired"):
        raise Refusal(f"build {number} of {version} has expired")
    return found[0]["id"]


def store(connect: Connect, app_id: str, version: str, build_id: str,
          notes: str | None, dry_run: bool, say=print) -> None:
    """Attach the build to the version's listing and submit it for review."""
    versions = connect.call(
        "GET", f"/v1/apps/{app_id}/appStoreVersions?filter[platform]=IOS"
               f"&filter[versionString]={version}")["data"]
    if versions:
        version_id = versions[0]["id"]
        state = versions[0]["attributes"]["appStoreState"]
        if state not in EDITABLE:
            raise Refusal(f"version {version} is {state}; it can no longer be submitted")
    elif notes is None:
        # A version made now would have no What's New, so the refusal below
        # comes before it is made.
        raise Refusal(f"version {version} is new and has no What's New; pass --notes-file")
    elif dry_run:
        version_id = ""
        say(f"would create App Store version {version}")
    else:
        version_id = connect.call("POST", "/v1/appStoreVersions", {"data": {
            "type": "appStoreVersions",
            "attributes": {"platform": "IOS", "versionString": version},
            "relationships": {"app": {"data": {"type": "apps", "id": app_id}}}}})["data"]["id"]
        say(f"created App Store version {version}")

    localizations = connect.call(
        "GET", f"/v1/appStoreVersions/{version_id}/appStoreVersionLocalizations")["data"] \
        if version_id else []
    if notes is None:
        missing = [loc["attributes"]["locale"] for loc in localizations
                   if not (loc["attributes"].get("whatsNew") or "").strip()]
        if missing or not localizations:
            raise Refusal(f"version {version} has no What's New for "
                          f"{', '.join(missing) or LOCALE}; write it in App Store "
                          "Connect or pass --notes-file")
    if dry_run:
        say(f"would attach the build to version {version}, release it once approved "
            "with no phased release" + (", set What's New" if notes is not None else "")
            + " and submit it for review")
        return

    connect.call("PATCH", f"/v1/appStoreVersions/{version_id}", {"data": {
        "type": "appStoreVersions", "id": version_id,
        "attributes": {"releaseType": "AFTER_APPROVAL"}}})
    connect.call("PATCH", f"/v1/appStoreVersions/{version_id}/relationships/build",
                 {"data": {"type": "builds", "id": build_id}})
    phased = connect.call(
        "GET", f"/v1/appStoreVersions/{version_id}/appStoreVersionPhasedRelease").get("data")
    if phased:
        connect.call("DELETE", f"/v1/appStoreVersionPhasedReleases/{phased['id']}")
    if notes is not None:
        for loc in localizations:
            connect.call("PATCH", f"/v1/appStoreVersionLocalizations/{loc['id']}", {"data": {
                "type": "appStoreVersionLocalizations", "id": loc["id"],
                "attributes": {"whatsNew": notes}}})
    say(f"attached the build to version {version}")

    # A submission not yet sent, or one review sent back, is the one to send:
    # a run that stopped after adding the version, or a resubmission after a
    # rejection, carries on with it rather than starting another.
    open_ = connect.call(
        "GET", f"/v1/reviewSubmissions?filter[app]={app_id}&filter[platform]=IOS"
               "&filter[state]=READY_FOR_REVIEW,UNRESOLVED_ISSUES")["data"]
    if open_:
        submission = open_[0]["id"]
    else:
        submission = connect.call("POST", "/v1/reviewSubmissions", {"data": {
            "type": "reviewSubmissions", "attributes": {"platform": "IOS"},
            "relationships": {"app": {"data": {"type": "apps", "id": app_id}}}}})["data"]["id"]
    items = connect.call(
        "GET", f"/v1/reviewSubmissions/{submission}/items?include=appStoreVersion") if open_ \
        else {"data": []}
    held = any(((item.get("relationships") or {}).get("appStoreVersion") or {})
               .get("data", {}) == {"type": "appStoreVersions", "id": version_id}
               for item in items["data"])
    if not held:
        connect.call("POST", "/v1/reviewSubmissionItems", {"data": {
            "type": "reviewSubmissionItems",
            "relationships": {
                "reviewSubmission": {"data": {"type": "reviewSubmissions", "id": submission}},
                "appStoreVersion": {"data": {"type": "appStoreVersions", "id": version_id}}}}})
    connect.call("PATCH", f"/v1/reviewSubmissions/{submission}", {"data": {
        "type": "reviewSubmissions", "id": submission, "attributes": {"submitted": True}}})
    say(f"submitted version {version} for review; it is released once approved")


def testflight(connect: Connect, app_id: str, build_id: str, notes: str | None,
               group: str | None, dry_run: bool, say=print) -> None:
    """Give the build to the external TestFlight group."""
    groups = [found for found in connect.call(
        "GET", f"/v1/apps/{app_id}/betaGroups?limit=200")["data"]
        if not found["attributes"].get("isInternalGroup")]
    names = [found["attributes"]["name"] for found in groups]
    chosen = [found for found in groups if group in (None, found["attributes"]["name"])]
    if len(chosen) != 1:
        raise Refusal(("no external TestFlight group is named " + repr(group) if group else
                       "the app has no external TestFlight group" if not groups else
                       "the app has several external TestFlight groups; name one with --group")
                      + (f" ({', '.join(names)})" if names else ""))
    name, group_id = chosen[0]["attributes"]["name"], chosen[0]["id"]
    state = connect.call("GET", f"/v1/builds/{build_id}/buildBetaDetail")["data"] \
        ["attributes"]["externalBuildState"]
    review = state == "READY_FOR_BETA_SUBMISSION"
    if dry_run:
        say(f"would give the build to {name}" + (", set What to Test" if notes is not None else "")
            + (" and submit it for beta review" if review else f" (it is {state})"))
        return
    if notes is not None:
        found = connect.call("GET", f"/v1/builds/{build_id}/betaBuildLocalizations")["data"]
        mine = next((loc for loc in found if loc["attributes"]["locale"] == LOCALE), None)
        if mine:
            connect.call("PATCH", f"/v1/betaBuildLocalizations/{mine['id']}", {"data": {
                "type": "betaBuildLocalizations", "id": mine["id"],
                "attributes": {"whatsNew": notes}}})
        else:
            connect.call("POST", "/v1/betaBuildLocalizations", {"data": {
                "type": "betaBuildLocalizations",
                "attributes": {"locale": LOCALE, "whatsNew": notes},
                "relationships": {"build": {"data": {"type": "builds", "id": build_id}}}}})
    connect.call("POST", f"/v1/betaGroups/{group_id}/relationships/builds",
                 {"data": [{"type": "builds", "id": build_id}]})
    say(f"gave the build to {name}")
    if review:
        connect.call("POST", "/v1/betaAppReviewSubmissions", {"data": {
            "type": "betaAppReviewSubmissions",
            "relationships": {"build": {"data": {"type": "builds", "id": build_id}}}}})
        say("submitted it for beta review; testers get it once Apple approves")


def deploy(connect: Connect, version: str, number: int, channel: str, notes: str | None,
           group: str | None, dry_run: bool, say=print) -> None:
    app_id = app(connect)
    build_id = build(connect, app_id, version, number)
    say(f"build {number} of {version} is processed")
    if channel == "stable":
        store(connect, app_id, version, build_id, notes, dry_run, say)
    else:
        testflight(connect, app_id, build_id, notes, group, dry_run, say)


def main(arguments: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("version")
    parser.add_argument("--channel", choices=("stable", "preview"), default="stable")
    parser.add_argument("--build", type=int, default=0,
                        help="the build number, instead of the highest the tags record")
    parser.add_argument("--notes-file", type=Path)
    parser.add_argument("--group", help="the external TestFlight group, when there are several")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(arguments)
    try:
        version = args.version.removeprefix("v")
        number = args.build or tagged_build(tags(), version)
        notes = args.notes_file.read_text().strip() if args.notes_file else None
        deploy(signed_in(), version, number, args.channel, notes, args.group, args.dry_run)
    except Refusal as refusal:
        print(f"ios-deploy: {refusal}", file=sys.stderr)
        return 1
    if args.dry_run:
        print("nothing was changed")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
