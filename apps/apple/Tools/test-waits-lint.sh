#!/bin/sh
# Refuses a test that waits a fixed number of turns for work to finish.
#
# Yielding twenty times and then checking is a guess at how long a task the
# code under test started takes to run: fast enough on a quiet Mac, and wrong
# on a loaded CI runner, where the check then fails for no fault of the code.
# A test waits for the work itself instead (the task an action returns, or
# the one a model holds for it) or, for something outside the process, for a
# condition to hold. Yielding until a condition holds is a wait for that
# condition and is allowed; a counted loop of yields is not.
#
# It checks itself first, against a file written to be wrong, so a rule that
# has stopped being able to fail says so instead of passing.

set -eu

roots=${AMUX_TEST_ROOTS:-"apps/apple/Packages apps/apple/AmuxAppTests apps/apple/AmuxComponentSnapshotTests apps/apple/AmuxUITests"}

counted() {
    grep -rnE --include='*.swift' 'for[[:space:]]+_[[:space:]]+in[^{]*\{[[:space:]]*await[[:space:]]+Task\.yield\(\)' "$@" || true
}

probe=$(mktemp -d)
trap 'rm -rf "$probe"' EXIT
printf 'func settle() async {\n    for _ in 0..<20 { await Task.yield() }\n}\n' >"$probe/Wrong.swift"
printf 'func until() async {\n    while pending { await Task.yield() }\n}\n' >"$probe/Right.swift"
if [ -z "$(counted "$probe/Wrong.swift")" ] || [ -n "$(counted "$probe/Right.swift")" ]; then
    echo "test-waits-lint: the rule no longer tells a counted wait from a conditional one" >&2
    exit 1
fi

# shellcheck disable=SC2086
found=$(counted $roots)
if [ -n "$found" ]; then
    echo "a test waits a fixed number of turns; wait for the task or the condition instead:"
    echo "$found" | sed 's/^/    /'
    exit 1
fi
echo "$roots: no test waits a fixed number of turns"
