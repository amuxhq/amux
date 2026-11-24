#!/usr/bin/env python3
"""Check that the documentation under docs/ holds together.

- Every relative link and image in docs/**/*.md resolves: the file exists,
  it is not in the untracked notes/ or target/ directories, and, for a
  `#fragment` into a Markdown page, the page has that heading.
- Every file under docs/figures is referenced by some page.
- Every SVG under docs/ is a standalone drawing: it parses as XML, uses no
  CSS variable and no currentColor (both depend on the page embedding it,
  which GitHub's image proxy never provides), and styles any class it uses
  with a <style> element of its own.
- The vocabulary PNGs match the hashes in their manifest, so a stale or
  hand-edited rendering is caught.
- docs/README.md links every page in docs/ exactly once.
- A fenced block under a `<!-- source: <path> -->` line is that file, word
  for word: a page that shows a compiled example cannot drift from it.
- No page or figure names a system or tool the design drew on, or the
  crate name of a tool amux runs: the docs describe amux in its own words,
  and a tool's name belongs in the comment of the recipe that runs it. A
  recipe invoked by name (`just miri`) is not a mention.

    docs-check.py [--tracked LISTING]

`--tracked` names a file listing the checkout's files, one path a line,
which links are resolved against instead of the filesystem: how the check
runs as a Buck2 test (build/checks.bzl), where only docs/ and the files
whose content it reads are present.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import sys
from urllib.parse import unquote
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
DOCS = ROOT / "docs"
FIGURES = DOCS / "figures"
VOCABULARY = FIGURES / "vocabulary" / "manifest.json"

# With --tracked, the checkout's files and the directories holding them, by
# their paths in it.
TRACKED: set[str] | None = None
TRACKED_DIRS: set[str] = set()


def exists(path: Path, directory: bool = True) -> bool:
    """Whether the checkout has `path` (a file, or a directory unless
    `directory` is false)."""
    if TRACKED is None or not path.is_relative_to(ROOT):
        return path.exists() if directory else path.is_file()
    relative = path.relative_to(ROOT).as_posix()
    return relative in TRACKED or (directory and (relative in TRACKED_DIRS or relative == "."))


def readable(path: Path, where: str, errors: list[str]) -> bool:
    """Whether a file whose content the check reads is present; under
    --tracked one outside docs/ is present only when the check's target
    declares it."""
    if path.is_file():
        return True
    errors.append(f"{where}: {path.relative_to(ROOT)} is tracked but not an input of the docs check; add it to //:docs-check's srcs")
    return False

# The systems and tools the design drew on, and the crate names of the tools
# amux runs, matched as whole words in any case.
VENDOR_NAMES = [
    "Stateright", "TLA+", "TigerBeetle", "FoundationDB", "Antithesis", "Jepsen", "Loom",
    "Shuttle", "Kani", "Verus", "Creusot", "Prusti", "Elle", "Maelstrom",
    "proptest", "cargo-fuzz", "libFuzzer", "Miri", "cargo-llvm-cov", "llvm-cov",
    "cargo-deny", "cargo-shear", "AddressSanitizer", "ThreadSanitizer",
]
VENDOR = re.compile(
    r"(?<![\w-])(?:" + "|".join(re.escape(name) for name in VENDOR_NAMES) + r")(?![\w-])",
    re.I,
)
RECIPE = re.compile(r"\bjust\s+[\w-]+")

FENCE = re.compile(r"^\s*(```|~~~)")
INLINE_CODE = re.compile(r"`+[^`]*`+")
# [text](target "title") and ![alt](target); the text may hold one level of
# brackets so a linked image inside a link is read too.
INLINE_LINK = re.compile(r"!?\[(?:[^\[\]]|\[[^\[\]]*\])*\]\(\s*<?([^)\s>]+)>?(?:\s+\"[^\"]*\")?\s*\)")
REFERENCE_DEF = re.compile(r"^\s{0,3}\[[^\]]+\]:\s*<?(\S+?)>?(?:\s+.*)?$")
HTML_REF = re.compile(r"<(?:img|a|source)\b[^>]*?\b(?:src|href|srcset)=\"([^\"]+)\"", re.I)
HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
HTML_ANCHOR = re.compile(r"<a\s+(?:name|id)=\"([^\"]+)\"", re.I)


def prose_lines(text: str) -> list[tuple[int, str]]:
    """The page's lines outside fenced code, with inline code blanked."""
    lines = []
    fenced = False
    for number, line in enumerate(text.splitlines(), 1):
        if FENCE.match(line):
            fenced = not fenced
            continue
        if not fenced:
            lines.append((number, INLINE_CODE.sub("", line)))
    return lines


