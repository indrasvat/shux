#!/usr/bin/env bash
set -euo pipefail

echo "=== shux dev environment setup ==="

# Check Rust
if ! command -v cargo &>/dev/null; then
    echo "Rust not found. Install from https://rustup.rs/"
    exit 1
fi

echo "Rust $(rustc --version | cut -d' ' -f2)"

# Install dev tools
echo "Installing dev tools..."
cargo install cargo-nextest --locked 2>/dev/null || echo "  cargo-nextest already installed"
cargo install cargo-llvm-cov --locked 2>/dev/null || echo "  cargo-llvm-cov already installed"
cargo install cargo-deny --locked 2>/dev/null || echo "  cargo-deny already installed"

# Install lefthook
if ! command -v lefthook &>/dev/null; then
    echo "Installing lefthook..."
    cargo install lefthook --locked 2>/dev/null || npm i -g lefthook
fi
echo "lefthook $(lefthook version 2>/dev/null || echo 'installed')"

# Install git hooks
echo "Installing git hooks..."
lefthook install

# Build to verify
echo "Building..."
cargo build --workspace

echo ""
echo "=== Setup complete! ==="
echo "Run 'make check' to verify everything works."
