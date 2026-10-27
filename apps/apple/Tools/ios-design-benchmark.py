#!/usr/bin/env python3
"""Measure source design rounds without changing the design checkout.

The original compiler, capture driver and harness run in an isolated copy.
Generated layout variants are benchmark inputs, not production components.
"""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import shlex
import subprocess
import tempfile
import time


ROOT = Path(__file__).resolve().parents[3]


def replace_once(text, old, new):
    if text.count(old) != 1:
        raise ValueError(f"expected one source anchor: {old[:80]}")
    return text.replace(old, new, 1)


def catalog_round(original, count, gutter_offset=0):
    """Replace only the round's catalogue; keep shot/index generation intact."""
    options = ",\n".join(
        f'        Variant(id: "{i}", name: "Layout {i + 1}", '
        f'blurb: "Gutter {14 + i * 4 + gutter_offset} pt; title {24 + i * 2} pt")'
        for i in range(count)
    )
    start = original.index("    static let variants:")
    end = original.index("    static var screens:", start)
    return original[:start] + f'''    static let variants: [Variant] = [
{options}
    ]
    static let groups: [Group] = [
        Group(id: "benchmark", name: "Layout ideas", screens: [
            Screen(id: "home", name: "Agents", note: "Benchmark only",
                   size: nil, options: variants)
        ])
    ]

''' + original[end:]


def run(command, cwd, log):
    started = time.monotonic()
    with log.open("w") as output:
        result = subprocess.run(command, cwd=cwd, stdout=output,
                                stderr=subprocess.STDOUT, timeout=300)
    if result.returncode:
        raise RuntimeError(f"command failed ({result.returncode}); see {log}")
    return time.monotonic() - started


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("--simulator", default="amux-design")
    parser.add_argument("--rounds", type=int, default=2)
    args = parser.parse_args()
    if not 1 <= args.rounds <= 5:
        parser.error("--rounds must be between 1 and 5")
    source = args.source.resolve()
    for required in ("ios/make.sh", "ios/Sources/App/Catalog.swift",
                     "design/fixtures.json", "design/harness/check.mjs"):
        if not (source / required).is_file():
            parser.error(f"missing design input: {required}")
    base = ROOT / "target/ios/design-benchmarks"
    base.mkdir(parents=True, exist_ok=True)
    out = Path(tempfile.mkdtemp(prefix="round-", dir=base))
    checkout = out / "source"
    shutil.copytree(source / "ios", checkout / "ios",
                    ignore=shutil.ignore_patterns(".build", ".DS_Store"))
    shutil.copytree(source / "design", checkout / "design",
                    ignore=shutil.ignore_patterns("captures", "rounds", "decisions.json"))
    # Isolate the installed app and retain the source simulator/device. The
    # source make script deletes only this benchmark app's capture directory.
    make = checkout / "ios/make.sh"
    build_script = replace_once(make.read_text(), 'BUNDLE_ID="sh.amux.design"',
                                'BUNDLE_ID="sh.amux.design.benchmark"')
    build_script = replace_once(build_script, 'SIM_NAME="amux-design"',
                                f'SIM_NAME={shlex.quote(args.simulator)}')
    # The source pipeline masks compiler failure and can reuse a stale binary.
    # Fail closed in this measuring copy; do not change its compiler flags.
    build_script = replace_once(build_script,
                                "$sources 2>&1 | grep -v 'incompatible-sysroot' || true",
                                "$sources 2>&1")
    make.write_text(build_script)
    catalog = checkout / "ios/Sources/App/Catalog.swift"
    main_source = checkout / "ios/Sources/App/Main.swift"
    original_catalog, original_main = catalog.read_text(), main_source.read_text()
    records = []
    metadata = {
        "source": str(source),
        "revision": subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=source, text=True).strip(),
        "xcode": subprocess.check_output(["xcodebuild", "-version"], text=True).strip(),
        "simulators": json.loads(subprocess.check_output(
            ["xcrun", "simctl", "list", "devices", "available", "-j"], text=True)),
        "source_hashes": {
            str(path.relative_to(source)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted((source / "ios/Sources").rglob("*.swift"))
        },
        "limits": ["Excludes human idea-authoring time and browser paint latency",
                   "Uses source window renderer, not golden-qualified display capture",
                   "Layout variants vary gutter and title size, not whole screen structure",
                   "Simulator boot is included if needed; SDK caches are not cleared"],
        "runs": records,
    }
    print(f"Benchmark output: {out}", flush=True)
    for count in (0, 2, 4):
        for repeat in range(args.rounds):
            label = f"{'catalogue' if count == 0 else str(count) + '-ideas'}-{repeat}"
            record_dir = out / label
            record_dir.mkdir()
            began = time.monotonic()
            if count:
                catalog.write_text(catalog_round(original_catalog, count, repeat))
                # A real source change on each repeat, compiled once for all
                # ideas. No timing is inferred by multiplying one-idea time.
                main_source.write_text(replace_once(original_main,
                    '    let variant: String\n', f'''    let variant: String

    private var benchmarkDesign: Design {{
        var value = Design.app
        let idea = Int(variant) ?? 0
        value.metrics.gutter = CGFloat(14 + idea * 4 + {repeat})
        value.type.titleSize = CGFloat(24 + idea * 2)
        return value
    }}
''').replace('.environment(\\.design, .app)',
                            '.environment(\\.design, benchmarkDesign)'))
            else:
                catalog.write_text(original_catalog)
                main_source.write_text(original_main)
            preparation = time.monotonic() - began
            seconds = run(["bash", str(make), "shots"], checkout,
                          record_dir / "capture.log")
            harness_seconds = run(["node", "design/harness/check.mjs"], checkout,
                                  record_dir / "harness.log")
            captures = checkout / "design/captures"
            index = json.loads((captures / "index.json").read_text())
            expected = sum(len(screen["options"]) * len(index["themes"])
                           for group in index["groups"] for screen in group["screens"])
            if len(index["captures"]) != expected:
                raise RuntimeError(f"incomplete capture: {len(index['captures'])}/{expected}")
            hashes = {name: hashlib.sha256((captures / name).read_bytes()).hexdigest()
                      for name in index["captures"]}
            if count and len({hashes[f"home.{i}.light.png"] for i in range(count)}) != count:
                raise RuntimeError("layout ideas produced duplicate images")
            shutil.copytree(captures, record_dir / "captures")
            records.append({"label": label, "ideas": count or None,
                            "images": expected, "source_preparation_seconds": preparation,
                            "build_to_harness_files_seconds": seconds,
                            "harness_check_seconds": harness_seconds,
                            "image_hashes": hashes})
            (out / "results.json").write_text(json.dumps(metadata, indent=2) + "\n")
            print(f"{label}: {expected} images, {seconds:.2f}s + "
                  f"{harness_seconds:.2f}s harness validation", flush=True)
    print(f"Original harness with final benchmark images: {checkout / 'design/index.html'}")
    print("Serve that design directory locally to inspect the existing harness.")


if __name__ == "__main__":
    main()