def slug(heading: str) -> str:
    """GitHub's anchor for a heading."""
    text = re.sub(r"<[^>]+>", "", heading)
    text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = text.replace("`", "").strip().lower()
    text = re.sub(r"[^\w\- ]", "", text)
    return text.replace(" ", "-")


def anchors(page: Path) -> set[str]:
    found: set[str] = set()
    seen: dict[str, int] = {}
    for _, line in prose_lines(page.read_text(encoding="utf-8")):
        for name in HTML_ANCHOR.findall(line):
            found.add(name)
        match = HEADING.match(line)
        if not match:
            continue
        base = slug(match.group(2))
        count = seen.get(base, 0)
        seen[base] = count + 1
        found.add(base if count == 0 else f"{base}-{count}")
    return found


def targets(page: Path) -> list[tuple[int, str]]:
    found = []
    for number, line in prose_lines(page.read_text(encoding="utf-8")):
        for pattern in (INLINE_LINK, HTML_REF):
            found.extend((number, target) for target in pattern.findall(line))
        match = REFERENCE_DEF.match(line)
        if match:
            found.append((number, match.group(1)))
    return found


def is_external(target: str) -> bool:
    return bool(re.match(r"^[a-zA-Z][a-zA-Z0-9+.-]*:", target)) or target.startswith("//")


def check_links(pages: list[Path], errors: list[str]) -> set[Path]:
    referenced: set[Path] = set()
    anchor_cache: dict[Path, set[str]] = {}
    for page in pages:
        where = page.relative_to(ROOT)
        for number, target in targets(page):
            if is_external(target):
                continue
            path_part, _, fragment = target.partition("#")
            resolved = (page.parent / unquote(path_part)).resolve() if path_part else page
            if not exists(resolved):
                errors.append(f"{where}:{number}: {target} does not resolve")
                continue
            if not resolved.is_relative_to(ROOT) or resolved.is_relative_to(ROOT / "notes") or resolved.is_relative_to(ROOT / "target"):
                errors.append(f"{where}:{number}: {target} leaves the repository's tracked tree")
                continue
            referenced.add(resolved)
            if fragment and resolved.suffix == ".md":
                if not readable(resolved, f"{where}:{number}", errors):
                    continue
                known = anchor_cache.setdefault(resolved, anchors(resolved))
                if fragment not in known:
                    errors.append(f"{where}:{number}: {target} names no heading in {resolved.relative_to(ROOT)}")
    return referenced


def check_figures(referenced: set[Path], errors: list[str]) -> None:
    for path in sorted(FIGURES.rglob("*")):
        if path.is_file() and path.resolve() not in referenced:
            errors.append(f"{path.relative_to(ROOT)} is not referenced by any page")


def check_svgs(errors: list[str]) -> None:
    for path in sorted(DOCS.rglob("*.svg")):
        where = path.relative_to(ROOT)
        text = path.read_text(encoding="utf-8")
        try:
            root = ET.fromstring(text)
        except ET.ParseError as error:
            errors.append(f"{where}: not well-formed XML: {error}")
            continue
        if "var(--" in text:
            errors.append(f"{where}: uses a CSS variable")
        if "currentColor" in text:
            errors.append(f"{where}: uses currentColor")
        styled = any(element.tag.endswith("}style") or element.tag == "style" for element in root.iter())
        classed = any("class" in element.attrib for element in root.iter())
        if classed and not styled:
            errors.append(f"{where}: uses class attributes without an embedded <style>")


