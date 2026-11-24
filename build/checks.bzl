"""The repository's own checks as Buck2 tests (docs/BUILD.md, "Repository checks").

Each check is a script that reads files of the checkout. As a test target
it runs in an action holding only the files it declares, at their paths in
the checkout, so its passing result is cached on exactly those files: a
change elsewhere doesn't run it again, in this tree or any other.

Where a check reads something other than the checkout's own files (an
earlier release's schema, the list of files the checkout tracks), the
`just` recipe that runs it writes that into build/checks/inputs/ first
(build/checks/inputs.py), and the target declares it like any other file.

A check is a Linux target while a Linux worker is in reach, so a Mac and
nova run the same action and share its result; with none in reach it runs
on the machine's own worker (`nova_only = False`), or is skipped and named
(`nova_only = True`, for a check whose tools only the Linux toolchain has).
"""

load("@prelude//decls:re_test_common.bzl", "re_test_common")
load("@prelude//tests:re_utils.bzl", "get_re_executors_from_props")
load("@toolchains//:nix.bzl", "NixToolchainInfo")
load("//build/platforms:defs.bzl", "host_platform")

def _check_impl(ctx: AnalysisContext) -> list[Provider]:
    inputs = list(ctx.attrs.srcs)
    for dep in ctx.attrs.deps:
        info = dep[DefaultInfo]
        inputs.extend(info.default_outputs)
        inputs.extend(info.other_outputs)

    # On Linux the command runs under the Linux toolchain (build/toolchains/
    # nix.bzl), whose bin directory comes first on PATH, so `python3`,
    # `buf`, `node` and `tmux` are the pinned ones. On a Mac's worker PATH
    # holds a modern python3, tmux and the system's tools
    # (scripts/nativelink.py); a check names any other tool it needs there.
    nix = ctx.attrs._nix[NixToolchainInfo].run if ctx.attrs._nix else None
    command = cmd_args(nix or [], "--" if nix else [], "/bin/sh", ctx.attrs._home, ctx.attrs.command, hidden = inputs)
    executors = get_re_executors_from_props(ctx)
    return [
        DefaultInfo(),
        # `buck2 run` runs it here, in the checkout, as the recipe does.
        RunInfo(args = cmd_args("/usr/bin/env", [cmd_args(k, "=", v, delimiter = "") for k, v in ctx.attrs.env.items()], ctx.attrs.command)),
        ExternalRunnerTestInfo(
            type = "custom",
            command = [command],
            env = ctx.attrs.env,
            labels = ctx.attrs.labels,
            default_executor = executors.default_executor,
            executor_overrides = executors.executor_overrides,
            run_from_project_root = True,
            use_project_relative_paths = True,
            supports_test_execution_caching = True,
        ),
    ]

_amux_check = rule(
    impl = _check_impl,
    attrs = {
        # The command, from the root of the checkout: `python3
        # scripts/docs-check.py`.
        "command": attrs.list(attrs.arg()),
        "env": attrs.dict(key = attrs.string(), value = attrs.arg(), sorted = False, default = {}),
        # Files the check reads, at their paths in the checkout.
        "srcs": attrs.list(attrs.source(), default = []),
        # Targets whose outputs it reads: another package's files
        # (`//crates/wire:files`), or a generated file it compares.
        "deps": attrs.list(attrs.dep(), default = []),
        "labels": attrs.list(attrs.string(), default = []),
        "_home": attrs.source(default = "root//build:checks/home.sh"),
        "_nix": attrs.option(attrs.exec_dep(providers = [NixToolchainInfo]), default = select({
            "DEFAULT": None,
            "prelude//os:linux": "root//:nix-linux",
        })),
    } | re_test_common.test_args(),
)

# Whether a Linux worker is in reach (scripts/buck-config.py writes it).
_LINUX = read_root_config("amux", "linux", "true") == "true"

LINUX_PLATFORM = "root//build/platforms:linux-cli-testing-dev"

def check_platform(nova_only = False) -> str:
    """The configuration a check, or the JavaScript's targets, build in:
    Linux while a Linux worker is in reach, and the machine's own
    otherwise, unless only a Linux worker can run it."""
    if nova_only or _LINUX:
        return LINUX_PLATFORM
    return host_platform(purpose = "testing")

def amux_check(name, command, srcs = [], deps = [], env = {}, labels = [], nova_only = False, host = False, **kwargs):
    """A repository check: `command` run from the checkout's root over
    `srcs` (this package's files) and `deps` (other targets' outputs),
    failing when the command does. `nova_only = True` for a check whose
    tools only the Linux toolchain has: it is skipped, and named, when no
    Linux worker is in reach. `host = True` for one that checks how amux
    behaves on the machine it runs on, which builds in that machine's
    configuration as the Rust tests do (the terminal journeys)."""
    _amux_check(
        name = name,
        command = command,
        srcs = srcs,
        deps = deps,
        env = env,
        labels = ["check"] + labels + (["nova-only"] if nova_only else []),
        default_target_platform = host_platform(purpose = "testing") if host else check_platform(nova_only),
        target_compatible_with = ["root//build/constraints:linux-workers"] if nova_only else [],
        **kwargs
    )
