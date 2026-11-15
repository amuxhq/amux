"""Set the browser extension's version in its manifest.

Chrome takes one to four dot-separated numbers, each at most 65535, and the
Web Store refuses a version not above the one it holds; this checks the form
and that the version goes up from what the manifest says now.
"""

import json
import re
import sys
from pathlib import Path

MANIFEST = Path(__file__).resolve().parent.parent / "js/extension/static/manifest.json"


def parts(version: str) -> tuple[int, ...]:
    if not re.fullmatch(r"\d+(\.\d+){0,3}", version):
        sys.exit(f"{version!r} is not a Chrome extension version: one to four numbers joined by dots")
    numbers = tuple(int(part) for part in version.split("."))
    if any(number > 65535 for number in numbers):
        sys.exit(f"{version!r} has a part over 65535")
    return numbers


def set_version(version: str) -> None:
    manifest = json.loads(MANIFEST.read_text())
    if parts(version) <= parts(manifest["version"]):
        sys.exit(f"{version} is not above the manifest's {manifest['version']}")
    text = MANIFEST.read_text()
    MANIFEST.write_text(text.replace(f'"version": "{manifest["version"]}"', f'"version": "{version}"', 1))
    print(f"manifest version {manifest['version']} -> {version}")


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit("usage: browser_version.py VERSION")
    set_version(sys.argv[1])


if __name__ == "__main__":
    main()
