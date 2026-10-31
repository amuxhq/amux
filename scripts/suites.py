"""The Cargo tests a suite of tests/catalog.toml selects, as nextest runs them.

A suite's `cargo` selectors name a package and either Cargo targets of a kind
(`lib`, `bin`, or a `test` target with a harness of its own) or `modules`:
suites of the package's one integration test binary, `integration`
(tests/integration.rs, one module per suite). `selection` turns them into the
packages, features and filterset that `cargo nextest run` takes.
"""

from __future__ import annotations

from pathlib import Path
import re
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
# Every crate's integration suites are modules of this one test target.
INTEGRATION = "integration"


def integration_root(package_dir: Path) -> Path:
    return package_dir / "tests" / f"{INTEGRATION}.rs"


def integration_modules(package_dir: Path) -> set[str]:
    """The suites of a package's integration binary: the modules its root
    declares that hold tests (shared support modules hold none)."""
    root = integration_root(package_dir)
    if not root.is_file():
        return set()
    modules: set[str] = set()
    text = root.read_text()
    for match in re.finditer(r'^(?:#\[path = "([^"]+)"\]\n)?mod (\w+);', text, re.MULTILINE):
        path, name = match.groups()
        tests = root.parent
        candidates = [tests / path] if path else [tests / f"{name}.rs", tests / name / "mod.rs"]
        source = next((candidate for candidate in candidates if candidate.is_file()), None)
        if source is None:
            continue
        files = [source] + (sorted(source.parent.rglob("*.rs")) if source.name == "mod.rs" else [])
        if any(re.search(r"#\[(?:\w+::)*test\b|proptest!", file.read_text()) for file in files):
            modules.add(name)
    return modules


def filterset(selector: dict[str, Any]) -> str:
    """The nextest filterset of one selector."""
    package = selector["package"]
    modules = selector.get("modules", [])
    if modules:
        names = "|".join(sorted(modules))
        return f"(binary_id({package}::{INTEGRATION}) & test(/^({names})::/))"
    kind = selector.get("kind", "test")
    if kind == "lib":
        return f"(package({package}) & kind(lib))"
    binaries = "|".join(sorted(selector.get("targets", [])))
    return f"(package({package}) & kind({kind}) & binary(/^({binaries})$/))"


def selection(suite: dict[str, Any]) -> list[str]:
    """`cargo nextest run` arguments that run exactly the suite's Cargo tests.

    The workspace scope and features `just test` builds with, so a suite
    reuses its build: naming the packages instead unifies the dependencies'
    features differently and rebuilds much of the graph beside it. A
    feature only the suite needs is added to that scope."""
    selectors = suite.get("cargo", [])
    features = {
        f"{selector['package']}/{feature}"
        for selector in selectors
        for feature in selector.get("features", [])
    }
    scope = ["--workspace"]
    if features:
        scope += ["--features", ",".join(sorted(features))]
    return scope + ["-E", " | ".join(filterset(selector) for selector in selectors)]
