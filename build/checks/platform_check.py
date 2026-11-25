#!/usr/bin/env python3
"""Keep desktop-platform code in the homes named for it.

A desktop-platform conditional is any of:

- a `cfg` predicate (in `#[cfg(...)]`, `#![cfg(...)]`, the predicate of
  `#[cfg_attr(...)]`, or `cfg!(...)`, through `any`, `all` and `not`) that
  names `unix`, `windows`, `target_family`, or a `target_os` other than
  `"ios"` and `"android"`;
- a path into `std::os::unix` or `std::os::windows` (`use std::os::{unix, ..}`
  too), `libc::` or `windows_sys::`.

Each one is counted where it is written. Three rules hold them:

1. They appear only in the homes (HOMES below: `crates/os`, the local-socket
   module, pty-host, node's `platform`, the login item and keep-awake, the
   terminal client's `platform`, the phone bridges and the launcher). Exempt
   everywhere: the mobile axis (`target_os = "ios"` or `"android"` alone is
   not desktop), `cfg!` read as data (below), and a gate carrying
   `// out of scope on Windows: <feature>`, on its line or in the comment
   lines directly above it, naming a feature Windows leaves out by choice
   (FEATURES below). Such a gate exempts what it gates too: the item after
   an attribute (and a `mod name;`'s file and folder), the whole file for an
   inner `#![cfg(...)]`, the block an `if cfg!(...) {` opens.
2. Test code is held to the same rule, in the homes too where it is an
   integration test (a file under a crate's `tests/`), since those exercise
   an API callers use without `cfg`. A home's unit tests sit beside the
   per-OS code they test and are left to review. Test code is a file under
   any `tests/` folder, one named `tests.rs`, `*_test.rs` or `*_tests.rs`,
   a module declared under a `cfg(test)` gate (`#[cfg(test)] mod name;`,
   its file and folder) and the item after any gate whose predicate holds
   `test` outside a `not(...)`. A test held back on Windows while a phase
   works is written `#[cfg_attr(windows, ignore = "windows: <symptom>")]`:
   allowed, and counted.
3. What is left is compared with the allowlist (platform-allowlist.txt
   beside this script), a count per file. A file over its count, or holding
   any when not listed, fails, and its conditionals are printed. A file
   under its count passes with a note that its entry can be lowered (so
   work running side by side never conflicts on the file); the lead lowers
   it, with `--write-allowlist`, which writes the allowlist from the tree.

`cfg!` is data when it picks a value rather than gating a statement: it is
not inside an `if` or `while` condition on its line (`windows: cfg!(windows),`,
`!cfg!(windows)`, `cfg!(windows).then(..)`, a `match` on it), or the `if`
it conditions is an expression, which is when the `if` follows `=`, `(`,
`[`, `,`, `|`, `:`, `=>` or `return`, when the whole `if .. else ..` is on
one line, or when the chain ends in an `else` and no branch holds a
statement (a `;`), as a function's tail `if cfg!(..) { A } else { B }`
does. An `else if cfg!(..)` reads as the head of its chain does (the line
above it at the same indent). An `if cfg!(..) {` that begins a statement, a
match guard (`X if cfg!(..) =>`) and a condition continued on a line of its
own (`&& cfg!(..)`) are gates. This reads syntax, not meaning; review holds
the rest.

    platform_check.py [--strict] [--list] [--write-allowlist]
                      [--root DIR] [--allowlist FILE] [--tracked FILE]

`--strict`, the end state, also fails on any allowlist entry and any
`ignore = "windows: ..."`. `--list` prints every conditional
counted, not only those of failing files. `--tracked FILE` names the list
of tracked files (build/checks/inputs.py writes it): as a Buck2 test the
check sees only the files its target declares, and every tracked Rust file
under crates/ must be among them.
"""

from __future__ import annotations

import argparse
import bisect
import os
from dataclasses import dataclass, field
from pathlib import Path
import re
import sys

# Not resolved: as a Buck2 test the script's inputs may be links into the
# checkout, and the check reads the files beside it in the action.
ROOT = Path(os.path.abspath(__file__)).parents[2]

