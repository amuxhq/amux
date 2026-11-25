#!/bin/bash
# Runs a CI macOS job's Buck2 command, given as one shell string, and runs it
# once more if it failed only because the runner's NativeLink gave up on a
# blob: NativeLink allows each blob of a batch read or write 30 s, fixed in
# its code, and Buck2 fails the action or test that needed it rather than
# ask again. A runner stalled that long is not the change's fault: a
# booting simulator on its three cores and 7 GB stalled every process for
# 20 s at a time, and the burst of uploads as eighty tests start at once
# held a 62 KB write past the limit. The second run takes everything the
# first finished from the cache. Any other failure is the command's own and
# stands.
set -uo pipefail

log=$(mktemp)
bash -c "$1" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
if [ "$status" -ne 0 ] && grep -qE "Batch(Read|Update)Blobs per-blob timeout" "$log"; then
    echo "::warning::NativeLink timed out on a blob in its own store; running the command again"
    bash -c "$1"
    status=$?
fi
rm -f "$log"
exit "$status"
