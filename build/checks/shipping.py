#!/usr/bin/env python3
"""Check that nothing built only for tests reaches what ships.

Buck2 builds a crate for tests with its test features on, and builds the
crates only tests and tools use (docs/BUILD.md, "Configurations"); the
shipping configuration is meant to carry neither. This builds nothing: it
asks Buck2 for the configured graph of each product as it ships, the code
linked into it (not the build scripts and proc macros that run while
building it), and fails when that graph holds

- a crate that exists for tests and tools, or
- a workspace crate with one of its test features on.

    shipping.py
"""

import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
# scripts/buck2 is a shell script, which Windows can't start itself.
BUCK2 = (["sh"] if os.name == "nt" else []) + [str(ROOT / "scripts/buck2")]

# Each product as it ships: its root target and its configuration.
PRODUCTS = {
    "the desktop binary": ("//crates/amux:amux", "macos-cli-shipping-release"),
    "the browser extension's WebAssembly": ("//crates/browser-core:browser-core", "wasm-app-shipping-wasm"),
}

# The crates only tests and tools use.
TEST_CRATES = {"models", "patience", "provider-fakes", "fake-amux", "qualification", "replay-support", "shot", "testnet", "source-tree"}

# Each workspace crate's features that exist for tests and tools.
TEST_FEATURES = {"debug-tools", "explore", "fixtures", "schema", "synthetic", "test-key", "testkind"}


def graph(root: str, platform: str) -> dict:
    out = subprocess.run(
        [
            *BUCK2,
            "cquery",
            f"deps({root}, 100000, target_deps())",
            "--target-platforms",
            f"root//build/platforms:{platform}",
            "--output-attribute",
            "^features$",
            "--json",
        ],
        cwd=ROOT,
        check=True,
        text=True,
        capture_output=True,
    ).stdout
    return json.loads(out)


def main() -> int:
    failures = []
    for product, (root, platform) in PRODUCTS.items():
        for label, attributes in graph(root, platform).items():
            target = label.split(" ")[0].split("//", 1)[-1]
            package, _, name = target.partition(":")
            if not package.startswith("crates/"):
                continue
            crate = package.removeprefix("crates/")
            if crate in TEST_CRATES:
                failures.append(f"{product} links {crate}, which exists for tests ({target})")
            if on := TEST_FEATURES & set(attributes.get("features", [])):
                failures.append(f"{product} builds {crate} with test features {sorted(on)}")
    if failures:
        print("Test code reaches what ships:")
        for failure in sorted(set(failures)):
            print(f"  - {failure}")
        return 1
    print(f"nothing built for tests reaches {' or '.join(PRODUCTS)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
