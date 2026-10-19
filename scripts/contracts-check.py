#!/usr/bin/env python3
"""Check that every contract in tests/contracts.toml names tests that exist.

A contract is a promise the architecture and testing pages make: a worked
failure, a row of the invariants table, or a suite contract. Each names the
tests asserting it:

  <package>/<target>::<test path>   a Cargo test; <target> is a test target
                                    name, `lib` for the library's unit tests
                                    or `bin:<name>` for a binary's
  swift:<Target>/<Class>/<method>   an XCTest method in a phone test bundle
  journey:<client>/<story>          a journey in journeys/manifest.json

Cargo names are checked against the listing of the test binaries the
workspace test run builds (`just test`, the same Cargo arguments), so a
contract names a test that run executes, and after that run nothing is
compiled again. The check runs on macOS, where every named test builds:
some are Unix-only and one is macOS-only. XCTest names are read from the test bundles' sources,
since building the bundles needs Xcode and a simulator.

A `model` contract is a property of a protocol model: it names the property
by its crate, machine and sentence, the checker test that explores it, and
the real-mode stories that demonstrate it (`tests`). The property must be
one the crate's `properties` binary lists, the checker a test of that crate
and no story one of its tests.
"""

from __future__ import annotations

import argparse
from collections import defaultdict
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
CONTRACTS = ROOT / "tests" / "contracts.toml"
CATALOG = ROOT / "tests" / "catalog.toml"
MANIFEST = ROOT / "journeys" / "manifest.json"
SOURCES = {"worked failure", "invariant", "testing page", "model"}
HEADINGS = {
    "worked failure": "Worked failures",
    "invariant": "Invariants",
    "testing page": "Testing page",
    "model": "Models",
}
# The binary of a property crate that lists its properties as JSON lines.
PROPERTIES_BINARY = "properties"
JOURNEY_CLIENTS = {"terminal", "phone", "system"}


def load(path: Path) -> dict[str, Any]:
    with path.open("rb") as source:
        return tomllib.load(source)


def parse_cargo(name: str) -> tuple[str, str, str, str] | None:
    """Split `package/target::path` into package, kind, target and test path."""
    match = re.fullmatch(r"([A-Za-z0-9_-]+)/((?:bin:)?[A-Za-z0-9_-]+)::(.+)", name)
    if not match:
        return None
    package, target, path = match.groups()
    if target == "lib":
        return package, "lib", package.replace("-", "_"), path
    if target.startswith("bin:"):
        return package, "bin", target[4:], path
    return package, "test", target, path


def catalog_targets() -> set[tuple[str, str, str]]:
    targets: set[tuple[str, str, str]] = set()
    for suite in load(CATALOG)["suite"]:
        for selector in suite.get("cargo", []):
            kind = selector.get("kind", "test")
            for target in selector.get("targets", []):
                targets.add((selector["package"], kind, target.replace("-", "_") if kind == "lib" else target))
    return targets


def cargo_listing(
    wanted: set[tuple[str, str, str]],
    catalogued: set[tuple[str, str, str]],
    errors: list[str],
) -> tuple[dict[tuple[str, str, str], set[str]], dict[str, Path]]:
    """List the tests each wanted binary of the workspace test build holds,
    and find each package's `properties` binary the same build made."""
    for key in sorted(wanted - catalogued):
        errors.append(f"Cargo target {key[0]}/{key[1]}:{key[2]} is not in tests/catalog.toml")
    environment = dict(os.environ, AMUX_TEST_DISCOVERY_MODE="disabled")
    # The compile phase of scripts/workspace-test.sh. Building packages one
    # at a time instead unifies their features differently and recompiles
    # hundreds of crates the test run has already built.
    built = subprocess.run(
        [
            "cargo", "test", "--locked", "--workspace", "--features", "bundled",
            "--all-targets", "--no-run", "--message-format=json",
        ],
        cwd=ROOT, text=True, capture_output=True, env=environment,
    )
    if built.returncode != 0:
        errors.append(f"building the workspace tests failed:\n{built.stderr[-4000:]}")
        return {}, {}
    packages = {
        package["id"]: package["name"]
        for package in json.loads(
            subprocess.run(
                ["cargo", "metadata", "--locked", "--format-version", "1", "--no-deps"],
                cwd=ROOT, text=True, capture_output=True, check=True,
            ).stdout
        )["packages"]
    }
    listing: dict[tuple[str, str, str], set[str]] = {}
    properties: dict[str, Path] = {}
    for line in built.stdout.splitlines():
        if not line.startswith("{"):
            continue
        message = json.loads(line)
        if message.get("reason") != "compiler-artifact" or not message.get("executable"):
            continue
        if not message["profile"]["test"]:
            if message["target"]["name"] == PROPERTIES_BINARY and "bin" in message["target"]["kind"]:
                properties[packages.get(message["package_id"], "")] = Path(message["executable"])
            continue
        target = message["target"]
        kinds = set(target["kind"])
        kind = "test" if "test" in kinds else "bin" if "bin" in kinds else "lib"
        key = (packages.get(message["package_id"], ""), kind, target["name"])
        if key not in wanted:
            continue
        listed = subprocess.run(
            [message["executable"], "--list", "--format", "terse"],
            cwd=ROOT,
            text=True,
            capture_output=True,
            env=environment,
        )
        if listed.returncode != 0:
            errors.append(f"listing {key[0]}/{kind}:{target['name']} failed:\n{listed.stderr[-2000:]}")
            continue
        listing[key] = {
            line.rsplit(": ", 1)[0]
            for line in listed.stdout.splitlines()
            if line.endswith(": test")
        }
    for package, kind, target in sorted(wanted):
        if (package, kind, target) not in listing:
            errors.append(f"the workspace test build made no test binary for {package}/{kind}:{target}")
    return listing, properties


