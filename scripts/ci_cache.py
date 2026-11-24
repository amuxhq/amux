#!/usr/bin/env python3
"""Delete a CI job's older entries from GitHub's Actions cache.

    ci_cache.py prune PREFIX [--keep N] [--except KEY]

Deletes this ref's entries whose keys start with PREFIX: all but KEY with
`--except`, or all but the newest N with `--keep`. Run in a job with
`actions: write` and GH_TOKEN set; the repository and ref come from
GITHUB_REPOSITORY and GITHUB_REF.

GitHub keeps 10 GB of cache for the whole repository and evicts the least
recently used entry past that, so a job that saves a new entry every run
would otherwise push out other jobs' caches (docs/CI.md, "Workflows").
"""

import argparse
import json
import os
import subprocess
import sys


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


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("command", choices=["prune"])
    parser.add_argument("prefix")
    parser.add_argument("--keep", type=int, default=0, help="keep the newest N")
    parser.add_argument("--except", dest="keep_key", help="keep this key")
    args = parser.parse_args()
    repository, ref = os.environ["GITHUB_REPOSITORY"], os.environ["GITHUB_REF"]
    caches = sorted(entries(repository, ref, args.prefix), key=lambda cache: cache["created_at"], reverse=True)
    doomed = [cache for cache in caches[args.keep:] if cache["key"] != args.keep_key]
    for cache in doomed:
        gh("-X", "DELETE", f"repos/{repository}/actions/caches/{cache['id']}")
        print(f"deleted {cache['key']} ({cache['size_in_bytes'] / 1e6:.0f} MB)")
    print(f"kept {len(caches) - len(doomed)} of {len(caches)} entries starting {args.prefix} on {ref}")


if __name__ == "__main__":
    sys.exit(main())
