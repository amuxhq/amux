#!/bin/sh

set -eu

"$(dirname "$0")/bounded" 900 cargo test --locked -p ui-state --test spec "$@"
# The desktop SQLite linkage the workspace run names, so this build is the
# one `just test` left behind and not a second store.
exec "$(dirname "$0")/bounded" 900 cargo test --locked -p testnet --features store/bundled --test 'spec_*' "$@"
