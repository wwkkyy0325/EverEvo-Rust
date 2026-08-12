#!/bin/bash
# Build everevo-server for Linux inside a Docker container.
# This avoids the need for a Linux cross-compiler on the Windows host.
#
# Usage: bash scripts/build_linux_binary.sh
# Output: target/x86_64-unknown-linux-gnu/release/everevo-server

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WS_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "=== Building everevo-server for Linux (x86_64) ==="
echo "Workspace: $WS_ROOT"

# Check Docker
if ! docker ps >/dev/null 2>&1; then
    echo "ERROR: Docker is not running. Please start Docker Desktop first."
    exit 1
fi

# Create output directory
mkdir -p "$WS_ROOT/target/x86_64-unknown-linux-gnu/release"

# Run cargo build inside a Rust Linux container
# We mount the workspace and use the same Rust version as the project
echo "=== Running cargo build inside rust:1.80-slim container ==="
# MSYS_NO_PATHCONV: Git Bash otherwise rewrites container paths like /build
# into Windows paths (C:/Program Files/Git/build) and docker rejects them.
MSYS_NO_PATHCONV=1 docker run --rm \
    -v "$WS_ROOT:/build" \
    -w /build \
    -e CARGO_HOME=/build/target/.cargo \
    rust:1.80-slim \
    bash -c "
        # Install build deps
        apt-get update && apt-get install -y pkg-config libssl-dev 2>&1 | tail -3

        # Add Linux target
        rustup target add x86_64-unknown-linux-gnu

        # Build server crate only (skip unused crates for speed)
        cargo build -p everevo-server --target x86_64-unknown-linux-gnu --release 2>&1
    "

# Check output
BINARY="$WS_ROOT/target/x86_64-unknown-linux-gnu/release/everevo-server"
if [ -f "$BINARY" ]; then
    echo "=== Build successful ==="
    ls -lh "$BINARY"
    file "$BINARY" 2>/dev/null || echo "(file command not available on Windows)"
else
    echo "ERROR: Build failed, binary not found at $BINARY"
    exit 1
fi

# Copy into the gaia-docker build context (Dockerfile: COPY everevo-server ...)
# so `docker build -t everevo-gaia scripts/gaia-docker/` works out of the box.
GAIADIR="$WS_ROOT/scripts/gaia-docker"
cp "$BINARY" "$GAIADIR/everevo-server"
echo "=== Copied to $GAIADIR/everevo-server ==="
