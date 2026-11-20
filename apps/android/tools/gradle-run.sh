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
    for candidate in \
            "${ANDROID_SDK_ROOT:-}" \
            "$HOME/Library/Android/sdk" \
            "$HOME/Android/Sdk"; do
        if [ -n "$candidate" ] && [ -d "$candidate" ]; then
            export ANDROID_HOME="$candidate"
            break
        fi
    done
fi

cd "$repo_root/apps/android"
exec ./gradlew "$@"