def check_vocabulary(errors: list[str]) -> None:
    if not VOCABULARY.exists():
        return
    manifest = json.loads(VOCABULARY.read_text(encoding="utf-8"))
    listed = set()
    for entry in manifest["entries"]:
        path = VOCABULARY.parent / entry["file"]
        listed.add(path.name)
        if not path.exists():
            errors.append(f"{VOCABULARY.relative_to(ROOT)}: {entry['file']} is missing")
            continue
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        if digest != entry["sha256"]:
            errors.append(f"{path.relative_to(ROOT)}: hash differs from the manifest; re-render with `{manifest['command']}`")
    for path in VOCABULARY.parent.glob("*.png"):
        if path.name not in listed:
            errors.append(f"{path.relative_to(ROOT)} is not in the vocabulary manifest")


def check_readme(errors: list[str]) -> None:
    readme = DOCS / "README.md"
    pages = {path.name for path in DOCS.glob("*.md") if path.name != "README.md"}
    counts: dict[str, int] = {}
    for _, target in targets(readme):
        if is_external(target):
            continue
        path_part = target.partition("#")[0]
        if path_part and "/" not in path_part and path_part.endswith(".md"):
            counts[path_part] = counts.get(path_part, 0) + 1
    for page in sorted(pages):
        if counts.get(page, 0) != 1:
            errors.append(f"docs/README.md links {page} {counts.get(page, 0)} times; it should link it once")


SOURCE = re.compile(r"^<!-- source: (\S+) -->$")


def check_sources(pages: list[Path], errors: list[str]) -> None:
    for page in pages:
        lines = page.read_text(encoding="utf-8").splitlines()
        for number, line in enumerate(lines, 1):
            marker = SOURCE.match(line)
            if not marker:
                continue
            where = f"{page.relative_to(ROOT)}:{number}"
            source = ROOT / marker.group(1)
            if number >= len(lines) or not FENCE.match(lines[number]):
                errors.append(f"{where}: a source line must sit just above a fenced block")
                continue
            end = next((i for i in range(number + 1, len(lines)) if FENCE.match(lines[i])), None)
            if end is None:
                errors.append(f"{where}: the fenced block never closes")
            elif not exists(source, directory=False):
                errors.append(f"{where}: {marker.group(1)} does not exist")
            elif not readable(source, where, errors):
                pass
            elif lines[number + 1:end] != source.read_text(encoding="utf-8").splitlines():
                errors.append(f"{where}: the block differs from {marker.group(1)}; copy the file in again")


def check_vendor_names(errors: list[str]) -> None:
    for path in sorted([*DOCS.rglob("*.md"), *DOCS.rglob("*.svg")]):
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            for name in VENDOR.findall(RECIPE.sub("", line)):
                errors.append(
                    f"{path.relative_to(ROOT)}:{number}: names {name!r}; describe it in amux's own words"
                )


def main() -> int:
    global TRACKED
    parser = argparse.ArgumentParser()
    parser.add_argument("--tracked", type=Path, help="a listing of the checkout's files to resolve links against")
    arguments = parser.parse_args()
    if arguments.tracked:
        TRACKED = set(arguments.tracked.read_text(encoding="utf-8").splitlines())
        for path in TRACKED:
            parts = path.split("/")[:-1]
            TRACKED_DIRS.update("/".join(parts[:end]) for end in range(1, len(parts) + 1))
    errors: list[str] = []
    pages = sorted(DOCS.rglob("*.md"))
    referenced = check_links(pages, errors)
    check_figures(referenced, errors)
    check_svgs(errors)
    check_vocabulary(errors)
    check_readme(errors)
    check_sources(pages, errors)
    check_vendor_names(errors)
    for error in errors:
        print(error, file=sys.stderr)
    if errors:
        print(f"docs check: {len(errors)} problem(s)", file=sys.stderr)
        return 1
    print(f"docs check: {len(pages)} pages, every link, figure and page accounted for, every shown source current, no vendor named")
    return 0


if __name__ == "__main__":
    sys.exit(main())
