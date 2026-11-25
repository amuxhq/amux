#!/usr/bin/env python3
"""Say where a Buck2 command's time went: what it built and tested afresh,
how busy the worker was, and the critical path.

    timing.py [EVENT_LOG]

With no argument it reads the newest `buck2 test` log in buck-out. `just
premerge` prints this at its end, so a landing's log says the same about
the build host as a run on the Mac says about the Mac, without access to
the host's buck-out. Only actions and tests that ran count; what came from
the cache costs nothing here. Each kind is counted apart by where it ran:
`remote` on a NativeLink worker (the Mac's own, or the build host's),
`local` in Buck2 itself, as every build action does on CI's macOS runners
(AMUX_BUILD_HERE=1). A "link" is rustc building a binary, which compiles the
binary's own crate (all of a library's code again, for its unit tests)
before the linker runs; "metadata" is rustc writing a crate's .rmeta, which
the crates after it compile against; "clippy" is a crate's lints.
"""

import json
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BUCK2 = str(ROOT / "scripts/buck2")
LOGS = ROOT / "buck-out/v2/log"


def newest_test_log() -> Path | None:
    logs = sorted(LOGS.glob("*_test_*_events.pb.zst"), key=lambda p: p.stat().st_mtime)
    return logs[-1] if logs else None


def label(owner: dict) -> str:
    target = owner.get("TargetLabel") or owner.get("target_label") or {}
    name = target.get("label", {})
    return f"{name.get('package', '?').removeprefix('root//crates/').removeprefix('root//')}:{name.get('name', '?')}"


KINDS = ("compile", "metadata", "link", "clippy", "test", "other")
WHERE = ("remote", "local")


def kind_of(category: str, identifier: str) -> str:
    # The prelude names a clippy action by its category alone, and a rustc
    # one by what it emits: `metadata [pic]` for the full .rmeta, `check` or
    # `diag` for the fast one (prelude/rust/build.bzl, _explain).
    if category == "clippy":
        return "clippy"
    if category == "rustc" and identifier.split(" ")[0] in ("metadata", "check", "diag"):
        return "metadata"
    if category == "rustc" and "link" in identifier:
        return "link"
    if category == "rustc" and ("rlib" in identifier or "cdylib" in identifier):
        return "compile"
    return "other"


def ran(details: dict) -> dict | None:
    """The timings of a command that ran, not from the cache, and where."""
    command = details.get("command_kind", {}).get("command", {})
    remote = command.get("RemoteCommand")
    if remote is not None:
        if remote.get("cache_hit"):
            return None
        where = "remote"
    # Buck2 logs a command it ran itself as `OmittedLocalCommand` unless
    # asked to keep its whole command line, and one it ran in a persistent
    # worker as `WorkerCommand`.
    elif any(kind in command for kind in ("LocalCommand", "OmittedLocalCommand", "WorkerCommand")):
        where = "local"
    else:
        return None
    meta = details.get("metadata") or {}
    start = meta.get("start_time")
    if not start:
        return None
    return {
        "where": where,
        "start": start[0] + start[1] / 1e9,
        # Null, not absent, for a local command, which waits in no queue.
        "queue": (meta.get("queue_duration_us") or 0) / 1e6,
        "inputs": (meta.get("input_materialization_duration_us") or 0) / 1e6,
        "exec": (meta.get("execution_time_us") or 0) / 1e6,
    }


def collect(log: Path) -> list[tuple[str, str, dict]]:
    shown = subprocess.Popen([BUCK2, "log", "show", str(log)], cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    runs = []
    for line in shown.stdout:
        if '"SpanEnd"' not in line or ('"ActionExecution"' not in line and '"TestRun"' not in line):
            continue
        data = json.loads(line)["Event"]["data"]["SpanEnd"]["data"]
        if "ActionExecution" in data:
            action = data["ActionExecution"]
            for command in action.get("commands", []):
                timing = ran(command.get("details", {}))
                if timing:
                    name = action.get("name", {})
                    runs.append((kind_of(name.get("category", ""), name.get("identifier", "")), label(action.get("key", {}).get("owner", {})), timing))
        elif "TestRun" in data:
            test = data["TestRun"]
            timing = ran((test.get("command_report") or {}).get("details", {}))
            if timing:
                runs.append(("test", label({"TargetLabel": test.get("suite", {}).get("target_label", {})}), timing))
    shown.wait()
    return runs


def concurrency(runs: list[tuple[str, str, dict]]) -> tuple[int, float, float]:
    edges = []
    for _, _, t in runs:
        begin = t["start"] + t["queue"] + t["inputs"]
        edges += [(begin, 1), (begin + t["exec"], -1)]
    edges.sort()
    if not edges:
        return 0, 0.0, 0.0
    level = most = 0
    busy = 0.0
    last = edges[0][0]
    for at, step in edges:
        busy += level * (at - last)
        last = at
        level += step
        most = max(most, level)
    span = edges[-1][0] - edges[0][0]
    return most, busy / span if span else 0.0, span


def critical_path(log: Path) -> list[str]:
    shown = subprocess.run([BUCK2, "log", "critical-path", str(log)], cwd=ROOT, capture_output=True, text=True)
    steps = []
    for line in shown.stdout.splitlines():
        # start, total, [waiting,] user, potential, kind, label, (configuration), category, identifier..., execution kind
        fields = line.split()
        at = next((i for i, f in enumerate(fields) if f in ("action", "test-execution")), None)
        if at is None or len(fields) < at + 2 or not fields[1].isdigit():
            continue
        seconds = int(fields[1]) / 1e6
        if seconds < 2:
            continue
        what = re.sub(r"^root//(crates/)?", "", fields[at + 1])
        identifier = " ".join(f for f in fields[at + 4 :] if not f.startswith("ACTION_EXECUTION_KIND")) if fields[at] == "action" else "test"
        steps.append(f"{what} {identifier} {seconds:.0f} s")
    return steps


def main() -> int:
    log = Path(sys.argv[1]) if len(sys.argv) > 1 else newest_test_log()
    if log is None or not log.exists():
        return 0
    runs = collect(log)
    if not runs:
        print("timing: everything came from the cache")
        return 0
    by_kind: dict[tuple[str, str], list[tuple[float, str]]] = defaultdict(list)
    for kind, name, t in runs:
        by_kind[(kind, t["where"])].append((t["exec"], name))
    print("timing: what ran afresh, by time running (remote: on a NativeLink worker; local: in Buck2 itself)")
    for kind in KINDS:
        for where in WHERE:
            rows = sorted(by_kind.get((kind, where), []), reverse=True)
            if rows:
                longest = ", ".join(f"{name} {seconds:.0f} s" for seconds, name in rows[:3])
                print(f"  {kind:8} {where:6} {len(rows):4} ran, {sum(s for s, _ in rows):6.0f} s in all; longest {longest}")
    for where in WHERE:
        here = [run for run in runs if run[2]["where"] == where]
        if not here:
            continue
        most, mean, span = concurrency(here)
        queued = sum(t["queue"] for _, _, t in here)
        print(f"  {where:15} {most} at once at most, {mean:.1f} on average over {span:.0f} s" + (f"; {queued:.0f} s queued in all" if where == "remote" else ""))
    steps = critical_path(log)
    if steps:
        print("  critical path: " + "; ".join(steps))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:  # A report, never a reason to fail the run.
        print(f"timing: no summary ({error})")
        sys.exit(0)
