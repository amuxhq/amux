#!/bin/bash
# nix-run.sh ROOT_FILE PATH ARGS...: run PATH (`bin/rustc`) under the Linux
# toolchain whose store path ROOT_FILE holds (build/toolchains/nix.bzl),
# with the toolchain's bin directory first on PATH, so whatever the tool
# starts by name (a test's tmux and git, a compiler's assembler) is the
# toolchain's too. `nix-run.sh ROOT_FILE -- COMMAND ARGS...` runs a command
# that isn't the toolchain's (a test binary) with that PATH.
#
# Bash, not sh: Debian's sh (dash) drops environment variables whose names
# aren't shell identifiers, and Cargo's names have hyphens
# (CARGO_BIN_EXE_fake-acp).
set -eu
root=$(cat "$1")
tool=$2
shift 2
if [ ! -d "$root" ]; then
    echo "nix-run: the toolchain $root is not on this worker: Nix collected it after its realization was cached. Realize it again with 'scripts/buck2 build //:nix-linux --no-remote-cache'." >&2
    exit 1
fi
PATH="$root/bin:$PATH"
export PATH
# tmux runs a pane's command with $SHELL, and the worker's account has no
# login shell (its SHELL is nologin).
SHELL=/bin/sh
export SHELL
if [ "$tool" = -- ]; then
    exec "$@"
fi
exec "$root/$tool" "$@"
