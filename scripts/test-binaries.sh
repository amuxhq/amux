#!/bin/sh

# The nextest setup script (.config/nextest.toml): builds every binary the
# integration suites start — amux, the fake providers, fake-amux and the
# agent's install-path stand-in — once, before any test runs, and names
# their directory to the tests in TESTNET_BINARIES. A test never runs cargo
# itself: one build per test process would queue every test on cargo's lock
# on the target directory.
#
# A caller that built the binaries itself, with flags of its own (the
# sanitizers, a release perf run), names them in TESTNET_BINARIES already,
# and this builds nothing.

set -eu

if [ -n "${TESTNET_BINARIES-}" ]; then
    exit 0
fi

# The desktop SQLite linkage, named as the workspace run names it.
cargo build --locked --features bundled \
    -p amux -p provider-fakes -p fake-amux -p agent --bins --example amux_stand_in

# The tests read the directory as a native path: on Windows, Git's sh
# spells the working directory /d/a/..., which a Windows process cannot
# open, and `pwd -W` spells it D:/a/....
here=$(pwd -W 2>/dev/null || pwd)
target=${CARGO_TARGET_DIR:-target}
case "$target" in
    /* | [A-Za-z]:*) ;;
    *) target="$here/$target" ;;
esac
echo "TESTNET_BINARIES=$target/debug" >> "$NEXTEST_ENV"
