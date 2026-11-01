#!/usr/bin/env python3
"""Build Release and inspect the iPhone app's shipped scope."""

from pathlib import Path
import json
import plistlib
import re
import struct
import subprocess
import tomllib

import ios_project

OUTPUT = Path("target/ios/scope-audit")
DERIVED = Path("target/ios/DerivedData")
APP = DERIVED / "Build/Products/Release-iphonesimulator/Amux.app"
# Reporting a problem ships: the report store, its bundle, its screens, the
# freeze that photographs the window and the dump the report carries are in
# every build. What stays out is what only driving needs: the door and the
# library built with the driving tools.
DEBUG_SYMBOLS = (
    "DoorServer", "DoorHost", "DoorCapture", "DoorFrames",
    "DrivenRoot", "VisibleTree", "AmuxTestSupport",
    "DebugReports", "PerfRun", "Workloads", "BudgetTable", "+debug-tools",
)
# What a person reports a problem with, which a Release build must carry: the
# two entry points, the report screen and the dump the report carries.
REPORT_SYMBOLS = ("ReportStore", "ReportScreen", "ReportFreeze", "amux_profile_dump")
REPORT_COPY = ("Report a Problem", "What went wrong?")
FORBIDDEN_APIS = (
    # The app browses the local network through Network.framework and nothing
    # else. The two legacy Bonjour entry points stay refused: they reach the
    # same multicast without the system's own permission prompt in front of
    # them, so code that used one would be browsing where nobody was asked.
    # Features are not listed here: what the app offers is settled in review,
    # and a list of features kept out goes stale as the app grows.
    "DNSServiceBrowse", "NSNetServiceBrowser",
)
# The one service this app may look for, and the sentence the system shows
# when it asks. A declaration for anything else is scope nobody agreed to.
BONJOUR_SERVICES = ["_amux._udp"]
LOCAL_NETWORK_PURPOSE = ("amux finds hosts on your network so this phone can "
                         "pair and connect to them directly.")
# Resources that only the driving and capture tools need. Code that reads them
# is caught by the symbol list above, but a resource can be carried into the
# bundle on its own — a stray copy phase, or a package whose test-support
# target became a dependency of the app — and then it ships as dead weight
# that says the app was built with the door open.
FORBIDDEN_RESOURCES = ("frozen-frame.png", "AmuxTestSupport")


def run(argv, *, timeout=120, check=True):
    result = subprocess.run(argv, check=False, capture_output=True, timeout=timeout)
    if check and result.returncode:
        raise RuntimeError(f"{' '.join(map(str, argv))} failed:\n"
                           + result.stderr.decode(errors="replace"))
    return result


def binary_violations(symbols: str, strings: str) -> list[str]:
    text = symbols + "\n" + strings
    return [f"excluded symbol or API: {name}"
            for name in (*DEBUG_SYMBOLS, *FORBIDDEN_APIS) if name in text]


def reporting_violations(symbols: str, strings: str) -> list[str]:
    """Refuse a bundle somebody could not report a problem from.

    Reporting is for everybody, so its absence is as much a scope failure as a
    door left in: a build that lost the report screen, the freeze or the Help
    row would still launch and look complete."""
    failures = [f"reporting is absent: {name}" for name in REPORT_SYMBOLS if name not in symbols]
    failures += [f"reporting copy is absent: {copy}" for copy in REPORT_COPY if copy not in strings]
    return failures


def png_size(path: Path) -> tuple[int, int]:
    """A PNG's pixel width and height, read out of its IHDR chunk.

    The chunks are walked rather than read from a fixed offset, because the
    PNGs Apple's packaging writes into a device bundle carry a proprietary
    `CgBI` chunk ahead of IHDR. An image library would be a dependency this
    repository does not otherwise need for two integers."""
    raw = path.read_bytes()
    if raw[:8] != b"\x89PNG\r\n\x1a\n":
        raise AssertionError(f"{path} is not a PNG")
    at = 8
    while at + 8 <= len(raw):
        length, kind = struct.unpack(">I4s", raw[at:at + 8])
        if kind == b"IHDR":
            return struct.unpack(">II", raw[at + 8:at + 16])
        at += 12 + length
    raise AssertionError(f"{path} has no IHDR chunk")


