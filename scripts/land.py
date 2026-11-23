#!/usr/bin/env python3
"""Land a branch on main: `just land "What the branch does"`.

    land.py MESSAGE              the caller's HEAD, merged with MESSAGE
    land.py --pr N [MESSAGE]     pull request N's head, once it is reviewed

One landing at a time on this machine: a lock under ~/.cache/amux/land
holds a second caller until the first is done. Each landing happens in a
worktree of its own there, so the caller's tree, its HEAD and whatever is
running in it are never touched, and that worktree's Buck2 daemon stays
warm from one landing to the next. In it:

1. fetch origin/main and start from it, detached;
2. merge the commit with --no-ff and MESSAGE;
3. run `just premerge` on the merge;
4. run `just miri` when the merge changes a file the phone's C ABI is
   built from, the one crate Miri checks;
5. push the merge to main. When the push is refused, someone landed
   first, and it starts again from the fetch.

It prints `LANDED <sha>` at the end, or what stopped it. A merge conflict
or a failing check is fixed on the branch, and the branch landed again.
"""

import argparse
import fcntl
import subprocess
import sys
from pathlib import Path

CHECKOUT = Path(__file__).resolve().parents[1]
STATE = Path.home() / ".cache/amux/land"
ATTEMPTS = 5


def git(*args: str, cwd: Path, capture: bool = False) -> str:
    result = subprocess.run(["git", *args], cwd=cwd, check=True, text=True, capture_output=capture)
    return result.stdout.strip() if capture else ""


def step(what: str) -> None:
    print(f"land: {what}", flush=True)


def worktree() -> Path:
    """The landing worktree, made on first use beside the lock."""
    tree = STATE / "tree"
    if not (tree / ".git").exists():
        step(f"making the landing worktree at {tree}")
        git("worktree", "add", "--detach", str(tree), "origin/main", cwd=CHECKOUT)
    return tree


def miri_covers(tree: Path, base: str) -> bool:
    """Whether the merge changes a file //crates/app-ffi is built from."""
    changed = git("diff", "--name-only", base, "HEAD", cwd=tree, capture=True).splitlines()
    if not changed:
        return False
    query = subprocess.run(
        [
            str(tree / "scripts/buck2"),
            "uquery",
            "intersect(deps(//crates/app-ffi:app-ffi), owner(%Ss))",
            *changed,
        ],
        cwd=tree,
        check=True,
        text=True,
        capture_output=True,
    )
    return bool(query.stdout.strip())


def land_once(tree: Path, commit: str, message: str) -> str | None:
    """One try: the merge's sha once pushed, or None when the push was
    refused. Raises CalledProcessError when a step fails."""
    step("fetching origin/main")
    git("fetch", "--quiet", "origin", "main", cwd=tree)
    git("reset", "--quiet", "--hard", cwd=tree)
    git("clean", "--quiet", "-fdx", "-e", ".buckconfig.local", "-e", "target", "-e", "js/node_modules", cwd=tree)
    git("switch", "--quiet", "--detach", "origin/main", cwd=tree)
    base = git("rev-parse", "HEAD", cwd=tree, capture=True)
    step(f"merging {commit[:12]} onto {base[:12]}")
    git("merge", "--quiet", "--no-ff", "-m", message, commit, cwd=tree)
    step("running just premerge")
    subprocess.run(["just", "premerge"], cwd=tree, check=True)
    if miri_covers(tree, base):
        step("running just miri: the merge changes what app-ffi is built from")
        subprocess.run(["just", "miri"], cwd=tree, check=True)
    step("pushing to main")
    push = subprocess.run(["git", "push", "--quiet", "origin", "HEAD:main"], cwd=tree)
    if push.returncode != 0:
        return None
    return git("rev-parse", "HEAD", cwd=tree, capture=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--pr", type=int, help="land this pull request's head")
    parser.add_argument("message", nargs="?", help="the merge commit's message: what the branch does")
    args = parser.parse_args()

    if args.pr is not None:
        ref = f"refs/pull/{args.pr}/head"
        git("fetch", "--quiet", "origin", ref, cwd=CHECKOUT)
        commit = git("rev-parse", "FETCH_HEAD", cwd=CHECKOUT, capture=True)
        message = args.message or f"Merge pull request #{args.pr}"
    else:
        if not args.message:
            parser.error("say what the branch does: just land \"What the branch does\"")
        if git("status", "--porcelain", "--untracked-files=no", cwd=CHECKOUT, capture=True):
            print("land: the tree has uncommitted changes; commit them, then land", file=sys.stderr)
            return 1
        commit = git("rev-parse", "HEAD", cwd=CHECKOUT, capture=True)
        message = args.message

    STATE.mkdir(parents=True, exist_ok=True)
    with open(STATE / "lock", "w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            step("another landing is running; waiting for it")
            fcntl.flock(lock, fcntl.LOCK_EX)
        tree = worktree()
        for _ in range(ATTEMPTS):
            try:
                landed = land_once(tree, commit, message)
            except subprocess.CalledProcessError as error:
                print(f"land: stopped: `{' '.join(map(str, error.cmd))}` failed", file=sys.stderr)
                return 1
            if landed:
                print(f"LANDED {landed}", flush=True)
                return 0
            step("the push was refused: someone landed first; starting again")
        print(f"land: the push was refused {ATTEMPTS} times", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
