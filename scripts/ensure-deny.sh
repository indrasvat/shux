#!/usr/bin/env bash
# Make sure `cargo deny` is available, cheaply.
#
# Why this exists: `make deny` runs on every push (lefthook pre-push) and in CI,
# but nothing installed it. `make install-tools` carries a `cargo install
# cargo-deny --locked` line that nothing invokes, so on a fresh clone or a cloud
# container the first push died with cargo's own error —
#
#     error: no such command: `deny`
#     help: a command with a similar name exists: `bench`
#
# — which names neither the target that needs it nor the way to get it. Every
# other tool this repo gates on already has this treatment: `make test` depends
# on `nextest-ready`, `make bench-test-suite` on `setup-bench`. cargo-deny was
# the one that did not, and it is the one that blocks pushes.
#
# Pre-built binary first. `cargo install --locked` builds cargo-deny from source
# against the toolchain `rust-toolchain.toml` pins — measured here at ~7 minutes,
# and a tool whose MSRV outruns the pinned channel would not build at all. The
# release binary is neither slow nor coupled to the toolchain.
#
# Idempotent: if `cargo deny` already works this exits immediately.
set -euo pipefail

# `command -v cargo-deny` is not the question — the question is whether
# `cargo deny` resolves, which is what the Makefile actually runs.
if cargo deny --version >/dev/null 2>&1; then
  exit 0
fi

cargo_bin="${CARGO_HOME:-${HOME}/.cargo}/bin"
mkdir -p "${cargo_bin}"

if [ -x "${cargo_bin}/cargo-deny" ]; then
  # Present but possibly invisible: a child script cannot put this directory on
  # the parent make process's PATH, so callers that probe for it would report it
  # missing right after we "succeeded". Say where it is.
  case ":${PATH}:" in
    *":${cargo_bin}:"*) ;;
    *) echo "note: cargo-deny is installed at ${cargo_bin}/cargo-deny, which is not on PATH." >&2
       echo "      Add it to PATH, or invoke it by absolute path." >&2 ;;
  esac
  exit 0
fi

# Pinned, not `latest`: an unpinned audit tool means a push can start failing on
# a morning when nothing in this repo changed. Bump it deliberately.
version="0.20.2"
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
  echo "▶ Installing cargo-deny ${version} (pre-built) into ${cargo_bin}..."
  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' EXIT
  url="https://github.com/EmbarkStudios/cargo-deny/releases/download/${version}/cargo-deny-${version}-${triple}.tar.gz"
  # The version check is part of the success condition, not a victory lap after
  # it: a binary for the wrong architecture unpacks perfectly and then dies with
  # "Exec format error" on first use, having skipped the fallbacks below.
  if curl -fsSL --retry 3 "${url}" -o "${tmp}/deny.tar.gz" \
     && tar zxf "${tmp}/deny.tar.gz" -C "${tmp}" \
     && mv "${tmp}/cargo-deny-${version}-${triple}/cargo-deny" "${cargo_bin}/cargo-deny" \
     && chmod +x "${cargo_bin}/cargo-deny" \
     && installed="$("${cargo_bin}/cargo-deny" --version 2>/dev/null)" \
     && [ -n "${installed}" ]; then
    echo "✓ ${installed}"
    exit 0
  fi
  rm -f "${cargo_bin}/cargo-deny"
  echo "warning: pre-built download failed; falling back to a source install" >&2
fi

if command -v cargo-binstall >/dev/null 2>&1; then
  echo "▶ Installing cargo-deny via cargo-binstall..."
  cargo binstall --no-confirm cargo-deny
  exit 0
fi

echo "▶ Installing cargo-deny from source (slow — no pre-built path available)..."
cargo install cargo-deny --locked
