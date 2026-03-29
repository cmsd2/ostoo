#!/bin/bash
# Build Rust userspace programs and deploy binaries to user/ for virtio-9p.
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
USER_RS_DIR="$PROJECT_DIR/user-rs"
TARGET_DIR="$USER_RS_DIR/target/x86_64-ostoo-user/release"
DEPLOY_DIR="$PROJECT_DIR/user/bin"

# Ensure musl sysroot is available
"$SCRIPT_DIR/extract-musl-sysroot.sh"

cd "$USER_RS_DIR"

# Build packages separately to avoid Cargo feature unification:
# hello-rs uses ostoo-rt with no_std (default), hello-std uses it without.
cargo build --release -p hello-rs "$@"
cargo build --release -p hello-std "$@"

# Deploy binaries (skip the runtime library).
mkdir -p "$DEPLOY_DIR"
for bin in "$TARGET_DIR"/hello-rs "$TARGET_DIR"/hello-std; do
    if [ -f "$bin" ] && file "$bin" | grep -q "ELF"; then
        name=$(basename "$bin")
        cp "$bin" "$DEPLOY_DIR/$name"
        echo "deployed: $name ($(wc -c < "$bin" | tr -d ' ') bytes)"
    fi
done