# Where desktop-platform code lives (docs/TESTING.md, "Holding a test back
# on Windows"): a folder ends in "/", a file is named whole.
HOMES = (
    "crates/os/",
    "crates/agent-dir/src/local_socket/",
    "crates/agent-dir/src/local_socket.rs",
    "crates/pty-host/",
    "crates/node/src/platform/",
    "crates/node/src/platform.rs",
    "crates/node/src/supervisor/login/",
    "crates/node/src/supervisor/login.rs",
    "crates/node/src/supervisor/keep_awake/",
    "crates/node/src/supervisor/keep_awake.rs",
    "crates/tui/src/platform/",
    "crates/tui/src/platform.rs",
    "crates/app-ffi/",
    "crates/app-jni/",
    "crates/launch/",
)

# What Windows leaves out by choice, as words an `out of scope on
# Windows:` comment names one by: terminal Claude (claude_pty), raw attach,
# the wt workspace backend, the browser extension's native host, SSH with a
# Windows far end, Arm64 release artifacts, the terminal client.
FEATURES = re.compile(
    r"\b(claude_pty|terminal claude|attach|wt|browser|native host|ssh|arm64|tui)\b",
    re.IGNORECASE,
)
MARKER = re.compile(r"out of scope on Windows:\s*(.*)")
MOBILE = {"ios", "android"}

# The lexer's openings: comments, raw strings, strings and quotes.
SPECIAL = re.compile(r"""//|/\*|(?<![\w])[bc]?r(#*)"|"|'""")
STRING_REST = re.compile(r'[^"\\]*(?:\\.[^"\\]*)*"', re.S)
CHAR = re.compile(r"""'(?:\\(?:u\{[0-9a-fA-F]{1,6}\}|x[0-9a-fA-F]{2}|.)|[^\\'\n])'""")
BLOCK = re.compile(r"/\*|\*/")
NOT_NEWLINE = re.compile(r"[^\n]+")

ATTRIBUTE = re.compile(r"#(!?)\s*\[\s*(cfg_attr|cfg)\s*\(")
MACRO = re.compile(r"\bcfg!\s*\(")
PATH = re.compile(r"\bstd::os::(?:(unix|windows)\b|\{)|\b(libc|windows_sys)::")
BRACES = re.compile(r"[{};]")
KEY_VALUE = re.compile(r'(\w+)\s*=\s*"([^"]*)"')
NOT = re.compile(r"\bnot\s*\([^()]*\)")
IGNORE = re.compile(r'\s*ignore\s*=\s*"windows: \S[^"]*"\s*,?\s*', re.S)
FOLLOWING_ATTRIBUTES = re.compile(r"(?:\s*#\s*\[[^\]]*\])*")
MOD_FILE = re.compile(r"(?:\s*#\s*\[[^\]]*\])*\s*(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+(\w+)\s*;")
# A file naming none of these has nothing to count, and lexing is most of
# the time.
RELEVANT = re.compile(r"\bcfg|\blibc::|\bwindows_sys::|\bstd::os::")
CONDITION = re.compile(r"\b(?:if|while)\b")
VALUE_BEFORE = re.compile(r"(?:[=(\[,|]|(?<!:):|=>|\breturn)$")


@dataclass
class Source:
    """A Rust file with its comments and string contents blanked out of
    `code` (same length and lines as `text`), and its comments by line."""

    path: str
    text: str
    code: str
    comments: dict[int, str]
    starts: list[int]
    code_lines: list[str] = field(init=False)
    lines: list[str] = field(init=False)

    def __post_init__(self):
        self.code_lines = self.code.split("\n")
        self.lines = self.text.split("\n")

    def line(self, offset: int) -> int:
        return bisect.bisect_right(self.starts, offset) - 1


@dataclass
class Found:
    path: str
    line: int  # 1-based
    what: str
    text: str
    test: bool


@dataclass
class Result:
    violations: dict[str, list[Found]]
    ignores: list[Found]
    marked: list[Found]


