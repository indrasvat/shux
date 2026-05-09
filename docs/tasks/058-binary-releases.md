# 058 — Binary Releases and Distribution

**Status:** Pending
**Depends On:** 052
**Parallelizable With:** 053, 054, 057

---

## Problem

Users need to install shux without compiling from source. The PRD requires binary releases for macOS and Linux (both glibc and musl), a Homebrew formula, and `cargo install` support. The release process must be automated via GitHub Actions on tag push, producing signed tarballs with SHA256 checksums. A Homebrew tap makes installation a one-liner for macOS users, and musl builds enable static linking for Alpine and container deployments.

## PRD Reference

- **SS 17** M3 deliverables: "Binary releases for macOS + Linux", "Homebrew formula, cargo install"
- **SS 15.3** Build: "cargo build --release produces a single binary artifact. Cross-compilation for macOS (aarch64 + x86_64) and Linux (glibc + musl)."
- **SS 20.1** OS support: "macOS 13+ (aarch64, x86_64), Linux (glibc, x86_64/aarch64), Linux (musl, x86_64/aarch64)"

---

## Files to Create

- `.github/workflows/release.yml` — Release workflow triggered on tag push
- `Formula/shux.rb` — Homebrew formula (for indrasvat/homebrew-tap)
- `scripts/build-release.sh` — Local release build script
- `.github/workflows/build-check.yml` — Cross-compilation CI check (not release)

## Files to Modify

- `Cargo.toml` — Verify metadata fields for `cargo install` (description, homepage, repository, license, keywords, categories)
- `crates/shux/Cargo.toml` — Verify metadata for crates.io publishing
- `.github/workflows/ci.yml` — Add cross-compilation check job
- `docs/PROGRESS.md` — Mark task 058 complete

---

## Execution Steps

### Step 1: Verify Cargo.toml Metadata

Ensure `crates/shux/Cargo.toml` has all fields needed for `cargo install shux`:

```toml
[package]
name = "shux"
version = "1.0.0"
edition.workspace = true
rust-version.workspace = true
license = "MIT"
repository = "https://github.com/indrasvat/shux"
homepage = "https://github.com/indrasvat/shux"
description = "A modern, batteries-included terminal multiplexer"
readme = "../../README.md"
keywords = ["terminal", "multiplexer", "tmux", "plugin", "tui"]
categories = ["command-line-utilities", "development-tools"]

[package.metadata.docs.rs]
all-features = true

[[bin]]
name = "shux"
path = "src/main.rs"
```

### Step 2: Create Release Workflow

Create `.github/workflows/release.yml`:

