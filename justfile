set shell := ["sh", "-cu"]
set windows-shell := ["sh", "-cu"]
set positional-arguments

export AMUX_GIT_SHA := `git rev-parse HEAD`

# Wall-clock bound for every recipe; portable where GNU timeout is absent.
bounded := "scripts/bounded"
# The pinned nightly's cargo, for formatting and Miri. rustup's `+toolchain`
# selects it; the Nix dev shell has no rustup and provides `cargo-nightly`
# instead (nix/toolchain.nix), at the same date.
nightly_cargo := `command -v cargo-nightly >/dev/null && echo cargo-nightly || echo cargo +nightly-2026-08-30`
# Desktop roots keep naming the feature explicitly so an invocation that
# disables defaults cannot silently switch the SQLite linkage policy.
desktop_features := "--features bundled"

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

# The iPhone app's recipes: `just ios build`, `just ios verify`, ... (`just --list ios`).
mod ios 'apps/apple/justfile'

# Build the desktop product binaries.
build:
    {{bounded}} 900 cargo build --locked -p amux --bins {{desktop_features}}

# Check every workspace library and binary.
check:
    {{bounded}} 900 cargo check --locked --workspace --lib --bins {{desktop_features}}

# Every test runs in a process of its own (.config/nextest.toml).
# Run workspace tests under cargo-nextest, preserving explicit Cargo target selections.
test *ARGS: nextest
    if [ "${1-}" = -- ]; then shift; fi; scripts/workspace-test.sh "$@"

# Run tests for one named workspace crate.
test-crate CRATE *ARGS: nextest
    crate=$1; shift; if [ "${1-}" = -- ]; then shift; fi; feature=; if grep -q '^bundled *=' "crates/$crate/Cargo.toml"; then feature='{{desktop_features}}'; fi; {{bounded}} 900 cargo nextest run --locked -p "$crate" $feature "$@"

# `just flaky [--load] TEST [COUNT]` runs every test whose name contains TEST
# COUNT times (default 50), one run after another, and stops at the first
# failure; --load keeps every core busy meanwhile.
# Make a test that fails now and then fail on demand.
flaky *ARGS: nextest
    {{bounded}} 7200 scripts/flaky.sh "$@"

# Refuse to start a test recipe without the pinned cargo-nextest.
[private]
nextest:
    @cargo nextest --version >/dev/null 2>&1 || { echo "cargo-nextest is not installed: brew install cargo-nextest, or cargo install --locked cargo-nextest (.config/nextest.toml names the version)" >&2; exit 1; }

# Exercise the profile store: its reader and writer suite, retention and the SQLite file.
test-store *ARGS: nextest
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 1200 cargo nextest run --locked -p store --features bundled "$@"

# Cross-compile every store test target and run it on the leased, booted iOS
# simulator, after proving the SQLite suite's binary carries SQLite itself.
test-store-ios:
    scripts/with iphone -- {{bounded}} 2300 scripts/python -B apps/apple/Tools/ios-store-test.py

# Exercise the pure store-backed reducer lifecycle and recorded UI specs.
test-ui *ARGS: nextest
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 1200 cargo nextest run --locked -p ui-state "$@"

# Compile every ordinary workspace test target without running it.
test-build:
    {{bounded}} 1200 cargo test --locked --workspace --lib --tests --no-run {{desktop_features}}

# Run all workspace documentation tests.
doctest:
    {{bounded}} 600 cargo test --locked --workspace --doc {{desktop_features}}

# Run the whole-daemon and UI-state specification suites.
spec *ARGS: nextest
    if [ "${1-}" = -- ]; then shift; fi; scripts/spec-test.sh "$@"

# Exercise TUI behavior.
test-tui *ARGS: nextest
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 1200 cargo nextest run --locked -p tui --features fixtures "$@"

# Lint every workspace target with warnings denied. The qualification
# features only decide which targets build, so they are on here: the live
# tests and the perf binaries run by hand, never in CI, and this is what
# keeps them compiling.
lint:
    {{bounded}} 1200 cargo clippy --locked --workspace --all-targets {{desktop_features}} --features qualification/live,qualification/perf -- -D warnings

# Format all Rust sources with the pinned nightly toolchain.
fmt:
    {{bounded}} 600 {{nightly_cargo}} fmt --all

