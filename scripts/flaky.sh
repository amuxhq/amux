#!/bin/sh

# Usage: scripts/flaky.sh [--load] TARGET TEST [COUNT]
#
# Runs the tests of the Buck2 test target TARGET whose names contain TEST
# COUNT times (default 50), one round after another, and stops at the round
# a test fails in, with its output (build/test-runner.py): how a
# test that fails now and then is made to fail on demand. --load keeps
# every core busy meanwhile, as a full parallel run or a slow CI runner
# does, which is where most timing failures show.

set -eu

load=
if [ "${1-}" = --load ]; then
    load=yes
    shift
fi
if [ $# -lt 2 ] || [ $# -gt 3 ]; then
    echo "usage: just flaky [--load] TARGET TEST [COUNT], as in just flaky //crates/node:integration reconnects" >&2
    exit 2
fi
target=$1
test=$2
count=${3:-50}
buck2="$(dirname "$0")/buck2"

if [ -n "$load" ]; then
    # Built before the load starts: the busy loops would otherwise starve
    # the build and the materializing of its binaries, which once sat for
    # twenty minutes beside other builds before a single round ran.
    "$buck2" build "$target"
    cores=$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)
    burners=
    i=0
    while [ "$i" -lt "$cores" ]; do
        # A burner ends by itself once this script is gone: killed outright,
        # the script runs no trap, and loops left behind keep every core
        # busy until someone finds them.
        sh -c 'n=0; while :; do n=$((n + 1)); if [ "$n" -ge 200000 ]; then n=0; kill -0 "$1" 2>/dev/null || exit 0; fi; done' burner $$ &
        burners="$burners $!"
        i=$((i + 1))
    done
    # shellcheck disable=SC2086 # one word per burner's pid
    trap 'kill $burners 2>/dev/null' EXIT INT TERM
    echo "flaky: $cores busy loops running beside the test" >&2
fi

# Run here rather than in the build's sandbox, so nothing is cached and
# every round runs.
AMUX_TEST_REPEAT=$count exec "$buck2" run "$target" -- "$test"
