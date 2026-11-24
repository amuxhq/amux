"""The Apple apps' rules (docs/BUILD.md, "The Apple apps").

The iPhone and Mac apps are built from these rules rather than the
prelude's Apple rules, which outside Meta have no toolchain to build with and
whose tests are run by Meta's own runner (`apple_test`'s command is `false`).
Each step is a small action calling Xcode's tools (build/apple/apple.py):

  swift_library   one Swift module, compiled whole into one object, as
                  SwiftPM builds a package target, with its resources in a
                  bundle of their own reached through `Bundle.module`
  c_module        a C target with its headers and module map
  header_module   a prebuilt static library (the Rust bridge) with its header
  apple_binary    the link: an executable, an extension or a test bundle's code
  apple_bundle    the .app, .appex or .xctest around a binary: Info.plist,
                  resources, compiled asset and string catalogs, nested
                  bundles, and an ad hoc signature for a simulator or this Mac
  apple_xctest    a test bundle run with xcodebuild on a simulator leased
                  from wt's pool, or on this Mac (build/apple/xctest.py)

Every action runs on a Mac's worker (`exec_compatible_with` below), and
what it builds for comes from the target platform: the SDK and target
triple from the platform's OS and SDK, and the Swift build from its mode
(phone-dev: Debug, release: Release, measured: Release with the app's
driving tools). Device builds are left unsigned; a release signs them with
the distribution identity on this Mac, outside the build (build/apple/sign.py).
"""

load("@prelude//decls:re_test_common.bzl", "re_test_common")
load("@prelude//cxx:cxx_toolchain_types.bzl", "CxxToolchainInfo")
load("@prelude//decls:toolchains_common.bzl", "toolchains_common")
load("@prelude//linking:link_info.bzl", "LinkStrategy", "MergedLinkInfo", "get_link_args_for_strategy", "unpack_link_args")
load("@prelude//tests:re_utils.bzl", "get_re_executors_from_props")

AppleModuleInfo = provider(fields = [
    # .swiftmodule files; their directories are the importers' search paths.
    "swiftmodules",
    # Directories holding a module.modulemap and its headers.
    "include_dirs",
    # Objects linked whole, as Xcode links a package target's object.
    "objects",
    # Static archives (the Rust bridge), and those linked whole.
    "archives",
    "force_load",
    # SwiftPM-style resource bundles (`<Package>_<Target>.bundle`).
    "resource_bundles",
    # A prebuilt library's link arguments (the bridge's archive and the C
    # libraries its crates build), by the label that gave them.
    "link_args",
])

AppleBinaryInfo = provider(fields = [
    "executable",
    # The entitlements it was linked with, settings filled in.
    "entitlements",
    # What it linked, so a test bundle loaded into it links none of it again.
    "linked",
    "resource_bundles",
])

AppleBundleInfo = provider(fields = ["bundle", "name", "identifier", "binary"])

# Only a Mac can build or run any of this.
MACOS_EXEC = ["prelude//os:macos"]

# Where everything here is deployed: iOS 26 and macOS 26 (as in Package.swift
# before these rules).
MINIMUM = "26.0"

_SDK = select({
    "prelude//os:macos": "macosx",
    "prelude//os:iphoneos": select({
        "DEFAULT": "iphoneos",
        "root//build/constraints:simulator": "iphonesimulator",
    }),
})

_TARGET = select({
    "prelude//os:macos": "arm64-apple-macos" + MINIMUM,
    "prelude//os:iphoneos": select({
        "DEFAULT": "arm64-apple-ios" + MINIMUM,
        "root//build/constraints:simulator": "arm64-apple-ios{}-simulator".format(MINIMUM),
    }),
})

# Xcode's three configurations, from the platform's mode.
_BUILD = select({
    "DEFAULT": "debug",
    "root//build/constraints:release": "release",
    "root//build/constraints:measured": "measured",
})

# The App ID prefix in the app's entitlements and Info.plist. A Mac with a
# signing file (apps/apple/Signing.local.xcconfig) gets its team, which
# scripts/buck-config.py copies into `apple.team`; a simulator build without
# one gets a stand-in, since only a device checks it.
_TEAM = read_root_config("apple", "team", "") or "AMUXLOCAL0"

