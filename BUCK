# Files outside the crates that the crates' builds and tests read.
load("//build:rust.bzl", "amux_files")
load("@toolchains//:nix.bzl", "nix_toolchain")

# Compiled in with include_str! by a crate's code or tests; each is one
# file, named by its path in the checkout in the crate's `include`.
[
    export_file(
        name = path,
        src = path,
        mode = "reference",
        visibility = ["PUBLIC"],
    )
    for path in [
        "apps/apple/justfile",
        "clippy.toml",
        "journeys/manifest.json",
        "justfile",
        "qa/FINDING.md",
        "tests/stories.json",
    ]
]

# Read at run time by tests, as their `resources`.
amux_files(
    name = "journeys",
    srcs = glob(["journeys/**"]),
)

amux_files(
    name = "environments",
    srcs = glob(["environments/**"]),
)

amux_files(
    name = "scripts",
    srcs = glob(["scripts/**"]),
)

amux_files(
    name = "guide",
    srcs = glob(["docs/guide/**"]),
)

# The parsed prose the storybooks draw, on the Android test-support
# module's classpath.
export_file(
    name = "markdown-fixtures.json",
    src = "apps/apple/Packages/AmuxCore/Sources/AmuxFixtures/Fixtures/markdown-fixtures.json",
    visibility = ["PUBLIC"],
)

# The same file on the Android push module's test classpath.
export_file(
    name = "sealed-pushes.json",
    src = "apps/apple/Packages/AmuxPush/Tests/AmuxPushTests/Fixtures/sealed-pushes.json",
    visibility = ["PUBLIC"],
)

amux_files(
    name = "sealed-pushes",
    srcs = ["apps/apple/Packages/AmuxPush/Tests/AmuxPushTests/Fixtures/sealed-pushes.json"],
)

# The phone apps' committed mirrors of the view values and fixtures, which
# ios-tools' tests hold to the Rust definitions (the Android app's are in
# its packages: //apps/android/push:mirrors).
amux_files(
    name = "mirrors",
    srcs = [
        "apps/apple/Packages/AmuxCore/Sources/AmuxFixtures/Fixtures/markdown-fixtures.json",
        "apps/apple/Packages/AmuxCore/Sources/AmuxValues/Values.swift",
        "apps/apple/Packages/AmuxPush/Sources/AmuxPush/Said.swift",
    ],
)

# The budgets page, which testnet's perf tests hold to the runner's budgets.
amux_files(
    name = "performance",
    srcs = ["docs/PERFORMANCE.md"],
)

# The release workflow, whose trusted keys xtask's release tests read.
amux_files(
    name = "release-workflow",
    srcs = [".github/workflows/release.yml"],
)

# The protobuf schemas, as one directory, for //crates/wire:protobuf.
export_file(
    name = "proto",
    src = "proto",
    visibility = ["PUBLIC"],
)

# The Linux toolchain every Linux action runs with (build/toolchains/nix.bzl):
# realized on the Linux worker from these files, so a branch that changes
# them is checked with the toolchain it brings, from any machine.
nix_toolchain(
    name = "nix-linux",
    srcs = ["flake.lock", "flake.nix", "nix/build-tools.nix", "nix/toolchain.nix", "rust-toolchain.toml"],
    output = "packages.x86_64-linux.actions",
    exec_compatible_with = ["prelude//os:linux"],
    target_compatible_with = ["prelude//os:linux"],
    visibility = ["PUBLIC"],
)

# The Windows toolchain (nix/windows.nix): the pinned Rust, LLVM's
# MSVC-compatible compiler and linker, and Microsoft's CRT and SDK, with
# which a Linux worker compiles and links everything built for Windows.
# Apart from the Linux toolchain, so an SDK bump rekeys only Windows work.
nix_toolchain(
    name = "nix-windows",
    srcs = ["flake.lock", "flake.nix", "nix/windows.nix", "rust-toolchain.toml"],
    output = "packages.x86_64-linux.windows",
    exec_compatible_with = ["prelude//os:linux"],
    target_compatible_with = ["prelude//os:linux"],
    visibility = ["PUBLIC"],
)

