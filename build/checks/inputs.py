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
- `day`: today's date (UTC), which the npm audit (js/BUCK) declares: the npm
  registry's answer changes without any file changing, so the audit is kept
  for a day and asked again on the next.
- `deny/`: what cargo-deny reads (the root BUCK's `deny`). `metadata.json` is
  Cargo's resolved graph with every feature, which needs Cargo's registry, so
  it is made here, with this checkout's path replaced by `@ROOT@` and Cargo's
  home by `/cargo`, so every tree and machine writes the same bytes;
  `workspace/` holds the workspace's manifests, which cargo-deny reads beside
  it. Both are made again only when a manifest or the lockfile changes.
  `advisory-db.tar` and `advisory-db.ref` are a snapshot of the RustSec
  advisory database (its files and its commit and date), fetched at most once
  a day into a clone this machine's trees share; `index/` is crates.io's index
  entry for each crate in the graph, which says whether a release was yanked,
  copied from Cargo's cache after Cargo has refreshed it, at most once a day
  (`cargo update --dry-run`, which writes no lockfile). When a fetch fails,
  the last snapshot stays.

A file is written only when its content changes. The `just` recipes that run
the check targets run this first (`just checks`, `just test`, `just
premerge`); `scripts/buck2 test` alone uses whatever was last written.

    inputs.py
"""

import hashlib
import io
import json
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
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"


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
    if ref.is_file() and ref.read_text(encoding="utf-8") == f"{tag}\n" and ((out / "proto").is_dir() or not tag):
        return
    shutil.rmtree(out / "proto", ignore_errors=True)
    archive = git("archive", "--format=tar", tag, "proto", binary=True) if tag else None
    if archive is not None:
        with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
            tar.extractall(out, filter="data")
    write(ref, f"{tag}\n".encode())


def warn(message: str) -> None:
    print(f"check inputs: {message}", file=sys.stderr)


def cargo_inputs(listing: bytes) -> None:
    """Cargo's graph and the workspace's manifests, for cargo-deny."""
    manifests = sorted(path for path in listing.decode().splitlines() if path == "Cargo.toml" or path.endswith("/Cargo.toml"))
    key = hashlib.sha256()
    for path in ["Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml", *manifests]:
        file = ROOT / path
        key.update(path.encode() + b"\0" + (file.read_bytes() if file.is_file() else b"") + b"\0")
    stamp = DENY / "key"
    if stamp.is_file() and stamp.read_text(encoding="utf-8") == key.hexdigest() and (DENY / "metadata.json").is_file():
        return
    try:
        result = subprocess.run(
            ["cargo", "metadata", "--all-features", "--locked", "--format-version", "1"],
            cwd=ROOT, capture_output=True, text=True, encoding="utf-8", check=False,
        )
    except OSError as error:
        result = subprocess.CompletedProcess([], 1, "", str(error))
    if result.returncode != 0:
        # Not fatal: only the deny check reads it, and it fails without it.
        warn(f"cargo metadata failed, so the deny check has no graph to read:\n{result.stderr.strip()}")
        (DENY / "metadata.json").unlink(missing_ok=True)
        return
    metadata = json.loads(result.stdout)
    workspace = Path(metadata["workspace_root"])
    text = result.stdout.replace(str(workspace), "@ROOT@/build/checks/inputs/deny/workspace").replace(str(cargo_home()), "/cargo")
    shutil.rmtree(DENY / "workspace", ignore_errors=True)
    for package in metadata["packages"]:
        manifest = Path(package["manifest_path"])
        if workspace in manifest.parents:
            write(DENY / "workspace" / manifest.relative_to(workspace), manifest.read_bytes())
    for name in ["Cargo.toml", "Cargo.lock"]:
        write(DENY / "workspace" / name, (workspace / name).read_bytes())
    write(DENY / "metadata.json", text.encode())
    write(stamp, key.hexdigest().encode())


def cargo_home() -> Path:
    return Path(os.environ.get("CARGO_HOME") or Path.home() / ".cargo")


def index_path(name: str) -> str:
    """Where crates.io's index keeps a crate's entry, as Cargo's cache does."""
    name = name.lower()
    if len(name) <= 2:
        return f"{len(name)}/{name}"
    if len(name) == 3:
        return f"3/{name[0]}/{name}"
    return f"{name[:2]}/{name[2:4]}/{name}"


def index_entries() -> None:
    """Each locked crate's index entry, for the yanked check."""
    metadata = DENY / "metadata.json"
    if not metadata.is_file():
        return
    stamp = DENY / "index-refreshed"
    if not stamp.is_file() or time.time() - stamp.stat().st_mtime > ADVISORY_SECONDS:
        try:
            result = subprocess.run(["cargo", "update", "--dry-run"], cwd=ROOT, capture_output=True, text=True, encoding="utf-8", check=False, timeout=300)
            if result.returncode == 0:
                stamp.parent.mkdir(parents=True, exist_ok=True)
                stamp.touch()
            else:
                warn(f"could not refresh crates.io's index; the deny check reads the last copy: {result.stderr.strip()}")
        except (OSError, subprocess.TimeoutExpired) as error:
            warn(f"could not refresh crates.io's index ({error}); the deny check reads the last copy")
    caches = sorted((cargo_home() / "registry" / "index").glob("index.crates.io-*"))
    if not caches:
        warn("Cargo has no crates.io index cache; the deny check cannot tell a yanked release")
        return
    cache = caches[-1]
    names = sorted({package["name"] for package in json.loads(metadata.read_text(encoding="utf-8"))["packages"] if package["source"] == CRATES_IO})
    # Under `cache/`, not Cargo's `.cache/`: a Buck2 glob skips hidden files.
    wanted = {"config.json": cache / "config.json"} | {f"cache/{index_path(name)}": cache / ".cache" / index_path(name) for name in names}
    out = DENY / "index"
    for relative, source in wanted.items():
        if source.is_file():
            write(out / relative, source.read_bytes())
        else:
            warn(f"no index entry for {relative} in Cargo's cache; the deny check will say so")
    for stale in [path for path in out.rglob("*") if path.is_file() and path.relative_to(out).as_posix() not in wanted]:
        stale.unlink()


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


def main() -> None:
    listing = tracked()
    write(OUT / "tracked", listing)
    write(OUT / "day", f"{time.strftime('%Y-%m-%d', time.gmtime())}\n".encode())
    cargo_inputs(listing)
    index_entries()
    advisories()
    tag = release()
    store_baseline(tag)
    proto_baseline(tag)


if __name__ == "__main__":
    main()
