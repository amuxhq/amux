#!/usr/bin/env python3
"""Promote a released Android build on Google Play, or inspect track status.

`just android release` uploads a signed .aab to Google Play's internal testing
track and tags it `android-v<version>-b<build>`. This tool promotes that
uploaded versionCode to a Play track:

  stable   (default)  `production` track
  preview             `beta` (open testing) track
  closed              `alpha` (closed testing) track
  internal            `internal` testing track

Pass `--track NAME` to target a custom closed-testing track by its Play Console
track ID. Pass `--rollout PCT` (1..99) for a staged rollout, `--notes-file PATH`
for en-US release notes (max 500 characters), `--draft` to keep the release in
draft status, or `--dry-run` to inspect Play Console and say what would change
without committing anything.
"""

from __future__ import annotations

import argparse
from pathlib import Path
import re
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
from googleplay import PACKAGE_NAME, PlayPublisher, Refusal, key_file_path  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parents[3]
TAG_RE = re.compile(r"^android-v(\d+\.\d+\.\d+)-b(\d+)$")
CHANNEL_TO_TRACK = {
    "internal": "internal",
    "closed": "alpha",
    "preview": "beta",
    "stable": "production",
}
TRACK_ORDER = ("internal", "alpha", "beta", "production")
LOCALE = "en-US"
MAX_PLAY_NOTES_LEN = 500


def tagged_build(tags_list: list[str], version: str) -> int:
    """The highest versionCode the tags record for `version`."""
    numbers = [
        int(match.group(2))
        for match in map(TAG_RE.match, (t.strip() for t in tags_list))
        if match and match.group(1) == version
    ]
    if not numbers:
        raise Refusal(
            f"no android-v{version}-b<build> tag records a build of {version}; "
            "`just android release` cuts one"
        )
    return max(numbers)


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


def validate_rollout(rollout: int | None) -> float | None:
    if rollout is None:
        return None
    if rollout < 1 or rollout > 100:
        raise Refusal(f"--rollout {rollout} must be between 1 and 100")
    if rollout == 100:
        return None
    return round(rollout / 100.0, 4)


def validate_notes(notes: str | None) -> str | None:
    if notes is None:
        return None
    trimmed = notes.strip()
    if not trimmed:
        raise Refusal("release notes are empty")
    if len(trimmed) > MAX_PLAY_NOTES_LEN:
        raise Refusal(
            f"release notes are {len(trimmed)} characters; Google Play allows at most "
            f"{MAX_PLAY_NOTES_LEN}"
        )
    return trimmed


def describe_tracks(tracks: list[dict]) -> str:
    if not tracks:
        return "no tracks found"
    order = {name: idx for idx, name in enumerate(TRACK_ORDER)}
    ordered = sorted(tracks, key=lambda t: (order.get(t.get("track", ""), 99), t.get("track", "")))
    lines: list[str] = []
    for track in ordered:
        name = track.get("track", "?")
        releases = track.get("releases") or []
        if not releases:
            lines.append(f"{name}: none")
            continue
        parts: list[str] = []
        for rel in releases:
            codes = ",".join(str(c) for c in rel.get("versionCodes", [])) or "?"
            status = rel.get("status", "?")
            label = rel.get("name")
            fraction = rel.get("userFraction")
            detail = f"{status} {int(round(fraction * 100))}%" if fraction is not None else status
            prefix = f"{label} " if label else ""
            parts.append(f"{prefix}[{codes}] ({detail})")
        lines.append(f"{name}: {'; '.join(parts)}")
    return "\n".join(lines)


def build_release_entry(
    version: str,
    build: int,
    *,
    rollout_fraction: float | None,
    notes: str | None,
    draft: bool,
) -> dict:
    if draft and rollout_fraction is not None:
        raise Refusal("--draft cannot be combined with a staged --rollout below 100%")
    if draft:
        status = "draft"
    elif rollout_fraction is not None:
        status = "inProgress"
    else:
        status = "completed"
    entry: dict = {
        "name": f"{version} ({build})",
        "versionCodes": [str(build)],
        "status": status,
    }
    if rollout_fraction is not None:
        entry["userFraction"] = rollout_fraction
    if notes is not None:
        entry["releaseNotes"] = [{"language": LOCALE, "text": notes}]
    return entry