# The repository checks, each a test over exactly the files it reads
# (build/checks.bzl), so a passing one is cached until those change. `just
# checks` writes what they read from outside the checkout first
# (build/checks/inputs.py) and runs them all; each recipe of the same name
# (`just docs-check`) still runs its script in the checkout, as CI does.
load("//build:checks.bzl", "amux_check")

_CORES = ["agent-core", "core-ring", "daemon-core", "path-core", "relay-core", "socket-core", "supervisor-core", "workspace-core"]

amux_check(
    name = "docs-check",
    command = ["python3", "scripts/docs-check.py", "--tracked", "build/checks/inputs/tracked"],
    # Links are resolved against the list of tracked files; the pages, and
    # the files whose content a page shows, are read.
    srcs = glob(["docs/**"]) + ["scripts/docs-check.py"],
    deps = ["//build:tracked", "//crates/ui-runtime:examples/fleet.rs"],
)

amux_check(
    name = "store-check",
    command = ["python3", "scripts/store-check.py", "--baseline", "build/checks/inputs/store-baseline"],
    srcs = ["scripts/store-check.py"],
    deps = ["//build:store-baseline", "//crates/store:src/schema.rs"],
)

amux_check(
    name = "proto-compat",
    command = ["sh", "scripts/proto-compat.sh", "--baseline", "build/checks/inputs/proto-baseline"],
    srcs = glob(["proto/**"]) + ["buf.yaml", "scripts/proto-compat.sh"],
    deps = ["//build:proto-baseline"],
    env = select({
        "DEFAULT": {"BUF": "$(exe //build:buf)"},
        "prelude//os:linux": {},
    }),
)

# The cores' sources; the tokio half of `just cores-check` reads Cargo's
# whole graph, so premerge runs it as a script.
amux_check(
    name = "cores-check",
    command = ["python3", "scripts/cores-check.py", "--sources"],
    srcs = ["scripts/cores-check.py"],
    deps = ["//crates/{}:files".format(core) for core in _CORES],
)

amux_check(
    name = "typed-provider-check",
    command = ["python3", "scripts/typed-provider-check.py"],
    srcs = ["scripts/typed-provider-check.py"],
    deps = ["//crates/agent:files", "//crates/interpret:files"],
)

amux_check(
    name = "client-kind-check",
    command = ["python3", "scripts/client-kind-check.py"],
    srcs = glob(
        ["apps/apple/**/*.swift"],
        exclude = ["apps/apple/**/.build/**"],
    ) + ["scripts/client-kind-check.py"],
    deps = [
        "//crates/app-runtime:files",
        "//crates/browser-core:files",
        "//crates/tui:files",
        "//crates/ui-runtime:files",
        "//crates/ui-state:files",
        "//crates/ui-view:files",
        "//crates/wire:files",
    ],
)

# The committed protobuf bindings against what the schemas generate now.
amux_check(
    name = "codegen-check",
    command = [
        "sh",
        "-c",
        "diff -ru crates/wire/src/generated \"$1\" | head -200; diff -r crates/wire/src/generated \"$1\" >/dev/null || { echo \"committed wire output is stale; run 'just protobuf' and commit it\" >&2; exit 1; }",
        "sh",
        "$(location //crates/wire:protobuf)",
    ],
    deps = ["//crates/wire:files", "//crates/wire:protobuf"],
)

# The Chromium the browser extension's end-to-end test drives (js/BUCK).
nix_toolchain(
    name = "nix-browser",
    srcs = ["flake.lock", "flake.nix", "nix/build-tools.nix", "nix/toolchain.nix", "rust-toolchain.toml"],
    output = "packages.x86_64-linux.browser",
    exec_compatible_with = ["prelude//os:linux"],
    target_compatible_with = ["prelude//os:linux"],
    visibility = ["PUBLIC"],
)

# The Android toolchain (nix/android.nix): the JDK and Google's Android SDK,
# realized on the Linux worker like the toolchain above, apart from it so a
# Rust toolchain change doesn't rekey Kotlin compiles and an SDK bump
# doesn't rekey Rust ones.
nix_toolchain(
    name = "nix-android",
    srcs = ["flake.lock", "flake.nix", "nix/android.nix"],
    output = "packages.x86_64-linux.android",
    exec_compatible_with = ["prelude//os:linux"],
    target_compatible_with = ["prelude//os:linux"],
    visibility = ["PUBLIC"],
)