_PLATFORM_ATTRS = {
    "_sdk": attrs.string(default = _SDK),
    "_target": attrs.string(default = _TARGET),
    "_build": attrs.string(default = _BUILD),
    "_tool": attrs.source(default = "root//build/apple:apple.py"),
}

def _unique(items):
    seen = {}
    out = []
    for item in items:
        if item not in seen:
            seen[item] = True
            out.append(item)
    return out

def _merge(infos, own = None):
    own = own or {}
    return AppleModuleInfo(
        swiftmodules = _unique(own.get("swiftmodules", []) + [m for i in infos for m in i.swiftmodules]),
        include_dirs = _unique(own.get("include_dirs", []) + [d for i in infos for d in i.include_dirs]),
        objects = _unique(own.get("objects", []) + [o for i in infos for o in i.objects]),
        archives = _unique(own.get("archives", []) + [a for i in infos for a in i.archives]),
        force_load = _unique(own.get("force_load", []) + [a for i in infos for a in i.force_load]),
        resource_bundles = _unique(own.get("resource_bundles", []) + [b for i in infos for b in i.resource_bundles]),
        link_args = {k: v for i in infos for k, v in i.link_args.items()} | own.get("link_args", {}),
    )

def _search_paths(info):
    paths = []
    for module in info.swiftmodules:
        paths += ["-I", cmd_args(module, parent = 1)]
    for include in info.include_dirs:
        paths += ["-I", include]
    return paths

def _swift_build_flags(build, testable):
    if build == "debug":
        return ["-Onone", "-g"] + (["-enable-testing"] if testable else [])
    return ["-O"]

# SwiftPM's resource accessor, for a target compiled here: the target's
# resources travel in `<Package>_<Target>.bundle`, which an app carries at its
# root (Contents/Resources on a Mac) and a test bundle in its own.
_ACCESSOR = """import Foundation

// Written by build/apple.bzl, as SwiftPM writes it for a package target with
// resources.
private final class AmuxResourceBundleFinder {{}}

extension Foundation.Bundle {{
    static let module: Bundle = {{
        let name = "{bundle}"
        let places = [
            Bundle.main.resourceURL,
            Bundle(for: AmuxResourceBundleFinder.self).resourceURL,
            Bundle.main.bundleURL,
        ]
        for place in places {{
            if let url = place?.appendingPathComponent(name), let bundle = Bundle(url: url) {{
                return bundle
            }}
        }}
        fatalError("could not find the resource bundle \\(name)")
    }}()
}}
"""

def _swift_library_impl(ctx: AnalysisContext) -> list[Provider]:
    module = ctx.attrs.module_name or ctx.label.name
    deps = _merge([dep[AppleModuleInfo] for dep in ctx.attrs.deps])
    srcs = list(ctx.attrs.srcs)
    bundles = []
    if ctx.attrs.resources:
        name = "{}_{}".format(ctx.attrs.package, module)
        srcs.append(ctx.actions.write("resource_bundle_accessor.swift", _ACCESSOR.format(bundle = name + ".bundle")))
        bundle = ctx.actions.declare_output(name + ".bundle", dir = True)
        ctx.actions.run(
            cmd_args(
                "python3",
                ctx.attrs._tool,
                "resource-bundle",
                "--sdk",
                ctx.attrs._sdk,
                "--name",
                name,
                "--identifier",
                "{}.{}.resources".format(ctx.attrs.package, module),
                "--minimum",
                MINIMUM,
                "--out",
                bundle.as_output(),
                [cmd_args("--file=", src, "=", path, delimiter = "") for path, src in ctx.attrs.resources.items()],
            ),
            category = "resource_bundle",
            identifier = name,
        )
        bundles.append(bundle)
    swiftmodule = ctx.actions.declare_output("modules/{}.swiftmodule".format(module))
    obj = ctx.actions.declare_output("{}.o".format(module))
    conditions = list(ctx.attrs.conditions)
    if ctx.attrs.package:
        conditions.append("SWIFT_PACKAGE")
    if ctx.attrs._build == "debug" and ctx.attrs.debug_condition:
        conditions.append("DEBUG")
    ctx.actions.run(
        cmd_args(
            "python3",
            ctx.attrs._tool,
            "swiftc",
            "--sdk",
            ctx.attrs._sdk,
            ["--xctest"] if ctx.attrs.xctest else [],
            [cmd_args("--src-dir=", d[DefaultInfo].default_outputs[0], delimiter = "") for d in ctx.attrs.src_dirs],
            "--",
            "-target",
            ctx.attrs._target,
            "-module-name",
            module,
            "-parse-as-library",
            "-swift-version",
            "6",
            "-strict-concurrency=complete",
            _swift_build_flags(ctx.attrs._build, ctx.attrs.testable),
            ["-application-extension"] if ctx.attrs.application_extension else [],
            ["-D" + condition for condition in conditions],
            ctx.attrs.swift_flags,
            _search_paths(deps),
            "-emit-module",
            "-emit-module-path",
            swiftmodule.as_output(),
            "-wmo",
            "-c",
            "-o",
            obj.as_output(),
            srcs,
        ),
        category = "swiftc",
        identifier = module,
    )
    info = _merge([deps], {
        "swiftmodules": [swiftmodule],
        "objects": [obj],
        "resource_bundles": bundles,
    })
    return [DefaultInfo(default_outputs = [obj, swiftmodule], other_outputs = bundles), info]