def lex(path: str, text: str) -> Source:
    starts = [0] + [m.end() for m in re.finditer("\n", text)]
    pieces: list[str] = []
    comments: dict[int, str] = {}
    last = 0
    at = 0

    def blank(start: int, end: int, comment: bool):
        nonlocal last
        pieces.append(text[last:start])
        span = text[start:end]
        pieces.append(NOT_NEWLINE.sub(lambda m: " " * len(m.group()), span))
        last = end
        if comment:
            line = bisect.bisect_right(starts, start) - 1
            for number, part in enumerate(span.split("\n"), line):
                comments[number] = comments.get(number, "") + " " + part

    while (m := SPECIAL.search(text, at)) is not None:
        start = m.start()
        token = m.group()
        if token == "//":
            end = text.find("\n", start)
            end = len(text) if end < 0 else end
            blank(start, end, True)
        elif token == "/*":
            depth, end = 0, len(text)
            for b in BLOCK.finditer(text, start):
                depth += 1 if b.group() == "/*" else -1
                if depth == 0:
                    end = b.end()
                    break
            blank(start, end, True)
        elif token == "'":
            char = CHAR.match(text, start)
            end = char.end() if char else start + 1
            if char:
                blank(start + 1, end - 1, False)
        elif token.endswith('"') and "r" in token:
            close = text.find('"' + m.group(1), m.end())
            end = len(text) if close < 0 else close + 1 + len(m.group(1))
            blank(m.end(), max(m.end(), close if close >= 0 else len(text)), False)
        else:
            rest = STRING_REST.match(text, m.end())
            end = rest.end() if rest else len(text)
            blank(m.end(), max(m.end(), end - 1), False)
        at = max(end, start + 1)
    pieces.append(text[last:])
    return Source(path, text, "".join(pieces), comments, starts)


def close_paren(code: str, open_at: int) -> int:
    """The offset of the `)` matching the `(` at `open_at`."""
    depth = 0
    for i in range(open_at, len(code)):
        c = code[i]
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0:
                return i
    return len(code) - 1


def item_end(code: str, at: int) -> int:
    """Where the item, statement or block starting at `at` ends: its first
    `;` outside braces, the brace closing its first block, or the end of
    the block it is in."""
    depth = 0
    for m in BRACES.finditer(code, at):
        c = m.group()
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return m.end()
            if depth < 0:
                return m.start()
        elif depth == 0:
            return m.end()
    return len(code)


def block_end(code: str, at: int) -> int:
    """The end of the block `at` is in (the file's end at the top)."""
    depth = 0
    for m in BRACES.finditer(code, at):
        c = m.group()
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth < 0:
                return m.start()
    return len(code)


def desktop(predicate: str) -> bool:
    for key, value in KEY_VALUE.findall(predicate):
        if key == "target_os" and value not in MOBILE:
            return True
        if key == "target_family" and value != "wasm":
            return True
    bare = KEY_VALUE.sub("", predicate)
    return re.search(r"\b(unix|windows|target_family)\b", bare) is not None


def mentions_test(predicate: str) -> bool:
    bare = KEY_VALUE.sub("", predicate)
    while NOT.search(bare):
        bare = NOT.sub("", bare)
    return re.search(r"\btest\b", bare) is not None


def split_cfg_attr(content: str) -> tuple[str, str]:
    depth = 0
    for i, c in enumerate(content):
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
        elif c == "," and depth == 0:
            return content[:i], content[i + 1 :]
    return content, ""


def is_home(path: str) -> bool:
    return any(path == home or (home.endswith("/") and path.startswith(home)) for home in HOMES)


def crate_dir(path: str) -> str:
    parts = path.split("/")
    return "/".join(parts[:2]) + "/"


def is_integration_test(path: str) -> bool:
    return path.startswith(crate_dir(path) + "tests/")


def is_test_file(path: str) -> bool:
    parts = path.split("/")
    name = parts[-1]
    return "tests" in parts[:-1] or name == "tests.rs" or name.endswith(("_test.rs", "_tests.rs"))


def module_base(path: str) -> str:
    """The folder a file's `mod name;` resolves in."""
    parent, _, name = path.rpartition("/")
    inside = path[len(crate_dir(path)) :]
    root = name in ("lib.rs", "main.rs", "mod.rs", "build.rs") or re.fullmatch(
        r"(tests|examples|benches)/[^/]+\.rs|src/bin/[^/]+\.rs", inside
    )
    return parent + "/" if root else parent + "/" + name[: -len(".rs")] + "/"


def marker(source: Source, line: int) -> str | None:
    """The `out of scope on Windows:` comment on `line` (0-based) or in the
    comment lines directly above it: the feature it names, or "" when it
    names none."""
    candidates = [line]
    j = line - 1
    while j >= 0 and not source.code_lines[j].strip() and source.comments.get(j):
        candidates.append(j)
        j -= 1
    for j in candidates:
        found = MARKER.search(source.comments.get(j, ""))
        if found:
            feature = FEATURES.search(found.group(1))
            return feature.group(1) if feature else ""
    return None


