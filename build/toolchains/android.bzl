"""The Android toolchains' pieces: the JDK and Google's Android SDK from the
Android Nix toolchain (nix/android.nix, realized on the Linux worker by
//:nix-android), as the tools and files the prelude's Java, Kotlin and
Android toolchains take.

The prelude's scripts take a tool as one path (`--java PATH`), so each tool
is a small script that runs it by its store path, written on the Linux
worker from the realized toolchain. The store path is in the script, so a
toolchain change changes the script and with it every action that runs it.
Files the rules read (android.jar) are copied out of the store, so their
content is an input like any other.

Every target here builds on a Linux worker: Android never builds on the
Mac (docs/ANDROID.md).
"""

load("@prelude//android:android_toolchain.bzl", "AndroidToolchainInfo")
load("@prelude//java:java_toolchain.bzl", "JavaToolchainInfo")
load(":nix.bzl", "NixToolchainInfo")

# The NDK's compilers and binary tools, by their paths under the Android
# toolchain (nix/android.nix), for Android's C (ring) and links.
NDK_BIN = "sdk/ndk/28.2.13676358/toolchains/llvm/prebuilt/linux-x86_64/bin/"

def ndk_tools(triple: str) -> dict[str, str]:
    """The C toolchain's tools for an Android target (`aarch64-linux-android`)
    in place of a Linux toolchain's bin/cc and the rest. The compiler drivers
    named for an API level target it: the app's minSdk."""
    return {
        "bin/ar": NDK_BIN + "llvm-ar",
        "bin/c++": NDK_BIN + triple + "34-clang++",
        "bin/cc": NDK_BIN + triple + "34-clang",
        "bin/nm": NDK_BIN + "llvm-nm",
        "bin/objcopy": NDK_BIN + "llvm-objcopy",
        "bin/objdump": NDK_BIN + "llvm-objdump",
        "bin/ranlib": NDK_BIN + "llvm-ranlib",
        "bin/strip": NDK_BIN + "llvm-strip",
    }

def _tool_impl(ctx: AnalysisContext) -> list[Provider]:
    script = ctx.actions.declare_output(ctx.label.name)
    ctx.actions.run(
        cmd_args(
            "/bin/sh",
            "-c",
            'printf \'#!/bin/sh\\nexec %s/%s "$@"\\n\' "$(cat "$1")" "$2" > "$3" && chmod +x "$3"',
            "--",
            ctx.attrs.nix[DefaultInfo].default_outputs[0],
            ctx.attrs.path,
            script.as_output(),
        ),
        category = "android_tool",
    )
    return [DefaultInfo(default_output = script), RunInfo(args = cmd_args(script))]

# A tool of the Android toolchain, by its path under the toolchain
# (`bin/java`, `sdk/build-tools/36.0.0/aapt2`).
android_tool = rule(
    impl = _tool_impl,
    attrs = {
        "nix": attrs.exec_dep(providers = [NixToolchainInfo], default = "root//:nix-android"),
        "path": attrs.string(),
    },
)

# Buck's JUnit runner exits 0 whatever its tests did (it reports them as
# XML on stdout for Buck1 to read), and Buck2 judges a test by its exit, so
# a failing test would pass. The JVM the tests run on judges the report:
# a test that failed, or a run with no tests in it, fails the run. A test
# that assumed something false (a capture run only on request) was skipped,
# not failed.
_JUNIT_JAVA = """#!/bin/sh
report=$(mktemp)
trap 'rm -f "$report"' EXIT
%s/bin/java "$@" > "$report"
status=$?
cat "$report"
[ "$status" -eq 0 ] || exit "$status"
if grep -q 'type="FAILURE"' "$report"; then
    echo 'junit: a test failed (type="FAILURE" above)' >&2
    exit 1
fi
if ! grep -q '<test ' "$report"; then
    echo 'junit: no test ran' >&2
    exit 1
fi
"""

def _junit_java_impl(ctx: AnalysisContext) -> list[Provider]:
    script = ctx.actions.declare_output(ctx.label.name)
    ctx.actions.run(
        cmd_args(
            "/bin/sh",
            "-c",
            'printf "$2" "$(cat "$1")" > "$3" && chmod +x "$3"',
            "--",
            ctx.attrs.nix[DefaultInfo].default_outputs[0],
            # The script's one `%s` is the store path; it has no other `%`
            # or backslash for printf to read.
            _JUNIT_JAVA,
            script.as_output(),
        ),
        category = "junit_java",
    )
    return [DefaultInfo(default_output = script), RunInfo(args = cmd_args(script))]

