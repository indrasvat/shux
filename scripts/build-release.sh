#!/usr/bin/env bash
# Cross-compiles shux for four targets on a single macOS runner and
# packages each into a per-platform tarball with SHA-256 sidecar.
#
# Toolchain assumptions (CI provides these via release.yml):
#   - rustup with all four targets installed
#   - zig (system linker) on PATH
#   - cargo-zigbuild on PATH
#   - macOS aarch64 native, macOS x86_64 cross via apple SDK
#
# Outputs:
#   artifacts/shux-v<version>-<arch>-<os>.tar.gz
#   artifacts/shux-v<version>-<arch>-<os>.tar.gz.sha256
#
# Local development: run `make release-package` instead. That sets
# HOST_ONLY=1 so this script packages whatever the host already built
# without trying to cross-compile.

set -euo pipefail

VERSION="${1:-}"
if [[ -z "$VERSION" ]]; then
    echo "Usage: $0 <version>" >&2
    exit 1
fi
VERSION="${VERSION#v}"

OUT="${ARTIFACTS_DIR:-artifacts}"

# Wipe artifacts/ so a partial rerun cannot ship stale tarballs that
# still match the publish glob.
rm -rf "$OUT"
mkdir -p "$OUT"

build_target() {
    local target="$1"
    if [[ "${HOST_ONLY:-0}" == "1" ]]; then
        # Local mode: only package what's already in target/<triple>/release.
        return 0
    fi

    case "$target" in
        *-apple-darwin)
            # Native or `rustup target add`-driven cross-compile. Apple
            # ships an x86_64 SDK on every macOS runner, so this works
            # without zig.
            cargo build --release --bin shux --target "$target"
            ;;
        *-unknown-linux-gnu)
            # Use cargo-zigbuild for the Linux targets. Pin glibc to
            # 2.17 (RHEL7 / CentOS7 era) for maximum distro reach.
            local zb_target="${target}.2.17"
            cargo zigbuild --release --bin shux --target "$zb_target"
            ;;
        *)
            echo "Unknown target $target" >&2
            return 1
            ;;
    esac
}

package_target() {
    local target="$1" suffix="$2"
    local bin_path="target/${target}/release/shux"

    if [[ ! -x "$bin_path" ]]; then
        if [[ "${HOST_ONLY:-0}" == "1" ]]; then
            echo "Skipping $target (HOST_ONLY=1, binary missing)" >&2
            return 0
        fi
        echo "Error: expected binary missing at $bin_path" >&2
        return 1
    fi

    local tar_name="shux-v${VERSION}-${suffix}.tar.gz"
    local work
    work="$(mktemp -d)"
    local pkg_dir="${work}/shux-v${VERSION}-${suffix}"
    mkdir -p "$pkg_dir"
    cp "$bin_path" "$pkg_dir/shux"
    cp README.md "$pkg_dir/README.md" 2>/dev/null || true
    cp CHANGELOG.md "$pkg_dir/CHANGELOG.md" 2>/dev/null || true

    tar -czvf "${OUT}/${tar_name}" -C "$work" "shux-v${VERSION}-${suffix}"

    if command -v sha256sum >/dev/null 2>&1; then
        ( cd "$OUT" && sha256sum "$tar_name" > "${tar_name}.sha256" )
    else
        ( cd "$OUT" && shasum -a 256 "$tar_name" > "${tar_name}.sha256" )
    fi

    rm -rf "$work"
    echo "Packaged: ${OUT}/${tar_name}"
}

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
    echo "==> Building $triple"
    build_target "$triple"
done

for entry in "${TARGETS[@]}"; do
    triple="${entry%%:*}"
    suffix="${entry##*:}"
    package_target "$triple" "$suffix"
done

# Belt-and-suspenders: in CI, refuse to ship an incomplete release.
if [[ "${HOST_ONLY:-0}" != "1" ]]; then
    EXPECTED=4
    ACTUAL=$(find "$OUT" -name 'shux-v*.tar.gz' -type f | wc -l | tr -d ' ')
    if [[ "$ACTUAL" -ne "$EXPECTED" ]]; then
        echo "Error: expected $EXPECTED tarballs, found $ACTUAL" >&2
        ls -la "$OUT/" >&2
        exit 1
    fi
fi

echo ""
echo "==> Build complete!"
ls -la "$OUT/"
