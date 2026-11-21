#!/usr/bin/env python3
"""Check Android string resources against copy rules and verify noncopy.json.

Enforces the mechanical punctuation rules from docs/IOS_COPY.md on every
strings.xml resource under apps/android/ (no exclamation marks, straight double
quotes, straight apostrophes, three-dot ellipses, em dashes, or semicolons),
enforces duplicate-name and empty-entry checks across <string>, <string-array>,
and <plurals>, and flags unreviewed hardcoded UI string literals in Compose
feature sources (`Text("...")`, `contentDescription = "..."`) unless an exact,
reasoned exemption is recorded in apps/android/tools/noncopy.json. Also
validates noncopy.json's schema and fails on missing files or stale counts.
"""

from __future__ import annotations

import argparse
from collections import Counter
import json
from pathlib import Path
import re
import sys
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[3]
EXEMPTIONS = Path("apps/android/tools/noncopy.json")
FEATURES_SOURCES = Path("apps/android/features/src/main")

FORBIDDEN_PUNCTUATION = (
    ("!", "exclamation mark"),
    ('"', "straight double quote (use curly quotes)"),
    ("'", "straight apostrophe (use curly apostrophe ’)"),
    ("...", "three-dot ellipsis (use …)"),
    ("—", "em dash"),
    (";", "semicolon"),
)

_ANDROID_XML_ESCAPE_RE = re.compile(r"\\([\\'\"nt])")
_UI_LITERAL_PATTERNS = (
    (
        "Text",
        re.compile(r'(?<![A-Za-z0-9_])Text\s*\(\s*(?:text\s*=\s*)?("((?:[^"\\]|\\.)*)")'),
    ),
    (
        "contentDescription",
        re.compile(r'(?<![A-Za-z0-9_])contentDescription\s*=\s*("((?:[^"\\]|\\.)*)")'),
    ),
)


def unescape_android_string(text: str) -> str:
    """Decode Android string resource backslash escapes for copy inspection."""
    mapping = {
        "\\": "\\",
        "'": "'",
        '"': '"',
        "n": "\n",
        "t": "\t",
    }
    return _ANDROID_XML_ESCAPE_RE.sub(lambda m: mapping[m.group(1)], text)


def strip_kotlin_comments(source: str) -> str:
    """Replace // and /* ... */ comments with spaces while keeping line breaks."""
    out: list[str] = []
    i = 0
    n = len(source)
    in_block = False
    in_str = False
    while i < n:
        ch = source[i]
        nxt = source[i + 1] if i + 1 < n else ""
        if in_block:
            if ch == "*" and nxt == "/":
                out.append("  ")
                in_block = False
                i += 2
            elif ch == "\n":
                out.append("\n")
                i += 1
            else:
                out.append(" ")
                i += 1
        elif in_str:
            out.append(ch)
            if ch == "\\" and i + 1 < n:
                out.append(nxt)
                i += 2
            else:
                if ch == '"':
                    in_str = False
                i += 1
        elif ch == '"':
            in_str = True
            out.append(ch)
            i += 1
        elif ch == "/" and nxt == "*":
            out.append("  ")
            in_block = True
            i += 2
        elif ch == "/" and nxt == "/":
            while i < n and source[i] != "\n":
                out.append(" ")
                i += 1
        else:
            out.append(ch)
            i += 1
    return "".join(out)


def read_json(path: Path):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"{path}: duplicate key {key!r}")
            result[key] = value
        return result

    return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=unique)


