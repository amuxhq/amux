#!/usr/bin/env python3
"""Write third-party/android/BUCK from the Android app's version catalog.

apps/android/libs.versions.toml is the only place the app's library
versions are edited. Its `[bundles]` say what to resolve: `app`, what the
app ships; `test`, what its tests and debug builds add; and `tools`, the
build's own (the Kotlin compiler plugins, R8). This resolves them with
Coursier against Google's Maven repository and Maven Central, taking every
transitive dependency at the newest version anything asks for, as Gradle
does. The app's set is resolved first and its versions are forced on the
other two, so adding a test library never changes what ships.

Each resolved artifact becomes a `remote_file` pinned by SHA-256 and a
`prebuilt_jar` or `android_prebuilt_aar` depending on what it depends on;
each catalog entry becomes an alias under its catalog name
(`//third-party/android:androidx-compose-ui`). Two rules follow what
Gradle's module metadata would choose, which Coursier reads no part of:

- A Kotlin Multiplatform library's root module (`androidx.compose.ui:ui`)
  publishes, beside its Android or JVM module (`ui-android`), an artifact
  of common metadata, which Gradle never puts on an Android classpath. Its
  Gradle metadata names the platform module, which the root's POM may not
  depend on (okhttp's doesn't). The root becomes a library of no code that
  exports the platform module, resolved with everything else. Another
  platform's module of the same library, which a POM may name in its
  place, stands for that one.
- A library's modules are kept at one version (activity and
  activity-ktx) by constraints in their Gradle metadata, which raise any
  of them the resolution has to the version another asks for.
- `kotlin-stdlib` is the one the Kotlin toolchain compiles against: the
  prelude's kotlincd puts its own standard library on every compile's
  classpath (but in no APK, so the app's is this one), so the catalog's
  Kotlin version must be the prelude's and the resolved one is forced to it.

Coursier runs from PATH (`cs`), or from the flake's pinned nixpkgs through
`nix shell` on a machine with Nix. Run it after editing the catalog, and
commit what it writes.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
import urllib.request
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
CATALOG = ROOT / "apps/android/libs.versions.toml"
OUTPUT = ROOT / "third-party/android/BUCK"

REPOSITORIES = {
    "https://maven.google.com/": "google",
    "https://repo1.maven.org/maven2/": "central",
}

# The standard library, at the Kotlin toolchain's version.
KOTLIN_STDLIB = "org.jetbrains.kotlin:kotlin-stdlib"

# A KMP root module's platform modules, in the order an Android build
# prefers them.
PLATFORM_SUFFIXES = ("-android", "-jvm")


def load_catalog(path: Path) -> dict:
    with path.open("rb") as f:
        return tomllib.load(f)


def library_coordinates(catalog: dict, alias: str) -> tuple[str, str | None]:
    """`group:name` and its version (None where a BOM gives it)."""
    entry = catalog["libraries"][alias]
    if isinstance(entry, str):
        group, name, version = entry.split(":")
        return f"{group}:{name}", version
    module = entry.get("module") or f"{entry['group']}:{entry['name']}"
    version = entry.get("version")
    if isinstance(version, dict):
        version = catalog["versions"][version["ref"]]
    return module, version


def bom_versions(module: str, version: str) -> dict[str, str]:
    """The versions a BOM (`platform(...)` in Gradle) manages."""
    group, name = module.split(":")
    rel = f"{group.replace('.', '/')}/{name}/{version}/{name}-{version}.pom"
    for base in REPOSITORIES:
        try:
            with urllib.request.urlopen(base + rel, timeout=60) as response:
                pom = ET.fromstring(response.read())
            break
        except OSError:
            continue
    else:
        raise SystemExit(f"android-deps: no repository has the BOM {module}:{version}")
    ns = {"m": "http://maven.apache.org/POM/4.0.0"}
    managed = {}
    for dep in pom.findall("m:dependencyManagement/m:dependencies/m:dependency", ns):
        g = dep.findtext("m:groupId", namespaces=ns)
        a = dep.findtext("m:artifactId", namespaces=ns)
        v = dep.findtext("m:version", namespaces=ns)
        if g and a and v:
            managed[f"{g}:{a}"] = v
    return managed


def resolved_sets(catalog: dict) -> dict[str, list[str]]:
    """Each bundle's `group:name:version` coordinates, with the BOMs'
    versions filled in."""
    boms = {}
    for alias in catalog["libraries"]:
        module, version = library_coordinates(catalog, alias)
        if module.endswith("-bom") and version:
            boms.update(bom_versions(module, version))
    sets = {}
    for bundle, aliases in catalog["bundles"].items():
        coords = []
        for alias in aliases:
            module, version = library_coordinates(catalog, alias)
            version = version or boms.get(module)
            if not version:
                raise SystemExit(f"android-deps: no version for {alias} ({module})")
            coords.append(f"{module}:{version}")
        sets[bundle] = coords
    return sets


def coursier() -> list[str]:
    for name in ("cs", "coursier"):
        found = shutil.which(name)
        if found:
            return [found]
    if shutil.which("nix"):
        lock = json.loads((ROOT / "flake.lock").read_text())
        rev = lock["nodes"]["nixpkgs"]["locked"]["rev"]
        return [
            "nix",
            "--extra-experimental-features",
            "nix-command flakes",
            "shell",
            f"github:NixOS/nixpkgs/{rev}#coursier",
            "-c",
            "cs",
        ]
    raise SystemExit(
        "android-deps: needs Coursier (`cs`) on PATH, or Nix to run the flake's;"
        " run it on a Linux machine with Nix"
    )


def fetch(coords: list[str], forced: dict[str, str]) -> list[dict]:
    """Coursier's resolution of `coords`, with `forced` versions."""
    with tempfile.TemporaryDirectory() as tmp:
        out = Path(tmp) / "fetch.json"
        command = coursier() + ["fetch", "--quiet"]
        for repo in REPOSITORIES.values():
            command += ["-r", repo]
        # AndroidX pins its own modules to exact versions of each other
        # (`[1.4.2]`), which Gradle's module metadata lines up and a POM
        # resolver sees as a conflict: take the newest, as Gradle ends up.
        command += ["--reconciliation", "*:*:relaxed", "-A", "jar,aar,bundle"]
        for module, version in sorted(forced.items()):
            command += ["--force-version", f"{module}:{version}"]
        command += ["--json-output-file", str(out)] + coords
        subprocess.run(command, check=True, stdout=subprocess.DEVNULL, timeout=1800)
        return json.loads(out.read_text())["dependencies"]


