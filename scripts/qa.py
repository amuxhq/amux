#!/usr/bin/env python3
"""Run adversarial QA: waves of Sonnet testers, each exploring amux on a test
network of its own through `testnet ctl`, keeping evidence of what it finds.

`scripts/qa.py [--testers N] [--phone M] [--persona P ...] [--waves W]`

It builds every binary once (debug, with fixed features) and the phone app
when phone testers run, launches each fresh binary once, and checks the
operator's Claude and Codex logins with one small real agent each. It then
makes the run folder under ~/.local/state/amux-qa/<run>/, holding
`reference/` (the docs and the QA guides), `ledger.jsonl` and a folder per
tester, and starts the testers a wave at a time. A phone tester runs inside
a simulator lease for its whole life. A wave ends when its testers do; the
next starts with fresh personas, until a wave records no new finding,
`--waves` is reached, or Ctrl-C, which stops every tester and shuts every
net down. A summary is printed and kept in the run folder.

Real providers are used throughout; see docs/QA.md.
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
QA = ROOT / "qa"
TARGET = ROOT / "target" / "debug"
APP = ROOT / "target" / "ios" / "DerivedData" / "Build" / "Products" / "Debug-iphonesimulator" / "Amux.app"
STATE = Path.home() / ".local" / "state" / "amux-qa"
SEVERITIES = ["data loss", "broken flow", "wrong state", "visual", "wording"]
# What a tester's shell may run besides testnet: reading, never changing.
READ_ONLY = ["ls", "cat", "head", "tail", "grep", "jq", "wc", "diff", "file"]
BINARIES = ["amux", "testnet", "dump-show", "claude-probe", "codex-probe",
            "fake-claude-sdk", "fake-claude-pty", "fake-codex"]


def say(text: str) -> None:
    print(f"qa: {text}", flush=True)


def build(phone: bool) -> None:
    """One debug build with fixed features, so no net ever rebuilds."""
    say("building the binaries")
    subprocess.run(
        [str(ROOT / "scripts" / "bounded"), "1800", "cargo", "build", "--locked",
         "-p", "amux", "-p", "provider-fakes", "-p", "testnet", "-p", "claude-specs",
         "-p", "codex-specs", "-p", "replay-support", "--bins", "--features", "amux/bundled"],
        cwd=ROOT, check=True,
    )
    if phone:
        say("building the phone app")
        subprocess.run(["just", "ios", "build"], cwd=ROOT, check=True)
        if not APP.is_dir():
            sys.exit(f"qa: the phone app is not at {APP}")


def warm() -> None:
    """A fresh binary's first launch can stall on macOS while it is checked;
    take that stall here, once, rather than inside a tester's first command."""
    say("launching each fresh binary once")
    for name in BINARIES:
        binary = TARGET / name
        if not binary.exists():
            sys.exit(f"qa: {binary} was not built")
        try:
            subprocess.run([str(binary), "--help"], stdin=subprocess.DEVNULL,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=120)
        except subprocess.TimeoutExpired:
            say(f"{name} took over two minutes to launch the first time; going on")


def ctl(folder: Path, env: dict[str, str], *args: str, timeout: int = 300) -> subprocess.CompletedProcess:
    return subprocess.run([str(TARGET / "testnet"), "ctl", *args], cwd=folder, env=env,
                          capture_output=True, text=True, timeout=timeout)


