#!/bin/sh
# Lints the protobuf schemas, then holds amux.v1 to the rules in docs/WIRE.md
# against the newest release tag behind HEAD, or against a ref named on the
# command line. The comparison starts at 0.11.0, the first release to carry
# the services as they now stand: releases before it are not checked
# against. Comparing with a release needs its tag, which a shallow clone
# lacks.
set -eu

buf lint

floor=0.11.0
baseline=${1:-}
if [ -z "$baseline" ]; then
    baseline=$(git describe --tags --abbrev=0 --match 'v[0-9]*')
    version=${baseline#v}
    oldest=$(printf '%s\n%s\n' "$version" "$floor" | sort -V | head -n 1)
    if [ "$oldest" != "$floor" ]; then
        echo "the newest release, $baseline, predates $floor, the first to carry these schemas; nothing to check against"
        exit 0
    fi
fi

# The rules are this tree's, whatever the baseline's buf.yaml says or whether
# it has one.
buf breaking --against ".git#ref=$baseline" --against-config buf.yaml
echo "amux.v1 still reads what $baseline wrote"