def split(coord: str) -> tuple[str, str]:
    """`group:name` and version of a Coursier coordinate
    (`g:a:v`, `g:a:type:v` or `g:a:type:classifier:v`)."""
    parts = coord.split(":")
    return f"{parts[0]}:{parts[1]}", parts[-1]


def gradle_metadata(module: str, version: str, url: str | None) -> tuple[str | None, dict[str, str]]:
    """What a module's Gradle metadata says to an Android build: the module
    a Kotlin Multiplatform root sends it to (`okhttp` to `okhttp-android`),
    or None, and the versions it constrains other modules to. A root's POM
    may not depend on its platform module at all, so a POM resolution can
    miss the code; and no POM carries the constraints."""
    group, name = module.split(":")
    candidates = [url.rsplit(".", 1)[0] + ".module"] if url else [
        f"{base}{group.replace('.', '/')}/{name}/{version}/{name}-{version}.module" for base in REPOSITORIES
    ]
    for candidate in candidates:
        try:
            with urllib.request.urlopen(candidate, timeout=60) as response:
                metadata = json.loads(response.read())
            break
        except (OSError, ValueError):
            continue
    else:
        return None, {}
    # AndroidX keeps a library's modules at one version (activity and
    # activity-ktx) by constraints in every runtime variant, which Gradle
    # applies to whichever of them the build has.
    constraints: dict[str, str] = {}
    for variant in metadata.get("variants", []):
        if variant.get("attributes", {}).get("org.gradle.usage") != "java-runtime":
            continue
        for constraint in variant.get("dependencyConstraints", []):
            wanted = constraint.get("version", {})
            wanted = wanted.get("strictly") or wanted.get("requires")
            if wanted:
                key = f"{constraint['group']}:{constraint['module']}"
                if key not in constraints or newer(wanted, constraints[key]):
                    constraints[key] = wanted
    # The runtime library variant an Android build matches: one for an
    # Android JVM (AndroidX says so by its JVM environment, Kotlin libraries
    # by their platform type) before one for any JVM.
    best, best_rank = None, 0
    for variant in metadata.get("variants", []):
        attributes = variant.get("attributes", {})
        target = variant.get("available-at")
        if not target or attributes.get("org.gradle.usage") != "java-runtime":
            continue
        if attributes.get("org.gradle.category", "library") != "library":
            continue
        environment = attributes.get("org.gradle.jvm.environment")
        platform = attributes.get("org.jetbrains.kotlin.platform.type")
        if environment == "android" or platform == "androidJvm":
            rank = 2
        elif platform == "jvm" and environment in (None, "standard-jvm") and "stubs" not in target["module"]:
            rank = 1
        else:
            continue
        if rank > best_rank:
            best, best_rank = f"{target['group']}:{target['module']}:{target['version']}", rank
    return best, constraints


