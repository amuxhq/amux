"""Platforms (docs/BUILD.md, "Configurations")."""

# Everything the workspace is built for, as prelude constraints.
TARGETS = {
    "macos": ["prelude//os:macos", "prelude//cpu:arm64"],
    "linux": ["prelude//os:linux", "prelude//cpu:x86_64"],
    "windows": ["prelude//os:windows", "prelude//cpu:x86_64"],
    # Windows on Arm, which a Windows VM on an Apple Silicon Mac is: its
    # tests run natively there rather than as x64 under emulation.
    "windows-arm64": ["prelude//os:windows", "prelude//cpu:arm64"],
    "ios": ["prelude//os:iphoneos", "prelude//cpu:arm64", "root//build/constraints:device"],
    "ios-sim": ["prelude//os:iphoneos", "prelude//cpu:arm64", "root//build/constraints:simulator"],
    "android-arm64": ["prelude//os:android", "prelude//cpu:arm64"],
    "android-x86_64": ["prelude//os:android", "prelude//cpu:x86_64"],
    "wasm": ["prelude//os:none", "prelude//cpu:wasm32"],
}

# Cargo's profiles.
MODES = ["dev", "models", "release", "phone-dev", "wasm", "full-debug"]

# For a rule with no toolchain to say it: a Linux target's actions run on
# a Linux worker. Buck2 sends an action to the first execution platform it
# fits, which on a Mac is the Mac's, so without this the same Linux target
# would have one key built on the build host and another built from a Mac,
# and an execution dependency (a binary a Linux test starts) would be built
# for macOS.
EXEC_LIKE_TARGET = select({
    "DEFAULT": [],
    "prelude//os:linux": ["prelude//os:linux"],
    # Android builds only on a Linux worker (docs/ANDROID.md).
    "prelude//os:android": ["prelude//os:linux"],
    # So does Windows (nix/windows.nix): only its tests run on Windows;
    # but a release build of Windows is built on Windows.
    "prelude//os:windows": select({
        "DEFAULT": ["prelude//os:linux"],
        "root//build/constraints:local-build": [],
    }),
})

# For the binaries a test starts (build/rust.bzl's amux_binaries): built
# for the machine the test runs on, which for a Windows test is Windows
# though it is built on Linux, and of the test's own architecture.
EXEC_RUNS_TARGET = select({
    "DEFAULT": EXEC_LIKE_TARGET,
    "prelude//os:windows": select({
        "DEFAULT": ["prelude//os:windows", "prelude//cpu:x86_64"],
        "prelude//cpu:arm64": ["prelude//os:windows", "prelude//cpu:arm64"],
    }),
})

def host_target() -> str:
    """The TARGETS entry for the machine Buck2 runs on."""
    if host_info().os.is_macos:
        return "macos"
    if host_info().os.is_linux:
        return "linux"
    if host_info().os.is_windows:
        return "windows"
    fail("no target platform for this host")

def host_platform(product = "cli", purpose = "shipping", mode = "dev") -> str:
    return "root//build/platforms:{}-{}-{}-{}".format(host_target(), product, purpose, mode)

def _merged_configuration(deps) -> ConfigurationInfo:
    constraints = {}
    values = {}
    for dep in deps:
        info = dep[ConfigurationInfo]
        constraints.update(info.constraints)
        values.update(info.values)
    return ConfigurationInfo(constraints = constraints, values = values)

def _target_platform_impl(ctx):
    return [
        DefaultInfo(),
        PlatformInfo(
            label = str(ctx.label.raw_target()),
            configuration = _merged_configuration(ctx.attrs.constraints),
        ),
    ]

# A target platform made of configuration settings: the prelude's `os` and
# `cpu` settings are config_settings rather than constraint values, which
# the built-in `platform` rule doesn't take.
target_platform = rule(
    impl = _target_platform_impl,
    attrs = {
        "constraints": attrs.list(attrs.dep(providers = [ConfigurationInfo])),
    },
    is_configuration_rule = True,
)

