#!/usr/bin/env python3
"""Check that each crate's BUCK file says what its Cargo.toml says.

Cargo still builds the workspace (CI, rust-analyzer, `cargo` by hand), so a
crate's manifest and its BUCK file describe the same crate twice and are
kept in step by hand (build/rust.bzl). This fails when they disagree on:

- the crate's targets: its library, each [[bin]], [[example]] and [[test]];
- its dependencies, by name: [dependencies] against the library's (or, for
  a crate without one, its binaries') `deps`, and [dev-dependencies]
  against what its unit tests add to those. A dependency in any branch of
  a `select` counts, as a platform-specific or optional one does in Cargo.

    parity.py [--metadata cargo-metadata.json] [--buck buck-targets.json]

With no arguments it asks Buck2 for both: //:cargo-metadata, Cargo's view
from a Linux worker (scripts/cargo_metadata.py, which asks this machine's
Cargo when no worker is in reach), and `buck2 uquery`.
"""

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


sys.path.insert(0, str(ROOT / "scripts"))
import cargo_metadata as cargo  # noqa: E402


def cargo_metadata(path: str | None) -> dict:
    if path:
        return cargo.read(Path(path))
    return cargo.load()


def buck_targets(path: str | None) -> dict:
    if path:
        return json.loads(Path(path).read_text())
    return json.loads(
        subprocess.check_output(
            [
                str(ROOT / "scripts/buck2"),
                "uquery",
                "kind('^rust_(library|binary)$', //crates/...)",
                "--output-attribute",
                "^(deps|name|buck.type)$",
                "--json",
            ],
            cwd=ROOT,
            text=True,
            stderr=subprocess.DEVNULL,
        )
    )


def labels(value) -> set[str]:
    """Every label in a `deps` value, through any `select`s and `+`s."""
    found: set[str] = set()
    if isinstance(value, str):
        found.add(value)
    elif isinstance(value, list):
        for item in value:
            found |= labels(item)
    elif isinstance(value, dict):
        for item in value.values():
            found |= labels(item)
    return found


def crate_of(label: str) -> str | None:
    """The Cargo name a dependency label stands for."""
    target = label.split("//", 1)[-1]
    package, _, name = target.partition(":")
    if package == "third-party/rust":
        return name
    if package.startswith("crates/"):
        return name
    return None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--metadata")
    parser.add_argument("--buck")
    args = parser.parse_args()

    metadata = cargo_metadata(args.metadata)
    members = set(metadata["workspace_members"])
    # The workspace's own crates; a member elsewhere (third-party/rust/dev-only,
    # which only lists crates for reindeer) has no Buck target of its own.
    packages = {
        p["name"]: p
        for p in metadata["packages"]
        if p["id"] in members and Path(p["manifest_path"]).parent.parent == ROOT / "crates"
    }

    rules: dict[str, dict[str, dict]] = {}
    for label, attributes in buck_targets(args.buck).items():
        target = label.split("//", 1)[-1]
        package, _, name = target.partition(":")
        crate = package.removeprefix("crates/")
        rules.setdefault(crate, {})[name] = attributes

    failures: list[str] = []
    for name, package in sorted(packages.items()):
        crate_rules = rules.get(name)
        if crate_rules is None:
            failures.append(f"{name}: no BUCK targets (crates/{name}/BUCK)")
            continue

        has_lib = False
        for target in package["targets"]:
            kinds = set(target["kind"])
            if kinds & {"lib", "rlib", "staticlib", "cdylib", "proc-macro"}:
                has_lib = True
                wanted = name
            elif kinds & {"bin", "example"}:
                # A binary named after its crate's library takes `-bin`.
                has_library = any(
                    set(t["kind"]) & {"lib", "rlib", "staticlib", "cdylib", "proc-macro"}
                    for t in package["targets"]
                )
                wanted = target["name"] + ("-bin" if has_library and target["name"] == name else "")
            elif "test" in kinds:
                wanted = target["name"] + "-binary"
            else:
                continue
            if wanted not in crate_rules:
                failures.append(f"{name}: Cargo's {'/'.join(sorted(kinds))} target {target['name']} has no BUCK target :{wanted}")

        def cargo_deps(kind):
            return {d["name"] for d in package["dependencies"] if d["kind"] == kind}

        main = crate_rules.get(name) if has_lib else None
        if main is None:
            binaries = [r for n, r in crate_rules.items() if r.get("buck.type", "").endswith("rust_binary") and not n.endswith("-binary")]
            main = binaries[0] if binaries else None
        if main is not None:
            buck_normal = {c for c in map(crate_of, labels(main.get("deps", []))) if c and c != name}
            if (want := cargo_deps(None)) != buck_normal:
                if missing := want - buck_normal:
                    failures.append(f"{name}: [dependencies] has {sorted(missing)}, which BUCK's deps lack")
                if extra := buck_normal - want:
                    failures.append(f"{name}: BUCK's deps have {sorted(extra)}, which [dependencies] lacks")

        unit = crate_rules.get("unit-binary")
        if unit is not None and main is not None:
            buck_dev = {c for c in map(crate_of, labels(unit.get("deps", []))) if c} - {
                c for c in map(crate_of, labels(main.get("deps", []))) if c
            }
            want = cargo_deps("dev") - cargo_deps(None)
            if missing := want - buck_dev:
                failures.append(f"{name}: [dev-dependencies] has {sorted(missing)}, which the unit tests' deps lack")
            if extra := buck_dev - want:
                failures.append(f"{name}: the unit tests' deps have {sorted(extra)}, which [dev-dependencies] lacks")

    if failures:
        print("BUCK and Cargo disagree:")
        for failure in failures:
            print(f"  - {failure}")
        return 1
    print(f"BUCK and Cargo agree on {len(packages)} crates")
    return 0


if __name__ == "__main__":
    sys.exit(main())
