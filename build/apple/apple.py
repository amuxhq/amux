#!/usr/bin/env python3
"""The steps of the Apple apps' build (build/apple.bzl, docs/BUILD.md).

Each subcommand is one Buck2 action on a Mac's worker. They call Xcode's
tools through xcrun, which finds the selected Xcode; its version is in every
Mac action's key (build/platforms/BUCK), so an Xcode upgrade reruns them.
Paths on the command line are relative to the action's root, as Buck2 gives
them, so nothing the outputs carry names the worker's directory.

  swiftc       compile one Swift module (whole-module) to an object file
               and its .swiftmodule
  c-module     compile a C target's sources and lay out its headers with a
               module map, as SwiftPM does for a C target
  header-module  give a prebuilt library's header a module map (the bridge)
  link         link objects and archives into an executable or a bundle
  entitlements fill in an entitlements file's build settings
  resource-bundle  the bundle a Swift package target's resources travel in
  bundle       assemble an app, extension or test bundle and sign it
"""

from pathlib import Path
import argparse
import json
import os
import plistlib
import shutil
import subprocess
import sys
import tempfile

# Shared by every compile on the machine, as Xcode's DerivedData module cache
# is: building the SDK's modules (Foundation, SwiftUI, UIKit...) takes far
# longer than compiling a module of ours, and an action's own directory starts
# empty. Clang keys every entry by the compiler, SDK and flags that built it
# and validates it on use, so a stale or foreign entry is rebuilt, never used;
# what is compiled does not depend on what the cache holds.
MODULE_CACHE = "/tmp/amux-swift-module-cache"

PLATFORM_NAMES = {
    "iphoneos": "iPhoneOS",
    "iphonesimulator": "iPhoneSimulator",
    "macosx": "MacOSX",
}


def xcrun(sdk: str, *arguments: str) -> str:
    return subprocess.run(
        ["xcrun", "--sdk", sdk, *arguments],
        check=True, capture_output=True, text=True, timeout=120,
    ).stdout.strip()


def sdk_path(sdk: str) -> str:
    return xcrun(sdk, "--show-sdk-path")


def platform_path(sdk: str) -> str:
    return xcrun(sdk, "--show-sdk-platform-path")


def run(command: list[str], **kwargs) -> None:
    """Run a tool, and on failure say what it printed and stop."""
    result = subprocess.run(command, capture_output=True, text=True, **kwargs)
    if result.returncode:
        sys.stderr.write(result.stdout)
        sys.stderr.write(result.stderr)
        sys.exit(f"{Path(command[0]).name} failed ({result.returncode})")
    # Warnings are worth seeing; a clean compile prints nothing.
    if result.stderr.strip():
        sys.stderr.write(result.stderr)


def xctest_flags(sdk: str) -> list[str]:
    """Where a test module finds XCTest: the platform's developer frameworks,
    which aren't in the SDK."""
    platform = platform_path(sdk)
    return [
        "-F", f"{platform}/Developer/Library/Frameworks",
        "-I", f"{platform}/Developer/usr/lib",
    ]


def swiftc(arguments: argparse.Namespace, rest: list[str]) -> None:
    # A directory of sources (another package's, by a filegroup) compiles
    # as its files.
    sources = sorted(str(path) for directory in arguments.src_dir
                     for path in Path(directory).rglob("*.swift"))
    command = [
        "xcrun", "--sdk", arguments.sdk, "swiftc",
        "-sdk", sdk_path(arguments.sdk),
        "-module-cache-path", MODULE_CACHE,
        # Debug information names paths relative to the root the sources
        # were compiled in, not the worker's directory for this action.
        "-file-compilation-dir", ".",
        "-debug-prefix-map", f"{os.getcwd()}=.",
        *(xctest_flags(arguments.sdk) if arguments.xctest else []),
        *rest,
        *sources,
    ]
    run(command, timeout=1800)


