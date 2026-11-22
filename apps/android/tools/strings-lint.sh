#!/bin/sh
set -eu
cd "$(dirname "$0")/../../.."
exec scripts/py -B apps/android/tools/strings_lint.py "$@"
