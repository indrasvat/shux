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

# No "installed but not on PATH" branch, unlike the sibling scripts: this is a
# cargo SUBCOMMAND, and cargo searches ${CARGO_HOME}/bin for `cargo-*` itself.
# Measured — with that directory stripped from PATH, `command -v cargo-deny`
# fails and `cargo deny --version` still prints 0.20.2. So the probe above is
# already the question the caller asks, and a branch that exited 0 merely
# because a file exists would be reporting success after that probe had failed:
# a stale or wrong-architecture binary would sail straight through it.

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
  cargo binstall --no-confirm "cargo-deny@${version}"
  exit 0
fi

echo "▶ Installing cargo-deny ${version} from source (slow — no pre-built path available)..."
# `--version`, not just `--locked`: `--locked` only preserves the lockfile of
# whichever release is selected, so on its own this installs whatever the
# registry currently publishes — an unpinned audit tool in the fallback of a
# script whose whole point above is the pin.
cargo install cargo-deny --version "${version}" --locked
