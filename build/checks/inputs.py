#!/usr/bin/env python3
"""Write what the repository checks read from outside the checkout's files.

A Buck2 action sees only the files its target declares, and no git history.
Five checks need more than that, so this writes it into build/checks/inputs/
(not committed), where the check targets in the root BUCK file declare it:

- `tracked`: every file the checkout tracks, or would (untracked but not
  ignored), one path a line. The docs check resolves links against it, so
  it reruns when a file is added or removed, not whenever one changes.
- `store-baseline/`: the newest release tag behind HEAD (`ref`) and that
  release's crates/store/src/schema.rs, which the store check compares.
- `proto-baseline/`: the same tag and that release's proto/ tree, which the
  protocol check compares.
- `day`: today's date (UTC), which the npm audit (js/BUCK) and the fetch of
  Cargo's registry (//:cargo-registry) declare: npm's advisories and
  crates.io's yanked releases change without any file changing, so each is
  kept for a day and asked again on the next.
- `deny/`: the RustSec advisory database cargo-deny reads (the root BUCK's
  `deny`), as `advisory-db.tar` and `advisory-db.ref`, a snapshot of its
  files and its commit and date, fetched at most once a day into a clone
  this machine's trees share. When a fetch fails, the last snapshot stays.
  Cargo's graph and crates.io's index, which it reads too, come from a
  Buck2 action on the Linux worker (//:cargo-registry, build/cargo.bzl), so
  the machine running the checks needs no Cargo.

A file is written only when its content changes. The `just` recipes that run
the check targets run this first (`just checks`, `just test`, `just
premerge`); `scripts/buck2 test` alone uses whatever was last written.

    inputs.py [BUCK2_ARGS...]

Given the arguments of the `buck2 test` a recipe is about to run, it writes
nothing when every target pattern there is under crates/ or apps/: only the
root package's and js/'s checks read these files, and on a machine with no
Cargo cache, as on a CI runner, Cargo's graph alone fetches every crate the
workspace depends on, which took the macOS jobs 30 to 50 s a run. A pattern
it can't place, and no pattern at all, writes everything.
"""

import io
import os
import shutil
import subprocess
import sys
import tarfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "build" / "checks" / "inputs"
SCHEMA = "crates/store/src/schema.rs"
DENY = OUT / "deny"
ADVISORIES = "https://github.com/rustsec/advisory-db"
# The clone of the advisory database every tree on this machine snapshots.
ADVISORY_CLONE = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache") / "amux" / "advisory-db"
ADVISORY_SECONDS = 24 * 60 * 60


def git(*args: str, binary: bool = False):
    result = subprocess.run(["git", *args], cwd=ROOT, capture_output=True, check=False)
    if result.returncode != 0:
        return None
    return result.stdout if binary else result.stdout.decode()


def write(path: Path, content: bytes) -> None:
    if path.is_file() and path.read_bytes() == content:
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_bytes(content)
    os.replace(temporary, path)


def tracked() -> bytes:
    listing = git("ls-files", "-z", "--cached", "--others", "--exclude-standard", binary=True)
    if listing is None:
        sys.exit("check inputs: git ls-files failed; the checks need a git checkout")
    paths = sorted({path for path in listing.decode().split("\0") if path and (ROOT / path).is_file()})
    return "".join(f"{path}\n" for path in paths).encode()


def release() -> str:
    """The newest release tag behind HEAD, or "" in a clone without tags."""
    return (git("describe", "--tags", "--abbrev=0", "--match", "v[0-9]*") or "").strip()


def store_baseline(tag: str) -> None:
    out = OUT / "store-baseline"
    write(out / "ref", f"{tag}\n".encode())
    schema = git("show", f"{tag}:{SCHEMA}", binary=True) if tag else None
    if schema is None:
        (out / "schema.rs").unlink(missing_ok=True)
    else:
        write(out / "schema.rs", schema)


