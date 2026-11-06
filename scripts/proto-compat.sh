#!/bin/sh
# Lints the protobuf schemas, then holds amux.v1 to the rules in docs/WIRE.md
# against the newest release tag behind HEAD, or against a ref named on the
# command line. Releases before 0.9.0 predate the freeze and are not checked
# against. Comparing with a release needs its tag, which a shallow clone lacks.
set -eu

buf lint

baseline=${1:-}
if [ -z "$baseline" ]; then
    baseline=$(git describe --tags --abbrev=0 --match 'v[0-9]*')
    version=${baseline#v}
    oldest=$(printf '%s\n0.9.0\n' "$version" | sort -V | head -n 1)
    if [ "$oldest" != 0.9.0 ]; then
        echo "the newest release, $baseline, predates the freeze at 0.9.0; nothing to check against"
        exit 0
    fi
fi

# The rules are this tree's, whatever the baseline's buf.yaml says or whether
# it has one.
buf breaking --against ".git#ref=$baseline" --against-config buf.yaml
echo "amux.v1 still reads what $baseline wrote"