_swift_library = rule(
    impl = _swift_library_impl,
    attrs = {
        "module_name": attrs.string(default = ""),
        "srcs": attrs.list(attrs.source()),
        # Directories of sources: another package's, compiled into this
        # module (a filegroup's output).
        "src_dirs": attrs.list(attrs.dep(), default = []),
        "deps": attrs.list(attrs.dep(providers = [AppleModuleInfo]), default = []),
        # bundle path -> file, as SwiftPM's `.copy` lays them out.
        "resources": attrs.dict(attrs.string(), attrs.source(), sorted = False, default = {}),
        # The Swift package the target was declared in: names its resource
        # bundle and defines SWIFT_PACKAGE, as SwiftPM does.
        "package": attrs.string(default = ""),
        "conditions": attrs.list(attrs.string(), default = []),
        # DEBUG in a debug build, as Xcode defines it for every target.
        "debug_condition": attrs.bool(default = True),
        "swift_flags": attrs.list(attrs.arg(), default = []),
        "testable": attrs.bool(default = True),
        "xctest": attrs.bool(default = False),
        "application_extension": attrs.bool(default = False),
    } | _PLATFORM_ATTRS,
)

def swift_library(name, **kwargs):
    _swift_library(name = name, exec_compatible_with = MACOS_EXEC, **kwargs)

def _c_module_impl(ctx: AnalysisContext) -> list[Provider]:
    include = ctx.actions.declare_output("include", dir = True)
    obj = ctx.actions.declare_output(ctx.attrs.module_name + ".o")
    build = ["-O0", "-g"] if ctx.attrs._build == "debug" else ["-Os"]
    ctx.actions.run(
        cmd_args(
            "python3",
            ctx.attrs._tool,
            "c-module",
            "--sdk",
            ctx.attrs._sdk,
            "--target",
            ctx.attrs._target,
            "--name",
            ctx.attrs.module_name,
            "--include",
            include.as_output(),
            "--object",
            obj.as_output(),
            [cmd_args("--header=", h, delimiter = "") for h in ctx.attrs.headers],
            [cmd_args("--src=", s, delimiter = "") for s in ctx.attrs.srcs],
            [cmd_args("--flag=", f, delimiter = "") for f in build],
        ),
        category = "cc",
        identifier = ctx.attrs.module_name,
    )
    return [
        DefaultInfo(default_outputs = [obj, include]),
        _merge([], {"include_dirs": [include], "objects": [obj]}),
    ]

_c_module = rule(
    impl = _c_module_impl,
    attrs = {
        "module_name": attrs.string(),
        "srcs": attrs.list(attrs.source()),
        "headers": attrs.list(attrs.source()),
    } | _PLATFORM_ATTRS,
)

def c_module(name, **kwargs):
    _c_module(name = name, exec_compatible_with = MACOS_EXEC, **kwargs)