def load_exemptions(root: Path, exemptions_path: Path = EXEMPTIONS) -> dict[tuple[str, str, str], int]:
    full = root / exemptions_path
    data = read_json(full)
    if not isinstance(data, dict) or data.get("version") != 1:
        raise ValueError(f"{exemptions_path}: unsupported non-copy inventory version")
    files = data.get("files")
    if not isinstance(files, dict):
        raise ValueError(f"{exemptions_path}: 'files' must be an object")

    exemptions: dict[tuple[str, str, str], int] = {}
    for rel_path, entries in files.items():
        if not isinstance(rel_path, str) or not (
            rel_path.endswith(".xml") or rel_path.endswith(".kt")
        ):
            raise ValueError(f"{exemptions_path}: unsupported source path {rel_path!r}")
        if not (root / rel_path).is_file():
            raise ValueError(f"{exemptions_path}: non-copy inventory names a missing source: {rel_path}")
        if not isinstance(entries, list):
            raise ValueError(f"{rel_path}: exemptions must be a list")
        for entry in entries:
            if not isinstance(entry, dict):
                raise ValueError(f"{rel_path}: exemption entry must be an object")
            reason = entry.get("reason")
            if not isinstance(reason, str) or not reason.strip():
                raise ValueError(f"{rel_path}: non-copy exemption has no reason")
            count = entry.get("count")
            if type(count) is not int or count < 1:
                raise ValueError(f"{rel_path}: non-copy count must be a positive integer")
            literal = entry.get("literal")
            context = entry.get("context")
            if not isinstance(literal, str) or not literal:
                raise ValueError(f"{rel_path}: non-copy literal must be a non-empty string")
            if not isinstance(context, str) or not context.strip():
                raise ValueError(f"{rel_path}: non-copy context must be a non-empty string")
            identity = (rel_path, literal, context.strip())
            if identity in exemptions:
                raise ValueError(f"{rel_path}: duplicate non-copy exemption for {literal!r}")
            exemptions[identity] = count
    return exemptions


def strings_xml_files(root: Path) -> list[Path]:
    android_root = root / "apps/android"
    if not android_root.is_dir():
        return []
    found = [
        path
        for path in android_root.rglob("strings*.xml")
        if path.parent.name.startswith("values")
        and "build" not in path.relative_to(android_root).parts
        and ".gradle" not in path.relative_to(android_root).parts
    ]
    return sorted(found)


def feature_kotlin_files(root: Path) -> list[Path]:
    features_root = root / FEATURES_SOURCES
    if not features_root.is_dir():
        return []
    return sorted(features_root.rglob("*.kt"))


def _find_line_context(lines: list[str], tag: str, name: str | None, raw_text: str) -> str:
    if name is not None:
        needle = f'name="{name}"'
        for line in lines:
            if needle in line:
                return line.strip()
    for line in lines:
        if f"<{tag}" in line and raw_text in line:
            return line.strip()
    return raw_text.strip()


def _check_punctuation(value: str) -> list[str]:
    issues = []
    for mark, description in FORBIDDEN_PUNCTUATION:
        if mark in value:
            issues.append(description)
    return issues


