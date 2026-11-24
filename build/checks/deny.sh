#!/bin/sh
# deny.sh: cargo-deny over the workspace's dependency graph, from what
# build/checks/inputs.py wrote into build/checks/inputs/deny/ (the root
# BUCK's `deny`): Cargo's graph, the workspace's manifests and a snapshot
# of the advisory database. Run from the action's root, offline.
set -eu
root=$PWD
inputs=$root/build/checks/inputs/deny
for file in metadata.json advisory-db.tar advisory-db.ref index/config.json; do
    if [ ! -f "$inputs/$file" ]; then
        echo "deny: build/checks/inputs/deny/$file is missing; build/checks/inputs.py writes it (it says why it could not)" >&2
        exit 1
    fi
done
# Cargo's home is the check's own HOME's (build/checks/home.sh), where some
# cargo-deny releases look for the advisory database whatever CARGO_HOME says.
home=$HOME/.cargo
# Where cargo-deny looks for the database its config names (the default,
# github.com/rustsec/advisory-db): a directory named for the URL's hash.
db=$home/advisory-dbs/advisory-db-3157b0e258782691
mkdir -p "$db"
sed "s|@ROOT@|$root|g" "$inputs/metadata.json" >"$home/metadata.json"
tar -xf "$inputs/advisory-db.tar" -C "$db"
# crates.io's index entries, where Cargo keeps them, for the yanked check.
index=$home/registry/index/index.crates.io-1949cf8c6b5b557f
mkdir -p "$index"
cp "$inputs/index/config.json" "$index/"
cp -R "$inputs/index/cache" "$index/.cache"
chmod -R u+w "$home"
# The lock Cargo and cargo-deny take on its home.
touch "$home/.package-cache"
# cargo-deny takes the database's age from its last commit, so the snapshot
# becomes a repository again with the commit's own date.
date=$(sed -n 2p "$inputs/advisory-db.ref")
git -C "$db" init -q
git -C "$db" add -A
GIT_AUTHOR_DATE=$date GIT_COMMITTER_DATE=$date git -C "$db" -c user.name=snapshot -c user.email=snapshot@invalid commit -q -m "$(sed -n 1p "$inputs/advisory-db.ref")"
# Offline: everything it would fetch is above. It still asks Cargo to fetch
# the crates first, which can't work here and isn't needed (it reads the
# graph from the metadata); it logs that as an error and goes on.
cd "$inputs/workspace"
CARGO_HOME=$home exec cargo-deny --offline check --metadata-path "$home/metadata.json" --config "$root/deny.toml"
