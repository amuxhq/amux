"""Macros for the Android app's modules (docs/ANDROID.md, "Building").

Each module under apps/android/ is one BUCK file: a pure-JVM module is a
`kotlin_library` with JUnit tests, a module with Android code an
`android_library` (its resources an `android_resource`) with Robolectric
tests. The prelude's Java, Kotlin and Android toolchains build them
(build/toolchains/BUCK), every action on a Linux worker.

Configuration: the Kotlin and Android code is built in one configuration,
`linux-app-shipping-dev`, wherever it ends up. The prelude's own build
tools are built in the same configuration as what they build, and choose
their dex toolchain by OS: under an Android OS they would dex themselves
with the d8 they are building. JVM bytecode doesn't depend on the target,
so the Linux configuration costs nothing and the Robolectric tests, which
run on the Linux JVM, share every library build with the app. Only the
Rust bridge is built for Android (`amux_jni_library`).

Every target here is `linux-only`: Android builds and tests only on a Linux
worker, so with none in reach they are skipped and named, never built on
the Mac.
"""

load("@prelude//decls:re_test_common.bzl", "re_test_common")
load("@prelude//tests:re_utils.bzl", "get_re_executors_from_props")
load("@toolchains//:nix.bzl", "NixToolchainInfo")
load("//build:rust.bzl", "amux_files")
load("@prelude//decls:toolchains_common.bzl", "toolchains_common")
load("@prelude//java:java_toolchain.bzl", "JavaToolchainInfo")

PLATFORM = "root//build/platforms:linux-app-shipping-dev"

# Labels and compatibility for a target that runs only on a Linux worker
# (build/rust.bzl's `linux_only`).
LINUX_ONLY = ["linux-only"]
LINUX_WORKERS = ["root//build/constraints:linux-workers"]

# For a target whose rule has no toolchain to send it to Linux (a genrule,
# the rules below): its actions run on a Linux worker, not the Mac.
LINUX_EXEC = ["prelude//os:linux"]

COMPOSE_PLUGIN = "//third-party/android:kotlin-compose-compiler-plugin"
SERIALIZATION_PLUGIN = "//third-party/android:kotlin-serialization-compiler-plugin"

# The Android framework Robolectric runs tests against, offline.
ANDROID_ALL = "//third-party/android:org.robolectric_android-all-instrumented-file"

# The bytecode the modules compile to, as Gradle's `jvmToolchain(21)`.
JAVA_VERSION = "21"

# Classes the Compose compiler treats as stable (the file says why).
_STABILITY = "//apps/android:compose-stability.conf"

# The prelude's kotlincd toolchain says its Kotlin is 2.2 (whatever compiler
# it carries) and asks for that language version; the app is written to the
# compiler's own, as Gradle compiled it.
_LANGUAGE = ["-language-version=2.3"]

def _kotlin_args(compose, stability):
    if not (compose and stability):
        return _LANGUAGE
    return _LANGUAGE + [
        "-P",
        "plugin:androidx.compose.compiler.plugins.kotlin:stabilityConfigurationPath=$(location {})".format(_STABILITY),
    ]

def _plugins(compose, serialization):
    plugins = []
    if compose:
        plugins.append((COMPOSE_PLUGIN, {}))
    if serialization:
        plugins.append((SERIALIZATION_PLUGIN, {}))
    return plugins

def _common():
    return dict(
        default_target_platform = PLATFORM,
        target_compatible_with = LINUX_WORKERS,
        labels = LINUX_ONLY,
    )

def _sources():
    # The module's sources and resources as the writing lints read them
    # (//apps/android:feature-lint and the rest), once per package.
    if not native.rule_exists("sources"):
        amux_files(name = "sources", srcs = native.glob(["src/**"]))