def check(root: Path = ROOT) -> tuple[list[str], str]:
    exemptions = load_exemptions(root)
    used: Counter[tuple[str, str, str]] = Counter()
    complaints: list[str] = []

    xml_files = strings_xml_files(root)
    if not xml_files:
        raise ValueError("no strings.xml resources found under apps/android/")

    entry_count = 0
    seen_by_dir: dict[Path, dict[str, set[str]]] = {}
    for xml_file in xml_files:
        rel_path = xml_file.relative_to(root).as_posix()
        raw_content = xml_file.read_text(encoding="utf-8")
        lines = raw_content.splitlines()
        try:
            tree = ET.fromstring(raw_content)
        except ET.ParseError as error:
            raise ValueError(f"{rel_path}: malformed XML: {error}") from error

        if tree.tag != "resources":
            raise ValueError(f"{rel_path}: expected <resources> root element")

        seen_names = seen_by_dir.setdefault(
            xml_file.parent,
            {
                "string": set(),
                "string-array": set(),
                "plurals": set(),
            },
        )
        for elem in tree:
            if elem.tag == "string":
                name = elem.attrib.get("name")
                if not name:
                    complaints.append(f"{rel_path}: <string> element missing name attribute")
                    continue
                if name in seen_names["string"]:
                    complaints.append(f"{rel_path}: duplicate string key {name!r}")
                seen_names["string"].add(name)

                raw_text = "".join(elem.itertext())
                value = unescape_android_string(raw_text)
                entry_count += 1
                context = _find_line_context(lines, "string", name, raw_text)
                identity = (rel_path, value, context)
                if identity in exemptions:
                    used[identity] += 1
                    continue

                if not value.strip():
                    complaints.append(f"{rel_path}: string {name!r} is empty")
                    continue
                for issue in _check_punctuation(value):
                    complaints.append(
                        f"{rel_path}: string {name!r} ({value!r}) violates copy rules: {issue}"
                    )
            elif elem.tag in ("string-array", "plurals"):
                name = elem.attrib.get("name")
                if not name:
                    complaints.append(f"{rel_path}: <{elem.tag}> element missing name attribute")
                    continue
                if name in seen_names[elem.tag]:
                    complaints.append(f"{rel_path}: duplicate {elem.tag} key {name!r}")
                seen_names[elem.tag].add(name)

                items = elem.findall("item")
                if not items:
                    complaints.append(f"{rel_path}: {elem.tag} {name!r} has no items")
                    continue
                for idx, item in enumerate(items):
                    raw_text = "".join(item.itertext())
                    value = unescape_android_string(raw_text)
                    entry_count += 1
                    context = _find_line_context(lines, "item", None, raw_text)
                    identity = (rel_path, value, context)
                    if identity in exemptions:
                        used[identity] += 1
                        continue
                    if not value.strip():
                        complaints.append(f"{rel_path}: {elem.tag} {name!r}[{idx}] is empty")
                        continue
                    for issue in _check_punctuation(value):
                        complaints.append(
                            f"{rel_path}: {elem.tag} {name!r}[{idx}] ({value!r}) violates copy rules: {issue}"
                        )

    # Scan Compose feature sources for hardcoded user-facing string literals.
    kt_files = feature_kotlin_files(root)
    for kt_file in kt_files:
        rel_path = kt_file.relative_to(root).as_posix()
        raw_content = kt_file.read_text(encoding="utf-8")
        raw_lines = raw_content.splitlines()
        cleaned = strip_kotlin_comments(raw_content)
        for kind, pattern in _UI_LITERAL_PATTERNS:
            for match in pattern.finditer(cleaned):
                quoted = match.group(1)
                unquoted = match.group(2)
                line_no = cleaned.count("\n", 0, match.start(1)) + 1
                context = raw_lines[line_no - 1].strip()
                if (rel_path, quoted, context) in exemptions or (
                    rel_path,
                    unquoted,
                    context,
                ) in exemptions:
                    continue
                complaints.append(
                    f"{rel_path}:{line_no}: hardcoded {kind} string literal {quoted}; "
                    "move user-facing copy to strings.xml or record a reasoned exemption in noncopy.json"
                )

    # Check Kotlin exemptions (and any XML exemptions not consumed via parsed entries)
    # against actual file lines so stale or moved exemptions always fail.
    file_cache: dict[str, list[str]] = {}
    for identity, expected_count in exemptions.items():
        rel_path, literal, context = identity
        if rel_path.endswith(".kt"):
            if rel_path not in file_cache:
                file_cache[rel_path] = (root / rel_path).read_text(encoding="utf-8").splitlines()
            found = sum(
                line.count(literal)
                for line in file_cache[rel_path]
                if line.strip() == context
            )
            used[identity] = found
        if used[identity] != expected_count:
            complaints.append(
                f"{rel_path}: stale or reused non-copy exemption {literal!r}: "
                f"expected {expected_count} uses, found {used[identity]}"
            )

    summary = (
        f"{len(xml_files)} strings*.xml file(s), {entry_count} string entry/entries, "
        f"{len(kt_files)} feature Kotlin file(s), {len(exemptions)} non-copy exemption(s)"
    )
    return complaints, summary


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Lint Android strings.xml copy and noncopy.json")
    parser.add_argument("--root", type=Path, default=ROOT, help="Repository root directory")
    args = parser.parse_args(argv)
    try:
        complaints, summary = check(args.root)
        if complaints:
            print("\n".join(complaints), file=sys.stderr)
            return 1
        print(f"strings-lint: {summary}; all reviewed")
        return 0
    except (ValueError, KeyError, OSError, TypeError) as error:
        print(f"strings-lint: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
