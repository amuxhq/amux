"""Macros for the workspace's own Rust crates (docs/BUILD.md, "Rust").

Each crate's BUCK file sits beside its Cargo.toml and says the same things
in Buck's terms: its library, binaries and tests, their dependencies and
features. The two are kept in step by hand; the parity check
(//build/checks:parity) fails when they disagree.

Naming: a crate's library target has the crate's own name
(`//crates/wire:wire`), its unit tests are `:unit`, each integration test
binary is named after its `[[test]]`, and each binary after its `[[bin]]`.

Sources are laid out as they sit in the checkout. Cargo compiles a crate in
place, so a relative `include_str!("../../../justfile")` reaches the
checkout's root; Buck compiles in a sandbox holding only what the target
declares. Each target's sources are therefore a directory shaped like the
checkout: the crate's files under `crates/<name>/`, and any file of another
package it includes at compile time (`include`, a map from that file's
target to its path in the checkout) at its own path.

At run time a test finds its files and the crate's binaries through the
source-tree crate (`crate_dir!()`, `bin!()`), which reads what these macros
set: `AMUX_CRATE_DIR` when it compiles, `CARGO_BIN_EXE_<name>` when it runs.
A test's `resources` are the crate's own files plus any `test_resources`
(targets such as `//crates/interpret:fixtures`), present at their paths in
the checkout when it runs.

A test binary is built as a binary compiled with `--test` and run by
build/test-runner.py, which gives each of its tests a process of its own as
cargo-nextest does: `buck2 test` reports one result per binary, and its
output names each test that failed.
"""

load("@prelude//decls:re_test_common.bzl", "re_test_common")
load("@prelude//rust:sources.bzl", "RustSources", "RustSourcesTSet")
load("@prelude//tests:re_utils.bzl", "get_re_executors_from_props")
load("//build/platforms:defs.bzl", "EXEC_RUNS_TARGET", "MODES", "TARGETS", "host_platform")
load("@toolchains//:nix.bzl", "NixToolchainInfo")

# Cargo.toml's [profile.dev.package.*]: crates the model checker runs
# through, optimised in every build (`rustc_flags = OPTIMISED`).
OPTIMISED = ["-Copt-level=3"]

TESTING = "root//build/constraints:testing"
APP = "root//build/constraints:app"

# The phone bridges' in-app debug tools: in every build but a release.
DEBUG_TOOLS = select({
    "DEFAULT": ["debug-tools"],
    "root//build/constraints:release": [],
})

def _srcs_impl(ctx: AnalysisContext) -> list[Provider]:
    tree = ctx.actions.symlinked_dir("srcs", {path: src for src, path in ctx.attrs.srcs.items()})
    return [
        DefaultInfo(default_output = tree),
        RustSources(tset = ctx.actions.tset(RustSourcesTSet, value = tree, children = [])),
    ]

# A target's sources at their paths in the checkout (the module docstring).
_rust_srcs = rule(
    impl = _srcs_impl,
    attrs = {"srcs": attrs.dict(key = attrs.source(), value = attrs.string(), sorted = False)},
)

def _files_impl(ctx: AnalysisContext) -> list[Provider]:
    # Named by its package too: a test's resources are keyed by name, and
    # many packages have a `files` target.
    listing = ctx.actions.write(
        "/".join([p for p in [ctx.label.package, ctx.label.name + ".files"] if p]),
        [src.short_path for src in ctx.attrs.srcs],
    )
    return [DefaultInfo(default_output = listing, other_outputs = ctx.attrs.srcs)]

_files = rule(
    impl = _files_impl,
    attrs = {"srcs": attrs.list(attrs.source())},
)

def amux_files(name, srcs, visibility = ["PUBLIC"]):
    """Files of this package a test of another reads at run time, as one
    target for its `test_resources`. They are present at their paths in the
    checkout when the test runs."""
    _files(name = name, srcs = srcs, visibility = visibility)

def _rust_files():
    """Every Rust file of the crate, its tests', examples' and build
    script's among them, as `:rust-files`, for a check that reads them all
    (`//:platform-check`). Once per crate, by whichever macro comes first."""
    if not native.rule_exists("rust-files"):
        amux_files(name = "rust-files", srcs = native.glob(["**/*.rs"]))