def amux_jvm_library(
        name,
        srcs = None,
        deps = [],
        exported_deps = [],
        serialization = False,
        test_deps = [],
        test_resources = [],
        test_resources_root = None,
        visibility = ["PUBLIC"]):
    """A pure-JVM module: Kotlin under src/main/kotlin, its JUnit tests
    under src/test/kotlin as `:test`."""
    _sources()
    native.kotlin_library(
        name = name,
        srcs = srcs if srcs != None else native.glob(["src/main/kotlin/**/*.kt"]),
        deps = deps,
        exported_deps = exported_deps,
        kotlin_compiler_plugins = _plugins(False, serialization),
        extra_kotlinc_arguments = _LANGUAGE,
        java_version = JAVA_VERSION,
        visibility = visibility,
        **_common()
    )
    tests = native.glob(["src/test/kotlin/**/*.kt"])
    if tests:
        native.kotlin_test(
            name = "test",
            srcs = tests,
            deps = [":" + name] + deps + exported_deps + test_deps + ["//third-party/android:junit"],
            # Tests see the module's `internal` declarations, as Gradle's do.
            friend_paths = [":" + name],
            kotlin_compiler_plugins = _plugins(False, serialization),
            extra_kotlinc_arguments = _LANGUAGE,
            java_version = JAVA_VERSION,
            resources = test_resources,
            resources_root = test_resources_root,
            # Every class with tests, as Gradle runs them: Kotlin files
            # aren't named after the classes they hold.
            discover_all_test_classes = True,
            **_common()
        )

def amux_android_library(
        name,
        package,
        deps = [],
        exported_deps = [],
        compose = False,
        serialization = False,
        stability = False,
        srcs = None,
        res = True,
        assets = False,
        res_name = None,
        manifest = None,
        resources = [],
        resources_root = None,
        tests = True,
        test_deps = [],
        test_vm_args = [],
        test_env = {},
        visibility = ["PUBLIC"]):
    """A module with Android code: Kotlin under src/main/kotlin, resources
    under src/main/res (`package` is their R class's, Gradle's
    `namespace`), and its Robolectric tests under src/test/kotlin as
    `:test`. `stability = True` gives the Compose compiler the app's
    stability configuration. Libraries of one package that share their
    resources (the app's build types) name them alike (`res_name`)."""
    _sources()
    resource_deps = []
    has_res = res and native.glob(["src/main/res/**"])
    has_assets = assets and native.glob(["src/main/assets/**"])
    res_name = res_name or name + "-res"
    if (has_res or has_assets) and native.rule_exists(res_name):
        resource_deps = [":" + res_name]
    elif has_res or has_assets:
        native.android_resource(
            name = res_name,
            package = package,
            res = "src/main/res" if has_res else None,
            assets = "src/main/assets" if has_assets else None,
            # Each module's R class holds only its own resources, as
            # Gradle's `android.nonTransitiveRClass`.
            deps = [],
            visibility = visibility,
            **_common()
        )
        resource_deps = [":" + res_name]
    native.android_library(
        name = name,
        srcs = srcs if srcs != None else native.glob(["src/main/kotlin/**/*.kt"]),
        language = "KOTLIN",
        deps = deps + resource_deps,
        exported_deps = exported_deps,
        kotlin_compiler_plugins = _plugins(compose, serialization),
        extra_kotlinc_arguments = _kotlin_args(compose, stability),
        java_version = JAVA_VERSION,
        manifest = manifest,
        resources = resources,
        resources_root = resources_root,
        visibility = visibility,
        **_common()
    )
    test_srcs = native.glob(["src/test/kotlin/**/*.kt"]) if tests else []
    if test_srcs:
        amux_robolectric_test(
            name = "test",
            srcs = test_srcs,
            deps = [":" + name] + deps + exported_deps + resource_deps + test_deps,
            friend_paths = [":" + name],
            compose = compose,
            serialization = serialization,
            stability = stability,
            vm_args = test_vm_args,
            env = test_env,
        )

