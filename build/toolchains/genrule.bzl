"""The genrule toolchain: the prelude's system one, plus the Linux toolchain
(nix.bzl) as an execution dependency for Linux targets.

The toolchain gives genrules nothing they call. The dependency is there so
a Linux target's genrule runs on a Linux worker, as its compiles do: Buck2
otherwise sends an action to the first execution platform it fits, which on
a Mac is the Mac's, and the same Linux target would then have one key when
built on the build host and another when built from a Mac.
"""

load("@prelude//:genrule_toolchain.bzl", "GenruleToolchainInfo")
load(":nix.bzl", "NixToolchainInfo")

def _amux_genrule_toolchain_impl(_ctx):
    return [DefaultInfo(), GenruleToolchainInfo(zip_scrubber = None)]

amux_genrule_toolchain = rule(
    impl = _amux_genrule_toolchain_impl,
    attrs = {
        "nix": attrs.option(attrs.exec_dep(providers = [NixToolchainInfo]), default = None),
    },
    is_toolchain_rule = True,
)
