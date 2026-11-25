#!/usr/bin/env python3
"""Keep desktop-platform code in the homes named for it.

A desktop-platform conditional is any of:

- a `cfg` predicate (in `#[cfg(...)]`, `#![cfg(...)]`, `cfg!(...)`, and
  `#[cfg_attr(...)]`'s predicate or any `cfg`/`cfg_attr` it applies, so
  `#[cfg_attr(test, cfg(unix))]` counts; through `any`, `all` and `not`;
  read with its comments taken out) that names `unix`, `windows`,
  `target_family`, or a `target_os` other than `"ios"` and `"android"`;
- a path into `std::os::unix` or `std::os::windows`, `libc` or
  `windows_sys`: in a `use`, grouped trees too (`use std::{os::unix::fs}`,
  counted once for each module a `use` reaches), or in an expression
  (`libc::kill`, `os::unix::fs::symlink` after `use std::os`).

Each one is counted where it is written. Three rules hold them:

1. They appear only in the homes (HOMES below: `crates/os`, the local-socket
   module, pty-host, node's `platform`, the login item and keep-awake, the
   terminal client's `platform`, the phone bridges and the launcher). Exempt
   everywhere: the mobile axis (`target_os = "ios"` or `"android"` alone is
   not desktop), `cfg!` read as data (below), and a gate carrying
   `// out of scope on Windows: <feature>`, on its line or in the comment
   lines directly above it, naming a feature Windows leaves out by choice
   (FEATURES below), whose predicate is false in a build for Windows
   (`cfg(not(windows))`, `cfg(unix)`; not `cfg(windows)`, nor
   `cfg(any(unix, feature = ..))`, nor a lint's `cfg_attr`; a `cfg` nested
   in `cfg_attr`s only where every one around it applies on Windows). Such
   a gate exempts what it gates too: the item after an attribute (only the
   field, variant, parameter or arm, where it is on one), a `mod name;`'s
   file and folder, the whole file for an inner `#![cfg(...)]`, and the
   block an `if cfg!(...) {` or `if !cfg!(...) {` opens, when that is its
   whole condition.
2. Test code is held to the same rule. In a home, its code is free but its
   tests are held, since they test an API callers use without `cfg`; only
   a home's file for one OS (a path part named `unix`, `windows`, `macos`,
   `linux`, ...: `process/unix.rs`) may gate the tests of its own code.
   Test code is a file under any `tests/` folder, one named `tests.rs`,
   `*_test.rs` or `*_tests.rs`, a module declared under a `cfg(test)` gate
   (`#[cfg(test)] mod name;`, its file and folder), the item after a `cfg`
   that holds only in test builds (`test`, `all(test, unix)`; not
   `any(windows, test)`), and an item marked `#[test]` (before or after the
   gate). A test held back on Windows while a phase works is written
   `#[cfg_attr(windows, ignore = "windows: <symptom>")]`: allowed, and
   counted, wherever it is (homes and out-of-scope modules too). An ignore
   under a desktop predicate written any other way, nested ones
   (`cfg_attr(test, cfg_attr(windows, ignore))`) among them, is a
   violation everywhere.
3. What is left is compared with the allowlist (platform-allowlist.txt
   beside this script), a count per file. A file over its count, or holding
   any when not listed, fails, and its conditionals are printed. A file
   under its count passes with a note that its entry can be lowered (so
   work running side by side never conflicts on the file); the lead lowers
   it, with `--write-allowlist`, which writes the allowlist from the tree.

`cfg!` is data when it picks a value rather than deciding what runs: it is
not in the condition of an `if` or `while` or the scrutinee of a `match`,
looking back across lines to the statement's start (`windows: cfg!(windows),`,
`!cfg!(windows)`,
`cfg!(windows).then(..)`), or it is, and every branch or arm gives a value:
an `if` chain that ends in an `else` (`let s = if cfg!(windows) { ';' }
else { ':' };`, a function's tail `if cfg!(..) { A } else { B }`), a
`match` whose arms are all values. A branch or arm runs code when it is
empty or `()`, holds a statement (a `;`), or is an assertion, a print, a
panic or a jump (`match cfg!(windows) { true => (), false => assert!(..) }`
is a gate). An `else if cfg!(..)` reads as the head of its chain does (the
line above it at the same indent). A `while` and a match guard
(`X if cfg!(..) =>`) are gates. This reads syntax, not meaning; review
holds the rest.

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
# The names a home gives a file or folder for one OS.
OS_WORDS = {"unix", "windows", "macos", "linux", "apple", "posix", "ios", "android"}

# The lexer's openings: comments, raw strings, strings and quotes.
SPECIAL = re.compile(r"""//|/\*|(?<![\w])[bc]?r(#*)"|"|'""")
STRING_REST = re.compile(r'[^"\\]*(?:\\.[^"\\]*)*"', re.S)
CHAR = re.compile(r"""'(?:\\(?:u\{[0-9a-fA-F]{1,6}\}|x[0-9a-fA-F]{2}|.)|[^\\'\n])'""")
BLOCK = re.compile(r"/\*|\*/")
NOT_NEWLINE = re.compile(r"[^\n]+")