def newer(a: str, b: str) -> bool:
    """Whether version `a` is after `b` (`1.11.0` after `1.7.0`; a release
    after its own prereleases)."""

    def key(version: str) -> tuple:
        release, _, pre = version.partition("-")
        numbers = tuple(int(n) for n in re.findall(r"\d+", release))
        return numbers, (1,) if not pre else (0, pre)

    return key(a) > key(b)


def url_of(file: str) -> str:
    # Coursier's cache keeps each file under its URL (…/v1/https/host/path).
    match = re.search(r"/v1/(https?)/(.+)$", file)
    if not match:
        raise SystemExit(f"android-deps: can't tell where {file} came from")
    return f"{match.group(1)}://{match.group(2)}"


def target_name(module: str) -> str:
    return module.replace(":", "_")


def write_buck(catalog: dict, artifacts: dict[str, dict]) -> str:
    modules = set(artifacts)
    lines = [
        "# @generated by scripts/android-deps.py from",
        "# apps/android/libs.versions.toml; edit the catalog and run it",
        "# again rather than editing this file.",
        "",
    ]

    def dep_label(module: str) -> str:
        return ":" + target_name(module)

    for module in sorted(modules):
        info = artifacts[module]
        deps = sorted({dep_label(d) for d in info["deps"] if d in modules})
        platform = info.get("platform") or next((module + s for s in PLATFORM_SUFFIXES if module + s in info["deps"]), None)
        name = target_name(module)
        lines.append(f"# {module}:{info['version']}")
        if platform or not info["file"]:
            # A KMP root module (or one with no artifact of its own): only
            # what it depends on.
            lines += [
                "java_library(",
                f'    name = "{name}",',
                "    exported_deps = [",
                *[f'        "{d}",' for d in deps],
                "    ],",
                '    visibility = ["PUBLIC"],',
                ")",
                "",
            ]
            continue
        filename = info["url"].rsplit("/", 1)[1]
        lines += [
            "remote_file(",
            f'    name = "{name}-file",',
            f'    url = "{info["url"]}",',
            f'    sha256 = "{info["sha256"]}",',
            # With its size Buck2 knows the file's digest in the cache, so a
            # build fetches it from Maven only when the cache lacks it.
            f'    size_bytes = {info["size"]},',
            f'    out = "{filename}",',
            '    visibility = ["PUBLIC"],',
            ")",
            "",
        ]
        if filename.endswith(".aar"):
            lines += ["android_prebuilt_aar(", f'    name = "{name}",', f'    aar = ":{name}-file",']
        else:
            lines += ["prebuilt_jar(", f'    name = "{name}",', f'    binary_jar = ":{name}-file",']
        lines += [f'    maven_coords = "{module}:{info["version"]}",']
        if deps:
            lines += ["    deps = [", *[f'        "{d}",' for d in deps], "    ],"]
        lines += ['    visibility = ["PUBLIC"],', ")", ""]

    lines += ["# The catalog's own names (apps/android/libs.versions.toml).", ""]
    for alias in sorted(catalog["libraries"]):
        module, _ = library_coordinates(catalog, alias)
        if module not in modules or module.endswith("-bom"):
            continue
        lines += [
            "alias(",
            f'    name = "{alias}",',
            f'    actual = "{dep_label(module)}",',
            '    visibility = ["PUBLIC"],',
            ")",
            "",
        ]
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.parse_args(argv)
    catalog = load_catalog(CATALOG)
    kotlin = catalog["versions"]["kotlin"]
    sets = resolved_sets(catalog)

    forced = {KOTLIN_STDLIB: kotlin}
    app = fetch(sets["app"], forced)
    for entry in app:
        module, version = split(entry["coord"])
        forced.setdefault(module, version)
    # Resolve, then apply what Gradle's metadata says and Coursier doesn't
    # read: follow each multiplatform root to its Android or JVM module, and
    # raise each module a constraint names to the version it asks for,
    # until the resolution is one the metadata agrees with.
    requested = sets["app"] + sets["test"] + sets["tools"]
    known: dict[tuple[str, str], tuple[str | None, dict[str, str]]] = {}
    while True:
        everything = fetch(requested, forced)
        versions = dict(split(entry["coord"]) for entry in everything)
        checks = {}
        for entry in everything:
            module, version = split(entry["coord"])
            file = entry.get("file")
            if (module, version) not in known:
                checks[(module, version)] = url_of(file) if file else None
        with ThreadPoolExecutor(max_workers=16) as pool:
            found = pool.map(lambda item: gradle_metadata(item[0][0], item[0][1], item[1]), checks.items())
            known.update(zip(checks, found))
        redirects = {module: known[(module, version)][0] for module, version in versions.items() if (module, version) in known}
        new = sorted({target for target in redirects.values() if target and split(target)[0] not in versions})
        raised = False
        # A root's platform module at the root's version, as its metadata
        # names it, however old a version some POM asks for.
        for target in redirects.values():
            if target:
                other, wanted = split(target)
                if other in versions and newer(wanted, versions[other]) and (other not in forced or newer(wanted, forced[other])):
                    forced[other] = wanted
                    raised = True
        for module, version in versions.items():
            for other, wanted in known.get((module, version), (None, {}))[1].items():
                if other in versions and other != KOTLIN_STDLIB and newer(wanted, versions[other]):
                    if other not in forced or newer(wanted, forced[other]):
                        forced[other] = wanted
                        raised = True
        if not new and not raised:
            break
        requested = requested + new

    artifacts: dict[str, dict] = {}
    for entry in everything:
        module, version = split(entry["coord"])
        file = entry.get("file")
        info = artifacts.setdefault(module, {"version": version, "file": None, "deps": set()})
        info["deps"].update(split(d)[0] for d in entry.get("directDependencies", []))
        if file and not info["file"]:
            info["file"] = file
            info["url"] = url_of(file)
            info["sha256"] = hashlib.sha256(Path(file).read_bytes()).hexdigest()
            info["size"] = Path(file).stat().st_size
    # A platform module's POM can name another platform's module of the
    # same library (datastore-core-okio-jvm names datastore-core-jvm) where
    # Gradle's metadata names the root, which an Android build takes to its
    # Android module: every module of a library stands for the one its root
    # chose, so the classes of one library are on the classpath once.
    chosen: dict[str, str] = {}
    for module, target in redirects.items():
        if target and module in artifacts and split(target)[0] in artifacts:
            platform = split(target)[0]
            artifacts[module]["platform"] = platform
            artifacts[module]["deps"].add(platform)
            for sibling in artifacts:
                if sibling != platform and sibling.startswith(module + "-") and sibling[len(module):] in ("-jvm", "-android"):
                    chosen[sibling] = platform
    for module in chosen:
        del artifacts[module]
    for info in artifacts.values():
        info["deps"] = {chosen.get(d, d) for d in info["deps"]}

    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_text(write_buck(catalog, artifacts))
    print(f"android-deps: wrote {len(artifacts)} artifacts to {OUTPUT.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
