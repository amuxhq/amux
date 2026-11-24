"""The interpreter the prelude's bootstrap Python tools run under: the
system's python3, through bootstrap-guard.py, which fails a Rust compile the
prelude's failure filter would report as a success (that script says why)."""

load("@prelude//os_lookup:defs.bzl", "Os", "OsLookup")
load("@prelude//python_bootstrap:toolchain.bzl", "PythonBootstrapToolchainInfo")

def _amux_python_bootstrap_toolchain_impl(ctx):
    # The interpreter the exec platform has, as the prelude's system
    # toolchain picks it.
    python = "python.exe" if ctx.attrs._exec_os_type[OsLookup].os == Os("windows") else "python3"
    return [
        DefaultInfo(),
        PythonBootstrapToolchainInfo(interpreter = cmd_args(python, ctx.attrs.guard)),
    ]

amux_python_bootstrap_toolchain = rule(
    impl = _amux_python_bootstrap_toolchain_impl,
    attrs = {
        "guard": attrs.source(),
        "_exec_os_type": attrs.default_only(attrs.exec_dep(default = "prelude//os_lookup/targets:os_lookup")),
    },
    is_toolchain_rule = True,
)