ATTRIBUTE = re.compile(r"#(!?)\s*\[\s*(cfg_attr|cfg)\s*\(")
MACRO = re.compile(r"\bcfg!\s*\(")
BRACES = re.compile(r"[{};]")
KEY_VALUE = re.compile(r'(\w+)\s*=\s*"([^"]*)"')
IGNORE = re.compile(r'\s*ignore\s*=\s*"windows: \S[^"]*"\s*,?\s*', re.S)
FOLLOWING_ATTRIBUTES = re.compile(r"(?:\s*#\s*\[[^\]]*\])*")
# An attribute that makes an item a test, or test code.
TEST_ATTRIBUTE = re.compile(r"#\s*\[\s*(?:(?:\w+::)*test\s*[\](]|cfg\s*\(\s*test\s*\))")
MOD_FILE = re.compile(r"(?:\s*#\s*\[[^\]]*\])*\s*(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+(\w+)\s*;")
# A file naming none of these has nothing to count, and lexing is most of
# the time.
RELEVANT = re.compile(r"\bcfg|\blibc\b|\bwindows_sys\b|\bos::")
HEAD = re.compile(r"\b(if|while|match)\b")


@dataclass
class Source:
    """A Rust file with its comments and string contents blanked out of
    `code`, its comments alone blanked out of `bare` (both the same length
    and lines as `text`), and its comments by line."""

    path: str
    text: str
    code: str
    comments: dict[int, str]
    starts: list[int]
    bare: str = ""
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
    bare: list[str] = []
    comments: dict[int, str] = {}
    last = 0
    at = 0

    def blank(start: int, end: int, comment: bool):
        nonlocal last
        pieces.append(text[last:start])
        bare.append(text[last:start])
        span = text[start:end]
        blanked = NOT_NEWLINE.sub(lambda m: " " * len(m.group()), span)
        pieces.append(blanked)
        bare.append(blanked if comment else span)
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
    bare.append(text[last:])
    return Source(path, text, "".join(pieces), comments, starts, "".join(bare))


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


