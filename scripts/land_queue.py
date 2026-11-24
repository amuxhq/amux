#!/usr/bin/env python3
"""The landing queue: lands what `just land` sends, one request at a time.

    land_queue.py serve --inbox DIR --tree DIR [--self-update]
    land_queue.py serve --once --inbox DIR --tree DIR
    land_queue.py check --inbox DIR --tree DIR

It runs on a landing host, as an account that may push to the repository,
started by a service the host configures with its inbox and tree
(docs/BUILD.md). Requests arrive in the inbox, a bare repository `just
land` pushes to:

    refs/queue/<id>     a tag naming the commit to land; its message is the
                        merge commit's
    refs/results/<id>   written here: a blob whose first line is
                        `LANDED <sha>` or `STOPPED <why>`, then the end of
                        the log when it stopped
    refs/logs/<id>      written here: a blob of the landing's whole log

Ids start with the time the request was made, so the oldest request without
a result is the next one. Each is landed in --tree, a clone of the amux
repository whose `origin` the queue may push to, by scripts/landing.py's
steps. Results older than KEEP_DAYS go, with their request and log.

The queue runs its code from main (the service brings its checkout there
when it starts the queue). With --self-update, when main's copy of the
queue's own code changes, it checks the new code starts (`check`) before
moving to it, so a landing that breaks the queue leaves the old queue
running to land the fix.

Between landings, every TRIM_SECONDS, it trims the tree's buck-out (`trim`).

Standard library only, as scripts/landing.py says.
"""

from __future__ import annotations

import argparse
import datetime
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Callable, Sequence

sys.path.insert(0, str(Path(__file__).resolve().parent))
from landing import Gate, Stopped, git, land  # noqa: E402

CHECKOUT = Path(__file__).resolve().parents[1]
POLL_SECONDS = 2
UPDATE_SECONDS = 300
KEEP_DAYS = 14
TAIL_LINES = 80
TRIM_SECONDS = 6 * 3600
# What `trim` keeps: outputs a build used in the last TRIM_AGE. Anything
# older comes back from the cache when a landing needs it again.
TRIM_AGE = "1d"
# The queue's own code: what --self-update watches on main.
OWN_CODE = ("scripts/land_queue.py", "scripts/landing.py")


def say(what: str) -> None:
    print(f"{datetime.datetime.now().isoformat(timespec='seconds')} {what}", flush=True)


# --- the inbox --------------------------------------------------------------


def refs(inbox: Path, prefix: str) -> dict[str, str]:
    """id -> object, for the refs under refs/<prefix>/."""
    out = git("for-each-ref", "--format=%(objectname) %(refname)", f"refs/{prefix}/", cwd=inbox, capture=True)
    pairs = (line.split(" ", 1) for line in out.splitlines() if line)
    return {ref.removeprefix(f"refs/{prefix}/"): sha for sha, ref in pairs}


def pending(inbox: Path) -> list[str]:
    done = refs(inbox, "results")
    return sorted(rid for rid in refs(inbox, "queue") if rid not in done)


def request(inbox: Path, rid: str) -> tuple[str, str, dict[str, str] | None]:
    """The commit a request names, the landing's message, and who sent it:
    the tag's tagger, who authors the merge commit. The queue's own account
    has no git identity of its own, and a landing is its sender's."""
    ref = f"refs/queue/{rid}"
    commit = git("rev-parse", "--verify", f"{ref}^{{commit}}", cwd=inbox, capture=True)
    # A tag's contents are its message; a bare commit's, which `just land`
    # never sends, would be the commit's own, so it counts as none.
    is_tag = git("cat-file", "-t", ref, cwd=inbox, capture=True) == "tag"
    if not is_tag:
        return commit, "", None
    message = git("for-each-ref", "--format=%(contents)", ref, cwd=inbox, capture=True)
    name, email = git("for-each-ref", "--format=%(taggername)%00%(taggeremail:trim)", ref, cwd=inbox, capture=True).split("\0")
    lander = {
        "GIT_AUTHOR_NAME": name,
        "GIT_AUTHOR_EMAIL": email,
        "GIT_COMMITTER_NAME": name,
        "GIT_COMMITTER_EMAIL": email,
    }
    return commit, message.strip(), lander


def write_blob(inbox: Path, ref: str, text: str) -> None:
    sha = subprocess.run(
        ["git", "hash-object", "-w", "--stdin"], cwd=inbox, input=text, text=True, capture_output=True, check=True
    ).stdout.strip()
    git("update-ref", ref, sha, cwd=inbox)


def requested_at(rid: str) -> datetime.datetime | None:
    try:
        return datetime.datetime.strptime(rid[:15], "%Y%m%dT%H%M%S").replace(tzinfo=datetime.timezone.utc)
    except ValueError:
        return None


