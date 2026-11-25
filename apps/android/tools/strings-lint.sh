#!/bin/sh
set -eu
cd "$(dirname "$0")/../../.."
# Through sh, not by its executable bit, which a file Buck2 sends to the
# Linux worker from a Windows checkout lacks (docs/BUILD.md).
exec sh scripts/py -B apps/android/tools/strings_lint.py "$@"