```yaml
name: Release

on:
  push:
    tags:
      - 'v*'

permissions:
  contents: write

env:
  CARGO_TERM_COLOR: always

jobs:
  build:
    name: Build (${{ matrix.target }})
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        include:
          # macOS
          - target: aarch64-apple-darwin
            os: macos-14  # M1 runner
            archive: tar.gz
          - target: x86_64-apple-darwin
            os: macos-13
            archive: tar.gz

          # Linux glibc
          - target: x86_64-unknown-linux-gnu
            os: ubuntu-latest
            archive: tar.gz
          - target: aarch64-unknown-linux-gnu
            os: ubuntu-latest
            archive: tar.gz
            cross: true

          # Linux musl (static)
          - target: x86_64-unknown-linux-musl
            os: ubuntu-latest
            archive: tar.gz
          - target: aarch64-unknown-linux-musl
            os: ubuntu-latest
            archive: tar.gz
            cross: true

    steps:
      - uses: actions/checkout@v4

      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}

      - uses: Swatinem/rust-cache@v2
        with:
          key: ${{ matrix.target }}

      - name: Install cross (for cross-compilation)
        if: matrix.cross
        run: cargo install cross --locked

      - name: Install musl tools
        if: contains(matrix.target, 'musl') && !matrix.cross
        run: sudo apt-get update && sudo apt-get install -y musl-tools

      - name: Build
        run: |
          if [ "${{ matrix.cross }}" = "true" ]; then
            cross build --release --target ${{ matrix.target }}
          else
            cargo build --release --target ${{ matrix.target }}
          fi

      - name: Package
        id: package
        run: |
          VERSION="${GITHUB_REF_NAME#v}"
          ARCHIVE_NAME="shux-${VERSION}-${{ matrix.target }}"
          ARCHIVE_FILE="${ARCHIVE_NAME}.${{ matrix.archive }}"

          mkdir -p "staging/${ARCHIVE_NAME}"
          cp "target/${{ matrix.target }}/release/shux" "staging/${ARCHIVE_NAME}/"
          cp README.md LICENSE "staging/${ARCHIVE_NAME}/" 2>/dev/null || true

          # Include man page and completions
          if [ -f man/shux.1 ]; then
            cp man/shux.1 "staging/${ARCHIVE_NAME}/"
          fi

          cd staging
          tar czf "../${ARCHIVE_FILE}" "${ARCHIVE_NAME}"
          cd ..

          # Generate SHA256
          shasum -a 256 "${ARCHIVE_FILE}" > "${ARCHIVE_FILE}.sha256"

          echo "archive=${ARCHIVE_FILE}" >> "$GITHUB_OUTPUT"
          echo "sha256=${ARCHIVE_FILE}.sha256" >> "$GITHUB_OUTPUT"

      - name: Upload artifact
        uses: actions/upload-artifact@v4
        with:
          name: ${{ matrix.target }}
          path: |
            ${{ steps.package.outputs.archive }}
            ${{ steps.package.outputs.sha256 }}

  release:
    name: Create Release
    needs: build
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Download all artifacts
        uses: actions/download-artifact@v4
        with:
          path: artifacts

      - name: Generate checksums
        run: |
          cd artifacts
          # Collect all SHA256 files into one
          find . -name '*.sha256' -exec cat {} \; > ../SHA256SUMS.txt
          cd ..
          echo "=== SHA256 Checksums ==="
          cat SHA256SUMS.txt

      - name: Generate release notes
        id: notes
        run: |
          VERSION="${GITHUB_REF_NAME#v}"
          cat > release-notes.md << 'NOTES_EOF'
          ## shux ${VERSION}

          ### Installation

          **Homebrew (macOS):**
          ```bash
          brew install indrasvat/tap/shux
          ```

          **Cargo:**
          ```bash
          cargo install shux
          ```

          **Binary download:**
          Download the archive for your platform below, extract, and add to your PATH.

          ### Platforms

          | Platform | Architecture | Archive |
          |----------|-------------|---------|
          | macOS | Apple Silicon (aarch64) | `shux-${VERSION}-aarch64-apple-darwin.tar.gz` |
          | macOS | Intel (x86_64) | `shux-${VERSION}-x86_64-apple-darwin.tar.gz` |
          | Linux | x86_64 (glibc) | `shux-${VERSION}-x86_64-unknown-linux-gnu.tar.gz` |
          | Linux | aarch64 (glibc) | `shux-${VERSION}-aarch64-unknown-linux-gnu.tar.gz` |
          | Linux | x86_64 (musl/static) | `shux-${VERSION}-x86_64-unknown-linux-musl.tar.gz` |
          | Linux | aarch64 (musl/static) | `shux-${VERSION}-aarch64-unknown-linux-musl.tar.gz` |

          ### Checksums

          Verify your download:
          ```bash
          shasum -a 256 -c SHA256SUMS.txt
          ```
          NOTES_EOF

      - name: Create GitHub Release
        uses: softprops/action-gh-release@v2
        with:
          body_path: release-notes.md
          files: |
            artifacts/**/*.tar.gz
            artifacts/**/*.sha256
            SHA256SUMS.txt
          fail_on_unmatched_files: false
          generate_release_notes: true
          draft: false
          prerelease: ${{ contains(github.ref_name, 'rc') || contains(github.ref_name, 'beta') || contains(github.ref_name, 'alpha') }}

  homebrew:
    name: Update Homebrew Formula
    needs: release
    runs-on: ubuntu-latest
    if: "!contains(github.ref_name, 'rc') && !contains(github.ref_name, 'beta')"
    steps:
      - uses: actions/checkout@v4

      - name: Download macOS artifacts
        uses: actions/download-artifact@v4
        with:
          path: artifacts

      - name: Calculate SHA256 for Homebrew
        id: sha
        run: |
          VERSION="${GITHUB_REF_NAME#v}"

          # macOS aarch64
          DARWIN_ARM_SHA=$(cat artifacts/aarch64-apple-darwin/*.sha256 | awk '{print $1}')
          DARWIN_X86_SHA=$(cat artifacts/x86_64-apple-darwin/*.sha256 | awk '{print $1}')
          LINUX_X86_SHA=$(cat artifacts/x86_64-unknown-linux-gnu/*.sha256 | awk '{print $1}')
          LINUX_ARM_SHA=$(cat artifacts/aarch64-unknown-linux-gnu/*.sha256 | awk '{print $1}')

          echo "version=${VERSION}" >> "$GITHUB_OUTPUT"
          echo "darwin_arm_sha=${DARWIN_ARM_SHA}" >> "$GITHUB_OUTPUT"
          echo "darwin_x86_sha=${DARWIN_X86_SHA}" >> "$GITHUB_OUTPUT"
          echo "linux_x86_sha=${LINUX_X86_SHA}" >> "$GITHUB_OUTPUT"
          echo "linux_arm_sha=${LINUX_ARM_SHA}" >> "$GITHUB_OUTPUT"

      - name: Update Homebrew formula
        run: |
          # Generate the formula (to be pushed to indrasvat/homebrew-tap)
          cat > Formula/shux.rb << FORMULA_EOF
          class Shux < Formula
            desc "Modern, batteries-included terminal multiplexer"
            homepage "https://github.com/indrasvat/shux"
            version "${{ steps.sha.outputs.version }}"
            license "MIT"

            on_macos do
              if Hardware::CPU.arm?
                url "https://github.com/indrasvat/shux/releases/download/${GITHUB_REF_NAME}/shux-${{ steps.sha.outputs.version }}-aarch64-apple-darwin.tar.gz"
                sha256 "${{ steps.sha.outputs.darwin_arm_sha }}"
              else
                url "https://github.com/indrasvat/shux/releases/download/${GITHUB_REF_NAME}/shux-${{ steps.sha.outputs.version }}-x86_64-apple-darwin.tar.gz"
                sha256 "${{ steps.sha.outputs.darwin_x86_sha }}"
              end
            end

            on_linux do
              if Hardware::CPU.arm?
                url "https://github.com/indrasvat/shux/releases/download/${GITHUB_REF_NAME}/shux-${{ steps.sha.outputs.version }}-aarch64-unknown-linux-gnu.tar.gz"
                sha256 "${{ steps.sha.outputs.linux_arm_sha }}"
              else
                url "https://github.com/indrasvat/shux/releases/download/${GITHUB_REF_NAME}/shux-${{ steps.sha.outputs.version }}-x86_64-unknown-linux-gnu.tar.gz"
                sha256 "${{ steps.sha.outputs.linux_x86_sha }}"
              end
            end

            def install
              bin.install "shux"
              man1.install "shux.1" if File.exist? "shux.1"
            end

            def caveats
              <<~EOS
                To enable shell completions:
                  bash: shux completions bash > $(brew --prefix)/etc/bash_completion.d/shux
                  zsh:  shux completions zsh > $(brew --prefix)/share/zsh/site-functions/_shux
                  fish: shux completions fish > $(brew --prefix)/share/fish/vendor_completions.d/shux.fish
              EOS
            end

            test do
              assert_match "shux", shell_output("#{bin}/shux --version")
            end
          end
          FORMULA_EOF

          echo "Formula generated. Push to indrasvat/homebrew-tap separately."
```