def macro_is_data(source: Source, line: int, column: int) -> bool:
    text = source.code_lines[line]
    prefix = text[:column]
    stripped = text.lstrip()
    head = None
    for m in CONDITION.finditer(prefix):
        if not re.search(r"[{};]", prefix[m.end() :]):
            head = m
    if head is None:
        # A condition continued from the line above gates.
        return not stripped.startswith(("&&", "||"))
    if re.search(r"\}\s*else\b", text[column:]):
        return True
    at = source.starts[line] + head.start()
    before = prefix[: head.start()].rstrip()
    if before.endswith("else"):
        # The chain's head: earlier on this line, or the line above at the
        # same indent that doesn't close a block.
        first = CONDITION.search(prefix)
        if first is not None and first.start() < head.start():
            at = source.starts[line] + first.start()
            before = prefix[: first.start()].rstrip()
        else:
            indent = len(text) - len(stripped)
            at, before = -1, ""
            for j in range(line - 1, -1, -1):
                above = source.code_lines[j]
                body = above.lstrip()
                if not body or len(above) - len(body) != indent or body.startswith("}"):
                    continue
                found = CONDITION.search(above)
                if found:
                    at = source.starts[j] + found.start()
                    before = above[: found.start()].rstrip()
                break
    if VALUE_BEFORE.search(before):
        return True
    return at >= 0 and chain_is_value(source.code, at)


ELSE = re.compile(r"\s*else\b\s*")


def chain_is_value(code: str, at: int) -> bool:
    """Whether the `if` at `at` is an expression whose every branch is one:
    it ends in an `else`, and no branch's block holds a statement (a `;`
    outside nested brackets), as in a function's tail `if cfg!(..) { A }
    else { B }`."""
    if not code.startswith("if", at):
        return False
    pos = at
    while True:
        open_at = code.find("{", pos)
        if open_at < 0:
            return False
        close = item_end(code, open_at)
        if statement_in(code, open_at + 1, close - 1):
            return False
        otherwise = ELSE.match(code, close)
        if otherwise is None:
            return False
        pos = otherwise.end()
        if re.match(r"if\b", code[pos : pos + 3]):
            continue
        if code.startswith("{", pos):
            return not statement_in(code, pos + 1, item_end(code, pos) - 1)
        return False


def statement_in(code: str, start: int, end: int) -> bool:
    depth = 0
    for c in code[start:end]:
        if c in "{([":
            depth += 1
        elif c in "})]":
            depth -= 1
        elif c == ";" and depth == 0:
            return True
    return False


@dataclass
class Gate:
    start: int
    end: int
    line: int
    what: str
    test: bool
    desktop: bool
    ignore: bool
    data: bool
    feature: str | None
    extent: tuple[int, int] | None = None


def gates(source: Source) -> list[Gate]:
    code, text = source.code, source.text
    found = []
    for m in ATTRIBUTE.finditer(code):
        inner, kind = m.group(1) == "!", m.group(2)
        open_at = m.end() - 1
        close = close_paren(code, open_at)
        content = text[open_at + 1 : close]
        predicate, rest = split_cfg_attr(content) if kind == "cfg_attr" else (content, "")
        bracket = code.find("]", close)
        bracket = close if bracket < 0 else bracket
        line = source.line(m.start())
        if inner:
            # At the top of a file it gates the file; inside a block, the block.
            at_top = code[: m.start()].count("{") == code[: m.start()].count("}")
            extent = (m.start(), len(code) if at_top else block_end(code, m.start()))
        else:
            extent = (m.start(), item_end(code, bracket + 1))
        following = FOLLOWING_ATTRIBUTES.match(code, bracket + 1)
        test_item = following is not None and re.search(r"\btest\b", code[following.start() : following.end()]) is not None
        is_desktop = desktop(predicate)
        found.append(
            Gate(
                start=m.start(),
                end=bracket + 1,
                line=line,
                what=f"#{'!' if inner else ''}[{kind}({' '.join(content.split())})]",
                test=(kind == "cfg" and mentions_test(predicate)) or test_item,
                desktop=is_desktop,
                ignore=kind == "cfg_attr" and predicate.strip() == "windows" and IGNORE.fullmatch(rest) is not None,
                data=False,
                feature=marker(source, line) if is_desktop else None,
                extent=extent,
            )
        )
    for m in MACRO.finditer(code):
        open_at = m.end() - 1
        close = close_paren(code, open_at)
        content = text[open_at + 1 : close]
        if not desktop(content):
            continue
        line = source.line(m.start())
        column = m.start() - source.starts[line]
        line_code = source.code_lines[line].rstrip()
        extent = (m.start(), item_end(code, code.rfind("{", 0, source.starts[line] + len(line_code)))) if line_code.endswith("{") else None
        found.append(
            Gate(
                start=m.start(),
                end=close + 1,
                line=line,
                what=f"cfg!({' '.join(content.split())})",
                test=False,
                desktop=True,
                ignore=False,
                data=macro_is_data(source, line, column),
                feature=marker(source, line),
                extent=extent,
            )
        )
    return found