def check_logins(run: Path, env: dict[str, str]) -> None:
    """One small real agent of each provider, the way the testers will make
    them: a failed login shows here rather than as every tester's finding."""
    say("checking the Claude and Codex logins")
    folder = run / "login-check"
    folder.mkdir()
    env = dict(env, AMUX_QA_SCOPE=f"qa-{run.name}-login")
    try:
        for args in (["up"], ["host", "add", "check", "--provider", "real"]):
            done = ctl(folder, env, *args)
            if done.returncode != 0:
                sys.exit(f"qa: ctl {' '.join(args)} failed:\n{done.stdout}{done.stderr}")
        for name, kind in (("claude", "claude-sdk"), ("codex", "codex")):
            done = ctl(folder, env, "spawn", "--host", "check", "--kind", kind, "--wait",
                       "--prompt", "Reply with only the word ok.", name)
            chat = ctl(folder, env, "chat", "check", name)
            if done.returncode != 0 or ": ok" not in chat.stdout.lower():
                sys.exit(f"qa: a real {name} agent did not answer; is the login good?\n"
                         f"{done.stdout}{done.stderr}{chat.stdout}")
    finally:
        ctl(folder, env, "down", timeout=120)


def personas(chosen: list[str]) -> list[Path]:
    every = sorted((QA / "personas").glob("*.md"))
    if not chosen:
        return every
    picked = []
    for name in chosen:
        path = QA / "personas" / f"{name}.md"
        if not path.is_file():
            sys.exit(f"qa: no persona {name}; there are {', '.join(p.stem for p in every)}")
        picked.append(path)
    return picked


def settings(run: Path, folder: Path) -> dict:
    """The tester's permissions: testnet and reading, anywhere in the run;
    writing only in its own folder; nothing else, and nothing asks."""
    return {
        "permissions": {
            "defaultMode": "dontAsk",
            "allow": ["Bash(testnet:*)", *[f"Bash({tool}:*)" for tool in READ_ONLY],
                      f"Read(/{run}/**)", f"Edit(/{folder}/**)"],
            "deny": ["WebFetch", "WebSearch", "Task", "Agent", "Bash(git:*)"],
        },
        # A wait on a real agent's turn may take four minutes; the shell
        # tool's own two-minute default would cut it off.
        "env": {"BASH_DEFAULT_TIMEOUT_MS": "330000", "BASH_MAX_TIMEOUT_MS": "600000"},
    }


def prompt(run: Path, folder: Path, persona: Path, phone: bool) -> str:
    parts = [(QA / "PROMPT.md").read_text(), "\n---\n\n# Your persona\n\n", persona.read_text()]
    for name in ("ATTACKS.md", "AGENTS-GUIDE.md"):
        parts += ["\n---\n\n", (QA / name).read_text()]
    phone_line = ("You hold a phone: `testnet ctl phone open` installs and launches the app on the "
                  "simulator leased to you." if phone else
                  "You have no phone. Use the terminal client and the host's record.")
    parts.append(f"""
---

# This run

- Your folder, where you work and where `testnet ctl` keeps your net: `{folder}`.
- The run folder, with every tester's folder in it: `{run}`.
- The docs: `{run}/reference/docs` (start at README.md); the QA guides: `{run}/reference/qa`.
- The ledger: `testnet ctl ledger read`.
- {phone_line}
- Run `testnet ctl --help`, and `--help` on any command, before guessing a flag.
""")
    return "".join(parts)


