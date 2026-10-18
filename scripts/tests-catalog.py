#!/usr/bin/env python3
"""List and validate the repository's test-suite catalogue."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tomllib
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
CATALOG = ROOT / "tests" / "catalog.toml"
BOUNDARIES = {
    "Interpreter",
    "Provider adapter",
    "Single daemon",
    "Many daemons",
    "Client model",
    "Store, effects, bridge",
    "Views",
    "Native presentation",
    "System composition",
    "Journeys",
    "Model",
}
TIMES = {"driven", "real", "none"}
LANES = {"fast", "system", "journey", "qualification", "tool"}
REQUIRED = {
    "name",
    "contract",
    "paths",
    "recipe",
    "boundary",
    "oracle",
    "real",
    "substituted",
    "time",
    "capabilities",
    "lane",
    "evidence",
    "update",
}
CI_WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
MODELS_SUITE = "protocol-models"
PERF_BASELINE_ROOTS = (
    ROOT / "perf" / "baselines" / "desktop",
    ROOT / "perf" / "baselines" / "phone",
)


def run(*arguments: str) -> str:
    return subprocess.run(
        arguments,
        cwd=ROOT,
        check=True,
        text=True,
        capture_output=True,
    ).stdout


def load_catalog() -> list[dict[str, Any]]:
    with CATALOG.open("rb") as source:
        parsed = tomllib.load(source)
    suites = parsed.get("suite")
    if not isinstance(suites, list):
        raise ValueError("tests/catalog.toml must contain [[suite]] entries")
    return suites


def cargo_kind(target: dict[str, Any]) -> str:
    kinds = set(target["kind"])
    if "test" in kinds:
        return "test"
    if "bin" in kinds:
        return "bin"
    return "lib"


def cargo_inventory() -> tuple[dict[tuple[str, str, str], dict[str, Any]], dict[str, set[str]]]:
    metadata = json.loads(run("cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"))
    targets: dict[tuple[str, str, str], dict[str, Any]] = {}
    features: dict[str, set[str]] = {}
    for package in metadata["packages"]:
        name = package["name"]
        features[name] = set(package.get("features", {}))
        for target in package["targets"]:
            if target.get("test"):
                targets[(name, cargo_kind(target), target["name"])] = target
    return targets, features


def swift_inventory() -> set[str]:
    targets: set[str] = set()
    for manifest in sorted((ROOT / "apps" / "apple" / "Packages").glob("*/Package.swift")):
        text = manifest.read_text()
        targets.update(
            re.findall(r"\.testTarget\s*\(\s*name:\s*\"([^\"]+)\"", text, re.DOTALL)
        )

    project = (ROOT / "apps" / "apple" / "project.yml").read_text().splitlines()
    in_targets = False
    current: str | None = None
    for line in project:
        if line == "targets:":
            in_targets = True
            continue
        if in_targets and line == "schemes:":
            break
        match = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
        if in_targets and match:
            current = match.group(1)
            continue
        if current and re.match(r"^    type: bundle\.(?:unit-test|ui-testing)\s*$", line):
            targets.add(current)
    return targets


def just_recipes() -> set[str]:
    return set(run("just", "--summary").split())


def recipe_name(command: str) -> str | None:
    try:
        words = shlex.split(command)
    except ValueError:
        return None
    if len(words) < 2 or words[0] != "just":
        return None
    if words[1] == "ios":
        return f"ios::{words[2]}" if len(words) >= 3 else None
    return words[1]


def validate_cargo_recipe(
    label: str,
    command: str,
    selectors: list[dict[str, Any]],
    errors: list[str],
) -> None:
    if not selectors:
        return
    words = shlex.split(command)
    selected = {
        (selector["package"], selector.get("kind", "test"), target)
        for selector in selectors
        for target in selector.get("targets", [])
    }
    packages = {package for package, _, _ in selected}
    recipe = recipe_name(command)
    if recipe == "test-crate" and len(words) >= 3:
        package = words[2]
        if package not in packages:
            errors.append(f"{label}: recipe selects unlisted Cargo package {package}")
        if "--test" in words:
            position = words.index("--test")
            if position + 1 >= len(words) or (package, "test", words[position + 1]) not in selected:
                value = words[position + 1] if position + 1 < len(words) else "<missing>"
                errors.append(f"{label}: recipe selects zero catalogued tests named {value!r}")
    elif recipe == "test":
        requested = {words[index + 1] for index, word in enumerate(words[:-1]) if word == "-p"}
        if requested and not requested <= packages:
            errors.append(
                f"{label}: recipe selects unlisted Cargo packages "
                + ", ".join(sorted(requested - packages))
            )
        if "--test" in words:
            position = words.index("--test")
            value = words[position + 1] if position + 1 < len(words) else "<missing>"
            if not any(kind == "test" and target == value for _, kind, target in selected):
                errors.append(f"{label}: recipe selects zero catalogued tests named {value!r}")


def expand(pattern: str) -> list[Path]:
    if any(character in pattern for character in "*?["):
        return sorted(ROOT.glob(pattern))
    path = ROOT / pattern
    return [path] if path.exists() else []


def journey_inventory(manifest: str) -> set[str]:
    parsed = json.loads((ROOT / manifest).read_text())
    return {journey["id"] for journey in parsed.get("journeys", [])}


def validate(suites: list[dict[str, Any]]) -> tuple[list[str], set[tuple[str, str, str]], set[str]]:
    errors: list[str] = []
    cargo_targets, cargo_features = cargo_inventory()
    swift_targets = swift_inventory()
    recipes = just_recipes()
    seen_names: set[str] = set()
    catalog_cargo: dict[tuple[str, str, str], str] = {}
    catalog_swift: dict[str, str] = {}
    catalog_journeys: dict[tuple[str, str], str] = {}
    catalog_baselines: dict[str, str] = {}

    for index, suite in enumerate(suites, 1):
        label = str(suite.get("name", f"entry {index}"))
        missing = REQUIRED - suite.keys()
        if missing:
            errors.append(f"{label}: missing fields {', '.join(sorted(missing))}")
        if label in seen_names:
            errors.append(f"{label}: duplicate suite name")
        seen_names.add(label)
        if not isinstance(suite.get("contract"), str) or not suite.get("contract", "").strip():
            errors.append(f"{label}: contract must be a non-empty sentence")
        if suite.get("boundary") not in BOUNDARIES:
            errors.append(f"{label}: unknown boundary {suite.get('boundary')!r}")
        if suite.get("time") not in TIMES:
            errors.append(f"{label}: unknown time mode {suite.get('time')!r}")
        if suite.get("lane") not in LANES:
            errors.append(f"{label}: unknown lane {suite.get('lane')!r}")
        for field in ("paths", "real", "substituted", "capabilities"):
            if not isinstance(suite.get(field), list):
                errors.append(f"{label}: {field} must be a list")
        for field in ("recipe", "oracle", "evidence", "update"):
            if not isinstance(suite.get(field), str):
                errors.append(f"{label}: {field} must be a string")

        command = suite.get("recipe", "")
        recipe = recipe_name(command) if isinstance(command, str) else None
        if recipe is None:
            errors.append(f"{label}: recipe must start with a concrete just recipe")
        elif recipe not in recipes:
            errors.append(f"{label}: recipe {recipe!r} does not exist")
        if isinstance(command, str):
            validate_cargo_recipe(label, command, suite.get("cargo", []), errors)

        for pattern in suite.get("paths", []):
            matches = expand(pattern)
            if not matches:
                errors.append(f"{label}: path {pattern!r} does not exist")

        selected = 0
        for selector in suite.get("cargo", []):
            package = selector.get("package")
            kind = selector.get("kind", "test")
            features = set(selector.get("features", []))
            if kind not in {"lib", "bin", "test"}:
                errors.append(f"{label}: unknown Cargo target kind {kind!r}")
            if package not in cargo_features:
                errors.append(f"{label}: Cargo package {package!r} does not exist")
                continue
            unknown_features = features - cargo_features[package]
            if unknown_features:
                errors.append(
                    f"{label}: Cargo package {package} has no features "
                    + ", ".join(sorted(unknown_features))
                )
            for target_name in selector.get("targets", []):
                selected += 1
                key = (package, kind, target_name)
                target = cargo_targets.get(key)
                if target is None:
                    errors.append(f"{label}: Cargo test target {package}/{kind}:{target_name} does not exist")
                    continue
                required = set(target.get("required-features", []))
                if not required <= features:
                    errors.append(
                        f"{label}: Cargo target {package}/{kind}:{target_name} omits required features "
                        + ", ".join(sorted(required - features))
                    )
                if key in catalog_cargo:
                    errors.append(
                        f"{label}: Cargo target {package}/{kind}:{target_name} is already listed by "
                        f"{catalog_cargo[key]}"
                    )
                catalog_cargo[key] = label

        for target in suite.get("swift", []):
            selected += 1
            if target not in swift_targets:
                errors.append(f"{label}: Swift test target {target!r} does not exist")
            if target in catalog_swift:
                errors.append(f"{label}: Swift target {target} is already listed by {catalog_swift[target]}")
            catalog_swift[target] = label

        manifest = suite.get("journey_manifest")
        journeys = suite.get("journeys", [])
        if journeys and not isinstance(manifest, str):
            errors.append(f"{label}: journey selections require journey_manifest")
        elif isinstance(manifest, str):
            if not (ROOT / manifest).is_file():
                errors.append(f"{label}: journey manifest {manifest!r} does not exist")
            else:
                available = journey_inventory(manifest)
                for journey in journeys:
                    selected += 1
                    key = (manifest, journey)
                    if journey not in available:
                        errors.append(f"{label}: journey {journey!r} does not exist in {manifest}")
                    if key in catalog_journeys:
                        errors.append(f"{label}: journey {journey} is already listed by {catalog_journeys[key]}")
                    catalog_journeys[key] = label

        for manifest_path in suite.get("golden_manifests", []):
            selected += 1
            path = ROOT / manifest_path
            if not path.is_file():
                errors.append(f"{label}: golden manifest {manifest_path!r} does not exist")
                continue
            try:
                manifest_json = json.loads(path.read_text())
            except json.JSONDecodeError as error:
                errors.append(f"{label}: golden manifest {manifest_path} is invalid: {error}")
                continue
            if not manifest_json.get("screens"):
                errors.append(f"{label}: golden manifest {manifest_path} selects zero screens")

        for baseline in suite.get("baselines", []):
            selected += 1
            if not (ROOT / baseline).exists():
                errors.append(f"{label}: baseline path {baseline!r} does not exist")
            if baseline in catalog_baselines:
                errors.append(f"{label}: baseline {baseline} is already listed by {catalog_baselines[baseline]}")
            catalog_baselines[baseline] = label

        for pattern in suite.get("cases", []):
            matches = expand(pattern)
            selected += len(matches)
            if not matches:
                errors.append(f"{label}: case selection {pattern!r} matches nothing")

        if selected == 0:
            errors.append(f"{label}: recipe would select zero catalogued tests")

    missing_cargo = sorted(set(cargo_targets) - set(catalog_cargo))
    for package, kind, target in missing_cargo:
        errors.append(f"Cargo test target {package}/{kind}:{target} is not listed")
    missing_swift = sorted(swift_targets - set(catalog_swift))
    for target in missing_swift:
        errors.append(f"Swift test target {target} is not listed")

    manifests = {
        suite["journey_manifest"]
        for suite in suites
        if isinstance(suite.get("journey_manifest"), str)
    }
    for manifest in sorted(manifests):
        if not (ROOT / manifest).is_file():
            continue
        listed = {journey for listed_manifest, journey in catalog_journeys if listed_manifest == manifest}
        for journey in sorted(journey_inventory(manifest) - listed):
            errors.append(f"journey {journey!r} from {manifest} is not listed")

    discovered_baselines = {
        path.relative_to(ROOT).as_posix()
        for root in PERF_BASELINE_ROOTS
        for path in root.glob("*.json")
    }
    for baseline in sorted(discovered_baselines - set(catalog_baselines)):
        errors.append(f"performance baseline {baseline} is not listed")

    errors.extend(validate_models_matrix(suites, CI_WORKFLOW.read_text()))

    return errors, set(catalog_cargo), set(catalog_swift)


def models_matrix(text: str) -> set[str] | None:
    """The test targets a CI workflow's models job runs, one per matrix entry."""
    job = re.search(r"^  models:\n((?:    .*\n|\s*\n)*)", text, re.MULTILINE)
    if job is None:
        return None
    targets = re.search(r"^\s+target: \[([^\]]*)\]", job.group(1), re.MULTILINE)
    if targets is None:
        return None
    return {target.strip() for target in targets.group(1).split(",") if target.strip()}


