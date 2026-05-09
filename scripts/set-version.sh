#!/usr/bin/env bash
# Updates workspace version in Cargo.toml.
# Usage: ./scripts/set-version.sh <version>

set -euo pipefail

VERSION="${1:-}"

if [[ -z "$VERSION" ]]; then
    echo "Usage: $0 <version>" >&2
    exit 1
fi

# Strip leading 'v' if present.
VERSION="${VERSION#v}"

# Validate semver: MAJOR.MINOR.PATCH with optional `-prerelease` and
# optional `+build` metadata per https://semver.org/spec/v2.0.0.html.
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
    echo "Invalid semver version: $VERSION" >&2
    exit 1
fi

CARGO_TOML="Cargo.toml"

if [[ ! -f "$CARGO_TOML" ]]; then
    echo "Error: $CARGO_TOML not found" >&2
    exit 1
fi

# Update version in [workspace.package] section.
# Cross-platform sed (macOS vs Linux) via temp file. The replacement
# pattern matches MAJOR.MINOR.PATCH plus an optional pre-release suffix
# AND an optional build-metadata suffix, both of which are dropped when
# the new version omits them.
TEMP_FILE=$(mktemp)
sed -E 's/^(version = ")[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?(")/\1'"$VERSION"'\4/' "$CARGO_TOML" > "$TEMP_FILE"
mv "$TEMP_FILE" "$CARGO_TOML"

echo "Updated $CARGO_TOML to version $VERSION"

# Verify the change landed in the workspace.package block.
grep -E '^version = "' "$CARGO_TOML" | head -1
