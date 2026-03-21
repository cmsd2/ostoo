#!/bin/bash
# Extract musl sysroot from the ostoo-compiler Docker image.
# Produces: sysroot/x86_64-ostoo-user/ with lib/ and include/.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SYSROOT_DIR="$PROJECT_DIR/sysroot/x86_64-ostoo-user"
DOCKER_IMAGE="ostoo-compiler"
# Crosstool-ng sysroot inside the Docker image
CT_SYSROOT="/home/ctng/x-tools/x86_64-unknown-linux-musl/x86_64-unknown-linux-musl/sysroot"

# Skip if sysroot already extracted and libc.a exists
if [ -f "$SYSROOT_DIR/lib/libc.a" ]; then
    echo "Sysroot already exists at $SYSROOT_DIR"
    exit 0
fi

# Ensure Docker image exists
if ! docker image inspect "$DOCKER_IMAGE" >/dev/null 2>&1; then
    echo "Building ostoo-compiler Docker image..."
    "$SCRIPT_DIR/../crosstool/build.sh"
fi

echo "Extracting musl sysroot from Docker image..."
rm -rf "$SYSROOT_DIR"
mkdir -p "$SYSROOT_DIR"

# Use a tar pipe to extract with correct ownership (avoids Docker cp permission issues)
docker run --rm "$DOCKER_IMAGE" tar cf - \
    -C "$CT_SYSROOT/usr" \
    lib/libc.a lib/crt1.o lib/crti.o lib/crtn.o lib/rcrt1.o lib/Scrt1.o \
    include \
    | tar xf - -C "$SYSROOT_DIR"

# Create an empty libunwind.a stub.
# musl's built-in unwinder is in libc.a itself, but Rust's unwind crate
# emits #[link(name = "unwind")] for musl targets with crt-static.
# An empty archive satisfies the linker; with panic=abort no unwind
# code is actually called at runtime.
LLVM_AR="$(find "$HOME/.rustup" -name llvm-ar -path '*/nightly-*/bin/*' 2>/dev/null | head -1)"
if [ -n "$LLVM_AR" ]; then
    "$LLVM_AR" cr "$SYSROOT_DIR/lib/libunwind.a"
elif command -v llvm-ar >/dev/null 2>&1; then
    llvm-ar cr "$SYSROOT_DIR/lib/libunwind.a"
else
    echo "warning: could not find llvm-ar; libunwind.a stub not created"
fi

echo ""
echo "Sysroot extracted to $SYSROOT_DIR"
echo "  libc.a: $(wc -c < "$SYSROOT_DIR/lib/libc.a" | tr -d ' ') bytes"
ls "$SYSROOT_DIR/lib/"