# Check Rust formatting with the pinned nightly toolchain.
fmt-check:
    {{bounded}} 600 {{nightly_cargo}} fmt --all -- --check

# Regenerate the committed protobuf bindings.
protobuf:
    {{bounded}} 600 cargo run --locked -p xtask -- codegen

# Check that committed protobuf bindings match their sources.
codegen-check:
    {{bounded}} 600 scripts/codegen-check.sh

# Check that amux.v1 still reads what the latest release wrote; with a ref,
# check against that ref instead.
proto-compat *ARGS:
    {{bounded}} 600 cargo run --locked -p xtask -- proto-compat "$@"

# Build the shipping binary and enforce the release dependency policy.
release-check *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 2400 cargo build --locked --release -p amux --bins --no-default-features {{desktop_features}} "$@"
    if [ "${1-}" = -- ]; then shift; fi; scripts/release-policy-check.sh "$@"

# `just release 0.8.0` writes the version, checks the release build, commits,
# tags v0.8.0 and pushes; the tag's workflow builds the binaries into a
# GitHub Release.
# Cut a release of the amux binary.
release VERSION:
    {{bounded}} 1800 cargo run --locked -q -p xtask -- release cut {{VERSION}}

# `just deploy 0.8.0` signs the release's binaries with the keychain's key and
# publishes the stable manifest; `--channel preview` and `--rollout 10`
# choose who takes it.
# Put a cut release in front of a channel.
deploy VERSION *ARGS:
    shift; if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 3600 cargo run --locked -q -p xtask -- release deploy {{VERSION}} "$@"

# `just release-key generate` makes one in the login keychain and prints its
# public half; `just release-key public` prints it again.
# The release signing key.
release-key *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 600 cargo run --locked -q -p xtask -- release key "$@"

# Install the pre-push credential scan (scripts/hooks/pre-push) for every worktree.
hooks:
    scripts/hooks/install

# Check the provider-free graph used by embedded clients.
embedded-check:
    {{bounded}} 900 cargo check --locked -p node -p client -p ui-state -p ui-runtime -p app-runtime -p app-embedded

# Exercise the public embedded owner and client boundary.
embedded-test: nextest
    {{bounded}} 900 cargo nextest run --locked -p app-embedded --test embedded

# Check the provider-free graph for iOS devices and simulators.
mobile-check:
    {{bounded}} 1200 cargo check --locked -p node -p client -p ui-state -p ui-runtime --target aarch64-apple-ios
    {{bounded}} 1200 cargo check --locked -p node -p client -p ui-state -p ui-runtime --target aarch64-apple-ios-sim

# Run workspace tests with isolated user configuration and no external network,
# on Linux, through sudo. The workspace test script bounds its own compile and
# run phases. The protocol models are pure computation that would take most of
# the run's bound, and run in their own CI job, as they do for the test jobs.
offline-test: nextest
    scripts/offline-check.sh scripts/workspace-test.sh --lib --tests --no-fail-fast --exclude models

# Build the product with the full-debug profile.
full-debug:
    {{bounded}} 900 cargo build --locked -p amux --bins --profile full-debug {{desktop_features}}

# Render or inspect deterministic TUI evidence.
shot *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 600 cargo run --locked --quiet -p shot --bin amux-shot -- "$@"

# Interactive, so only the build and a capture are bounded; `just storybook
# capture --out DIR` draws every story to text and PNG (docs/TERMINAL.md).
# Step through the terminal client's named screens, rebuilding on save.
storybook *ARGS:
    #!/usr/bin/env sh
    set -eu
    {{bounded}} 900 cargo build --locked -q -p shot --bin amux-storybook
    case "${1-}" in
    capture) exec {{bounded}} 600 target/debug/amux-storybook "$@" ;;
    *) exec target/debug/amux-storybook --watch "$@" ;;
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
        {{bounded}} 900 cargo build --locked -p amux -p provider-fakes -p testnet --bins {{desktop_features}}
        if [ "{{NAME}}" = all ]; then
            {{bounded}} 3000 target/debug/testnet journey terminal all
        else
            {{bounded}} 600 target/debug/testnet journey terminal "{{NAME}}"
        fi
        ;;
    system)
        # --keep leaves the install running for other clients to open.
        if [ "${1-}" = --keep ]; then export AMUX_JOURNEY_KEEP=1; fi
        {{bounded}} 1500 cargo nextest run --locked -p amux {{desktop_features}} --test system_journeys --no-capture -- --exact "$(echo "{{NAME}}" | tr - _)"
        ;;
    *)
        echo "no {{CLIENT}} journeys: terminal or system" >&2
        exit 2
        ;;
    esac

