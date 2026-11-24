"""The macro reindeer's generated third-party/rust/BUCK builds each crate
library with (reindeer.toml, `rust_library`).

It is the prelude's `cargo.rust_library` with two differences. A crate is
configured once per target and mode, whatever product and purpose reach it
(the transition below). And a dependent's compile does not carry this
crate's sources. The prelude adds the sources of
every transitive Rust dependency to each rustc and clippy action as hidden
inputs (its `RustSources` provider), so a crate near the top of the graph
staged 11,000 files of extracted crates.io archives on the worker for every
compile, though rustc reads only the dependencies' rmeta and rlib files.
Each library here is built under an inner name and published under its own
through a rule that forwards every provider but `RustSources`, as the
prelude's `rust_linkable_symbol` does for its generated crates. The
workspace's own crates (build/rust.bzl) still pass theirs on: a few hundred
symlinks, not worth renaming every first-party target over.

What it costs: a diagnostic that points into a dependency still names the
file and line, but cannot quote the line. The prelude carries the sources
for one more reason (rust-lang/rust#153898): a crate's hash depends on
whether its dependencies' sources are present when it compiles, so a crate
compiled once with them and once without would not match. Every action runs
on NativeLink in a sandbox holding only its declared inputs, with no local
fallback (build/platforms), so they are absent from every compile alike.
"""

load("@prelude//rust:cargo_buildscript.bzl", "buildscript_run")
load("@prelude//rust:cargo_package.bzl", "cargo")
load("//build/platforms:defs.bzl", "EXEC_LIKE_TARGET", "MODES", "TARGETS")
load("@prelude//rust:link_info.bzl", "RustProcMacroPlugin")
load("@prelude//rust:sources.bzl", "RustSources")

# Product and purpose (build/constraints) decide the features of the
# workspace's own crates only: reindeer resolves a third-party crate's
# features once for the whole workspace, and nothing in third-party/rust
# selects on either. Left alone, a crate would still be configured once per
# product and purpose that reaches it (and once more as a build tool, on an
# execution platform), and each configuration has its own output paths, so
# the same compile ran and was cached up to five times over. So a
# third-party crate is always configured as the desktop product as it
# ships, keeping the target, SDK and mode: `macos-app-testing-dev` and the
# `exec-macos` execution platform both become `macos-cli-shipping-dev`.
#
# The new configuration takes the name of the target platform it equals,
# so it is that platform's configuration, not one that only looks like it.
_PLATFORMS = "root//build/platforms:"

_THIRD_PARTY_PLATFORM = {
    _PLATFORMS + "{}-{}-{}-{}".format(target, product, purpose, mode): _PLATFORMS + "{}-cli-shipping-{}".format(target, mode)
    for target in TARGETS
    for product in ["cli", "app"]
    for purpose in ["shipping", "testing"]
    for mode in MODES
}

# The execution platforms (build/platforms/BUCK) carry the tools'
# constraints: cli, testing, dev.
_THIRD_PARTY_PLATFORM.update({
    _PLATFORMS + "exec-macos": _PLATFORMS + "macos-cli-shipping-dev",
    _PLATFORMS + "exec-linux": _PLATFORMS + "linux-cli-shipping-dev",
    _PLATFORMS + "exec-windows": _PLATFORMS + "windows-cli-shipping-dev",
    _PLATFORMS + "exec-windows-arm64": _PLATFORMS + "windows-arm64-cli-shipping-dev",
})

def _third_party_platform_impl(platform: PlatformInfo, refs: struct) -> PlatformInfo:
    constraints = dict(platform.configuration.constraints)
    for value in [refs.cli[ConstraintValueInfo], refs.shipping[ConstraintValueInfo]]:
        constraints[value.setting.label] = value

    # The prelude marks the configuration of a crate's build script with
    # the platform the script is building for, which the script's own
    # platform-specific features select on. The mark then rides along to
    # every library the script depends on, none of which reads it, and would
    # configure `cc` once more for each platform a build script runs for.
    constraints.pop(refs.buildscript_for_platform[ConstraintSettingInfo].label, None)
    if constraints == platform.configuration.constraints:
        # Already there: every crate's dependencies pass through this again.
        return platform
    return PlatformInfo(
        # A platform not named above (one made by another transition) gets
        # a name of its own; once transitioned it is left as it is, so the
        # name doesn't grow along a chain of crates.
        label = _THIRD_PARTY_PLATFORM.get(platform.label, platform.label + "-cli-shipping"),
        configuration = ConfigurationInfo(constraints = constraints, values = platform.configuration.values),
    )

_third_party_platform = transition(
    impl = _third_party_platform_impl,
    refs = {
        "cli": "root//build/constraints:cli",
        "shipping": "root//build/constraints:shipping",
        "buildscript_for_platform": "prelude//rust/buildscript:buildscript_for_platform",
    },
)

def _without_sources_impl(ctx: AnalysisContext) -> list[Provider]:
    return [p for p in ctx.attrs.actual.providers if not isinstance(p, RustSources)]

_without_sources = rule(
    impl = _without_sources_impl,
    # Every third-party library is reached through this rule (its public
    # name, or a proc macro's plugin), so this is where its configuration
    # changes; what it builds from (the inner library, its build script
    # run, its C code and download) follows.
    cfg = _third_party_platform,
    # Proc macros reach a crate's dependents as plugins, which pass through
    # a dependency only where it says so, as the prelude's `deps` do.
    attrs = {"actual": attrs.dep(pulls_and_pushes_plugins = [RustProcMacroPlugin])},
)

def third_party_rust_library(name, visibility = None, **kwargs):
    if kwargs.get("proc_macro"):
        # The prelude publishes a proc macro as an alias (`name`) whose
        # plugin is the library itself (`_name`); a dependent compiles
        # against the plugin's providers, so the forwarding rule stands in
        # as the plugin.
        _without_sources(name = name + "@plugin", actual = ":_" + name)
        cargo.rust_library(
            name = name,
            proc_macro_alias_actual = ":" + name + "@plugin",
            visibility = visibility,
            **kwargs
        )
    else:
        cargo.rust_library(name = name + "@library", **kwargs)
        _without_sources(name = name, actual = ":" + name + "@library", visibility = visibility)

# A crate's download and unpacking (reindeer.toml, `http_archive` and
# `git_fetch`) run where the crate's compile does: on a Linux worker for a
# Linux target (build/platforms/defs.bzl, EXEC_LIKE_TARGET), so the
# unpacked sources have one key whichever machine started the build.
def third_party_http_archive(**kwargs):
    native.http_archive(exec_compatible_with = EXEC_LIKE_TARGET, **kwargs)

def third_party_git_fetch(**kwargs):
    native.git_fetch(exec_compatible_with = EXEC_LIKE_TARGET, **kwargs)

# A crate's build script run (reindeer.toml, `buildscript_genrule`). The
# prelude hands a build script the C toolchain's tools as CC, CXX, AR and
# LD shims. In a Windows release build, which uses MSVC
# (build/constraints:local-windows-build), those are blanked, so cc-rs
# finds MSVC's tools itself, as it does under Cargo: through the shims, its
# archive step left no library behind.
_MSVC_OWN = {"AR": "", "CC": "", "CXX": "", "LD": ""}

def third_party_buildscript_run(env = {}, **kwargs):
    buildscript_run(
        env = select({
            "DEFAULT": env,
            "root//build/constraints:local-windows-build": env | _MSVC_OWN,
        }),
        **kwargs
    )
