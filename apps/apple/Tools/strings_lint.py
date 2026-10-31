#!/usr/bin/env python3
"""Fail closed on unreviewed Swift literals, including model and debug copy.

This is a lexical inventory, not a guess about which APIs display strings.
Non-copy exemptions bind to an exact literal and its surrounding source line,
so using a wire key as a new label requires review too. Interpolation bodies
are scanned recursively: both the template and any fallback words are checked.
"""

from collections import Counter
from dataclasses import dataclass
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[3]
CATALOGUE = Path("apps/apple/Amux/Resources/Localizable.xcstrings")
DEBUG_CATALOGUE = Path(
    "apps/apple/Packages/AmuxTestSupport/Sources/AmuxTestSupport/Resources/DebugCopy.xcstrings")
EXEMPTIONS = Path("apps/apple/Tools/noncopy.json")


@dataclass(frozen=True)
class Literal:
    line: int
    source: str
    context: str
    key: str


def literals(source):
    """Read Swift comments, ordinary/raw/multiline strings and interpolation.

    The catalogue uses %@ for a substituted value, independently of its Swift
    expression or type. It inventories English copy; it does not infer a
    translation's format arguments from Swift types.
    """
    found = []

    def scan(i, interpolation=False):
        depth = 0
        while i < len(source):
            if source.startswith("//", i):
                end = source.find("\n", i)
                i = len(source) if end < 0 else end
                continue
            if source.startswith("/*", i):
                nesting = 1
                i += 2
                while nesting and i < len(source):
                    if source.startswith("/*", i):
                        nesting += 1
                        i += 2
                    elif source.startswith("*/", i):
                        nesting -= 1
                        i += 2
                    else:
                        i += 1
                if nesting:
                    raise ValueError("unterminated block comment")
                continue
            match = re.match(r'(#+)?("""|")', source[i:]) if source[i] in '#"' else None
            if match:
                start = i
                hashes, quote = match.groups()
                hashes = hashes or ""
                i += len(match[0])
                end = quote + hashes
                escape = "\\" + hashes
                parts = []
                while i < len(source) and not source.startswith(end, i):
                    if source.startswith(escape + "(", i):
                        i = scan(i + len(escape) + 1, interpolation=True)
                        parts.append("%@")
                    elif source.startswith(escape, i):
                        i += len(escape)
                        if i >= len(source):
                            raise ValueError("unterminated escape")
                        char = source[i]
                        if source.startswith("u{", i):
                            close = source.find("}", i + 2)
                            if close < 0:
                                raise ValueError("unterminated Unicode escape")
                            parts.append(chr(int(source[i + 2:close], 16)))
                            i = close + 1
                        elif char == "\n":
                            i += 1
                            while i < len(source) and source[i] in " \t":
                                i += 1
                        else:
                            escapes = {"n": "\n", "r": "\r", "t": "\t", "0": "\0",
                                       '"': '"', "'": "'", "\\": "\\"}
                            if char not in escapes:
                                raise ValueError(f"unsupported Swift escape: {char!r}")
                            parts.append(escapes[char])
                            i += 1
                    else:
                        parts.append(source[i])
                        i += 1
                if i >= len(source):
                    raise ValueError("unterminated string")
                i += len(end)
                key = "".join(parts)
                if quote == '"""':
                    closing_line = key.rfind("\n")
                    indent = key[closing_line + 1:]
                    if not key.startswith("\n") or indent.strip():
                        raise ValueError("malformed multiline string")
                    key = "\n".join(line.removeprefix(indent)
                                    for line in key[1:closing_line].split("\n"))
                line_start = source.rfind("\n", 0, start) + 1
                line_end = source.find("\n", i)
                if line_end < 0:
                    line_end = len(source)
                found.append(Literal(source.count("\n", 0, start) + 1,
                                     source[start:i], source[line_start:line_end].strip(), key))
                continue
            if interpolation:
                if source[i] == "(":
                    depth += 1
                elif source[i] == ")":
                    if depth == 0:
                        return i + 1
                    depth -= 1
            i += 1
        if interpolation:
            raise ValueError("unterminated interpolation")
        return i

    scan(0)
    return sorted(found, key=lambda item: item.line)