def proto_baseline(tag: str) -> None:
    out = OUT / "proto-baseline"
    ref = out / "ref"
    if ref.is_file() and ref.read_text() == f"{tag}\n" and ((out / "proto").is_dir() or not tag):
        return
    shutil.rmtree(out / "proto", ignore_errors=True)
    archive = git("archive", "--format=tar", tag, "proto", binary=True) if tag else None
    if archive is not None:
        with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
            tar.extractall(out, filter="data")
    write(ref, f"{tag}\n".encode())


def warn(message: str) -> None:
    print(f"check inputs: {message}", file=sys.stderr)


def drop_cargo_snapshots() -> None:
    """What this wrote for cargo-deny before the worker's Cargo made it."""
    for name in ["metadata.json", "key", "index-refreshed"]:
        (DENY / name).unlink(missing_ok=True)
    for name in ["workspace", "index"]:
        shutil.rmtree(DENY / name, ignore_errors=True)


def git_in(directory: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(["git", "-C", str(directory), *args], capture_output=True, check=False, timeout=120)


def advisories() -> None:
    """Today's advisory database, as a snapshot the deny check declares."""
    fetched = ADVISORY_CLONE / ".git" / "amux-fetched"
    if not fetched.is_file() or time.time() - fetched.stat().st_mtime > ADVISORY_SECONDS:
        try:
            if (ADVISORY_CLONE / ".git").is_dir():
                result = git_in(ADVISORY_CLONE, "fetch", "--quiet", "--depth", "1", "origin", "HEAD")
                if result.returncode == 0:
                    result = git_in(ADVISORY_CLONE, "reset", "--quiet", "--hard", "FETCH_HEAD")
            else:
                ADVISORY_CLONE.parent.mkdir(parents=True, exist_ok=True)
                shutil.rmtree(ADVISORY_CLONE, ignore_errors=True)
                result = subprocess.run(
                    ["git", "clone", "--quiet", "--depth", "1", ADVISORIES, str(ADVISORY_CLONE)],
                    capture_output=True, check=False, timeout=120,
                )
            if result.returncode == 0:
                fetched.write_text("")
            else:
                warn(f"could not fetch the advisory database; the deny check reads the last snapshot: {result.stderr.decode().strip()}")
        except subprocess.TimeoutExpired:
            warn("fetching the advisory database timed out; the deny check reads the last snapshot")
    head = git_in(ADVISORY_CLONE, "log", "-1", "--format=%H%n%cI")
    if head.returncode != 0:
        warn("no advisory database to snapshot; the deny check fails without one")
        return
    ref = DENY / "advisory-db.ref"
    if ref.is_file() and ref.read_bytes() == head.stdout and (DENY / "advisory-db.tar").is_file():
        return
    archive = git_in(ADVISORY_CLONE, "archive", "--format=tar", "HEAD")
    if archive.returncode != 0:
        warn(f"could not archive the advisory database: {archive.stderr.decode().strip()}")
        return
    write(DENY / "advisory-db.tar", archive.stdout)
    write(ref, head.stdout)


# No target in these packages reads what this writes (build/BUCK's
# `tracked`, `day`, `store-baseline`, `proto-baseline` and `deny`).
UNREACHED = ("//crates/", "//apps/", "root//crates/", "root//apps/")
# buck2 test's options whose value is a label, not a target pattern.
LABEL_OPTIONS = ("--exclude", "--include")


def reaches_checks(arguments: list[str]) -> bool:
    patterns, label = [], False
    for argument in arguments:
        if argument == "--":
            break
        if label:
            label = False
        elif argument in LABEL_OPTIONS:
            label = True
        elif not argument.startswith("-"):
            patterns.append(argument)
    return not patterns or any(not pattern.startswith(UNREACHED) for pattern in patterns)


def main() -> None:
    if not reaches_checks(sys.argv[1:]):
        return
    listing = tracked()
    write(OUT / "tracked", listing)
    write(OUT / "day", f"{time.strftime('%Y-%m-%d', time.gmtime())}\n".encode())
    drop_cargo_snapshots()
    advisories()
    tag = release()
    store_baseline(tag)
    proto_baseline(tag)


if __name__ == "__main__":
    main()