def _execution_platform_impl(ctx):
    configuration = _merged_configuration(ctx.attrs.constraints)
    if ctx.attrs.build_here:
        # CI's macOS runners (scripts/buck-config.py's builds_here): Buck2
        # runs the action itself, after looking it up in the cache, and
        # uploads what it made. A worker stages each action's inputs into a
        # directory of its own first: 0.37 s an action from a Mac's local
        # CAS, about 4 s from the build host's across the internet, against
        # well under a second of compiling for most actions; Buck2 runs it
        # in buck-out with no staging. Tests still run on the worker (their
        # executor is the test toolchain's), so their results are cached as
        # before.
        return [DefaultInfo(), ExecutionPlatformInfo(
            label = ctx.label.raw_target(),
            configuration = configuration,
            executor_config = CommandExecutorConfig(
                local_enabled = True,
                remote_enabled = False,
                remote_cache_enabled = True,
                allow_cache_uploads = True,
                remote_execution_properties = ctx.attrs.properties,
                remote_execution_use_case = "buck2-default",
                remote_output_paths = "output_paths",
            ),
        )]
    platform = ExecutionPlatformInfo(
        label = ctx.label.raw_target(),
        configuration = configuration,
        executor_config = CommandExecutorConfig(
            # Every action goes to NativeLink, so its result is cached and it
            # sees only its declared inputs. Limited hybrid runs locally only
            # what must (a `local_only` action such as fetching a git
            # dependency); nothing falls back to local when remote fails.
            local_enabled = True,
            remote_enabled = True,
            use_limited_hybrid = True,
            allow_limited_hybrid_fallbacks = False,
            allow_hybrid_fallbacks_on_failure = False,
            remote_execution_properties = ctx.attrs.properties,
            remote_execution_use_case = "buck2-default",
            remote_output_paths = "output_paths",
            # A Windows action names its paths as Windows does; it runs on
            # Windows, on CI's Windows worker or in a release build there.
            use_windows_path_separators = ctx.attrs.windows,
        ),
    )
    return [DefaultInfo(), platform]

# One place an action can run: the constraints it satisfies and the
# properties NativeLink's scheduler matches a worker by.
execution_platform = rule(
    impl = _execution_platform_impl,
    attrs = {
        "build_here": attrs.bool(default = False),
        "constraints": attrs.list(attrs.dep(providers = [ConfigurationInfo])),
        "properties": attrs.dict(attrs.string(), attrs.string()),
        "windows": attrs.bool(default = False),
    },
    is_configuration_rule = True,
)

def _execution_platforms_impl(ctx):
    return [
        DefaultInfo(),
        ExecutionPlatformRegistrationInfo(
            platforms = [p[ExecutionPlatformInfo] for p in ctx.attrs.platforms],
        ),
    ]

# The places this host's Buck2 may send an action, in order of preference:
# an action goes to the first whose constraints it is compatible with.
execution_platforms = rule(
    impl = _execution_platforms_impl,
    attrs = {
        "platforms": attrs.list(attrs.dep(providers = [ExecutionPlatformInfo])),
    },
    is_configuration_rule = True,
)

# The Rust target triple a configuration builds for: the toolchain's, and
# what crates that name it (release's AMUX_TARGET) are told.
RUST_TARGET_TRIPLE = select({
    "prelude//os:macos": "aarch64-apple-darwin",
    "prelude//os:linux": "x86_64-unknown-linux-gnu",
    "prelude//os:windows": select({
        "DEFAULT": "x86_64-pc-windows-msvc",
        "prelude//cpu:arm64": "aarch64-pc-windows-msvc",
    }),
    "prelude//os:iphoneos": select({
        "DEFAULT": "aarch64-apple-ios",
        "root//build/constraints:simulator": "aarch64-apple-ios-sim",
    }),
    "prelude//os:android": select({
        "prelude//cpu:arm64": "aarch64-linux-android",
        "prelude//cpu:x86_64": "x86_64-linux-android",
    }),
    "prelude//os:none": "wasm32-unknown-unknown",
})
