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
next starts with fresh personas, until a wave records no new finding (one
its tester marked a duplicate of an earlier one does not count), `--waves`
is reached, or Ctrl-C, which stops every tester and shuts every net down.
While a wave runs it samples memory: how far the machine's use rises over
its level when the wave began, and each tester's own peak. A summary is
printed and kept in the run folder.

Real providers are used throughout; see docs/QA.md.
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
import re
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
    """The tester's permissions. Claude Code's auto mode judges each other
    action against the tester's instructions, so ordinary shell work (loops,
    pipes, waits) runs without an ever-growing list and nothing waits on a
    person. `testnet` is allowed outright so the classifier never holds up
    the tool every action goes through; reading the run and writing the
    tester's folder likewise; git and the web are refused."""
    return {
        "permissions": {
            "defaultMode": "auto",
            "allow": ["Bash(testnet:*)", f"Read(/{run}/**)", f"Edit(/{folder}/**)"],
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
        self.ended: float | None = None
        self.udid: str | None = None
        # The largest total seen, with its parts at that moment.
        self.peak: dict[str, int] = {}

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

    def stop(self) -> list[int]:
        """Ends the tester's whole process tree and returns what is left of
        it. A phone tester's Claude runs under `wt exec` and the simulator
        script, which start their own process groups, so its group alone
        would leave the session running."""
        # An ended tester's pid may since belong to anything else.
        if self.process is None or self.process.poll() is not None:
            return []
        tree = process_tree(self.process.pid)
        for sig, patience in ((signal.SIGTERM, 10.0), (signal.SIGKILL, 5.0)):
            for pid in tree:
                try:
                    os.kill(pid, sig)
                except ProcessLookupError:
                    pass
            deadline = time.monotonic() + patience
            while time.monotonic() < deadline:
                self.process.poll()
                tree = [pid for pid in tree if alive(pid)]
                if not tree:
                    return []
                time.sleep(0.2)
        return tree

    def finished(self) -> bool:
        """Whether the tester has ended, noting when it first did."""
        if self.process is None or self.process.poll() is None:
            return False
        if self.ended is None:
            self.ended = time.monotonic()
        return True

    def shut_net(self, env: dict[str, str]) -> None:
        if (self.folder / ".testnet" / "net.json").exists():
            ctl(self.folder, dict(env, AMUX_QA_TESTER=self.name), "down", timeout=180)

    def outcome(self) -> str:
        try:
            lines = (self.folder / "transcript.jsonl").read_text().splitlines()
        except OSError:
            lines = []
        result, session_ms = results(lines)
        if session_ms is not None:
            minutes = session_ms / 60_000
        else:
            minutes = ((self.ended or time.monotonic()) - self.started) / 60
        memory = ""
        if self.peak:
            parts = ", ".join(f"{name} {gib(size)}" for name, size in self.peak.items() if name != "total")
            memory = f"; peak {gib(self.peak['total'])} GiB ({parts})"
            if self.phone and "simulator" not in self.peak:
                memory += ", simulator not attributed: the phone never opened"
        return (f"{self.name} ({self.persona.stem}{', phone' if self.phone else ''}): "
                f"{result}, {minutes:.0f} min{memory}")


def results(lines: list[str]) -> tuple[str, float | None]:
    """A tester's result in words, and how long its session ran until its
    closing summary, from its transcript.

    A tester can be woken again after its closing summary, by a background
    task's notification, and each wake ends in a result of its own: the
    turns and cost are the sum of every result, and the session's length is
    the first's, so time spent idle after the summary is not counted."""
    found = []
    for line in lines:
        # A phone tester's stream also carries the lease's own lines (wt and
        # the simulator script), which are not JSON.
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(event, dict) and event.get("type") == "result":
            found.append(event)
    if not found:
        return "no result line", None
    turns = sum(event.get("num_turns") or 0 for event in found)
    cost = sum(event.get("total_cost_usd") or 0 for event in found)
    wakes = f" over {len(found)} wakes" if len(found) > 1 else ""
    duration = found[0].get("duration_ms")
    return (f"{found[0].get('subtype', '?')}, {turns} turns, ${cost:.2f}{wakes}",
            float(duration) if isinstance(duration, (int, float)) else None)


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


UNITS = {"B": 1, "K": 2**10, "M": 2**20, "G": 2**30}


def processes() -> dict[int, tuple[int, int, str]]:
    """Every process: its parent, its memory and its command line.

    Memory is the physical footprint `top` reports (what Activity Monitor
    shows), not the resident size: resident sizes count the pages processes
    share once for each of them, which for a simulator's few hundred
    processes adds up to several times the machine's memory."""
    try:
        listed = subprocess.run(["ps", "-axo", "pid=,ppid=,command="],
                                capture_output=True, text=True, check=True).stdout
        top = subprocess.run(["top", "-l", "1", "-stats", "pid,mem", "-n", "100000"],
                             capture_output=True, text=True, check=True).stdout
    except (OSError, subprocess.CalledProcessError):
        return {}
    footprint = {}
    for line in top.splitlines():
        match = re.match(r"\s*(\d+)\s+([\d.]+)([BKMG])[+-]?\s*$", line)
        if match:
            footprint[int(match[1])] = int(float(match[2]) * UNITS[match[3]])
    table = {}
    for line in listed.splitlines():
        parts = line.split(None, 2)
        if len(parts) >= 2:
            pid, parent = int(parts[0]), int(parts[1])
            table[pid] = (parent, footprint.get(pid, 0), parts[2] if len(parts) > 2 else "")
    return table


def process_tree(root: int) -> list[int]:
    """`root` and every process below it, by parent links."""
    try:
        listed = subprocess.run(["ps", "-axo", "pid=,ppid="], capture_output=True, text=True,
                                check=True).stdout
    except (OSError, subprocess.CalledProcessError):
        return [root]
    children: dict[int, list[int]] = {}
    for line in listed.splitlines():
        parts = line.split()
        if len(parts) == 2:
            children.setdefault(int(parts[1]), []).append(int(parts[0]))
    tree, queue = [], [root]
    while queue:
        pid = queue.pop()
        if pid not in tree:
            tree.append(pid)
            queue.extend(children.get(pid, []))
    return tree


def alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    # A child that has exited but not been reaped still answers; it is gone.
    try:
        state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True,
                               text=True).stdout.strip()
    except OSError:
        return True
    return bool(state) and not state.startswith("Z")


def descendants(table: dict[int, tuple[int, int, str]], roots: set[int]) -> set[int]:
    children: dict[int, list[int]] = {}
    for pid, (parent, _, _) in table.items():
        children.setdefault(parent, []).append(pid)
    found, queue = set(), [pid for pid in roots if pid in table]
    while queue:
        pid = queue.pop()
        if pid not in found:
            found.add(pid)
            queue.extend(children.get(pid, []))
    return found


def tester_memory(table: dict[int, tuple[int, int, str]], tester: Tester) -> dict[str, int]:
    """What one tester's own processes hold, by part: its Claude session
    (with every command it runs), its net (`testnet serve`, process and
    supervised daemons, the agents and their providers: everything whose
    command line lies in its folder, with their children) and, for a phone
    tester, its leased simulator's own processes (the shared Simulator app
    and CoreSimulator service are nobody's)."""
    if tester.process is None:
        return {}
    session = descendants(table, {tester.process.pid})
    # The folder as a whole path component: t1's must not match t10's.
    mine = re.compile(re.escape(str(tester.folder)) + r"(?:/|\s|$)")
    net = descendants(table, {pid for pid, (_, _, command) in table.items()
                              if mine.search(command)}) - session
    parts = {"session": session, "net": net}
    if tester.phone:
        try:
            tester.udid = json.loads((tester.folder / ".testnet" / "phone.json").read_text())["udid"]
        except (OSError, ValueError, KeyError):
            pass
        if tester.udid:
            parts["simulator"] = descendants(table, {pid for pid, (_, _, command) in table.items()
                                                     if tester.udid in command}) - session - net
    return {name: sum(table[pid][1] for pid in pids) for name, pids in parts.items()}


def count(number: int, noun: str) -> str:
    return f"{number} {noun}{'' if number == 1 else 's'}"


def gib(size: int) -> str:
    return f"{size / 2**30:.1f}"


def severity(text: str) -> str:
    """The severity a finding names, or "unlabelled". An unfilled template
    still holds its comment listing every severity, so comments are dropped
    and the field must start with one of them."""
    text = re.sub(r"<!--.*?(-->|$)", "", text, flags=re.S).strip().lower()
    return next((name for name in SEVERITIES
                 if text == name or re.match(rf"{re.escape(name)}\b", text)), "unlabelled")


def ledger(run: Path) -> list[dict]:
    entries = []
    for line in (run / "ledger.jsonl").read_text().splitlines():
        try:
            entries.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    return entries


def findings(entries: list[dict]) -> tuple[list[dict], list[dict]]:
    """The findings among ledger entries: those new to the ledger, and those
    their testers marked as duplicates of an earlier one."""
    found = [entry for entry in entries if entry.get("kind") == "finding"]
    return ([entry for entry in found if not entry.get("duplicate_of")],
            [entry for entry in found if entry.get("duplicate_of")])


def tree_name() -> str | None:
    """The wt tree this checkout is, for a phone tester's lease."""
    trees = Path(os.environ.get("WT_HOME", Path.home() / ".wt")) / "trees"
    try:
        return str(ROOT.relative_to(trees))
    except ValueError:
        return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--testers", type=int, default=4, help="testers in each wave (default 4)")
    parser.add_argument("--phone", type=int, default=None,
                        help="how many of them hold a phone (default 2, or every tester when fewer)")
    parser.add_argument("--persona", action="append", default=[], help="a persona to use (repeatable)")
    parser.add_argument("--waves", type=int, default=2,
                        help="stop after this many waves (default 2; 0: until one finds nothing new)")
    args = parser.parse_args()
    if args.phone is None:
        args.phone = min(2, args.testers)
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
    waves: list[str] = []
    stopping = threading.Event()

    def interrupt(_signum, _frame):
        if stopping.is_set():
            return
        stopping.set()
        say("stopping every tester")
        for tester in testers:
            left = tester.stop()
            if left:
                say(f"{tester.name}: processes {left} would not end; kill them by hand")

    signal.signal(signal.SIGINT, interrupt)
    signal.signal(signal.SIGTERM, interrupt)
    wave = 0
    next_persona = 0
    while not stopping.is_set():
        wave += 1
        before = len(ledger(run))
        baseline = memory_in_use()
        current = []
        for slot in range(args.testers):
            tester = Tester(run, len(testers) + 1, pool[next_persona % len(pool)], slot < args.phone, tree)
            next_persona += 1
            testers.append(tester)
            current.append(tester)
            tester.start(env)
        peak = baseline
        while not all([t.finished() for t in current]):  # every one, to note each end
            peak = max(peak, memory_in_use())
            table = processes()
            for tester in current:
                parts = tester_memory(table, tester)
                total = sum(parts.values())
                if total > tester.peak.get("total", 0):
                    tester.peak = {"total": total, **parts}
            time.sleep(5)
        for tester in current:
            tester.shut_net(env)
            say(f"{tester.outcome()}")
        new, duplicates = findings(ledger(run)[before:])
        line = (f"wave {wave}: {count(len(current), 'tester')}, {count(len(new), 'new finding')}, "
                f"{count(len(duplicates), 'duplicate')}; the machine's memory in use rose by up to "
                f"{gib(max(peak - baseline, 0))} GiB over the {gib(baseline)} GiB in use when it began")
        waves.append(line)
        say(line)
        if stopping.is_set() or not new or (args.waves and wave >= args.waves):
            break

    new, duplicates = findings(ledger(run))
    by_severity: dict[str, int] = {}
    for entry in new:
        key = severity(entry.get("severity") or "")
        by_severity[key] = by_severity.get(key, 0) + 1
    lines = [f"run {run.name}: {count(len(testers), 'tester')} in {count(wave, 'wave')}, "
             f"{count(len(new), 'finding')} and {count(len(duplicates), 'duplicate')}"]
    lines += [f"  {name}: {by_severity[name]}" for name in [*SEVERITIES, "unlabelled"] if name in by_severity]
    lines += [f"  {entry.get('tester')}: {entry.get('finding')} — {entry.get('title', '')}" for entry in new]
    lines += [f"  {entry.get('tester')}: {entry.get('finding')} — duplicates {entry['duplicate_of']}"
              for entry in duplicates]
    lines += waves
    lines.append("tester peaks are physical footprints in GiB: its Claude session, its net and agents, "
                 "and a phone tester's simulator")
    lines += [tester.outcome() for tester in testers]
    lines.append(f"run folder: {run}")
    (run / "summary.txt").write_text("\n".join(lines) + "\n")
    print("\n".join(lines))
    return 130 if stopping.is_set() else 0


if __name__ == "__main__":
    sys.exit(main())
