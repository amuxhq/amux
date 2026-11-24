set shell := ["sh", "-cu"]
set windows-shell := ["sh", "-cu"]
set positional-arguments

export AMUX_GIT_SHA := `git rev-parse HEAD`

# Wall-clock bound for every recipe; portable where GNU timeout is absent.
bounded := "scripts/bounded"
# The pinned Buck2, which builds and tests through NativeLink (docs/BUILD.md).
buck2 := "scripts/buck2"
# The pinned nightly's cargo, for formatting and Miri. rustup's `+toolchain`
# selects it; the Nix dev shell has no rustup and provides `cargo-nightly`
# instead (nix/toolchain.nix), at the same date.
nightly_cargo := `command -v cargo-nightly >/dev/null && echo cargo-nightly || echo cargo +nightly-2026-08-30`

# List the available repository tasks.
default:
    @just --list

# Export the editable logo and render its visual review sheet.
logo *ARGS:
    {{bounded}} 120 uv run --script scripts/logo.py "$@"

# The scene comes up in ~/.local/state/amux-marketing/<tree>/launch, its
# clones in /Users/Shared; `shots` starts it afresh, waits for the agents to
# come to rest and photographs the terminal client, and the phone when run
# under a simulator lease. Waits for agents, so only its steps are bounded.
# Start or photograph the README's launch scene (marketing/README.md).
scene *ARGS:
    scripts/py -B marketing/scene.py "$@"

# Compose the README's pictures from the scene's shots into images/readme/.
compose *ARGS:
    {{bounded}} 300 uv run --script marketing/compose.py "$@"

# Compose the App Store's screenshots from the scene's phone shots (marketing/README.md).
store *ARGS:
    {{bounded}} 300 uv run --script marketing/store.py "$@"

# Compose the Google Play Store's screenshots, feature graphic and icon from the scene's Android shots.
play-store *ARGS:
    {{bounded}} 300 uv run --script marketing/play_store.py "$@"

# The amux binary's release recipes: `just amux release`, `just amux deploy` (`just --list amux`).
mod amux 'crates/amux/justfile'

# The iPhone app's recipes: `just ios build`, `just ios verify`, ... (`just --list ios`).
mod ios 'apps/apple/justfile'

# The Mac app's recipes: `just mac build`, `just mac run` (`just --list mac`).
mod mac 'apps/apple/Mac/justfile'

# The Android app's recipes: `just android build`, `just android unit`, ... (`just --list android`).
mod android 'apps/android/justfile'

# The browser extension's recipes: `just browser build`, `just browser test`, ... (`just --list browser`).
mod browser 'js/extension/justfile'

# The tree's own amux, at .wt/bin/amux, where wt's `amux` finds it
# (.wt.toml); the path holds whatever Buck2 configuration built it.
# Build the desktop product binary.
build:
    mkdir -p .wt/bin && {{bounded}} 900 {{buck2}} build //crates/amux:amux --out .wt/bin/amux

# Check every workspace library and binary, and build every test.
check:
    {{bounded}} 3600 {{buck2}} build //crates/...

