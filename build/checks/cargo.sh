#!/bin/sh
# cargo.sh: run Cargo over the workspace inside an action (build/cargo.bzl),
# from the action's root, under the Linux toolchain.
#
#   cargo.sh fetch PATHS            every dependency into $OUT, a Cargo home
#   cargo.sh metadata PATHS         `cargo metadata --no-deps` into $OUT
#   cargo.sh run PATHS [HOME] CMD   CMD, offline, with the dependencies in HOME
#
# PATHS lists every file of the workspace (a `cargo_workspace` target's
# output). An action that carries only the manifests gets an empty file for
# each one it lacks, so Cargo finds every target where it is.
set -eu
what=$1
paths=$2
shift 2
while IFS= read -r path || [ -n "$path" ]; do
    if [ ! -e "$path" ]; then
        mkdir -p "$(dirname "$path")"
        : >"$path"
    fi
done <"$paths"
# The registry's crates are the action's inputs, which nothing may change:
# Cargo would otherwise collect the ones it hasn't used lately.
export CARGO_CACHE_AUTO_CLEAN_FREQUENCY=never
case $what in
fetch)
    # The one step that reaches the network: crates.io's index and every
    # crate the lockfile names, for every platform, and the git
    # dependencies. A function of the lockfile, but the index also says
    # which releases were yanked since, so the target declares the day
    # and fetches again on the next.
    CARGO_HOME=$PWD/$OUT cargo fetch --locked --quiet
    # Without the crates it unpacked: a check unpacks what it reads, and
    # they are seven times the size of the crates themselves.
    rm -rf "$OUT/registry/src"
    ;;
metadata)
    # The checkout's root in place of the action's, so the file is the
    # same from any machine and a reader puts its own root back.
    cargo metadata --no-deps --locked --offline --format-version 1 >"$TMPDIR/metadata.json"
    sed "s|$PWD|@ROOT@|g" "$TMPDIR/metadata.json" >"$OUT"
    ;;
run)
    if [ -d "${1-}" ]; then
        # A Cargo home of the check's own, holding the fetched one's
        # crates and index; Cargo unpacks each crate into it as it reads
        # it.
        fetched=$(cd "$1" && pwd)
        shift
        mkdir -p "$HOME/.cargo/registry"
        ln -s "$fetched/registry/cache" "$HOME/.cargo/registry/cache"
        cp -R "$fetched/registry/index" "$HOME/.cargo/registry/index"
        chmod -R u+w "$HOME/.cargo/registry/index"
        if [ -d "$fetched/git" ]; then
            cp -R "$fetched/git" "$HOME/.cargo/git"
            chmod -R u+w "$HOME/.cargo/git"
        fi
    fi
    # What it prints names files by their paths in the checkout, not the
    # action's directory on the worker.
    status=0
    CARGO_HOME=$HOME/.cargo CARGO_NET_OFFLINE=true "$@" >"$TMPDIR/output" 2>&1 || status=$?
    sed "s|$PWD/||g" "$TMPDIR/output"
    exit $status
    ;;
*)
    echo "cargo.sh: fetch, metadata or run, not $what" >&2
    exit 2
    ;;
esac
