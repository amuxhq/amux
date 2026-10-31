#!/bin/sh

set -eu

# Two phases, each under its own bound: compiling the test targets, then
# running them under cargo-nextest (.config/nextest.toml), whose setup script
# first builds the binaries the integration suites start. A bound is a hang
# detector sized above the honest duration of its one phase on the slowest
# CI runner (docs/CI.md records the measurements); it is never raised to hide
# a failing or slow test. One test that hangs is nextest's to end
# (`slow-timeout`); these bound the phases. The compile bound is the one
# `just test-build` carries.
compile_bound=1200
run_bound=1000

# The whole workspace unless the arguments name packages.
scope=--workspace
for arg do
    case "$arg" in
        --) break ;;
        -p|-p*|--package|--package=*) scope= ;;
    esac
done

# A Cargo target selection decides which test binaries are built and listed;
# a filter only narrows what runs in them.
targets=--all-targets
compile=yes
for arg do
    case "$arg" in
        --) break ;;
        --no-run) compile= run_bound=$compile_bound ;;
        --lib|--bins|--bin|--bin=*|--examples|--example|--example=*|--tests|--test|--test=*|--benches|--bench|--bench=*|--all-targets)
            targets=
            ;;
    esac
done

bounded="$(dirname "$0")/bounded"

if [ -n "$compile" ]; then
    "$bounded" "$compile_bound" cargo nextest run --locked $scope $targets --no-run "$@"
fi

exec "$bounded" "$run_bound" cargo nextest run --locked $scope $targets "$@"