def _binaries_impl(ctx: AnalysisContext) -> list[Provider]:
    # Each under its name with the binary's own extension, `.exe` for
    # Windows, as Cargo leaves them: provider_fakes::binaries adds it, and
    # Windows finds a program named without one only by adding it too.
    binaries = {
        path + dep[DefaultInfo].default_outputs[0].extension: dep[DefaultInfo].default_outputs[0]
        for path, dep in ctx.attrs.binaries.items()
    }
    # Copies on Windows: a worker there materializes the links as Windows
    # symbolic links with the forward-slashed relative targets Buck2 writes,
    # which Windows doesn't resolve, so a test can't start a program
    # through one.
    tree = (ctx.actions.copied_dir if ctx.attrs._windows else ctx.actions.symlinked_dir)("binaries", binaries)
    return [DefaultInfo(default_output = tree)]

_PLATFORMS = "root//build/platforms:"

# The binaries a test starts are a development build of the desktop
# product with test features, for the system the test runs on. That is
# exactly the configuration of a desktop test (`macos-cli-testing-dev`), so
# they take that configuration's name, and a crate both a test and the
# binaries link compiles once. As execution dependencies they had the
# execution platform's (`exec-macos`), whose constraints are the same but
# whose output paths are not, so the whole workspace compiled twice for
# every test run, three times with the phone bridge's `app` tests.
_TEST_BINARIES_PLATFORM = {
    _PLATFORMS + "{}-{}-{}-{}".format(target, product, purpose, mode): _PLATFORMS + "{}-cli-testing-dev".format(target)
    for target in TARGETS
    for product in ["cli", "app"]
    for purpose in ["shipping", "testing"]
    for mode in MODES
}

def _test_binaries_platform_impl(platform: PlatformInfo, refs: struct) -> PlatformInfo:
    constraints = dict(platform.configuration.constraints)
    for value in [refs.cli[ConstraintValueInfo], refs.testing[ConstraintValueInfo], refs.dev[ConstraintValueInfo]]:
        constraints[value.setting.label] = value
    if constraints == platform.configuration.constraints:
        return platform
    return PlatformInfo(
        label = _TEST_BINARIES_PLATFORM.get(platform.label, platform.label + "-cli-testing-dev"),
        configuration = ConfigurationInfo(constraints = constraints, values = platform.configuration.values),
    )

_test_binaries_platform = transition(
    impl = _test_binaries_platform_impl,
    refs = {
        "cli": "root//build/constraints:cli",
        "dev": "root//build/constraints:dev",
        "testing": TESTING,
    },
)

# The binaries the integration suites start, in one directory, as
# scripts/test-binaries.sh builds them for nextest: provider-fakes'
# `binaries` module finds them through TESTNET_BINARIES. They are built for
# the system the test runs on, as the desktop product with test features,
# whatever the test itself builds as:
# for a Linux test, Linux, and for a Windows test, Windows, wherever the
# build was started.
_amux_binaries = rule(
    impl = _binaries_impl,
    attrs = {
        "binaries": attrs.dict(key = attrs.string(), value = attrs.transition_dep(cfg = _test_binaries_platform), sorted = False),
        "_windows": attrs.bool(default = select({"DEFAULT": False, "prelude//os:windows": True})),
    },
)

def amux_binaries(**kwargs):
    _amux_binaries(exec_compatible_with = EXEC_RUNS_TARGET, **kwargs)

# The binaries in that directory (//crates/provider-fakes:binaries), by the
# name provider_fakes::binaries finds each under.
TEST_BINARIES = {
    "amux": "//crates/amux:amux",
    "examples/amux_stand_in": "//crates/agent:amux_stand_in",
    "fake-acp": "//crates/provider-fakes:fake-acp",
    "fake-amux": "//crates/fake-amux:fake-amux",
    "fake-claude-pty": "//crates/provider-fakes:fake-claude-pty",
    "fake-claude-sdk": "//crates/provider-fakes:fake-claude-sdk",
    "fake-codex": "//crates/provider-fakes:fake-codex",
    "fake-provider": "//crates/provider-fakes:fake-provider",
}

# What a test that starts those binaries runs with (`binaries = True`).
_BINARIES_ENV = {"TESTNET_BINARIES": "$(location //crates/provider-fakes:binaries)"}

# `env KEY=VALUE... COMMAND...` in Python: the leading words with an `=` in
# them set the environment, and the rest is the command, whose exit status
# is this one's.
_SET_ENV = "; ".join([
    "import os, subprocess, sys",
    "n = next(i for i, a in enumerate(sys.argv[1:] + [''], 1) if '=' not in a)",
    "os.environ.update(a.split('=', 1) for a in sys.argv[1:n])",
    "sys.exit(subprocess.call(sys.argv[n:]))",
])

