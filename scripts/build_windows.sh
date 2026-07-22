#!/bin/bash
set -euxo pipefail

CONTAINERFILE="scripts/Containerfile.build-windows"
IMAGE_TAG="xi-agent-windows-build:latest"
OUTPUT_DIR="target/x86_64-pc-windows-gnu/release"
OUTPUT_BINARY="$OUTPUT_DIR/xi.exe"

echo "=== Building Windows cross-compilation container image ==="
podman build -t "$IMAGE_TAG" -f "$CONTAINERFILE" .

echo "=== Copying build artifacts from container ==="
mkdir -p "$OUTPUT_DIR"

CONTAINER=$(podman create "$IMAGE_TAG")
podman cp "$CONTAINER:/build/$OUTPUT_BINARY" "$OUTPUT_BINARY"
podman rm "$CONTAINER"

echo "=== Build complete ==="
echo "Binary: $OUTPUT_BINARY"
ls -lh "$OUTPUT_BINARY"
