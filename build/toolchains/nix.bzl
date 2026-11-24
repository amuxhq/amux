"""The Linux toolchain: flake.nix's `packages.actions`, realized by an action.

A Linux action can be sent from a Mac, which has no Nix and can't put a
store path on the Linux worker. So realizing the toolchain is itself an
action on the Linux worker: it builds or fetches the flake output there and
writes its store path to a file. Every Linux tool is then called through
nix-run.sh with that file as an input, so each Linux action's key covers
the store path, whose hash changes with anything in the toolchain, and the
toolchain exists on the worker before any action that uses it runs.

The target that realizes it is compatible only with Linux, and the Rust and
C toolchains take it as an execution dependency for Linux targets, so Buck2
sends those targets' actions to a Linux execution platform.
"""

NixToolchainInfo = provider(fields = {
    # `RunInfo` running a path under the realized toolchain (`bin/rustc`)
    # with the toolchain's bin directory first on PATH.
    "run": provider_field(RunInfo),
})

def nix_tool(nix, path: str) -> RunInfo:
    return RunInfo(args = cmd_args(nix[NixToolchainInfo].run, path))

def _nix_toolchain_impl(ctx: AnalysisContext) -> list[Provider]:
    # A copy, not symlinks: Nix copies a `path:` flake into its store and
    # would not follow links out of it.
    flake = ctx.actions.copied_dir("flake", {src.short_path: src for src in ctx.attrs.srcs})
    root = ctx.actions.declare_output("root")
    ctx.actions.run(
        cmd_args("/bin/sh", ctx.attrs._realize, flake, ctx.attrs.output, root.as_output()),
        category = "nix_realize",
    )
    return [
        DefaultInfo(default_output = root),
        NixToolchainInfo(run = RunInfo(args = cmd_args("/bin/bash", ctx.attrs._run, root))),
    ]

nix_toolchain = rule(
    impl = _nix_toolchain_impl,
    attrs = {
        # The flake's files: what the output's store path is a function of.
        "srcs": attrs.list(attrs.source()),
        # The flake output, `packages.x86_64-linux.actions`.
        "output": attrs.string(),
        "_realize": attrs.source(default = "toolchains//:nix-realize.sh"),
        "_run": attrs.source(default = "toolchains//:nix-run.sh"),
    },
)