def prune(inbox: Path, now: datetime.datetime) -> None:
    """Forgets requests whose result is older than KEEP_DAYS."""
    cutoff = now - datetime.timedelta(days=KEEP_DAYS)
    old = [rid for rid in refs(inbox, "results") if (at := requested_at(rid)) is not None and at < cutoff]
    if not old:
        return
    commands = "".join(f"delete refs/{kind}/{rid}\n" for rid in old for kind in ("queue", "results", "logs"))
    subprocess.run(["git", "update-ref", "--stdin"], cwd=inbox, input=commands, text=True, check=True)


# --- one landing ------------------------------------------------------------


Wrap = Callable[[Sequence[str], Path], Sequence[str]]


def logged_gate(log, wrap: Wrap | None, **checks) -> Gate:
    """The gate, with each command's output going to the landing's log.
    `checks` replaces the gate's commands (the tests' own)."""

    # The queue's account has no login shell (its SHELL is nologin), and
    # tmux, which the terminal journeys drive, starts each pane with $SHELL.
    env = dict(os.environ, SHELL="/bin/sh")

    def run(argv: Sequence[str], cwd: Path) -> int:
        log.write(f"$ {' '.join(argv)}\n")
        log.flush()
        return subprocess.run(list(argv), cwd=cwd, env=env, stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL).returncode

    gate = Gate(run=run, **checks)
    if wrap is not None:
        gate.wrap = wrap
    return gate


def tail(text: str) -> str:
    return "\n".join(text.rstrip().splitlines()[-TAIL_LINES:])


def land_request(inbox: Path, tree: Path, rid: str, gate_for: Callable[..., Gate]) -> str:
    """Lands one request and writes its result and log; returns the
    result's first line. `gate_for(log)` makes the gate."""
    with tempfile.TemporaryFile("w+") as log:

        def step(what: str) -> None:
            log.write(f"land: {what}\n")
            log.flush()
            say(f"{rid}: {what}")

        try:
            commit, message, lander = request(inbox, rid)
            if not message:
                raise Stopped("the request carries no message; land it with `just land \"What the branch does\"`")
            step(f"fetching {commit[:12]} from the inbox")
            git("fetch", "--quiet", str(inbox), f"refs/queue/{rid}", cwd=tree)
            landed = land(tree, commit, message, gate_for(log), step, lander)
            verdict = f"LANDED {landed}\n"
        except Stopped as stopped:
            verdict = f"STOPPED {stopped}\n"
        except subprocess.CalledProcessError as error:
            verdict = f"STOPPED `{' '.join(map(str, error.cmd))}` failed (exited {error.returncode})\n"
        log.write(verdict)
        log.seek(0)
        whole = log.read()
    if verdict.startswith("STOPPED"):
        verdict += "\n" + tail(whole) + "\n"
    write_blob(inbox, f"refs/logs/{rid}", whole)
    write_blob(inbox, f"refs/results/{rid}", verdict)
    first = verdict.splitlines()[0]
    say(f"{rid}: {first}")
    return first


def trim(tree: Path, wrap: Wrap | None) -> None:
    """Removes what no build has used for TRIM_AGE from the tree's buck-out.
    Buck2 keeps every output it materializes there and never removes one, so
    without this a landing tree fills its disk. `clean --stale` goes through
    the tree's Buck2 daemon, which stays warm for the next landing and knows
    what it removed; deleting files under a running daemon instead leaves it
    believing outputs exist that don't. A trim that fails is said and left
    to the next one: landing matters more than the disk it will need later."""
    if not (tree / "buck-out").is_dir():
        return
    argv: Sequence[str] = ["scripts/buck2", "clean", "--stale", TRIM_AGE]
    if wrap is not None:
        argv = wrap(argv, tree)
    say(f"trimming buck-out: outputs unused for {TRIM_AGE} go")
    started = time.monotonic()
    try:
        status = subprocess.run(list(argv), cwd=tree, stdin=subprocess.DEVNULL).returncode
        failure = f"exited {status}" if status else ""
    except OSError as error:
        failure = str(error)
    if failure:
        say(f"trimming buck-out failed ({failure}); the next try is in {TRIM_SECONDS // 3600}h")
    else:
        say(f"trimmed buck-out in {time.monotonic() - started:.0f}s")


# --- running from main ------------------------------------------------------


def main_changed_own_code() -> bool:
    git("fetch", "--quiet", "origin", "main", cwd=CHECKOUT)
    return subprocess.run(
        ["git", "diff", "--quiet", "HEAD", "origin/main", "--", *OWN_CODE], cwd=CHECKOUT
    ).returncode != 0


