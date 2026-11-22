#!/usr/bin/env python3
"""Prune dead Cargo build caches in target/ without throwing away active work.

`cargo-sweep` never removes `target/*/incremental/` directories, and `--time 7`
cleans nothing from active branches where Cargo hashes churn across commits and
feature sets. Meanwhile `cargo-sweep --maxsize` counts every unmanaged directory
in `target/` (`incremental/`, `ios/`, `mac/`, `size/`, ...) toward its cap, so
a naïve `--maxsize` wipes all compiled third-party `deps/` whenever unmanaged
directories exceed the budget.

This script handles both:
1. Prunes `target/*/incremental/` (and `target/<triple>/*/incremental/`) by
   recency down to a byte budget (`INCREMENTAL_BUDGET`, or 0 under `--cold` for
   `wt`'s stop hook on suspension), keeping the most recently used crate caches
   warm during active iteration.
2. Measures what `cargo-sweep` does not manage (remaining `incremental/`, `ios/`,
   `mac/`, etc.) and passes `--maxsize <unmanaged + DEPS_BUDGET>` so `deps/` and
   `.fingerprint/` stay bounded without evicting active third-party crates.
3. Sweeps `target/ios/rust-cargo` and `target/android/rust-cargo/{host,android}`
   separately through `CARGO_TARGET_DIR` when present.

With `--check`, performs a fast (<1 ms) check on `target/debug/incremental`,
`target/debug/deps`, and free disk space, printing a single warning on stderr
when `target/` has grown large enough to warrant running `just sweep`.
"""

from __future__ import annotations

import argparse
import math
import os
from pathlib import Path
import shutil
import subprocess
import sys
from typing import Callable

ROOT = Path(__file__).resolve().parents[1]

GIB = 1024**3
MB = 1_000_000

# Mid-session budget for target/*/incremental/: keeps the active working set
# warm (1-4 s incremental rebuilds on node/tui) while pruning dead hashes.
INCREMENTAL_BUDGET = 4 * GIB
# Cap for Cargo-managed artifacts (deps/, .fingerprint/, build/, examples/) in
# the main target/ directory: enough for warm debug + release deps.
DEPS_BUDGET = 12 * GIB
# Separate budgets for apps/apple's and apps/android's nested Cargo target dirs.
IOS_INCREMENTAL_BUDGET = 1 * GIB
IOS_DEPS_BUDGET = 2 * GIB
ANDROID_INCREMENTAL_BUDGET = 1 * GIB
ANDROID_DEPS_BUDGET = 2 * GIB
ANDROID_RUST_SUBDIRS = ("host", "android")
# Top-level target/ subdirectories that own their own Cargo target directories
# and are swept separately rather than counted as managed outer target dirs.
NESTED_TARGET_ROOTS = frozenset({"ios", "android"})

# Fast --check thresholds (single flat scandir + statvfs, never a recursive du).
# A clean 4-command working set is ~110 incremental dirs (~5.6 GiB) and ~5,100
# debug/deps entries (~5 GiB).
WARN_INCREMENTAL_DIRS = 350
WARN_DEPS_ENTRIES = 9_000
WARN_FREE_BYTES = 20 * GIB

# Directory names inside a Cargo profile directory that cargo-sweep manages.
CARGO_MANAGED_DIRS = frozenset({"deps", ".fingerprint", "build", "examples"})


def _entry_bytes(st: os.stat_result) -> int:
    blocks = getattr(st, "st_blocks", None)
    if blocks is not None:
        return blocks * 512
    return st.st_size


def dir_bytes(path: Path) -> int:
    """Disk bytes used by a directory tree, tolerating disappearing entries."""
    total = 0
    stack = [path]
    while stack:
        current = stack.pop()
        try:
            with os.scandir(current) as it:
                for entry in it:
                    try:
                        if entry.is_dir(follow_symlinks=False):
                            stack.append(Path(entry.path))
                        else:
                            total += _entry_bytes(entry.stat(follow_symlinks=False))
                    except OSError:
                        continue
        except OSError:
            continue
    return total


def dir_mtime(path: Path) -> float:
    """Recency of one incremental crate directory (top level + session dirs)."""
    try:
        newest = path.stat(follow_symlinks=False).st_mtime
    except OSError:
        return 0.0
    try:
        with os.scandir(path) as it:
            for entry in it:
                try:
                    mt = entry.stat(follow_symlinks=False).st_mtime
                    if mt > newest:
                        newest = mt
                except OSError:
                    continue
    except OSError:
        pass
    return newest


def incremental_roots(target: Path) -> list[Path]:
    """Find `incremental/` directories under Cargo profile dirs of `target`.

    Checks `target/<profile>/incremental` and `target/<triple>/<profile>/incremental`,
    skipping `target/ios/` and `target/android/` which own separate Cargo target
    directories swept on their own.
    """
    if not target.is_dir():
        return []
    roots: list[Path] = []
    try:
        top = list(os.scandir(target))
    except OSError:
        return []
    for entry in top:
        if not entry.is_dir(follow_symlinks=False):
            continue
        p = Path(entry.path)
        inc = p / "incremental"
        if inc.is_dir() and not inc.is_symlink():
            roots.append(inc)
        if entry.name in NESTED_TARGET_ROOTS:
            continue
        try:
            sub = list(os.scandir(p))
        except OSError:
            continue
        for child in sub:
            if not child.is_dir(follow_symlinks=False):
                continue
            nested = Path(child.path) / "incremental"
            if nested.is_dir() and not nested.is_symlink():
                roots.append(nested)
    return sorted(roots)


