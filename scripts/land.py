#!/usr/bin/env python3
"""Land this branch on main: `just land "What the branch does"`.

    land.py MESSAGE            through the landing queue, when one is set up
    land.py --local MESSAGE    here, on this machine

The caller's HEAD is landed, with MESSAGE as its merge commit's message;
the tree must have no uncommitted changes. Nothing in the caller's tree is
touched: not its HEAD, its branch or whatever is running in it.

Through the queue (scripts/land_queue.py, on a landing host): HEAD is pushed
to the queue's inbox as refs/queue/<id>, carried by a tag holding the
message, and this waits for the queue's verdict at refs/results/<id>. The
inbox is this machine's `git config amux.landInbox` (an SSH URL, or a path
on the landing host itself); without it the landing happens here. The
queue lands one request at a time, oldest first, wherever it was sent
from.

Here (--local): one landing at a time on this machine, under a lock in
~/.cache/amux/land, in a worktree of its own there whose Buck2 daemon stays
warm from one landing to the next. This is the way to land while the landing
host is down.

Either way the steps are scripts/landing.py's, and it prints `LANDED <sha>`
at the end, or what stopped it. A merge conflict or a failing check is
fixed on the branch, and the branch landed again. Before sending anything it
merges HEAD onto a freshly fetched origin/main in memory (`git merge-tree`,
which touches no tree), so a conflict stops here in seconds rather than at
the queue.

    land.py --log ID           print the whole log of the queue's landing ID
"""

from __future__ import annotations

import argparse
import datetime
import os
import re
import socket
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from landing import Gate, Stopped, conflict_detail, git, land  # noqa: E402

CHECKOUT = Path(__file__).resolve().parents[1]
STATE = Path.home() / ".cache/amux/land"
POLL_SECONDS = 3


def step(what: str) -> None:
    print(f"land: {what}", flush=True)


# --- before sending ---------------------------------------------------------


def conflicts_with_main(commit: str) -> str | None:
    """What stops commit merging onto a fresh origin/main, or None. Only
    origin/main's remote-tracking ref moves. When main can't be fetched or
    merge-tree can't judge, this says nothing and the landing decides."""
    if subprocess.run(["git", "fetch", "--quiet", "origin", "main"], cwd=CHECKOUT).returncode != 0:
        return None
    merged = subprocess.run(
        ["git", "merge-tree", "--write-tree", "--name-only", "--no-messages", "origin/main", commit],
        cwd=CHECKOUT, text=True, capture_output=True,
    )
    if merged.returncode != 1:
        return None
    # The merged tree's id, then each conflicting file.
    files = [line for line in merged.stdout.splitlines()[1:] if line]
    base = git("rev-parse", "origin/main", cwd=CHECKOUT, capture=True)
    return conflict_detail(CHECKOUT, base, commit, files)


# --- here -------------------------------------------------------------------


def worktree() -> Path:
    """The landing worktree, made on first use beside the lock."""
    tree = STATE / "tree"
    if not (tree / ".git").exists():
        step(f"making the landing worktree at {tree}")
        git("worktree", "add", "--detach", str(tree), "origin/main", cwd=CHECKOUT)
    return tree


def try_lock(lock) -> bool:
    """Takes the landing lock without waiting; false if another landing holds it."""
    if os.name == "nt":
        import msvcrt

        try:
            msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
            return True
        except OSError:
            return False
    import fcntl

    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return True
    except BlockingIOError:
        return False


def wait_lock(lock) -> None:
    """Waits for the landing lock."""
    if os.name == "nt":
        while not try_lock(lock):
            time.sleep(2)
        return
    import fcntl

    fcntl.flock(lock, fcntl.LOCK_EX)


def land_here(commit: str, message: str) -> int:
    STATE.mkdir(parents=True, exist_ok=True)
    with open(STATE / "lock", "w") as lock:
        if not try_lock(lock):
            step("another landing is running; waiting for it")
            wait_lock(lock)
        gate = Gate(run=lambda argv, cwd: subprocess.run(list(argv), cwd=cwd).returncode)
        try:
            landed = land(worktree(), commit, message, gate, step)
        except Stopped as stopped:
            print(f"land: stopped: {stopped}", file=sys.stderr)
            return 1
        except subprocess.CalledProcessError as error:
            print(f"land: stopped: `{' '.join(map(str, error.cmd))}` failed", file=sys.stderr)
            return 1
        print(f"LANDED {landed}", flush=True)
        return 0


# --- through the queue ------------------------------------------------------


def request_id(branch: str) -> str:
    """When, from where and what: ids sort in the order requests were made,
    and say whose a request is at a glance."""
    when = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    host = socket.gethostname().split(".")[0]
    return re.sub(r"[^A-Za-z0-9._-]+", "-", f"{when}-{host}-{branch}").strip("-.")


def request_tag(commit: str, rid: str, message: str) -> str:
    """The request as one object: a tag naming the commit and carrying the
    landing's message, so the two arrive together."""
    ident = git("var", "GIT_COMMITTER_IDENT", cwd=CHECKOUT, capture=True)
    body = f"object {commit}\ntype commit\ntag land/{rid}\ntagger {ident}\n\n{message.rstrip()}\n"
    return subprocess.run(
        ["git", "mktag"], cwd=CHECKOUT, input=body, text=True, capture_output=True, check=True
    ).stdout.strip()