# Replay a headless Claude recording through the interpreter and print JSON:
# the snapshot count, the largest and median encoded snapshot, and per tool
# call the bytes its items and appends took. Offline and deterministic.
wire-size RECORDING:
    {{bounded}} 600 cargo run --locked --quiet -p replay-support --bin wire-size -- "{{RECORDING}}"

# Generate and verify the complete TUI evidence bundle.
tui-evidence *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 1800 scripts/tui-evidence "$@"

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
# and the rustls patch as sources. Fetches the advisory database. Needs
# `cargo install cargo-deny`.
deny:
    {{bounded}} 600 cargo deny --locked check

# Fail on any dependency no crate's code uses, so unused ones do not pile up
# again. A crate lists what the code cannot show it needs under
# `[package.metadata.cargo-shear]`. Needs `cargo install cargo-shear`.
shear:
    {{bounded}} 300 cargo shear --locked --deny-warnings

# Warm the product and ordinary test build graphs used by new worktrees.
warm: build test-build

# Print the suite catalogue with its boundaries, lanes and focused recipes.
tests-list:
    scripts/python -B scripts/tests-catalog.py list

# Validate every catalogued recipe, target, feature, manifest and baseline.
tests-check:
    {{bounded}} 120 scripts/python -B scripts/tests-catalog.py check

# The unit tests of the repository's own scripts. Pure Python; nothing is
# built. The phone's scripts have theirs in `just ios script-tests`.
script-tests:
    {{bounded}} 120 scripts/py -m unittest discover -s scripts/tests -p '*_test.py' -t scripts/tests

# List every parked property of the protocol models, with its reason, and fail
# while any remains: the landing gate for decisions still waiting.
parked-check:
    {{bounded}} 600 cargo run --locked -q -p models --bin parked-check