def property_listing(crate: str, binary: Path | None, errors: list[str]) -> set[tuple[str, str]] | None:
    """Every (machine, sentence) the crate's `properties` binary lists."""
    if binary is None:
        errors.append(f"the workspace test build made no {PROPERTIES_BINARY} binary for {crate}")
        return None
    listed = subprocess.run([str(binary)], cwd=ROOT, text=True, capture_output=True)
    if listed.returncode != 0:
        errors.append(f"listing the properties of {crate} failed:\n{listed.stderr[-2000:]}")
        return None
    return {
        (entry["machine"], entry["sentence"])
        for entry in map(json.loads, listed.stdout.splitlines())
    }


def swift_listing() -> set[str]:
    """Every XCTest method as Target/Class/method, read from the bundle sources."""
    roots = [path for path in (ROOT / "apps" / "apple" / "Packages").glob("*/Tests/*") if path.is_dir()]
    roots += [
        path
        for path in (ROOT / "apps" / "apple").iterdir()
        if path.is_dir() and path.name.endswith("Tests")
    ]
    names: set[str] = set()
    for root in roots:
        for source in sorted(root.rglob("*.swift")):
            current: str | None = None
            for line in source.read_text().splitlines():
                declared = re.match(
                    r"^(?:final\s+|@MainActor\s+|open\s+)*(?:class\s+(\w+)\s*:|extension\s+(\w+)\b)",
                    line.strip(),
                )
                if declared:
                    current = declared.group(1) or declared.group(2)
                    continue
                method = re.match(r"^\s*(?:@MainActor\s+)?func\s+(test\w*)\s*\(\s*\)", line)
                if method and current:
                    names.add(f"{root.name}/{current}/{method.group(1)}")
    return names


def journey_listing() -> set[str]:
    manifest = json.loads(MANIFEST.read_text())
    names: set[str] = set()
    for journey in manifest.get("journeys", []):
        for client in journey.get("clients", []):
            names.add(f"{client}/{journey['id']}")
    return names


