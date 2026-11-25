#!/bin/sh
# Merges a fresh origin/main into this branch (`just merge-main`).
#
# When only the committed protobuf bindings conflict, they are regenerated
# from the merged sources (`just protobuf`) and the merge is committed: they
# are generated from proto/ alone, so that is the one right resolution. Any
# other conflict is left as `git merge` leaves it, for you to resolve.
set -eu
generated=crates/wire/src/generated/
git fetch --quiet origin main
if git merge --no-edit origin/main; then
    exit 0
fi
conflicts=$(git diff --name-only --diff-filter=U)
others=$(printf '%s\n' "$conflicts" | grep -v "^$generated" || true)
if [ -n "$others" ]; then
    echo "merge-main: these conflict; resolve them, then commit:" >&2
    printf '  %s\n' $others >&2
    exit 1
fi
echo "merge-main: only the generated protobuf bindings conflict; regenerating them from the merged sources"
just protobuf
git add -A "$generated"
git commit --quiet --no-edit
echo "merge-main: merged main, with the bindings regenerated"
