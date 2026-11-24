"""The macro reindeer's generated third-party/rust/BUCK builds each crate
library with (reindeer.toml, `rust_library`).

It is the prelude's `cargo.rust_library` with one difference: a dependent's
compile does not carry this crate's sources. The prelude adds the sources of
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

load("@prelude//rust:cargo_package.bzl", "cargo")
load("//build/platforms:defs.bzl", "EXEC_LIKE_TARGET")
load("@prelude//rust:link_info.bzl", "RustProcMacroPlugin")
load("@prelude//rust:sources.bzl", "RustSources")

def _without_sources_impl(ctx: AnalysisContext) -> list[Provider]:
    return [p for p in ctx.attrs.actual.providers if not isinstance(p, RustSources)]

_without_sources = rule(
    impl = _without_sources_impl,
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
