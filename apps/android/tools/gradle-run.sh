#!/bin/sh
# Locate JDK 21 and the Android SDK when non-interactive shells do not export
# them, then invoke the committed Gradle Wrapper under apps/android/.

set -eu

repo_root=$(cd "$(dirname "$0")/../../.." && pwd)

if [ -z "${JAVA_HOME:-}" ]; then
    for candidate in \
            /opt/homebrew/opt/openjdk@21/libexec/openjdk.jdk/Contents/Home \
            /usr/local/opt/openjdk@21/libexec/openjdk.jdk/Contents/Home \
            /usr/lib/jvm/java-21-openjdk-amd64 \
            /usr/lib/jvm/java-21-openjdk-arm64; do
        if [ -x "$candidate/bin/java" ]; then
            export JAVA_HOME="$candidate"
            break
        fi
    done
fi

if [ -z "${ANDROID_HOME:-}" ]; then
    # android_sdk.py is where every Android script finds the SDK. When it
    # finds none, ANDROID_HOME stays unset and Gradle says what is missing.
    if sdk=$("$repo_root/scripts/py" -B "$repo_root/apps/android/tools/android_sdk.py" sdk); then
        export ANDROID_HOME="$sdk"
    fi
fi

cd "$repo_root/apps/android"

# `scripts/bounded` runs Gradle under `timeout`, which moves it into a
# background process group while keeping stdin on the caller's TTY. On exit
# Gradle's console restores termios on stdin, triggering SIGTTOU and leaving
# the wrapper stopped (`T`) after `BUILD SUCCESSFUL`. Detaching stdin prevents
# that hang, and running Kotlin compilation in-process under `--no-daemon`
# prevents a detached KotlinCompileDaemon from outliving the invocation.
no_daemon=0
has_kotlin_strategy=0
has_console=0
for arg in "$@"; do
    case "$arg" in
        --no-daemon) no_daemon=1 ;;
        -Pkotlin.compiler.execution.strategy=*|-Dkotlin.compiler.execution.strategy=*)
            has_kotlin_strategy=1 ;;
        --console=*|--console)
            has_console=1 ;;
    esac
done

if [ "$has_console" -eq 0 ] && [ ! -t 1 ]; then
    set -- --console=plain "$@"
fi
if [ "$no_daemon" -eq 1 ] && [ "$has_kotlin_strategy" -eq 0 ]; then
    set -- -Pkotlin.compiler.execution.strategy=in-process "$@"
fi

exec ./gradlew "$@" </dev/null
