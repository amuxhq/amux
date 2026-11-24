#!/usr/bin/env python3
"""Land this branch on main: `just land "What the branch does"`.

    land.py MESSAGE            through the landing queue, when one is set up
    land.py --local MESSAGE    here, on this machine

The caller's HEAD is landed, with MESSAGE as its merge commit's message;
the tree must have no uncommitted changes. Nothing in the caller's tree is
touched: not its HEAD, its branch or whatever is running in it.

Through the queue (scripts/land_queue.py, on nova): HEAD is pushed to the
queue's inbox as refs/queue/<id>, carried by a tag holding the message, and
this waits for the queue's verdict at refs/results/<id>. The inbox is this
machine's `git config amux.landInbox` (an SSH URL, or a path on nova
itself); without it the landing happens here. The queue lands one request
at a time, oldest first, wherever it was sent from.

Here (--local): one landing at a time on this machine, under a lock in
~/.cache/amux/land, in a worktree of its own there whose Buck2 daemon stays
warm from one landing to the next. This is the way to land while nova is
down.

Either way the steps are scripts/landing.py's, and it prints `LANDED <sha>`
at the end, or what stopped it. A merge conflict or a failing check is
fixed on the branch, and the branch landed again.
"""

from __future__ import annotations

import argparse
import datetime
import fcntl
import os
import re
import socket
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from landing import Gate, Stopped, git, land  # noqa: E402

CHECKOUT = Path(__file__).resolve().parents[1]
STATE = Path.home() / ".cache/amux/land"
POLL_SECONDS = 3


def step(what: str) -> None:
    print(f"land: {what}", flush=True)


# --- here -------------------------------------------------------------------


def worktree() -> Path:
    """The landing worktree, made on first use beside the lock."""
    tree = STATE / "tree"
    if not (tree / ".git").exists():
        step(f"making the landing worktree at {tree}")
        git("worktree", "add", "--detach", str(tree), "origin/main", cwd=CHECKOUT)
    return tree


def land_here(commit: str, message: str) -> int:
    STATE.mkdir(parents=True, exist_ok=True)
    with open(STATE / "lock", "w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            step("another landing is running; waiting for it")
            fcntl.flock(lock, fcntl.LOCK_EX)
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
    control = Path.home() / ".ssh" / "amux-land-%C"
    ssh = os.environ.get("GIT_SSH_COMMAND", "ssh")
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
    print(f"land: the whole log: git fetch {inbox} refs/logs/{rid} && git cat-file blob FETCH_HEAD", file=sys.stderr)
    return 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--local", action="store_true", help="land here rather than through the landing queue")
    parser.add_argument("message", nargs="?", help="the merge commit's message: what the branch does")
    args = parser.parse_args()
    if not args.message:
        parser.error('say what the branch does: just land "What the branch does"')
    if git("status", "--porcelain", "--untracked-files=no", cwd=CHECKOUT, capture=True):
        print("land: the tree has uncommitted changes; commit them, then land", file=sys.stderr)
        return 1
    commit = git("rev-parse", "HEAD", cwd=CHECKOUT, capture=True)
    inbox = git("config", "--get", "amux.landInbox", cwd=CHECKOUT, capture=True, check=False)
    if args.local or not inbox:
        return land_here(commit, args.message)
    branch = git("rev-parse", "--abbrev-ref", "HEAD", cwd=CHECKOUT, capture=True)
    return land_through(inbox, commit, args.message, "detached" if branch == "HEAD" else branch)


if __name__ == "__main__":
    sys.exit(main())