def member_end(code: str, at: int) -> int:
    """Where the field, variant, parameter or arm starting at `at` ends: at
    its comma outside brackets, or the bracket closing the list."""
    depth = 0
    for i in range(at, len(code)):
        c = code[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
            if depth < 0:
                return i
        elif c == "," and depth == 0:
            return i + 1
    return len(code)


# A block whose members a `,` separates: a struct, enum or union's body,
# a match's arms, or a struct literal (a path before the brace, and none of
# the words that open a block of statements).
MEMBER_LIST = re.compile(r"\b(?:struct|enum|union|match)\b")
STATEMENT_BLOCK = re.compile(r"\b(?:fn|impl|mod|trait|extern|loop|if|else|while|for|unsafe|async|move|where|macro_rules|const|static)\b|=>\s*$")


def enclosing(code: str, offsets: list[int]) -> dict[int, tuple[str, int]]:
    """For each offset, the innermost bracket open around it and where."""
    found, stack, wanted = {}, [], sorted(offsets)
    at = 0
    for m in re.finditer(r"[(){}\[\]]", code):
        while at < len(wanted) and wanted[at] < m.start():
            found[wanted[at]] = stack[-1] if stack else ("", -1)
            at += 1
        if m.group() in "([{":
            stack.append((m.group(), m.start()))
        elif stack:
            stack.pop()
    for offset in wanted[at:]:
        found[offset] = stack[-1] if stack else ("", -1)
    return found


def in_member_list(code: str, bracket: str, at: int) -> bool:
    """Whether the bracket at `at` opens a list of members (a tuple
    struct's fields, a function's parameters, a struct's or enum's body,
    a match's arms), so an attribute in it gates one member."""
    if bracket in ("(", "["):
        return True
    if bracket != "{":
        return False
    start = max(code.rfind(c, 0, at) for c in "{};") + 1
    header = code[start:at].strip()
    if MEMBER_LIST.search(header):
        return True
    return re.search(r"[\w>]$", header) is not None and STATEMENT_BLOCK.search(header) is None


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


CFG_TOKENS = re.compile(r'\w+\s*=\s*"[^"]*"|\w+|[(),]')


def evaluate(predicate: str, atom) -> bool | None:
    """The predicate's value where `atom(name, value)` says what each atom
    is (`value` is None for a bare name), None for one it cannot say."""
    tokens = CFG_TOKENS.findall(predicate)
    at = 0

    def expression() -> bool | None:
        nonlocal at
        token = tokens[at]
        at += 1
        if token in ("any", "all", "not") and at < len(tokens) and tokens[at] == "(":
            at += 1
            values = []
            while tokens[at] != ")":
                values.append(expression())
                if tokens[at] == ",":
                    at += 1
            at += 1
            if token == "not":
                return None if not values or values[0] is None else not values[0]
            if token == "any":
                return True if True in values else (False if all(v is False for v in values) else None)
            return False if False in values else (True if all(v is True for v in values) else None)
        pair = KEY_VALUE.fullmatch(token)
        return atom(*pair.groups()) if pair else atom(token, None)

    try:
        return expression()
    except IndexError:
        return None


def windows_atom(name: str, value: str | None) -> bool | None:
    if value is None:
        return {"windows": True, "unix": False}.get(name)
    return value == "windows" if name in ("target_os", "target_family") else None


def on_windows(predicate: str) -> bool | None:
    """The predicate's value in a build for Windows, or None when it depends
    on something else (a feature, `test`, `debug_assertions`)."""
    return evaluate(predicate, windows_atom)


def needs_test(predicate: str) -> bool:
    """Whether the predicate holds only in a test build: `test`,
    `all(test, unix)`, but not `any(windows, test)`."""
    return evaluate(predicate, lambda name, value: False if (name, value) == ("test", None) else None) is False


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


def attribute_tree(rest: str, chain: tuple[str, ...]) -> list[tuple[str, str, tuple[str, ...]]]:
    """What a `cfg_attr` applies, at any depth, as (kind, text, chain):
    `cfg` with its predicate, `cfg_attr` with its predicate, or any other
    attribute (`ignore`, `allow(..)`) as written, each with the predicates
    of the `cfg_attr`s it sits in, outermost first."""
    found = []
    for item in split_top(rest):
        item = item.strip()
        nested = re.match(r"(cfg_attr|cfg)\s*\(", item)
        if nested and item.endswith(")"):
            content = item[nested.end() : -1]
            if nested.group(1) == "cfg":
                found.append(("cfg", content, chain))
            else:
                predicate, inner = split_cfg_attr(content)
                found.append(("cfg_attr", predicate, chain))
                found += attribute_tree(inner, chain + (predicate,))
        elif item:
            found.append(("other", item, chain))
    return found


def split_top(text: str) -> list[str]:
    """`text` split at its commas outside brackets and strings."""
    parts, depth, start, quoted = [], 0, 0, False
    i = 0
    while i < len(text):
        c = text[i]
        if quoted:
            if c == "\\":
                i += 1
            elif c == '"':
                quoted = False
        elif c == '"':
            quoted = True
        elif c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        elif c == "," and depth == 0:
            parts.append(text[start:i])
            start = i + 1
        i += 1
    parts.append(text[start:])
    return parts


def is_home(path: str) -> bool:
    return any(path == home or (home.endswith("/") and path.startswith(home)) for home in HOMES)


def per_os(path: str) -> bool:
    """A home's file for one OS (`process/unix.rs`, `windows/mod.rs`), whose
    tests may test its own code where only that OS has it."""
    return any(part.removesuffix(".rs") in OS_WORDS for part in path[len(crate_dir(path)) :].split("/"))


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


def condition_head(code: str, offset: int) -> tuple[int, re.Match | None]:
    """The `if`, `while` or `match` whose condition or scrutinee holds the
    `cfg!` at `offset`, looking back across lines to the statement's start
    (the last `{`, `}` or `;`): the boundary, and the keyword or None."""
    boundary = max(code.rfind(c, 0, offset) for c in "{};")
    heads = list(HEAD.finditer(code, boundary + 1, offset))
    return boundary, heads[-1] if heads else None


def macro_is_data(source: Source, offset: int) -> bool:
    code = source.code
    boundary, head = condition_head(code, offset)
    if head is None:
        return True
    at = head.start()
    if head.group(1) == "while":
        return False
    if head.group(1) == "match":
        return match_is_value(code, at)
    if code[boundary + 1 : at].strip() == "else":
        # The chain's head: the line above the `else if` at its indent
        # that doesn't close a block.
        line = source.line(at)
        text = source.code_lines[line]
        indent = len(text) - len(text.lstrip())
        at = -1
        for j in range(line - 1, -1, -1):
            above = source.code_lines[j]
            body = above.lstrip()
            if not body or len(above) - len(body) != indent or body.startswith("}"):
                continue
            found = re.search(r"\bif\b", above)
            if found:
                at = source.starts[j] + found.start()
            break
    return at >= 0 and chain_is_value(code, at)


ELSE = re.compile(r"\s*else\b\s*")
# An arm or branch that does something rather than give a value.
SIDE_EFFECT = re.compile(
    r"(?:(?:debug_)?assert\w*|panic|e?print(?:ln)?|write(?:ln)?|unreachable|todo|unimplemented)\s*!|(?:return|break|continue)\b"
)


def runs_code(body: str) -> bool:
    """Whether a branch's body runs code rather than give a value: a block
    holding a statement, an empty or unit body, or an assertion, a print,
    a panic or a jump."""
    b = body.strip()
    if b.startswith("{") and b.endswith("}"):
        inner = b[1:-1]
        if statement_in(inner, 0, len(inner)):
            return True
        b = inner.strip()
    return b in ("", "()") or SIDE_EFFECT.match(b) is not None


def chain_is_value(code: str, at: int) -> bool:
    """Whether the `if` at `at` is an expression whose every branch is a
    value: it ends in an `else`, and no branch runs code, as in a
    function's tail `if cfg!(..) { A } else { B }`."""
    if not code.startswith("if", at):
        return False
    pos = at
    while True:
        open_at = code.find("{", pos)
        # A match guard's `if` has an arm, not a block.
        if open_at < 0 or "=>" in code[pos:open_at]:
            return False
        close = item_end(code, open_at)
        if runs_code(code[open_at:close]):
            return False
        otherwise = ELSE.match(code, close)
        if otherwise is None:
            return False
        pos = otherwise.end()
        if re.match(r"if\b", code[pos : pos + 3]):
            continue
        if code.startswith("{", pos):
            return not runs_code(code[pos : item_end(code, pos)])
        return False


def match_is_value(code: str, at: int) -> bool:
    """Whether the `match` at `at` picks a value in every arm: no arm runs
    code (`true => (), false => assert!(..)` does)."""
    open_at = code.find("{", at)
    if open_at < 0:
        return False
    body = code[open_at + 1 : item_end(code, open_at) - 1]
    arms, depth, i, start = [], 0, 0, None
    while i < len(body):
        c = body[i]
        if start is None:
            if c in "([{":
                depth += 1
            elif c in ")]}":
                depth -= 1
            elif depth == 0 and body.startswith("=>", i):
                start = i + 2
                i += 2
                continue
        else:
            # The arm's body: a block, or up to the next comma outside brackets.
            if not body[start:i].strip() and c == "{":
                end = item_end(body, i)
                arms.append(body[i:end])
                start, i = None, end
                continue
            if c in "([{":
                depth += 1
            elif c in ")]}":
                depth -= 1
            elif c == "," and depth == 0:
                arms.append(body[start:i])
                start = None
        i += 1
    if start is not None:
        arms.append(body[start:])
    return bool(arms) and not any(runs_code(arm) for arm in arms)


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
    # A `cfg_attr(windows, ignore = "windows: ...")`, and an ignore on a
    # desktop platform written any other way.
    ignore: bool
    bad_ignore: bool
    data: bool
    feature: str | None
    # Whether what it gates is left out of a build for Windows.
    excludes_windows: bool
    extent: tuple[int, int] | None = None

    @property
    def exempts(self) -> bool:
        return bool(self.feature) and self.excludes_windows


def gates(source: Source) -> list[Gate]:
    code, bare = source.code, source.bare
    found = []
    attributes = list(ATTRIBUTE.finditer(code))
    around = enclosing(code, [m.start() for m in attributes])
    for m in attributes:
        inner, kind = m.group(1) == "!", m.group(2)
        open_at = m.end() - 1
        close = close_paren(code, open_at)
        content = bare[open_at + 1 : close]
        predicate, rest = split_cfg_attr(content) if kind == "cfg_attr" else (content, "")
        tree = attribute_tree(rest, (predicate,)) if kind == "cfg_attr" else []
        bracket = code.find("]", close)
        bracket = close if bracket < 0 else bracket
        line = source.line(m.start())
        if inner:
            # At the top of a file it gates the file; inside a block, the block.
            at_top = code[: m.start()].count("{") == code[: m.start()].count("}")
            extent = (m.start(), len(code) if at_top else block_end(code, m.start()))
        elif in_member_list(code, *around[m.start()]):
            extent = (m.start(), member_end(code, bracket + 1))
        else:
            extent = (m.start(), item_end(code, bracket + 1))
        following = FOLLOWING_ATTRIBUTES.match(code, bracket + 1)
        after = code[following.start() : following.end()] if following else ""
        test_item = TEST_ATTRIBUTE.search(after) is not None or TEST_ATTRIBUTE.search(attributes_before(code, m.start())) is not None
        is_desktop = desktop(predicate) or any(k != "other" and desktop(t) for k, t, _ in tree)
        ignore = kind == "cfg_attr" and predicate.strip() == "windows" and IGNORE.fullmatch(rest) is not None
        # An ignore anywhere in the tree under a desktop predicate.
        bad_ignore = not ignore and any(
            k == "other" and re.match(r"ignore\b", t) and any(desktop(p) for p in chain) for k, t, chain in tree
        )
        if kind == "cfg":
            excludes = on_windows(predicate) is False
        else:
            # A nested cfg leaves Windows out only where every cfg_attr
            # around it applies on Windows.
            excludes = any(
                k == "cfg" and on_windows(t) is False and all(on_windows(p) is True for p in chain) for k, t, chain in tree
            )
        found.append(
            Gate(
                start=m.start(),
                end=bracket + 1,
                line=line,
                what=f"#{'!' if inner else ''}[{kind}({' '.join(content.split())})]",
                test=(kind == "cfg" and needs_test(predicate)) or test_item,
                desktop=is_desktop or bad_ignore,
                ignore=ignore,
                bad_ignore=bad_ignore,
                data=False,
                feature=marker(source, line) if is_desktop else None,
                excludes_windows=excludes,
                extent=extent,
            )
        )
    for m in MACRO.finditer(code):
        open_at = m.end() - 1
        close = close_paren(code, open_at)
        content = bare[open_at + 1 : close]
        if not desktop(content):
            continue
        line = source.line(m.start())
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
                bad_ignore=False,
                data=macro_is_data(source, m.start()),
                feature=marker(source, line),
                excludes_windows=extent is not None and block_left_out(code, m.start(), close + 1, content),
                extent=extent,
            )
        )
    return found


