#!/usr/bin/env python3
"""Inspect the iPhone app's shipped scope.

    ios-scope-audit.py [--app PATH]

With no app named, Buck2 builds the shipping app for the simulator and that
is audited (`just ios scope-audit`); a release names the app it has just
signed for the App Store, so what is audited is what is uploaded.
"""

from pathlib import Path
import argparse
import json
import plistlib
import re
import struct
import subprocess
import tempfile
import tomllib

OUTPUT = Path("target/ios/scope-audit")
TARGET = "//apps/apple:Amux"
SIMULATOR_PLATFORM = "//build/platforms:ios-sim-app-shipping-release"
DEVICE_PLATFORM = "//build/platforms:ios-app-shipping-release"
# The platforms a binary of this app may be built for, as `vtool` names them
# from its LC_BUILD_VERSION: a phone and its simulator. Mac Catalyst, visionOS
# and the rest are other platforms, so a binary built for any of them names
# it here, whatever the bundle's Info.plist claims.
BUILD_PLATFORMS = {"IOS", "IOSSIMULATOR"}
SUPPORTED_PLATFORMS = {"iPhoneOS", "iPhoneSimulator"}
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
# The domains whose links open the app: amux.sh's pairing page, which the QR
# code a host shows is. Without the entitlement the system opens the page in
# the browser instead, and a phone with the app installed is sent to the App
# Store for an app it already has.
ASSOCIATED_DOMAINS = ["applinks:amux.sh"]
BONJOUR_SERVICES = ["_amux._udp"]
# The extension that opens a push. iOS gives it a few megabytes, which the Rust
# bridge alone would overrun, so nothing of the bridge may reach it.
EXTENSION = "PlugIns/AmuxNotifications.appex"
EXTENSION_POINT = "com.apple.usernotifications.service"
BRIDGE_SYMBOL = "amux_runtime_"
# Where the app keeps push keys and the extension reads them. Named as the
# entitlements files spell it, since the team's prefix is not committed; a
# signed bundle carries it with the prefix filled in.
PUSH_KEY_GROUP = "$(AppIdentifierPrefix)sh.amux.push"
PUSH_KEY_SUFFIX = ".sh.amux.push"
LOCAL_NETWORK_PURPOSE = ("amux finds computers on your network so this phone can "
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


def holds_push_group(grants: dict) -> bool:
    return any(group == PUSH_KEY_GROUP or group.endswith(PUSH_KEY_SUFFIX)
               for group in grants.get("keychain-access-groups", []))


def bundle_violations(info: dict, entitlements: dict, platforms: set[str],
                      bundle: Path) -> list[str]:
    """`platforms` are the ones the executable was built for (BUILD_PLATFORMS)."""
    failures = icon_violations(info, bundle) + resource_violations(bundle)
    # Required: without it the app asks for a token and never gets one, and
    # nothing but the notifications that don't arrive says so.
    if "aps-environment" not in entitlements:
        failures.append("no push entitlement: aps-environment")
    domains = entitlements.get("com.apple.developer.associated-domains")
    if domains != ASSOCIATED_DOMAINS:
        failures.append(f"associated domains must be {ASSOCIATED_DOMAINS}, not {domains}")
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
    if not platforms:
        failures.append("the executable names no platform it was built for")
    elif platforms - BUILD_PLATFORMS:
        failures.append(f"executable built for {' '.join(sorted(platforms - BUILD_PLATFORMS))}, "
                        "not only an iPhone")
    if set(info.get("CFBundleSupportedPlatforms", [])) - SUPPORTED_PLATFORMS:
        failures.append("non-iPhone platform in supported destinations: "
                        f"{info.get('CFBundleSupportedPlatforms')}")
    return failures


def extension_violations(app_info: dict, app_grants: dict, info: dict, grants: dict,
                         symbols: str) -> list[str]:
    """Refuse a notification extension that would launch and do nothing.

    Every one of these still builds and installs: the app's pushes would just
    show the plain alert, or the App Store would turn the upload down."""
    failures = []
    if info.get("NSExtension", {}).get("NSExtensionPointIdentifier") != EXTENSION_POINT:
        failures.append(f"the extension does not declare {EXTENSION_POINT}")
    for key in ("CFBundleShortVersionString", "CFBundleVersion"):
        if info.get(key) != app_info.get(key):
            failures.append(f"the extension's {key} is {info.get(key)}, "
                            f"not the app's {app_info.get(key)}")
    if BRIDGE_SYMBOL in symbols:
        failures.append("the extension links the Rust bridge")
    for name, held in (("app", app_grants), ("extension", grants)):
        if not holds_push_group(held):
            failures.append(f"the {name} lacks the push key group {PUSH_KEY_GROUP}")
    if info.get("AmuxPushKeychainGroup") != app_info.get("AmuxPushKeychainGroup"):
        failures.append("the app and the extension name different push key groups")
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


def platforms(binary: Path) -> set[str]:
    shown = run(["xcrun", "vtool", "-show-build", str(binary)]).stdout.decode(errors="replace")
    return set(re.findall(r"^\s*platform\s+(\S+)", shown, re.M))


def signed_entitlements(bundle: Path) -> dict:
    result = run(["codesign", "-d", "--entitlements", ":-", str(bundle)], check=False)
    if result.returncode:
        if b"not signed at all" in result.stderr:
            return {}
        raise RuntimeError(result.stderr.decode(errors="replace"))
    return plistlib.loads(result.stdout) if result.stdout.strip() else {}


def embedded_entitlements(binary: Path) -> dict:
    """The entitlements a simulator build carries in its `__TEXT,__entitlements`
    section, which is where the simulator reads them from: its ad hoc
    signature carries none (build/apple.bzl says why)."""
    with tempfile.TemporaryDirectory() as directory:
        out = Path(directory) / "entitlements.plist"
        found = run(["xcrun", "segedit", str(binary), "-extract", "__TEXT", "__entitlements",
                     str(out)], check=False)
        if found.returncode or not out.is_file() or not out.read_bytes().strip(b"\0"):
            return {}
        return plistlib.loads(out.read_bytes().rstrip(b"\0"))


def granted(bundle: Path, executable: str) -> dict:
    """What a bundle is entitled to: its signature's grants, or for a
    simulator build the ones its executable embeds."""
    return signed_entitlements(bundle) or embedded_entitlements(bundle / executable)


def built_app() -> Path:
    """The shipping app for the simulator, as Buck2 builds it."""
    built = run(["scripts/buck2", "build", TARGET, "--target-platforms", SIMULATOR_PLATFORM,
                 "--show-full-simple-output"], timeout=3600)
    return Path(built.stdout.decode().strip().splitlines()[-1])


def dependency_graph(platform: str) -> list[dict]:
    """Every target the app is built from, as Buck2 resolves it for the
    platform it was built for: where a cloud or React Native package would
    have to come in."""
    listed = run(["scripts/buck2", "cquery", f"deps({TARGET})", "--target-platforms", platform],
                 timeout=600)
    labels = {line.split(" (")[0].strip() for line in listed.stdout.decode().splitlines()}
    return [{"name": label, "dependencies": []} for label in sorted(labels) if label]


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
    parser = argparse.ArgumentParser()
    parser.add_argument("--app", help="the app to audit, instead of building the simulator's")
    arguments = parser.parse_args()
    OUTPUT.mkdir(parents=True, exist_ok=True)
    report = OUTPUT / "audit.txt"
    report.write_text("Release scope audit started; no verdict yet.\n")
    app = Path(arguments.app) if arguments.app else built_app()
    info = plistlib.loads((app / "Info.plist").read_bytes())
    executable = app / info["CFBundleExecutable"]
    grants = granted(app, info["CFBundleExecutable"])
    (OUTPUT / "entitlements.json").write_text(json.dumps(grants, indent=2))
    built_for = platforms(executable)
    failures = bundle_violations(info, grants, built_for, app)
    appex = app / EXTENSION
    if appex.is_dir():
        extension_info = plistlib.loads((appex / "Info.plist").read_bytes())
        failures += extension_violations(
            info, grants, extension_info, granted(appex, extension_info["CFBundleExecutable"]),
            inspect_binary(appex / extension_info["CFBundleExecutable"])[0])
    else:
        failures.append(f"no extension at {EXTENSION}")
    symbols, strings = inspect_binary(executable)
    (OUTPUT / "symbols.txt").write_text(symbols)
    # Include compiled localization resources: a row may no longer be a literal
    # in the executable after it moves into the string catalogue.
    for resource in app.rglob("*.strings"):
        table = plistlib.loads(resource.read_bytes())
        strings += "\n" + "\n".join(str(value) for value in table.values())
    (OUTPUT / "strings.txt").write_text(strings)
    failures += binary_violations(symbols, strings)
    if "Contact Support" not in strings:
        failures.append("Contact Support is absent")
    failures += reporting_violations(symbols, strings)
    if not any(copy in strings for copy in ("need you", "needs you")):
        failures.append("in-app attention copy is absent")
    graphs = dependency_graph(DEVICE_PLATFORM if "IOS" in built_for else SIMULATOR_PLATFORM)
    rust_packages = tomllib.loads(Path("Cargo.lock").read_text())["package"]
    rust_graph = [{"name": package["name"], "url": package.get("source", ""),
                   "dependencies": [{"name": dependency} for dependency in package.get("dependencies", [])]}
                  for package in rust_packages]
    (OUTPUT / "package-graph.json").write_text(json.dumps(
        {"buck": graphs, "rust": rust_graph}, indent=2))
    failures += graph_violations(graphs + rust_graph)
    detector_probe()
    lines = [f"Release app: {app}",
             "Inspected: Info.plist, the app icon, bundle resources, entitlements, the notification extension,",
             f"the platforms the executable was built for ({' '.join(sorted(built_for))}), executable symbols,",
             "compiled strings, every Buck2 target the app is built from and the locked Rust dependency graph.",
             "Detector rejected a compiler-built test executable carrying a debug export."]
    if failures:
        lines += ["FAIL: " + failure for failure in failures]
    else:
        lines += [f"PASS: icon {info['CFBundleIconName']} named at the top level of Info.plist "
                  f"and a {'x'.join(map(str, png_size(app / 'AppIcon60x60@2x.png')))} iPhone icon in the bundle.",
                  "PASS: no frozen-frame or test-support resource in the bundle.",
                  f"PASS: Bonjour limited to {' '.join(BONJOUR_SERVICES)} behind the agreed explanation "
                  "and no legacy browser; iPhone destinations only;",
                  "no amuxcloud or React Native package; no driving code.",
                  "PASS: in-app attention copy, Contact Support and Report a Problem remain;",
                  "the report screen, its freeze and the record snapshot ship.",
                  "PASS: the notification extension is embedded, shares the push key group and the",
                  "app's numbers, and carries none of the Rust bridge."]
    signed = bool(signed_entitlements(app))
    lines += ["Limit: " + ("the signed bundle, as uploaded." if signed else
                           "simulator Release bundle inspection; distribution signing is checked before release.")]
    report.write_text("\n".join(lines) + "\n")
    print(report.read_text(), end="", flush=True)
    if failures:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