class Tester:
    def __init__(self, run: Path, number: int, persona: Path, phone: bool, tree: str | None):
        self.name = f"t{number}"
        self.run = run
        self.folder = run / self.name
        self.persona = persona
        self.phone = phone
        self.tree = tree
        self.process: subprocess.Popen | None = None
        self.started = 0.0

    def start(self, env: dict[str, str]) -> None:
        self.folder.mkdir()
        (self.folder / "settings.json").write_text(json.dumps(settings(self.run, self.folder), indent=2))
        text = prompt(self.run, self.folder, self.persona, self.phone)
        (self.folder / "prompt.md").write_text(text)
        env = dict(env, AMUX_QA_TESTER=self.name, AMUX_QA_SCOPE=f"qa-{self.run.name}-{self.name}")
        command = ["claude", "-p", "--model", "sonnet", "--setting-sources", "project",
                   "--strict-mcp-config", "--disable-slash-commands",
                   "--tools", "Bash,Read,Write,Edit,Glob,Grep",
                   "--settings", str(self.folder / "settings.json"), "--add-dir", str(self.run),
                   "--output-format", "stream-json", "--verbose"]
        if self.phone:
            # The lease is held for the tester's whole life, so every
            # `ctl phone` it runs uses the one simulator it was given.
            # `wt exec` runs in the tree's root; the tester goes back to its
            # own folder, outside the repository, before it starts.
            command = ["wt", "exec", self.tree, "--", str(ROOT / "scripts" / "with"), "iphone", "--",
                       "sh", "-c", 'cd "$0" && exec "$@"', str(self.folder), *command]
        self.started = time.monotonic()
        self.process = subprocess.Popen(
            command, cwd=self.folder, env=env, stdin=subprocess.PIPE,
            stdout=(self.folder / "transcript.jsonl").open("wb"),
            stderr=(self.folder / "launcher.err").open("wb"),
            start_new_session=True,
        )
        assert self.process.stdin is not None
        self.process.stdin.write(text.encode())
        self.process.stdin.close()
        say(f"{self.name} started as {self.persona.stem}{' with a phone' if self.phone else ''}")

    def stop(self) -> None:
        if self.process and self.process.poll() is None:
            try:
                os.killpg(self.process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass

    def shut_net(self, env: dict[str, str]) -> None:
        if (self.folder / ".testnet" / "net.json").exists():
            ctl(self.folder, dict(env, AMUX_QA_TESTER=self.name), "down", timeout=180)

    def outcome(self) -> str:
        result = "no result line"
        try:
            for line in (self.folder / "transcript.jsonl").read_text().splitlines():
                event = json.loads(line)
                if event.get("type") == "result":
                    result = (f"{event.get('subtype', '?')}, {event.get('num_turns', '?')} turns, "
                              f"${event.get('total_cost_usd', 0):.2f}")
        except (OSError, json.JSONDecodeError):
            pass
        minutes = (time.monotonic() - self.started) / 60
        return f"{self.name} ({self.persona.stem}{', phone' if self.phone else ''}): {result}, {minutes:.0f} min"


def memory_in_use() -> int:
    """Bytes of memory the machine has in use: active, wired and compressed."""
    try:
        text = subprocess.run(["vm_stat"], capture_output=True, text=True, check=True).stdout
    except (OSError, subprocess.CalledProcessError):
        return 0
    page = 4096
    counts = {}
    for line in text.splitlines():
        if "page size of" in line:
            page = int(line.split("page size of")[1].split()[0])
        elif ":" in line:
            key, value = line.split(":", 1)
            try:
                counts[key.strip()] = int(value.strip().rstrip("."))
            except ValueError:
                pass
    used = sum(counts.get(key, 0) for key in
               ("Pages active", "Pages wired down", "Pages occupied by compressor"))
    return used * page


def ledger(run: Path) -> list[dict]:
    entries = []
    for line in (run / "ledger.jsonl").read_text().splitlines():
        try:
            entries.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    return entries


def tree_name() -> str | None:
    """The wt tree this checkout is, for a phone tester's lease."""
    trees = Path(os.environ.get("WT_HOME", Path.home() / ".wt")) / "trees"
    try:
        return str(ROOT.relative_to(trees))
    except ValueError:
        return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--testers", type=int, default=8, help="testers in each wave")
    parser.add_argument("--phone", type=int, default=2, help="how many of them hold a phone")
    parser.add_argument("--persona", action="append", default=[], help="a persona to use (repeatable)")
    parser.add_argument("--waves", type=int, default=0, help="stop after this many waves (0: until one finds nothing new)")
    args = parser.parse_args()
    if args.phone > args.testers:
        sys.exit("qa: --phone cannot exceed --testers")
    if args.phone > 2:
        sys.exit("qa: there are two simulator leases, so at most two phone testers")
    tree = tree_name()
    if args.phone and tree is None:
        sys.exit("qa: phone testers need this checkout to be a wt tree, for the simulator lease")
    pool = personas(args.persona)

    build(args.phone > 0)
    warm()
    run = STATE / datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    (run / "reference").mkdir(parents=True)
    shutil.copytree(ROOT / "docs", run / "reference" / "docs")
    shutil.copytree(QA, run / "reference" / "qa")
    (run / "ledger.jsonl").touch()
    bin_dir = run / "bin"
    bin_dir.mkdir()
    # A script rather than a link: testnet finds the binaries it runs
    # beside its own executable.
    (bin_dir / "testnet").write_text(f'#!/bin/sh\nexec "{TARGET / "testnet"}" "$@"\n')
    (bin_dir / "testnet").chmod(0o755)
    # A launcher started from inside another Claude Code session must not
    # hand its testers that session's identity, nor its own amux or tmux.
    env = {key: value for key, value in os.environ.items()
           if key not in ("AMUX_CONFIG", "AMUX_LOG", "TMUX")
           and not key.startswith(("CLAUDECODE", "CLAUDE_CODE_"))
           and key not in ("CLAUDE_PID", "CLAUDE_EFFORT")}
    env.update(PATH=f"{bin_dir}:{env.get('PATH', '')}", TESTNET_BINARIES=str(TARGET),
               AMUX_QA_RUN=str(run), AMUX_QA_APP=str(APP))
    say(f"run folder {run}")
    check_logins(run, env)

    testers: list[Tester] = []
    peaks: list[int] = []
    stopping = threading.Event()

    def interrupt(_signum, _frame):
        if stopping.is_set():
            return
        stopping.set()
        say("stopping every tester")
        for tester in testers:
            tester.stop()

    signal.signal(signal.SIGINT, interrupt)
    signal.signal(signal.SIGTERM, interrupt)
    wave = 0
    next_persona = 0
    while not stopping.is_set():
        wave += 1
        before = sum(1 for entry in ledger(run) if entry.get("kind") == "finding")
        current = []
        for slot in range(args.testers):
            tester = Tester(run, len(testers) + 1, pool[next_persona % len(pool)], slot < args.phone, tree)
            next_persona += 1
            testers.append(tester)
            current.append(tester)
            tester.start(env)
        peak = 0
        while any(t.process and t.process.poll() is None for t in current):
            peak = max(peak, memory_in_use())
            time.sleep(5)
        peaks.append(peak)
        for tester in current:
            tester.shut_net(env)
            say(f"{tester.outcome()}")
        found = sum(1 for entry in ledger(run) if entry.get("kind") == "finding") - before
        say(f"wave {wave}: {found} new findings, peak memory {peak / 2**30:.1f} GiB")
        if stopping.is_set() or found == 0 or (args.waves and wave >= args.waves):
            break

    findings = [entry for entry in ledger(run) if entry.get("kind") == "finding"]
    by_severity: dict[str, int] = {}
    for entry in findings:
        severity = (entry.get("severity") or "").strip().lower()
        key = next((name for name in SEVERITIES if name in severity), "unlabelled")
        by_severity[key] = by_severity.get(key, 0) + 1
    lines = [f"run {run.name}: {len(testers)} testers in {wave} waves, {len(findings)} findings"]
    lines += [f"  {name}: {by_severity[name]}" for name in [*SEVERITIES, "unlabelled"] if name in by_severity]
    lines += [f"  {entry.get('tester')}: {entry.get('finding')} — {entry.get('title', '')}" for entry in findings]
    lines += [tester.outcome() for tester in testers]
    lines += [f"wave {n + 1} peak memory in use: {peak / 2**30:.1f} GiB" for n, peak in enumerate(peaks)]
    lines.append(f"run folder: {run}")
    (run / "summary.txt").write_text("\n".join(lines) + "\n")
    print("\n".join(lines))
    return 130 if stopping.is_set() else 0


if __name__ == "__main__":
    sys.exit(main())