def _header_module_impl(ctx: AnalysisContext) -> list[Provider]:
    include = ctx.actions.declare_output("include", dir = True)
    header = ctx.attrs.header[DefaultInfo].default_outputs[0]
    ctx.actions.run(
        cmd_args(
            "python3",
            ctx.attrs._tool,
            "header-module",
            "--name",
            ctx.attrs.module_name,
            "--include",
            include.as_output(),
            "--header",
            header,
            "--header-name",
            ctx.attrs.header_name,
        ),
        category = "header_module",
        identifier = ctx.attrs.module_name,
    )
    archive = ctx.attrs.archive[DefaultInfo].default_outputs[0]
    # The crate's whole static link: its archive and the C libraries its
    # dependencies' build scripts make (ring's, for one), which a Cargo
    # staticlib bundles and Buck2's leaves to the final link.
    link = unpack_link_args(get_link_args_for_strategy(
        ctx.actions,
        ctx.label,
        ctx.attrs._cxx_toolchain[CxxToolchainInfo].linker_info,
        [ctx.attrs.library[MergedLinkInfo]],
        LinkStrategy("static_pic"),
        False,
        None,
    ))
    return [
        DefaultInfo(default_outputs = [archive, include]),
        _merge([], {
            "include_dirs": [include],
            "force_load": [archive] if ctx.attrs.force_load else [],
            "link_args": {str(ctx.label.raw_target()): link},
        }),
    ]

_header_module = rule(
    impl = _header_module_impl,
    attrs = {
        "module_name": attrs.string(),
        # (target, platform): the library built as the app needs it, whose
        # configuration is its own (the bridge's mode and features), and
        # its archive (the library's `[staticlib]`).
        "library": attrs.configured_dep(providers = [MergedLinkInfo]),
        "archive": attrs.configured_dep(),
        "header": attrs.configured_dep(),
        # The file in `header` when it is a directory.
        "header_name": attrs.string(default = ""),
        # Linked whole: everything in it is reachable from the app, so a
        # test bundle loaded into the app finds any symbol it names.
        "force_load": attrs.bool(default = False),
        "_cxx_toolchain": toolchains_common.cxx(),
    } | _PLATFORM_ATTRS,
)

def header_module(name, **kwargs):
    _header_module(name = name, exec_compatible_with = MACOS_EXEC, **kwargs)

def _team_values():
    return {"AppIdentifierPrefix": _TEAM + ".", "TeamIdentifierPrefix": _TEAM + "."}

def _apple_binary_impl(ctx: AnalysisContext) -> list[Provider]:
    deps = _merge([dep[AppleModuleInfo] for dep in ctx.attrs.deps])
    linked = {}
    loader = None
    if ctx.attrs.bundle_loader:
        loader = ctx.attrs.bundle_loader[AppleBinaryInfo]
        linked = {artifact: True for artifact in loader.linked}
    objects = [o for o in deps.objects if o not in linked]
    archives = [a for a in deps.archives if a not in linked]
    force_load = [a for a in deps.force_load if a not in linked]
    bundles = [b for b in deps.resource_bundles if b not in linked]
    # A test bundle loaded into an app links none of the app's prebuilt
    # libraries again: the app carries them.
    link_args = [] if loader else deps.link_args.values()
    out = ctx.actions.declare_output(ctx.attrs.product_name)
    grants = None
    if ctx.attrs.entitlements:
        grants = ctx.actions.declare_output("entitlements.plist")
        ctx.actions.run(
            cmd_args(
                "python3",
                ctx.attrs._tool,
                "entitlements",
                "--src",
                ctx.attrs.entitlements,
                "--out",
                grants.as_output(),
                [cmd_args("--value=", k, "=", v, delimiter = "") for k, v in _team_values().items()],
            ),
            category = "entitlements",
            identifier = ctx.attrs.product_name,
        )
    kind = ctx.attrs.kind
    if kind == "executable":
        shape = ["-emit-executable"]
    elif kind == "extension":
        shape = ["-emit-executable", "-application-extension", "-Xlinker", "-e", "-Xlinker", "_NSExtensionMain"]
    else:
        shape = ["-emit-library", "-Xlinker", "-bundle"]
        if loader:
            shape += ["-Xlinker", "-bundle_loader", "-Xlinker", loader.executable]
    rpath = "@executable_path/../Frameworks" if ctx.attrs._sdk == "macosx" else "@executable_path/Frameworks"
    if kind == "bundle":
        rpath = "@loader_path/../Frameworks" if ctx.attrs._sdk == "macosx" else "@loader_path/Frameworks"
    simulator_grants = grants if grants and ctx.attrs._sdk == "iphonesimulator" else None
    ctx.actions.run(
        cmd_args(
            "python3",
            ctx.attrs._tool,
            "link",
            "--sdk",
            ctx.attrs._sdk,
            ["--xctest"] if ctx.attrs.xctest else [],
            ["--entitlements", simulator_grants] if simulator_grants else [],
            "--",
            "-target",
            ctx.attrs._target,
            shape,
            "-o",
            out.as_output(),
            "-Xlinker",
            "-rpath",
            "-Xlinker",
            rpath,
            ["-Xlinker", "-dead_strip"] if ctx.attrs._build != "debug" else [],
            ctx.attrs.linker_flags,
            objects,
            [["-Xlinker", "-force_load", "-Xlinker", a] for a in force_load],
            [a for a in archives if a not in force_load],
            list(link_args),
        ),
        category = "link",
        identifier = ctx.attrs.product_name,
    )
    info = AppleBinaryInfo(
        executable = out,
        entitlements = grants,
        linked = objects + archives + force_load + bundles + list(linked.keys()),
        resource_bundles = bundles,
    )
    return [DefaultInfo(default_output = out), info]

