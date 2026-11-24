"""Landing a commit on main: what `just land` on a Mac (scripts/land.py
--local) and the landing queue on nova (scripts/land_queue.py) both do.

In a tree of its own, never the caller's:

1. fetch origin/main and start from it, detached;
2. merge the commit with --no-ff and the landing's message, so `main`'s
   first parents are the states that passed the gate and each landing is
   one merge commit saying what the branch does;
3. run the gate (`just premerge`) on the merge;
4. push the merge to main. A refused push means someone landed first, and
   it starts again from the fetch.

The gate is the whole check. The protocol models, Miri and the sanitizers
look for rarer faults than a landing should wait on, so they run every
night instead (.github/workflows/nightly.yml).

Standard library only, and no newer Python than Debian's: the landing queue
runs this from main with no build step, so a main that doesn't build can't
stop it landing the fix.
"""

from __future__ import annotations

import os
import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Sequence

ATTEMPTS = 5

# Build state kept from one landing to the next: buck-out is the Buck2
# daemon's (deleting it under the daemon leaves it believing outputs exist
# that don't). Anything else ignored, a Cargo target/ included, goes.
KEEP = (".buckconfig.local", "buck-out", "js/node_modules")


class Stopped(Exception):
    """A landing that can't go on: a conflict or a failing check. The
    branch is fixed and landed again."""


def git(*args: str, cwd: Path, capture: bool = False, check: bool = True) -> str:
    result = subprocess.run(["git", *args], cwd=cwd, check=check, text=True, capture_output=capture)
    return result.stdout.strip() if capture else ""


@dataclass
class Gate:
    """How a landing is checked. `run(argv, cwd)` runs one command and
    returns its exit status; the queue sends the output to the landing's
    log, the Mac to the terminal."""

    run: Callable[[Sequence[str], Path], int]
    premerge: Sequence[str] = ("just", "premerge")
    # Wraps each command, as nova runs them inside the merge's own Nix
    # toolchain (`nix develop <tree> --command ...`).
    wrap: Callable[[Sequence[str], Path], Sequence[str]] = field(default=lambda argv, tree: argv)

    def check(self, what: str, argv: Sequence[str], tree: Path) -> None:
        status = self.run(self.wrap(argv, tree), tree)
        if status != 0:
            raise Stopped(f"{what} failed (`{' '.join(argv)}` exited {status})")


def land_once(
    tree: Path, commit: str, message: str, gate: Gate, step: Callable[[str], None], lander: dict[str, str] | None = None
) -> str | None:
    """One try: the merge's sha once pushed, or None when the push was
    refused. Raises Stopped for a conflict or a failing check. `lander`
    is the merge commit's author and committer, as git's GIT_AUTHOR_* and
    GIT_COMMITTER_* variables; without it, git's own configuration."""
    step("fetching origin/main")
    git("fetch", "--quiet", "origin", "main", cwd=tree)
    git("reset", "--quiet", "--hard", cwd=tree)
    git("clean", "--quiet", "-fdx", *[arg for keep in KEEP for arg in ("-e", keep)], cwd=tree)
    git("switch", "--quiet", "--detach", "origin/main", cwd=tree)
    base = git("rev-parse", "HEAD", cwd=tree, capture=True)
    if subprocess.run(["git", "merge-base", "--is-ancestor", commit, base], cwd=tree).returncode == 0:
        raise Stopped(f"{commit[:12]} is already on main")
    step(f"merging {commit[:12]} onto {base[:12]}")
    merged = subprocess.run(
        ["git", "merge", "--quiet", "--no-ff", "-m", message, commit],
        cwd=tree, text=True, capture_output=True, env=dict(os.environ, **(lander or {})),
    )
    if merged.returncode != 0:
        conflicts = git("diff", "--name-only", "--diff-filter=U", cwd=tree, capture=True, check=False)
        git("merge", "--abort", cwd=tree, check=False)
        detail = ", ".join(conflicts.splitlines()) or (merged.stderr.strip() or merged.stdout.strip())
        raise Stopped(f"the branch doesn't merge onto main: {detail}")
    step("running the gate (just premerge)")
    gate.check("the gate", gate.premerge, tree)
    step("pushing to main")
    pushed = subprocess.run(["git", "push", "--quiet", "origin", "HEAD:main"], cwd=tree, text=True, capture_output=True)
    if pushed.returncode != 0:
        # Someone landing first moves main, and the landing starts again
        # from the fetch. A refusal that leaves main where it was (a
        # protected branch, a key without write access) would only be
        # refused again, so it stops the landing and says why.
        git("fetch", "--quiet", "origin", "main", cwd=tree)
        if git("rev-parse", "origin/main", cwd=tree, capture=True) == base:
            raise Stopped(f"the push to main was refused: {pushed.stderr.strip() or pushed.stdout.strip()}")
        return None
    return git("rev-parse", "HEAD", cwd=tree, capture=True)


def land(
    tree: Path, commit: str, message: str, gate: Gate, step: Callable[[str], None], lander: dict[str, str] | None = None
) -> str:
    """Lands commit, trying again from the fetch when someone landed first.
    Returns the merge's sha; raises Stopped when it can't."""
    for _ in range(ATTEMPTS):
        landed = land_once(tree, commit, message, gate, step, lander)
        if landed:
            return landed
        step("the push was refused: someone landed first; starting again")
    raise Stopped(f"the push was refused {ATTEMPTS} times")
