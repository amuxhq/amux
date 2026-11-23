"""Platforms (docs/BUILD.md, "Configurations")."""

# Everything the workspace is built for, as prelude constraints.
TARGETS = {
    "macos": ["prelude//os:macos", "prelude//cpu:arm64"],
    "linux": ["prelude//os:linux", "prelude//cpu:x86_64"],
    "windows": ["prelude//os:windows", "prelude//cpu:x86_64"],
    "ios": ["prelude//os:iphoneos", "prelude//cpu:arm64", "root//build/constraints:device"],
    "ios-sim": ["prelude//os:iphoneos", "prelude//cpu:arm64", "root//build/constraints:simulator"],
    "android-arm64": ["prelude//os:android", "prelude//cpu:arm64"],
    "android-x86_64": ["prelude//os:android", "prelude//cpu:x86_64"],
    "wasm": ["prelude//os:none", "prelude//cpu:wasm32"],
}

# Cargo's profiles.
MODES = ["dev", "models", "release", "phone-dev", "wasm", "full-debug"]

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
        ),
    )
    return [DefaultInfo(), platform]

# One place an action can run: the constraints it satisfies and the
# properties NativeLink's scheduler matches a worker by.
execution_platform = rule(
    impl = _execution_platform_impl,
    attrs = {
        "constraints": attrs.list(attrs.dep(providers = [ConfigurationInfo])),
        "properties": attrs.dict(attrs.string(), attrs.string()),
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
    "prelude//os:windows": "x86_64-pc-windows-msvc",
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