_apple_binary = rule(
    impl = _apple_binary_impl,
    attrs = {
        "product_name": attrs.string(),
        "deps": attrs.list(attrs.dep(providers = [AppleModuleInfo])),
        # executable, extension (an app extension's NSExtensionMain), or
        # bundle (a test bundle's code, loaded into a host).
        "kind": attrs.enum(["executable", "extension", "bundle"], default = "executable"),
        "bundle_loader": attrs.option(attrs.dep(providers = [AppleBinaryInfo]), default = None),
        "entitlements": attrs.option(attrs.source(), default = None),
        "linker_flags": attrs.list(attrs.arg(), default = []),
        "xctest": attrs.bool(default = False),
    } | _PLATFORM_ATTRS,
)

def apple_binary(name, **kwargs):
    _apple_binary(name = name, exec_compatible_with = MACOS_EXEC, **kwargs)

def _apple_bundle_impl(ctx: AnalysisContext) -> list[Provider]:
    product = ctx.attrs.product_name
    out = ctx.actions.declare_output("{}.{}".format(product, ctx.attrs.extension), dir = True)
    binary = ctx.attrs.binary[AppleBinaryInfo] if ctx.attrs.binary else None
    values = {
        "DEVELOPMENT_LANGUAGE": "en",
        "EXECUTABLE_NAME": ctx.attrs.executable_name or product,
        "PRODUCT_NAME": product,
        "PRODUCT_MODULE_NAME": ctx.attrs.module_name or product,
        "PRODUCT_BUNDLE_IDENTIFIER": ctx.attrs.identifier,
    } | _team_values() | ctx.attrs.values
    resources = [(src, path) for path, src in ctx.attrs.resources.items()]
    plugins = [p[AppleBundleInfo].bundle for p in ctx.attrs.plugins]
    spec = {
        "sdk": ctx.attrs._sdk,
        "minimum": MINIMUM,
        "info_plist": ctx.attrs.info_plist,
        "info": ctx.attrs.info,
        "values": values,
        "resources": resources,
        "resource_bundles": binary.resource_bundles if binary else [],
        "plugins": plugins,
        "executable": binary.executable if binary else None,
        # A simulator reads entitlements from the binary (apple_binary links
        # them in) and refuses an ad hoc signature carrying restricted ones,
        # as Xcode's "Sign to Run Locally" leaves them out of it.
        "entitlements": binary.entitlements if binary and ctx.attrs._sdk == "macosx" else None,
        "app_icon": ctx.attrs.app_icon,
        # A simulator and this Mac run an ad hoc signature; a phone needs the
        # team's, which a release applies (build/apple/sign.py).
        "sign": "none" if ctx.attrs._sdk == "iphoneos" else "adhoc",
    }
    spec_file = ctx.actions.write_json("bundle.json", spec, with_inputs = True)
    ctx.actions.run(
        cmd_args("python3", ctx.attrs._tool, "bundle", "--spec", spec_file, "--out", out.as_output()),
        category = "bundle",
        identifier = product,
    )
    return [
        DefaultInfo(default_output = out),
        AppleBundleInfo(bundle = out, name = product, identifier = ctx.attrs.identifier, binary = binary),
    ]

