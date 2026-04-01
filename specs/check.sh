#!/usr/bin/env bash
#
# Run TLC model checker on ostoo's PlusCal/TLA+ specs.
#
# Uses the Java and tla2tools.jar bundled with the TLA+ Toolbox.
#
# Usage:
#   ./check.sh                                    # run base specs only
#   ./check.sh all                                # run base + issues specs
#   ./check.sh spsc_ring                          # run one base spec
#   ./check.sh issues/lost-wakeup/completion_port # run an issue spec
#

set -euo pipefail
cd "$(dirname "$0")"

# ---- Locate TLA+ Toolbox bundled JRE and jar ----

TOOLBOX="/Applications/TLA+ Toolbox.app"
TLA2TOOLS="$TOOLBOX/Contents/Eclipse/tla2tools.jar"

# Try well-known path first, then glob for other versions
JAVA="$TOOLBOX/Contents/Eclipse/plugins/org.lamport.openjdk.macosx.x86_64_14.0.1.7/Contents/Home/bin/java"

if [[ ! -x "$JAVA" ]]; then
    # Fallback: search for any bundled java
    JAVA=$(find "$TOOLBOX/Contents" -name "java" -path "*/bin/java" -type f 2>/dev/null | head -1 || true)
fi

if [[ ! -x "$JAVA" ]]; then
    # Last resort: system java
    JAVA=$(command -v java 2>/dev/null || true)
fi

if [[ -z "$JAVA" || ! -f "$TLA2TOOLS" ]]; then
    echo "ERROR: Cannot find Java or tla2tools.jar"
    echo "  Java: ${JAVA:-not found}"
    echo "  Jar:  $TLA2TOOLS (exists: $(test -f "$TLA2TOOLS" && echo yes || echo no))"
    echo ""
    echo "Install TLA+ Toolbox from https://lamport.azurewebsites.net/tla/toolbox.html"
    exit 1
fi

echo "Using Java: $JAVA"
echo "Using jar:  $TLA2TOOLS"
echo ""

# ---- Results tracking ----

PASSED=()
FAILED=()

# ---- TLC runner ----

run_spec() {
    local arg="$1"
    local dir
    local base
    # Support both "spsc_ring" and "issues/lost-wakeup/completion_port"
    if [[ "$arg" == */* ]]; then
        dir="$arg"
        base="$(basename "$arg")"
    else
        dir="$arg"
        base="$arg"
    fi
    local tla="${dir}/${base}.tla"
    local cfg="${dir}/${base}.cfg"

    if [[ ! -f "$tla" ]]; then
        echo "ERROR: $tla not found"
        FAILED+=("$arg")
        return 1
    fi
    if [[ ! -f "$cfg" ]]; then
        echo "ERROR: $cfg not found"
        FAILED+=("$arg")
        return 1
    fi

    echo "========================================"
    echo "  Checking: $tla"
    echo "========================================"

    # Step 1: Translate PlusCal to TLA+
    echo "→ Translating PlusCal..."
    if ! "$JAVA" -cp "$TLA2TOOLS" pcal.trans "$tla" 2>&1; then
        FAILED+=("$arg")
        return 1
    fi

    echo ""

    # Step 2: Run TLC model checker
    echo "→ Running TLC..."
    if "$JAVA" -cp "$TLA2TOOLS" tlc2.TLC \
        -config "$cfg" \
        -workers auto \
        -cleanup \
        "$tla" 2>&1; then
        PASSED+=("$arg")
    else
        FAILED+=("$arg")
    fi

    echo ""
}

print_summary() {
    local total=$(( ${#PASSED[@]} + ${#FAILED[@]} ))
    [[ $total -eq 0 ]] && return

    echo "========================================"
    echo "  Summary: ${#PASSED[@]} passed, ${#FAILED[@]} failed (${total} total)"
    echo "========================================"
    for spec in "${PASSED[@]}"; do
        echo "  PASS  $spec"
    done
    for spec in "${FAILED[@]}"; do
        echo "  FAIL  $spec"
    done
    echo ""
}

# ---- Discover specs ----

# Base specs: immediate child directories of specs/ that contain a .tla file
# (excludes issues/ and other non-spec directories).
discover_base_specs() {
    for dir in */; do
        dir="${dir%/}"
        [[ "$dir" == "issues" ]] && continue
        [[ -f "${dir}/${dir}.tla" ]] && echo "$dir"
    done
}

# Issue specs: specs under issues/<issue>/<name>/
discover_issue_specs() {
    [[ -d "issues" ]] || return 0
    for issue_dir in issues/*/; do
        [[ -d "$issue_dir" ]] || continue
        for spec_dir in "${issue_dir}"*/; do
            [[ -d "$spec_dir" ]] || continue
            spec_dir="${spec_dir%/}"
            local base
            base="$(basename "$spec_dir")"
            [[ -f "${spec_dir}/${base}.tla" ]] && echo "$spec_dir"
        done
    done
}

# ---- Main ----

if [[ $# -eq 0 ]]; then
    # No args: run base specs only
    for spec in $(discover_base_specs); do
        echo ""
        run_spec "$spec" || true
    done
elif [[ $# -eq 1 && "$1" == "all" ]]; then
    # "all": run base specs + issue specs
    for spec in $(discover_base_specs); do
        echo ""
        run_spec "$spec" || true
    done
    for spec in $(discover_issue_specs); do
        echo ""
        run_spec "$spec" || true   # issue specs may intentionally fail (red phase)
    done
else
    # Explicit spec names
    for arg in "$@"; do
        run_spec "$arg" || true
    done
fi

print_summary
[[ ${#FAILED[@]} -eq 0 ]]
