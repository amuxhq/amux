#!/bin/sh
# nix-realize.sh FLAKE OUTPUT ROOT_FILE: build or fetch the flake's OUTPUT
# on this worker and write its store path to ROOT_FILE (build/toolchains/nix.bzl).
#
# The worker gives an action no HOME; Nix keeps its fetcher cache there, so
# it gets one in the action's own directory. Flake inputs already in the
# store (by their locked hash) aren't fetched again.
set -eu
flake=$(cd "$1" && pwd)
output=$2
root_file=$3
export HOME="$PWD/.nix-home"
mkdir -p "$HOME"
path=$(nix --extra-experimental-features 'nix-command flakes' build \
    --no-link --print-out-paths "path:$flake#$output")
# Nix may collect a path nothing roots. A cached result of this action
# would then name a toolchain that's gone, so where the worker names a
# directory for roots, the path gets one there.
if [ -n "${AMUX_NIX_GCROOTS-}" ]; then
    nix-store --add-root "$AMUX_NIX_GCROOTS/${path##*/}" --realise "$path" > /dev/null
fi
printf '%s\n' "$path" > "$root_file"