def prune_incremental(
    target: Path, budget: int, *, dry_run: bool = False
) -> tuple[int, int, int]:
    """Prune `incremental/<crate>-<hash>` directories down to `budget` bytes.

    Returns `(kept_dirs, kept_bytes, removed_dirs)`. When `budget == 0`,
    removes every incremental crate directory without spending time measuring
    their sizes first.
    """
    crate_dirs: list[Path] = []
    for inc_root in incremental_roots(target):
        try:
            with os.scandir(inc_root) as it:
                for entry in it:
                    if entry.is_dir(follow_symlinks=False):
                        crate_dirs.append(Path(entry.path))
        except OSError:
            continue

    if not crate_dirs:
        return (0, 0, 0)

    if budget <= 0:
        removed = 0
        for d in crate_dirs:
            if not dry_run:
                shutil.rmtree(d, ignore_errors=True)
            removed += 1
        return (0, 0, removed)

    ranked = sorted(crate_dirs, key=dir_mtime, reverse=True)
    kept_dirs = 0
    kept_bytes = 0
    to_remove: list[Path] = []
    budget_reached = False

    for d in ranked:
        if budget_reached:
            to_remove.append(d)
            continue
        size = dir_bytes(d)
        if kept_dirs == 0 or kept_bytes + size <= budget:
            kept_dirs += 1
            kept_bytes += size
        else:
            budget_reached = True
            to_remove.append(d)

    for d in to_remove:
        if not dry_run:
            shutil.rmtree(d, ignore_errors=True)

    return (kept_dirs, kept_bytes, len(to_remove))


def unmanaged_bytes(target: Path) -> int:
    """Bytes inside `target` that `cargo-sweep` does not clean.

    `cargo-sweep` cleans `deps/`, `.fingerprint/`, `build/`, and `examples/`
    under `target/<profile>/` and `target/<triple>/<profile>/`, while counting
    the entire `target/` tree toward `--maxsize`. Everything outside those
    managed directories is unmanaged.
    """
    if not target.is_dir():
        return 0
    total = 0
    # Stack holds (path, depth_from_target, in_nested_root). Managed dirs live
    # at depth 2 (`target/<profile>/deps`) or depth 3
    # (`target/<triple>/<profile>/deps`), except under `target/ios/` and
    # `target/android/` which are unmanaged from the outer target's perspective.
    stack: list[tuple[Path, int, bool]] = [(target, 0, False)]
    while stack:
        current, depth, in_nested = stack.pop()
        try:
            with os.scandir(current) as it:
                for entry in it:
                    try:
                        if entry.is_dir(follow_symlinks=False):
                            child_depth = depth + 1
                            child_in_nested = in_nested or (
                                depth == 0 and entry.name in NESTED_TARGET_ROOTS
                            )
                            if (
                                not child_in_nested
                                and child_depth in (2, 3)
                                and entry.name in CARGO_MANAGED_DIRS
                            ):
                                continue
                            stack.append((Path(entry.path), child_depth, child_in_nested))
                        else:
                            total += _entry_bytes(entry.stat(follow_symlinks=False))
                    except OSError:
                        continue
        except OSError:
            continue
    return total


def sweep_cargo(
    root: Path,
    target: Path,
    deps_budget: int,
    *,
    dry_run: bool = False,
    extra_env: dict[str, str] | None = None,
    which: Callable[[str], str | None] = shutil.which,
    runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
) -> int:
    """Run `cargo sweep --maxsize` sized so unmanaged dirs never evict active deps."""
    if not target.is_dir():
        return 0
    if which("cargo-sweep") is None:
        print("cargo-sweep is not installed; skipped deps sweep")
        return 0

    other = unmanaged_bytes(target)
    maxsize_mb = max(1, math.ceil((other + deps_budget) / MB))
    cmd = ["cargo", "sweep", "--maxsize", f"{maxsize_mb}MB"]
    if dry_run:
        cmd.append("--dry-run")

    env = os.environ.copy()
    if extra_env:
        env.update(extra_env)

    result = runner(cmd, cwd=str(root), env=env, check=False)
    return result.returncode


def _format_gib(num_bytes: int) -> str:
    return f"{num_bytes / GIB:.2f} GiB"


