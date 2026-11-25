"""The workspace as Cargo reads it, for the checks that run Cargo (docs/BUILD.md, "Repository checks").

Cargo reads a workspace as a whole: every member's manifest, which files
its targets are (a `tests/*.rs` is a test because it is there), and, for
anything past `--no-deps`, every dependency's own manifest from its
registry. A check that runs Cargo is a Buck2 action like the others, on a
Linux worker with the Linux toolchain's Cargo, so these targets give it
exactly that:

- each crate's `:cargo-package`, its manifest, Rust files and README, which the
  crate macros (build/rust.bzl) declare;
- `cargo_workspace`, the members together with the workspace's own files,
  and a list of every path among them. With `sources = False` it carries
  only the manifests and the list, and the action makes an empty file for
  each path it lacks (build/checks/cargo.sh): enough for Cargo to tell the
  targets apart, so an action that needs only the graph reruns when a
  manifest changes or a file is added or removed, not on every edit;
- `cargo_output`, an action that writes a file or directory with Cargo:
  the registry every dependency comes from (`cargo fetch`, the one action
  here that reaches the network), or Cargo's metadata.
"""

load("@toolchains//:nix.bzl", "NixToolchainInfo")

CargoPackageInfo = provider(fields = {
    "manifest": provider_field(Artifact),
    "files": provider_field(list[Artifact]),
})

def _cargo_package_impl(ctx: AnalysisContext) -> list[Provider]:
    return [DefaultInfo(), CargoPackageInfo(manifest = ctx.attrs.manifest, files = ctx.attrs.srcs)]

_cargo_package = rule(
    impl = _cargo_package_impl,
    attrs = {
        "manifest": attrs.source(),
        "srcs": attrs.list(attrs.source()),
    },
)

def amux_cargo_package():
    """This package as Cargo reads it, as `:cargo-package`: its Cargo.toml,
    every Rust file under it and its README. Once per package, whichever crate macro
    asks first."""
    if native.rule_exists("cargo-package"):
        return
    _cargo_package(
        name = "cargo-package",
        manifest = "Cargo.toml",
        # A README beside the manifest is the package's readme to Cargo.
        srcs = native.glob(["**/*.rs", "README", "README.md", "README.txt"]),
        visibility = ["PUBLIC"],
    )

def _cargo_workspace_impl(ctx: AnalysisContext) -> list[Provider]:
    packages = [dep[CargoPackageInfo] for dep in ctx.attrs.packages]
    manifests = [package.manifest for package in packages]
    files = [file for package in packages for file in package.files]
    # Every path, one a line, relative to the checkout's root.
    listing = ctx.actions.write(
        ctx.label.name + ".paths",
        cmd_args(ctx.attrs.srcs + manifests + files),
    )
    carried = ctx.attrs.srcs + manifests + (files if ctx.attrs.sources else [])
    return [DefaultInfo(default_output = listing, other_outputs = carried)]

_cargo_workspace = rule(
    impl = _cargo_workspace_impl,
    attrs = {
        # The workspace's own files: the root manifest, the lockfile.
        "srcs": attrs.list(attrs.source(), default = []),
        # Each member's `:cargo-package`.
        "packages": attrs.list(attrs.dep(providers = [CargoPackageInfo])),
        # Whether the members' Rust files come too, or only their paths.
        "sources": attrs.bool(),
    },
)

def _cargo_output_impl(ctx: AnalysisContext) -> list[Provider]:
    out = ctx.actions.declare_output(ctx.attrs.out, dir = ctx.attrs.directory)
    inputs = list(ctx.attrs.srcs)
    for dep in ctx.attrs.deps:
        info = dep[DefaultInfo]
        inputs.extend(info.default_outputs)
        inputs.extend(info.other_outputs)
    command = cmd_args(
        ctx.attrs._nix[NixToolchainInfo].run,
        "--",
        "/bin/sh",
        ctx.attrs._home,
        ctx.attrs.command,
        hidden = inputs,
    )
    ctx.actions.run(
        command,
        env = {"OUT": out.as_output()},
        category = "cargo",
        identifier = ctx.label.name,
    )
    return [DefaultInfo(default_output = out)]

_cargo_output = rule(
    impl = _cargo_output_impl,
    attrs = {
        # Run from the checkout's root, under the Linux toolchain, with
        # `$OUT` naming the file or directory it writes.
        "command": attrs.list(attrs.arg()),
        "out": attrs.string(),
        "directory": attrs.bool(default = False),
        "srcs": attrs.list(attrs.source(), default = []),
        "deps": attrs.list(attrs.dep(), default = []),
        "_home": attrs.source(default = "root//build:checks/home.sh"),
        "_nix": attrs.exec_dep(providers = [NixToolchainInfo], default = "root//:nix-linux"),
    },
)

# Cargo here is the Linux toolchain's, so these build for Linux, and with
# no Linux worker in reach they are skipped, as the checks that read them
# are (build/checks.bzl's `linux_only`).
_LINUX = dict(
    default_target_platform = "root//build/platforms:linux-cli-testing-dev",
    target_compatible_with = ["root//build/constraints:linux-workers"],
)

def cargo_workspace(name, packages, sources, srcs = [], **kwargs):
    """The workspace's members (`packages`, each a `:cargo-package`) and
    its own files (`srcs`). Its output lists every path; it carries the
    members' Rust files only with `sources = True`."""
    _cargo_workspace(name = name, packages = packages, sources = sources, srcs = srcs, **(_LINUX | kwargs))

def cargo_output(name, command, out, directory = False, srcs = [], deps = [], **kwargs):
    """A file (or, with `directory = True`, a directory) that `command`
    writes at `$OUT`, run from the checkout's root on a Linux worker under
    the Linux toolchain over `srcs` and `deps`' outputs."""
    _cargo_output(name = name, command = command, out = out, directory = directory, srcs = srcs, deps = deps, **(_LINUX | kwargs))