def paths(source: Source) -> list[tuple[int, str]]:
    found = []
    for m in PATH.finditer(source.code):
        if m.group(0).endswith("{"):
            close = item_end(source.code, m.end() - 1)
            if not re.search(r"\b(unix|windows)\b", source.code[m.end() : close]):
                continue
            found.append((m.start(), "std::os::{..}"))
        elif m.group(1):
            found.append((m.start(), f"std::os::{m.group(1)}"))
        else:
            found.append((m.start(), f"{m.group(2)}::"))
    return found


def rust_files(root: Path) -> list[str]:
    files = []
    crates = root / "crates"
    for directory, folders, names in os.walk(crates):
        folders[:] = sorted(f for f in folders if f != "target" and not f.startswith("."))
        for name in names:
            if name.endswith(".rs"):
                files.append(Path(directory, name).relative_to(root).as_posix())
    return sorted(files)


def under(path: str, prefixes: set[str]) -> bool:
    return path in prefixes or any(path.startswith(p) for p in prefixes if p.endswith("/"))


def scan(root: Path) -> Result:
    sources = {}
    for path in rust_files(root):
        text = (root / path).read_text(encoding="utf-8", errors="replace")
        if RELEVANT.search(text):
            sources[path] = lex(path, text)

    # First the gates, and from them the modules declared in a file of
    # their own that are test code, or out of scope, whole.
    all_gates = {path: gates(source) for path, source in sources.items()}
    test_modules: set[str] = set()
    scoped_modules: set[str] = set()
    for path, source in sources.items():
        for gate in all_gates[path]:
            if not (gate.test or (gate.desktop and gate.feature)) or gate.what.startswith("cfg!"):
                continue
            module = MOD_FILE.match(source.code, gate.end)
            if module is None:
                continue
            base = module_base(path) + module.group(1)
            targets = {base + ".rs", base + "/"}
            (test_modules if gate.test else scoped_modules).update(targets)
            if gate.test and gate.desktop and gate.feature:
                scoped_modules.update(targets)

    result = Result(violations={}, ignores=[], marked=[])
    for path, source in sources.items():
        home = is_home(path)
        test_file = is_test_file(path) or under(path, test_modules)
        if home and not is_integration_test(path):
            # Only the ignores count here.
            result.ignores += [found(source, g, True) for g in all_gates[path] if g.ignore]
            continue
        if under(path, scoped_modules):
            continue
        tests = [g.extent for g in all_gates[path] if g.test and g.extent]
        scoped = [g.extent for g in all_gates[path] if g.desktop and g.feature and g.extent]

        def inside(offset: int, spans) -> bool:
            return any(start <= offset < end for start, end in spans)

        for gate in all_gates[path]:
            if not gate.desktop:
                continue
            if gate.ignore:
                result.ignores.append(found(source, gate, True))
                continue
            if gate.data:
                continue
            if gate.feature:
                result.marked.append(found(source, gate, True))
                continue
            if inside(gate.start, scoped):
                continue
            is_test = test_file or gate.test or inside(gate.start, tests)
            what = gate.what
            if gate.feature == "":
                what += " (its `out of scope on Windows:` comment names no feature Windows leaves out)"
            result.violations.setdefault(path, []).append(Found(path, gate.line + 1, what, source.lines[gate.line].strip(), is_test))
        for offset, what in paths(source):
            if inside(offset, scoped):
                continue
            line = source.line(offset)
            is_test = test_file or inside(offset, tests)
            result.violations.setdefault(path, []).append(Found(path, line + 1, what, source.lines[line].strip(), is_test))
    for violations in result.violations.values():
        violations.sort(key=lambda f: f.line)
    return result


def found(source: Source, gate: Gate, test: bool) -> Found:
    return Found(source.path, gate.line + 1, gate.what, source.lines[gate.line].strip(), test)


def read_allowlist(path: Path) -> dict[str, int]:
    allowed = {}
    if not path.exists():
        return allowed
    for number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        name, _, count = line.rpartition(" ")
        if not name or not count.isdigit():
            raise SystemExit(f"{path}:{number}: expected `<file> <count>`, found {raw!r}")
        allowed[name.strip()] = int(count)
    return allowed