def sweep(
    root: Path,
    *,
    cold: bool = False,
    dry_run: bool = False,
    incremental_budget: int = INCREMENTAL_BUDGET,
    deps_budget: int = DEPS_BUDGET,
    ios_incremental_budget: int = IOS_INCREMENTAL_BUDGET,
    ios_deps_budget: int = IOS_DEPS_BUDGET,
    android_incremental_budget: int = ANDROID_INCREMENTAL_BUDGET,
    android_deps_budget: int = ANDROID_DEPS_BUDGET,
    which: Callable[[str], str | None] = shutil.which,
    runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
) -> int:
    target = root / "target"
    if not target.is_dir():
        return 0

    inc_budget = 0 if cold else incremental_budget
    kept, kept_bytes, removed = prune_incremental(target, inc_budget, dry_run=dry_run)
    verb = "Would prune" if dry_run else "Pruned"
    if removed or cold:
        print(
            f"incremental: {verb.lower()} {removed} dirs "
            f"(kept {kept}, {_format_gib(kept_bytes)}) from {target}"
        )

    code = sweep_cargo(
        root,
        target,
        deps_budget,
        dry_run=dry_run,
        which=which,
        runner=runner,
    )
    if code != 0:
        return code

    ios_target = target / "ios" / "rust-cargo"
    if ios_target.is_dir():
        ios_inc_budget = 0 if cold else ios_incremental_budget
        ios_kept, ios_kept_bytes, ios_removed = prune_incremental(
            ios_target, ios_inc_budget, dry_run=dry_run
        )
        if ios_removed:
            print(
                f"ios incremental: {verb.lower()} {ios_removed} dirs "
                f"(kept {ios_kept}, {_format_gib(ios_kept_bytes)}) from {ios_target}"
            )
        code = sweep_cargo(
            root,
            ios_target,
            ios_deps_budget,
            dry_run=dry_run,
            extra_env={"CARGO_TARGET_DIR": str(ios_target)},
            which=which,
            runner=runner,
        )
        if code != 0:
            return code

    android_root = target / "android" / "rust-cargo"
    if android_root.is_dir():
        android_inc_budget = 0 if cold else android_incremental_budget
        for sub_name in ANDROID_RUST_SUBDIRS:
            android_target = android_root / sub_name
            if not android_target.is_dir():
                continue
            android_kept, android_kept_bytes, android_removed = prune_incremental(
                android_target, android_inc_budget, dry_run=dry_run
            )
            if android_removed:
                print(
                    f"android/{sub_name} incremental: {verb.lower()} {android_removed} dirs "
                    f"(kept {android_kept}, {_format_gib(android_kept_bytes)}) from {android_target}"
                )
            code = sweep_cargo(
                root,
                android_target,
                android_deps_budget,
                dry_run=dry_run,
                extra_env={"CARGO_TARGET_DIR": str(android_target)},
                which=which,
                runner=runner,
            )
            if code != 0:
                return code

    return code


def _count_entries(path: Path, *, dirs_only: bool = False) -> int:
    if not path.is_dir():
        return 0
    count = 0
    try:
        with os.scandir(path) as it:
            for entry in it:
                if not dirs_only or entry.is_dir(follow_symlinks=False):
                    count += 1
    except OSError:
        return 0
    return count


def check_target(
    target: Path,
    *,
    warn_incremental_dirs: int = WARN_INCREMENTAL_DIRS,
    warn_deps_entries: int = WARN_DEPS_ENTRIES,
    warn_free_bytes: int = WARN_FREE_BYTES,
    ignore_ci: bool = True,
    disk_usage: Callable[[Path], object] = shutil.disk_usage,
) -> list[str]:
    """Return human-readable reasons to warn when `target/` looks overgrown."""
    if ignore_ci and os.environ.get("CI"):
        return []
    if not target.is_dir():
        return []

    reasons: list[str] = []
    inc_dirs = _count_entries(target / "debug" / "incremental", dirs_only=True)
    if inc_dirs > warn_incremental_dirs:
        reasons.append(f"target/debug/incremental has {inc_dirs} dirs")

    deps_entries = _count_entries(target / "debug" / "deps")
    if deps_entries > warn_deps_entries:
        reasons.append(f"target/debug/deps has {deps_entries} entries")

    try:
        free = disk_usage(target).free
    except OSError:
        free = warn_free_bytes
    if free < warn_free_bytes:
        reasons.append(f"only {free / GIB:.1f} GiB free on disk")

    return reasons


def emit_warning(target: Path, stream=None, **kwargs) -> bool:
    """Print a single stderr nudge if `target` trips any threshold."""
    reasons = check_target(target, **kwargs)
    if not reasons:
        return False
    out = sys.stderr if stream is None else stream
    joined = "; ".join(reasons)
    print(
        f"warning: {joined} — run `just sweep` to prune dead build caches "
        "(keeps your active cache warm)",
        file=out,
    )
    return True


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Prune dead Cargo incremental and dependency caches in target/."
    )
    parser.add_argument(
        "--cold",
        action="store_true",
        help="Drop all incremental/ caches (used on worktree suspend/stop).",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Report what would be pruned without deleting files.",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Fast (<1 ms) check that warns on stderr if target/ has grown large.",
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=ROOT,
        help="Repository root containing target/ (defaults to this checkout).",
    )
    args = parser.parse_args(argv)

    if args.check:
        emit_warning(args.root / "target")
        return 0

    return sweep(args.root, cold=args.cold, dry_run=args.dry_run)


if __name__ == "__main__":
    raise SystemExit(main())