# Swift generated from the Rust definitions of the bridge's values: its
# literals are wire keys and case names, which change with the Rust and are
# never shown to anybody, and `ios-tools swift-types --check` already fails on
# any hand edit.
GENERATED = Path("apps/apple/Packages/AmuxCore/Sources/AmuxValues")


def swift_sources(root):
    roots = [root / "apps/apple/Amux/Sources", root / "apps/apple/Amux/Debug",
             *sorted((root / "apps/apple/Packages").glob("*/Sources"))]
    generated = root / GENERATED
    return sorted(
        path for directory in roots for path in directory.rglob("*.swift")
        if generated not in path.parents)


def debug_source(path):
    return (path.startswith("apps/apple/Amux/Debug/")
            or path.startswith("apps/apple/Packages/AmuxTestSupport/Sources/"))


def read_json(path):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"{path}: duplicate key {key!r}")
            result[key] = value
        return result
    return json.loads(path.read_text(), object_pairs_hook=unique)


def catalogue(root, path):
    data = read_json(root / path)
    if data.get("sourceLanguage") != "en" or data.get("version") != "1.0":
        raise ValueError(f"{path}: expected an English string catalogue")
    entries = data["strings"]
    if not entries:
        raise ValueError(f"{path}: empty string catalogue")
    for key, entry in entries.items():
        unit = entry.get("localizations", {}).get("en", {}).get("stringUnit", {})
        if unit.get("value") != key or unit.get("state") != "translated":
            raise ValueError(f"{path}: missing or different English value for {key!r}")
        if not entry.get("comment", "").strip():
            raise ValueError(f"{path}: missing copy context for {key!r}")
        if any(mark in key for mark in ("—", ";", "...", "!", "'")):
            raise ValueError(f"{path}: copy punctuation violates IOS_COPY.md: {key!r}")
    return entries


def check(root=ROOT):
    public = catalogue(root, CATALOGUE)
    debug = catalogue(root, DEBUG_CATALOGUE)
    data = read_json(root / EXEMPTIONS)
    if data.get("version") != 1:
        raise ValueError("unsupported non-copy inventory version")
    exemptions = {}
    for path, entries in data["files"].items():
        if not path.endswith(".swift") or not (root / path).is_file():
            raise ValueError(f"non-copy inventory names a missing source: {path}")
        for entry in entries:
            if not entry.get("reason", "").strip():
                raise ValueError(f"{path}: non-copy exemption has no reason")
            if type(entry.get("count")) is not int or entry["count"] < 1:
                raise ValueError(f"{path}: non-copy count must be positive")
            identity = (path, entry["literal"], entry["context"])
            if identity in exemptions:
                raise ValueError(f"{path}: duplicate non-copy exemption")
            exemptions[identity] = entry["count"]
    used = Counter()
    worded = {CATALOGUE: set(), DEBUG_CATALOGUE: set()}
    complaints = []
    seen = 0
    sources = swift_sources(root)
    if not sources:
        raise ValueError("no Swift application/package sources")
    for file in sources:
        path = file.relative_to(root).as_posix()
        for item in literals(file.read_text()):
            # Empty/whitespace literals have no words to review. Punctuation,
            # format strings and identifier-like words still require review.
            if not item.key.strip():
                continue
            seen += 1
            identity = (path, item.source, item.context)
            if identity in exemptions:
                used[identity] += 1
                continue
            if item.key in public:
                worded[CATALOGUE].add(item.key)
            if debug_source(path) and item.key in debug:
                worded[DEBUG_CATALOGUE].add(item.key)
            if item.key not in public and not (debug_source(path) and item.key in debug):
                complaints.append(f"{path}:{item.line}: uncatalogued literal {item.source}")
    # The other way round too: copy whose code went must not stay behind.
    for path, entries in ((CATALOGUE, public), (DEBUG_CATALOGUE, debug)):
        for key in entries:
            if key not in worded[path]:
                complaints.append(f"{path}: no literal uses the catalogued copy {key!r}")
    for identity, count in exemptions.items():
        if used[identity] != count:
            path, literal, _ = identity
            complaints.append(f"{path}: stale or reused non-copy exemption {literal}: "
                              f"expected {count} uses, found {used[identity]}")
    return complaints, f"{len(sources)} Swift sources, {seen} literals, {len(public)} English and {len(debug)} debug copy entries"


def main():
    try:
        complaints, summary = check()
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