_apple_bundle = rule(
    impl = _apple_bundle_impl,
    attrs = {
        "product_name": attrs.string(),
        "executable_name": attrs.string(default = ""),
        "module_name": attrs.string(default = ""),
        # app, appex or xctest.
        "extension": attrs.string(default = "app"),
        "identifier": attrs.string(),
        "binary": attrs.option(attrs.dep(providers = [AppleBinaryInfo]), default = None),
        "info_plist": attrs.option(attrs.source(), default = None),
        # Keys added to (or, given no Info.plist, making up) the Info.plist.
        "info": attrs.dict(attrs.string(), attrs.any(), default = {}),
        # $(NAME) in the Info.plist.
        "values": attrs.dict(attrs.string(), attrs.string(), default = {}),
        # bundle path -> file. A path under `<Name>.xcassets/` is an asset
        # catalog, compiled; a `.xcstrings` file is compiled into each
        # language's tables.
        "resources": attrs.dict(attrs.string(), attrs.source(), sorted = False, default = {}),
        "app_icon": attrs.option(attrs.string(), default = None),
        # Bundles carried in PlugIns: an extension, or a hosted test bundle.
        "plugins": attrs.list(attrs.dep(providers = [AppleBundleInfo]), default = []),
    } | _PLATFORM_ATTRS,
)

def apple_bundle(name, **kwargs):
    _apple_bundle(name = name, exec_compatible_with = MACOS_EXEC, **kwargs)

def _apple_xctest_impl(ctx: AnalysisContext) -> list[Provider]:
    tests = ctx.attrs.bundle[AppleBundleInfo]
    host = ctx.attrs.host[AppleBundleInfo] if ctx.attrs.host else None
    inputs = [tests.bundle] + ([host.bundle] if host else []) + list(ctx.attrs.srcs)
    for dep in ctx.attrs.deps:
        inputs.extend(dep[DefaultInfo].default_outputs)
        inputs.extend(dep[DefaultInfo].other_outputs)
    command = cmd_args(
        "python3",
        ctx.attrs._runner,
        "--sdk",
        ctx.attrs._sdk,
        "--name",
        tests.name,
        "--bundle",
        tests.bundle,
        ["--host", host.bundle, "--host-identifier", host.identifier] if host else [],
        # The simulator pool (scripts/buck-config.py): the same on every
        # tree of a Mac, so a result is a hit from any of them.
        ["--pool-tree", ctx.attrs._pool_tree] if ctx.attrs._pool_tree else [],
        [cmd_args("--env=", k, "=", v, delimiter = "") for k, v in ctx.attrs.env.items()],
        hidden = inputs,
    )
    executors = get_re_executors_from_props(ctx)
    return [
        DefaultInfo(default_outputs = [tests.bundle]),
        ExternalRunnerTestInfo(
            type = "custom",
            command = [command],
            labels = ctx.attrs.labels,
            default_executor = executors.default_executor,
            executor_overrides = executors.executor_overrides,
            run_from_project_root = True,
            use_project_relative_paths = True,
            supports_test_execution_caching = True,
        ),
    ]

_apple_xctest = rule(
    impl = _apple_xctest_impl,
    attrs = {
        "bundle": attrs.dep(providers = [AppleBundleInfo]),
        # The app a hosted test bundle runs inside, carrying it in PlugIns.
        "host": attrs.option(attrs.dep(providers = [AppleBundleInfo]), default = None),
        # What the tests read: files of the checkout (at their paths under
        # AMUX_SOURCE_ROOT) and other targets' outputs.
        "srcs": attrs.list(attrs.source(), default = []),
        "deps": attrs.list(attrs.dep(), default = []),
        "env": attrs.dict(attrs.string(), attrs.arg(), sorted = False, default = {}),
        "labels": attrs.list(attrs.string(), default = []),
        "_runner": attrs.source(default = "root//build/apple:xctest.py"),
        "_pool_tree": attrs.string(default = read_root_config("apple", "simulator_pool_tree", "")),
    } | _PLATFORM_ATTRS | re_test_common.test_args(),
)

def apple_xctest(name, labels = [], **kwargs):
    _apple_xctest(name = name, labels = ["apple"] + labels, exec_compatible_with = MACOS_EXEC, **kwargs)