def icon_violations(info: dict, bundle: Path) -> list[str]:
    """Refuse a bundle the App Store would reject for having no icon.

    Apple rejects an upload with code 90713 unless `CFBundleIconName` sits at
    the top level of Info.plist — the catalog compiler writes its own copy
    nested inside `CFBundleIcons`, which is not the one read — and with code
    90022 unless the bundle carries an iPhone app icon of exactly 120x120.
    Neither shows up in a simulator run: an app with no icon launches
    perfectly well. So the built bundle is inspected here, where a release
    stops on it instead of a validation server doing so."""
    failures = []
    if not info.get("CFBundleIconName"):
        failures.append("no top-level CFBundleIconName in Info.plist")
    iphone_icon = bundle / "AppIcon60x60@2x.png"
    if not iphone_icon.is_file():
        failures.append("no 120x120 iPhone app icon: AppIcon60x60@2x.png is absent")
    elif png_size(iphone_icon) != (120, 120):
        failures.append("iPhone app icon is "
                        f"{'x'.join(map(str, png_size(iphone_icon)))}, not 120x120")
    return failures


def resource_violations(bundle: Path) -> list[str]:
    """Refuse a bundle carrying a file only the debug tools have a use for."""
    failures = []
    for path in sorted(bundle.rglob("*")):
        name = path.name
        for excluded in FORBIDDEN_RESOURCES:
            if name == excluded or excluded in name:
                failures.append("excluded resource in the bundle: "
                                f"{path.relative_to(bundle)}")
                break
    return failures


def bundle_violations(info: dict, entitlements: dict, settings: dict,
                      bundle: Path) -> list[str]:
    failures = icon_violations(info, bundle) + resource_violations(bundle)
    if "aps-environment" in entitlements:
        failures.append("push entitlement: aps-environment")
    # Required as well as bounded. Without the declaration iOS answers the
    # app's browser with nothing, and a phone on the same network as a machine
    # reports finding none — which looks like an empty network, not a bundle
    # built wrong, so nothing downstream would catch it.
    if info.get("NSBonjourServices") != BONJOUR_SERVICES:
        failures.append("Bonjour must be declared for exactly "
                        f"{BONJOUR_SERVICES}, not {info.get('NSBonjourServices')}")
    if info.get("NSLocalNetworkUsageDescription") != LOCAL_NETWORK_PURPOSE:
        failures.append("the local network is browsed without the agreed "
                        "explanation in Info.plist")
    if info.get("UIDeviceFamily") != [1]:
        failures.append("bundle device family must be iPhone only")
    if settings.get("TARGETED_DEVICE_FAMILY") != "1":
        failures.append("build device family must be iPhone only")
    for flag in ("SUPPORTS_MACCATALYST", "SUPPORTS_MAC_DESIGNED_FOR_IPHONE_IPAD",
                 "SUPPORTS_XR_DESIGNED_FOR_IPHONE_IPAD"):
        if settings.get(flag) != "NO":
            failures.append(f"destination not disabled: {flag}")
    if set(settings.get("SUPPORTED_PLATFORMS", "").split()) - {"iphoneos", "iphonesimulator"}:
        failures.append("non-iPhone platform in supported destinations")
    return failures


def graph_violations(graphs: list[dict]) -> list[str]:
    failures = []
    def visit(node):
        for key in ("name", "identity", "url", "path"):
            if re.search(r"amuxcloud|react[-_ ]?native", str(node.get(key, "")), re.I):
                failures.append(f"excluded package dependency: {node.get(key)}")
        for dependency in node.get("dependencies", []):
            visit(dependency)
    for graph in graphs:
        visit(graph)
    return failures


def inspect_binary(binary: Path) -> tuple[str, str]:
    symbols = run(["xcrun", "nm", "-a", str(binary)]).stdout.decode(errors="replace")
    strings = run(["strings", "-a", str(binary)]).stdout.decode(errors="replace")
    return symbols, strings


def entitlements() -> dict:
    result = run(["codesign", "-d", "--entitlements", ":-", str(APP)], check=False)
    if result.returncode:
        if b"not signed at all" in result.stderr:
            return {}
        raise RuntimeError(result.stderr.decode(errors="replace"))
    return plistlib.loads(result.stdout) if result.stdout.strip() else {}


def detector_probe() -> None:
    """Compile a test executable with a debug export and require rejection.

    This exercises the same Mach-O inspection used on the app, rather than
    asserting that a search through a synthetic symbol listing works.
    """
    source = OUTPUT / "excluded-symbol.c"
    binary = OUTPUT / "excluded-symbol"
    source.write_text("void DoorServer(void) {}\n"
                      "int main(void) { DoorServer(); return 0; }\n")
    sdk = run(["xcrun", "--sdk", "iphonesimulator", "--show-sdk-path"]).stdout.decode().strip()
    run(["xcrun", "clang", "-target", "arm64-apple-ios26.0-simulator", "-isysroot", sdk,
         str(source), "-o", str(binary)])
    failures = binary_violations(*inspect_binary(binary))
    if "excluded symbol or API: DoorServer" not in failures:
        raise RuntimeError("audit accepted a test build containing a debug-only export")
    (OUTPUT / "detector-probe.txt").write_text("Rejected compiler-built probe:\n" + "\n".join(failures) + "\n")