def c_module(arguments: argparse.Namespace) -> None:
    include = Path(arguments.include)
    include.mkdir(parents=True, exist_ok=True)
    names = []
    for header in arguments.header:
        shutil.copy2(header, include / Path(header).name)
        names.append(Path(header).name)
    (include / "module.modulemap").write_text(
        f"module {arguments.name} {{\n"
        + "".join(f'  header "{name}"\n' for name in names)
        + "  export *\n}\n")
    command = [
        "xcrun", "--sdk", arguments.sdk, "clang",
        "-target", arguments.target, "-isysroot", sdk_path(arguments.sdk),
        "-I", str(include), "-c", *arguments.flag,
        "-ffile-compilation-dir=.", f"-fdebug-prefix-map={os.getcwd()}=.",
    ]
    objects = []
    with tempfile.TemporaryDirectory() as scratch:
        for index, source in enumerate(arguments.src):
            target = Path(scratch) / f"{index}-{Path(source).stem}.o"
            run([*command, source, "-o", str(target)], timeout=600)
            objects.append(str(target))
        # One object for the target, as SwiftPM links a C target's objects
        # whole: a constructor nothing references must still run.
        run(["xcrun", "ld", "-r", "-o", arguments.object, *objects], timeout=600)


def header_module(arguments: argparse.Namespace) -> None:
    include = Path(arguments.include)
    include.mkdir(parents=True, exist_ok=True)
    header = Path(arguments.header)
    if header.is_dir():
        header = header / arguments.header_name
    shutil.copy2(header, include / header.name)
    (include / "module.modulemap").write_text(
        f'module {arguments.name} {{\n  header "{header.name}"\n  export *\n}}\n')


def link(arguments: argparse.Namespace, rest: list[str]) -> None:
    flags = list(rest)
    if arguments.xctest:
        platform = platform_path(arguments.sdk)
        flags += [
            "-F", f"{platform}/Developer/Library/Frameworks",
            "-L", f"{platform}/Developer/usr/lib",
            "-framework", "XCTest", "-lXCTestSwiftSupport",
        ]
    if arguments.entitlements:
        # A simulator reads an app's entitlements from its binary rather than
        # its signature, as Xcode links them for a simulator build.
        der = Path(tempfile.mkdtemp()) / "entitlements.der"
        run(["/usr/bin/derq", "query", "-f", "xml", "-i", arguments.entitlements,
             "-o", str(der), "--raw"], timeout=60)
        flags += [
            "-Xlinker", "-sectcreate", "-Xlinker", "__TEXT", "-Xlinker", "__entitlements",
            "-Xlinker", arguments.entitlements,
            "-Xlinker", "-sectcreate", "-Xlinker", "__TEXT", "-Xlinker", "__ents_der",
            "-Xlinker", str(der),
        ]
    run(["xcrun", "--sdk", arguments.sdk, "swiftc", "-sdk", sdk_path(arguments.sdk), *flags],
        timeout=1800)


def substitute(text: str, values: dict[str, str]) -> str:
    for name, value in values.items():
        text = text.replace(f"$({name})", value).replace(f"${{{name}}}", value)
    return text


def entitlements(arguments: argparse.Namespace) -> None:
    values = dict(value.split("=", 1) for value in arguments.value)
    text = substitute(Path(arguments.src).read_text(), values)
    grants = plistlib.loads(text.encode())
    for extra in arguments.drop:
        grants.pop(extra, None)
    Path(arguments.out).write_bytes(plistlib.dumps(grants))


