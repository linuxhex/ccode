#!/bin/bash
# Build the Rust Python extension using maturin

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PYTHON_DIR="$(dirname "$SCRIPT_DIR")"
REPO_ROOT="$(dirname "$PYTHON_DIR")"
CRATE_DIR="$REPO_ROOT/crates/codegen/ccore-python"

# Find local ripgrep to avoid network download during build
if [ -z "$CCODE_TOOLS_BUNDLE_RG_PATH" ]; then
    if command -v rg &> /dev/null; then
        RG_PATH="$(command -v rg)"
        export CCODE_TOOLS_BUNDLE_RG_PATH="$RG_PATH"
        echo "Using local ripgrep: $RG_PATH"
    fi
fi

echo "Building ccore_engine Python extension..."
echo "Crate directory: $CRATE_DIR"

cd "$CRATE_DIR"

# Build with maturin
maturin develop --release

echo "Build complete!"
echo "You can now import ccore_engine in Python"
