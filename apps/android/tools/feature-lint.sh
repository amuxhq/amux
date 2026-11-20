#!/bin/sh
# Refuses what the Compose features module must not contain.
#
# Screens under apps/android/features/src/main/ are pure functions of their
# state and callbacks. Four kinds of imports or fully-qualified references
# break that boundary quietly, and spinners hide local truth instead of showing
# cached state marked unconfirmed:
#
#   * android.app.Activity (or wildcard android.app.*)
#   * androidx.navigation (or wildcard androidx.navigation.*)
#   * okhttp3 (or wildcard okhttp3.*)
#   * android.widget.Toast (or wildcard android.widget.*)
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

        bad_import=$(awk '
            function strip_comments(line,    out, i, n, c, nextc, in_str) {
                out = ""
                in_str = 0
                i = 1
                n = length(line)
                while (i <= n) {
                    c = substr(line, i, 1)
                    nextc = (i < n) ? substr(line, i + 1, 1) : ""
                    if (in_block) {
                        if (c == "*" && nextc == "/") {
                            in_block = 0
                            i += 2
                        } else {
                            i += 1
                        }
                    } else if (in_str) {
                        out = out c
                        if (c == "\\" && i < n) {
                            out = out nextc
                            i += 2
                        } else {
                            if (c == "\"") {
                                in_str = 0
                            }
                            i += 1
                        }
                    } else if (c == "\"") {
                        in_str = 1
                        out = out c
                        i += 1
                    } else if (c == "/" && nextc == "*") {
                        in_block = 1
                        i += 2
                    } else if (c == "/" && nextc == "/") {
                        break
                    } else {
                        out = out c
                        i += 1
                    }
                }
                return out
            }
            {
                code = strip_comments($0)
                if (code ~ /^[[:space:]]*import[[:space:]]+(android\.app\.Activity|android\.app\.\*|androidx\.navigation|okhttp3|android\.widget\.Toast|android\.widget\.\*)(\.|[[:space:]]|$)/) {
                    printf "%d:%s\n", NR, $0
                }
            }
        ' "$file")
        if [ -n "$bad_import" ]; then
            echo "$file: forbidden import in features module; keep screens pure state + callbacks"
            echo "$bad_import" | sed 's/^/    /'
            complaints=$((complaints + 1))
        fi

        bad_fqn=$(awk '
            function strip_comments(line,    out, i, n, c, nextc, in_str) {
                out = ""
                in_str = 0
                i = 1
                n = length(line)
                while (i <= n) {
                    c = substr(line, i, 1)
                    nextc = (i < n) ? substr(line, i + 1, 1) : ""
                    if (in_block) {
                        if (c == "*" && nextc == "/") {
                            in_block = 0
                            i += 2
                        } else {
                            i += 1
                        }
                    } else if (in_str) {
                        out = out c
                        if (c == "\\" && i < n) {
                            out = out nextc
                            i += 2
                        } else {
                            if (c == "\"") {
                                in_str = 0
                            }
                            i += 1
                        }
                    } else if (c == "\"") {
                        in_str = 1
                        out = out c
                        i += 1
                    } else if (c == "/" && nextc == "*") {
                        in_block = 1
                        i += 2
                    } else if (c == "/" && nextc == "/") {
                        break
                    } else {
                        out = out c
                        i += 1
                    }
                }
                return out
            }
            {
                code = strip_comments($0)
                if (code ~ /^[[:space:]]*import[[:space:]]/) {
                    next
                }
                if (code ~ /(^|[^A-Za-z0-9_.])(android\.app\.Activity|android\.widget\.Toast|androidx\.navigation\.[A-Za-z0-9_]+|okhttp3\.[A-Za-z0-9_]+)([^A-Za-z0-9_]|$)/) {
                    printf "%d:%s\n", NR, $0
                }
            }
        ' "$file")
        if [ -n "$bad_fqn" ]; then
            echo "$file: forbidden fully-qualified reference in features module; keep screens pure state + callbacks"
            echo "$bad_fqn" | sed 's/^/    /'
            complaints=$((complaints + 1))
        fi

        spinner=$(awk '
            function strip_comments(line,    out, i, n, c, nextc, in_str) {
                out = ""
                in_str = 0
                i = 1
                n = length(line)
                while (i <= n) {
                    c = substr(line, i, 1)
                    nextc = (i < n) ? substr(line, i + 1, 1) : ""
                    if (in_block) {
                        if (c == "*" && nextc == "/") {
                            in_block = 0
                            i += 2
                        } else {
                            i += 1
                        }
                    } else if (in_str) {
                        out = out c
                        if (c == "\\" && i < n) {
                            out = out nextc
                            i += 2
                        } else {
                            if (c == "\"") {
                                in_str = 0
                            }
                            i += 1
                        }
                    } else if (c == "\"") {
                        in_str = 1
                        out = out c
                        i += 1
                    } else if (c == "/" && nextc == "*") {
                        in_block = 1
                        i += 2
                    } else if (c == "/" && nextc == "/") {
                        break
                    } else {
                        out = out c
                        i += 1
                    }
                }
                return out
            }
            {
                code = strip_comments($0)
                if (code ~ /(^|[^A-Za-z0-9_])(CircularProgressIndicator|LinearProgressIndicator)([^A-Za-z0-9_]|$)/) {
                    printf "%d:%s\n", NR, $0
                }
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
    printf 'import android.app.*\nclass BadAppWildcard\n' > "$fixtures/BadAppWildcard.kt"
    printf 'import androidx.navigation.NavController\nclass BadNav\n' > "$fixtures/BadNav.kt"
    printf 'import androidx.navigation.*\nclass BadNavWildcard\n' > "$fixtures/BadNavWildcard.kt"
    printf 'import okhttp3.OkHttpClient\nclass BadHttp\n' > "$fixtures/BadHttp.kt"
    printf 'import okhttp3.*\nclass BadHttpWildcard\n' > "$fixtures/BadHttpWildcard.kt"
    printf 'import android.widget.Toast\nclass BadToast\n' > "$fixtures/BadToast.kt"
    printf 'import android.widget.*\nclass BadWidgetWildcard\n' > "$fixtures/BadWidgetWildcard.kt"
    printf 'fun BadToastFqn() { android.widget.Toast.makeText(null, "", 0) }\n' > "$fixtures/BadToastFqn.kt"
    printf 'fun BadActivityFqn(a: android.app.Activity) {}\n' > "$fixtures/BadActivityFqn.kt"
    printf 'fun BadSpin() { CircularProgressIndicator() }\n' > "$fixtures/BadCircular.kt"
    printf 'fun BadSpinFqn() { androidx.compose.material3.CircularProgressIndicator() }\n' > "$fixtures/BadCircularFqn.kt"
    printf 'fun BadBar() { LinearProgressIndicator() }\n' > "$fixtures/BadLinear.kt"

    found=$(scan "$fixtures" || true)
    for expected in \
            BadActivity.kt BadAppWildcard.kt \
            BadNav.kt BadNavWildcard.kt \
            BadHttp.kt BadHttpWildcard.kt \
            BadToast.kt BadWidgetWildcard.kt \
            BadToastFqn.kt BadActivityFqn.kt \
            BadCircular.kt BadCircularFqn.kt BadLinear.kt; do
        case $found in
            *"$expected"*) ;;
            *)
                echo "feature-lint cannot catch $expected any more; the rule it breaks is not working" >&2
                exit 1
                ;;
        esac
    done

    rm -f "$fixtures"/*.kt
    printf 'package sh.amux.features\n/* Never use android.widget.Toast or CircularProgressIndicator() */\nimport androidx.compose.material3.Text\nfun Fine() { Text("ok") }\n' \
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