# Every test runs in a process of its own (build/test-runner.py), and a
# passing result is cached in NativeLink, so only tests whose inputs changed
# run. Target patterns narrow the run (`just test //crates/node:`); the live
# provider lanes are left out (`just live`). With no Linux worker in reach,
# the `nova-only` tests are skipped, and the run ends by naming them.
# Run the workspace's tests through Buck2.
test *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; [ $# -gt 0 ] || set -- //crates/...; status=0; {{bounded}} 3600 {{buck2}} test "$@" --exclude live -- --timeout 3600 || status=$?; scripts/py scripts/buck-config.py skipped "$@"; exit $status

# Run tests for one named workspace crate.
test-crate CRATE:
    {{bounded}} 3600 {{buck2}} test //crates/{{CRATE}}: --exclude live -- --timeout 3600

# `just goldens` re-records the goldens of the interpreters, the terminal
# client and the views; target names narrow it (`just goldens
# //crates/ui-view:integration`). The tests run here, in the checkout, so what
# they write lands in it; review the diff before committing it.
# Re-record the goldens.
goldens *ARGS:
    {{bounded}} 1800 scripts/py build/goldens.py "$@"

# CI's runners have no Buck2 yet, so CI runs Cargo and nextest through these
# twins of the recipes above, with nextest's flags (.github/workflows/ci.yml).
# Run workspace tests under cargo-nextest, as CI does.
cargo-test *ARGS: nextest
    if [ "${1-}" = -- ]; then shift; fi; scripts/workspace-test.sh "$@"

# Run one crate's tests under cargo-nextest, as CI does.
cargo-test-crate CRATE *ARGS: nextest
    crate=$1; shift; if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 900 cargo nextest run --locked -p "$crate" "$@"

# `just flaky [--load] TARGET TEST [COUNT]` runs the tests of TARGET
# (`//crates/node:integration`) whose names contain TEST COUNT times
# (default 50), one round after another, and stops at the first failure;
# --load keeps every core busy meanwhile.
# Make a test that fails now and then fail on demand.
flaky *ARGS:
    {{bounded}} 7200 scripts/flaky.sh "$@"

# Refuse to start a test recipe without the pinned cargo-nextest.
[private]
nextest:
    @cargo nextest --version >/dev/null 2>&1 || { echo "cargo-nextest is not installed: brew install cargo-nextest, or cargo install --locked cargo-nextest@0.9.150 (.config/nextest.toml requires it)" >&2; exit 1; }

# Cross-compile every store test target and run it on the leased, booted iOS
# simulator, after proving the store's integration binary carries SQLite itself.
test-store-ios:
    scripts/with iphone -- {{bounded}} 2300 scripts/py -B apps/apple/Tools/ios-store-test.py

# Compile every ordinary workspace test target without running it.
test-build:
    {{bounded}} 3600 {{buck2}} build //crates/...

# Run all workspace documentation tests.
doctest:
    {{bounded}} 600 cargo test --locked --workspace --doc

# Exercise TUI behavior.
test-tui:
    {{bounded}} 1200 {{buck2}} test //crates/tui: -- --timeout 1200

# Lint every workspace target with warnings denied: clippy over each of the
# workspace's own libraries, binaries and tests (build/checks/clippy.py),
# the live tests and perf binaries among them.
lint:
    {{bounded}} 3600 scripts/py build/checks/clippy.py

# Fail when a crate's BUCK file and its Cargo.toml disagree on its targets
# or dependencies (build/checks/parity.py).
parity:
    {{bounded}} 300 scripts/py build/checks/parity.py

# Fail when a test-only crate or a test feature reaches the desktop binary
# or the extension's WebAssembly as they ship (build/checks/shipping.py).
shipping-check:
    {{bounded}} 600 scripts/py build/checks/shipping.py

# Regenerate third-party/rust/BUCK from Cargo.toml and Cargo.lock with
# reindeer, after a dependency changes, then check every crate's BUCK still
# agrees with its manifest.
buckify:
    {{bounded}} 600 sh -c '"$(scripts/py scripts/buck-tools.py path reindeer)" buckify'
    {{just_executable()}} parity

# Lint as CI does, under Cargo. The qualification features only decide which
# targets build, so they are on here: the live tests and the perf binaries
# run by hand, never in CI, and this is what keeps them compiling.
cargo-lint:
    {{bounded}} 1200 cargo clippy --locked --workspace --all-targets --features qualification/live,qualification/perf -- -D warnings

# Format all Rust sources with the pinned nightly toolchain.
fmt:
    {{bounded}} 600 {{nightly_cargo}} fmt --all

# Check Rust formatting with the pinned nightly toolchain.
fmt-check:
    {{bounded}} 600 {{nightly_cargo}} fmt --all -- --check

# Regenerate the committed protobuf bindings.
protobuf:
    out=$({{bounded}} 600 scripts/built //crates/wire:protobuf) && rm -rf crates/wire/src/generated && cp -R "$out" crates/wire/src/generated && chmod -R u+w crates/wire/src/generated

# Check that committed protobuf bindings match their sources.
codegen-check:
    out=$({{bounded}} 600 scripts/built //crates/wire:protobuf) && if ! diff -r "$out" crates/wire/src/generated >/dev/null; then diff -ru crates/wire/src/generated "$out" | head -200; echo "committed wire output is stale; run 'just protobuf' and commit it" >&2; exit 1; fi

# CI's twin of `codegen-check`, generating with Cargo's xtask.
# Check the committed protobuf bindings with Cargo, as CI does.
cargo-codegen-check:
    {{bounded}} 600 scripts/codegen-check.sh

# Lint amux.v1, and check it still reads what the latest release wrote; with
# a ref, check against that ref instead. Needs buf (docs/BUILD.md).
proto-compat *ARGS:
    {{bounded}} 120 scripts/proto-compat.sh "$@"

# Check the store only grew since the latest release; with a ref, since that
# ref instead.
store-check *ARGS:
    {{bounded}} 60 scripts/py scripts/store-check.py "$@"

# Write the profile the upgrade test opens, as this commit's build leaves it,
# under crates/node/tests/fixtures/upgrade/. Run once at the tag of a release
# that changed what a profile holds on disk, and commit the result.
upgrade-fixture: nextest
    {{bounded}} 600 cargo nextest run --locked -p node --run-ignored only -E 'test(write_this_releases_profile)'

# Build the shipping binary and enforce the release dependency policy.
release-check *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 2400 cargo build --locked --release -p amux --bins "$@"
    if [ "${1-}" = -- ]; then shift; fi; scripts/release-policy-check.sh "$@"

# Install the pre-push credential scan (scripts/hooks/pre-push) for every worktree.
hooks:
    scripts/hooks/install

# Check the provider-free graph for iOS.
mobile-check:
    {{bounded}} 1200 cargo check --locked -p node -p client -p ui-state -p ui-runtime -p app-runtime -p app-embedded -p app-bridge --target aarch64-apple-ios
    {{bounded}} 1200 cargo check --locked -p node -p client -p ui-state -p ui-runtime -p app-runtime -p app-embedded -p app-bridge --target aarch64-apple-ios-sim

# Run workspace tests with isolated user configuration and no external network,
# on Linux, through sudo. The workspace test script bounds its own compile and
# run phases. The protocol models are pure computation that would take most of
# the run's bound, and run in their own CI job, as they do for the test jobs.
offline-test: nextest
    scripts/offline-check.sh scripts/workspace-test.sh --lib --tests --exclude models

# Build the product with the full-debug profile.
full-debug:
    {{bounded}} 900 cargo build --locked -p amux --bins --profile full-debug

# Render or inspect deterministic TUI evidence.
shot *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 600 {{buck2}} run --quiet //crates/shot:amux-shot -- "$@"

# Replay a report (its id, its prefix in the reports bucket, its file or its
# folder), an installation folder, or a relay's or the supervisor's report
# through every stage it carries, keeping it under target/repro/ (docs/DEBUGGING.md).
repro WHAT *ARGS:
    repro=$(scripts/built //crates/replay-support:repro) && cd "{{invocation_directory()}}" && "{{justfile_directory()}}/{{bounded}}" 900 "$repro" "$@"

# Interactive, so only the build and a capture are bounded; `just storybook
# capture --out DIR` draws every story to text and PNG (docs/TERMINAL.md).
# Step through the terminal client's named screens, rebuilding on save.
storybook *ARGS:
    #!/usr/bin/env sh
    set -eu
    storybook=$({{bounded}} 900 scripts/built //crates/shot:amux-storybook)
    case "${1-}" in
    capture) exec {{bounded}} 600 "$storybook" "$@" ;;
    *) exec "$storybook" --watch "$@" ;;
    esac

# Run one declared journey through its real client. System journeys are the
# built binaries on the fakes, printing their transcripts. `just journey
# terminal all` runs every terminal story, keeps going after a failure,
# prints one line per story to target/journeys/summary.txt and fails if any
# story did.
journey CLIENT NAME *ARGS:
    #!/usr/bin/env sh
    set -eu
    if [ "${3-}" = -- ]; then shift 3; else shift 2; fi
    case "{{CLIENT}}" in
    terminal)
        TESTNET_BINARIES=$({{bounded}} 900 scripts/built //crates/provider-fakes:binaries)
        testnet=$({{bounded}} 900 scripts/built //crates/testnet:testnet-bin)
        export TESTNET_BINARIES
        if [ "{{NAME}}" = all ]; then
            {{bounded}} 3000 "$testnet" journey terminal all
        else
            {{bounded}} 600 "$testnet" journey terminal "{{NAME}}"
        fi
        ;;
    system)
        # --keep leaves the install running for other clients to open.
        if [ "${1-}" = --keep ]; then export AMUX_JOURNEY_KEEP=1; fi
        AMUX_TEST_LOCAL=1 {{bounded}} 1500 {{buck2}} run //crates/amux:integration -- --exact "system_journeys::$(echo "{{NAME}}" | tr - _)"
        ;;
    *)
        echo "no {{CLIENT}} journeys: terminal or system" >&2
        exit 2
        ;;
    esac

# CI's twin of `just journey terminal`, building with Cargo.
# Run terminal journeys on Cargo's build, as CI does.
cargo-journey-terminal NAME:
    {{bounded}} 900 cargo build --locked --workspace --bin amux --bin fake-claude-pty --bin fake-claude-sdk --bin fake-codex --bin fake-acp --bin testnet
    if [ "{{NAME}}" = all ]; then {{bounded}} 3000 target/debug/testnet journey terminal all; else {{bounded}} 600 target/debug/testnet journey terminal "{{NAME}}"; fi

# The world comes up in ~/.local/state/amux-env/<tree>/NAME with the
# clients its file declares and is held, with the simulator when it has a
# phone, until Ctrl-C; drive it from that folder with `testnet ctl`.
# Interactive, so only its steps are bounded.
# Start a named world of real agents from environments/NAME.json (docs/TESTNET.md).
env NAME:
    scripts/py -B scripts/env.py {{NAME}}

# Fuzz the code that reads bytes amux did not write (the Claude and Codex
# decoders, the journal's frames and reader, redaction) for 30 seconds a
# target on the pinned nightly, seeded from the recorded fixtures; fails on a
# crash and prints the command that replays it. `--seconds N` and target
# names (`just fuzz-smoke journal_reader`) change the run. Needs cargo-fuzz.
fuzz-smoke *ARGS:
    {{bounded}} 1500 scripts/py scripts/fuzz-smoke.py "$@"

# Run the phone's C ABI tests under the pinned nightly's Miri, which checks
# every pointer handed across the boundary, read and freed again. Tests that
# start an installation or a tokio runtime are ignored under Miri, each with
# its reason. Needs the nightly's miri and rust-src components.
miri *ARGS:
    {{bounded}} 1500 {{nightly_cargo}} miri test --locked -p app-ffi "$@"

# Run the tests that reach unsafe code and C under the pinned nightly's
# AddressSanitizer (`just sanitize address`) or ThreadSanitizer (`just
# sanitize thread`, Linux only), with the standard library rebuilt under it;
# crate names after the kind narrow the run. scripts/sanitize.py says which
# suites run and what goes unchecked. Needs the nightly's rust-src component.
sanitize KIND *CRATES:
    {{bounded}} 2400 scripts/py scripts/sanitize.py --cargo "{{nightly_cargo}}" "$@"

# Fail when any state-machine core's dependency graph contains tokio, or any
# core source contains a lock, a spawned task, an async fn or an await.
cores-check:
    {{bounded}} 120 scripts/py scripts/cores-check.py

# Enforce production and test-infrastructure dependency boundaries.
dependency-policy:
    {{bounded}} 60 scripts/py scripts/check-dependency-policy.py

# Check every dependency against deny.toml with cargo-deny: no open security
# advisory or yanked release, only the licences it lists, and only crates.io
# and the rustls patch and wt as sources. Fetches the advisory database. Needs
# `cargo install --locked cargo-deny@0.20.2`.
deny:
    {{bounded}} 600 cargo deny --locked check

# Fail on any dependency no crate's code uses, so unused ones do not pile up
# again. A crate lists what the code cannot show it needs under
# `[package.metadata.cargo-shear]`. Needs
# `cargo install --locked cargo-shear@1.14.0`.
shear:
    {{bounded}} 300 cargo shear --locked --deny-warnings

# `just suite desktop-cli-system` runs exactly the Cargo tests that suite of
# tests/catalog.toml lists, on `just test`'s build; anything after the
# name goes to nextest (a test-name filter, `--no-capture`). The bound is the
# protocol models' at their CI bounds, one after another on a four-core machine.
# Run one catalogued suite's Cargo tests.
suite NAME *ARGS:
    {{bounded}} 2400 scripts/py -B scripts/tests-catalog.py run "$@"

# Print the suite catalogue with its boundaries, lanes and focused recipes.
tests-list:
    scripts/py -B scripts/tests-catalog.py list

# Validate every catalogued recipe, target, feature, manifest and baseline.
tests-check:
    {{bounded}} 120 scripts/py -B scripts/tests-catalog.py check

# The unit tests of the repository's own scripts. Pure Python; nothing is
# built. The phone's scripts have theirs in `just ios script-tests`.
script-tests:
    {{bounded}} 120 scripts/py -m unittest discover -s scripts/tests -p '*_test.py' -t scripts/tests

# Check the protocol models at their nightly bounds: each model's ignored
# test, one after another, every core each. Model names, the modules of the
# models' integration tests (`just models-nightly family`), narrow it to
# those models. The bound
# leaves the nightly job, one model each on a four-core runner, a quarter of an
# hour of its six for the build; the single daemon's peer alone took two hours
# on the twelve-core Mac.
models-nightly *MODELS: nextest
    if [ "${1-}" = -- ]; then shift; fi; models='[a-z_]+'; [ $# -eq 0 ] || models=$(echo "$*" | tr ' ' '|'); {{bounded}} 20700 cargo nextest run --locked --cargo-profile models -p models --profile nightly --run-ignored only --no-capture -E "binary_id(models::integration) & test(/^($models)::/)"

# Walk the testnet's simulated stories by seed: every suite tests/catalog.toml
# declares a simulated mode for, under TESTNET_MODE=simulated, a few seeds per
# story. `--seed N` reruns one seed (a failure prints its command), `--seeds N`
# walks N, and names after them narrow the run to matching stories.
sim *ARGS: nextest
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 1200 scripts/py scripts/sim.py "$@"

# Run every workspace test under cargo-llvm-cov and write line coverage per
# crate, with the commit it measured, to target/coverage/coverage.md. Needs
# `cargo install --locked cargo-llvm-cov@0.9.0` and the llvm-tools-preview
# rustup component.
coverage: nextest
    {{bounded}} 3600 scripts/py scripts/coverage.py

# Fail when a docs link or image does not resolve, a figure is unreferenced
# or not standalone, or docs/README.md does not list every page once.
docs-check:
    {{bounded}} 120 scripts/py scripts/docs-check.py

# Fail if an interpreter or the agent's provider handshake builds or reads
# provider JSON by hand instead of through the protocol crates.
typed-provider-check:
    {{bounded}} 60 scripts/py scripts/typed-provider-check.py

# Fail if a client crate decides something by an agent's kind or a
# provider's name outside the places its list allows.
client-kind-check:
    {{bounded}} 60 scripts/py scripts/client-kind-check.py

# Refuse fixture-regeneration flags before any asserted CI check runs.
[private]
no-update-flags:
    scripts/no-update-flags.sh

# The checks a change passes before it reaches main: the repository's
# checks, clippy over every crate, every crate's tests through Buck2 (only
# those whose inputs changed run; the rest come from NativeLink's cache, in
# this tree or any other), the phone apps' lints and script tests, the
# terminal journeys, and the browser extension's checks when js/ changed.
# The other platforms' builds and the Apple app's tests stay with
# `just ios verify` and CI (docs/CI.md, "`just premerge`").
# Run the checks a change passes before it reaches main.
premerge: no-update-flags fmt-check codegen-check proto-compat store-check dependency-policy deny shear cores-check typed-provider-check client-kind-check docs-check tests-check script-tests parity shipping-check lint
    {{just_executable()}} test
    {{just_executable()}} ios lint
    {{just_executable()}} ios script-tests
    {{just_executable()}} android lint
    {{just_executable()}} android script-tests
    {{just_executable()}} journey terminal all
    if git diff --quiet "$(git merge-base HEAD origin/main)" -- js; then echo "js: unchanged since main"; else {{just_executable()}} browser check; fi

# `just land "What the branch does"` merges this tree's HEAD onto a fresh
# origin/main, runs `just premerge` (and `just miri` when the phone's C ABI
# changed) on the merge and pushes it. With `git config amux.landInbox` set
# it goes through nova's landing queue, one landing at a time from every
# machine; `--local`, or no inbox, lands from a worktree of this machine's
# own. Nothing in this tree is touched (scripts/land.py).
# Land this branch on main.
land *ARGS:
    scripts/py -B scripts/land.py "$@"

# Run the live provider compatibility lane for one kind (claude_pty,
# claude_sdk or codex) and scenario (initialize, respond, decide, interrupt,
# resume, plan, questions, usage, catalogue, host_catalogue, permission, mode,
# attach, a headless-corpus scenario for claude_sdk, or all), under the
# operator's existing login. The acp kind takes an agent and a scenario
# instead (`just live acp grok cancel`, `just live acp all all`; lists may
# be comma-separated), drives the agent with a scripted ACP client through
# acp-probe, and joins each run to crates/interpret/fixtures/acp/live.
# With no arguments it reports not_run for every kind and starts nothing.
# Its tests are programs of their own, not libtest's, so each runs here as
# it is, with the operator's HOME, after Buck2 builds it and the binaries
# it starts.
live *ARGS:
    set -e; if [ "${1-}" = -- ]; then shift; fi; if [ $# -eq 0 ]; then for kind in claude_pty claude_sdk codex acp; do echo "live $kind: not_run (no scenario selected)"; done; exit 0; fi; kind=$1; shift; case "$kind" in claude_pty|claude_sdk|codex|acp) ;; *) echo "live: unknown kind $kind; known: claude_pty, claude_sdk, codex, acp" >&2; exit 2 ;; esac; {{bounded}} 3600 {{buck2}} run "//crates/qualification:${kind}_live" -- "$@"

# Waves of Sonnet testers, each on a test network of its own with real
# Claude and Codex agents, keeping evidence of what they find (docs/QA.md).
# By hand only, never in CI: `just qa --testers 2 --phone 1 --waves 1`.
# Run adversarial QA by hand: waves of testers on real agents (docs/QA.md).
qa *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 86400 scripts/py -B scripts/qa.py "$@"

# Qualify desktop performance on an enrolled machine. Pass --baseline to
# record the current release medians after every absolute budget passes.
perf *ARGS:
    set -e; if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 2400 cargo build --locked --release -p qualification --bin perf --features perf; {{bounded}} 2400 cargo build --locked --release -p amux -p provider-fakes --bins; {{bounded}} 1800 target/release/perf "$@"

# Records the release flood under an Instruments template on this Mac, for
# reading what the daemon's core thread spends its time on and waits for
# (docs/PERFORMANCE.md): `just perf-profile 'System Trace' 15s` stops the run
# 15 s in (the agents it leaves ignore SIGTERM and are killed), `just perf-profile 'Time
# Profiler'` records it whole. Trace and report land in target/perf-profile/.
perf-profile TEMPLATE LIMIT="300s":
    set -e; {{bounded}} 2400 cargo build --locked --release -p qualification --bin perf --features perf; {{bounded}} 2400 cargo build --locked --release -p amux -p provider-fakes --bins; mkdir -p target/perf-profile; out="target/perf-profile/$(date +%Y%m%d-%H%M%S)"; window=; case "{{TEMPLATE}}" in "System Trace") window="--window 300s" ;; esac; {{bounded}} 1800 xcrun xctrace record --template "{{TEMPLATE}}" $window --no-prompt --time-limit "{{LIMIT}}" --env TESTNET_BINARIES="$PWD/target/release" --target-stdout "$out.log" --output "$out.trace" --launch -- "$PWD/target/release/perf" --only flood || echo "xctrace exited $? (a time limit ends the run this way)"; pkill -9 -f "$PWD/target/release/amux agent" || true; pkill -9 -f "$PWD/target/release/fake-" || true; pkill -9 -f "$PWD/target/release/perf --cluster-warmer" || true; test -d "$out.trace"; echo "$out.trace"

# Records the loopback scaling curve of the production relay at 10, 100 and
# 1,000 connections (aggregate throughput, stream-open latency p50 and p99);
# --baseline records, --compare judges within noise (docs/PERFORMANCE.md).
relay-curve *ARGS:
    set -e; if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 2400 cargo build --locked --release -p qualification --bin relay-curve --features perf; {{bounded}} 1200 target/release/relay-curve "$@"