# The JVM tests run on: the toolchain's java, failing a run whose report
# says a test failed.
junit_java = rule(
    impl = _junit_java_impl,
    attrs = {
        "nix": attrs.exec_dep(providers = [NixToolchainInfo], default = "root//:nix-android"),
    },
)

def _file_impl(ctx: AnalysisContext) -> list[Provider]:
    out = ctx.actions.declare_output(ctx.attrs.out or ctx.attrs.path.split("/")[-1])
    ctx.actions.run(
        cmd_args(
            "/bin/sh",
            "-c",
            'cp -L "$(cat "$1")/$2" "$3" && chmod u+w "$3"',
            "--",
            ctx.attrs.nix[DefaultInfo].default_outputs[0],
            ctx.attrs.path,
            out.as_output(),
        ),
        category = "android_file",
    )
    return [DefaultInfo(default_output = out)]

# A file of the Android toolchain (`sdk/platforms/android-36/android.jar`),
# copied out of the store.
android_file = rule(
    impl = _file_impl,
    attrs = {
        "nix": attrs.exec_dep(providers = [NixToolchainInfo], default = "root//:nix-android"),
        "out": attrs.option(attrs.string(), default = None),
        "path": attrs.string(),
    },
)

def _sdk_impl(ctx: AnalysisContext) -> list[Provider]:
    # The prelude's android_sdk_tools, from the toolchain's targets.
    sub_targets = {}
    for name, dep in ctx.attrs.tools.items():
        sub_targets[name] = [dep[DefaultInfo], dep[RunInfo]]
    for name, dep in ctx.attrs.files.items():
        sub_targets[name] = [dep[DefaultInfo]]
    return [DefaultInfo(sub_targets = sub_targets)]

# The SDK's tools and files under the names the prelude's toolchains take
# them by (prelude//toolchains:android.bzl's android_sdk_tools).
android_sdk = rule(
    impl = _sdk_impl,
    attrs = {
        "files": attrs.dict(attrs.string(), attrs.dep()),
        "tools": attrs.dict(attrs.string(), attrs.dep(providers = [RunInfo])),
    },
)

def _linux_toolchain_impl(ctx: AnalysisContext):
    return ctx.attrs.actual.providers

# A prelude toolchain whose actions must run on a Linux worker: the
# toolchain's `exec_compatible_with` carries to every target that uses it,
# which the prelude's own toolchain macros don't let a caller set.
linux_toolchain = rule(
    impl = _linux_toolchain_impl,
    attrs = {"actual": attrs.toolchain_dep()},
    is_toolchain_rule = True,
)

def _android_toolchain_impl(ctx: AnalysisContext) -> list[Provider]:
    info = ctx.attrs.actual[AndroidToolchainInfo]
    fields = {field: getattr(info, field) for field in dir(info)}
    fields.update(ctx.attrs.replace)
    fields.update({field: dep[DefaultInfo].default_outputs[0] for field, dep in ctx.attrs.replace_files.items()})
    others = [p for p in ctx.attrs.actual.providers if not isinstance(p, AndroidToolchainInfo)]
    return others + [AndroidToolchainInfo(**fields)]

# The prelude's Android toolchain with some of its tools replaced
# (build/android/manifest says why).
android_toolchain_with = rule(
    impl = _android_toolchain_impl,
    attrs = {
        "actual": attrs.toolchain_dep(),
        "replace": attrs.dict(attrs.string(), attrs.dep(providers = [RunInfo])),
        # Fields that are a file (`proguard_jar`), by the target that builds it.
        "replace_files": attrs.dict(attrs.string(), attrs.dep(), default = {}),
    },
    is_toolchain_rule = True,
)

def _java_toolchain_impl(ctx: AnalysisContext) -> list[Provider]:
    info = ctx.attrs.actual[JavaToolchainInfo]
    fields = {field: getattr(info, field) for field in dir(info)}
    fields.update(ctx.attrs.values)
    others = [p for p in ctx.attrs.actual.providers if not isinstance(p, JavaToolchainInfo)]
    return others + [JavaToolchainInfo(**fields)]

# The prelude's Java toolchain with settings its macro doesn't take
# (`proguard_max_heap_size`).
java_toolchain_with = rule(
    impl = _java_toolchain_impl,
    attrs = {
        "actual": attrs.toolchain_dep(),
        "values": attrs.dict(attrs.string(), attrs.string()),
    },
    is_toolchain_rule = True,
)