def amux_robolectric_test(
        name,
        srcs,
        deps,
        compose = False,
        serialization = False,
        stability = False,
        vm_args = [],
        env = {},
        test_class_names = None,
        manifest_skeleton = "//build/android:TestAndroidManifest.xml",
        **kwargs):
    """Robolectric tests of `srcs`: every class with tests, or only
    `test_class_names`. The manifest the tests' app is merged from is a
    bare one, or the app's own for tests of the app (its Application
    class, its permissions)."""
    if test_class_names:
        _lines(
        exec_compatible_with = LINUX_EXEC,
            name = name + "-classes",
            lines = test_class_names,
            default_target_platform = PLATFORM,
        )
        kwargs["test_class_names_file"] = ":" + name + "-classes"
    else:
        kwargs["discover_all_test_classes"] = True
    native.robolectric_test(
        name = name,
        srcs = srcs,
        language = "KOTLIN",
        deps = deps + [
            "//third-party/android:junit",
            "//third-party/android:robolectric",
        ],
        kotlin_compiler_plugins = _plugins(compose, serialization),
        extra_kotlinc_arguments = _kotlin_args(compose, stability),
        java_version = JAVA_VERSION,
        manifest_skeleton = manifest_skeleton,
        robolectric_runtime_dependencies = [ANDROID_ALL],
        vm_args = vm_args,
        env = env,
        **(_common() | kwargs)
    )

# The configurations the Rust bridge builds in for the app: the host JVM's
# for the tests, and each Android ABI's. Driving builds (debug APKs, tests) are `phone-dev` with the debug
# tools; shipping builds are `release` without them (build/rust.bzl's
# DEBUG_TOOLS follows the mode).
_JNI_PLATFORMS = {
    "android-arm64-phone-dev": "root//build/platforms:android-arm64-app-shipping-phone-dev",
    "android-arm64-release": "root//build/platforms:android-arm64-app-shipping-release",
    "android-x86_64-phone-dev": "root//build/platforms:android-x86_64-app-shipping-phone-dev",
    "android-x86_64-release": "root//build/platforms:android-x86_64-app-shipping-release",
    "host-phone-dev": "root//build/platforms:linux-app-shipping-phone-dev",
}

def _jni_transition_impl(platform: PlatformInfo, refs: struct, attrs: struct) -> PlatformInfo:
    _ignore = platform  # buildifier: disable=unused-variable
    return getattr(refs, attrs.configuration.replace("-", "_"))[PlatformInfo]

_jni_transition = transition(
    impl = _jni_transition_impl,
    refs = {name.replace("-", "_"): platform for name, platform in _JNI_PLATFORMS.items()},
    attrs = ["configuration"],
)

def _jni_library_impl(ctx: AnalysisContext) -> list[Provider]:
    lib = ctx.attrs.lib[DefaultInfo].default_outputs[0]
    name = "libamux_app.so"
    if not ctx.attrs.abi:
        out = ctx.actions.copy_file(name, lib)
        return [DefaultInfo(default_output = out)]

    # Android: a directory of `<abi>/libamux_app.so`, as
    # prebuilt_native_library takes it. A shipping library loses its
    # debug sections (`-Cdebuginfo=line-tables-only` keeps line tables for
    # symbolication, which the APK needn't carry).
    if ctx.attrs.strip:
        stripped = ctx.actions.declare_output("stripped/" + name)
        ctx.actions.run(
            cmd_args(ctx.attrs._strip[RunInfo], "--strip-debug", lib, "-o", stripped.as_output()),
            category = "strip_jni",
        )
        lib = stripped
    out = ctx.actions.symlinked_dir("jni", {ctx.attrs.abi + "/" + name: lib})
    return [DefaultInfo(default_output = out)]

_jni_library = rule(
    impl = _jni_library_impl,
    cfg = _jni_transition,
    attrs = {
        "abi": attrs.option(attrs.string(), default = None),
        "configuration": attrs.string(),
        "lib": attrs.dep(),
        "strip": attrs.bool(default = False),
        "_strip": attrs.exec_dep(default = "toolchains//:llvm-strip"),
    },
)

def amux_jni_library(name, configuration, abi = None, visibility = ["PUBLIC"]):
    """The Rust bridge (//crates/app-jni) as `libamux_app.so`, built in one
    of `_JNI_PLATFORMS`: for the host JVM (`abi = None`, the file), or for
    an Android ABI (`abi = "arm64-v8a"`, a native library for the APK)."""
    lib = name + "-lib" if abi else name
    _jni_library(
        exec_compatible_with = LINUX_EXEC,
        name = lib,
        configuration = configuration,
        abi = abi,
        lib = "//crates/app-jni:app-jni[shared]",
        strip = configuration.endswith("-release"),
        default_target_platform = PLATFORM,
        target_compatible_with = LINUX_WORKERS,
        visibility = visibility,
    )
    if abi:
        native.prebuilt_native_library(
            name = name,
            native_libs = ":" + lib,
            default_target_platform = PLATFORM,
            target_compatible_with = LINUX_WORKERS,
            labels = LINUX_ONLY,
            visibility = visibility,
        )