def _test_impl(ctx: AnalysisContext) -> list[Provider]:
    # A test with a `main` of its own (Cargo's `harness = false`) is a
    # program, run as it is.
    #
    # A Linux test runs under the Linux toolchain (build/toolchains/nix.bzl):
    # its Python runs the runner, and its bin directory is first on the
    # PATH the test and what it starts (tmux, git) see.
    nix = ctx.attrs._nix[NixToolchainInfo].run if ctx.attrs._nix else None
    command = cmd_args(nix or [], "--" if nix else [], ctx.attrs.binary[RunInfo]) if not ctx.attrs.harness else cmd_args(
        nix or [],
        "bin/python3" if nix else ctx.attrs._python,
        ctx.attrs._runner,
        "--threads",
        str(ctx.attrs.threads),
        "--slow",
        str(ctx.attrs.slow),
        "--terminate-after",
        str(ctx.attrs.terminate_after),
        "--",
        ctx.attrs.binary[RunInfo],
        ctx.attrs.filters,
    )
    executors = get_re_executors_from_props(ctx)
    # `buck2 run` (how `just goldens` re-records) runs the runner here, in
    # the checkout, so its environment rides on the command line, as the
    # KEY=VALUE words `env` takes (`_set_env`).
    run = cmd_args(
        ctx.attrs._set_env,
        [cmd_args(key, "=", value, delimiter = "") for key, value in ctx.attrs.env.items()],
        command,
    )
    return [
        DefaultInfo(default_output = ctx.attrs.binary[DefaultInfo].default_outputs[0]),
        RunInfo(args = run),
        ExternalRunnerTestInfo(
            type = "custom",
            command = [command],
            env = ctx.attrs.env,
            labels = ctx.attrs.labels,
            default_executor = executors.default_executor,
            executor_overrides = executors.executor_overrides,
            run_from_project_root = True,
            use_project_relative_paths = True,
            # A passing result is kept in NativeLink's action cache, so the
            # same test on the same inputs isn't run again, in this tree or
            # another. Failures aren't kept. The live lanes check the world
            # outside the repository, which no input describes, so they
            # always run.
            supports_test_execution_caching = "live" not in ctx.attrs.labels,
        ),
    ]

_amux_test = rule(
    impl = _test_impl,
    attrs = {
        "binary": attrs.dep(providers = [RunInfo]),
        "env": attrs.dict(key = attrs.string(), value = attrs.arg(), sorted = False, default = {}),
        # The test runner's name filters: only the tests they select run.
        "filters": attrs.list(attrs.string(), default = []),
        "harness": attrs.bool(default = True),
        "labels": attrs.list(attrs.string(), default = []),
        # .config/nextest.toml's bounds: slow after a minute, ended at three.
        "slow": attrs.int(default = 60),
        "terminate_after": attrs.int(default = 3),
        # Four cases at a time; two on Windows, where the worker runs one
        # test binary per core (build/toolchains/BUCK).
        "threads": attrs.int(default = select({
            "DEFAULT": 4,
            "prelude//os:windows": 2,
        })),
        "_runner": attrs.source(default = "root//build:test-runner.py"),
        # What sets a `buck2 run`'s environment and starts its command:
        # `env`, or on Windows, which has none, the runner's Python doing
        # what `env` does.
        "_set_env": attrs.list(attrs.string(), default = select({
            "DEFAULT": ["/usr/bin/env"],
            "prelude//os:windows": ["python", "-c", _SET_ENV],
        })),
        # The Python the runner runs under, by the name the worker's PATH
        # has it: Windows' installers make no python3.
        "_python": attrs.string(default = select({
            "DEFAULT": "python3",
            "prelude//os:windows": "python",
        })),
        "_nix": attrs.option(attrs.exec_dep(providers = [NixToolchainInfo]), default = select({
            "DEFAULT": None,
            "prelude//os:linux": "root//:nix-linux",
        })),
    } | re_test_common.test_args(),
)

