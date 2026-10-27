#!/bin/sh

# Runs a command with no network but loopback, and with its own empty
# configuration homes. The command runs in a network namespace of its own,
# which holds nothing but a loopback interface, rather than under a rule
# filter: a filter such as macOS's sandbox also refuses setuid programs and
# misreads loopback addresses, so tests failed there for reasons that had
# nothing to do with the network. Creating the namespace takes root, so the
# command runs through sudo and then drops back to the calling user.

set -eu

if [ "$#" -eq 0 ]; then
    echo "usage: $0 <command> [args...]" >&2
    exit 64
fi

if [ "$(uname -s)" != Linux ]; then
    echo "offline-check: needs Linux, for a network namespace" >&2
    exit 69
fi

original_home=${HOME:?HOME must be set}
offline_cargo_home=${CARGO_HOME:-"$original_home/.cargo"}
offline_rustup_home=${RUSTUP_HOME:-"$original_home/.rustup"}
offline_path=${PATH:?PATH must be set}

offline_root=$(mktemp -d "${TMPDIR:-/tmp}/amux-offline.XXXXXX")
case "$offline_root" in
    "${TMPDIR:-/tmp}"/amux-offline.*) ;;
    *)
        echo "offline-check: mktemp returned an unexpected path: $offline_root" >&2
        exit 70
        ;;
esac

cleanup() {
    rm -rf -- "$offline_root"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

offline_home="$offline_root/home"
offline_claude_config="$offline_root/claude"
offline_codex_home="$offline_root/codex"
mkdir -p "$offline_home" "$offline_claude_config" "$offline_codex_home"

printf '%s\n' \
    "HOME=$offline_home" \
    "CLAUDE_CONFIG_DIR=$offline_claude_config" \
    "CODEX_HOME=$offline_codex_home" \
    "CARGO_HOME=$offline_cargo_home" \
    "RUSTUP_HOME=$offline_rustup_home" \
    "PATH=$offline_path" \
    "CARGO_NET_OFFLINE=true" \
    "AMUX_OFFLINE=1"

run_offline() {
    # sudo keeps the caller's environment but resets PATH, which env sets
    # again once the command runs as the caller.
    sudo --preserve-env unshare --net -- /bin/sh -c '
        ip link set lo up
        user=$1 group=$2
        shift 2
        exec setpriv --reuid="$user" --regid="$group" --init-groups -- "$@"
    ' sh "$(id -u)" "$(id -g)" \
        /usr/bin/env \
        HOME="$offline_home" \
        USER="$(id -un)" \
        LOGNAME="$(id -un)" \
        CLAUDE_CONFIG_DIR="$offline_claude_config" \
        CODEX_HOME="$offline_codex_home" \
        CARGO_HOME="$offline_cargo_home" \
        RUSTUP_HOME="$offline_rustup_home" \
        PATH="$offline_path" \
        CARGO_NET_OFFLINE=true \
        AMUX_OFFLINE=1 \
        "$@"
}

if run_offline nc -z -w 1 1.1.1.1 443 >/dev/null 2>&1; then
    echo "offline-check: refusing to run because outbound TCP is reachable" >&2
    exit 70
fi

run_offline "$@"