HEADER = """\
# platform-check's allowlist (build/checks/platform_check.py): each file
# outside the homes that still holds desktop-platform conditionals, with how
# many. A file's count may only fall, and a file not listed may hold none.
# The lead writes it when landing, never a work package:
#     python3 build/checks/platform_check.py --write-allowlist
"""


def write_allowlist(path: Path, result: Result) -> None:
    lines = [f"{name} {len(found)}\n" for name, found in sorted(result.violations.items())]
    path.write_text(HEADER + "".join(lines), encoding="utf-8", newline="\n")


def missing_inputs(root: Path, tracked: Path) -> list[str]:
    if not tracked.exists():
        raise SystemExit(f"platform-check: no {tracked}; build/checks/inputs.py writes it, and `just test` runs that first")
    names = tracked.read_text(encoding="utf-8").splitlines()
    return [n for n in names if n.startswith("crates/") and n.endswith(".rs") and not (root / n).exists()]


def show(found: Found, kind: str | None = None) -> str:
    kind = kind or ("test" if found.test else "code")
    # An attribute as one line (it may span several); anything else as
    # the line it is on.
    what = found.what if found.what.startswith("#") else found.text
    return f"  {found.path}:{found.line}: {kind}: {what}"


def check(root: Path, allowlist: Path, strict: bool = False, listing: bool = False, tracked: Path | None = None) -> int:
    if tracked is not None:
        missing = missing_inputs(root, tracked)
        if missing:
            print("platform-check: tracked Rust files the check was not given (add their crate to the root BUCK's _CRATES):")
            for name in missing[:20]:
                print(f"  {name}")
            return 1
    result = scan(root)
    allowed = read_allowlist(allowlist)
    failed = False
    over, lower, stale = [], [], []
    for name, found in sorted(result.violations.items()):
        limit = allowed.get(name, 0)
        if len(found) > limit:
            over.append((name, found, limit))
        elif len(found) < limit:
            lower.append((name, len(found), limit))
    for name, limit in sorted(allowed.items()):
        if name not in result.violations:
            stale.append((name, 0, limit))
    if over:
        failed = True
        print("platform-check: desktop-platform conditionals outside the homes, over the allowlist")
        print("(move them into a home, docs/TESTING.md \"Holding a test back on Windows\"; never raise the allowlist):")
        for name, found, limit in over:
            print(f"{name}: {len(found)}, allowlist {'holds ' + str(limit) if limit else 'has none'}")
            if not listing:
                for f in found:
                    print(show(f))
    if listing:
        for name, found in sorted(result.violations.items()):
            for f in found:
                print(show(f))
        for f in result.ignores:
            print(show(f, "ignore"))
    if lower or stale:
        print("platform-check: allowlist entries the tree is under (the lead lowers them with --write-allowlist):")
        for name, count, limit in lower + stale:
            print(f"  {name}: {count}, allowlist {limit}")
    total = sum(len(f) for f in result.violations.values())
    tests = sum(1 for found in result.violations.values() for f in found if f.test)
    print(
        f"platform-check: {total} desktop-platform conditionals outside the homes ({tests} in tests) "
        f"in {len(result.violations)} files; allowlist {len(allowed)} entries, {sum(allowed.values())} in all; "
        f"{len(result.ignores)} `ignore = \"windows: ...\"`; {len(result.marked)} gates out of scope on Windows"
    )
    if strict:
        if allowed or total or result.ignores:
            print("platform-check --strict: the allowlist must be empty, and nothing may be ignored on Windows")
            for f in result.ignores:
                print(show(f, "ignore"))
            failed = True
    return 1 if failed else 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--strict", action="store_true", help="also fail on any allowlist entry or ignore")
    parser.add_argument("--list", action="store_true", help="print every conditional counted")
    parser.add_argument("--write-allowlist", action="store_true", help="write the allowlist from the tree")
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--allowlist", type=Path)
    parser.add_argument("--tracked", type=Path)
    args = parser.parse_args(argv)
    allowlist = args.allowlist or (args.root / "build/checks/platform-allowlist.txt")
    if args.write_allowlist:
        result = scan(args.root)
        write_allowlist(allowlist, result)
        print(f"platform-check: wrote {allowlist} ({len(result.violations)} files, {sum(len(f) for f in result.violations.values())} conditionals)")
        return 0
    return check(args.root, allowlist, strict=args.strict, listing=args.list, tracked=args.tracked)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
