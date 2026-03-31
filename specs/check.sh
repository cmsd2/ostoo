#!/usr/bin/env bash
#
# Run TLC model checker on ostoo's PlusCal/TLA+ specs.
#
# Uses the Java and tla2tools.jar bundled with the TLA+ Toolbox.
#
# Usage:
#   ./check.sh                      # run all specs
#   ./check.sh spsc_ring            # run one spec
#   ./check.sh completion_port_fixed
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

# ---- TLC runner ----

run_spec() {
    local name="$1"
    local tla="${name}.tla"
    local cfg="${name}.cfg"

    if [[ ! -f "$tla" ]]; then
        echo "ERROR: $tla not found"
        return 1
    fi
    if [[ ! -f "$cfg" ]]; then
        echo "ERROR: $cfg not found"
        return 1
    fi

    echo "========================================"
    echo "  Checking: $name"
    echo "========================================"

    # Step 1: Translate PlusCal to TLA+
    echo "→ Translating PlusCal..."
    "$JAVA" -cp "$TLA2TOOLS" pcal.trans "$tla" 2>&1

    echo ""

    # Step 2: Run TLC model checker
    echo "→ Running TLC..."
    "$JAVA" -cp "$TLA2TOOLS" tlc2.TLC \
        -config "$cfg" \
        -workers auto \
        -cleanup \
        "$tla" 2>&1

    echo ""
}

# ---- Main ----

SPECS=("spsc_ring" "completion_port" "completion_port_fixed")

if [[ $# -gt 0 ]]; then
    for arg in "$@"; do
        run_spec "$arg"
    done
else
    for spec in "${SPECS[@]}"; do
        echo ""
        run_spec "$spec" || true   # continue on failure (expected for buggy spec)
    done
fi