def main() -> None:
    OUTPUT.mkdir(parents=True, exist_ok=True)
    report = OUTPUT / "audit.txt"
    report.write_text("Release scope audit started; no verdict yet.\n")
    ios_project.generate()
    args = ["xcodebuild", "-project", "apps/apple/Amux.xcodeproj", "-scheme", "Amux",
            "-configuration", "Release", "-destination", "generic/platform=iOS Simulator",
            "-derivedDataPath", str(DERIVED), "ARCHS=arm64", "ONLY_ACTIVE_ARCH=YES"]
    built = run([*args, "build", "-quiet"], timeout=900)
    (OUTPUT / "build.txt").write_bytes(built.stdout + built.stderr)
    settings_rows = json.loads(run([*args, "-showBuildSettings", "-json"]).stdout)
    settings = next(row["buildSettings"] for row in settings_rows if row["target"] == "Amux")
    (OUTPUT / "build-settings.json").write_text(json.dumps(settings, indent=2))
    info = plistlib.loads((APP / "Info.plist").read_bytes())
    grants = entitlements()
    # Simulator builds may be unsigned. Inspect the Release signing input too,
    # so a push entitlement cannot hide behind disabled simulator signing.
    declared = settings.get("CODE_SIGN_ENTITLEMENTS", "")
    if declared:
        path = Path(declared)
        if not path.is_absolute():
            path = Path(settings["SRCROOT"]) / path
        grants |= plistlib.loads(path.read_bytes())
    (OUTPUT / "entitlements.json").write_text(json.dumps(grants, indent=2))
    failures = bundle_violations(info, grants, settings, APP)
    symbols, strings = inspect_binary(APP / info["CFBundleExecutable"])
    (OUTPUT / "symbols.txt").write_text(symbols)
    # Include compiled localization resources: a row may no longer be a literal
    # in the executable after it moves into the string catalogue.
    for resource in APP.rglob("*.strings"):
        table = plistlib.loads(resource.read_bytes())
        strings += "\n" + "\n".join(str(value) for value in table.values())
    (OUTPUT / "strings.txt").write_text(strings)
    failures += binary_violations(symbols, strings)
    if "Contact Support" not in strings:
        failures.append("Contact Support is absent")
    failures += reporting_violations(symbols, strings)
    if not any(copy in strings for copy in ("need you", "needs you")):
        failures.append("in-app attention copy is absent")
    graphs = []
    for package in sorted(Path("apps/apple/Packages").glob("*/Package.swift")):
        graphs.append(json.loads(run([
            "swift", "package", "--package-path", str(package.parent),
            "--scratch-path", str((OUTPUT / "SwiftPM" / package.parent.name).resolve()),
            "show-dependencies", "--format", "json"]).stdout))
    rust_packages = tomllib.loads(Path("Cargo.lock").read_text())["package"]
    rust_graph = [{"name": package["name"], "url": package.get("source", ""),
                   "dependencies": [{"name": dependency} for dependency in package.get("dependencies", [])]}
                  for package in rust_packages]
    (OUTPUT / "package-graph.json").write_text(json.dumps(
        {"swift": graphs, "rust": rust_graph}, indent=2))
    failures += graph_violations(graphs + rust_graph)
    detector_probe()
    lines = [f"Release app: {APP}",
             "Inspected: Info.plist, the app icon, bundle resources, entitlements, build destinations, executable symbols,",
             "compiled strings and the complete Swift package and locked Rust dependency graphs.",
             "Detector rejected a compiler-built test executable carrying a debug export."]
    if failures:
        lines += ["FAIL: " + failure for failure in failures]
    else:
        lines += [f"PASS: icon {info['CFBundleIconName']} named at the top level of Info.plist "
                  f"and a {'x'.join(map(str, png_size(APP / 'AppIcon60x60@2x.png')))} iPhone icon in the bundle.",
                  "PASS: no frozen-frame or test-support resource in the bundle.",
                  f"PASS: Bonjour limited to {' '.join(BONJOUR_SERVICES)} behind the agreed explanation "
                  "and no legacy browser; iPhone destinations only;",
                  "no amuxcloud or React Native package; no driving code.",
                  "PASS: in-app attention copy, Contact Support and Report a Problem remain;",
                  "the report screen, its freeze and the record snapshot ship."]
    lines += ["Limit: simulator Release bundle inspection; distribution signing is checked before release."]
    report.write_text("\n".join(lines) + "\n")
    print(report.read_text(), end="", flush=True)
    if failures:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