def attributes_before(code: str, at: int) -> str:
    """The attributes written directly before the one at `at`."""
    start = at
    while True:
        end = len(code[:start].rstrip())
        if end == 0 or code[end - 1] != "]":
            return code[start:at]
        depth, i = 0, end - 1
        while i >= 0:
            if code[i] == "]":
                depth += 1
            elif code[i] == "[":
                depth -= 1
                if depth == 0:
                    break
            i -= 1
        hash_at = len(code[:i].rstrip()) - 1
        if i < 0 or hash_at < 0 or code[hash_at] != "#":
            return code[start:at]
        start = hash_at


def block_left_out(code: str, start: int, end: int, predicate: str) -> bool:
    """Whether the block the `if` around the `cfg!` at `start..end` opens
    is left out on Windows: its whole condition is `cfg!(..)` or
    `!cfg!(..)`, false there."""
    _, head = condition_head(code, start)
    if head is None or head.group(1) != "if":
        return False
    brace = code.find("{", end)
    condition = code[head.end() : brace].strip()
    macro = code[start:end]
    value = on_windows(predicate)
    if condition == macro:
        return value is False
    if condition.startswith("!") and condition[1:].strip() == macro:
        return value is True
    return False


USE = re.compile(r"\buse\s+([^;{}]*(?:\{[^;]*\})?[^;{}]*);", re.S)
PLATFORM_PATH = re.compile(r"(?:std::os::(unix|windows)|(libc|windows_sys))(?:::|$)")
EXPRESSION_PATH = re.compile(r"\bstd::os::(unix|windows)\b|(?<![\w:])os::(unix|windows)\b|\b(libc|windows_sys)::")


