#!/usr/bin/env bash
# Make sure `hyperfine` is available for `make bench-test-suite`.
#
# Timing a test suite with one `date` either side of it measures that run and
# nothing else — on a shared box the spread between samples is routinely larger
# than the difference you are trying to detect. hyperfine runs each arm N times,
# warms up first, and reports mean ± σ with min/max, which is the difference
# between "it felt faster" and a number worth putting in a pull request.
#
# Pre-built binary first (seconds); package manager and cargo only as fallbacks.
set -euo pipefail

if command -v hyperfine >/dev/null 2>&1; then
  exit 0
fi

cargo_bin="${CARGO_HOME:-${HOME}/.cargo}/bin"
mkdir -p "${cargo_bin}"
if [ -x "${cargo_bin}/hyperfine" ]; then
  exit 0
fi

version="1.19.0"
os="$(uname -s)"
arch="$(uname -m)"
case "${arch}" in
  arm64 | aarch64) arch="aarch64" ;;
  x86_64 | amd64) arch="x86_64" ;;
esac
case "${os}" in
  Linux) triple="${arch}-unknown-linux-musl" ;;
  Darwin) triple="${arch}-apple-darwin" ;;
  *) triple="" ;;
esac

if [ -n "${triple}" ] && command -v curl >/dev/null 2>&1; then
  echo "▶ Installing hyperfine ${version} (${triple}) into ${cargo_bin}..."
  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' EXIT
  url="https://github.com/sharkdp/hyperfine/releases/download/v${version}/hyperfine-v${version}-${triple}.tar.gz"
  if curl -fsSL --retry 3 "${url}" -o "${tmp}/hf.tar.gz" \
     && tar zxf "${tmp}/hf.tar.gz" -C "${tmp}" \
     && cp "${tmp}/hyperfine-v${version}-${triple}/hyperfine" "${cargo_bin}/hyperfine"; then
    chmod +x "${cargo_bin}/hyperfine"
    echo "✓ $("${cargo_bin}/hyperfine" --version)"
    exit 0
  fi
  echo "warning: pre-built download failed; falling back" >&2
fi

if command -v brew >/dev/null 2>&1; then
  brew install hyperfine
  exit 0
fi

echo "▶ Installing hyperfine from source (slow — no pre-built path available)..."
cargo install hyperfine --locked
