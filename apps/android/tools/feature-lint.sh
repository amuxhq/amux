#!/bin/sh
# Refuses what the Compose features module must not contain.
#
# Screens under apps/android/features/src/main/ are pure functions of their
# state and callbacks. Four kinds of imports break that boundary quietly, and
# spinners hide local truth instead of showing cached state marked unconfirmed:
#
#   * android.app.Activity
#   * androidx.navigation
#   * okhttp3
#   * android.widget.Toast
#   * CircularProgressIndicator / LinearProgressIndicator
#
# It checks itself first against fixtures written to break every rule, so a
# check that stops catching violations fails loudly instead of passing.

set -eu

package=${AMUX_ANDROID_FEATURES_SOURCES:-apps/android/features/src/main}

scan() {
    root=$1
    complaints=0

    while IFS= read -r file; do
        [ -n "$file" ] || continue

        bad_import=$(grep -nE '^[[:space:]]*import[[:space:]]+(android\.app\.Activity|androidx\.navigation|okhttp3|android\.widget\.Toast)(\.|$|[[:space:]])' "$file" || true)
        if [ -n "$bad_import" ]; then
            echo "$file: forbidden import in features module; keep screens pure state + callbacks"
            echo "$bad_import" | sed 's/^/    /'
            complaints=$((complaints + 1))
        fi

        spinner=$(awk '
            /^[[:space:]]*\/\// { next }
            /(^|[^A-Za-z0-9_])(CircularProgressIndicator|LinearProgressIndicator)([^A-Za-z0-9_]|$)/ {
                printf "%d:%s\n", NR, $0
            }
        ' "$file")
        if [ -n "$spinner" ]; then
            echo "$file: draws a spinner; show cached state marked unconfirmed instead"
            echo "$spinner" | sed 's/^/    /'
            complaints=$((complaints + 1))
        fi
    done <<EOF
$(find "$root" -type f -name '*.kt' | sort)
EOF

    [ "$complaints" -eq 0 ]
}

self_test() {
    fixtures=$(mktemp -d)
    trap 'rm -rf "$fixtures"' EXIT

    printf 'import android.app.Activity\nclass BadActivity\n' > "$fixtures/BadActivity.kt"
    printf 'import androidx.navigation.NavController\nclass BadNav\n' > "$fixtures/BadNav.kt"
    printf 'import okhttp3.OkHttpClient\nclass BadHttp\n' > "$fixtures/BadHttp.kt"
    printf 'import android.widget.Toast\nclass BadToast\n' > "$fixtures/BadToast.kt"
    printf 'fun BadSpin() { CircularProgressIndicator() }\n' > "$fixtures/BadCircular.kt"
    printf 'fun BadBar() { LinearProgressIndicator() }\n' > "$fixtures/BadLinear.kt"

    found=$(scan "$fixtures" || true)
    for expected in BadActivity.kt BadNav.kt BadHttp.kt BadToast.kt BadCircular.kt BadLinear.kt; do
        case $found in
            *"$expected"*) ;;
            *)
                echo "feature-lint cannot catch $expected any more; the rule it breaks is not working" >&2
                exit 1
                ;;
        esac
    done

    rm -f "$fixtures/BadActivity.kt" "$fixtures/BadNav.kt" "$fixtures/BadHttp.kt" \
        "$fixtures/BadToast.kt" "$fixtures/BadCircular.kt" "$fixtures/BadLinear.kt"
    printf 'package sh.amux.features\nimport androidx.compose.material3.Text\nfun Fine() { Text("ok") }\n' \
        > "$fixtures/Fine.kt"
    if ! scan "$fixtures" > /dev/null; then
        echo "feature-lint complains about sources that break none of its rules" >&2
        exit 1
    fi

    rm -rf "$fixtures"
    trap - EXIT
}

self_test

if [ ! -d "$package" ]; then
    echo "no sources at $package; run this from the checkout root" >&2
    exit 1
fi

if scan "$package"; then
    echo "$package: pure Compose screens, no forbidden imports, no spinners"
else
    exit 1
fi