def use_paths(tree: str) -> list[str]:
    """Each path a `use` tree names: `std::{io, os::unix::fs}` is
    `std::io` and `std::os::unix::fs`."""
    tree = tree.strip()
    open_at = tree.find("{")
    if open_at < 0:
        return [re.sub(r"\s+as\s+\w+$", "", tree).replace(" ", "").replace("\n", "")]
    prefix = re.sub(r"\s+", "", tree[:open_at])
    inner = tree[open_at + 1 : tree.rfind("}")]
    paths, depth, start = [], 0, 0
    for i, c in enumerate(inner + ","):
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
        elif c == "," and depth == 0:
            part = inner[start:i].strip()
            if part:
                paths += [prefix + p for p in use_paths(part)]
            start = i + 1
    return paths


def paths(source: Source) -> list[tuple[int, str]]:
    code = source.code
    found, uses = [], []
    for m in USE.finditer(code):
        uses.append((m.start(), m.end()))
        # Once for each platform module a `use` reaches, however many names
        # it takes from it.
        named = set()
        for path in use_paths(m.group(1)):
            hit = PLATFORM_PATH.match(path.removeprefix("::"))
            if hit:
                named.add(f"use std::os::{hit.group(1)}" if hit.group(1) else f"use {hit.group(2)}")
        found += [(m.start(), what) for what in sorted(named)]
    for m in EXPRESSION_PATH.finditer(code):
        if any(start <= m.start() < end for start, end in uses):
            continue
        os_name = m.group(1) or m.group(2)
        found.append((m.start(), f"std::os::{os_name}" if os_name else f"{m.group(3)}::"))
    return sorted(found)


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
            if not (gate.test or gate.exempts) or gate.what.startswith("cfg!"):
                continue
            module = MOD_FILE.match(source.code, gate.end)
            if module is None:
                continue
            base = module_base(path) + module.group(1)
            targets = {base + ".rs", base + "/"}
            if gate.test:
                test_modules.update(targets)
            if gate.exempts:
                scoped_modules.update(targets)

    result = Result(violations={}, ignores=[], marked=[])

    def add(source: Source, line: int, what: str, test: bool):
        result.violations.setdefault(source.path, []).append(Found(source.path, line + 1, what, source.lines[line].strip(), test))

    def inside(offset: int, spans) -> bool:
        return any(start <= offset < end for start, end in spans)

    for path, source in sources.items():
        file_gates = all_gates[path]
        # The ignores on Windows first, wherever they are.
        for gate in file_gates:
            if gate.ignore:
                result.ignores.append(found(source, gate, True))
            elif gate.bad_ignore:
                add(source, gate.line, gate.what + ' (an ignore on Windows reads `ignore = "windows: <symptom>"`)', True)
        if under(path, scoped_modules):
            continue
        home = is_home(path)
        if home and per_os(path):
            continue
        # In a home's other files the code is free, and its tests are held:
        # what they test is the API callers use without `cfg`.
        held_tests_only = home and not is_integration_test(path)
        test_file = is_test_file(path) or under(path, test_modules)
        tests = [g.extent for g in file_gates if g.test and g.extent]
        scoped = [g.extent for g in file_gates if g.exempts and g.extent]

        for gate in file_gates:
            if not gate.desktop or gate.ignore or gate.bad_ignore or gate.data:
                continue
            is_test = test_file or gate.test or inside(gate.start, tests)
            if held_tests_only and not is_test:
                continue
            if gate.exempts:
                result.marked.append(found(source, gate, True))
                continue
            if inside(gate.start, scoped):
                continue
            what = gate.what
            if gate.feature == "":
                what += " (its `out of scope on Windows:` comment names no feature Windows leaves out)"
            elif gate.feature:
                what += " (marked out of scope on Windows, but what it gates is built for Windows)"
            add(source, gate.line, what, is_test)
        for offset, what in paths(source):
            if inside(offset, scoped):
                continue
            is_test = test_file or inside(offset, tests)
            if held_tests_only and not is_test:
                continue
            add(source, source.line(offset), what, is_test)
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
# that still holds desktop-platform conditionals outside the homes (or in a
# home's tests), with how many. A file's count may only fall, and a file
# not listed may hold none.
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
        print("platform-check: desktop-platform conditionals outside the homes (or in their tests), over the allowlist")
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
        f"platform-check: {total} desktop-platform conditionals held ({tests} in tests) "
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
