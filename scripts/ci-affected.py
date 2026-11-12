#!/usr/bin/env python3
"""Say which CI jobs a change can affect, so the rest are skipped.

Reads the files a push or pull request changed (`--base` and `--head`, two
commits) and prints one `name=true|false` line per group of jobs, the form
GitHub reads as a job's outputs:

  tests       Clippy, formatting and the workspace tests. Everything but a
              change to documentation alone.
  product     The terminal journeys, the simulated stories and doctests. Not documentation, and not the phone app alone,
              which the iOS gate judges.
  models      The protocol models: a crate the models crate is built from.
  sanitizers  The sanitizers: a crate whose tests they run. Not the crates
              those are built from, which is nearly every crate: the
              sanitizers are there for the unsafe code in the crates
              themselves, and a nightly run covers the rest.
  bridge      The pointer checks and the iOS target check: a crate the
              phone's bridge is built from.

The policy job is not here: it is a minute, holds the documentation check,
and always runs.

A file is documentation, the phone app, one crate's, or anything else.
Anything else (a manifest, a script, a workflow, a journey, a schema) can
reach every job, so all of them run. Which crates a crate is built from is
read from the manifests, not from cargo, so this needs no toolchain. When the
two commits cannot be compared, every job runs.

A test that reads a file under `docs/` names it in READS, or a change to that
file would skip the test; this script's own tests fail on one that does not.
"""

from __future__ import annotations

import argparse
import ast
import subprocess
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # Apple's command-line tools still ship Python 3.9.
    from pip._vendor import tomli as tomllib

ROOT = Path(__file__).resolve().parents[1]
GROUPS = ["tests", "product", "models", "sanitizers", "bridge"]

# Documentation: nothing built or tested reads it, but for READS.
DOCS = ("docs/", "images/", "marketing/")
# A file under DOCS that a crate's test reads, and that crate.
READS = {"docs/guide/understand/what-amux-sends.md": "analytics"}
PHONE = "apps/apple/"

MODELS = ["models"]
BRIDGE = ["app-ffi"]


def sanitized() -> list[str]:
    """The crates whose tests the sanitizers run (scripts/sanitize.py)."""
    names: list[str] = []
    for line in (ROOT / "scripts/sanitize.py").read_text().splitlines():
        if line.startswith(("WHOLE = ", "LIBRARY = ")):
            names += ast.literal_eval(line.split(" = ", 1)[1])
    if not names:
        raise SystemExit("scripts/sanitize.py no longer lists its crates as WHOLE and LIBRARY")
    return names


def workspace(root: Path = ROOT) -> dict[str, dict]:
    """Each workspace crate by name: its folder and the crates it names."""
    crates: dict[str, dict] = {}
    for manifest in sorted((root / "crates").glob("*/Cargo.toml")):
        data = tomllib.loads(manifest.read_text())
        tables = [data] + list(data.get("target", {}).values())

        def named(kind: str) -> set[str]:
            return {
                (spec.get("package") if isinstance(spec, dict) else None) or key
                for table in tables
                for key, spec in table.get(kind, {}).items()
            }

        crates[data["package"]["name"]] = {
            "folder": manifest.parent.name,
            "built": named("dependencies") | named("build-dependencies"),
            "tests": named("dev-dependencies"),
        }
    return crates


def built_from(names: list[str], crates: dict[str, dict]) -> set[str]:
    """The workspace crates `names` and their tests are built from.

    A crate's own test dependencies count; those of the crates beneath it
    do not, since cargo builds none of them.
    """
    found = set(names)
    queue = [dep for name in names for dep in crates[name]["built"] | crates[name]["tests"]]
    while queue:
        name = queue.pop()
        if name in crates and name not in found:
            found.add(name)
            queue.extend(crates[name]["built"])
    return found


def affected(files: list[str], crates: dict[str, dict], sanitizers: list[str]) -> dict[str, bool]:
    by_folder = {crate["folder"]: name for name, crate in crates.items()}
    changed: set[str] = set()
    everything = phone = False
    for file in files:
        parts = file.split("/")
        if file in READS:
            changed.add(READS[file])
        elif file.startswith(DOCS) or ("/" not in file and file.endswith(".md")):
            continue
        elif file.startswith(PHONE):
            phone = True
        elif parts[0] == "crates" and len(parts) > 2 and parts[1] in by_folder:
            changed.add(by_folder[parts[1]])
        else:
            everything = True
    product = everything or bool(changed)

    def reaches(names: list[str]) -> bool:
        return everything or bool(changed & built_from(names, crates))

    return {
        "tests": product or phone,
        "product": product,
        "models": reaches(MODELS),
        "sanitizers": everything or bool(changed & set(sanitizers)),
        "bridge": reaches(BRIDGE),
    }


def changed_files(base: str, head: str) -> list[str] | None:
    """The files that differ between two commits; None when they cannot be compared."""
    if not base or set(base) == {"0"}:
        return None
    for commit in (base, head):
        present = subprocess.run(
            ["git", "cat-file", "-e", f"{commit}^{{commit}}"], cwd=ROOT, capture_output=True
        )
        if present.returncode != 0:
            subprocess.run(
                ["git", "fetch", "--quiet", "--depth=1", "origin", commit],
                cwd=ROOT, capture_output=True, timeout=120,
            )
    diff = subprocess.run(
        ["git", "diff", "--name-only", "--no-renames", base, head],
        cwd=ROOT, capture_output=True, text=True,
    )
    if diff.returncode != 0:
        return None
    return [line for line in diff.stdout.splitlines() if line]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default="")
    parser.add_argument("--head", default="HEAD")
    args = parser.parse_args()
    files = changed_files(args.base, args.head)
    if files is None:
        print("the change cannot be read, so every job runs", file=sys.stderr)
        result = dict.fromkeys(GROUPS, True)
    else:
        result = affected(files, workspace(), sanitized())
        print(f"{len(files)} files changed", file=sys.stderr)
    for group in GROUPS:
        print(f"{group}={'true' if result[group] else 'false'}")


if __name__ == "__main__":
    main()
