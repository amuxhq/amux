#!/bin/sh

# Registers the development extension's native messaging host,
# `sh.amux.browser.dev`, with every installed Chromium browser: a manifest in
# each browser's NativeMessagingHosts folder, running a script that starts
# this tree's built amux as the bridge, with the config AMUX_CONFIG names
# now. The installed extension's host, `sh.amux.browser`, is the supervisor's
# and is never touched. `remove` takes the development host out again.

set -eu

here=$(cd "$(dirname "$0")/.." && pwd)
name=sh.amux.browser.dev
script="$here/target/browser-dev-host"
id=omnhphmiepghpgpbfgjebeaajfppkhie

case "$(uname -s)" in
    Darwin)
        base="$HOME/Library/Application Support"
        browsers="Google/Chrome|Google/Chrome Beta|Google/Chrome Canary|Chromium|Microsoft Edge|BraveSoftware/Brave-Browser|Arc/User Data|Vivaldi"
        ;;
    *)
        base="${XDG_CONFIG_HOME:-$HOME/.config}"
        browsers="google-chrome|google-chrome-beta|chromium|microsoft-edge|BraveSoftware/Brave-Browser|vivaldi"
        ;;
esac

if [ "${1-}" = remove ]; then
    IFS='|'
    for browser in $browsers; do
        rm -f "$base/$browser/NativeMessagingHosts/$name.json"
    done
    rm -f "$script"
    echo "Removed $name."
    exit 0
fi

mkdir -p "$here/target"
{
    echo '#!/bin/sh'
    if [ -n "${AMUX_CONFIG-}" ]; then
        printf 'export AMUX_CONFIG=%s\n' "'$(printf %s "$AMUX_CONFIG" | sed "s/'/'\\\\''/g")'"
    fi
    printf 'exec %s internal browser "$@"\n' "'$here/.wt/bin/amux'"
} > "$script"
chmod 755 "$script"

manifest=$(printf '{\n  "name": "%s",\n  "description": "amux, from %s",\n  "path": "%s",\n  "type": "stdio",\n  "allowed_origins": ["chrome-extension://%s/"]\n}\n' \
    "$name" "$here" "$script" "$id")
IFS='|'
for browser in $browsers; do
    [ -d "$base/$browser" ] || continue
    mkdir -p "$base/$browser/NativeMessagingHosts"
    printf '%s' "$manifest" > "$base/$browser/NativeMessagingHosts/$name.json"
    echo "Registered $name with $browser."
done
echo "It runs $here/.wt/bin/amux${AMUX_CONFIG:+ with $AMUX_CONFIG}; build it with \`just build\`."