def check(contracts: list[dict[str, Any]], errors: list[str]) -> None:
    seen: set[str] = set()
    cargo: dict[tuple[str, str, str], set[str]] = defaultdict(set)
    properties: dict[str, set[tuple[str, str, str]]] = defaultdict(set)
    parsed: list[tuple[str, str, Any]] = []
    for index, contract in enumerate(contracts, 1):
        label = str(contract.get("id", f"entry {index}"))
        if label in seen:
            errors.append(f"{label}: duplicate contract id")
        seen.add(label)
        source = contract.get("source")
        if source not in SOURCES:
            errors.append(f"{label}: source must be one of {', '.join(sorted(SOURCES))}")
        tests = contract.get("tests")
        if not isinstance(tests, list) or not tests:
            errors.append(f"{label}: names no test")
            continue
        if source == "model":
            prop = contract.get("property")
            if not isinstance(prop, dict) or not all(
                isinstance(prop.get(field), str) and prop[field].strip()
                for field in ("crate", "machine", "sentence")
            ):
                errors.append(f"{label}: property must name its crate, machine and sentence")
                continue
            crate = prop["crate"]
            properties[crate].add((label, prop["machine"], prop["sentence"]))
            checker = contract.get("checker")
            split = parse_cargo(checker) if isinstance(checker, str) else None
            if split is None:
                errors.append(f"{label}: checker must name the Cargo test that runs the property")
            elif split[0] != crate:
                errors.append(f"{label}: checker {checker} is not a test of {crate}")
            else:
                cargo[split[0:3]].add(split[3])
                parsed.append((label, "cargo", split))
            for test in tests:
                if test.startswith(f"{crate}/"):
                    errors.append(f"{label}: {test} is a test of {crate}, not a story demonstrating the property")
        else:
            for field in ("section", "claim"):
                if not isinstance(contract.get(field), str) or not contract[field].strip():
                    errors.append(f"{label}: {field} must be a non-empty string")
        for test in tests:
            if test.startswith("swift:"):
                parsed.append((label, "swift", test[len("swift:"):]))
            elif test.startswith("journey:"):
                parsed.append((label, "journey", test[len("journey:"):]))
            else:
                split = parse_cargo(test)
                if split is None:
                    errors.append(f"{label}: cannot read test name {test!r}")
                    continue
                package, kind, target, path = split
                cargo[(package, kind, target)].add(path)
                parsed.append((label, "cargo", split))

    listing, binaries = cargo_listing(set(cargo), catalog_targets(), errors)
    for crate, named in sorted(properties.items()):
        listed = property_listing(crate, binaries.get(crate), errors)
        if listed is None:
            continue
        for label, machine, sentence in sorted(named):
            if (machine, sentence) not in listed:
                errors.append(f"{label}: {crate} has no {machine} property {sentence!r}")
    swift = swift_listing()
    journeys = journey_listing()
    for label, kind, name in parsed:
        if kind == "swift" and name not in swift:
            errors.append(f"{label}: XCTest method {name} does not exist")
        elif kind == "journey" and name not in journeys:
            errors.append(f"{label}: journey {name} is not in journeys/manifest.json")
        elif kind == "cargo":
            package, target_kind, target, path = name
            listed = listing.get((package, target_kind, target))
            if listed is not None and path not in listed:
                errors.append(f"{label}: Cargo test {package}/{target_kind}:{target} {path} does not exist")


def cell(text: str) -> str:
    return " ".join(text.split()).replace("|", "\\|")


def table(contracts: list[dict[str, Any]]) -> str:
    lines = [
        "# Contract coverage",
        "",
        "Every contract the architecture and testing pages make, with the tests asserting it,",
        "and every model property entered, with its checker and the stories demonstrating it.",
        "Generated from tests/contracts.toml by `just contracts-check --table <path>`.",
        "",
    ]
    for source, heading in HEADINGS.items():
        rows = [contract for contract in contracts if contract.get("source") == source]
        if source == "model":
            lines += [
                f"## {heading} ({len(rows)})",
                "",
                "| Property | Machine | Checker | Evidence |",
                "| --- | --- | --- | --- |",
            ]
        else:
            lines += [f"## {heading} ({len(rows)})", "", "| Contract | Section | Tests |", "| --- | --- | --- |"]
        for contract in rows:
            tests = "<br>".join(f"`{test}`" for test in contract["tests"])
            if source == "model":
                prop = contract["property"]
                lines.append(
                    f"| {cell(prop['sentence'])} | {cell(prop['crate'] + ' ' + prop['machine'])} "
                    f"| `{contract['checker']}` | {tests} |"
                )
            else:
                lines.append(f"| {cell(contract['claim'])} | {cell(contract['section'])} | {tests} |")
        lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--table", type=Path, help="also write the coverage table as Markdown to this path")
    arguments = parser.parse_args()
    try:
        contracts = load(CONTRACTS).get("contract", [])
    except (OSError, tomllib.TOMLDecodeError) as error:
        print(f"contracts: {error}", file=sys.stderr)
        return 1
    errors: list[str] = []
    if not contracts:
        errors.append("tests/contracts.toml lists no contracts")
    check(contracts, errors)
    if errors:
        for error in errors:
            print(f"contracts: {error}", file=sys.stderr)
        return 1
    if arguments.table:
        arguments.table.parent.mkdir(parents=True, exist_ok=True)
        arguments.table.write_text(table(contracts))
    tests = sum(len(contract["tests"]) + ("checker" in contract) for contract in contracts)
    print(f"contracts: {len(contracts)} contracts, {tests} named tests, every one found")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
