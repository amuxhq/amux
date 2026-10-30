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

# The workspace run's desktop SQLite linkage, so this build reuses what the
# test build compiled rather than unifying features of its own.
cargo build --locked --features bundled \
    -p amux -p provider-fakes -p fake-amux -p agent --bins --example amux_stand_in

target=${CARGO_TARGET_DIR:-target}
case "$target" in
    /*) ;;
    *) target="$PWD/$target" ;;
esac
echo "TESTNET_BINARIES=$target/debug" >> "$NEXTEST_ENV"