def validate_models_matrix(suites: list[dict[str, Any]], workflow: str) -> list[str]:
    """Every protocol model's test target is its own entry of the CI models job."""
    suite = next((suite for suite in suites if suite["name"] == MODELS_SUITE), None)
    if suite is None:
        return [f"suite {MODELS_SUITE} is missing"]
    targets = {
        target
        for entry in suite.get("cargo", [])
        if entry.get("kind", "test") == "test" and entry.get("package") == "models"
        for target in entry.get("targets", [])
    }
    matrix = models_matrix(workflow)
    if matrix is None:
        return [f"{CI_WORKFLOW.relative_to(ROOT)} has no models job with a target matrix"]
    errors = [
        f"{MODELS_SUITE} test target {target} is not in the CI models job's matrix"
        for target in sorted(targets - matrix)
    ]
    errors.extend(
        f"the CI models job's matrix names {target}, which is no {MODELS_SUITE} test target"
        for target in sorted(matrix - targets)
    )
    return errors


def print_table(suites: list[dict[str, Any]], cargo: set[tuple[str, str, str]], swift: set[str]) -> None:
    headers = ("suite", "boundary", "lane", "time", "recipe")
    rows = [
        (suite["name"], suite["boundary"], suite["lane"], suite["time"], suite["recipe"])
        for suite in suites
    ]
    widths = [max(len(headers[column]), *(len(str(row[column])) for row in rows)) for column in range(5)]
    print("  ".join(headers[column].ljust(widths[column]) for column in range(5)))
    print("  ".join("-" * width for width in widths))
    for row in rows:
        print("  ".join(str(row[column]).ljust(widths[column]) for column in range(5)))
    print("\nCargo test targets:")
    for package, kind, target in sorted(cargo):
        print(f"  {package}/{kind}:{target}")
    print("\nPhone test targets:")
    for target in sorted(swift):
        print(f"  {target}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=("list", "check"))
    arguments = parser.parse_args()
    try:
        suites = load_catalog()
        errors, cargo, swift = validate(suites)
    except (OSError, ValueError, subprocess.CalledProcessError, tomllib.TOMLDecodeError) as error:
        print(f"tests catalogue: {error}", file=sys.stderr)
        return 1
    if errors:
        for error in errors:
            print(f"tests catalogue: {error}", file=sys.stderr)
        return 1
    if arguments.mode == "list":
        print_table(suites, cargo, swift)
    else:
        print(f"tests catalogue: {len(suites)} suites, {len(cargo)} Cargo targets, {len(swift)} phone targets")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
