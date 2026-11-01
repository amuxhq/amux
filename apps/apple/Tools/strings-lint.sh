#!/bin/sh
set -eu
cd "$(dirname "$0")/../../.."
scripts/py -B apps/apple/Tools/strings_lint_test.py
exec scripts/py -B apps/apple/Tools/strings_lint.py "$@"
