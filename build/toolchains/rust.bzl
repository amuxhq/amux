"""The Rust toolchain: rustup's pinned toolchain, called by its versioned path.

The prelude's system toolchain calls `rustc` by name, which would leave the
compiler's version out of every action's cache key. Here the compiler is
named by the directory scripts/buck-config.py found for the pinned channel,
and that path carries the version. For Linux targets it is the Linux
toolchain's (nix.bzl) instead, called by its store path on the Linux worker.
"""

load("@prelude//rust:rust_toolchain.bzl", "PanicRuntime", "RustToolchainInfo")
load(":nix.bzl", "NixToolchainInfo", "nix_tool")

def _amux_rust_toolchain_impl(ctx):
    if ctx.attrs.nix:
        tool = lambda name: nix_tool(ctx.attrs.nix, "bin/" + name)
    else:
        tool = lambda name: RunInfo(args = [ctx.attrs.toolchain_dir + "/bin/" + name])
    return [
        DefaultInfo(),
        RustToolchainInfo(
            allow_lints = ctx.attrs.allow_lints,
            # The prelude writes clippy-driver into a script line by line
            # of its arguments, so the Linux one goes in as one argument.
            clippy_driver = RunInfo(args = cmd_args(tool("clippy-driver"), delimiter = " ")) if ctx.attrs.nix else tool("clippy-driver"),
            clippy_toml = ctx.attrs.clippy_toml[DefaultInfo].default_outputs[0] if ctx.attrs.clippy_toml else None,
            compiler = tool("rustc"),
            default_edition = ctx.attrs.default_edition,
            panic_runtime = PanicRuntime(ctx.attrs.panic_runtime),
            deny_lints = ctx.attrs.deny_lints,
            doctests = ctx.attrs.doctests,
            nightly_features = False,
            report_unused_deps = False,
            rustc_binary_flags = ctx.attrs.rustc_binary_flags,
            rustc_flags = ctx.attrs.rustc_flags,
            rustc_target_triple = ctx.attrs.rustc_target_triple,
            rustc_test_flags = [],
            rustdoc = tool("rustdoc"),
            rustdoc_flags = [],
            warn_lints = ctx.attrs.warn_lints,
        ),
    ]

amux_rust_toolchain = rule(
    impl = _amux_rust_toolchain_impl,
    attrs = {
        "allow_lints": attrs.list(attrs.string(), default = []),
        "clippy_toml": attrs.option(attrs.dep(providers = [DefaultInfo]), default = None),
        "default_edition": attrs.option(attrs.string(), default = None),
        "deny_lints": attrs.list(attrs.string(), default = []),
        "doctests": attrs.bool(default = False),
        # The Linux toolchain, for Linux targets; otherwise `toolchain_dir`.
        "nix": attrs.option(attrs.exec_dep(providers = [NixToolchainInfo]), default = None),
        "panic_runtime": attrs.string(default = "unwind"),
        "rustc_binary_flags": attrs.list(attrs.arg(), default = []),
        "rustc_flags": attrs.list(attrs.arg(), default = []),
        "rustc_target_triple": attrs.string(),
        "toolchain_dir": attrs.string(),
        "warn_lints": attrs.list(attrs.string(), default = []),
    },
    is_toolchain_rule = True,
)
