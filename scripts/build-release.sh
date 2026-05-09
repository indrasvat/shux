#!/usr/bin/env bash
# Packages pre-built shux release binaries into per-platform tarballs.
#
# Inputs (downloaded by the release workflow into staging dirs):
#   staging/x86_64-unknown-linux-gnu/shux
#   staging/aarch64-unknown-linux-gnu/shux
#   staging/x86_64-apple-darwin/shux
#   staging/aarch64-apple-darwin/shux
#
# Outputs:
#   artifacts/shux-v<version>-<arch>-<os>.tar.gz
#   artifacts/shux-v<version>-<arch>-<os>.tar.gz.sha256
#
# Called by semantic-release's exec plugin (prepareCmd) with the next
# version as $1. Designed to be idempotent — re-running over an existing
# artifacts/ dir replaces the contents.

set -euo pipefail

VERSION="${1:-}"
if [[ -z "$VERSION" ]]; then
    echo "Usage: $0 <version>" >&2
    exit 1
fi
VERSION="${VERSION#v}"

STAGING="${STAGING_DIR:-staging}"
OUT="${ARTIFACTS_DIR:-artifacts}"

mkdir -p "$OUT"

# (target-triple, asset-suffix) pairs.
declare -a TARGETS=(
    "x86_64-unknown-linux-gnu:x86_64-linux"
    "aarch64-unknown-linux-gnu:aarch64-linux"
    "x86_64-apple-darwin:x86_64-darwin"
    "aarch64-apple-darwin:aarch64-darwin"
)

for entry in "${TARGETS[@]}"; do
    triple="${entry%%:*}"
    suffix="${entry##*:}"
    bin_path="${STAGING}/${triple}/shux"

    if [[ ! -x "$bin_path" ]]; then
        echo "Warning: binary missing at $bin_path; skipping" >&2
        continue
    fi

    tar_name="shux-v${VERSION}-${suffix}.tar.gz"
    work="$(mktemp -d)"
    pkg_dir="${work}/shux-v${VERSION}-${suffix}"
    mkdir -p "$pkg_dir"
    cp "$bin_path" "$pkg_dir/shux"
    cp README.md "$pkg_dir/README.md" 2>/dev/null || true
    cp CHANGELOG.md "$pkg_dir/CHANGELOG.md" 2>/dev/null || true

    tar -czvf "${OUT}/${tar_name}" -C "$work" "shux-v${VERSION}-${suffix}"

    # SHA256 checksum (cross-platform: shasum on macOS, sha256sum on Linux).
    if command -v sha256sum >/dev/null 2>&1; then
        ( cd "$OUT" && sha256sum "$tar_name" > "${tar_name}.sha256" )
    else
        ( cd "$OUT" && shasum -a 256 "$tar_name" > "${tar_name}.sha256" )
    fi

    rm -rf "$work"
    echo "Packaged: ${OUT}/${tar_name}"
done

echo ""
echo "==> Build complete!"
ls -la "$OUT/"