def status(publisher: PlayPublisher, say=print) -> None:
    with publisher.edit(commit=False) as edit_id:
        tracks = publisher.list_tracks(edit_id)
    say(describe_tracks(tracks))


def deploy(
    publisher: PlayPublisher,
    version: str,
    build: int,
    *,
    track: str,
    rollout: int | None = None,
    notes: str | None = None,
    draft: bool = False,
    changes_not_sent_for_review: bool = False,
    dry_run: bool = False,
    say=print,
) -> None:
    rollout_fraction = validate_rollout(rollout)
    checked_notes = validate_notes(notes)
    entry = build_release_entry(
        version,
        build,
        rollout_fraction=rollout_fraction,
        notes=checked_notes,
        draft=draft,
    )

    with publisher.edit(
        commit=not dry_run,
        changes_not_sent_for_review=changes_not_sent_for_review,
    ) as edit_id:
        bundles = publisher.list_bundles(edit_id)
        available_codes = {int(b["versionCode"]) for b in bundles if "versionCode" in b}
        tracks = publisher.list_tracks(edit_id)
        for existing_track in tracks:
            for rel in existing_track.get("releases") or []:
                for code in rel.get("versionCodes") or []:
                    available_codes.add(int(code))

        if build not in available_codes:
            raise Refusal(
                f"Google Play holds no bundle with versionCode {build} for {publisher.package_name}; "
                "upload it first with `just android release`"
            )

        say(describe_tracks(tracks))
        rollout_desc = (
            f" at {int(round(rollout_fraction * 100))}% rollout"
            if rollout_fraction is not None
            else (" as a draft" if draft else " at 100%")
        )
        notes_desc = " with en-US release notes" if checked_notes is not None else ""
        if dry_run:
            say(f"would promote {version} (build {build}) to {track}{rollout_desc}{notes_desc}")
            return

        publisher.update_track(edit_id, track, [entry])
        publisher.validate_edit(edit_id)

    review_note = (
        "; send for review in Play Console Publishing overview"
        if changes_not_sent_for_review
        else ""
    )
    say(f"promoted {version} (build {build}) to {track}{rollout_desc}{notes_desc}{review_note}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("status", help="show what each Google Play track currently holds")

    put = sub.add_parser("deploy", help="promote a released versionCode to a Google Play track")
    put.add_argument("version")
    put.add_argument(
        "--channel",
        choices=tuple(CHANNEL_TO_TRACK.keys()),
        default="stable",
        help="target channel (stable=production, preview=beta, closed=alpha, internal=internal)",
    )
    put.add_argument(
        "--track",
        default="",
        help="exact Play Console track ID (overrides --channel, e.g. a custom closed track)",
    )
    put.add_argument(
        "--build",
        type=int,
        default=0,
        help="versionCode to promote, instead of the highest android-v<version>-b* tag",
    )
    put.add_argument(
        "--rollout",
        type=int,
        default=None,
        help="staged rollout percentage (1..100; 100 or omitted means full rollout)",
    )
    put.add_argument("--notes-file", type=Path, help="en-US release notes file (max 500 chars)")
    put.add_argument(
        "--draft",
        action="store_true",
        help="create the track release in draft status",
    )
    put.add_argument(
        "--changes-not-sent-for-review",
        action="store_true",
        help="commit the edit with changesNotSentForReview=true",
    )
    put.add_argument("--dry-run", action="store_true", help="read Play Console and change nothing")

    args = parser.parse_args(argv)
    try:
        publisher = PlayPublisher(key_file_path(), package_name=PACKAGE_NAME)
        if args.command == "status":
            status(publisher)
        else:
            version = args.version.removeprefix("v")
            build = args.build or tagged_build(git_tags(), version)
            track = args.track.strip() or CHANNEL_TO_TRACK[args.channel]
            notes = args.notes_file.read_text(encoding="utf-8") if args.notes_file else None
            deploy(
                publisher,
                version,
                build,
                track=track,
                rollout=args.rollout,
                notes=notes,
                draft=args.draft,
                changes_not_sent_for_review=args.changes_not_sent_for_review,
                dry_run=args.dry_run,
            )
            if args.dry_run:
                print("nothing was changed")
    except Refusal as refused:
        print(f"android {args.command}: {refused}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
