"""The browser extension's JavaScript as Buck2 targets (js/BUCK, docs/BROWSER.md).

`js_action` runs build/js.py over the files it declares, at their paths in
the checkout, and keeps what the command writes: the installed npm packages,
or the extension built around browser-core's WebAssembly. The JavaScript's
checks and tests are repository checks (build/checks.bzl) that run the same
script.

Like the checks, these build for Linux while a Linux worker is in reach and
for the machine's own worker otherwise (`check_platform`). On Linux, node
and npm are the Linux toolchain's; on a Mac, the release `//build:node`
pins, which `NODE_BIN` names.
"""

load("@toolchains//:nix.bzl", "NixToolchainInfo")
load("//build:checks.bzl", "amux_check", "check_platform")

def _js_action_impl(ctx: AnalysisContext) -> list[Provider]:
    out = ctx.actions.declare_output(ctx.attrs.out, dir = ctx.attrs.directory)
    inputs = list(ctx.attrs.srcs)
    for dep in ctx.attrs.deps:
        inputs.extend(dep[DefaultInfo].default_outputs)
        inputs.extend(dep[DefaultInfo].other_outputs)
    env = dict(ctx.attrs.env)
    for name, dep in ctx.attrs.env_deps.items():
        env[name] = cmd_args(dep[DefaultInfo].default_outputs[0])
    nix = ctx.attrs._nix[NixToolchainInfo].run if ctx.attrs._nix else None
    command = cmd_args(
        nix or [],
        "--" if nix else [],
        "python3",
        ctx.attrs._script,
        ctx.attrs.args,
        out.as_output(),
        ctx.attrs.command,
        hidden = inputs,
    )
    ctx.actions.run(command, env = env, category = "js", identifier = ctx.label.name)
    return [DefaultInfo(default_output = out)]

_js_action = rule(
    impl = _js_action_impl,
    attrs = {
        # build/js.py's arguments before the output (`install`, or `run
        # MODULES --collect extension/dist-dev`) and the command after it.
        "args": attrs.list(attrs.arg()),
        "command": attrs.list(attrs.arg(), default = []),
        "out": attrs.string(),
        "directory": attrs.bool(default = False),
        "env": attrs.dict(key = attrs.string(), value = attrs.arg(), sorted = False, default = {}),
        # What env names, built in a configuration of its own (`(target,
        # platform)`): browser-core's WebAssembly, built as the extension
        # ships it.
        "env_deps": attrs.dict(key = attrs.string(), value = attrs.configured_dep(), sorted = False, default = {}),
        "srcs": attrs.list(attrs.source(), default = []),
        "deps": attrs.list(attrs.dep(), default = []),
        "_script": attrs.source(default = "root//build:js.py"),
        "_nix": attrs.option(attrs.exec_dep(providers = [NixToolchainInfo]), default = select({
            "DEFAULT": None,
            "prelude//os:linux": "root//:nix-linux",
        })),
    },
)

def js_action(name, **kwargs):
    _js_action(name = name, default_target_platform = check_platform(), **kwargs)

def js_check(name, command, srcs, deps = [], labels = [], **kwargs):
    """A check or test of the JavaScript: `command` run in js/ with the npm
    packages of `:node_modules` installed there."""
    amux_check(
        name = name,
        command = ["python3", "build/js.py", "run", "$(location :node_modules)", "--"] + command,
        srcs = srcs,
        deps = [":node_modules", "//build:js.py"] + deps,
        labels = ["js"] + labels,
        **kwargs
    )