# Check the protocol models at their nightly bounds: each model's ignored
# test, one test binary after another, every core each. Names of test
# targets (`just models-nightly family`) narrow it to those models. The bound
# leaves the nightly job, one model each on a four-core runner, a quarter of an
# hour of its six for the build; the single daemon's peer alone took two hours
# on the twelve-core Mac.
models-nightly *TARGETS: nextest
    if [ "${1-}" = -- ]; then shift; fi; targets=--tests; [ $# -eq 0 ] || targets=$(printf -- '--test %s ' "$@"); {{bounded}} 20700 cargo nextest run --locked -p models --profile nightly $targets --run-ignored only --no-capture

# Walk the testnet's simulated stories by seed: every suite tests/catalog.toml
# declares a simulated mode for, under TESTNET_MODE=simulated, a few seeds per
# story. `--seed N` reruns one seed (a failure prints its command), `--seeds N`
# walks N, and names after them narrow the run to matching stories.
sim *ARGS: nextest
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 1200 scripts/py scripts/sim.py sim "$@"

# The nightly run: walk `--seeds N` seeds per simulated story, then check the
# protocol models at `--bounds nightly` (the default), `ci` or `none`, and
# print a report of seeds walked, time and failing seeds.
nightly *ARGS: nextest
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 14400 scripts/py scripts/sim.py nightly "$@"

# Run every workspace test under cargo-llvm-cov with the desktop features and
# write line coverage per crate, with the commit it measured, to
# tests/coverage.md. Needs `cargo install cargo-llvm-cov` and the
# llvm-tools-preview rustup component.
coverage: nextest
    {{bounded}} 3600 scripts/py scripts/coverage.py

# Fail when a contract in tests/contracts.toml names no test, or a test or a
# model property that does not exist; `--table PATH` also writes the coverage
# table as Markdown.
contracts-check *ARGS:
    {{bounded}} 300 scripts/python -B scripts/contracts-check.py "$@"

# Fail when a docs link or image does not resolve, a figure is unreferenced
# or not standalone, or docs/README.md does not list every page once.
docs-check:
    {{bounded}} 120 scripts/py scripts/docs-check.py

# Fail if an interpreter or the agent's provider handshake builds or reads
# provider JSON by hand instead of through the protocol crates.
typed-provider-check:
    {{bounded}} 60 scripts/py scripts/typed-provider-check.py

# Refuse fixture-regeneration flags before any asserted CI check runs.
[private]
no-update-flags:
    scripts/no-update-flags.sh

# Run the same task sequence exercised across continuous-integration jobs.
ci: no-update-flags check lint fmt-check codegen-check proto-compat dependency-policy deny shear cores-check typed-provider-check docs-check tests-check script-tests test contracts-check doctest release-check embedded-check embedded-test mobile-check

# Run the live provider compatibility lane for one kind (claude_pty,
# claude_sdk or codex) and scenario (initialize, respond, decide, interrupt,
# resume, plan, questions, usage, catalogue, host_catalogue, permission, mode,
# attach, or all), under the operator's existing login. With no arguments
# it reports not_run for every kind and starts nothing. Its tests are not
# libtest's, so they run under cargo test, with the binaries they start built
# first.
live *ARGS:
    set -e; if [ "${1-}" = -- ]; then shift; fi; if [ $# -eq 0 ]; then for kind in claude_pty claude_sdk codex; do echo "live $kind: not_run (no scenario selected)"; done; exit 0; fi; kind=$1; shift; case "$kind" in claude_pty|claude_sdk|codex) ;; *) echo "live: unknown kind $kind; known: claude_pty, claude_sdk, codex" >&2; exit 2 ;; esac; {{bounded}} 900 cargo build --locked --features bundled -p amux -p provider-fakes -p claude-specs -p codex-specs --bins; TESTNET_BINARIES="$PWD/target/debug" {{bounded}} 3600 cargo test --locked -p qualification --features bundled,live --test "${kind}_live" -- "$@"

# Waves of Sonnet testers, each on a test network of its own with real
# Claude and Codex agents, keeping evidence of what they find (docs/QA.md).
# By hand only, never in CI: `just qa --testers 2 --phone 1 --waves 1`.
# Run adversarial QA by hand: waves of testers on real agents (docs/QA.md).
qa *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 86400 scripts/python -B scripts/qa.py "$@"

# Qualify desktop performance on an enrolled machine. Pass --baseline to
# record the current release medians after every absolute budget passes.
perf *ARGS:
    set -e; if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 2400 cargo build --locked --release -p qualification --bin perf --features bundled,perf; {{bounded}} 2400 cargo build --locked --release -p amux -p provider-fakes --bins; {{bounded}} 1800 target/release/perf "$@"

# Records the release flood under an Instruments template on this Mac, for
# reading what the daemon's core thread spends its time on and waits for
# (docs/PERFORMANCE.md): `just perf-profile 'System Trace' 15s` stops the run
# 15 s in (the agents it leaves ignore SIGTERM and are killed), `just perf-profile 'Time
# Profiler'` records it whole. Trace and report land in target/perf-profile/.
perf-profile TEMPLATE LIMIT="300s":
    set -e; {{bounded}} 2400 cargo build --locked --release -p qualification --bin perf --features bundled,perf; {{bounded}} 2400 cargo build --locked --release -p amux -p provider-fakes --bins; mkdir -p target/perf-profile; out="target/perf-profile/$(date +%Y%m%d-%H%M%S)"; window=; case "{{TEMPLATE}}" in "System Trace") window="--window 300s" ;; esac; {{bounded}} 1800 xcrun xctrace record --template "{{TEMPLATE}}" $window --no-prompt --time-limit "{{LIMIT}}" --env TESTNET_BINARIES="$PWD/target/release" --target-stdout "$out.log" --output "$out.trace" --launch -- "$PWD/target/release/perf" --only flood || echo "xctrace exited $? (a time limit ends the run this way)"; pkill -9 -f "$PWD/target/release/amux agent" || true; pkill -9 -f "$PWD/target/release/fake-" || true; pkill -9 -f "$PWD/target/release/perf --cluster-warmer" || true; test -d "$out.trace"; echo "$out.trace"

# Records the loopback scaling curve of the production relay at 10, 100 and
# 1,000 connections (aggregate throughput, stream-open latency p50 and p99);
# --baseline records, --compare judges within noise (docs/PERFORMANCE.md).
relay-curve *ARGS:
    set -e; if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 2400 cargo build --locked --release -p qualification --bin relay-curve --features bundled,perf; {{bounded}} 1200 target/release/relay-curve "$@"
