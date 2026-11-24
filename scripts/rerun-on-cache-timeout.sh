#!/bin/bash
# Runs a CI macOS job's Buck2 command, given as one shell string, and runs it
# once more if it failed only because the runner's NativeLink gave up on a
# read: NativeLink allows a blob 30 s to come out of its store, fixed in its
# code, and Buck2 fails the action that needed the blob rather than ask
# again. A runner stalled that long (a booting simulator on its three cores
# and 7 GB stalled every process for 20 s at a time) is not the change's
# fault, and the second run takes everything the first finished from the
# cache. Any other failure is the command's own and stands.
set -uo pipefail

log=$(mktemp)
bash -c "$1" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
if [ "$status" -ne 0 ] && grep -q "BatchReadBlobs per-blob timeout" "$log"; then
    echo "::warning::NativeLink timed out reading a blob from its own store; running the command again"
    bash -c "$1"
    status=$?
fi
rm -f "$log"
exit "$status"