def _resources_jar_impl(ctx: AnalysisContext) -> list[Provider]:
    tree = ctx.actions.symlinked_dir("resources", ctx.attrs.srcs)
    out = ctx.actions.declare_output(ctx.label.name + ".jar")
    ctx.actions.run(
        cmd_args(ctx.attrs._java_toolchain[JavaToolchainInfo].jar, "-cfM", out.as_output(), "-C", tree, "."),
        category = "resources_jar",
    )
    return [DefaultInfo(default_output = out)]

_resources_jar = rule(
    impl = _resources_jar_impl,
    attrs = {
        "srcs": attrs.dict(attrs.string(), attrs.source()),
        "_java_toolchain": toolchains_common.java(),
    },
)

def resource_jar(name, srcs, visibility = ["PUBLIC"]):
    """Files from elsewhere in the checkout on a module's classpath, each at
    the path in the jar it is keyed by (`{"sealed-pushes.json": ...}`), as
    Gradle's `resources.srcDir`."""
    _resources_jar(
        exec_compatible_with = LINUX_EXEC,
        name = name + "-jar",
        srcs = srcs,
        default_target_platform = PLATFORM,
        target_compatible_with = LINUX_WORKERS,
    )
    native.prebuilt_jar(
        name = name,
        binary_jar = ":" + name + "-jar",
        visibility = visibility,
        **_common()
    )

def _lines_impl(ctx: AnalysisContext) -> list[Provider]:
    return [DefaultInfo(default_output = ctx.actions.write(ctx.label.name, ctx.attrs.lines))]

_lines = rule(impl = _lines_impl, attrs = {"lines": attrs.list(attrs.string())})

def _capture_impl(ctx: AnalysisContext) -> list[Provider]:
    info = ctx.attrs.test[ExternalRunnerTestInfo]
    out = ctx.actions.declare_output(ctx.label.name, dir = True)
    properties = " ".join(["-D{}={}".format(k, v) for k, v in ctx.attrs.properties.items() if v])

    # The JVM reads JAVA_TOOL_OPTIONS before the test's own arguments, so
    # the output directory goes in as a property without the test target
    # (and its key) knowing where its pictures go.
    # A run that drew nothing (its test skipped itself) fails rather than
    # leaving an empty directory in the cache.
    script = 'out=$(pwd)/$1; shift; mkdir -p "$out" && JAVA_TOOL_OPTIONS="{} -D{}=$out" && export JAVA_TOOL_OPTIONS && "$@" || exit; [ -n "$(ls -A "$out")" ] || {{ echo "capture: the test wrote nothing" >&2; exit 1; }}'.format(
        properties,
        ctx.attrs.output_property,
    )
    ctx.actions.run(
        cmd_args("/bin/sh", "-c", script, "--", out.as_output(), info.command),
        env = info.env,
        category = "robolectric_capture",
    )
    return [DefaultInfo(default_output = out)]

_robolectric_capture = rule(
    impl = _capture_impl,
    attrs = {
        "output_property": attrs.string(),
        "properties": attrs.dict(attrs.string(), attrs.string(), sorted = False),
        "test": attrs.dep(providers = [ExternalRunnerTestInfo]),
        # Runs where the test would: on a Linux worker.
        "_java_toolchain": toolchains_common.java_for_host_test(),
    },
)

def robolectric_capture(name, test, properties, output_property, visibility = ["PUBLIC"]):
    """What a Robolectric test (`test`) writes, as a directory: it runs as a
    build action with the system `properties`, and `output_property` naming
    the directory it writes to. For captures such as the storybook, whose
    output is the point and which nothing compares."""
    _robolectric_capture(
        exec_compatible_with = LINUX_EXEC,
        name = name,
        test = test,
        properties = properties,
        output_property = output_property,
        default_target_platform = PLATFORM,
        target_compatible_with = LINUX_WORKERS,
        visibility = visibility,
    )

