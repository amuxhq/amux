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
# tests/feature_lint_test.py holds it to every rule with sources written to
# break each one.

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

if [ ! -d "$package" ]; then
    echo "no sources at $package; run this from the checkout root" >&2
    exit 1
fi

if scan "$package"; then
    echo "$package: pure Compose screens, no forbidden imports, no spinners"
else
    exit 1
fi
