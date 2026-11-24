"""Landing a commit on main: what `just land` on a Mac (scripts/land.py
--local) and the landing queue on nova (scripts/land_queue.py) both do.

In a tree of its own, never the caller's:

1. fetch origin/main and start from it, detached;
2. merge the commit with --no-ff and the landing's message, so `main`'s
   first parents are the states that passed the gate and each landing is
   one merge commit saying what the branch does;
3. run the gate (`just premerge`) on the merge;
4. run `just miri` when the merge changes a file the phone's C ABI or its
   tests are built from (app-ffi is the one crate Miri checks), or Cargo's
   manifest, lockfile or toolchain;
5. run `just sanitize address` and `just sanitize thread` when it changes
   a crate whose tests they run, or what Cargo resolves the build from.
   They run on Linux only, as the other nova-only checks do: a landing on a
   Mac skips them and says so, and CI's Linux job runs them after the push;
6. push the merge to main. A refused push means someone landed first, and
   it starts again from the fetch.

Miri and the sanitizers are Cargo runs, not Buck2 tests: both rebuild the
standard library (`-Zbuild-std`), which Buck2's Rust rules can't. Their
target directory is kept from one landing to the next (KEEP).

`landing.py covers --base REV` prints which of them a change since REV
reaches, as `miri=true|false` and `sanitizers=true|false` lines, which is
how CI decides the same for a push.

Standard library only, and no newer Python than Debian's: the landing queue
runs this from main with no build step, so a main that doesn't build can't
stop it landing the fix.
"""

from __future__ import annotations

import argparse
import importlib.util
import os
import platform
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Sequence

ATTEMPTS = 5
HERE = Path(__file__).resolve().parent

# What every Cargo build of app-ffi reads besides its crates' own files.
CARGO_INPUTS = {"Cargo.toml", "Cargo.lock", "rust-toolchain.toml"}

# Build state kept from one landing to the next: buck-out is the Buck2
# daemon's (deleting it under the daemon leaves it believing outputs exist
# that don't), target/ is miri's Cargo build.
KEEP = (".buckconfig.local", "buck-out", "target", "js/node_modules")


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
    miri: Sequence[str] | None = ("just", "miri")
    # Run once per kind, the kind appended.
    sanitize: Sequence[str] | None = ("just", "sanitize")
    # Wraps each command, as nova runs them inside the merge's own Nix
    # toolchain (`nix develop <tree> --command ...`).
    wrap: Callable[[Sequence[str], Path], Sequence[str]] = field(default=lambda argv, tree: argv)

    def check(self, what: str, argv: Sequence[str], tree: Path) -> None:
        status = self.run(self.wrap(argv, tree), tree)
        if status != 0:
            raise Stopped(f"{what} failed (`{' '.join(argv)}` exited {status})")


def miri_covers(tree: Path, base: str, wrap: Callable[[Sequence[str], Path], Sequence[str]] = lambda argv, tree: argv) -> bool:
    """Whether the merge changes what `just miri` builds: a file of a crate
    app-ffi or its tests link, as Buck2's configured graph has it, or what
    Cargo resolves the build from. The query runs wrapped as the gate's
    commands are, since Buck2 needs the same toolchain."""
    changed = git("diff", "--name-only", base, "HEAD", cwd=tree, capture=True).splitlines()
    if not changed:
        return False
    if CARGO_INPUTS & set(changed):
        return True
    graph = subprocess.run(
        list(wrap([str(tree / "scripts/buck2"), "cquery", "deps(set(//crates/app-ffi:app-ffi //crates/app-ffi:unit-binary), 100000, target_deps())"], tree)),
        cwd=tree,
        check=True,
        text=True,
        capture_output=True,
    ).stdout
    crates = {
        line.split("//", 1)[1].split(":", 1)[0] + "/"
        for line in graph.splitlines()
        if line.startswith("root//crates/")
    }
    return any(path.startswith(crate) for path in changed for crate in crates)


# What the sanitizers' runs read besides their crates' own files.
SANITIZER_SCRIPTS = {"scripts/sanitize.py", "scripts/sanitize-thread.supp"}


def sanitized_crates(tree: Path) -> list[str]:
    """The crates whose tests the sanitizers run, as the merge's own
    scripts/sanitize.py lists them."""
    spec = importlib.util.spec_from_file_location("landing_sanitize", tree / "scripts/sanitize.py")
    assert spec is not None and spec.loader is not None
    sanitize = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(sanitize)
    return sanitize.WHOLE + sanitize.LIBRARY


def sanitizers_cover(tree: Path, base: str) -> bool:
    """Whether the merge changes a crate whose tests the sanitizers run,
    what they are run by, or what Cargo resolves the build from. Not the
    crates those are built from, which is nearly every crate: the
    sanitizers are there for the unsafe code in the crates themselves, and
    the nightly runs them over everything (scripts/nightly.py)."""
    changed = set(git("diff", "--name-only", base, "HEAD", cwd=tree, capture=True).splitlines())
    if (CARGO_INPUTS | SANITIZER_SCRIPTS) & changed:
        return True
    crates = tuple(f"crates/{crate}/" for crate in sanitized_crates(tree))
    return any(path.startswith(crates) for path in changed)


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
    if gate.miri is not None and miri_covers(tree, base, gate.wrap):
        step("running just miri: the merge changes what app-ffi is built from")
        gate.check("miri", gate.miri, tree)
    if gate.sanitize is not None and sanitizers_cover(tree, base):
        if platform.system() != "Linux":
            step("not running the sanitizers, which run on Linux only; CI's Linux job runs them after the push")
        else:
            for kind in ("address", "thread"):
                step(f"running just sanitize {kind}: the merge changes a crate whose tests it runs")
                gate.check(f"the {kind} sanitizer", [*gate.sanitize, kind], tree)
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


def main() -> int:
    parser = argparse.ArgumentParser(description="Say which of a landing's Cargo checks a change reaches.")
    parser.add_argument("command", choices=["covers"])
    parser.add_argument("--base", required=True, help="the commit the change is measured from")
    args = parser.parse_args()
    tree = HERE.parent
    print(f"miri={'true' if miri_covers(tree, args.base) else 'false'}")
    print(f"sanitizers={'true' if sanitizers_cover(tree, args.base) else 'false'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
