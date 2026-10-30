#!/bin/sh

set -eu

target=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --target)
            [ "$#" -ge 2 ] || {
                echo "release-policy-check: --target needs a value" >&2
                exit 2
            }
            target=$2
            shift 2
            ;;
        --target=*)
            target=${1#--target=}
            shift
            ;;
        *)
            shift
            ;;
    esac
done

if [ -n "$target" ]; then
    binary="target/$target/release/amux"
else
    binary="target/release/amux"
fi
# Windows products carry an extension whether or not a --target was given.
# Prefer it when it exists: Git Bash finds and runs amux.exe when asked for
# amux, but the Python inspector below uses Win32 names and does not.
if [ -e "$binary.exe" ]; then
    binary="$binary.exe"
fi

[ -x "$binary" ] || {
    echo "release-policy-check: release product is missing: $binary" >&2
    exit 1
}

scripts/python -B scripts/sqlite_linkage.py "$binary"

echo "release policy holds"
