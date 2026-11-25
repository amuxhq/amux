#!/bin/bash
# Writes what a CI macOS runner is doing every ten seconds until it is
# killed: load, memory and swap, paging, and the processes using the most
# memory and CPU (a simulator's marked `sim:`). The macOS jobs keep it with
# Buck2's event logs (docs/CI.md, "The macOS jobs"), so a slow or stalled
# run can be told apart: a runner short of memory or cores, or a fault in
# what ran.
set -u

top() {
    ps -Ao pcpu=,rss=,comm= "$1" | head -"$2" | awk '{
        command = $0
        sub(/^ *[0-9.]+ +[0-9]+ +/, "", command)
        n = split(command, part, "/")
        printf "%6s %8s %s%s\n", $1, $2, (command ~ /CoreSimulator/ ? "sim:" : ""), part[n]
    }'
}

while :; do
    echo "== $(date -u +%H:%M:%S) load $(sysctl -n vm.loadavg) swap $(sysctl -n vm.swapusage)" \
        "procs $(ps -A | wc -l | tr -d ' ') sim $(ps -A | grep -c CoreSimulator/Profiles)"
    vm_stat | grep -E "Pages (free|active|inactive|wired down|occupied by compressor)|Pageins|Pageouts|Swapins|Swapouts"
    echo "-- by cpu (%cpu, rss KiB)"
    top -r 8
    echo "-- by memory"
    top -m 5
    sleep 10
done
