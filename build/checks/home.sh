#!/bin/sh
# home.sh COMMAND...: run a repository check (build/checks.bzl) with a HOME
# and TMPDIR of its own. A worker's actions share its account's HOME, or
# have none, and the tools the checks run keep caches there (buf's, npm's),
# which must neither leak between actions nor be missing. They live under
# /tmp, not the action's directory, because a check that starts amux puts
# sockets under TMPDIR, and the action's path would take a socket's past
# the 104 bytes macOS allows (108 on Linux).
set -eu
scratch=$(mktemp -d /tmp/amux-check.XXXXXX)
trap 'rm -rf "$scratch"' EXIT
HOME=$scratch/home
TMPDIR=$scratch/tmp
XDG_CACHE_HOME=$HOME/.cache
mkdir -p "$HOME" "$TMPDIR"
export HOME TMPDIR XDG_CACHE_HOME
"$@"
