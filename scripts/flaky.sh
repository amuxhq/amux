#!/bin/sh

# Usage: scripts/flaky.sh [--load] TEST [COUNT]
#
# Runs every test whose name contains TEST COUNT times (default 50), one
# run after another, and stops at the first failure with its output: how a
# test that fails now and then is made to fail on demand. --load keeps
# every core busy meanwhile, as a full parallel run or a slow CI runner
# does, which is where most timing failures show.

set -eu

load=
if [ "${1-}" = --load ]; then
    load=yes
    shift
fi
if [ $# -lt 1 ] || [ $# -gt 2 ]; then
    echo "usage: just flaky [--load] TEST [COUNT]" >&2
    exit 2
fi
test=$1
count=${2:-50}

if [ -n "$load" ]; then
    cores=$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)
    burners=
    i=0
    while [ "$i" -lt "$cores" ]; do
        sh -c 'while :; do :; done' &
        burners="$burners $!"
        i=$((i + 1))
    done
    # shellcheck disable=SC2086 # one word per burner's pid
    trap 'kill $burners 2>/dev/null' EXIT INT TERM
    echo "flaky: $cores busy loops running beside the test" >&2
fi

# The workspace scope and features `just test` builds with, so this reuses
# its build.
cargo nextest run --locked --workspace --features bundled --all-targets \
    --stress-count "$count" --max-fail 1 --failure-output immediate \
    -E "test($test)"
