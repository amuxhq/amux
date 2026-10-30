#!/usr/bin/env python3
"""Fail when an interpreter, the agent's provider handshake or Claude's
messaging socket handles provider JSON by hand.

The interpreters read and write Codex's and Claude's messages through the
`codex-protocol` and `claude-protocol` crates, the agent process builds its
handshake from them, and the `claude` crate writes Claude's messaging socket
from them. This check searches those sources for the ways of going around the
types: building JSON with `json!`, looking a field up by its name
(`.get("…")`, `["…"]`, `.pointer(…)`, `.remove("…")`, or a path of names
passed as `&["…", …]`), opening a value as an object or array, and parsing
bytes into a `serde_json::Value`.

Some JSON is not a provider message and stays a value on purpose: a tool's
input and result are whatever the tool wrote, a tool server's form schema and
content are whatever the server wrote and asks for, and Claude's settings file
and Codex's `--config` arguments are launch configuration. Each such place is
an exemption below, naming the function, the code it allows and why. An
exemption that no longer matches anything fails the check too, so the list
cannot outlive the code it describes. Test modules are not searched.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]

# The sources held to the types: every file under a directory, or one file.
SOURCES = [
    "crates/interpret/src/codex",
    "crates/interpret/src/claude_sdk",
    "crates/interpret/src/claude_pty",
    "crates/interpret/src/claude_common.rs",
    "crates/interpret/src/claude_answers.rs",
    "crates/agent/src/provider.rs",
    "crates/claude/src/messaging.rs",
]

# What going around the types looks like.
PATTERNS = [
    ("json! builds JSON by hand", re.compile(r"\bjson!")),
    ("a field looked up by name", re.compile(r"\.(get|get_mut|remove)\(\s*\"")),
    ("a field looked up by name", re.compile(r"[\w)\]]\[\s*\"")),
    ("a field looked up by path", re.compile(r"\.pointer(_mut)?\(")),
    ("a field looked up by path", re.compile(r"[(,]\s*&\[\s*\"")),
    ("a value opened as an object or array", re.compile(r"\.as_(object|array)(_mut)?\(")),
    ("a value opened as an object or array", re.compile(r"\bValue::(Object|Array)\b")),
    (
        "bytes parsed into an untyped value",
        re.compile(r"from_(slice|str|value|reader)::<\s*(serde_json::)?Value\s*>"),
    ),
    (
        "bytes parsed into an untyped value",
        re.compile(r":\s*(serde_json::)?Value\s*=\s*serde_json::from_"),
    ),
]

FN = re.compile(r"^\s*(pub(\([^)]*\))?\s+)?(const\s+)?(async\s+)?(unsafe\s+)?fn\s+(\w+)")
TESTS = re.compile(r"^#\[cfg\(test\)\]")


@dataclass(frozen=True)
class Exemption:
    path: str
    function: str
    code: str
    why: str


EXEMPTIONS = [
    Exemption(
        "crates/interpret/src/claude_common.rs",
        "without_image_bytes",
        r".",
        "Read's result is a tool payload kept as written; only the base64 copy of an image it read is cut out",
    ),
    Exemption(
        "crates/interpret/src/claude_common.rs",
        "same_json",
        r"from_str::<Value>",
        "compares tool inputs, which stay JSON as the model wrote them",
    ),
    Exemption(
        "crates/interpret/src/claude_pty/facts.rs",
        "task_tool",
        r"from_str::<Value>\(&tool\.input\)",
        "a task tool's input, kept as text on its call, read back for the task list",
    ),
    Exemption(
        "crates/interpret/src/claude_sdk/facts.rs",
        "tool_result",
        r"from_str::<Value>\(&tool\.input\)",
        "a task tool's input, kept as text on its call, read back for the task list",
    ),
    Exemption(
        "crates/interpret/src/claude_answers.rs",
        "decide",
        r"from_str::<Value>\(input\)|Value::Object\(input\)",
        "AskUserQuestion's input is handed back to the tool with the answers added",
    ),
    Exemption(
        "crates/interpret/src/claude_sdk/mod.rs",
        "leave_plan",
        r"from_str::<Value>\(&meta\.input\)",
        "ExitPlanMode's input is handed back to the tool unchanged when the plan is approved",
    ),
    Exemption(
        "crates/interpret/src/claude_answers.rs",
        "decide",
        r"Value::Object\(serde_json::Map::new\(\)\)",
        "a tool server's form content is whatever its schema asks for",
    ),
    Exemption(
        "crates/interpret/src/codex/mod.rs",
        "codex_answer_response",
        r"Value::Object\(Default::default\(\)\)",
        "a tool server's form content is whatever its schema asks for",
    ),
    Exemption(
        "crates/interpret/src/codex/facts.rs",
        "elicitation",
        r"json_as_written\(payload, &\[\"params\", \"requestedSchema\"\]\)",
        "a tool server's form schema is kept as the server wrote it; a Value would sort its keys and so the form's fields",
    ),
    Exemption(
        "crates/interpret/src/claude_sdk/facts.rs",
        "control_request_in",
        r"json_as_written\(payload, &\[\"request\", \"requested_schema\"\]\)",
        "a tool server's form schema is kept as the server wrote it; a Value would sort its keys and so the form's fields",
    ),
    Exemption(
        "crates/interpret/src/codex/facts.rs",
        "no_content",
        r"Value::Object\(Default::default\(\)\)",
        "the empty form content an approval accepts with",
    ),
    Exemption(
        "crates/interpret/src/claude_sdk/recording.rs",
        "read",
        r"from_str::<serde_json::Value>\(written\)",
        "a recorded line becomes the fact's payload as written; it is decoded through the types beside it",
    ),
    Exemption(
        "crates/interpret/src/claude_pty/mod.rs",
        "bind_asks_for",
        r"from_str::<Value>\(&meta\.input\)",
        "a tool's input, kept as text on its ask, compared with the call's to bind them",
    ),
    Exemption(
        "crates/interpret/src/claude_pty/recording.rs",
        "read",
        r"from_str::<Value>\(line\)",
        "a recorded row becomes the fact's payload as written; it is decoded through the types where it is read",
    ),
    Exemption(
        "crates/agent/src/provider.rs",
        "claude_launch",
        r"json!",
        "Claude's settings file is launch configuration, not a message",
    ),
    Exemption(
        "crates/agent/src/provider.rs",
        "codex_args",
        r"json!",
        "Codex reads `--config` values as TOML; json! quotes them",
    ),
]


def sources(root: Path) -> list[Path]:
    found = []
    for source in SOURCES:
        path = root / source
        if path.is_dir():
            found.extend(sorted(path.rglob("*.rs")))
        elif path.is_file():
            found.append(path)
        else:
            raise SystemExit(f"typed-provider-check: {source} does not exist")
    return found


def hits(path: Path, relative: str):
    """Every line that goes around the types, with its function."""
    function = ""
    for number, line in enumerate(path.read_text().splitlines(), start=1):
        if TESTS.match(line):
            return
        stripped = line.strip()
        if stripped.startswith("//"):
            continue
        if match := FN.match(line):
            function = match.group(6)
        for what, pattern in PATTERNS:
            if pattern.search(line):
                yield relative, number, function, what, stripped
                break


def check(root: Path = ROOT) -> int:
    used = set()
    failures = []
    for path in sources(root):
        relative = path.relative_to(root).as_posix()
        for hit in hits(path, relative):
            _, number, function, what, code = hit
            exemption = next(
                (
                    exemption
                    for exemption in EXEMPTIONS
                    if exemption.path == relative
                    and exemption.function == function
                    and re.search(exemption.code, code)
                ),
                None,
            )
            if exemption is None:
                failures.append(f"{relative}:{number} in {function or '(top level)'}: {what}\n    {code}")
            else:
                used.add(exemption)
    stale = [exemption for exemption in EXEMPTIONS if exemption not in used]
    for failure in failures:
        print(failure)
    for exemption in stale:
        print(
            f"stale exemption: {exemption.path} {exemption.function} /{exemption.code}/ matches nothing"
        )
    if failures or stale:
        print(
            f"typed-provider-check: {len(failures)} hand-handled provider JSON, {len(stale)} stale exemptions",
            file=sys.stderr,
        )
        return 1
    print(f"typed-provider-check: ok ({len(sources(root))} files, {len(EXEMPTIONS)} exemptions)")
    return 0


if __name__ == "__main__":
    sys.exit(check())
