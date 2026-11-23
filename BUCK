# Files outside the crates that the crates' builds and tests read.
load("//build:rust.bzl", "amux_files")

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

amux_files(
    name = "sealed-pushes",
    srcs = ["apps/apple/Packages/AmuxPush/Tests/AmuxPushTests/Fixtures/sealed-pushes.json"],
)

# The phone apps' committed mirrors of the view values and fixtures, which
# ios-tools' tests hold to the Rust definitions.
amux_files(
    name = "mirrors",
    srcs = [
        "apps/android/core/values/src/main/kotlin/sh/amux/values/Values.kt",
        "apps/android/push/src/main/kotlin/sh/amux/push/Said.kt",
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
