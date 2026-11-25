#!/usr/bin/env python3
"""Say which of CI's gated jobs a change runs (docs/CI.md, "Which jobs a change runs").

    ci_jobs.py decide                   in CI's `changes` job: one `key=true|false`
                                        line per gated job to GITHUB_OUTPUT, the
                                        reasons to GITHUB_STEP_SUMMARY
    ci_jobs.py explain BASE HEAD        what a change from BASE to HEAD runs
    ci_jobs.py --covered JOB [REV]      exit 0 when CI's last pass of JOB on main
                                        judged everything REV (HEAD) changes that
                                        JOB would run for; the release recipes ask
    ci_jobs.py graph-check LINUX... -- ANDROID...
                                        premerge: what the Apple apps' graphs read
                                        that no Linux target compiles is inside
                                        their globs

The gated jobs and what each alone exercises are in build/ci-jobs.toml. A job
judges everything that changed since its newest successful run on main, not
just the push: CI on main runs for the newest commit only, so a commit a
newer push took the place of was never judged by itself. That run's commit
must be an ancestor of HEAD; when there is none, or the commits cannot be
compared, the job runs. A pull request is judged from main's last pass when
it is behind the pull request, else from where the pull request left main.
A run started by hand runs every job. Whatever goes wrong deciding, every job
runs: a skipped job must be one that was decided.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TABLE = ROOT / "build" / "ci-jobs.toml"
WORKFLOW = ".github/workflows/ci.yml"
LOCK = "Cargo.lock"
# The runs on main whose passes count: a push's, and one started by hand.
EVENTS_THAT_PASS = ("push", "workflow_dispatch")
# How far back to look for a job's last pass: past this many of main's runs
# the job runs (a long-failing job judges a long range anyway).
RUNS_TO_SEARCH = 150


# --- the table -----------------------------------------------------------


def glob_regex(pattern: str) -> re.Pattern:
    out, i = "", 0
    while i < len(pattern):
        if pattern.startswith("**/", i):
            out += "(?:.*/)?"
            i += 3
        elif pattern.startswith("**", i):
            out += ".*"
            i += 2
        elif pattern[i] == "*":
            out += "[^/]*"
            i += 1
        elif pattern[i] == "?":
            out += "[^/]"
            i += 1
        else:
            out += re.escape(pattern[i])
            i += 1
    return re.compile(out + r"\Z")


class Globs:
    """A list of globs in which a `!` glob takes back what earlier ones matched."""

    def __init__(self, patterns: list[str]):
        self.patterns = patterns
        self.rules = [(p.startswith("!"), glob_regex(p.removeprefix("!"))) for p in patterns]

    def match(self, path: str) -> bool:
        hit = False
        for negated, regex in self.rules:
            if regex.match(path):
                hit = not negated
        return hit


class Job:
    def __init__(self, key: str, spec: dict):
        self.key = key
        self.output = key.replace("-", "_")
        self.name = spec["name"]
        self.workflow_job = spec["workflow_job"]
        self.paths = Globs(spec.get("paths", []))
        self.contains_in = Globs(spec.get("contains_in", ["**"]))
        self.contains = spec.get("contains", [])


class Table:
    def __init__(self, text: str):
        data = tomllib.loads(text)
        self.everyone = Globs(data["everyone"]["paths"])
        self.covered = Globs(data["covered"]["paths"])
        self.jobs = [Job(key, spec) for key, spec in data["jobs"].items()]

    def job(self, key: str) -> Job:
        for job in self.jobs:
            if key in (job.key, job.output):
                return job
        raise SystemExit(f"ci_jobs: no gated job {key!r} (build/ci-jobs.toml has {', '.join(j.key for j in self.jobs)})")

    def kind(self, path: str) -> str:
        """`everyone`, `job`, `covered` or `unclassified`, by path alone."""
        if self.everyone.match(path):
            return "everyone"
        if any(job.paths.match(path) for job in self.jobs):
            return "job"
        if self.covered.match(path):
            return "covered"
        return "unclassified"


def load_table(text: str | None = None) -> Table:
    return Table(text if text is not None else TABLE.read_text())


# --- git ------------------------------------------------------------------


def git(*args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True, check=check)


def is_commit(rev: str) -> bool:
    return git("cat-file", "-e", f"{rev}^{{commit}}", check=False).returncode == 0


def is_ancestor(old: str, new: str) -> bool:
    return git("merge-base", "--is-ancestor", old, new, check=False).returncode == 0


def changed_files(base: str, head: str) -> list[str] | None:
    out = git("diff", "--name-only", "--no-renames", base, head, check=False)
    if out.returncode != 0:
        return None
    return [line for line in out.stdout.splitlines() if line]


class Contents:
    """Files' text at two commits, read through one `git cat-file --batch`.

    In CI's partial clone a blob is fetched when it is first read; a `git
    diff` over the same files first fetches them all in one go."""

    def __init__(self, base: str, head: str):
        self.base, self.head = base, head
        self.cache: dict[tuple[str, str], str | None] = {}
        self.process = None

    def prefetch(self, paths: list[str]) -> None:
        if paths:
            git("diff", "--numstat", self.base, self.head, "--", *paths, check=False)

    def read(self, rev: str, path: str) -> str | None:
        key = (rev, path)
        if key not in self.cache:
            if self.process is None:
                self.process = subprocess.Popen(["git", "cat-file", "--batch"], cwd=ROOT, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
            self.process.stdin.write(f"{rev}:{path}\n".encode())
            self.process.stdin.flush()
            header = self.process.stdout.readline().decode().split()
            if len(header) < 3 or header[1] != "blob":
                self.cache[key] = None
            else:
                body = self.process.stdout.read(int(header[2]))
                self.process.stdout.read(1)
                self.cache[key] = body.decode("utf-8", errors="replace")
        return self.cache[key]

    def both(self, path: str) -> list[str]:
        return [text for text in (self.read(self.base, path), self.read(self.head, path)) if text is not None]


# --- the two files read rather than matched -----------------------------


def lock_first_party_only(base_text: str | None, head_text: str | None) -> bool:
    """Whether a Cargo.lock change touches only workspace crates' entries
    (they carry no `source`): a crate's version or its dependency on another
    crate, which the crate's own files say too."""
    if base_text is None or head_text is None:
        return False
    try:
        old, new = tomllib.loads(base_text), tomllib.loads(head_text)
    except tomllib.TOMLDecodeError:
        return False

    def packages(lock: dict) -> dict:
        return {(p["name"], p["version"]): p for p in lock.get("package", [])}

    old, new = packages(old), packages(new)
    changed = [key for key in old.keys() | new.keys() if old.get(key) != new.get(key)]
    return all("source" not in (old.get(key) or new[key]) for key in changed)


def workflow_owners(text: str) -> tuple[dict[int, str | None], dict[str, set[str]]]:
    """For each line of the workflow, the job whose block holds it (None
    outside `jobs:`), and for each job, the jobs that reuse a block of its
    through a YAML anchor (`steps: &macos-steps`, `steps: *macos-steps`)."""
    owner: dict[int, str | None] = {}
    anchors: dict[str, tuple[str, int, int]] = {}  # name -> (job, first line, indent)
    aliases: dict[str, set[str]] = {}  # anchor name -> jobs that alias it
    lines = text.splitlines()
    job = None
    in_jobs = False
    for number, line in enumerate(lines, 1):
        stripped = line.strip()
        indent = len(line) - len(line.lstrip(" "))
        if stripped and indent == 0:
            in_jobs = line.startswith("jobs:")
            job = None
        elif in_jobs and indent == 2 and re.match(r"[A-Za-z0-9_-]+:\s*(#.*)?$", stripped):
            job = stripped.split(":")[0]
        owner[number] = job if in_jobs else None
        if job:
            for name in re.findall(r"&([A-Za-z0-9_-]+)", line):
                anchors[name] = (job, number, indent)
            for name in re.findall(r"\*([A-Za-z0-9_-]+)\s*$", line):
                aliases.setdefault(name, set()).add(job)
    shared: dict[int, set[str]] = {}
    for name, (job, first, indent) in anchors.items():
        users = aliases.get(name, set())
        number = first
        while number <= len(lines):
            line = lines[number - 1]
            if number > first and line.strip() and len(line) - len(line.lstrip(" ")) <= indent:
                break
            shared.setdefault(number, set()).update(users)
            number += 1
    return owner, shared


def workflow_jobs(base_text: str | None, head_text: str | None) -> set[str] | None:
    """The workflow jobs (their keys) a change to the workflow touches; None
    when a line outside every job changed, or the change cannot be read."""
    if base_text is None or head_text is None:
        return None
    import difflib

    old, new = base_text.splitlines(), head_text.splitlines()
    old_owner, old_shared = workflow_owners(base_text)
    new_owner, new_shared = workflow_owners(head_text)
    touched: set[str] = set()
    for tag, i1, i2, j1, j2 in difflib.SequenceMatcher(None, old, new, autojunk=False).get_opcodes():
        if tag == "equal":
            continue
        sides = [(old_owner, old_shared, range(i1 + 1, i2 + 1)), (new_owner, new_shared, range(j1 + 1, j2 + 1))]
        # A pure insertion or deletion belongs to the block it lands in.
        if i1 == i2:
            sides[0] = (old_owner, old_shared, range(max(i1, 1), max(i1, 1) + 1))
        if j1 == j2:
            sides[1] = (new_owner, new_shared, range(max(j1, 1), max(j1, 1) + 1))
        for owners, shared, numbers in sides:
            for number in numbers:
                if number not in owners:
                    continue
                if owners[number] is None:
                    return None
                touched.add(owners[number])
                touched |= shared.get(number, set())
    return touched


# --- the decision ----------------------------------------------------------


def decide(table: Table, files: list[str], contents: Contents) -> dict[str, tuple[bool, str]]:
    """For each gated job, whether the change runs it, and why."""
    result: dict[str, tuple[bool, str]] = {}
    everyone: list[str] = []
    unclassified: list[str] = []
    named: dict[str, str] = {}
    contained: dict[str, str] = {}
    workflow_hits: set[str] = set()
    looked_up = [f for f in files if any(j.contains and j.contains_in.match(f) for j in table.jobs)]
    contents.prefetch(looked_up + [f for f in files if f in (LOCK, WORKFLOW)])
    for path in files:
        if path == LOCK and table.everyone.match(path):
            if lock_first_party_only(contents.read(contents.base, path), contents.read(contents.head, path)):
                continue
            everyone.append(path)
            continue
        if path == WORKFLOW:
            touched = workflow_jobs(contents.read(contents.base, path), contents.read(contents.head, path))
            if touched is None:
                everyone.append(f"{path} (outside its jobs)")
            else:
                workflow_hits |= touched
            continue
        if table.everyone.match(path):
            everyone.append(path)
            continue
        hit = False
        for job in table.jobs:
            if job.paths.match(path):
                named.setdefault(job.key, path)
                hit = True
        texts = None
        for job in table.jobs:
            if job.contains and job.key not in named and job.key not in contained and job.contains_in.match(path):
                texts = contents.both(path) if texts is None else texts
                for needle in job.contains:
                    if any(needle in text for text in texts):
                        contained[job.key] = f"{path} holds {needle!r}"
                        break
        if not hit and not table.covered.match(path):
            unclassified.append(path)
    for job in table.jobs:
        if everyone:
            result[job.key] = (True, f"{everyone[0]} reaches every job")
        elif unclassified:
            result[job.key] = (True, f"{unclassified[0]} is in no table of build/ci-jobs.toml, so every job runs")
        elif job.key in named:
            result[job.key] = (True, f"{named[job.key]} is in its territory")
        elif job.key in contained:
            result[job.key] = (True, contained[job.key])
        elif job.workflow_job in workflow_hits:
            result[job.key] = (True, f"its job in {WORKFLOW} changed")
        else:
            result[job.key] = (False, f"none of the {len(files)} changed files is in its territory")
    return result


# --- GitHub ------------------------------------------------------------------


def gh_json(path: str):
    out = subprocess.run(["gh", "api", path], cwd=ROOT, capture_output=True, text=True, timeout=60)
    if out.returncode != 0:
        raise RuntimeError(f"gh api {path}: {out.stderr.strip()}")
    return json.loads(out.stdout)


def last_passes(repository: str, names: set[str]) -> dict[str, str]:
    """Each job's newest successful run on main (by job name): its commit."""
    found: dict[str, str] = {}
    seen = 0
    page = 1
    while seen < RUNS_TO_SEARCH and len(found) < len(names):
        runs = gh_json(f"repos/{repository}/actions/workflows/ci.yml/runs?branch=main&status=completed&per_page=50&page={page}")["workflow_runs"]
        if not runs:
            break
        for run in runs:
            seen += 1
            if run["event"] not in EVENTS_THAT_PASS:
                continue
            jobs = gh_json(f"repos/{repository}/actions/runs/{run['id']}/jobs?filter=all&per_page=100")["jobs"]
            for job in jobs:
                if job["conclusion"] == "success" and job["name"] in names and job["name"] not in found:
                    found[job["name"]] = run["head_sha"]
            if len(found) == len(names):
                break
        page += 1
    return found


def repository() -> str:
    if os.environ.get("GITHUB_REPOSITORY"):
        return os.environ["GITHUB_REPOSITORY"]
    out = subprocess.run(["gh", "repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner"], cwd=ROOT, capture_output=True, text=True, timeout=60)
    if out.returncode != 0:
        raise RuntimeError(f"gh repo view: {out.stderr.strip()}")
    return out.stdout.strip()


# --- the commands ------------------------------------------------------------


def judge(table: Table, job: Job, base: str | None, head: str, why_no_base: str) -> tuple[bool, str, str]:
    """(runs, why, base) for one job judged from `base`."""
    if base is None:
        return True, why_no_base, ""
    if not is_commit(base) or not is_ancestor(base, head):
        return True, f"its last pass, {base[:12]}, is not behind {head[:12]}", base
    files = changed_files(base, head)
    if files is None:
        return True, f"{base[:12]} and {head[:12]} cannot be compared", base
    runs, why = decide(table, files, Contents(base, head))[job.key]
    return runs, why, base


def decide_in_ci() -> int:
    table = load_table()
    event = os.environ.get("GITHUB_EVENT_NAME", "")
    rows: list[tuple[Job, bool, str, str]] = []
    try:
        head = git("rev-parse", "HEAD").stdout.strip()
        if event == "workflow_dispatch":
            rows = [(job, True, "a run started by hand runs every job", "") for job in table.jobs]
        else:
            passes = last_passes(repository(), {job.name for job in table.jobs})
            fork_point = None
            if event == "pull_request":
                out = git("merge-base", "HEAD", "origin/main", check=False)
                fork_point = out.stdout.strip() if out.returncode == 0 else None
            for job in table.jobs:
                base = passes.get(job.name)
                if event == "pull_request" and fork_point and not (base and is_commit(base) and is_ancestor(base, head)):
                    base = fork_point
                runs, why, used = judge(table, job, base, head, f"no successful {job.name!r} in main's last {RUNS_TO_SEARCH} runs")
                rows.append((job, runs, why, used))
    except Exception as error:  # Fail open: a skipped job must be a decided one.
        rows = [(job, True, f"deciding failed ({error}), so every job runs", "") for job in table.jobs]
    write_outputs(rows)
    return 0


def write_outputs(rows: list[tuple[Job, bool, str, str]]) -> None:
    lines = [f"{job.output}={'true' if runs else 'false'}" for job, runs, _, _ in rows]
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with open(output, "a") as out:
            out.write("\n".join(lines) + "\n")
    summary = ["### Which gated jobs this change runs", "", "| Job | Runs | Judged from | Why |", "| --- | --- | --- | --- |"]
    for job, runs, why, base in rows:
        summary.append(f"| {job.name} | {'yes' if runs else 'skipped'} | {base[:12] or '—'} | {why} |")
    summary += ["", "Each job judges everything changed since its last successful run on main (docs/CI.md, \"Which jobs a change runs\")."]
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as out:
            out.write("\n".join(summary) + "\n")
    for job, runs, why, base in rows:
        print(f"{job.key}: {'runs' if runs else 'skipped'} ({why})")
    print("\n".join(lines))


def explain(base: str, head: str) -> int:
    table = load_table()
    files = changed_files(base, head)
    if files is None:
        print(f"{base} and {head} cannot be compared, so every job runs")
        return 0
    for key, (runs, why) in decide(table, files, Contents(base, head)).items():
        print(f"{key}: {'runs' if runs else 'skipped'} ({why})")
    return 0


def covered(key: str, rev: str) -> int:
    table = load_table()
    job = table.job(key)
    head = git("rev-parse", rev).stdout.strip()
    how = (
        f"To have CI judge it, run CI on main with every job (`gh workflow run ci.yml --ref main`, "
        f"which starts every gated job), wait for {job.name!r} to pass, and try again."
    )
    try:
        base = last_passes(repository(), {job.name}).get(job.name)
    except Exception as error:
        print(f"ci_jobs: could not ask GitHub when {job.name!r} last passed ({error}). {how}", file=sys.stderr)
        return 1
    runs, why, _ = judge(table, job, base, head, f"CI has no successful {job.name!r} on main in its last {RUNS_TO_SEARCH} runs")
    if runs:
        since = f"since it last passed, at {base[:12]}, " if base else ""
        print(f"ci_jobs: CI's {job.name!r} has not judged {head[:12]}: {since}{why}. {how}", file=sys.stderr)
        return 1
    print(f"ci_jobs: CI's {job.name!r} passed at {base[:12]}, and nothing since is in its territory")
    return 0


def graph_check(linux: list[str], android: list[str]) -> int:
    """What each Apple app's graph reads, less what a Linux target compiles,
    is inside that app's globs: the Linux gate cannot vouch for it, so a
    change to it has to run the app's job."""
    if sys.platform != "darwin":
        # The Apple apps' graphs configure only where a macOS execution
        # platform exists, and build/platforms gives one to a Mac host alone.
        print("ci_jobs graph-check: skipped: the Apple apps' graphs configure only on a Mac")
        return 0
    table = load_table()
    buck2 = subprocess.run([str(ROOT / "scripts/py"), str(ROOT / "scripts/buck-tools.py"), "path", "buck2"], cwd=ROOT, capture_output=True, text=True, check=True).stdout.strip()
    tracked = set(git("ls-files").stdout.splitlines())
    data = "^(_files|_amux_check|export_file)$"

    def query(expression: str, platform: str | None = None, kind: str = "cquery") -> set[str]:
        args = [buck2, kind, expression] + (["--target-platforms", platform] if platform else [])
        out = subprocess.run(args, cwd=ROOT, capture_output=True, text=True)
        if out.returncode != 0:
            raise SystemExit(f"ci_jobs graph-check: buck2 cquery failed:\n{out.stderr[-2000:]}")
        return {line.strip() for line in out.stdout.splitlines() if line.strip()}

    def compiled(patterns: list[str], platform: str | None) -> set[str]:
        targets = f"set({' '.join(patterns)})"
        return query(f"inputs(deps({targets}) - kind('{data}', deps({targets})))", platform)

    linux_compiled = compiled(linux, "root//build/platforms:linux-cli-testing-dev") | (compiled(android, None) if android else set())
    apps = [
        ("iphone", "ios-sim", "root//build/platforms:ios-sim-app-shipping-dev"),
        ("mac-app", "macos", "root//build/platforms:macos-app-shipping-dev"),
    ]
    problems = []
    for key, platform_name, platform in apps:
        job = table.job(key)
        targets = query(f"attrregexfilter(default_target_platform, {platform_name}, //apps/apple/...)", kind="uquery")
        reads = query(f"inputs(deps(set({' '.join(sorted(targets))})))", platform)
        for path in sorted(reads - linux_compiled):
            if path in tracked and not job.paths.match(path) and not table.everyone.match(path):
                problems.append(f"{path}: the {job.name} graph reads it and no Linux target compiles it, but it is outside [jobs.{key}].paths")
    if problems:
        print("ci_jobs graph-check: build/ci-jobs.toml would skip a job for a change only that job tests:", file=sys.stderr)
        for line in problems:
            print(f"  {line}", file=sys.stderr)
        return 1
    print("ci_jobs graph-check: every file only the Apple apps' graphs read is in their territory")
    return 0


def main(argv: list[str]) -> int:
    if argv[:1] == ["--covered"]:
        argv = ["covered", *argv[1:]]
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("decide")
    explain_parser = sub.add_parser("explain")
    explain_parser.add_argument("base")
    explain_parser.add_argument("head", nargs="?", default="HEAD")
    covered_parser = sub.add_parser("covered")
    covered_parser.add_argument("job")
    covered_parser.add_argument("rev", nargs="?", default="HEAD")
    graph = sub.add_parser("graph-check")
    graph.add_argument("patterns", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    if args.command == "decide":
        return decide_in_ci()
    if args.command == "explain":
        return explain(args.base, args.head)
    if args.command == "covered":
        return covered(args.job, args.rev)
    patterns = args.patterns
    linux, android = (patterns[: patterns.index("--")], patterns[patterns.index("--") + 1 :]) if "--" in patterns else (patterns, [])
    return graph_check(linux, android)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
