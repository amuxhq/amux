#!/bin/sh
# Refuses fixed wall-clock sleeps in Android and JVM Kotlin tests.
#
# Thread.sleep(...) and bare delay(...) guess how long asynchronous work takes
# and flake under load. Wait on the coroutine/future or poll a real condition
# instead. When a test genuinely needs delay(...) (for example to drive a
# virtual clock or test timeout logic), mark that line with:
#
#     delay(100) // test-wait-exempt: explains why
#
# It checks itself first against fixtures written to fail and pass.

set -eu

scan_files() {
    complaints=0
    while IFS= read -r file; do
        [ -n "$file" ] || continue

        sleeps=$(awk '
            /^[[:space:]]*\/\// { next }
            /(^|[^A-Za-z0-9_])Thread\.sleep[[:space:]]*\(/ {
                printf "%d:%s\n", NR, $0
            }
        ' "$file")
        if [ -n "$sleeps" ]; then
            echo "$file: uses Thread.sleep(); wait on a signal or condition instead"
            echo "$sleeps" | sed 's/^/    /'
            complaints=$((complaints + 1))
        fi

        delays=$(awk '
            /^[[:space:]]*\/\// { next }
            /(^|[^A-Za-z0-9_])delay[[:space:]]*\(/ {
                if ($0 !~ /\/\/[[:space:]]*test-wait-exempt:[[:space:]]*[A-Za-z0-9]/) {
                    printf "%d:%s\n", NR, $0
                }
            }
        ' "$file")
        if [ -n "$delays" ]; then
            echo "$file: uses bare delay() without // test-wait-exempt: <reason>"
            echo "$delays" | sed 's/^/    /'
            complaints=$((complaints + 1))
        fi
    done

    [ "$complaints" -eq 0 ]
}

list_test_files() {
    if [ -n "${AMUX_ANDROID_TEST_ROOTS:-}" ]; then
        # shellcheck disable=SC2086
        find $AMUX_ANDROID_TEST_ROOTS -type f -name '*.kt' | sort
    else
        find apps/android -type f -name '*.kt' \( -path '*/src/test/*' -o -path '*/src/androidTest/*' \) | sort
    fi
}

self_test() {
    probe=$(mktemp -d)
    trap 'rm -rf "$probe"' EXIT

    printf 'fun badSleep() { Thread.sleep(50) }\n' > "$probe/BadSleep.kt"
    printf 'suspend fun badDelay() { delay(50) }\n' > "$probe/BadDelay.kt"
    printf 'suspend fun emptyExempt() {\n    delay(50) // test-wait-exempt:   \n}\n' > "$probe/EmptyExempt.kt"
    printf 'suspend fun okDelay() {\n    delay(50) // test-wait-exempt: virtual clock test\n}\n' > "$probe/OkDelay.kt"
    printf 'fun okOther() { customDelay(50) }\n' > "$probe/OkOther.kt"

    found=$(find "$probe" -type f -name '*.kt' | sort | scan_files || true)
    for expected in BadSleep.kt BadDelay.kt EmptyExempt.kt; do
        case $found in
            *"$expected"*) ;;
            *)
                echo "test-waits-lint: failed to catch $expected in self-test" >&2
                exit 1
                ;;
        esac
    done
    case $found in
        *OkDelay.kt*|*OkOther.kt*)
            echo "test-waits-lint: false positive in self-test" >&2
            exit 1
            ;;
    esac

    rm -rf "$probe"
    trap - EXIT
}

self_test

files=$(list_test_files)
if printf '%s\n' "$files" | scan_files; then
    echo "apps/android tests: no Thread.sleep() or unexempted delay()"
else
    exit 1
fi
