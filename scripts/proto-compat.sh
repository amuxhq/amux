#!/bin/sh
# Lints the protobuf schemas, then holds amux.v1 to the rules in docs/WIRE.md
# against the newest release tag behind HEAD, or against a ref named on the
# command line. The comparison starts at 0.11.0, the first release to carry
# the services as they now stand: releases before it are not checked
# against. Comparing with a release needs its tag, which a shallow clone
# lacks.
#
# `--baseline DIR` compares with DIR/proto, the release named in DIR/ref,
# rather than with git's history: how the check runs as a Buck2 test, which
# has none (build/checks/inputs.py writes DIR). $BUF names buf when it isn't
# the one on PATH.
set -eu
buf=${BUF:-buf}

"$buf" lint

floor=0.11.0
against=
named=
if [ "${1-}" = --baseline ]; then
    baseline=$(cat "$2/ref")
    if [ -z "$baseline" ]; then
        echo "this clone has no release tag to compare with" >&2
        exit 1
    fi
    against=$2
elif [ -n "${1-}" ]; then
    baseline=$1
    named=1
else
    baseline=$(git describe --tags --abbrev=0 --match 'v[0-9]*')
fi
if [ -z "$named" ]; then
    version=${baseline#v}
    oldest=$(printf '%s\n%s\n' "$version" "$floor" | sort -V | head -n 1)
    if [ "$oldest" != "$floor" ]; then
        echo "the newest release, $baseline, predates $floor, the first to carry these schemas; nothing to check against"
        exit 0
    fi
fi

# The rules are this tree's, whatever the baseline's buf.yaml says or whether
# it has one.
"$buf" breaking --against "${against:-.git#ref=$baseline}" --against-config buf.yaml
echo "amux.v1 still reads what $baseline wrote"