def _test(name, env, run_env, labels, product, timeouts, whole_worker = False, harness = True, linux_only = False, runs = {}, **binary):
    """A test binary `name` built from `binary` (rust_binary's attributes),
    run by the test runner with `run_env` and the compile-time `env`, as
    the prelude's rust_test runs with both. Each of `runs` is another test
    target running the same binary: its name to a dict of the `env` it adds
    and the `filters` that select its tests."""
    rustc_flags = (["--test"] if harness else []) + binary.pop("rustc_flags", [])
    platform = host_platform(product = product, purpose = "testing")
    compatible = []
    if linux_only:
        # Built and run for Linux from any machine, so a Mac sends it to
        # the build host's worker; skipped when no Linux worker is in reach.
        platform = "root//build/platforms:linux-{}-testing-dev".format(product)
        compatible = ["root//build/constraints:linux-workers"]
        labels = labels + ["linux-only"]
    native.rust_binary(
        name = name + "-binary",
        env = env,
        rustc_flags = rustc_flags,
        default_target_platform = platform,
        target_compatible_with = compatible,
        visibility = [],
        **binary
    )
    _amux_test(
        name = name,
        binary = ":" + name + "-binary",
        env = env | run_env,
        harness = harness,
        labels = labels,
        remote_execution = "whole-worker" if whole_worker else None,
        default_target_platform = platform,
        target_compatible_with = compatible,
        **timeouts
    )
    for run, how in runs.items():
        _amux_test(
            name = run,
            binary = ":" + name + "-binary",
            env = env | run_env | how.get("env", {}),
            filters = how.get("filters", []),
            harness = harness,
            labels = labels + how.get("labels", []),
            remote_execution = "whole-worker" if whole_worker else None,
            default_target_platform = platform,
            target_compatible_with = compatible,
            **timeouts
        )

def _crate_name() -> str:
    return native.package_name().split("/")[-1]

def _tree(name, files, include):
    """The `srcs_filegroup` of target `name` (a crate's library and binary
    may share a name, so it carries the kind too): `files` of this package,
    by their paths in it, and `include`, cross-package files by their paths
    in the checkout."""
    package = native.package_name()
    mapped = {f: package + "/" + f for f in files}
    mapped.update(include)
    _rust_srcs(name = name + "-srcs", srcs = mapped)
    return ":" + name + "-srcs"

def _env(crate: str, version: str, extra: dict) -> dict:
    package = native.package_name()
    env = {
        "AMUX_CRATE_DIR": package,
        "CARGO_CRATE_NAME": crate.replace("-", "_"),
        # Within the target's sources, which are laid out as in the checkout.
        "CARGO_MANIFEST_DIR": package,
        "CARGO_PKG_NAME": crate,
        "CARGO_PKG_VERSION": version,
    }
    env.update(extra)
    return env

def _package_files():
    return native.glob(["**"], exclude = ["BUCK"])

def amux_rust_library(
        version,
        deps = [],
        features = [],
        srcs = None,
        include = {},
        crate_root = "src/lib.rs",
        proc_macro = False,
        env = {},
        rustc_flags = [],
        test_deps = [],
        test_srcs = [],
        test_include = {},
        test_env = {},
        test_run_env = {},
        test_resources = [],
        binaries = False,
        test_timeouts = {},
        test_whole_worker = False,
        unit_tests = True,
        tool = False,
        product = "cli",
        visibility = ["PUBLIC"],
        **kwargs):
    """A crate's library, and its unit tests unless `unit_tests = False`.

    `deps` are Cargo's [dependencies]; `test_deps` its [dev-dependencies]
    (added to the unit tests; integration tests list their own).
    `binaries = True` when the unit tests start amux or a fake provider. `srcs`
    defaults to everything under src/; `test_srcs` adds this package's files
    the unit tests include at compile time, `test_env` what they compile
    with and `test_run_env` what they run with. `product` is the
    configuration the library builds in by default: "app" for the client
    bridges. Its unit tests build as the desktop product.
    `tool = True` for a test harness's or build tool's library, which
    builds with test features on, as its binaries do.
    """
    crate = _crate_name()
    package = native.package_name()
    files = srcs if srcs != None else native.glob(["src/**"])
    common = dict(
        crate = crate.replace("-", "_"),
        crate_root = package + "/" + crate_root,
        edition = "2024",
        features = features,
        rustc_flags = rustc_flags,
    )
    # The sources, for another crate's test that reads them.
    amux_files(name = "files", srcs = files)
    _rust_files()
    native.rust_library(
        name = crate,
        srcs_filegroup = _tree(crate + ".lib", files, include),
        deps = deps,
        proc_macro = proc_macro,
        env = _env(crate, version, env),
        default_target_platform = host_platform(product = product, purpose = "testing" if tool else "shipping"),
        visibility = visibility,
        **(common | kwargs)
    )
    if unit_tests and not proc_macro:
        _test(
            name = "unit",
            env = _env(crate, version, env | test_env),
            run_env = test_run_env | (_BINARIES_ENV if binaries else {}),
            labels = [],
            # As the desktop product, whatever the library ships as: the
            # app product differs only in settings' clap, and testing the
            # client bridges as the app compiled their whole dependency
            # graph a second time (95 crates, 992 s of a cold macOS run).
            product = "cli",
            timeouts = test_timeouts,
            whole_worker = test_whole_worker,
            srcs_filegroup = _tree("unit.test", files + test_srcs, include | test_include),
            deps = deps + test_deps,
            resources = _package_files() + test_resources,
            crate = common["crate"],
            crate_root = common["crate_root"],
            edition = common["edition"],
            features = common["features"],
            rustc_flags = rustc_flags,
        )

