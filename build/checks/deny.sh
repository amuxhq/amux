#!/bin/sh
# deny.sh: cargo-deny over the workspace's dependency graph (the root BUCK's
# `deny`), offline, from the action's root. build/checks/cargo.sh runs it
# with CARGO_HOME holding the day's registry (//:cargo-registry), whose
# crates.io index entries say which releases were yanked; the advisory
# database is the snapshot build/checks/inputs.py wrote into
# build/checks/inputs/deny/.
set -eu
root=$PWD
inputs=$root/build/checks/inputs/deny
for file in advisory-db.tar advisory-db.ref; do
    if [ ! -f "$inputs/$file" ]; then
        echo "deny: build/checks/inputs/deny/$file is missing; build/checks/inputs.py writes it (it says why it could not)" >&2
        exit 1
    fi
done
# Cargo's resolved graph with every feature.
cargo metadata --all-features --locked --format-version 1 >"$TMPDIR/metadata.json"
# Where cargo-deny looks for the database its config names (the default,
# github.com/rustsec/advisory-db): a directory named for the URL's hash.
# CARGO_HOME is the check's HOME's (build/checks/home.sh), where some
# cargo-deny releases look whatever CARGO_HOME says.
db=$CARGO_HOME/advisory-dbs/advisory-db-3157b0e258782691
mkdir -p "$db"
tar -xf "$inputs/advisory-db.tar" -C "$db"
# cargo-deny takes the database's age from its last commit, so the snapshot
# becomes a repository again with the commit's own date.
date=$(sed -n 2p "$inputs/advisory-db.ref")
git -C "$db" init -q
git -C "$db" add -A
GIT_AUTHOR_DATE=$date GIT_COMMITTER_DATE=$date git -C "$db" -c user.name=snapshot -c user.email=snapshot@invalid commit -q -m "$(sed -n 1p "$inputs/advisory-db.ref")"
exec cargo-deny --offline check --metadata-path "$TMPDIR/metadata.json" --config "$root/deny.toml"