def inbox_env() -> dict[str, str]:
    """One SSH connection for the push and every poll after it."""
    ssh = os.environ.get("GIT_SSH_COMMAND", "ssh")
    if os.name == "nt":
        # Windows' ssh has no working connection sharing ("Failed to connect to
        # new control master"), so each push and poll makes its own.
        return dict(os.environ)
    control = Path.home() / ".ssh" / "amux-land-%C"
    return {
        **os.environ,
        "GIT_SSH_COMMAND": f"{ssh} -o ControlMaster=auto -o ControlPath={control} -o ControlPersist=120",
    }


def remote_refs(inbox: str, env: dict[str, str], *patterns: str) -> dict[str, str]:
    out = subprocess.run(
        ["git", "ls-remote", inbox, *patterns], cwd=CHECKOUT, env=env, text=True, capture_output=True, check=True
    ).stdout.split()
    return dict(zip(out[1::2], out[0::2]))


def ahead_of(inbox: str, rid: str, env: dict[str, str]) -> int:
    """How many requests the queue takes before this one. A request is a
    tag, which ls-remote also lists peeled (`<ref>^{}`); that is the same
    request, not another."""
    refs = {ref: sha for ref, sha in remote_refs(inbox, env, "refs/queue/*", "refs/results/*").items() if not ref.endswith("^{}")}
    done = {r.removeprefix("refs/results/") for r in refs if r.startswith("refs/results/")}
    return sum(
        1 for r in refs if r.startswith("refs/queue/") and (q := r.removeprefix("refs/queue/")) < rid and q not in done
    )


def land_through(inbox: str, commit: str, message: str, branch: str) -> int:
    env = inbox_env()
    rid = request_id(branch)
    tag = request_tag(commit, rid, message)
    step(f"sending {commit[:12]} to the landing queue at {inbox} as {rid}")
    subprocess.run(["git", "push", "--quiet", inbox, f"{tag}:refs/queue/{rid}"], cwd=CHECKOUT, env=env, check=True)
    ahead = ahead_of(inbox, rid, env)
    step(f"waiting for the queue ({ahead} ahead of this one)" if ahead else "waiting for the queue")
    result = f"refs/results/{rid}"
    while result not in remote_refs(inbox, env, result):
        time.sleep(POLL_SECONDS)
    subprocess.run(["git", "fetch", "--quiet", inbox, result], cwd=CHECKOUT, env=env, check=True)
    verdict = git("cat-file", "blob", "FETCH_HEAD", cwd=CHECKOUT, capture=True)
    first, _, rest = verdict.partition("\n")
    if first.startswith("LANDED "):
        print(first, flush=True)
        return 0
    print(f"land: stopped: {first.removeprefix('STOPPED ')}", file=sys.stderr)
    if rest.strip():
        print(rest.rstrip(), file=sys.stderr)
    print(f"land: the whole log: just land-log {rid}", file=sys.stderr)
    return 1


def show_log(inbox: str, rid: str) -> int:
    fetched = subprocess.run(["git", "fetch", "--quiet", inbox, f"refs/logs/{rid}"], cwd=CHECKOUT, env=inbox_env())
    if fetched.returncode != 0:
        print(f"land: the queue has no log {rid}", file=sys.stderr)
        return 1
    print(git("cat-file", "blob", "FETCH_HEAD", cwd=CHECKOUT, capture=True))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--local", action="store_true", help="land here rather than through the landing queue")
    parser.add_argument("--log", metavar="ID", help="print the whole log of the queue's landing ID")
    parser.add_argument("message", nargs="?", help="the merge commit's message: what the branch does")
    args = parser.parse_args()
    inbox = git("config", "--get", "amux.landInbox", cwd=CHECKOUT, capture=True, check=False)
    if args.log:
        if not inbox:
            parser.error("no landing queue is set up here (git config amux.landInbox)")
        return show_log(inbox, args.log)
    if not args.message:
        parser.error('say what the branch does: just land "What the branch does"')
    if git("status", "--porcelain", "--untracked-files=no", cwd=CHECKOUT, capture=True):
        print("land: the tree has uncommitted changes; commit them, then land", file=sys.stderr)
        return 1
    commit = git("rev-parse", "HEAD", cwd=CHECKOUT, capture=True)
    stop = conflicts_with_main(commit)
    if stop:
        print(f"land: stopped: the branch doesn't merge onto main: {stop}", file=sys.stderr)
        print("land: merge main into the branch (`just merge-main`), then land again", file=sys.stderr)
        return 1
    if args.local or not inbox:
        return land_here(commit, args.message)
    branch = git("rev-parse", "--abbrev-ref", "HEAD", cwd=CHECKOUT, capture=True)
    return land_through(inbox, commit, args.message, "detached" if branch == "HEAD" else branch)


if __name__ == "__main__":
    sys.exit(main())