def new_code_starts(args: argparse.Namespace) -> bool:
    """Runs main's copy of the queue's code with `check`, from a scratch
    directory, against the same inbox and tree."""
    with tempfile.TemporaryDirectory() as scratch:
        for path in OWN_CODE:
            shown = subprocess.run(["git", "show", f"origin/main:{path}"], cwd=CHECKOUT, capture_output=True)
            if shown.returncode == 0:
                target = Path(scratch, path)
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(shown.stdout)
        checked = subprocess.run(
            [sys.executable, str(Path(scratch, "scripts/land_queue.py")), "check", "--inbox", str(args.inbox), "--tree", str(args.tree)],
            capture_output=True,
            text=True,
        )
    if checked.returncode != 0:
        say(f"main's landing queue doesn't start; staying on this one: {(checked.stderr or checked.stdout).strip()}")
    return checked.returncode == 0


def update_self(args: argparse.Namespace, refused: set[str]) -> None:
    """Moves to main's code when it changed and starts; returns otherwise,
    a fetch that fails included: the service restarting the queue over it
    would stop the tree's warm Buck2 daemon with it."""
    try:
        changed = main_changed_own_code()
    except subprocess.CalledProcessError as error:
        say(f"couldn't look at main (`{' '.join(map(str, error.cmd))}` exited {error.returncode}); staying on this code")
        return
    if not changed:
        return
    head = git("rev-parse", "origin/main", cwd=CHECKOUT, capture=True)
    if head in refused:
        return
    if not new_code_starts(args):
        refused.add(head)
        return
    say(f"moving to main's landing queue at {head[:12]}")
    git("reset", "--quiet", "--hard", "origin/main", cwd=CHECKOUT)
    os.execv(sys.executable, [sys.executable, str(CHECKOUT / "scripts/land_queue.py"), *sys.argv[1:]])


# --- commands ---------------------------------------------------------------


def nix_wrap(argv: Sequence[str], tree: Path) -> Sequence[str]:
    """Each check in the merge's own toolchain, as its flake pins it."""
    return ["nix", "develop", str(tree), "--command", *argv]


def check(args: argparse.Namespace) -> int:
    problems = []
    if not shutil.which("git"):
        problems.append("git isn't on PATH")
    for name, path, bare in (("inbox", args.inbox, "true"), ("tree", args.tree, "false")):
        found = subprocess.run(
            ["git", "rev-parse", "--is-bare-repository"], cwd=path, capture_output=True, text=True
        ) if path.is_dir() else None
        if found is None or found.returncode != 0 or found.stdout.strip() != bare:
            problems.append(f"the {name} {path} isn't a {'bare ' if bare == 'true' else ''}git repository")
    if args.nix and not shutil.which("nix"):
        problems.append("nix isn't on PATH")
    for problem in problems:
        print(f"land-queue: {problem}", file=sys.stderr)
    return 1 if problems else 0


def serve(args: argparse.Namespace) -> int:
    if check(args) != 0:
        return 1
    wrap = nix_wrap if args.nix else None

    def gate_for(log) -> Gate:
        return logged_gate(log, wrap)

    refused: set[str] = set()
    # Main is looked at after every landing, since a landing is what changes
    # it, and every UPDATE_SECONDS for anything pushed some other way.
    next_update = 0.0
    next_trim = 0.0
    say(f"landing queue up: inbox {args.inbox}, tree {args.tree}")
    while True:
        for rid in pending(args.inbox):
            land_request(args.inbox, args.tree, rid, gate_for)
            next_update = 0.0
        prune(args.inbox, datetime.datetime.now(datetime.timezone.utc))
        if args.once:
            return 0
        if args.self_update and time.monotonic() >= next_update:
            update_self(args, refused)
            next_update = time.monotonic() + UPDATE_SECONDS
        if time.monotonic() >= next_trim:
            trim(args.tree, wrap)
            next_trim = time.monotonic() + TRIM_SECONDS
        time.sleep(POLL_SECONDS)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("serve", "check"):
        command = commands.add_parser(name)
        command.add_argument("--inbox", type=Path, required=True, help="the bare repository requests arrive in")
        command.add_argument("--tree", type=Path, required=True, help="the clone landings happen in")
        command.add_argument("--nix", action="store_true", help="run each check inside the merge's `nix develop`")
        if name == "serve":
            command.add_argument("--once", action="store_true", help="land what is waiting, then stop")
            command.add_argument("--self-update", action="store_true", help="move to main's copy of this code when it changes")
    args = parser.parse_args()
    return check(args) if args.command == "check" else serve(args)


if __name__ == "__main__":
    sys.exit(main())