def amux_rust_test(
        name,
        version,
        crate_root = None,
        srcs = None,
        include = {},
        deps = [],
        features = [],
        env = {},
        run_env = {},
        resources = [],
        product = "cli",
        labels = [],
        link_library = True,
        binaries = False,
        timeouts = {},
        whole_worker = False,
        harness = True,
        linux_only = False,
        runs = {},
        **kwargs):
    """One integration-test binary: Cargo's `[[test]] name = ...`, which
    `autotests = false` crates keep to one per crate, built from
    tests/<name>.rs and the modules beside it. `run_env` is set when it
    runs (`CARGO_BIN_EXE_<bin>` for each of the crate's binaries);
    `resources` adds other packages' files to the crate's own.
    `link_library = False` in a crate with no library; `binaries = True`
    when it starts amux or a fake provider. `timeouts` overrides the
    runner's `slow`, `terminate_after` and `threads`; `whole_worker = True`
    reserves every core of the worker for a test that uses them all.
    `harness = False` for Cargo's `harness = false`: a test with its own
    `main`, run as a program rather than by the test runner. `linux_only =
    True` for a test too heavy for a laptop's worker: it is always built and
    run for Linux, on the build host's worker, and is skipped when that
    host is out of reach (label `linux-only`). `runs` names other test
    targets that run the same binary differently: a name to the `env` each
    adds, the `filters` that select its tests (build/test-runner.py's) and
    its `labels`."""
    crate = _crate_name()
    package = native.package_name()
    _rust_files()
    _test(
        name = name,
        harness = harness,
        env = _env(crate, version, env),
        run_env = run_env | (_BINARIES_ENV if binaries else {}),
        labels = labels,
        product = product,
        timeouts = timeouts,
        whole_worker = whole_worker,
        linux_only = linux_only,
        runs = runs,
        crate = name.replace("-", "_"),
        crate_root = package + "/" + (crate_root or "tests/{}.rs".format(name)),
        srcs_filegroup = _tree(name + ".test", srcs if srcs != None else native.glob(["tests/**"]), include),
        edition = "2024",
        deps = ([":" + crate] if link_library else []) + deps,
        features = features,
        resources = _package_files() + resources,
        **kwargs
    )

def amux_rust_binary(
        name,
        version,
        crate_root,
        srcs = None,
        include = {},
        deps = [],
        features = [],
        env = {},
        tool = False,
        link_library = True,
        product = "cli",
        unit_tests = False,
        test_deps = [],
        test_resources = [],
        binaries = False,
        visibility = ["PUBLIC"],
        **kwargs):
    """A crate's `[[bin]]` (or `[[example]]`). `tool = True` for test
    harnesses and build tools, which build with test features on; the
    product's binaries build as they ship. `link_library = False` for a
    binary in a crate with no library. A binary named after its crate's
    library is the target `<name>-bin`, since the library has the name.
    `unit_tests = True` for a binary with tests of its own, which Cargo
    runs as the binary's tests: they are the target `<name>-unit`, with
    `test_deps`, `test_resources` and `binaries` as a library's."""
    crate = _crate_name()
    package = native.package_name()
    _rust_files()
    if srcs == None:
        top = crate_root.split("/")[0]
        srcs = native.glob([top + "/**"])
    native.rust_binary(
        name = name + "-bin" if link_library and name == crate else name,
        crate = name.replace("-", "_"),
        crate_root = package + "/" + crate_root,
        srcs_filegroup = _tree(name + ".bin", srcs, include),
        edition = "2024",
        deps = ([":" + crate] if link_library else []) + deps,
        features = features,
        env = _env(crate, version, env),
        default_target_platform = host_platform(product = product, purpose = "testing" if tool else "shipping"),
        visibility = visibility,
        **kwargs
    )
    if unit_tests:
        _test(
            name = name + "-unit",
            env = _env(crate, version, env),
            run_env = _BINARIES_ENV if binaries else {},
            labels = [],
            product = product,
            timeouts = {},
            crate = name.replace("-", "_"),
            crate_root = package + "/" + crate_root,
            srcs_filegroup = _tree(name + ".unit", srcs, include),
            edition = "2024",
            deps = ([":" + crate] if link_library else []) + deps + test_deps,
            features = features,
            resources = _package_files() + test_resources,
        )
