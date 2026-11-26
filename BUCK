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

# The scripts the apps' recipe scripts run or import from here (their
# tests are checks in apps/apple/Tools/BUCK and apps/android/tools/BUCK).
amux_files(
    name = "app-script-helpers",
    srcs = ["scripts/credentials.py", "scripts/py"],
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

# The workspace as Cargo reads it (build/cargo.bzl), for the checks that
# run Cargo: Cargo.toml's members, each crate's manifest and Rust files.
# A member missing here fails those checks, Cargo naming its manifest.
load("//build:cargo.bzl", "cargo_output", "cargo_workspace")

_CARGO_PACKAGES = ["//crates/{}:cargo-package".format(name) for name in [
    "acp-protocol",
    "agent",
    "agent-core",
    "agent-dir",
    "amux",
    "analytics",
    "app-bridge",
    "app-embedded",
    "app-ffi",
    "app-jni",
    "app-runtime",
    "attachments",
    "browser-core",
    "claude-protocol",
    "client",
    "codex-protocol",
    "core-ring",
    "daemon-core",
    "exe-resource",
    "fake-amux",
    "git-facts",
    "highlight",
    "installer",
    "interpret",
    "ios-tools",
    "journal",
    "launch",
    "log-file",
    "models",
    "node",
    "os",
    "path-core",
    "patience",
    "preview",
    "provider-fakes",
    "pty-host",
    "qualification",
    "redaction",
    "relay-core",
    "release",
    "replay-support",
    "settings",
    "shot",
    "socket-core",
    "source-tree",
    "store",
    "supervisor-core",
    "testnet",
    "tui",
    "ui-code",
    "ui-runtime",
    "ui-state",
    "ui-view",
    "wire",
    "workspace-core",
    "xtask",
]] + ["//third-party/rust/dev-only:cargo-package"]

# Every member's manifest, and its Rust files' paths only: enough for
# Cargo's graph, which changes when a manifest or the lockfile does or a
# file comes or goes.
cargo_workspace(
    name = "cargo-layout",
    srcs = ["Cargo.lock", "Cargo.toml"],
    packages = _CARGO_PACKAGES,
    sources = False,
)

# The same with every Rust file: what formatting and cargo-shear read.
cargo_workspace(
    name = "cargo-sources",
    srcs = ["Cargo.lock", "Cargo.toml", "rustfmt.toml"],
    packages = _CARGO_PACKAGES,
    sources = True,
)

# Every dependency the lockfile names, for every platform, as a Cargo home:
# the registry's crates and index entries, and the git dependencies. Fetched
# again each day as well, since crates.io's index says which releases have
# been yanked (`deny`).
cargo_output(
    name = "cargo-registry",
    command = ["sh", "$(location //build:checks/cargo.sh)", "fetch", "$(location :cargo-layout)"],
    deps = [":cargo-layout", "//build:checks/cargo.sh", "//build:day"],
    out = "cargo-home",
    directory = True,
)

# `cargo metadata --no-deps`: each member's targets, features and
# dependencies as Cargo sees them, with `@ROOT@` for the checkout's root.
# The dependency policy reads it here, and the parity check and the test
# catalogue's check, which stay scripts, read it built.
cargo_output(
    name = "cargo-metadata",
    command = ["sh", "$(location //build:checks/cargo.sh)", "metadata", "$(location :cargo-layout)"],
    deps = [":cargo-layout", "//build:checks/cargo.sh"],
    out = "cargo-metadata.json",
    visibility = ["PUBLIC"],
)

# The repository checks, each a test over exactly the files it reads
# (build/checks.bzl), so a passing one is cached until those change. `just
# checks` writes what they read from outside the checkout first
# (build/checks/inputs.py) and runs them all; each recipe of the same name
# (`just docs-check`) still runs its script in the checkout, as CI does.
load("//build:checks.bzl", "amux_check", "amux_script_tests")

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

# Every dependency against deny.toml: no open advisory or yanked release,
# only the licences and sources it lists. On Cargo's graph over the day's
# registry, and the advisory database as build/checks/inputs.py snapshots
# it, so it reruns when the lockfile, a manifest, deny.toml or the day's
# registry or database changes. cargo-deny is the Linux toolchain's.
amux_check(
    name = "deny",
    command = [
        "sh", "$(location //build:checks/cargo.sh)", "run", "$(location :cargo-layout)", "$(location :cargo-registry)",
        "sh", "build/checks/deny.sh",
    ],
    srcs = ["deny.toml"],
    deps = [":cargo-layout", ":cargo-registry", "//build:checks/cargo.sh", "//build:deny"],
    linux_only = True,
)

# The cores' sources: no lock, spawn or await.
amux_check(
    name = "cores-check",
    command = ["python3", "scripts/cores-check.py", "--sources"],
    srcs = ["scripts/cores-check.py"],
    deps = ["//crates/{}:files".format(core) for core in _CORES],
)

# Every crate, for the platform check, which reads each one's Rust files
# (its tests' too). A crate not listed fails the check, which compares what
# it was given with the list of tracked files.
_CRATES = [
    "acp-protocol", "agent", "agent-core", "agent-dir", "amux", "analytics", "app-bridge",
    "app-embedded", "app-ffi", "app-jni", "app-runtime", "attachments", "browser-core",
    "claude-protocol", "client", "codex-protocol", "core-ring", "daemon-core", "exe-resource", "fake-amux",
    "git-facts", "highlight", "installer", "interpret", "ios-tools", "journal", "launch",
    "log-file", "models", "node", "os", "path-core", "patience", "preview", "provider-fakes",
    "pty-host", "qualification", "redaction", "relay-core", "release", "replay-support", "settings",
    "shot", "socket-core", "source-tree", "store", "supervisor-core", "testnet", "tui",
    "ui-code", "ui-runtime", "ui-state", "ui-view", "wire", "workspace-core", "xtask",
]

# Desktop-platform code only in its homes, the rest held to the allowlist
# (build/checks/platform_check.py says what counts).
amux_check(
    name = "platform-check",
    command = ["python3", "build/checks/platform_check.py", "--tracked", "build/checks/inputs/tracked"],
    deps = ["//build:platform-check", "//build:tracked"] + ["//crates/{}:rust-files".format(crate) for crate in _CRATES],
)

# The cores' dependency graphs, on every target: no tokio. `cargo tree`
# over the fetched registry, so it reruns when a manifest or the lockfile
# changes (or the day's registry does), not when a core's code does.
amux_check(
    name = "cores-graph-check",
    command = [
        "sh", "$(location //build:checks/cargo.sh)", "run", "$(location :cargo-layout)", "$(location :cargo-registry)",
        "python3", "scripts/cores-check.py", "--graph",
    ],
    srcs = ["scripts/cores-check.py"],
    deps = [":cargo-layout", ":cargo-registry", "//build:checks/cargo.sh"],
    linux_only = True,
)

# rustfmt over every member's Rust files, with the dated nightly
# (nix/toolchain.nix's `cargo-nightly`).
amux_check(
    name = "fmt-check",
    command = ["sh", "$(location //build:checks/cargo.sh)", "run", "$(location :cargo-sources)", "cargo-nightly", "fmt", "--all", "--", "--check"],
    deps = [":cargo-sources", "//build:checks/cargo.sh"],
    linux_only = True,
)

# Which crate may depend on which, from Cargo's metadata.
amux_check(
    name = "dependency-policy",
    command = ["python3", "scripts/check-dependency-policy.py", "--metadata", "$(location :cargo-metadata)"],
    srcs = ["scripts/check-dependency-policy.py"],
    deps = [":cargo-metadata"],
    linux_only = True,
)

# cargo-shear: no dependency that no crate's code uses.
amux_check(
    name = "shear",
    command = [
        "sh", "$(location //build:checks/cargo.sh)", "run", "$(location :cargo-sources)", "$(location :cargo-registry)",
        "cargo", "shear", "--locked", "--deny-warnings",
    ],
    deps = [":cargo-sources", ":cargo-registry", "//build:checks/cargo.sh"],
    linux_only = True,
)

amux_check(
    name = "typed-provider-check",
    command = ["python3", "scripts/typed-provider-check.py"],
    srcs = ["scripts/typed-provider-check.py"],
    deps = ["//crates/agent:files", "//crates/interpret:files"],
)

_CLIENT_KIND_SOURCES = [
    "//apps/apple:swift",
        "//apps/apple/Packages/AmuxCore:swift",
        "//apps/apple/Packages/AmuxDesign:swift",
        "//apps/apple/Packages/AmuxFeatures:swift",
        "//apps/apple/Packages/AmuxMacScreens:swift",
        "//apps/apple/Packages/AmuxPush:swift",
        "//apps/apple/Packages/AmuxShell:swift",
        "//apps/apple/Packages/AmuxTestSupport:swift",
        "//crates/app-runtime:files",
        "//crates/browser-core:files",
        "//crates/tui:files",
        "//crates/ui-runtime:files",
        "//crates/ui-state:files",
        "//crates/ui-view:files",
    "//crates/wire:files",
]

amux_check(
    name = "client-kind-check",
    command = ["python3", "scripts/client-kind-check.py"],
    srcs = ["scripts/client-kind-check.py"],
    deps = _CLIENT_KIND_SOURCES,
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

# The unit tests of the repository's scripts, one check per file
# (`:script-tests-land_queue`), each over every script, so a change to one
# reruns them all, side by side; `extra` is what a test reads elsewhere.
amux_script_tests(
    prefix = "script-tests",
    tests = glob(["scripts/tests/*_test.py"]),
    srcs = glob(["scripts/**"], exclude = ["**/__pycache__/**"]),
    extra = {
        "client_kind_check": {"deps": _CLIENT_KIND_SOURCES},
        "bootstrap_guard": {"deps": ["toolchains//:bootstrap-guard.py"]},
        "check_inputs": {"deps": ["//build:check-inputs"]},
        "ci_jobs": {"srcs": [".github/workflows/ci.yml"], "deps": ["//build:ci-jobs", "//build:tracked"]},
        "env": {"deps": [":environments"]},
        "icon": {"deps": ["//apps/apple:release-files"]},
        "nativelink": {"deps": ["//build:nativelink"]},
        "platform_check": {"deps": ["//build:platform-check"]},
        "qa_sandbox_purchase": {"srcs": [".gitignore"], "deps": ["//apps/apple:release-files"]},
        "release": {"srcs": [".gitignore"], "deps": ["//apps/apple:release-files"]},
        "store_check": {"deps": ["//crates/store:src/schema.rs"]},
        "test_runner": {"deps": ["//build:test-runner"]},
        "tests_catalog": {"srcs": ["tests/catalog.toml"], "deps": ["//crates/testnet:suites"]},
        "timing": {"deps": ["//build:timing"]},
        "typed_provider_check": {"deps": ["//crates/agent:files", "//crates/interpret:files"]},
    },
)