### Step 3: Create Local Build Script

Create `scripts/build-release.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?Usage: $0 <version>}"
TARGETS=(
    "aarch64-apple-darwin"
    "x86_64-apple-darwin"
    "x86_64-unknown-linux-gnu"
    "aarch64-unknown-linux-gnu"
    "x86_64-unknown-linux-musl"
    "aarch64-unknown-linux-musl"
)

echo "Building shux v${VERSION} for ${#TARGETS[@]} targets..."

DIST_DIR="dist/v${VERSION}"
mkdir -p "$DIST_DIR"

for target in "${TARGETS[@]}"; do
    echo ""
    echo "─── Building for ${target} ───"

    if [[ "$target" == *"linux"* ]] && [[ "$(uname)" == "Darwin" ]]; then
        echo "  Using cross for Linux target on macOS"
        cross build --release --target "$target"
    else
        cargo build --release --target "$target"
    fi

    ARCHIVE="shux-${VERSION}-${target}.tar.gz"
    mkdir -p "staging/shux-${VERSION}-${target}"
    cp "target/${target}/release/shux" "staging/shux-${VERSION}-${target}/"
    cp README.md "staging/shux-${VERSION}-${target}/" 2>/dev/null || true

    cd staging
    tar czf "../${DIST_DIR}/${ARCHIVE}" "shux-${VERSION}-${target}"
    cd ..
    rm -rf staging

    shasum -a 256 "${DIST_DIR}/${ARCHIVE}" >> "${DIST_DIR}/SHA256SUMS.txt"
    echo "  Created ${ARCHIVE}"
done

echo ""
echo "═══════════════════════════════"
echo "Release archives in ${DIST_DIR}/"
ls -lh "${DIST_DIR}"
echo ""
echo "SHA256 checksums:"
cat "${DIST_DIR}/SHA256SUMS.txt"
```

### Step 4: Cross-Compilation CI Check

Create `.github/workflows/build-check.yml` to verify cross-compilation works on PRs:

```yaml
name: Build Check

on:
  pull_request:
    branches: [main]

jobs:
  cross-check:
    name: Cross-compile check (${{ matrix.target }})
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        include:
          - target: aarch64-apple-darwin
            os: macos-14
          - target: x86_64-unknown-linux-gnu
            os: ubuntu-latest
          - target: x86_64-unknown-linux-musl
            os: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}
      - uses: Swatinem/rust-cache@v2
      - name: Install musl tools
        if: contains(matrix.target, 'musl')
        run: sudo apt-get update && sudo apt-get install -y musl-tools
      - name: Check compilation
        run: cargo build --release --target ${{ matrix.target }}
```

### Step 5: Verify cargo install

Test that `cargo install` works correctly:

```bash
# From the project root
cargo install --path crates/shux

# Verify
~/.cargo/bin/shux --version
# Expected: shux 1.0.0

# Test from crates.io (after publishing)
# cargo install shux
```

### Step 6: Add Tests

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn binary_has_version_info() {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_shux"))
            .arg("--version")
            .output()
            .expect("Failed to run shux");
        let version = String::from_utf8_lossy(&output.stdout);
        assert!(version.contains("shux"), "Version output should contain 'shux'");
    }

    #[test]
    fn cargo_toml_has_required_metadata() {
        let cargo_toml = include_str!("../../Cargo.toml");
        assert!(cargo_toml.contains("description"));
        assert!(cargo_toml.contains("repository"));
        assert!(cargo_toml.contains("license"));
        assert!(cargo_toml.contains("keywords"));
    }
}
```

---

## Verification

### Functional

```bash
# Test release build locally
cargo build --release
./target/release/shux --version
# Expected: shux 1.0.0

# Test cargo install
cargo install --path crates/shux
shux --version

# Test local release script
./scripts/build-release.sh 1.0.0
ls dist/v1.0.0/
# Expected: 6 .tar.gz files + SHA256SUMS.txt

# Verify checksums
cd dist/v1.0.0 && shasum -a 256 -c SHA256SUMS.txt

# Test Homebrew formula syntax (if brew is available)
brew audit --strict Formula/shux.rb 2>/dev/null || echo "Formula syntax check skipped"
```

### Tests

```bash
# Run release-related tests
cargo nextest run binary_has_version_info cargo_toml_has_required_metadata

# Verify CI workflow syntax
# Push to branch and verify GitHub Actions
```

---

## Completion Criteria

- [ ] Release workflow triggers on tag push (v*)
- [ ] Builds produced for all 6 targets: macOS (aarch64, x86_64), Linux glibc (x86_64, aarch64), Linux musl (x86_64, aarch64)
- [ ] Binary naming: `shux-{version}-{target}.tar.gz`
- [ ] SHA256 checksums generated for all archives
- [ ] GitHub Release created with archives, checksums, and release notes
- [ ] Homebrew formula generated with correct SHA256 hashes
- [ ] `cargo install shux` works (Cargo.toml metadata complete)
- [ ] Pre-release detection: rc/beta/alpha tags marked as pre-release
- [ ] Local build script works for all supported platforms
- [ ] Cross-compilation CI check runs on PRs
- [ ] Man page included in release archives
- [ ] Release archives include README and LICENSE

---

## Commit Message

```
ci: add release workflow, Homebrew formula, and binary distribution

- GitHub Actions release workflow on tag push (v*)
- Build matrix: macOS (aarch64, x86_64), Linux glibc/musl (x86_64, aarch64)
- Binary archives with SHA256 checksums
- Homebrew formula for indrasvat/homebrew-tap
- Cargo.toml metadata for cargo install support
- Local build script for manual releases
- Cross-compilation CI check on PRs
```

---

## Session Protocol

1. **Before starting:** Verify `cargo build --release` works locally. Set up cross-compilation toolchains if testing locally (e.g., `cross` for Linux from macOS). Create the `indrasvat/homebrew-tap` repository if it doesn't exist.
2. **During:** Start with Cargo.toml metadata (Step 1), then the release workflow (Step 2), then Homebrew (Step 3). Test the workflow by creating a test tag on a branch. Do not tag `main` until the workflow is verified.
3. **Testing the workflow:** Create a tag like `v0.0.0-test` on a feature branch. Verify the workflow runs, builds complete, and the release is created (mark as pre-release). Delete the test release afterward.
4. **Edge cases to watch for:**
   - macOS code signing (not required for v1 but may cause Gatekeeper warnings)
   - musl builds may need different linking flags for wasmtime
   - cross-compilation of wasmtime may require additional setup
   - Homebrew formula must not reference development-only files
   - SHA256 checksums must match across all download methods
5. **After:** Do a dry-run release with a test tag. Verify all 6 archives are produced, checksums match, and Homebrew formula is correct. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings (create from task 000 template if missing).