# The debug key every local build signs with: Android's conventional one
# (alias androiddebugkey, password android), committed so a build signs the
# same wherever it runs. An APK signed with it installs over another only
# when that one was too.
DEBUG_KEYSTORE = "//build/android:debug-keystore"

def amux_apk(name, deps, manifest_skeleton, manifest_entries, package_type, keystore = DEBUG_KEYSTORE, visibility = ["PUBLIC"], **kwargs):
    """An APK of `deps` for the app's one ABI the phones and the Mac's
    emulator run (arm64-v8a), dexed for its minSdk."""
    native.android_binary(**_package(name, deps, manifest_skeleton, manifest_entries, package_type, keystore, visibility, kwargs))

def amux_bundle(name, deps, manifest_skeleton, manifest_entries, package_type, keystore = DEBUG_KEYSTORE, visibility = ["PUBLIC"], **kwargs):
    """The app bundle of `deps` that Google Play takes, as `amux_apk`
    builds an APK of them. It is unsigned (`keystore` signs only APKs
    derived from it)."""
    native.android_bundle(**_package(name, deps, manifest_skeleton, manifest_entries, package_type, keystore, visibility, kwargs))

def _package(name, deps, manifest_skeleton, manifest_entries, package_type, keystore, visibility, kwargs):
    return dict(
        name = name,
        deps = deps,
        manifest_skeleton = manifest_skeleton,
        manifest_entries = manifest_entries,
        package_type = package_type,
        keystore = keystore,
        cpu_filters = ["arm64"],
        # Android 5 and later load every classes*.dex in the APK themselves.
        use_split_dex = True,
        multidex_min_api = "34",
        dex_compression = "raw",
        # Android loads every dex file itself, so the primary one needs
        # nothing in particular; the dex splitter only wants it to hold
        # something.
        primary_dex_patterns = ["^sh/amux/android/AmuxApplication^"],
        # The prelude's system toolchain closes a secondary dex at 1 KB, a
        # setting for its own tests that gives thousands of dex files and an
        # action per file; Buck's own default is 12 MB.
        secondary_dex_weight_limit = 12 * 1024 * 1024,
        visibility = visibility,
        **(_common() | kwargs)
    )

def _script_test_impl(ctx: AnalysisContext) -> list[Provider]:
    interpreter = ["bin/python3", "-B"] if ctx.attrs.shell == False else ["--", "/bin/sh"]
    command = cmd_args(
        ctx.attrs._nix[NixToolchainInfo].run,
        interpreter,
        ctx.attrs.script,
        ctx.attrs.args,
        hidden = [r[DefaultInfo].default_outputs + r[DefaultInfo].other_outputs for r in ctx.attrs.resources],
    )
    executors = get_re_executors_from_props(ctx)
    return [
        DefaultInfo(),
        ExternalRunnerTestInfo(
            type = "custom",
            command = [command],
            labels = ctx.attrs.labels,
            default_executor = executors.default_executor,
            executor_overrides = executors.executor_overrides,
            run_from_project_root = True,
            use_project_relative_paths = True,
            supports_test_execution_caching = True,
        ),
    ]

_script_test = rule(
    impl = _script_test_impl,
    attrs = {
        "args": attrs.list(attrs.arg(), default = []),
        "labels": attrs.list(attrs.string(), default = []),
        # Files the script reads by their paths in the checkout.
        "resources": attrs.list(attrs.dep(), default = []),
        "script": attrs.source(),
        "shell": attrs.bool(default = False),
        "_nix": attrs.exec_dep(providers = [NixToolchainInfo], default = "root//:nix-linux"),
    } | re_test_common.test_args(),
)

def script_test(name, script, args = [], resources = [], shell = False, visibility = ["PUBLIC"]):
    """A check run on a Linux worker: `script`, by the Linux toolchain's
    Python (or `sh` with `shell = True`), with `args` (`$(location ...)` of
    what the build made) and `resources` present at their paths in the
    checkout, passing when it exits 0."""
    _script_test(
        name = name,
        script = script,
        args = args,
        resources = resources,
        shell = shell,
        labels = LINUX_ONLY,
        default_target_platform = PLATFORM,
        target_compatible_with = LINUX_WORKERS,
        exec_compatible_with = LINUX_EXEC,
        visibility = visibility,
    )
