#!/usr/bin/env bash
# Make sure `cargo nextest` is available, cheaply.
#
# Why this exists: `make test` runs nextest so that the local gate and CI run the
# *same* runner at the *same* concurrency. That only holds if nextest is actually
# present everywhere — a laptop, a CI runner, and a fresh cloud container. The
# obvious `cargo install cargo-nextest` compiles it from source and takes minutes
# on a small box, which is exactly the cost this whole change exists to remove.
#
# So: prefer the official pre-built binary (a few seconds), fall back to
# cargo-binstall, and only then to a source build. Idempotent — if nextest is
# already on PATH this exits immediately.
set -euo pipefail

if command -v cargo-nextest >/dev/null 2>&1; then
  exit 0
fi

cargo_bin="${CARGO_HOME:-${HOME}/.cargo}/bin"
mkdir -p "${cargo_bin}"

# `cargo-nextest` may be installed but not on PATH (common when CARGO_HOME is
# relocated by CI). Check the destination before downloading anything.
if [ -x "${cargo_bin}/cargo-nextest" ]; then
  exit 0
fi

case "$(uname -s)" in
  Linux) platform="linux" ;;
  Darwin) platform="mac" ;;
  *) platform="" ;;
esac

# get.nexte.st serves a single universal tarball per platform (the mac one is a
# universal binary; the linux one is x86_64/aarch64 aware via the same URL).
if [ -n "${platform}" ] && command -v curl >/dev/null 2>&1; then
  echo "▶ Installing cargo-nextest (pre-built) into ${cargo_bin}..."
  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' EXIT
  if curl -fsSL --retry 3 "https://get.nexte.st/latest/${platform}" -o "${tmp}/nextest.tar.gz" \
     && tar zxf "${tmp}/nextest.tar.gz" -C "${cargo_bin}"; then
    echo "✓ $("${cargo_bin}/cargo-nextest" nextest --version)"
    exit 0
  fi
  echo "warning: pre-built download failed; falling back to a source install" >&2
fi

if command -v cargo-binstall >/dev/null 2>&1; then
  echo "▶ Installing cargo-nextest via cargo-binstall..."
  cargo binstall --no-confirm cargo-nextest
  exit 0
fi

echo "▶ Installing cargo-nextest from source (slow — no pre-built path available)..."
cargo install cargo-nextest --locked
