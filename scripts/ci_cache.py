#!/usr/bin/env python3
"""Delete a CI job's older entries from GitHub's Actions cache, and say
whether a job's store changed.

    ci_cache.py prune PREFIX [--keep N] [--except KEY]
    ci_cache.py digest DIR...

`prune` deletes this ref's entries whose keys start with PREFIX: all but
KEY with `--except`, or all but the newest N with `--keep`. Run in a job
with `actions: write` and GH_TOKEN set; the repository and ref come from
GITHUB_REPOSITORY and GITHUB_REF.

GitHub keeps 10 GB of cache for the whole repository and evicts the least
recently used entry past that, so a job that saves a new entry every run
would otherwise push out other jobs' caches (docs/CI.md, "Workflows").

`digest` prints one line that changes when anything under the DIRs does:
a file added, removed, resized or written again. A macOS job's store is
NativeLink's CAS and action cache, which write every blob and result to a
new file and never change one in place, so a store that digests the same
after the run as when it was restored holds nothing new, and the job needn't
spend a minute or two archiving and uploading it again (docs/CI.md, "The
macOS jobs"). A DIR that doesn't exist digests as empty.
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path


def gh(*args: str) -> str:
    return subprocess.run(["gh", "api", *args], check=True, capture_output=True, text=True).stdout


def entries(repository: str, ref: str, prefix: str) -> list[dict]:
    found, page = [], 1
    while True:
        out = json.loads(gh(f"repos/{repository}/actions/caches?key={prefix}&ref={ref}&per_page=100&page={page}"))
        caches = out.get("actions_caches", [])
        found += [cache for cache in caches if cache["key"].startswith(prefix)]
        if len(caches) < 100:
            return found
        page += 1


def digest(directories: list[Path]) -> str:
    hashed = hashlib.sha256()
    for directory in directories:
        files = sorted(path for path in directory.rglob("*") if path.is_file()) if directory.is_dir() else []
        for path in files:
            status = path.stat()
            hashed.update(f"{path}\0{status.st_size}\0{status.st_mtime_ns}\n".encode())
    return hashed.hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    prune = commands.add_parser("prune", help="delete a job's older cache entries")
    prune.add_argument("prefix")
    prune.add_argument("--keep", type=int, default=0, help="keep the newest N")
    prune.add_argument("--except", dest="keep_key", help="keep this key")
    digested = commands.add_parser("digest", help="say whether a store changed")
    digested.add_argument("directories", nargs="+", type=Path)
    args = parser.parse_args()
    if args.command == "digest":
        print(digest(args.directories))
        return
    repository, ref = os.environ["GITHUB_REPOSITORY"], os.environ["GITHUB_REF"]
    caches = sorted(entries(repository, ref, args.prefix), key=lambda cache: cache["created_at"], reverse=True)
    doomed = [cache for cache in caches[args.keep:] if cache["key"] != args.keep_key]
    for cache in doomed:
        gh("-X", "DELETE", f"repos/{repository}/actions/caches/{cache['id']}")
        print(f"deleted {cache['key']} ({cache['size_in_bytes'] / 1e6:.0f} MB)")
    print(f"kept {len(caches) - len(doomed)} of {len(caches)} entries starting {args.prefix} on {ref}")


if __name__ == "__main__":
    sys.exit(main())