def copy(source: Path, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    if source.is_dir():
        shutil.copytree(source, destination, symlinks=True, dirs_exist_ok=True)
    else:
        shutil.copyfile(source, destination)
    # Buck2's inputs are read-only; a bundle is signed in place.
    for path in [destination, *destination.rglob("*")] if destination.is_dir() else [destination]:
        path.chmod(path.stat().st_mode | 0o200)


def platform_keys(sdk: str, minimum: str) -> dict:
    """The keys Xcode adds to every bundle it builds, which the App Store
    and the system read: what it was built with and what it runs on."""
    version = xcrun(sdk, "--show-sdk-version")
    build = xcrun(sdk, "--show-sdk-build-version")
    xcode = subprocess.run(["xcodebuild", "-version"], check=True, capture_output=True,
                           text=True, timeout=60).stdout.split()
    # "Xcode 26.6 Build version 17F113": DTXcode is 2660.
    parts = (xcode[1].split(".") + ["0", "0"])[:3]
    keys = {
        "BuildMachineOSBuild": subprocess.run(["sw_vers", "-buildVersion"], check=True,
                                              capture_output=True, text=True).stdout.strip(),
        "CFBundleSupportedPlatforms": [PLATFORM_NAMES[sdk]],
        "DTCompiler": "com.apple.compilers.llvm.clang.1_0",
        "DTPlatformBuild": build,
        "DTPlatformName": sdk,
        "DTPlatformVersion": version,
        "DTSDKBuild": build,
        "DTSDKName": f"{sdk}{version}",
        "DTXcode": f"{int(parts[0]):02d}{int(parts[1])}{int(parts[2])}",
        "DTXcodeBuild": xcode[-1],
    }
    if sdk == "macosx":
        keys["LSMinimumSystemVersion"] = minimum
    else:
        keys["MinimumOSVersion"] = minimum
        # The phone only (TARGETED_DEVICE_FAMILY 1).
        keys["UIDeviceFamily"] = [1]
    return keys


def sign(path: Path, identity: str, grants: str | None) -> None:
    command = ["codesign", "--force", "--sign", identity, "--timestamp=none",
               "--generate-entitlement-der"]
    if grants:
        command += ["--entitlements", grants]
    run([*command, str(path)], timeout=300)


def resource_bundle(arguments: argparse.Namespace) -> None:
    bundle = Path(arguments.out)
    contents = bundle / "Contents" if arguments.sdk == "macosx" else bundle
    resources = contents / "Resources" if arguments.sdk == "macosx" else bundle
    resources.mkdir(parents=True, exist_ok=True)
    for pair in arguments.file:
        source, destination = pair.split("=", 1)
        copy(Path(source), resources / destination)
    info = {
        "CFBundleDevelopmentRegion": "en",
        "CFBundleIdentifier": arguments.identifier,
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleName": arguments.name,
        "CFBundlePackageType": "BNDL",
    } | platform_keys(arguments.sdk, arguments.minimum)
    (contents / "Info.plist").write_bytes(plistlib.dumps(info))


def compile_catalog(catalog: Path, sdk: str, minimum: str, icon: str | None,
                    resources: Path, scratch: Path) -> dict:
    partial = scratch / f"{catalog.stem}-partial.plist"
    device = {"macosx": "mac"}.get(sdk, "iphone")
    command = [
        "xcrun", "--sdk", sdk, "actool", str(catalog),
        "--compile", str(resources), "--platform", sdk,
        "--minimum-deployment-target", minimum, "--target-device", device,
        "--output-partial-info-plist", str(partial),
        "--output-format", "human-readable-text", "--errors", "--warnings",
        "--compress-pngs",
    ]
    if icon:
        command += ["--app-icon", icon]
    run(command, timeout=600)
    return plistlib.loads(partial.read_bytes()) if partial.is_file() else {}


def compile_strings(catalog: Path, resources: Path) -> None:
    run(["xcrun", "xcstringstool", "compile", str(catalog), "--output-directory", str(resources)],
        timeout=300)


def bundle(arguments: argparse.Namespace) -> None:
    spec = json.loads(Path(arguments.spec).read_text())
    out = Path(arguments.out)
    sdk = spec["sdk"]
    mac = sdk == "macosx"
    contents = out / "Contents" if mac else out
    resources = contents / "Resources" if mac else out
    executables = contents / "MacOS" if mac else out
    plugins = contents / "PlugIns"
    resources.mkdir(parents=True, exist_ok=True)

    info = plistlib.loads(substitute(Path(spec["info_plist"]).read_text(), spec["values"]).encode()) \
        if spec.get("info_plist") else {}
    info = info | spec.get("info", {})
    with tempfile.TemporaryDirectory() as scratch_dir:
        scratch = Path(scratch_dir)
        # Resources: a file or a directory copied as it is, an asset catalog
        # compiled, a string catalog compiled into each language's tables.
        catalogs = {}
        for source, destination in spec["resources"]:
            top = destination.split("/", 1)[0]
            if top.endswith(".xcassets"):
                catalogs.setdefault(top, scratch / "catalogs" / top)
                copy(Path(source), catalogs[top] / destination.split("/", 1)[1])
            elif destination.endswith(".xcstrings"):
                compile_strings(Path(source), resources)
            else:
                copy(Path(source), resources / destination)
        for catalog in catalogs.values():
            info = compile_catalog(catalog, sdk, spec["minimum"], spec.get("app_icon"),
                                   resources, scratch) | info
        for path in spec["resource_bundles"]:
            copy(Path(path), resources / Path(path).name)
        for path in spec["plugins"]:
            copy(Path(path), plugins / Path(path).name)
    if spec.get("executable"):
        executables.mkdir(parents=True, exist_ok=True)
        copy(Path(spec["executable"]), executables / spec["values"]["EXECUTABLE_NAME"])
        (executables / spec["values"]["EXECUTABLE_NAME"]).chmod(0o755)
    info = info | platform_keys(sdk, spec["minimum"])
    if sdk == "iphoneos" and spec.get("executable"):
        # A phone's binary is arm64 alone, which the App Store requires the
        # bundle to say (error 90502), as Xcode writes it for a device build.
        capabilities = list(info.get("UIRequiredDeviceCapabilities", []))
        if "arm64" not in capabilities:
            info["UIRequiredDeviceCapabilities"] = capabilities + ["arm64"]
    (contents / "Info.plist").write_bytes(plistlib.dumps(info))
    package_type = info.get("CFBundlePackageType")
    if package_type == "APPL":
        (contents / "PkgInfo").write_text("APPL????")

    if spec["sign"] == "adhoc":
        # Inside out, as codesign requires: what a bundle carries is sealed
        # by the bundle's own signature only once it is signed itself.
        for nested in sorted(resources.glob("*.bundle")):
            sign(nested, "-", None)
        for nested in sorted(plugins.glob("*")) if plugins.is_dir() else []:
            if not (nested / "_CodeSignature").is_dir() and not (nested / "Contents/_CodeSignature").is_dir():
                sign(nested, "-", None)
        sign(out, "-", spec.get("entitlements"))


def main() -> None:
    argv = sys.argv[1:]
    rest = []
    if "--" in argv:
        at = argv.index("--")
        argv, rest = argv[:at], argv[at + 1:]
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)

    command = commands.add_parser("swiftc")
    command.add_argument("--sdk", required=True)
    command.add_argument("--xctest", action="store_true")
    command.add_argument("--src-dir", action="append", default=[])

    command = commands.add_parser("c-module")
    command.add_argument("--sdk", required=True)
    command.add_argument("--target", required=True)
    command.add_argument("--name", required=True)
    command.add_argument("--include", required=True)
    command.add_argument("--object", required=True)
    command.add_argument("--header", action="append", default=[])
    command.add_argument("--src", action="append", default=[])
    command.add_argument("--flag", action="append", default=[])

    command = commands.add_parser("header-module")
    command.add_argument("--name", required=True)
    command.add_argument("--include", required=True)
    command.add_argument("--header", required=True)
    command.add_argument("--header-name", default="")

    command = commands.add_parser("link")
    command.add_argument("--sdk", required=True)
    command.add_argument("--xctest", action="store_true")
    command.add_argument("--entitlements")

    command = commands.add_parser("entitlements")
    command.add_argument("--src", required=True)
    command.add_argument("--out", required=True)
    command.add_argument("--value", action="append", default=[])
    command.add_argument("--drop", action="append", default=[])

    command = commands.add_parser("resource-bundle")
    command.add_argument("--sdk", required=True)
    command.add_argument("--name", required=True)
    command.add_argument("--identifier", required=True)
    command.add_argument("--minimum", required=True)
    command.add_argument("--out", required=True)
    command.add_argument("--file", action="append", default=[])

    command = commands.add_parser("bundle")
    command.add_argument("--spec", required=True)
    command.add_argument("--out", required=True)

    arguments = parser.parse_args(argv)
    if arguments.command == "swiftc":
        swiftc(arguments, rest)
    elif arguments.command == "c-module":
        c_module(arguments)
    elif arguments.command == "header-module":
        header_module(arguments)
    elif arguments.command == "link":
        link(arguments, rest)
    elif arguments.command == "entitlements":
        entitlements(arguments)
    elif arguments.command == "resource-bundle":
        resource_bundle(arguments)
    elif arguments.command == "bundle":
        bundle(arguments)


if __name__ == "__main__":
    main()
