#!/usr/bin/env python3
"""Fail when a client crate decides something by an agent's kind or its
provider's name outside the places listed here.

Clients read what an agent can do from its `AgentCapabilities` and how a
provider is named, started and signed in to from its host's
`ProviderDescriptor`. Matching a `Kind` variant or comparing a provider id
with "claude" or "codex" instead is how a new kind of agent ends up drawn as
Claude or Codex without anyone noticing, so this check searches the client
crates for both:

- a `Kind` variant named (`Kind::ClaudePty`, `Kind::ClaudeSdk`,
  `Kind::Codex`, `Kind::Acp`), which is how a match or a comparison on the
  kind is written;
- a provider id written out (`"claude"`, `"codex"`, `agents::CLAUDE`,
  `agents::CODEX`).

Some places have to know the kind: decoding a snapshot or item body, which
is a per-kind message, and building an input in the kind's own arm. Those
are the decode boundary. Others still decide by kind or name today and are
listed so the list can only shrink. Each entry names the file, the function,
the code it allows and why. An entry that matches nothing fails the check
too, so the list cannot outlive the code it describes. Test modules, items and
files are not searched.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]

# The client crates: what draws and drives an agent on every client.
CRATES = [
    "crates/ui-state",
    "crates/ui-view",
    "crates/ui-runtime",
    "crates/tui",
    "crates/app-runtime",
    "crates/browser-core",
]

PATTERNS = [
    ("a kind matched", re.compile(r"\bKind::(ClaudePty|ClaudeSdk|Codex|Acp)\b")),
    ("a provider named", re.compile(r"\"(claude|codex)\"")),
    ("a provider named", re.compile(r"\bagents::(CLAUDE|CODEX)\b")),
]

FN = re.compile(r"^\s*(pub(\([^)]*\))?\s+)?(const\s+)?(async\s+)?(unsafe\s+)?fn\s+(\w+)")
TESTS = re.compile(r"^#\[cfg\(test\)\]")

# Why a decode-boundary site may know the kind.
DECODE = "the decode boundary: a per-kind body is decoded, or an input built, in the kind's own arm"
# Why a site that names a provider by its kind may, for now.
NAMED = "names the provider by its kind; the host's ProviderDescriptor names it instead"

# What an entry's code matches: a kind named, or a provider id written out.
KIND = r"Kind::"
ID = r"\"(claude|codex)\"|agents::(CLAUDE|CODEX)"


@dataclass(frozen=True)
class Allowed:
    path: str
    function: str
    code: str
    why: str


ALLOWED: list[Allowed] = [
    # The decode boundary.
    Allowed("crates/ui-state/src/body.rs", "decode", KIND, DECODE),
    Allowed("crates/ui-state/src/body.rs", "decode_snapshot", KIND, DECODE),
    Allowed("crates/ui-state/src/body.rs", "snapshot_structure", KIND, DECODE),
    Allowed("crates/ui-runtime/src/inputs.rs", "wrap", KIND, DECODE),
    Allowed("crates/ui-runtime/src/inputs.rs", "stop_job", KIND,
            "builds the kind's own stop input; whether a stop is offered is AgentCapabilities.stops_job"),
    Allowed("crates/ui-view/src/ask.rs", "answer_input", KIND, DECODE),
    Allowed("crates/ui-view/src/ask.rs", "question_answer", KIND, DECODE),
    Allowed("crates/ui-view/src/ask.rs", "reply_answer", KIND, DECODE),
    Allowed("crates/ui-view/src/settings.rs", "setting_input", KIND, DECODE),
    Allowed("crates/browser-core/src/lib.rs", "prompt", KIND, DECODE),
    Allowed("crates/app-runtime/src/lib.rs", "create_agent", KIND,
            "builds the create request's arm for the kind the person picked"),
    Allowed("crates/tui/src/setup.rs", "request", KIND,
            "builds the create request's arm for the kind the person picked"),
    # Reading a host's Claude entry, which is Claude's to describe.
    Allowed("crates/app-runtime/src/lib.rs", "hosts", ID,
            "which kind a host runs Claude as is read from Claude's own descriptor"),
    Allowed("crates/tui/src/setup.rs", "started", ID,
            "which kind a host runs Claude as is read from Claude's own descriptor"),
    Allowed("crates/browser-core/src/lib.rs", "answer", f"{KIND}|{ID}",
            "the browser extension starts Claude alone, as the host runs it"),
    # A recorded report keeps amux's own ids and hides a person's.
    Allowed("crates/ui-state/src/session/record.rs", "kept_provider", ID,
            "a recorded report keeps amux's own provider ids and hides an id a person chose"),
    # The terminal's new-agent setup, which offers Claude and Codex.
    Allowed("crates/tui/src/setup.rs", "kind", KIND,
            "the terminal's new-agent setup offers Claude and Codex only; other agents start from amux new"),
    Allowed("crates/tui/src/setup.rs", "of", KIND,
            "the terminal's new-agent setup offers Claude and Codex only; other agents start from amux new"),
    Allowed("crates/tui/src/setup.rs", "provider", ID,
            "the terminal's new-agent setup offers Claude and Codex only; other agents start from amux new"),
    # Decisions the capabilities and descriptor will take over.
    Allowed("crates/ui-runtime/src/inputs.rs", "switch_back", KIND,
            "decides by kind whether a chat can switch back to the terminal; AgentCapabilities.switches_back says it"),
    Allowed("crates/tui/src/app.rs", "terminal_elsewhere", KIND,
            "decides by kind whether an agent has a terminal; AgentCapabilities.attachable says it"),
    Allowed("crates/ui-view/src/settings.rs", "settings", KIND,
            "what a chat can change, and how its permission reads, follow each kind's own settings; an ACP agent's are not drawn yet"),
    Allowed("crates/tui/src/chat/ask.rs", "box_label", KIND,
            "Codex approves a plan without naming a mode, which its label words"),
    Allowed("crates/ui-view/src/fleet.rs", "kind_name", KIND, NAMED),
    Allowed("crates/ui-view/src/rows.rs", "amux_call", ID, NAMED),
    Allowed("crates/tui/src/chat/words.rs", "provider_name", KIND, NAMED),
    Allowed("crates/tui/src/chat/words.rs", "refused_command_words", KIND, NAMED),
    Allowed("crates/tui/src/chat/words.rs", "sign_in_notice", f"{KIND}|{ID}",
            "words how to sign in by kind; the descriptor's sign_in_hint says it"),
    Allowed("crates/tui/src/home.rs", "provider_word", KIND, NAMED),
    Allowed("crates/tui/src/home.rs", "agent_lines", KIND, NAMED),
    # The storybook, which draws each kind's vocabulary from fixtures of it.
    *(
        Allowed("crates/tui/src/vocabulary.rs", function, f"{KIND}|{ID}",
                "storybook fixtures: each kind's vocabulary drawn from an authored agent of that kind")
        for function in (
            "around_a_chat", "authored_agent", "authored_home", "card", "cards",
            "components", "control_chats", "group", "native_composers",
        )
    ),
]


TEST_MOD = re.compile(r"^mod\s+(\w+)\s*;")


def test_modules(path: Path) -> set[Path]:
    """The files of the modules `path` declares under `#[cfg(test)]`."""
    lines = path.read_text().splitlines()
    owned = path.parent if path.name in ("lib.rs", "main.rs", "mod.rs") else path.with_suffix("")
    found = set()
    for at, line in enumerate(lines[:-1]):
        if TESTS.match(line) and (match := TEST_MOD.match(lines[at + 1])):
            found.add(owned / f"{match.group(1)}.rs")
            found.add(owned / match.group(1) / "mod.rs")
    return found


def sources(root: Path) -> list[Path]:
    found = []
    for crate in CRATES:
        src = root / crate / "src"
        if not src.is_dir():
            raise SystemExit(f"client-kind-check: {crate}/src does not exist")
        every = sorted(src.rglob("*.rs"))
        tests = set().union(*(test_modules(path) for path in every))
        for path in every:
            parts = path.relative_to(src).parts
            # Test files and directories, which build fixtures of every kind.
            if path in tests or any(part in ("tests", "tests.rs") for part in parts):
                continue
            if any(parent in tests for parent in path.parents):
                continue
            found.append(path)
    return found


def hits(path: Path, relative: str):
    """Every line outside a test item that names a kind or a provider, with
    its function."""
    function = ""
    skipping = None
    lines = path.read_text().splitlines()
    for number, line in enumerate(lines, start=1):
        if skipping == "item":
            # The item `#[cfg(test)]` marks: one line, or a block that ends
            # where it began, at the margin.
            skipping = "block" if line.rstrip().endswith("{") else None
            continue
        if skipping == "block":
            if line == "}":
                skipping = None
            continue
        if TESTS.match(line):
            skipping = "item"
            continue
        stripped = line.strip()
        if stripped.startswith("//"):
            continue
        if match := FN.match(line):
            function = match.group(6)
        for what, pattern in PATTERNS:
            if pattern.search(line):
                yield number, function, what, stripped
                break


def check(root: Path = ROOT) -> int:
    used = set()
    failures = []
    for path in sources(root):
        relative = path.relative_to(root).as_posix()
        for number, function, what, code in hits(path, relative):
            allowed = next(
                (
                    entry
                    for entry in ALLOWED
                    if entry.path == relative
                    and entry.function == function
                    and re.search(entry.code, code)
                ),
                None,
            )
            if allowed is None:
                failures.append(
                    f"{relative}:{number} in {function or '(top level)'}: {what}\n    {code}"
                )
            else:
                used.add(allowed)
    stale = [entry for entry in ALLOWED if entry not in used]
    for failure in failures:
        print(failure)
    for entry in stale:
        print(f"stale entry: {entry.path} {entry.function} /{entry.code}/ matches nothing")
    if failures or stale:
        print(
            f"client-kind-check: {len(failures)} decided by kind or provider name, "
            f"{len(stale)} stale entries",
            file=sys.stderr,
        )
        return 1
    print(f"client-kind-check: ok ({len(sources(root))} files, {len(ALLOWED)} listed places)")
    return 0


if __name__ == "__main__":
    sys.exit(check())
